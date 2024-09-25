// main.rs
#[macro_use]
extern crate pest_derive;

use clap::{Arg, ArgAction, Command};
use std::io::{self};

mod errors;
mod interpret_rules;
mod interpreter;
mod state;
mod types;
mod modal_groups;

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
                .help("Optional initial_state file to initialize state")
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

    let iteration_limit = matches.get_one::<usize>("iteration_limit").unwrap();

    let disable_forward_fill = matches.get_flag("disable_forward_fill");

    let input = std::fs::read_to_string(matches.get_one::<String>("input").unwrap())
        .map_err(|e| io::Error::new(io::ErrorKind::Other, format!("Error reading input file: {:?}", e)))?;
    

    let initial_state = matches.get_one::<String>("initial_state")
        .map(std::fs::read_to_string)
        .transpose()
        .map_err(|e| io::Error::new(io::ErrorKind::Other, format!("Error reading initial state file: {:?}", e)))?;


    let (mut df, _state) = nc_to_dataframe(
        &input,
        initial_state.as_deref(),
        axes_override.clone(),
        extra_axes,
        *iteration_limit,
        disable_forward_fill
    )?;

    let mut output_path = PathBuf::from(input_path.clone());
    output_path.set_extension("csv");

    dataframe_to_csv(&mut df, output_path.to_str().unwrap())
        .map_err(|e| io::Error::new(io::ErrorKind::Other, format!("Error writing DataFrame to CSV: {:?}", e)))
}
