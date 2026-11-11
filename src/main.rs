// main.rs
#[macro_use]
extern crate pest_derive;

use clap::{Arg, ArgAction, Command};
use std::collections::HashMap;
use std::io::{self};

mod errors;
mod flatten;
mod frame;
mod interpret_rules;
mod interpreter;
mod line_driver;
mod modal_groups;
mod output;
mod start;
mod state;
mod structure_scan;
mod subprogram;
mod types;

use interpreter::nc_to_table;
use serde_json::Value as Json;
use start::{StartInputs, StartState, VariableValue};
use std::path::PathBuf;

fn main() -> io::Result<()> {
    // Define and interpret the command-line arguments using `clap`
    let matches = Command::new("nc-gcode-interpreter")
        .version(env!("NC_GCODE_INTERPRETER_VERSION"))
        .about("A G-code interpreter")
        .arg(
            Arg::new("input")
                .help("Input G-code file (.mpf)")
                .required(true) // Input file is required
                .value_parser(clap::value_parser!(String)),
        )
        .arg(
            Arg::new("axes")
                .short('a')
                .long("axes")
                .value_name("AXIS")
                .help("Override default axis identifiers (comma-separated, e.g., \"X,Y,Z\")")
                .num_args(1) // Expect 1 argument (comma-separated values)
                .value_parser(clap::value_parser!(String)),
        )
        .arg(
            Arg::new("extra_axes")
                .short('e')
                .long("extra-axes")
                .value_name("EXTRA_AXIS")
                .help("Add extra axis identifiers (comma-separated, e.g., \"RA1,RA2\")")
                .num_args(1) // Expect 1 argument (comma-separated values)
                .value_parser(clap::value_parser!(String)),
        )
        .arg(
            Arg::new("definitions")
                .long("definitions")
                .value_name("FILE")
                .help("Definition file (GUD/MGUD .DEF) to read before the program; repeat for several, read in order")
                .action(ArgAction::Append)
                .value_parser(clap::value_parser!(String)),
        )
        .arg(
            Arg::new("variables")
                .long("variables")
                .value_name("FILE")
                .help("JSON object of variable values applied after the definitions, e.g. {\"COUNT\": 3}")
                .num_args(1)
                .value_parser(clap::value_parser!(String)),
        )
        .arg(
            Arg::new("start")
                .long("start")
                .value_name("FILE")
                .help("JSON object with the machine state at program start (the shape of the returned state)")
                .num_args(1)
                .value_parser(clap::value_parser!(String)),
        )
        .arg(
            Arg::new("iteration_limit")
                .short('l')
                .long("iteration_limit")
                .value_name("LIMIT")
                .help("Maximum number of iterations for loops")
                .default_value("10000")
                .num_args(1)
                .value_parser(clap::value_parser!(usize)),
        )
        .arg(
            Arg::new("disable_forward_fill")
                .short('f')
                .long("disable-forward-fill")
                .help("Disable forward-filling of null values in axes columns")
                .action(ArgAction::SetTrue),
        )
        .arg(
            Arg::new("axis_index_map")
                .long("axis-index-map")
                .value_name("AXIS_INDEX_MAP")
                .help("Axis index mapping, e.g. 'E:4,X:0' (comma-separated)")
                .num_args(1)
                .value_parser(clap::value_parser!(String)),
        )
        .arg(
            Arg::new("flatten_tolerance")
                .long("flatten-tolerance")
                .value_name("TOLERANCE")
                .help("Flatten arcs (G2/G3) and splines (ASPLINE/BSPLINE/CSPLINE) into G1 moves, keeping the polyline within TOLERANCE (max deviation, in path units) of the true curve")
                .num_args(1)
                .value_parser(clap::value_parser!(f64)),
        )
        .arg(
            Arg::new("allow_undefined_variables")
                .long("allow-undefined-variables")
                .help("Allow undefined variables in the input file (this will initialize these variables as 0.0), default is false")
                .action(ArgAction::SetTrue),
        )
        .get_matches();

    // Retrieve the input file
    let input_path = matches.get_one::<String>("input").unwrap();

    // Handle axes override
    let axes_override: Option<Vec<String>> = matches
        .get_one::<String>("axes")
        .map(|s| s.split(',').map(|axis| axis.trim().to_string()).collect());

    // Handle extra axes
    let extra_axes: Option<Vec<String>> = matches
        .get_one::<String>("extra_axes")
        .map(|s| s.split(',').map(|axis| axis.trim().to_string()).collect());

    // Parse axis_index_map argument if provided
    let axis_index_map: Option<HashMap<String, usize>> = matches.get_one::<String>("axis_index_map").map(|s| {
        s.split(',')
            .filter_map(|pair| {
                let mut parts = pair.split(':');
                let key = parts.next()?.trim().to_string();
                let value = parts.next()?.trim().parse::<usize>().ok()?;
                Some((key, value))
            })
            .collect::<HashMap<_, _>>()
    });

    let iteration_limit = matches.get_one::<usize>("iteration_limit").unwrap();

    let disable_forward_fill = matches.get_flag("disable_forward_fill");

    let input = std::fs::read_to_string(matches.get_one::<String>("input").unwrap())
        .map_err(|e| io::Error::new(io::ErrorKind::Other, format!("Error reading input file: {}", e)))?;

    let inputs = match read_start_inputs(&matches) {
        Ok(inputs) => inputs,
        Err(message) => {
            eprintln!("{message}");
            std::process::exit(1);
        }
    };

    let allow_undefined_variables = matches.get_flag("allow_undefined_variables");
    let flatten_tolerance = matches.get_one::<f64>("flatten_tolerance").copied();

    match nc_to_table(
        input,
        Some(&inputs),
        axes_override.clone(),
        extra_axes,
        *iteration_limit,
        disable_forward_fill,
        axis_index_map,
        allow_undefined_variables,
        flatten_tolerance,
    ) {
        Ok((table, _state)) => {
            let mut output_path = PathBuf::from(input_path.clone());
            output_path.set_extension("csv");

            let file = std::fs::File::create(&output_path)?;
            output::write_csv(&table, file)
        }
        Err(e) => {
            // Print error directly to stderr for better formatting
            eprintln!("{}", e);
            std::process::exit(1);
        }
    }
}

