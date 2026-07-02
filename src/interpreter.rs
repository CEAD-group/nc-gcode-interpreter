//interpreter.rs
use crate::errors::ParsingError;
use crate::interpret_rules::interpret_blocks;
use crate::output::Table;
use crate::state::{self, State};
use crate::types::{NCParser, Rule, Value};
use pest::Parser;
use std::collections::HashMap;

const DEFAULT_AXIS_IDENTIFIERS: &[&str] = &[
    "N", "X", "Y", "Z", "A", "B", "C", "D", "E", "F", "S", "U", "V", "RA1", "RA2", "RA3", "RA4", "RA5", "RA6",
];

/// Main function: interpret the input program into a sanitized output table.
pub fn nc_to_table(
    input: &str,
    initial_state: Option<&str>,
    axis_identifiers: Option<Vec<String>>,
    extra_axes: Option<Vec<String>>,
    iteration_limit: usize,
    disable_forward_fill: bool,
    axis_index_map: Option<HashMap<String, usize>>, // axis identifier to index mapping
    allow_undefined_variables: bool,
) -> Result<(Table, state::State), ParsingError> {
    // Use the override if provided, otherwise use the default identifiers
    let axis_identifiers: Vec<String> =
        axis_identifiers.unwrap_or_else(|| DEFAULT_AXIS_IDENTIFIERS.iter().map(|&s| s.to_string()).collect());

    // Add extra axes to the existing list if provided
    let mut axis_identifiers = axis_identifiers;
    if let Some(extra_axes) = extra_axes {
        axis_identifiers.extend(extra_axes);
    }

    let mut state = state::State::new(
        axis_identifiers.clone(),
        iteration_limit,
        axis_index_map,
        allow_undefined_variables,
    );
    if let Some(initial_state) = initial_state {
        // Propagate the error instead of exiting: this is library code, and
        // process::exit would kill e.g. a host Python interpreter.
        interpret_file(initial_state, &mut state)?;
    }

    // Now interpret the main input using the axis_index_map from state
    let results = interpret_file(input, &mut state)?;

    let table = Table::from_rows(&results, disable_forward_fill);
    Ok((table, state))
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
