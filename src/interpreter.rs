//interpreter.rs
use crate::errors::ParsingError;
use crate::interpret_rules::interpret_blocks;
use crate::modal_groups::{MODAL_G_GROUPS, NON_MODAL_G_GROUPS};
use crate::state::{self, State};
use crate::types::{NCParser, Rule, Value};
use pest::Parser;
use polars::chunked_array::ops::FillNullStrategy;
use polars::prelude::*;
use std::collections::{HashMap, HashSet};

/// Helper function to convert PolarsError to ParsingError
impl From<PolarsError> for ParsingError {
    fn from(err: PolarsError) -> Self {
        ParsingError::ParseError {
            message: format!("Polars error: {:?}", err),
        }
    }
}

const DEFAULT_AXIS_IDENTIFIERS: &[&str] = &[
    "N", "X", "Y", "Z", "A", "B", "C", "D", "E", "F", "S", "U", "V", "RA1", "RA2", "RA3", "RA4", "RA5", "RA6",
];

/// Main function to interpret input to DataFrame
pub fn nc_to_dataframe(
    input: &str,
    initial_state: Option<&str>,
    axis_identifiers: Option<Vec<String>>,
    extra_axes: Option<Vec<String>>,
    iteration_limit: usize,
    disable_forward_fill: bool,
    axis_index_map: Option<HashMap<String, usize>>, // axis identifier to index mapping
) -> Result<(DataFrame, state::State), ParsingError> {
    // Default axis identifiers

    // Use the override if provided, otherwise use the default identifiers
    let axis_identifiers: Vec<String> =
        axis_identifiers.unwrap_or_else(|| DEFAULT_AXIS_IDENTIFIERS.iter().map(|&s| s.to_string()).collect());

    // Add extra axes to the existing list if provided
    let mut axis_identifiers = axis_identifiers;
    if let Some(extra_axes) = extra_axes {
        axis_identifiers.extend(extra_axes);
    }

    // Pass axis_index_map to State::new
    let mut state = state::State::new(axis_identifiers.clone(), iteration_limit, axis_index_map);
    if let Some(initial_state) = initial_state {
        if let Err(error) = interpret_file(initial_state, &mut state) {
            eprintln!("Error while parsing defaults: {:?}", error);
            std::process::exit(1);
        }
    }

    // Now interpret the main input using the axis_index_map from state
    let results = interpret_file(input, &mut state)?;

    // Convert results to DataFrame
    let mut df = results_to_dataframe(results)?;

    df = sanitize_dataframe(df, disable_forward_fill)?;
    Ok((df, state))
}

// pub fn sanitize_dataframe(
//     df: DataFrame,
//     disable_forward_fill: bool,
// ) -> Result<(DataFrame), ParsingError> {
//     // - MODAL_G_GROUPS: string, g commands that persist
//     // - NON_MODAL_G_GROUPS: string
//     // - "function_call": string
//     // - "comment": string
//     // - "T": tool changes, string
//     // - "M": M commands, list of strings
//     // = "N": line numbers, Type int64 Should be the first column
//     // axis_identifiers: all other columns. Type float64

