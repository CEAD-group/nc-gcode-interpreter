//lib.rs
#[macro_use]
extern crate pest_derive;
mod types;

mod errors;
mod interpret_rules;
pub mod interpreter;
mod modal_groups;
pub mod output;
mod line_driver;
mod state;
mod structure_scan;

#[cfg(feature = "python")]
mod python_bindings {
    use pyo3::exceptions::PyValueError;
    use pyo3::prelude::*;
    use pyo3::types::{PyDict, PyList};
    use pyo3::wrap_pyfunction;
    use std::collections::HashMap;
    use std::sync::{mpsc, Mutex};

    use crate::interpreter::{nc_to_row_stream, nc_to_table};
    use crate::output::{is_forward_filled_column, is_string_column, Column};
    use crate::types::Value;

    /// Interpret an NC program and return plain columnar data:
    /// (data: dict[str, list], schema: list[(name, dtype)], state: dict).
    /// The Python wrapper assembles a polars DataFrame from this, keeping
    /// the Rust crate free of any polars dependency.
    #[pyfunction]
    #[pyo3(signature = (input, initial_state = None, axis_identifiers = None, extra_axes = None, iteration_limit = 10000, disable_forward_fill = false, axis_index_map = None, allow_undefined_variables=false))]
    #[allow(clippy::too_many_arguments)]
    fn nc_to_columns(
        py: Python<'_>,
        input: &str,
        initial_state: Option<String>,
        axis_identifiers: Option<Vec<String>>,
        extra_axes: Option<Vec<String>>,
        iteration_limit: usize,
        disable_forward_fill: bool,
        axis_index_map: Option<HashMap<String, usize>>,
        allow_undefined_variables: bool,
    ) -> PyResult<(
        Py<PyDict>,
        Vec<(String, String)>,
        HashMap<String, HashMap<String, f64>>,
    )> {
        let (table, state) = nc_to_table(
            input,
            initial_state.as_deref(),
            axis_identifiers,
            extra_axes,
            iteration_limit,
            disable_forward_fill,
            axis_index_map,
            allow_undefined_variables,
        )
        .map_err(|e| PyErr::new::<PyValueError, _>(format!("{}", e)))?;

        let data = PyDict::new(py);
        let mut schema: Vec<(String, String)> = Vec::with_capacity(table.columns.len());
        for (name, column) in table.columns {
            schema.push((name.clone(), column.dtype_name().to_string()));
            match column {
                Column::Float(v) => data.set_item(&name, v)?,
                Column::Int(v) => data.set_item(&name, v)?,
                Column::Str(v) => data.set_item(&name, v)?,
                Column::StrList(v) => data.set_item(&name, v)?,
            }
        }

        Ok((data.into(), schema, state.to_python_dict()))
    }

    type FinalState = HashMap<String, HashMap<String, f64>>;

    /// Iterator over interpreted rows: `next()` yields
    /// `(line_no, {column: value})` while the interpreter runs on a worker
    /// thread behind a bounded channel. Dropping the iterator hangs up the
    /// channel and aborts interpretation; note that in Python, breaking out
    /// of a loop only drops it when no other reference keeps it alive.
    #[pyclass]
    struct NcRowIterator {
        rows: Option<Mutex<mpsc::Receiver<crate::output::Row>>>,
        result: Option<Mutex<mpsc::Receiver<Result<FinalState, String>>>>,
        handle: Option<std::thread::JoinHandle<()>>,
        /// Running forward-fill values, keyed by column.
        fill: HashMap<String, Value>,
        forward_fill: bool,
        include_variables: bool,
        state: Option<FinalState>,
    }

