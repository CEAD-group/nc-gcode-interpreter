//lib.rs
#[macro_use]
extern crate pest_derive;
mod types;

mod errors;
pub mod flatten;
mod interpret_rules;
pub mod interpreter;
mod line_driver;
mod modal_groups;
pub mod output;
mod state;
mod structure_scan;

#[cfg(feature = "python")]
mod python_bindings {
    use pyo3::exceptions::PyValueError;
    use pyo3::prelude::*;
    use pyo3::types::PyCapsule;
    use pyo3::types::{PyDict, PyList};
    use pyo3::wrap_pyfunction;
    use std::collections::HashMap;
    use std::sync::Arc;

    /// One output batch as an Arrow struct array, exported to Python through the
    /// Arrow PyCapsule interface. `pl.DataFrame(obj)` (polars >= 1.3) reads the
    /// `__arrow_c_array__` capsules zero-copy - no `pyarrow`, and no `pyo3-arrow`
    /// (which would drag in numpy/chrono-tz/comfy_table). The struct array's
    /// fields are the table columns; `arrow_array::ffi::to_ffi` produces the two
    /// C structs and the capsule destructors free them if the consumer did not.
    #[pyclass]
    struct ArrowBatch {
        data: arrow_data::ArrayData,
    }

    #[pymethods]
    impl ArrowBatch {
        // The Arrow PyCapsule protocol passes `requested_schema` optionally, and
        // consumers (polars) call it with no args, so it must default to None; we
        // always export our own schema and ignore the request.
        #[pyo3(signature = (requested_schema=None))]
        fn __arrow_c_array__<'py>(
            &self,
            py: Python<'py>,
            requested_schema: Option<Bound<'py, PyAny>>,
        ) -> PyResult<(Bound<'py, PyCapsule>, Bound<'py, PyCapsule>)> {
            let _ = requested_schema;
            let (array, schema) = arrow_array::ffi::to_ffi(&self.data)
                .map_err(|e| PyErr::new::<PyValueError, _>(format!("Arrow FFI export failed: {}", e)))?;
            // Capsule names are fixed by the Arrow C data interface; the boxed
            // FFI struct's Drop runs the Arrow release callback when the consumer
            // has not moved it out.
            let schema_capsule = PyCapsule::new_with_value(py, schema, c"arrow_schema")?;
            let array_capsule = PyCapsule::new_with_value(py, array, c"arrow_array")?;
            Ok((schema_capsule, array_capsule))
        }
    }
    use std::sync::{mpsc, Mutex};

    use crate::errors::ErrorLocation;
    use crate::interpreter::{nc_to_batch_stream_with_line_numbers, nc_to_row_stream};
    use crate::output::{is_forward_filled_column, is_string_column, Column, Row, Table, VariableEvents};
    use crate::state::FinalState;
    use crate::types::Value;

    pyo3::create_exception!(
        _internal,
        NcError,
        PyValueError,
        "NC parse/interpret error. Subclasses ValueError (so `except ValueError` \
         still catches it) and carries the error's source location as data: the \
         `line`, `column`, `context`, and `line_text` attributes (each an int / \
         str, or None when not applicable), plus a stable `kind` string \
         discriminating the error class (e.g. 'unexpected_axis', \
         'undefined_variable') for branching without matching the message. \
         `str(err)` is the full formatted message as before."
    );

    /// An error crossing the worker channel: the formatted message, the stable
    /// error-kind discriminator, and the structured location, so the consuming
    /// thread can raise an `NcError` carrying all three.
    struct ErrInfo {
        message: String,
        kind: &'static str,
        location: Option<ErrorLocation>,
    }

    impl ErrInfo {
        fn from_error(error: &crate::errors::ParsingError) -> Self {
            ErrInfo {
                message: error.to_string(),
                kind: error.kind(),
                location: error.location(),
            }
        }

        /// Build the Python `NcError`, always setting the `kind` discriminator
        /// and the four location attributes (None when absent) so callers can
        /// read them unconditionally.
        fn into_pyerr(self, py: Python<'_>) -> PyErr {
            let err = NcError::new_err(self.message);
            let value = err.value(py);
            let (line, column, context, line_text) = match self.location {
                Some(l) => (Some(l.line), l.column, l.context, l.line_text),
                None => (None, None, None, None),
            };
            let _ = value.setattr("kind", self.kind);
            let _ = value.setattr("line", line);
            let _ = value.setattr("column", column);
            let _ = value.setattr("context", context);
            let _ = value.setattr("line_text", line_text);
            err
        }
    }

    /// Render a [`FinalState`] as the Python `.state` dict:
    /// `{axes, symbol_table, translation, string_table}`. The three numeric
    /// sub-tables become `dict[str, float]`; `string_table` (DEF STRING
    /// variables) becomes `dict[str, str]`. Shared by both iterators.
    fn final_state_to_py<'py>(py: Python<'py>, state: &FinalState) -> PyResult<Bound<'py, PyDict>> {
        let dict = PyDict::new(py);
        dict.set_item("axes", state.axes.clone())?;
        dict.set_item("symbol_table", state.symbol_table.clone())?;
        dict.set_item("translation", state.translation.clone())?;
        dict.set_item("string_table", state.string_table.clone())?;
        Ok(dict)
    }

    /// Build one Arrow [`RecordBatch`](arrow::record_batch::RecordBatch) directly
    /// from an output [`Table`], in column order. Each [`Column`] becomes an Arrow
    /// array straight from its `Vec` (a `Vec<Option<f64>>` -> Float64,
    /// `Vec<Option<i64>>` -> Int64, `Vec<Option<String>>` -> Utf8, list-of-strings
    /// -> `List(Utf8)`) - a bulk copy into an Arrow buffer with a validity bitmap,
    /// never a per-element Python object. The batch is handed to Python by
    /// [`table_to_arrow_batch`] as a zero-copy Arrow C-stream (PyCapsule); the
    /// Python side wraps it with `pl.DataFrame(...)`. Every field is nullable so
    /// batches with different null patterns keep one schema.
    fn table_to_record_batch(table: Table) -> PyResult<arrow_array::RecordBatch> {
        use arrow_array::builder::{ListBuilder, StringBuilder};
        use arrow_array::{Array, ArrayRef, Float64Array, Int64Array, RecordBatch, StringArray};
        use arrow_schema::{DataType, Field, Schema};

        let mut fields: Vec<Field> = Vec::with_capacity(table.columns.len());
        let mut arrays: Vec<ArrayRef> = Vec::with_capacity(table.columns.len());
        for (name, column) in table.columns {
            let (data_type, array): (DataType, ArrayRef) = match column {
                Column::Float(v) => (DataType::Float64, Arc::new(Float64Array::from(v))),
                Column::Int(v) => (DataType::Int64, Arc::new(Int64Array::from(v))),
                Column::Str(v) => (
                    DataType::Utf8,
                    // FromIterator<Option<S: AsRef<str>>>: None -> null, one bulk
                    // build of the offset + value buffers.
                    Arc::new(v.into_iter().collect::<StringArray>()),
                ),
                Column::StrList(v) => {
                    let values_cap: usize = v.iter().filter_map(|c| c.as_ref()).map(|l| l.len()).sum();
                    let mut builder = ListBuilder::with_capacity(StringBuilder::new(), v.len())
                        .with_field(Arc::new(Field::new("item", DataType::Utf8, false)));
                    let _ = values_cap;
                    for cell in v {
                        match cell {
                            Some(list) => {
                                for s in list {
                                    builder.values().append_value(s);
                                }
                                builder.append(true);
                            }
                            // Typed builder pins the dtype to List(Utf8) even for an
                            // all-null batch, so batches keep one schema.
                            None => builder.append(false),
                        }
                    }
                    let array = builder.finish();
                    (array.data_type().clone(), Arc::new(array))
                }
            };
            fields.push(Field::new(name.as_str(), data_type, true));
            arrays.push(array);
        }
        let schema = Arc::new(Schema::new(fields));
        RecordBatch::try_new(schema, arrays)
            .map_err(|e| PyErr::new::<PyValueError, _>(format!("failed to build RecordBatch: {}", e)))
    }

    /// [`table_to_record_batch`] as an [`ArrowBatch`] the Python side receives
    /// through the Arrow PyCapsule interface (`__arrow_c_array__`) - a zero-copy
    /// handoff turned into a `polars.DataFrame` with `pl.DataFrame(...)`, needing
    /// neither `pyarrow` nor a matching polars version. A `RecordBatch` is a
    /// struct array; converting once here means each export is a cheap
    /// `to_ffi`. Shared by `nc_to_dataframe` (via the batch concat) and
    /// `NcBatchIterator` (one batch).
    fn table_to_arrow_batch(table: Table) -> PyResult<ArrowBatch> {
        use arrow_array::Array;
        let struct_array = arrow_array::StructArray::from(table_to_record_batch(table)?);
        Ok(ArrowBatch {
            data: struct_array.into_data(),
        })
    }

    /// Spawn the interpreter on a worker thread, pushing finished rows into a
    /// bounded channel. Shared by the row and batch streaming iterators.
    #[allow(clippy::too_many_arguments)]
    fn spawn_stream(
        input: String,
        initial_state: Option<String>,
        axis_identifiers: Option<Vec<String>>,
        extra_axes: Option<Vec<String>>,
        iteration_limit: usize,
        axis_index_map: Option<HashMap<String, usize>>,
        allow_undefined_variables: bool,
        flatten_tolerance: Option<f64>,
    ) -> PyResult<(
        mpsc::Receiver<Row>,
        mpsc::Receiver<Result<FinalState, ErrInfo>>,
        std::thread::JoinHandle<()>,
    )> {
        let (row_sender, row_receiver) = mpsc::sync_channel::<Row>(1024);
        let (result_sender, result_receiver) = mpsc::sync_channel::<Result<FinalState, ErrInfo>>(1);

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
                    flatten_tolerance,
                    row_sender,
                );
                let message = match outcome {
                    Ok(state) => Ok(state.final_state()),
                    // The consumer hung up: nothing to report to nobody.
                    Err(crate::errors::ParsingError::StreamClosed) => return,
                    Err(error) => Err(ErrInfo::from_error(&error)),
                };
                let _ = result_sender.send(message);
            })
            .map_err(|e| PyErr::new::<PyValueError, _>(format!("failed to spawn interpreter thread: {}", e)))?;
        Ok((row_receiver, result_receiver, handle))
    }

    /// Spawn the interpreter on a worker thread building whole columnar batches.
    /// Completed [`Table`] batches are pushed into a small bounded channel (a cap
    /// of a few batches keeps memory bounded while letting one batch build while
    /// another is consumed). Used by `nc_to_batches`.
    #[allow(clippy::too_many_arguments)]
    fn spawn_batch_stream(
        input: String,
        batch_size: usize,
        initial_state: Option<String>,
        axis_identifiers: Option<Vec<String>>,
        extra_axes: Option<Vec<String>>,
        iteration_limit: usize,
        disable_forward_fill: bool,
        axis_index_map: Option<HashMap<String, usize>>,
        allow_undefined_variables: bool,
        flatten_tolerance: Option<f64>,
        emit_line_no: bool,
        include_variables: bool,
    ) -> PyResult<(
        mpsc::Receiver<Table>,
        mpsc::Receiver<Result<FinalState, ErrInfo>>,
        mpsc::Receiver<VariableEvents>,
        std::thread::JoinHandle<()>,
    )> {
        // A cap of 3 lets the worker build one batch ahead while the consumer
        // converts the previous one to a polars DataFrame, without unbounded
        // buffering.
        let (batch_sender, batch_receiver) = mpsc::sync_channel::<Table>(3);
        let (result_sender, result_receiver) = mpsc::sync_channel::<Result<FinalState, ErrInfo>>(1);
        // Variable-change events are a small sparse side-table sent once at the
        // end; an unbounded channel so the worker never blocks emitting them
        // (they are only read after the batch stream is drained).
        let (events_sender, events_receiver) = mpsc::channel::<VariableEvents>();

        let handle = std::thread::Builder::new()
            .name("nc-interpreter".to_string())
            .spawn(move || {
                let outcome = nc_to_batch_stream_with_line_numbers(
                    &input,
                    initial_state.as_deref(),
                    axis_identifiers,
                    extra_axes,
                    iteration_limit,
                    disable_forward_fill,
                    axis_index_map,
                    allow_undefined_variables,
                    flatten_tolerance,
                    batch_size,
                    emit_line_no,
                    batch_sender,
                    include_variables,
                    events_sender,
                );
                let message = match outcome {
                    Ok(state) => Ok(state.final_state()),
                    // The consumer hung up: nothing to report to nobody.
                    Err(crate::errors::ParsingError::StreamClosed) => return,
                    Err(error) => Err(ErrInfo::from_error(&error)),
                };
                let _ = result_sender.send(message);
            })
            .map_err(|e| PyErr::new::<PyValueError, _>(format!("failed to spawn interpreter thread: {}", e)))?;
        Ok((batch_receiver, result_receiver, events_receiver, handle))
    }

    /// Iterator over interpreted rows: `next()` yields
    /// `(line_no, {column: value})` while the interpreter runs on a worker
    /// thread behind a bounded channel. Dropping the iterator hangs up the
    /// channel and aborts interpretation; note that in Python, breaking out
    /// of a loop only drops it when no other reference keeps it alive.
    #[pyclass]
    struct NcRowIterator {
        rows: Option<Mutex<mpsc::Receiver<crate::output::Row>>>,
        result: Option<Mutex<mpsc::Receiver<Result<FinalState, ErrInfo>>>>,
        handle: Option<std::thread::JoinHandle<()>>,
        /// Running forward-fill values, keyed by interned column name.
        fill: HashMap<&'static str, Value>,
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
                    Ok(Err(info)) => {
                        self.join();
                        return Err(info.into_pyerr(py));
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
                                for (&key, value) in row.cells.iter() {
                                    if is_forward_filled_column(key) {
                                        self.fill.insert(key, value.clone());
                                    }
                                }
                            }
                            // Forward-filled columns first, then the row's own
                            // values (its fillable ones are already in `fill`).
                            for (&key, value) in &self.fill {
                                dict.set_item(key, Self::cell_to_py(py, key, value)?)?;
                            }
                            for (&key, value) in row.cells.iter() {
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

        /// The final interpreter state
        /// (`{axes, symbol_table, translation, string_table}`), available once
        /// the iterator is exhausted.
        #[getter]
        fn state<'py>(&self, py: Python<'py>) -> PyResult<Option<Bound<'py, PyDict>>> {
            self.state.as_ref().map(|s| final_state_to_py(py, s)).transpose()
        }
    }

    /// Interpret an NC program lazily: returns an iterator of
    /// `(line_no, row_dict)` — or `(line_no, row_dict, variables_dict)` when
    /// `include_variables` is set — produced by the interpreter running on a
    /// worker thread. Rows are forward-filled like the batch DataFrame
    /// unless `forward_fill` is false.
    #[pyfunction]
    #[pyo3(signature = (input, initial_state = None, axis_identifiers = None, extra_axes = None, iteration_limit = 10000, forward_fill = true, include_variables = false, axis_index_map = None, allow_undefined_variables = false, input_is_path = false, flatten_tolerance = None))]
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
        input_is_path: bool,
        flatten_tolerance: Option<f64>,
    ) -> PyResult<NcRowIterator> {
        // When `input_is_path` is set, `input` is a filesystem path: read the
        // program here (once) instead of copying a 1.1 GB Python str across the
        // PyO3 boundary. Reading before the worker spawns surfaces a missing
        // file as an immediate, clean error.
        let input = if input_is_path {
            std::fs::read_to_string(&input)
                .map_err(|e| PyErr::new::<PyValueError, _>(format!("Error reading input file '{}': {}", input, e)))?
        } else {
            input
        };
        let (row_receiver, result_receiver, handle) = spawn_stream(
            input,
            initial_state,
            axis_identifiers,
            extra_axes,
            iteration_limit,
            axis_index_map,
            allow_undefined_variables,
            flatten_tolerance,
        )?;

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

    /// Iterator over columnar batches: `next()` returns an Arrow record batch
    /// (via `pyo3-arrow`'s PyCapsule export) for up to `batch_size` output rows,
    /// built column-wise in Rust, while the interpreter runs on a worker thread
    /// behind a bounded channel. Memory stays bounded by the batch size.
    /// A `BatchBuilder` carries forward-fill state and the growing canonical
    /// column set across batches, so concatenating the batches reconstructs the
    /// whole-file `nc_to_dataframe` table.
    #[pyclass]
    struct NcBatchIterator {
        batches: Option<Mutex<mpsc::Receiver<Table>>>,
        result: Option<Mutex<mpsc::Receiver<Result<FinalState, ErrInfo>>>>,
        /// Set at construction only when `include_variables`; drained at finish
        /// into `variable_events`.
        events: Option<Mutex<mpsc::Receiver<VariableEvents>>>,
        handle: Option<std::thread::JoinHandle<()>>,
        state: Option<FinalState>,
        /// The sparse variable-change side-table, available once exhausted.
        /// `None` unless `include_variables` was set.
        variable_events: Option<VariableEvents>,
    }

    impl NcBatchIterator {
        fn finish(&mut self, py: Python<'_>) -> PyResult<()> {
            if let Some(result) = self.result.take() {
                let receiver = result.into_inner().expect("result receiver mutex poisoned");
                match py.detach(move || receiver.recv()) {
                    Ok(Ok(state)) => self.state = Some(state),
                    Ok(Err(info)) => {
                        self.join();
                        return Err(info.into_pyerr(py));
                    }
                    Err(_) => {}
                }
            }
            // The worker sends the events (once) before dropping the batch
            // sender, so by the time the batch stream has closed they are
            // already queued on the unbounded channel; a hung-up / disabled run
            // yields `Err` and leaves `variable_events` as `None`.
            if let Some(events) = self.events.take() {
                let receiver = events.into_inner().expect("events receiver mutex poisoned");
                if let Ok(ev) = py.detach(move || receiver.recv()) {
                    self.variable_events = Some(ev);
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
    impl NcBatchIterator {
        fn __iter__(slf: PyRef<'_, Self>) -> PyRef<'_, Self> {
            slf
        }

        fn __next__(&mut self, py: Python<'_>) -> PyResult<Option<Py<PyAny>>> {
            let Some(mutex) = self.batches.take() else {
                // The channel closed on a previous call (its final batch was
                // already yielded); finalize now - capture the state or raise
                // the interpreter's error, mirroring nc_to_rows.
                self.finish(py)?;
                return Ok(None);
            };
            // Wait for the next whole batch with the GIL released; the owned
            // Receiver moves into the detached closure and back out. The worker
            // built the columnar `Table` already (off this thread), so all that
            // remains here is converting it to a polars DataFrame below.
            let receiver = mutex.into_inner().expect("batch receiver mutex poisoned");
            let (received, receiver) = py.detach(move || {
                let received = receiver.recv();
                (received, receiver)
            });
            match received {
                Ok(table) => {
                    self.batches = Some(Mutex::new(receiver));
                    let batch = table_to_arrow_batch(table)?;
                    Ok(Some(Py::new(py, batch)?.into_any()))
                }
                // Channel closed: interpretation is done (or failed). Capture
                // the final state / raise the error.
                Err(_) => {
                    self.batches = None;
                    self.finish(py)?;
                    Ok(None)
                }
            }
        }

        /// The final interpreter state
        /// (`{axes, symbol_table, translation, string_table}`), available once
        /// the iterator is exhausted.
        #[getter]
        fn state<'py>(&self, py: Python<'py>) -> PyResult<Option<Bound<'py, PyDict>>> {
            self.state.as_ref().map(|s| final_state_to_py(py, s)).transpose()
        }

        /// The sparse variable-change events (`include_variables=True`) as an
        /// Arrow record batch with columns `row_idx` (Int64, the output-row
        /// index the change is seen at), `name_id` (Int64, index into
        /// `variable_names`) and `value` (Float64). `None` when the iterator was
        /// created without `include_variables`. Available once exhausted.
        /// `pl.DataFrame(it.variable_events)` wraps it zero-copy.
        #[getter]
        fn variable_events(&self) -> PyResult<Option<ArrowBatch>> {
            self.variable_events
                .as_ref()
                .map(|ev| table_to_arrow_batch(ev.to_table()))
                .transpose()
        }

        /// The variable names `name_id` indexes into (in first-seen order).
        /// `None` unless `include_variables` was set; available once exhausted.
        #[getter]
        fn variable_names(&self) -> Option<Vec<String>> {
            self.variable_events.as_ref().map(|ev| ev.names.clone())
        }
    }

    /// Interpret an NC program lazily into columnar batches: returns an iterator
    /// of Arrow record batches (transferred zero-copy via `pyo3-arrow`), each
    /// covering up to `batch_size` output rows and built column-wise on a worker
    /// thread. Wrapping each with `pl.DataFrame` and concatenating reconstructs
    /// `nc_to_dataframe`.
    #[pyfunction]
    #[pyo3(signature = (input, batch_size = 500_000, initial_state = None, axis_identifiers = None, extra_axes = None, iteration_limit = 10000, disable_forward_fill = false, axis_index_map = None, allow_undefined_variables = false, input_is_path = false, flatten_tolerance = None, include_line_numbers = false, include_variables = false))]
    #[allow(clippy::too_many_arguments)]
    fn nc_to_batches(
        input: String,
        batch_size: usize,
        initial_state: Option<String>,
        axis_identifiers: Option<Vec<String>>,
        extra_axes: Option<Vec<String>>,
        iteration_limit: usize,
        disable_forward_fill: bool,
        axis_index_map: Option<HashMap<String, usize>>,
        allow_undefined_variables: bool,
        input_is_path: bool,
        flatten_tolerance: Option<f64>,
        include_line_numbers: bool,
        include_variables: bool,
    ) -> PyResult<NcBatchIterator> {
        if batch_size == 0 {
            return Err(PyErr::new::<PyValueError, _>("batch_size must be greater than 0"));
        }
        // Read the program here when given a path (see nc_to_rows).
        let input = if input_is_path {
            std::fs::read_to_string(&input)
                .map_err(|e| PyErr::new::<PyValueError, _>(format!("Error reading input file '{}': {}", input, e)))?
        } else {
            input
        };
        let (batch_receiver, result_receiver, events_receiver, handle) = spawn_batch_stream(
            input,
            batch_size,
            initial_state,
            axis_identifiers,
            extra_axes,
            iteration_limit,
            disable_forward_fill,
            axis_index_map,
            allow_undefined_variables,
            flatten_tolerance,
            include_line_numbers,
            include_variables,
        )?;

        Ok(NcBatchIterator {
            batches: Some(Mutex::new(batch_receiver)),
            result: Some(Mutex::new(result_receiver)),
            // Only hold the events receiver when recording; otherwise leave it
            // `None` so `.variable_events` stays `None`.
            events: if include_variables {
                Some(Mutex::new(events_receiver))
            } else {
                None
            },
            handle: Some(handle),
            state: None,
            variable_events: None,
        })
    }

    /// Define the Python module
    #[pymodule(name = "_internal")]
    pub fn nc_gcode_interpreter(_py: Python<'_>, m: &Bound<'_, PyModule>) -> PyResult<()> {
        m.add_function(wrap_pyfunction!(nc_to_rows, m)?)?;
        m.add_function(wrap_pyfunction!(nc_to_batches, m)?)?;
        m.add_class::<NcRowIterator>()?;
        m.add_class::<NcBatchIterator>()?;
        m.add("NcError", m.py().get_type::<NcError>())?;
        Ok(())
    }
}

#[cfg(feature = "python")]
pub use python_bindings::nc_gcode_interpreter;
