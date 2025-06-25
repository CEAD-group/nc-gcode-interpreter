// main.rs
#[macro_use]
extern crate pest_derive;

use clap::{Arg, ArgAction, Command};
use std::collections::HashMap;
use std::io::{self};

mod errors;
mod interpret_rules;
mod interpreter;
mod modal_groups;
mod state;
mod types;

use interpreter::{dataframe_to_csv, nc_to_dataframe};
use std::path::PathBuf;

fn main() -> io::Result<()> {
    // Define and interpret the command-line arguments using `clap`
    let matches = Command::new("nc-gcode-interpreter")
        .version("1.0")
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
            Arg::new("initial_state")
                .short('i')
                .long("initial_state")
                .value_name("INITIAL_STATE")
                .help("Optional initial state file to e.g. define global variables or set axis positions")
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
    let axis_index_map: Option<HashMap<String, usize>> = matches
        .get_one::<String>("axis_index_map")
        .map(|s| {
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
        
    let initial_state = matches
        .get_one::<String>("initial_state")
        .map(std::fs::read_to_string)
        .transpose()
        .map_err(|e| {
            io::Error::new(
                io::ErrorKind::Other,
                format!("Error reading initial state file: {}", e),
            )
        })?;

    match nc_to_dataframe(
        &input,
        initial_state.as_deref(),
        axes_override.clone(),
        extra_axes,
        *iteration_limit,
        disable_forward_fill,
        axis_index_map, // Pass axis_index_map from CLI
    ) {
        Ok((mut df, _state)) => {
            let mut output_path = PathBuf::from(input_path.clone());
            output_path.set_extension("csv");

            dataframe_to_csv(&mut df, output_path.to_str().unwrap())
                .map_err(|e| io::Error::new(io::ErrorKind::Other, format!("{}", e)))
        }
        Err(e) => {
            // Print error directly to stderr for better formatting
            eprintln!("{}", e);
            std::process::exit(1);
        }
    }
}