pub fn sanitize_dataframe(mut df: DataFrame, disable_forward_fill: bool) -> Result<DataFrame, ParsingError> {
    // Define expected types for specific columns
    let mut expected_types: Vec<(&str, DataType)> = Vec::new();

    // Collect MODAL_G_GROUPS and NON_MODAL_G_GROUPS into HashSet
    let modal_g_groups: HashSet<&str> = MODAL_G_GROUPS.iter().cloned().collect();
    let non_modal_g_groups: HashSet<&str> = NON_MODAL_G_GROUPS.iter().cloned().collect();

    // Collect known columns by combining MODAL_G_GROUPS and NON_MODAL_G_GROUPS
    let mut known_columns: Vec<&str> = modal_g_groups.union(&non_modal_g_groups).cloned().collect();
    known_columns.extend(&["non_returning_function_call", "comment", "T", "M"]);

    // Collect column names from the DataFrame as Strings (to avoid immutable borrows)
    let column_names: Vec<String> = df.get_column_names().iter().map(|s| s.to_string()).collect();

    // Determine axis identifiers (columns not in known_columns)
    let axis_identifiers: HashSet<String> = column_names
        .iter()
        .filter(|col| !known_columns.contains(&col.as_str()))
        .cloned()
        .collect();

    // Insert all known columns into `expected_types` with their expected DataTypes (in desired order)
    expected_types.push(("N", DataType::Int64)); // Line numbers

    // Insert G group columns as DataType::String
    for &col in MODAL_G_GROUPS.iter().chain(NON_MODAL_G_GROUPS.iter()) {
        expected_types.push((col, DataType::String));
    }

    // Insert specific axis columns
    for &col in &[
        "X", "Y", "Z", "A", "B", "C", "D", "E", "F", "S", "U", "V", "RA1", "RA2", "RA3", "RA4", "RA5", "RA6",
    ] {
        expected_types.push((col, DataType::Float64));
    }

    // Insert all axis identifiers that are not already in `expected_types`
    for col in &axis_identifiers {
        if !expected_types.iter().any(|&(name, _)| name == col.as_str()) {
            expected_types.push((col.as_str(), DataType::Float64));
        }
    }

    // Insert other known columns
    expected_types.push(("T", DataType::String)); // Tool changes
    expected_types.push(("M", DataType::List(Box::new(DataType::String)))); // M Codes
    expected_types.push(("non_returning_function_call", DataType::String)); // Function calls
    expected_types.push(("comment", DataType::String)); // Comments

    // Iterate over each expected column and apply necessary type casting if available in the DataFrame
    for (col_name, expected_dtype) in &expected_types {
        if let Some(current_dtype) = df.column(col_name).ok().map(|c| c.dtype()) {
            if current_dtype != expected_dtype {
                let casted_series = df.column(col_name)?.cast(expected_dtype)?;
                // Mutable operation after ensuring no immutable borrow remains
                df.replace_or_add((*col_name).into(), casted_series)?;
            }
        }
    }

    // Build the list of ordered columns that are available in the DataFrame
    let ordered_columns: Vec<String> = expected_types
        .iter()
        .filter(|&&(col_name, _)| column_names.contains(&col_name.to_string()))
        .map(|&(s, _)| s.to_string())
        .collect();

    // Reassign the reordered DataFrame to `df`
    df = df
        .select(ordered_columns.iter().map(|s| PlSmallStr::from_str(s)))
        .map_err(ParsingError::from)?;

    // Handle forward fill if it's not disabled
    if !disable_forward_fill {
        let fill_columns: Vec<String> = df
            .get_column_names()
            .iter()
            .map(|s| s.to_string())
            .filter(|col| axis_identifiers.contains(col) || modal_g_groups.contains(col.as_str()))
            .collect();

        for col_name in fill_columns {
            let column = df.column(&col_name)?;
            let filled_column = column.fill_null(FillNullStrategy::Forward(None))?;
            df.replace_or_add(col_name.into(), filled_column)?;
        }
    }

    Ok(df)
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

    let mut file = std::fs::File::create(path).map_err(|e| PolarsError::ComputeError(format!("{:?}", e).into()))?;

    CsvWriter::new(&mut file)
        .with_float_precision(Some(3))
        .finish(df)
        .map_err(|e| PolarsError::ComputeError(format!("{:?}", e).into()))?;
    Ok(())
}

/// Parse file and return results as a vector of HashMaps
fn interpret_file(input: &str, state: &mut State) -> Result<Vec<HashMap<String, Value>>, ParsingError> {
    // Store input for error messages
    state.set_input(input.to_string());

    // Initialize results with an empty HashMap
    let mut results = vec![HashMap::new()];

    let file = NCParser::parse(Rule::file, input)
        .map_err(|e| {
            let (line, _col) = match &e.line_col {
                pest::error::LineColLocation::Pos(pos) => *pos,
                pest::error::LineColLocation::Span(start, _) => *start,
            };
            let preview = state.get_line(line).unwrap_or("(could not retrieve line)").to_string();
            ParsingError::with_context(line, preview, "initial file parsing".to_string(), format!("{}", e))
        })?
        .next()
        .ok_or_else(|| ParsingError::ParseError {
            message: "No blocks found".to_string(),
        })?;

    let blocks = file
        .into_inner()
        .next()
        .ok_or_else(|| ParsingError::ParseError {
            message: "No inner blocks found".to_string(),
        })?;

    interpret_blocks(blocks, &mut results, state)?;
    Ok(results)
}

fn results_to_dataframe(data: Vec<HashMap<String, Value>>) -> PolarsResult<DataFrame> {
    // Step 1: Collect all unique keys (column names)
    let columns: Vec<String> = data
        .iter()
        .flat_map(|row| row.keys().cloned())
        .collect::<std::collections::HashSet<String>>() // Deduplicate keys
        .into_iter()
        .collect();

    // Step 2: Initialize empty columns (vectors) for each key
    let mut series_map: HashMap<String, Vec<Option<AnyValue>>> =
        columns.iter().map(|key| (key.clone(), Vec::new())).collect();

    // Step 3: Populate the columns with data, inserting None where keys are missing
    for row in &data {
        if row.is_empty() {
            // Skip rows with no values
            continue;
        }

        for key in &columns {
            let column_data = series_map.get_mut(key).unwrap();
            column_data.push(row.get(key).map(|v| v.to_polars_value()));
        }
    }

    // Step 4: Convert each column to a Polars Series
    let polars_series: Vec<Series> = columns
        .iter()
        .map(|key| {
            let column_data = series_map.remove(key).unwrap();
            Series::new(
                key.as_str().into(), // Convert `&String` to `PlSmallStr` using `Into::into`
                column_data
                    .into_iter()
                    .map(|opt| opt.unwrap_or(AnyValue::Null))
                    .collect::<Vec<AnyValue>>(),
            )
        })
        .collect();

    // Step 5: Create the DataFrame
    DataFrame::new(polars_series)
}
