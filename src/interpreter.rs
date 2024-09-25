//interpreter.rs
use pest::Parser;
use polars::prelude::*;
use std::collections::HashMap;
use std::io::{self};
use polars::chunked_array::ops::FillNullStrategy;

use crate::errors::ParsingError;
use crate::interpret_rules::interpret_blocks;
use crate::state::{self, State};
use crate::types::{NCParser, Rule, Value};
use crate::modal_groups::MODAL_GROUPS;


/// Helper function to convert PolarsError to ParsingError
impl From<PolarsError> for ParsingError {
    fn from(err: PolarsError) -> Self {
        ParsingError::ParseError {
            message: format!("Polars error: {:?}", err),
        }
    }
}

/// Main function to interpret input to DataFrame
pub fn nc_to_dataframe(
    input: &str,
    initial_state: Option<&str>,
    axis_identifiers: Option<Vec<String>>,
    extra_axes: Option<Vec<String>>,
    iteration_limit: usize,
    disable_forward_fill: bool,

) -> Result<(DataFrame, state::State), ParsingError> {
    // Default axis identifiers
    const DEFAULT_AXIS_IDENTIFIERS: &[&str] = &[
        "N", "X", "Y", "Z", "A", "B", "C", "D", "E", "F", "S", "U", "V", "RA1", "RA2", "RA3", "RA4", "RA5", "RA6",
    ];

    // Use the override if provided, otherwise use the default identifiers
    let axis_identifiers: Vec<String> = axis_identifiers.unwrap_or_else(|| {
        DEFAULT_AXIS_IDENTIFIERS.iter().map(|&s| s.to_string()).collect()
    });

    // Add extra axes to the existing list if provided
    let mut axis_identifiers = axis_identifiers;
    if let Some(extra_axes) = extra_axes {
        axis_identifiers.extend(extra_axes);
    }

    // Process the defaults file first, if provided. This will set up the initial state
    let mut state = state::State::new(axis_identifiers.clone(), iteration_limit);
    if let Some(initial_state) = initial_state {
        if let Err(error) = interpret_file(initial_state, &mut state) {
            eprintln!("Error while parsing defaults: {:?}", error);
            std::process::exit(1);
        }
    }

    // Now interpret the main input
    let results = interpret_file(input, &mut state)?;

    // Convert results to DataFrame
    let mut df = results_to_dataframe(results)?;

    // Get the column names and reorder them
    let ordered_columns = reorder_columns(
        axis_identifiers,
        df.get_column_names().into_iter().map(|s| s.to_string()).collect()
    );

    // Select the columns in the specified order and return
    df = df.select(&ordered_columns).map_err(ParsingError::from)?;



    if !disable_forward_fill {

        let fill_columns: Vec<String> = df
        .get_column_names()
        .iter()
        .map(|col| col.to_string())
        .filter(|col| state.is_axis(col) || MODAL_GROUPS.contains(&col.as_str()))
        .collect();

        

    for col_name in fill_columns {
        let column = df
            .column(&col_name)
            .map_err(|e| io::Error::new(io::ErrorKind::Other, e.to_string()))?;
        let filled_column = column
            .fill_null(FillNullStrategy::Forward(None))
            .map_err(|e| io::Error::new(io::ErrorKind::Other, e.to_string()))?;
        df.with_column(filled_column)
            .map_err(|e| io::Error::new(io::ErrorKind::Other, e.to_string()))?;
    }
    }




    Ok((df, state))
}


#[allow(dead_code)] // Only used in main.rs, not in lib.rs
pub fn dataframe_to_csv(df: &mut DataFrame, path: &str) -> Result<(), PolarsError> {
    // Get all column names that are of List type
    let list_columns: Vec<String> = df
        .dtypes()
        .iter()
        .enumerate()
        .filter_map(|(idx, dtype)| {
            if matches!(dtype, DataType::List(_)) {
                Some(df.get_column_names()[idx].to_string())
            } else {
                None
            }
        })
        .collect();

    // Explode all list columns
    if !list_columns.is_empty() {
        let exploded_df = df.explode(list_columns)?;
        *df = exploded_df;
    }

    let mut file = std::fs::File::create(path)
        .map_err(|e| PolarsError::ComputeError(format!("{:?}", e).into()))?;

    CsvWriter::new(&mut file)
        .with_float_precision(Some(3))
        .finish(df)
        .map_err(|e| PolarsError::ComputeError(format!("{:?}", e).into()))?;
    Ok(())
}


/// Parse file and return results as a vector of HashMaps
fn interpret_file(input: &str, state: &mut State) -> Result<Vec<HashMap<String, Value>>, ParsingError> {
    let blocks = NCParser::parse(Rule::file, input)
        .map_err(|e| ParsingError::ParseError {
            message: format!("Parse error: {:?}", e),
        })?
        .next()
        .ok_or_else(|| ParsingError::ParseError {
            message: String::from("No blocks found"),
        })?
        .into_inner()
        .next()
        .ok_or_else(|| ParsingError::ParseError {
            message: String::from("No inner blocks found"),
        })?;

    let mut results = Vec::new();
    interpret_blocks(blocks, &mut results, state).map_err(|e| ParsingError::ParseError {
        message: format!("Parse blocks error: {:?}", e),
    })?;

    Ok(results)
}

fn results_to_dataframe(data: Vec<HashMap<String, Value>>) -> PolarsResult<DataFrame> {
    // Step 1: Collect all unique keys (column names)
    let columns: Vec<String> = data.iter()
        .flat_map(|row| row.keys().cloned())
        .collect::<std::collections::HashSet<String>>()  // Deduplicate keys
        .into_iter()
        .collect();

    // Step 2: Initialize empty columns (vectors) for each key
    let mut series_map: HashMap<String, Vec<Option<AnyValue>>> = columns
        .iter()
        .map(|key| (key.clone(), Vec::new()))
        .collect();

    // Step 3: Populate the columns with data, inserting None where keys are missing
    for row in &data {
        if row.is_empty() {  // Skip rows with no values
            continue;
        }

        for key in &columns {
            let column_data = series_map.get_mut(key).unwrap();
            column_data.push(row.get(key).map(|v| v.to_polars_value()));
        }
    }

    // Step 4: Convert each column to a Polars Series
    let polars_series: Vec<Series> = columns.iter()
        .map(|key| {
            let column_data = series_map.remove(key).unwrap();
            Series::new(
                key.as_str().into(),  // Convert `&String` to `PlSmallStr` using `Into::into`
                column_data.into_iter().map(|opt| opt.unwrap_or(AnyValue::Null)).collect::<Vec<AnyValue>>(),
            )
        })
        .collect();

    // Step 5: Create the DataFrame
    DataFrame::new(polars_series)
}

/// Helper function to reorder DataFrame columns
fn reorder_columns(
    axis_identifiers: Vec<String>,
    mut df_columns: Vec<String>
) -> Vec<String> {
    let mut ordered_columns = Vec::new();
    df_columns.sort();  // Sort columns alphabetically
    // Move axis columns first
    for axis in axis_identifiers {
        if let Some(index) = df_columns.iter().position(|col| col == &axis) {
            ordered_columns.push(df_columns.remove(index));
        }
    }

    // Append the remaining (non-axis) columns
    ordered_columns.append(&mut df_columns);

    // Ensure "comment" column is last if present
    if let Some(index) = ordered_columns.iter().position(|col| col == "comment") {
        let comment_column = ordered_columns.remove(index);
        ordered_columns.push(comment_column);
    }

    ordered_columns
}