    impl NcRowIterator {
        /// Convert one cell with the same name-based typing as the batch
        /// table (N: int, M: list[str], string columns: str, rest: float).
        fn cell_to_py(py: Python<'_>, name: &str, value: &Value) -> PyResult<Py<PyAny>> {
            let object = if name == "N" {
                match value {
                    Value::Str(s) => s
                        .parse::<i64>()
                        .ok()
                        .or_else(|| s.parse::<f64>().ok().map(|v| v as i64))
                        .into_pyobject(py)?
                        .into_any()
                        .unbind(),
                    Value::Float(f) => (*f as i64).into_pyobject(py)?.into_any().unbind(),
                    Value::StrList(l) => PyList::new(py, l)?.into_any().unbind(),
                }
            } else if name == "M" {
                match value {
                    Value::StrList(l) => PyList::new(py, l)?.into_any().unbind(),
                    Value::Str(s) => PyList::new(py, [s])?.into_any().unbind(),
                    Value::Float(f) => PyList::new(py, [f.to_string()])?.into_any().unbind(),
                }
            } else if is_string_column(name) {
                match value {
                    Value::Str(s) => s.into_pyobject(py)?.into_any().unbind(),
                    Value::Float(f) => f.to_string().into_pyobject(py)?.into_any().unbind(),
                    Value::StrList(l) => PyList::new(py, l)?.into_any().unbind(),
                }
            } else {
                match value {
                    Value::Float(f) => f.into_pyobject(py)?.into_any().unbind(),
                    Value::Str(s) => s.parse::<f64>().ok().into_pyobject(py)?.into_any().unbind(),
                    Value::StrList(l) => PyList::new(py, l)?.into_any().unbind(),
                }
            };
            Ok(object)
        }

        fn finish(&mut self, py: Python<'_>) -> PyResult<()> {
            if let Some(result) = self.result.take() {
                let receiver = result.into_inner().expect("result receiver mutex poisoned");
                match py.detach(move || receiver.recv()) {
                    Ok(Ok(state)) => self.state = Some(state),
                    Ok(Err(message)) => {
                        self.join();
                        return Err(PyErr::new::<PyValueError, _>(message));
                    }
                    Err(_) => {}
                }
            }
            self.join();
            Ok(())
        }

        fn join(&mut self) {
            if let Some(handle) = self.handle.take() {
                let _ = handle.join();
            }
        }
    }

    #[pymethods]
    impl NcRowIterator {
        fn __iter__(slf: PyRef<'_, Self>) -> PyRef<'_, Self> {
            slf
        }

        fn __next__(&mut self, py: Python<'_>) -> PyResult<Option<Py<PyAny>>> {
            loop {
                let Some(mutex) = self.rows.take() else {
                    return Ok(None);
                };
                // The receiver moves into the detached closure (an owned
                // Receiver is Send) and back out, releasing the GIL while
                // blocked on the channel.
                let receiver = mutex.into_inner().expect("row receiver mutex poisoned");
                let (received, receiver) = py.detach(move || {
                    let received = receiver.recv();
                    (received, receiver)
                });
                if received.is_ok() {
                    self.rows = Some(Mutex::new(receiver));
                }
                match received {
                    Ok(row) => {
                        // Blocks that only assigned variables have no output
                        // cells; the batch table drops them, so the default
                        // stream does too.
                        if row.cells.is_empty() && !self.include_variables {
                            continue;
                        }
                        let dict = PyDict::new(py);
                        // A variable-only row stays an empty dict: it is a
                        // state event, not an output row, so it is not
                        // forward-filled into a fake duplicate of the
                        // previous position.
                        if !row.cells.is_empty() {
                            if self.forward_fill {
                                for (key, value) in &row.cells {
                                    if is_forward_filled_column(key) {
                                        self.fill.insert(key.clone(), value.clone());
                                    }
                                }
                            }
                            // Forward-filled columns first, then the row's own
                            // values (its fillable ones are already in `fill`).
                            for (key, value) in &self.fill {
                                dict.set_item(key, Self::cell_to_py(py, key, value)?)?;
                            }
                            for (key, value) in &row.cells {
                                if !self.forward_fill || !is_forward_filled_column(key) {
                                    dict.set_item(key, Self::cell_to_py(py, key, value)?)?;
                                }
                            }
                        }
                        let item = if self.include_variables {
                            // Last write wins when a block assigns the same
                            // variable twice; replaying these dicts row by row
                            // reconstructs the symbol table at any point.
                            let variables = PyDict::new(py);
                            for (key, value) in &row.variable_changes {
                                variables.set_item(key, *value)?;
                            }
                            (row.line_no, dict, variables).into_pyobject(py)?.into_any().unbind()
                        } else {
                            (row.line_no, dict).into_pyobject(py)?.into_any().unbind()
                        };
                        return Ok(Some(item));
                    }
                    // Channel closed: interpretation finished (or failed).
                    Err(_) => {
                        self.rows = None;
                        self.finish(py)?;
                        return Ok(None);
                    }
                }
            }
        }

        /// The final interpreter state (axes, symbol_table, translation),
        /// available once the iterator is exhausted.
        #[getter]
        fn state(&self) -> Option<FinalState> {
            self.state.clone()
        }
    }

