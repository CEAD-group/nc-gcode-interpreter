//interpreter.rs
use crate::errors::ParsingError;
use crate::interpret_rules::{interpret_blocks, BlockFlow};
use crate::output::{OutputRows, Row, Table};
use crate::state::{self, State};
use crate::types::{NCParser, Rule};
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
    flatten_tolerance: Option<f64>,
) -> Result<(Table, state::State), ParsingError> {
    let mut state = build_state(
        axis_identifiers,
        extra_axes,
        iteration_limit,
        axis_index_map,
        allow_undefined_variables,
    );
    if let Some(initial_state) = initial_state {
        // Propagate the error instead of exiting: this is library code, and
        // process::exit would kill e.g. a host Python interpreter. The rows
        // of the initial-state file are discarded; only the state matters.
        let mut discard = OutputRows::collect();
        interpret_file(initial_state, &mut state, &mut discard)?;
    }

    // Now interpret the main input using the axis_index_map from state
    let mut output = OutputRows::collect();
    install_flattener(&mut output, &state, flatten_tolerance)?;
    interpret_file(input, &mut state, &mut output)?;
    let rows = output.finish()?;

    let table = Table::from_rows(&rows, disable_forward_fill);
    Ok((table, state))
}

fn build_state(
    axis_identifiers: Option<Vec<String>>,
    extra_axes: Option<Vec<String>>,
    iteration_limit: usize,
    axis_index_map: Option<HashMap<String, usize>>,
    allow_undefined_variables: bool,
) -> State {
    // Use the override if provided, otherwise use the default identifiers
    let mut axis_identifiers: Vec<String> =
        axis_identifiers.unwrap_or_else(|| DEFAULT_AXIS_IDENTIFIERS.iter().map(|&s| s.to_string()).collect());
    if let Some(extra_axes) = extra_axes {
        axis_identifiers.extend(extra_axes);
    }
    state::State::new(axis_identifiers, iteration_limit, axis_index_map, allow_undefined_variables)
}

/// Install the curve flattener on the output when a tolerance was given
/// (see [`crate::flatten`]): G2/G3 arcs and spline blocks come out as runs
/// of G1 rows sampled within `flatten_tolerance` of the true curve.
fn install_flattener(
    output: &mut OutputRows,
    state: &State,
    flatten_tolerance: Option<f64>,
) -> Result<(), ParsingError> {
    if let Some(tolerance) = flatten_tolerance {
        let mut flattener = crate::flatten::Flattener::new(tolerance, &state.axis_identifiers)?;
        // Seed with the machine positions the state already knows (an
        // initial-state file may have established the start point of the
        // first arc/spline of the main program).
        for axis in state.axes.keys() {
            if let Some(machine_value) = state.get_axis_machine(axis) {
                flattener.seed_position(axis, machine_value);
            }
        }
        output.set_flattener(flattener);
    }
    Ok(())
}

/// Streaming twin of `nc_to_table`: interpret the program pushing each
/// finished row into `sender` as `(line_no, row)`, returning the final
/// state. Blocks on the channel when the consumer is slower than the
/// interpreter; aborts with `StreamClosed` when the consumer hangs up.
#[allow(clippy::too_many_arguments)]
#[allow(dead_code)] // used by the python-feature bindings, not the bin
pub fn nc_to_row_stream(
    input: &str,
    initial_state: Option<&str>,
    axis_identifiers: Option<Vec<String>>,
    extra_axes: Option<Vec<String>>,
    iteration_limit: usize,
    axis_index_map: Option<HashMap<String, usize>>,
    allow_undefined_variables: bool,
    flatten_tolerance: Option<f64>,
    sender: std::sync::mpsc::SyncSender<Row>,
) -> Result<state::State, ParsingError> {
    let mut state = build_state(
        axis_identifiers,
        extra_axes,
        iteration_limit,
        axis_index_map,
        allow_undefined_variables,
    );
    if let Some(initial_state) = initial_state {
        let mut discard = OutputRows::collect();
        interpret_file(initial_state, &mut state, &mut discard)?;
    }
    let mut output = OutputRows::stream(sender);
    install_flattener(&mut output, &state, flatten_tolerance)?;
    interpret_file(input, &mut state, &mut output)?;
    output.finish()?;
    Ok(state)
}

