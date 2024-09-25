//lib.rs
#[macro_use]
extern crate pest_derive;
mod types;

use pyo3::exceptions::PyValueError;
use pyo3_polars::PyDataFrame;
use std::collections::HashMap;

mod errors;
mod interpret_rules;
mod interpreter;
mod state;
mod modal_groups;

use pyo3::prelude::*;
use pyo3::wrap_pyfunction;

use interpreter::nc_to_dataframe as nc_to_dataframe_rust;

#[pyfunction]
#[pyo3(signature = (input, initial_state = None, axis_identifiers = None, extra_axes = None,iteration_limit = 10000, disable_forward_fill = false))]
fn nc_to_dataframe(
    input: &str,
    initial_state: Option<String>,
    axis_identifiers: Option<Vec<String>>,
    extra_axes: Option<Vec<String>>,
    iteration_limit: usize,
    disable_forward_fill: bool,
) -> PyResult<(PyDataFrame, HashMap<String, HashMap<String, f32>>)> {
    let (df,state) = nc_to_dataframe_rust(input, initial_state.as_deref(), axis_identifiers, extra_axes, iteration_limit, disable_forward_fill)
        .map_err(|e| PyErr::new::<PyValueError, _>(format!("Error creating DataFrame: {:?}", e)))?;

    Ok((PyDataFrame(df), state.to_python_dict()))
}


/// Define the Python module
#[pymodule]
fn nc_gcode_interpreter(_py: Python<'_>, m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(nc_to_dataframe, m)?)?;
    Ok(())
}
