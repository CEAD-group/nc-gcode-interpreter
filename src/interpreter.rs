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
use std::io::{self};

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
    let mut expected_types: HashMap<String, DataType> = HashMap::new();

    // MODAL_G_GROUPS and NON_MODAL_G_GROUPS should be of type String
    let modal_g_groups: HashSet<String> = MODAL_G_GROUPS.iter().map(|s| s.to_string()).collect();
    let non_modal_g_groups: HashSet<String> = NON_MODAL_G_GROUPS.iter().map(|s| s.to_string()).collect();

    // Combine modal groups into a single set of known columns
    let mut known_columns: HashSet<String> = modal_g_groups.union(&non_modal_g_groups).cloned().collect();

    // Add these columns to the known_columns set
    known_columns.extend(vec![
        "function_call".to_string(),
        "comment".to_string(),
        "T".to_string(),
        "M".to_string(),
        "N".to_string(),
    ]);

    // Collect column names from the DataFrame
    let column_names: Vec<&PlSmallStr> = df.get_column_names().into_iter().collect();

    // Determine axis identifiers by excluding known columns
    let axis_identifiers: HashSet<&PlSmallStr> = column_names
        .iter()
        .filter(|col| !known_columns.contains(&col.to_string()))
        .cloned()
        .collect();

    // Assign expected types for known columns. Do this in the order they will apear in the output
    expected_types.insert("N".to_string(), DataType::Int64); // Line numbers

    // insert know axis identifiers in a nice order
    for col in [
        "X", "Y", "Z", "A", "B", "C", "D", "E", "F", "S", "U", "V", "RA1", "RA2", "RA3", "RA4", "RA5", "RA6",
    ]
    .iter()
    {
        expected_types.insert(col.to_string(), DataType::Float64);
    }
    // insert all axis identifiers that are not already in the known columns
    for col in axis_identifiers.iter() {
        if expected_types.contains_key(&col.to_string()) {
            continue;
        }
        expected_types.insert(col.to_string(), DataType::Float64);
    }
    expected_types.insert("T".to_string(), DataType::String); // Tool changes
    expected_types.insert("M".to_string(), DataType::List(Box::new(DataType::String))); // M Codes
    expected_types.insert("function_call".to_string(), DataType::String); // Function calls
    expected_types.insert("comment".to_string(), DataType::String); // Comments go last

    // Iterate over each column in the DataFrame
    for col_name in &column_names {
        if let Some(expected_dtype) = expected_types.get(&col_name.to_string()) {
            let current_dtype = df.column(col_name)?.dtype();
            if current_dtype != expected_dtype {
                // Attempt to cast the column to the expected type
                let casted_series =
                    df.column(col_name)?
                        .cast(expected_dtype)
                        .map_err(|e| ParsingError::ParseError {
                            message: format!("Error casting column {}: {:?}", col_name, e),
                        })?;
                // Replace the column in the DataFrame
                df.replace_or_add(**col_name, casted_series)
                    .map_err(|e| ParsingError::ParseError {
                        message: format!("Error replacing column {}: {:?}", col_name, e),
                    })?;
            }
        } else {
            // Raise an error if the column is not recognized
            return Err(ParsingError::ParseError {
                message: format!("Unrecognized column: {}", col_name),
            });
        }
    }

    let mut ordered_columns: Vec<PlSmallStr> = vec![];
    for col_name in expected_types.keys() {
        if column_names.contains(&&PlSmallStr::from_str(&col_name)) {
            ordered_columns.push(PlSmallStr::from_str(&col_name));
        }
    }

    df = df.select(ordered_columns).map_err(ParsingError::from)?;

    // Forward fill if not disabled
    if !disable_forward_fill {
        let fill_columns: Vec<PlSmallStr> = df
            .get_column_names()
            .iter()
            .filter(|col| axis_identifiers.contains(col.as_str()) || modal_g_groups.contains(col.as_str()))
            .collect();

        for col_name in fill_columns {
            let column = df.column(&col_name)?;
            let filled_column = column.fill_null(FillNullStrategy::Forward(None))?;
            df.replace_or_add(&filled_column)?;
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

/// Function to convert DataFrame back to NC code
pub fn dataframe_to_nc(
    df: &DataFrame,
    output_path: &str,
    precision: Option<usize>,
    ignore_comments: bool,
    skip_duplicated_values: bool,
) -> Result<(), ParsingError> {
    use std::collections::HashSet;

    // Set default precision to 3 if not provided
    let precision = precision.unwrap_or(3);

    // Open the output file for writing
    let mut file = File::create(output_path).map_err(|e| ParsingError::ParseError {
        message: format!("Failed to create output file: {:?}", e),
    })?;

    // Get column names from the DataFrame
    let column_names = df.get_column_names();
}
