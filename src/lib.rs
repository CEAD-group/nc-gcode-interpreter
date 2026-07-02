//lib.rs
#[macro_use]
extern crate pest_derive;
mod types;

mod errors;
mod interpret_rules;
pub mod interpreter;
mod modal_groups;
pub mod output;
mod state;

#[cfg(feature = "python")]
mod python_bindings {
    use pyo3::exceptions::PyValueError;
    use pyo3::prelude::*;
    use pyo3::types::PyDict;
    use pyo3::wrap_pyfunction;
    use std::collections::HashMap;

    use crate::interpreter::nc_to_table;
    use crate::output::Column;

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
        HashMap<String, HashMap<String, f32>>,
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
        .map_err(|e| PyErr::new::<PyValueError, _>(format!("Error creating DataFrame: {:?}", e)))?;

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

    /// Define the Python module
    #[pymodule(name = "_internal")]
    pub fn nc_gcode_interpreter(_py: Python<'_>, m: &Bound<'_, PyModule>) -> PyResult<()> {
        m.add_function(wrap_pyfunction!(nc_to_columns, m)?)?;
        Ok(())
    }
}

#[cfg(feature = "python")]
pub use python_bindings::nc_gcode_interpreter;