/// Batch-streaming twin of `nc_to_table`: interpret the program building
/// completed columnar batches on this worker thread and pushing each finished
/// [`Table`] into `sender` (every `batch_size` output rows, plus a trailing
/// partial batch). Forward-fill state is carried across batches, so - rows being
/// produced in program order - concatenating the batches reconstructs the
/// whole-file table. Blocks on the channel when the consumer is slower;
/// aborts with `StreamClosed` when the consumer hangs up.
#[allow(clippy::too_many_arguments)]
#[allow(dead_code)] // used by the python-feature bindings, not the bin
pub fn nc_to_batch_stream(
    input: &str,
    initial_state: Option<&str>,
    axis_identifiers: Option<Vec<String>>,
    extra_axes: Option<Vec<String>>,
    iteration_limit: usize,
    disable_forward_fill: bool,
    axis_index_map: Option<HashMap<String, usize>>,
    allow_undefined_variables: bool,
    flatten_tolerance: Option<f64>,
    batch_size: usize,
    sender: std::sync::mpsc::SyncSender<Table>,
) -> Result<state::State, ParsingError> {
    let mut state = build_state(
        axis_identifiers,
        extra_axes,
        iteration_limit,
        axis_index_map,
        allow_undefined_variables,
    );
    if let Some(initial_state) = initial_state {
        let mut discard = OutputRows::collect();
        interpret_file(initial_state, &mut state, &mut discard)?;
    }
    let mut output = OutputRows::batch_stream(sender, batch_size, disable_forward_fill);
    install_flattener(&mut output, &state, flatten_tolerance)?;
    interpret_file(input, &mut state, &mut output)?;
    output.finish()?;
    Ok(state)
}