fn read_file(path: &str, what: &str) -> Result<String, String> {
    std::fs::read_to_string(path).map_err(|e| format!("Error reading {what} file '{path}': {e}"))
}

fn read_json_object(path: &str, what: &str) -> Result<serde_json::Map<String, Json>, String> {
    match serde_json::from_str::<Json>(&read_file(path, what)?) {
        Ok(Json::Object(map)) => Ok(map),
        Ok(_) => Err(format!("{what} file '{path}' must hold a JSON object")),
        Err(e) => Err(format!("{what} file '{path}' is not valid JSON: {e}")),
    }
}

fn variable_value(name: &str, value: &Json) -> Result<VariableValue, String> {
    match value {
        Json::Bool(b) => Ok(VariableValue::Bool(*b)),
        Json::Number(n) => Ok(n
            .as_i64()
            .map(VariableValue::Int)
            .unwrap_or_else(|| VariableValue::Real(n.as_f64().unwrap_or(f64::NAN)))),
        Json::String(s) => Ok(VariableValue::Text(s.clone())),
        _ => Err(format!("variables['{name}'] must be a number, a boolean or a string")),
    }
}

fn number_map(value: &Json, field: &str) -> Result<Vec<(String, f64)>, String> {
    let Json::Object(map) = value else {
        return Err(format!("start['{field}'] must be an object of numbers"));
    };
    map.iter()
        .map(|(key, v)| {
            v.as_f64()
                .map(|n| (key.clone(), n))
                .ok_or_else(|| format!("start['{field}']['{key}'] must be a number"))
        })
        .collect()
}

fn string_map(value: &Json, field: &str) -> Result<Vec<(String, String)>, String> {
    let Json::Object(map) = value else {
        return Err(format!("start['{field}'] must be an object of strings"));
    };
    map.iter()
        .map(|(key, v)| {
            v.as_str()
                .map(|s| (key.clone(), s.to_string()))
                .ok_or_else(|| format!("start['{field}']['{key}'] must be a string"))
        })
        .collect()
}

fn start_state(map: &serde_json::Map<String, Json>) -> Result<StartState, String> {
    let mut start = StartState::default();
    for (key, value) in map {
        match key.as_str() {
            "axes" => start.axes = number_map(value, key)?,
            "translation" => start.translation = number_map(value, key)?,
            "symbol_table" => start.symbol_table = number_map(value, key)?,
            "string_table" => start.string_table = string_map(value, key)?,
            "g_groups" => start.g_groups = string_map(value, key)?,
            "feed" => start.feed = if value.is_null() { None } else { Some(value.as_f64().ok_or("start['feed'] must be a number")?) },
            "tool" => {
                start.tool = match value {
                    Json::Null => None,
                    Json::String(text) => Some(text.clone()),
                    Json::Number(n) => Some(n.to_string()),
                    _ => return Err("start['tool'] must be a string or a number".to_string()),
                }
            }
            "transformation" => {
                start.transformation = match value {
                    Json::Null => None,
                    Json::String(text) => Some(text.clone()),
                    _ => return Err("start['transformation'] must be a string".to_string()),
                }
            }
            "fgroup" => {
                start.fgroup = match value {
                    Json::Null => None,
                    Json::String(text) => Some(text.clone()),
                    _ => return Err("start['fgroup'] must be a string".to_string()),
                }
            }
            "fgref" => start.fgref = number_map(value, key)?,
            "rotation" => {
                let rows = value.as_array().filter(|rows| rows.len() == 3).ok_or("start['rotation'] must be a 3x3 list")?;
                let mut rotation = [[0.0; 3]; 3];
                for (i, row) in rows.iter().enumerate() {
                    let row = row.as_array().filter(|row| row.len() == 3).ok_or("start['rotation'] must be a 3x3 list")?;
                    for (j, v) in row.iter().enumerate() {
                        rotation[i][j] = v.as_f64().ok_or("start['rotation'] must hold numbers")?;
                    }
                }
                start.rotation = Some(rotation);
            }
            other => {
                return Err(format!(
                    "start has an unknown key '{other}' (expected axes, translation, rotation, symbol_table, string_table, g_groups, feed, tool, transformation, fgroup, fgref)"
                ))
            }
        }
    }
    Ok(start)
}

fn read_start_inputs(matches: &clap::ArgMatches) -> Result<StartInputs, String> {
    let definitions = matches
        .get_many::<String>("definitions")
        .into_iter()
        .flatten()
        .map(|path| read_file(path, "definitions"))
        .collect::<Result<Vec<_>, _>>()?;
    let variables = match matches.get_one::<String>("variables") {
        Some(path) => read_json_object(path, "variables")?
            .iter()
            .map(|(name, value)| Ok((name.clone(), variable_value(name, value)?)))
            .collect::<Result<Vec<_>, String>>()?,
        None => Vec::new(),
    };
    let start = match matches.get_one::<String>("start") {
        Some(path) => Some(start_state(&read_json_object(path, "start")?)?),
        None => None,
    };
    Ok(StartInputs {
        definitions,
        variables,
        start,
    })
}
