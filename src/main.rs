// main.rs
#[macro_use]
extern crate pest_derive;

use clap::{Arg, ArgAction, Command};
use std::collections::HashMap;
mod errors;
mod interpret_rules;
mod interpreter;
mod modal_groups;
mod state;
mod types;

use std::error::Error;
use interpreter::{dataframe_to_csv, nc_to_dataframe, DEFAULT_AXIS_IDENTIFIERS};

type Result<T> = std::result::Result<T, Box<dyn Error>>;

fn main() -> Result<()> {
    let matches = build_cli().get_matches();
    
    // Parse configuration
    let config = Config::from_matches(&matches)?;
    
    // Initialize interpreter state
    let mut state = state::State::new(
        config.all_axes.clone(),
        config.iteration_limit,
        config.axis_index_map.clone(),
    );
    state.auto_init_variables = config.auto_init_variables;
    state.init_variables(config.variable_initializations);

    // Read and process the input file
    let content = std::fs::read_to_string(&config.input_file)?;
    let (mut result, _) = nc_to_dataframe(
        &content,
        config.initial_state_file.as_deref(),
        Some(config.all_axes),
        None,
        config.iteration_limit,
        !config.disable_forward_fill,
        config.axis_index_map,
    )?;

    // Write output
    let output_path = config.input_file.replace(".mpf", ".csv");
    dataframe_to_csv(&mut result, &output_path)?;

    Ok(())
}

struct Config {
    input_file: String,
    all_axes: Vec<String>,
    axis_index_map: Option<HashMap<String, usize>>,
    iteration_limit: usize,
    initial_state_file: Option<String>,
    variable_initializations: HashMap<String, f32>,
    auto_init_variables: bool,
    disable_forward_fill: bool,
}

impl Config {
    fn from_matches(matches: &clap::ArgMatches) -> Result<Self> {
        let input_file = matches.get_one::<String>("input")
            .ok_or("Input file is required")?
            .to_string();

        // Get axis identifiers (default or from command line)
        let axis_identifiers: Vec<String> = matches
            .get_one::<String>("axes")
            .map(|s| s.split(',').map(|axis| axis.trim().to_string()).collect())
            .unwrap_or_else(|| DEFAULT_AXIS_IDENTIFIERS.iter().map(|&s| s.to_string()).collect());

        // Add extra axes if specified
        let mut all_axes = axis_identifiers;
        if let Some(extra) = matches.get_one::<String>("extra_axes") {
            all_axes.extend(extra.split(',').map(|s| s.trim().to_string()));
        }

        // Parse axis_index_map
        let axis_index_map = matches.get_one::<String>("axis_index_map").map(|s| {
            s.split(',')
                .filter_map(|pair| {
                    let mut parts = pair.split(':');
                    let key = parts.next()?.trim().to_string();
                    let value = parts.next()?.trim().parse::<usize>().ok()?;
                    Some((key, value))
                })
                .collect::<HashMap<_, _>>()
        });

        // Parse variable initializations
        let variable_initializations = matches
            .get_one::<String>("init_variables")
            .map(|s| {
                s.split(',')
                    .filter_map(|pair| {
                        let mut parts = pair.split(':');
                        let name = parts.next()?.trim().to_string();
                        let value = parts.next()?.trim().parse::<f32>().ok()?;
                        Some((name.to_string(), value))
                    })
                    .collect()
            })
            .unwrap_or_default();

        Ok(Config {
            input_file,
            all_axes,
            axis_index_map,
            iteration_limit: *matches.get_one::<usize>("iteration_limit").unwrap_or(&10000),
            initial_state_file: matches.get_one::<String>("initial_state").map(String::from),
            variable_initializations,
            auto_init_variables: matches.get_flag("auto_init_variables"),
            disable_forward_fill: matches.get_flag("disable_forward_fill"),
        })
    }
}

fn build_cli() -> Command {
    Command::new("nc-gcode-interpreter")
        .version("1.0")
        .about("A G-code interpreter")
        .arg(
            Arg::new("input")
                .help("Input G-code file (.mpf)")
                .required(true)
                .value_parser(clap::value_parser!(String)),
        )
        .arg(
            Arg::new("axes")
                .short('a')
                .long("axes")
                .value_name("AXIS")
                .help("Override default axis identifiers (comma-separated, e.g., \"X,Y,Z\")")
                .num_args(1)
                .value_parser(clap::value_parser!(String)),
        )
        .arg(
            Arg::new("extra_axes")
                .short('e')
                .long("extra-axes")
                .value_name("EXTRA_AXIS")
                .help("Add extra axis identifiers (comma-separated, e.g., \"RA1,RA2\")")
                .num_args(1)
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
            Arg::new("init_variables")
                .long("init-variables")
                .value_name("VARIABLES")
                .help("Initialize variables with values (comma-separated, e.g., \"FLOW_CC_8:1,X_COMP:0\")")
                .num_args(1)
                .value_parser(clap::value_parser!(String)),
        )
        .arg(
            Arg::new("auto_init_variables")
                .long("auto-init-variables")
                .help("Automatically initialize undefined variables to 0.0")
                .action(ArgAction::SetTrue),
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
}
