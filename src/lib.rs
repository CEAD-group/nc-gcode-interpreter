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
mod modal_groups;
mod state;

use pyo3::prelude::*;
use pyo3::wrap_pyfunction;

use interpreter::nc_to_dataframe as nc_to_dataframe_rust;
use interpreter::sanitize_dataframe as sanitize_dataframe_rust;

#[pyfunction]
#[pyo3(signature = (input, initial_state = None, axis_identifiers = None, extra_axes = None, iteration_limit = 10000, disable_forward_fill = false, axis_index_map = None))]
fn nc_to_dataframe(
    input: &str,
    initial_state: Option<String>,
    axis_identifiers: Option<Vec<String>>,
    extra_axes: Option<Vec<String>>,
    iteration_limit: usize,
    disable_forward_fill: bool,
    axis_index_map: Option<HashMap<String, usize>>,
) -> PyResult<(PyDataFrame, HashMap<String, HashMap<String, f32>>)> {
    let (df, state) = nc_to_dataframe_rust(
        input,
        initial_state.as_deref(),
        axis_identifiers,
        extra_axes,
        iteration_limit,
        disable_forward_fill,
        axis_index_map, // Pass through
    )
    .map_err(|e| PyErr::new::<PyValueError, _>(format!("Error creating DataFrame: {:?}", e)))?;

    Ok((PyDataFrame(df), state.to_python_dict()))
}

#[pyfunction]
#[pyo3(signature = (df, disable_forward_fill = false))]
fn sanitize_dataframe(df: PyDataFrame, disable_forward_fill: bool) -> PyResult<PyDataFrame> {
    // Convert PyDataFrame to Rust DataFrame
    let rust_df = df.into();

    // Call the Rust function to sanitize the DataFrame
    let sanitized_df = sanitize_dataframe_rust(rust_df, disable_forward_fill)
        .map_err(|e| PyErr::new::<PyValueError, _>(format!("Error sanitizing DataFrame: {:?}", e)))?;

    // Return the sanitized DataFrame back to Python
    Ok(PyDataFrame(sanitized_df))
}

/// Define the Python module
#[pymodule(name = "_internal")]
fn nc_gcode_interpreter(_py: Python<'_>, m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(nc_to_dataframe, m)?)?;
    m.add_function(wrap_pyfunction!(sanitize_dataframe, m)?)?;
    Ok(())
}