/// Interpret a file, pushing rows into `output`.
fn interpret_file(input: &str, state: &mut State, output: &mut OutputRows) -> Result<(), ParsingError> {
    // Store input for error messages
    state.set_input(input);

    // Validate control-structure nesting first: a PEG reports an unclosed
    // IF/WHILE at the end of the file; the line scan reports the opener.
    let shape = crate::structure_scan::check_structures(input)?;

    // Stage-1 fast path: structure-free programs (all CAM output) are
    // interpreted line by line - trivial lines through the byte decoder,
    // the rest through per-line pest parses. NC_STAGE1=0 disables it.
    if !shape.has_block_structures && crate::line_driver::stage1_enabled() {
        let mut padded_lines = Vec::new();
        // None: the line driver declined (padding budget); fall through to
        // the whole-file parse below.
        match crate::line_driver::interpret_lines(input, &mut padded_lines, output, state)? {
            Some(BlockFlow::Continue) | Some(BlockFlow::EndProgram) => return Ok(()),
            Some(BlockFlow::Jump(request)) => return Err(request.into_not_found_error(state)),
            None => {}
        }
    }

    let file = NCParser::parse(Rule::file, input)
        .map_err(|e| {
            let (line, _col) = match &e.line_col {
                pest::error::LineColLocation::Pos(pos) => *pos,
                pest::error::LineColLocation::Span(start, _) => *start,
            };
            let preview = state.get_line(line).unwrap_or("(could not retrieve line)").to_string();
            ParsingError::with_context(line, preview, "initial file parsing".to_string(), describe_parse_error(&e))
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

    match interpret_blocks(blocks, output, state)? {
        BlockFlow::Continue | BlockFlow::EndProgram => Ok(()),
        // A jump that no scope could resolve: the destination does not exist
        // in the programmed search direction (alarm 14080 on a real control).
        BlockFlow::Jump(request) => Err(request.into_not_found_error(state)),
    }
}

/// Turn a pest parse error into a human message: the expected-rule set is
/// mapped to user-facing phrasing and deduplicated (all sixty G-group rules
/// collapse into one "a G code" entry) instead of leaking grammar-internal
/// rule names like `gg08_work_offset`.
pub(crate) fn describe_parse_error(error: &pest::error::Error<Rule>) -> String {
    use pest::error::ErrorVariant;
    match &error.variant {
        ErrorVariant::ParsingError { positives, .. } if !positives.is_empty() => {
            let mut expected: Vec<&'static str> = Vec::new();
            for rule in positives {
                let description = describe_rule(*rule);
                if !expected.contains(&description) {
                    expected.push(description);
                }
            }
            format!("expected {}", expected.join(", or "))
        }
        _ => format!("{}", error),
    }
}

fn describe_rule(rule: Rule) -> &'static str {
    match rule {
        Rule::file | Rule::blocks | Rule::block => "an NC block",
        Rule::newline => "a new line",
        Rule::EOI => "the end of the program",
        Rule::comment => "a comment (;...)",
        Rule::statement => "a statement",
        Rule::axis_word => "an axis word (e.g. X12.5)",
        Rule::variable_single_char => "an axis letter",
        Rule::assignment => "an assignment",
        Rule::assignment_multi => "an array assignment (SET/REP)",
        Rule::axis_increment => "an incremental value IC(...)",
        Rule::expression | Rule::primary => "an expression",
        Rule::value | Rule::float | Rule::integer => "a number",
        Rule::identifier | Rule::variable => "a name",
        Rule::variable_array => "an array element",
        Rule::indices => "array indices",
        Rule::nc_variable => "a $-variable",
        Rule::arith_fun | Rule::arith_fun_name => "an arithmetic function",
        Rule::function_arguments => "function arguments",
        Rule::non_returning_function_call => "a subprogram call",
        Rule::quoted_string | Rule::string => "a quoted string",
        Rule::tool_selection => "a tool selection (T=\"...\")",
        Rule::definition => "a variable definition (DEF)",
        Rule::data_type => "a data type (REAL/INT/BOOL)",
        Rule::control => "a control statement",
        Rule::condition => "a condition",
        Rule::relational_operator => "a comparison operator",
        Rule::if_statement | Rule::if_goto_statement => "IF",
        Rule::while_statement => "WHILE",
        Rule::for_statement => "FOR",
        Rule::repeat_until_statement => "REPEAT",
        Rule::loop_statement => "LOOP",
        Rule::case_statement | Rule::case_kw => "CASE",
        Rule::case_arm => "a CASE arm (<constant> GOTO...)",
        Rule::case_default | Rule::default_kw => "DEFAULT",
        Rule::of_kw => "OF",
        Rule::goto_statement | Rule::goto_kw => "a jump (GOTO/GOTOF/GOTOB/GOTOC)",
        Rule::gotos_statement => "GOTOS",
        Rule::goto_target => "a jump destination (label or block number)",
        Rule::label_def | Rule::label_name => "a jump label",
        Rule::block_number => "a block number",
        Rule::frame_op | Rule::frame_kw => "a frame instruction (TRANS/ROT/...)",
        Rule::m_command => "an M code",
        Rule::g_command | Rule::g_command_numbered => "a G code",
        Rule::op_add | Rule::op_sub | Rule::op_mul | Rule::op_div | Rule::op_int_div | Rule::op_mod | Rule::neg => {
            "an operator"
        }
        Rule::value_array | Rule::value_repeating | Rule::value_none => "SET/REP values",
        Rule::WHITESPACE => "whitespace",
        // Everything not listed is one of the generated G-group rules.
        _ => "a G code",
    }
}

#[cfg(test)]
mod parse_speed {
    // NCParser and pest::Parser arrive via the glob from the parent module's
    // imports; explicit re-imports here would be redundant (though legal -
    // a glob import may be shadowed, only two explicit imports collide).
    use super::*;
    use std::fmt::Write as _;
    use std::time::Instant;

    /// Deterministic pseudo-random generator (LCG) so the benchmark input is
    /// reproducible without pulling in a rand dependency.
    struct Lcg(u64);
    impl Lcg {
        fn next(&mut self) -> u64 {
            self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            self.0 >> 33
        }
        fn coord(&mut self) -> f64 {
            (self.next() % 2_000_000) as f64 / 1000.0 - 1000.0
        }
    }

    /// Generate a large-format-additive-style flood file: mostly linear moves
    /// in XYZ, sometimes ABC, an E axis, an external axis, occasional modal
    /// G codes, block numbers, comments and spline sections.
    fn generate_flood(lines: usize) -> String {
        let mut rng = Lcg(42);
        let mut out = String::with_capacity(lines * 40);
        out.push_str("; synthetic parse benchmark\nG54 G90 G17\nG1 F2400\nX0 Y0 Z0 E0\n");
        let mut in_spline = false;
        for i in 0..lines {
            let x = rng.coord();
            let y = rng.coord();
            let z = rng.coord();
            match rng.next() % 1000 {
                0..=699 => {
                    let _ = writeln!(out, "X{:.3} Y{:.3} Z{:.3}", x, y, z);
                }
                700..=849 => {
                    let _ = writeln!(out, "X{:.3} Y{:.3} Z{:.3} E{:.3}", x, y, z, rng.coord().abs());
                }
                850..=909 => {
                    let _ = writeln!(out, "G1 X{:.3} Y{:.3} F{}", x, y, 1200 + rng.next() % 4800);
                }
                910..=949 => {
                    let _ = writeln!(
                        out,
                        "X{:.3} Y{:.3} Z{:.3} A{:.3} B{:.3} C{:.3}",
                        x,
                        y,
                        z,
                        rng.coord() / 10.0,
                        rng.coord() / 10.0,
                        rng.coord() / 10.0
                    );
                }
                950..=969 => {
                    let _ = writeln!(out, "X{:.3} Y{:.3} ELX={:.3}", x, y, rng.coord());
                }
                970..=984 => {
                    let _ = writeln!(out, "N{} X{:.3} Y{:.3}", i, x, y);
                }
                985..=994 => {
                    let _ = writeln!(out, "; layer {} progress marker", i);
                }
                _ => {
                    if in_spline {
                        out.push_str("G1\n");
                    } else {
                        out.push_str("BSPLINE\n");
                    }
                    in_spline = !in_spline;
                    let _ = writeln!(out, "X{:.3} Y{:.3} Z{:.3} PW=1.5", x, y, z);
                }
            }
        }
        out.push_str("M30\n");
        out
    }

    /// Not a correctness test: prints parse/interpret throughput for a 1M
    /// line flood file. Run with:
    /// cargo test --release --lib parse_speed -- --ignored --nocapture
    #[test]
    #[ignore = "performance benchmark, run explicitly in release mode"]
    fn parse_speed_1m_lines() {
        let lines = std::env::var("BENCH_LINES")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(1_000_000usize);
        // BENCH_FILE parses a real program instead of the synthetic flood;
        // BENCH_MODE isolates a single line shape to attribute parse cost.
        if let Ok(path) = std::env::var("BENCH_FILE") {
            let input = std::fs::read_to_string(&path).expect("BENCH_FILE must be readable");
            let lines = input.lines().count();
            println!("input: {} ({} lines, {:.1} MB)", path, lines, input.len() as f64 / 1_048_576.0);
            let start = Instant::now();
            let pairs = NCParser::parse(Rule::file, &input).expect("BENCH_FILE must parse");
            let parse_time = start.elapsed();
            println!(
                "pest parse:      {:>8.2?}  ({:.0} klines/s, {:.1} MB/s)",
                parse_time,
                lines as f64 / parse_time.as_secs_f64() / 1000.0,
                input.len() as f64 / 1_048_576.0 / parse_time.as_secs_f64()
            );
            println!("tree pairs: {}", pairs.flatten().count());

            return;
        }
        let input = match std::env::var("BENCH_MODE").as_deref() {
            Ok("xyz") => {
                let mut rng = Lcg(42);
                let mut out = String::with_capacity(lines * 40);
                for _ in 0..lines {
                    let _ = writeln!(out, "X{:.3} Y{:.3} Z{:.3}", rng.coord(), rng.coord(), rng.coord());
                }
                out
            }
            Ok("g1") => {
                let mut rng = Lcg(42);
                let mut out = String::with_capacity(lines * 40);
                for _ in 0..lines {
                    let _ = writeln!(out, "G1 X{:.3} Y{:.3} F2400", rng.coord(), rng.coord());
                }
                out
            }
            Ok("g54") => "G54\n".repeat(lines),
            Ok("elx") => {
                let mut rng = Lcg(42);
                let mut out = String::with_capacity(lines * 40);
                for _ in 0..lines {
                    let _ = writeln!(out, "X{:.3} Y{:.3} ELX={:.3}", rng.coord(), rng.coord(), rng.coord());
                }
                out
            }
            Ok("comment") => "; layer progress marker\n".repeat(lines),
            Ok("bspline") => "BSPLINE\n".repeat(lines),
            _ => generate_flood(lines),
        };
        println!(
            "input: {} lines, {:.1} MB",
            lines,
            input.len() as f64 / 1_048_576.0
        );

        let start = Instant::now();
        let pairs = NCParser::parse(Rule::file, &input).expect("benchmark input must parse");
        let parse_time = start.elapsed();
        println!(
            "pest parse:      {:>8.2?}  ({:.0} klines/s, {:.1} MB/s)",
            parse_time,
            lines as f64 / parse_time.as_secs_f64() / 1000.0,
            input.len() as f64 / 1_048_576.0 / parse_time.as_secs_f64()
        );

        let start = Instant::now();
        let token_count = pairs.flatten().count();
        println!(
            "tree iteration:  {:>8.2?}  ({} pairs)",
            start.elapsed(),
            token_count
        );

        let start = Instant::now();
        let (table, _state) =
            nc_to_table(&input, None, None, None, 10_000, false, None, false, None).expect("interpret");
        println!(
            "full nc_to_table:{:>8.2?}  ({} rows, {} columns)",
            start.elapsed(),
            table.columns.first().map_or(0, |(_, c)| c.len()),
            table.columns.len()
        );
    }
}

#[cfg(test)]
mod interpret_speed {
    use super::*;
    use std::time::Instant;

    /// Times pure interpret+materialize into a collect sink (no CSV, no
    /// python, no table build), plus the whole nc_to_table. Run with:
    /// BENCH_FILE=... cargo test --release --lib interpret_speed -- --ignored --nocapture
    #[test]
    #[ignore = "performance benchmark, run explicitly in release mode"]
    fn interpret_speed_bench() {
        let path = std::env::var("BENCH_FILE").expect("set BENCH_FILE");
        let input = std::fs::read_to_string(&path).expect("readable");
        let lines = input.lines().count();
        println!("input: {} ({} lines, {:.1} MB)", path, lines, input.len() as f64 / 1_048_576.0);
        let extra = Some(vec!["ELX".to_string()]);
        let aim = Some(HashMap::from([("E".to_string(), 4usize), ("ELX".to_string(), 5usize)]));

        // interpret + materialize into a Vec<Row> collect sink (no table build)
        let start = Instant::now();
        let mut state = build_state(None, extra.clone(), 10_000, aim.clone(), true);
        let mut output = OutputRows::collect();
        interpret_file(&input, &mut state, &mut output).expect("interpret");
        let rows = output.finish().expect("finish");
        let materialize = start.elapsed();
        println!("interpret+materialize (collect): {:>8.2?}  ({} rows)", materialize, rows.len());

        let start = Instant::now();
        let _table = Table::from_rows(&rows, false);
        println!("table build:                     {:>8.2?}", start.elapsed());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::output::Column;

    fn interpret(input: &str) -> Table {
        let (table, _state) = nc_to_table(input, None, None, None, 10000, false, None, false, None)
            .expect("program should interpret");
        table
    }

    fn floats<'a>(table: &'a Table, name: &str) -> &'a [Option<f64>] {
        table
            .columns
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, c)| match c {
                Column::Float(v) => v.as_slice(),
                other => panic!("column {name} is not a float column: {other:?}"),
            })
            .unwrap_or_else(|| panic!("column {name} missing; have {:?}", column_names(table)))
    }

    fn column_names(table: &Table) -> Vec<&str> {
        table.columns.iter().map(|(n, _)| n.as_str()).collect()
    }

    /// DEF STRING[n] declares string variables (manual 1.3: STRING is a
    /// standard data type): declaration with and without initialization,
    /// plus reassignment, must all parse; strings live outside the numeric
    /// pipeline and never produce output columns.
    #[test]
    fn def_string_variables() {
        let (table, state) = nc_to_table(
            "DEF STRING[28] CALIBRATION_TOOLPATH = \"move_grid_baseline\"\n\
             DEF STRING[200] _LOGFILENAME\n\
             DEF STRING[8] TAG = \"x\", NOTE = \"\"\n\
             _LOGFILENAME = \"LOG_TRACKER.MPF\"\n\
             G1 X0 Y0 F100\n",
            None,
            None,
            None,
            10000,
            false,
            None,
            false,
            None,
        )
        .expect("program should interpret");
        assert_eq!(state.string_table["CALIBRATION_TOOLPATH"], "move_grid_baseline");
        assert_eq!(state.string_table["_LOGFILENAME"], "LOG_TRACKER.MPF");
        assert_eq!(state.string_table["TAG"], "x");
        assert_eq!(state.string_table["NOTE"], "");
        assert_eq!(floats(&table, "X").len(), 1);
    }

    /// Strings must stay out of the numeric pipeline - loudly. Using a
    /// STRING variable in a numeric expression, initializing a STRING with a
    /// number, or a numeric variable with a string are all hard errors, never
    /// a silent 0.0.
    #[test]
    fn string_numeric_boundaries_error_loudly() {
        let run = |src: &str| nc_to_table(src, None, None, None, 10000, false, None, false, None);
        let err = run("DEF STRING[8] NAME = \"abc\"\nX = NAME + 1\n").unwrap_err();
        assert!(format!("{err}").contains("STRING variable"), "got: {err}");
        let err = run("DEF STRING[8] NAME = 5\n").unwrap_err();
        assert!(format!("{err}").contains("initialized with a number"), "got: {err}");
        let err = run("DEF REAL R1 = \"abc\"\n").unwrap_err();
        assert!(format!("{err}").contains("initialized with a string"), "got: {err}");
        let err = run("DEF STRING[8] NAME\nG1 X=\"abc\"\n").unwrap_err();
        assert!(format!("{err}").contains("cannot assign a string"), "got: {err}");
        // Every name has exactly one type: no numeric->string or
        // string->numeric flips after definition.
        let err = run("DEF REAL R_VAL = 1\nR_VAL = \"abc\"\n").unwrap_err();
        assert!(format!("{err}").contains("numeric variable"), "got: {err}");
        let err = run("DEF STRING[8] NAME = \"abc\"\nNAME = 5\n").unwrap_err();
        assert!(format!("{err}").contains("STRING variable"), "got: {err}");
        // A negative declared length must not parse.
        assert!(run("DEF STRING[-1] NAME\n").is_err());
    }

    /// The interpolation parameters I/J/K (arc-centre offsets) and the CR
    /// radius form must be emitted on the arc block that programs them and be
    /// absent (null) on ordinary linear blocks - never silently dropped and
    /// never forward-filled.
    #[test]
    fn arc_centre_offsets_are_emitted_per_block() {
        // Rows: 0 G1, 1 G2(I/J), 2 G1, 3 G3(I/J), 4 G2(helical I/J/K), 5 G1.
        let table = interpret(
            "G1 X0 Y0 Z0 F1000\n\
             G2 X100 Y0 I50 J0\n\
             G1 X100 Y50\n\
             G3 X0 Y50 I-50 J0\n\
             G2 X0 Y0 Z10 I0 J-25 K5\n\
             G1 X60 Y60\n",
        );

        assert_eq!(
            floats(&table, "I"),
            &[None, Some(50.0), None, Some(-50.0), Some(0.0), None]
        );
        assert_eq!(
            floats(&table, "J"),
            &[None, Some(0.0), None, Some(0.0), Some(-25.0), None]
        );
        assert_eq!(
            floats(&table, "K"),
            &[None, None, None, None, Some(5.0), None]
        );

        // Axes are still forward-filled; the arc offsets are not.
        assert_eq!(floats(&table, "X").last().unwrap(), &Some(60.0));
    }

    /// F on a G4 block is the dwell time in seconds, not a feed change: it
    /// must land in the per-block `dwell` column and leave the modal F
    /// column untouched (no forward-fill pollution).
    #[test]
    fn g4_dwell_does_not_pollute_feed() {
        let table = interpret(
            "G1 X0 Y0 F1000\n\
             G4 F0.01\n\
             G1 X10 Y0\n",
        );
        assert_eq!(floats(&table, "F"), &[Some(1000.0), Some(1000.0), Some(1000.0)]);
        assert_eq!(floats(&table, "dwell"), &[None, Some(0.01), None]);
    }

    /// The NC language is case-insensitive: lowercase axis words are axis
    /// words (not silently-swallowed subprogram calls), and G/M values are
    /// normalized to uppercase in the output - a lowercase g18 must never
    /// reach the flattener as a distinct plane string.
    #[test]
    fn lowercase_program_is_normalized() {
        let table = interpret("g17 g1 x0 y0 f100 m8\ng2 x10 y0 i5 j0\n");
        assert_eq!(floats(&table, "X"), &[Some(0.0), Some(10.0)]);
        assert_eq!(floats(&table, "I"), &[None, Some(5.0)]);
        assert!(
            !column_names(&table).contains(&"non_returning_function_call"),
            "lowercase axis word was treated as a subprogram call: {:?}",
            column_names(&table)
        );
        let strs = |name: &str| -> Vec<Option<String>> {
            match &table.columns.iter().find(|(n, _)| n == name).unwrap().1 {
                Column::Str(v) => v.clone(),
                other => panic!("column {name} is not a str column: {other:?}"),
            }
        };
        assert_eq!(
            strs("gg01_motion"),
            &[Some("G1".to_string()), Some("G2".to_string())]
        );
        assert_eq!(
            strs("gg06_plane_select"),
            &[Some("G17".to_string()), Some("G17".to_string())]
        );
        match &table.columns.iter().find(|(n, _)| n == "M").unwrap().1 {
            Column::StrList(v) => assert_eq!(v[0], Some(vec!["M8".to_string()])),
            other => panic!("M is not a str-list column: {other:?}"),
        }
    }

    /// A G4 block that programs BOTH F and S must consume both: the dwell
    /// value is F, and neither may forward-fill into the modal F/S columns.
    #[test]
    fn g4_consumes_both_f_and_s() {
        let (table, _state) = nc_to_table(
            "G1 X0 Y0 F1000 S200\n\
             G4 F0.5 S2\n\
             G1 X10 Y0\n",
            None,
            None,
            None,
            10000,
            false,
            None,
            false,
            None,
        )
        .expect("program should interpret");
        assert_eq!(floats(&table, "F"), &[Some(1000.0), Some(1000.0), Some(1000.0)]);
        assert_eq!(floats(&table, "S"), &[Some(200.0), Some(200.0), Some(200.0)]);
        assert_eq!(floats(&table, "dwell"), &[None, Some(0.5), None]);
    }

    /// A start position established by the initial-state file must seed the
    /// flattener: the first arc of the main program flattens from the correct
    /// start point instead of warning that the position is unknown.
    #[test]
    fn initial_state_position_seeds_flattener() {
        let (table, _state) = nc_to_table(
            "G2 X100 Y0 I50 J0 F1000\n",
            Some("G1 X0 Y0 Z0 F100\n"),
            None,
            None,
            10000,
            false,
            None,
            false,
            Some(0.1),
        )
        .expect("program should interpret");
        let x = floats(&table, "X");
        // A half circle of radius 50 at 0.1 mm tolerance needs ~25 samples;
        // an unseeded flattener would pass the single G2 row through instead.
        assert!(x.len() > 10, "arc was not flattened: {} row(s)", x.len());
        assert_eq!(x.last().unwrap(), &Some(100.0));
        // The intermediate samples sit on the r=50 circle around (50, 0).
        let y = floats(&table, "Y");
        for (xv, yv) in x.iter().zip(y).filter_map(|(a, b)| a.zip(*b)) {
            let r = ((xv - 50.0).powi(2) + yv.powi(2)).sqrt();
            assert!((r - 50.0).abs() < 1e-6, "sample ({xv}, {yv}) off the circle: r={r}");
        }
    }

    /// A pathologically tight tolerance must not materialize a gigabyte-scale
    /// row burst: the per-arc sample count is clamped (loudly) at 100k.
    #[test]
    fn arc_segment_count_is_clamped() {
        let (table, _state) = nc_to_table(
            "G1 X0 Y0 F1000\n\
             G2 X0 Y0 I1000 J0\n",
            None,
            None,
            None,
            10000,
            false,
            None,
            false,
            Some(1e-9),
        )
        .expect("program should interpret");
        // Full circle of radius 1000 at 1e-9 tolerance wants ~2.2M segments.
        assert_eq!(floats(&table, "X").len(), 1 + 100_000);
    }

    /// The CR= radius form is likewise a per-block interpolation parameter and
    /// is accepted both bare (I50) and with `=` (CR=20).
    #[test]
    fn arc_radius_form_is_emitted_per_block() {
        // Rows: 0 G1, 1 G2(CR), 2 G1.
        let table = interpret(
            "G1 X0 Y0 F1000\n\
             G2 X40 Y0 CR=20\n\
             G1 X50 Y0\n",
        );

        assert_eq!(floats(&table, "CR"), &[None, Some(20.0), None]);
    }
}