    /// Interpret an NC program lazily: returns an iterator of
    /// `(line_no, row_dict)` — or `(line_no, row_dict, variables_dict)` when
    /// `include_variables` is set — produced by the interpreter running on a
    /// worker thread. Rows are forward-filled like the batch DataFrame
    /// unless `forward_fill` is false.
    #[pyfunction]
    #[pyo3(signature = (input, initial_state = None, axis_identifiers = None, extra_axes = None, iteration_limit = 10000, forward_fill = true, include_variables = false, axis_index_map = None, allow_undefined_variables = false))]
    #[allow(clippy::too_many_arguments)]
    fn nc_to_rows(
        input: String,
        initial_state: Option<String>,
        axis_identifiers: Option<Vec<String>>,
        extra_axes: Option<Vec<String>>,
        iteration_limit: usize,
        forward_fill: bool,
        include_variables: bool,
        axis_index_map: Option<HashMap<String, usize>>,
        allow_undefined_variables: bool,
    ) -> PyResult<NcRowIterator> {
        let (row_sender, row_receiver) = mpsc::sync_channel::<crate::output::Row>(1024);
        let (result_sender, result_receiver) = mpsc::sync_channel::<Result<FinalState, String>>(1);

        let handle = std::thread::Builder::new()
            .name("nc-interpreter".to_string())
            .spawn(move || {
                let outcome = nc_to_row_stream(
                    &input,
                    initial_state.as_deref(),
                    axis_identifiers,
                    extra_axes,
                    iteration_limit,
                    axis_index_map,
                    allow_undefined_variables,
                    row_sender,
                );
                let message = match outcome {
                    Ok(state) => Ok(state.to_python_dict()),
                    // The consumer hung up: nothing to report to nobody.
                    Err(crate::errors::ParsingError::StreamClosed) => return,
                    Err(error) => Err(format!("{}", error)),
                };
                let _ = result_sender.send(message);
            })
            .map_err(|e| PyErr::new::<PyValueError, _>(format!("failed to spawn interpreter thread: {}", e)))?;

        Ok(NcRowIterator {
            rows: Some(Mutex::new(row_receiver)),
            result: Some(Mutex::new(result_receiver)),
            handle: Some(handle),
            fill: HashMap::new(),
            forward_fill,
            include_variables,
            state: None,
        })
    }

    /// Define the Python module
    #[pymodule(name = "_internal")]
    pub fn nc_gcode_interpreter(_py: Python<'_>, m: &Bound<'_, PyModule>) -> PyResult<()> {
        m.add_function(wrap_pyfunction!(nc_to_columns, m)?)?;
        m.add_function(wrap_pyfunction!(nc_to_rows, m)?)?;
        m.add_class::<NcRowIterator>()?;
        Ok(())
    }
}

#[cfg(feature = "python")]
pub use python_bindings::nc_gcode_interpreter;
