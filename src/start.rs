//! What the control holds before the main program starts: definition files,
//! variable values and the machine start state. Applied in that order, after
//! which the main program runs.
//!
//! * `definitions` are read like the GUD/MGUD/UGUD `.DEF` files a control
//!   loads (manual 4.1.1.4): only DEF and REDEF statements, assignments to
//!   variables and comments. The `%_N_..._DEF` header and `$PATH=` line are
//!   skipped, and M17/M30/M2 ends the file.
//! * `variables` writes values straight into the variable tables, checked
//!   against the declared type.
//! * `start` is the state dict a run returns: axis positions, frame,
//!   variables, modal G groups and F. The G groups and F seed the first
//!   output row; positions and frame seed only the internal state.

use crate::errors::ParsingError;
use crate::frame::{self, Matrix3};
use crate::modal_groups::{classify_g_command, MODAL_G_GROUPS, NON_MODAL_G_GROUPS};
use crate::output::OutputRows;
use crate::state::{State, VarType};
use crate::types::{NCParser, Rule, Value};
use pest::iterators::Pair;
use pest::Parser;
use std::collections::HashSet;

/// A value for one entry of `variables`.
#[derive(Debug, Clone, PartialEq)]
pub enum VariableValue {
    Real(f64),
    Int(i64),
    Bool(bool),
    Text(String),
}

/// The machine state at program start, in the shape of the state a run
/// returns (see [`crate::state::FinalState`]).
#[derive(Debug, Clone, Default)]
pub struct StartState {
    /// Local (workpiece) positions per axis.
    pub axes: Vec<(String, f64)>,
    pub translation: Vec<(String, f64)>,
    /// Row-major frame rotation; must be a proper rotation matrix.
    pub rotation: Option<[[f64; 3]; 3]>,
    pub symbol_table: Vec<(String, f64)>,
    pub string_table: Vec<(String, String)>,
    /// Modal G group column and code, e.g. `("gg01_motion", "G1")`.
    pub g_groups: Vec<(String, String)>,
    /// The path feed. A control without F programmed uses SD42110
    /// $SC_DEFAULT_FEED; pass that value here.
    pub feed: Option<f64>,
    /// The T column value (tool name or number as text).
    pub tool: Option<String>,
    /// The transformation column value, e.g. `TRAORI` or `TRAFOOF`.
    pub transformation: Option<String>,
}

/// Everything applied before the main program, in order: `definitions`,
/// `variables`, `start`.
#[derive(Debug, Clone, Default)]
pub struct StartInputs {
    pub definitions: Vec<String>,
    pub variables: Vec<(String, VariableValue)>,
    pub start: Option<StartState>,
}

/// Cells for the first output row of the main program.
pub(crate) type Seed = Vec<(&'static str, Value)>;

impl StartInputs {
    /// Apply the inputs to `state`, returning the cells that seed the first
    /// output row (the start state's modal G groups and F).
    pub(crate) fn apply(&self, state: &mut State) -> Result<Seed, ParsingError> {
        for (index, text) in self.definitions.iter().enumerate() {
            read_definition_file(text, state).map_err(|error| ParsingError::InDefinitionFile {
                index,
                error: Box::new(error),
            })?;
        }
        let snapshot_names: HashSet<String> = self
            .start
            .iter()
            .flat_map(|start| {
                let numbers = start.symbol_table.iter().map(|(name, _)| name);
                let texts = start.string_table.iter().map(|(name, _)| name);
                numbers.chain(texts).map(|name| name.trim().to_uppercase())
            })
            .collect();
        for (name, value) in &self.variables {
            let key = name.trim().to_uppercase();
            let field = format!("variables['{name}']");
            if snapshot_names.contains(&key) {
                return Err(invalid(
                    &field,
                    "the name is also in start's symbol_table or string_table; give it in one place",
                ));
            }
            set_variable(state, &key, value, false).map_err(|message| invalid(&field, &message))?;
        }
        match &self.start {
            Some(start) => apply_start(start, state),
            None => Ok(Vec::new()),
        }
    }
}

fn invalid(field: &str, message: &str) -> ParsingError {
    ParsingError::InvalidInput {
        field: field.to_string(),
        message: message.to_string(),
    }
}

/// What a line of a definition file contributes.
enum DefinitionLine {
    /// Interpreted as written.
    Keep,
    /// The `%_N_..._DEF` header or the `$PATH=` line: not NC code.
    Skip,
    /// M17, M30 or M2: the end of the file.
    End,
}

/// Read one definition file into `state`. Only the lines before the end code
/// are interpreted; line numbers in errors are those of the file.
fn read_definition_file(text: &str, state: &mut State) -> Result<(), ParsingError> {
    let mut kept = String::with_capacity(text.len());
    let mut declared = HashSet::new();
    for (index, raw) in text.split('\n').enumerate() {
        let line = raw.strip_suffix('\r').unwrap_or(raw);
        match classify_definition_line(line, index + 1, state, &mut declared)? {
            DefinitionLine::Keep => kept.push_str(line),
            DefinitionLine::Skip => {}
            DefinitionLine::End => break,
        }
        kept.push('\n');
    }
    let mut discard = OutputRows::collect();
    crate::interpreter::interpret_source(kept.into(), state, &mut discard)
}

fn not_allowed(line_no: usize, line: &str, found: &str) -> ParsingError {
    ParsingError::with_context(
        line_no,
        line,
        "definition file",
        &format!(
            "a definition file may contain only DEF and REDEF statements, assignments to variables, \
             comments and an end code (M17, M30, M2); '{}' is not allowed here",
            found.trim()
        ),
    )
}

/// Classify one line of a definition file. `declared` collects the names the
/// earlier lines of this file declare, so an assignment can be checked.
fn classify_definition_line(
    line: &str,
    line_no: usize,
    state: &State,
    declared: &mut HashSet<String>,
) -> Result<DefinitionLine, ParsingError> {
    let trimmed = line.trim();
    if trimmed.is_empty() || trimmed.starts_with(';') {
        return Ok(DefinitionLine::Keep);
    }
    if trimmed.starts_with('%') || trimmed.to_uppercase().starts_with("$PATH") {
        return Ok(DefinitionLine::Skip);
    }
    let parsed = NCParser::parse(Rule::single_line, line).map_err(|e| {
        let column = match &e.line_col {
            pest::error::LineColLocation::Pos((_, col)) | pest::error::LineColLocation::Span((_, col), _) => *col,
        };
        ParsingError::ParsingContext {
            line_no,
            column: Some(column),
            preview: line.to_string(),
            context: "definition file".to_string(),
            message: crate::interpreter::describe_parse_error(&e),
        }
    })?;
    let block = parsed
        .into_iter()
        .next()
        .and_then(|single| single.into_inner().next())
        .ok_or_else(|| not_allowed(line_no, line, line))?;
    let mut end = false;
    let mut content = false;
    let mut channel_select = false;
    for pair in block.into_inner() {
        match pair.as_rule() {
            Rule::block_number | Rule::comment => {}
            Rule::redef_decl => content = true,
            Rule::definition => {
                for item in pair.clone().into_inner() {
                    if let Some(base) = crate::interpret_rules::declared_base(&item.into(), state) {
                        declared.insert(crate::subprogram::base_name(&base).to_uppercase());
                    }
                }
                content = true;
            }
            Rule::statement => {
                let text = pair.as_str();
                let inner = pair
                    .into_inner()
                    .next()
                    .ok_or_else(|| not_allowed(line_no, line, text))?;
                match inner.as_rule() {
                    Rule::m_command if crate::interpret_rules::is_end_of_program_m_code(inner.as_str()) => end = true,
                    Rule::assignment | Rule::assignment_multi => {
                        let name =
                            check_assignment_target(&inner, state).map_err(|_| not_allowed(line_no, line, text))?;
                        if !is_declared(&name, state, declared) {
                            return Err(ParsingError::with_context(
                                line_no,
                                line,
                                "definition file",
                                &format!(
                                    "'{name}' is assigned but not declared; declare it with DEF first, \
                                     or pass allow_undefined_variables"
                                ),
                            ));
                        }
                        content = true;
                    }
                    Rule::non_returning_function_call => {
                        let call: String = text.split_whitespace().collect::<String>().to_uppercase();
                        let Some(channel) = call.strip_prefix("CHANDATA(").and_then(|rest| rest.strip_suffix(')'))
                        else {
                            return Err(not_allowed(line_no, line, text));
                        };
                        // The interpreter models one channel, so only its selection is harmless.
                        if channel != "1" {
                            return Err(ParsingError::with_context(
                                line_no,
                                line,
                                "definition file",
                                &format!(
                                    "CHANDATA({channel}) selects channel {channel}; the interpreter models only channel 1"
                                ),
                            ));
                        }
                        channel_select = true;
                    }
                    _ => return Err(not_allowed(line_no, line, text)),
                }
            }
            _ => return Err(not_allowed(line_no, line, pair.as_str())),
        }
    }
    if channel_select {
        if end || content {
            return Err(ParsingError::with_context(
                line_no,
                line,
                "definition file",
                "CHANDATA(1) must stand in a block of its own",
            ));
        }
        return Ok(DefinitionLine::Skip);
    }
    if end && content {
        return Err(ParsingError::with_context(
            line_no,
            line,
            "definition file",
            "the end code (M17, M30, M2) must stand in a block of its own",
        ));
    }
    Ok(if end { DefinitionLine::End } else { DefinitionLine::Keep })
}

/// An assignment in a definition file must set a variable: not an axis, a
/// block address or a system variable. Returns the target as written.
fn check_assignment_target(assignment: &Pair<Rule>, state: &State) -> Result<String, ()> {
    let target = assignment.clone().into_inner().next().ok_or(())?;
    if target.as_rule() == Rule::variable_single_char {
        return Err(());
    }
    let written: String = target.as_str().split_whitespace().collect::<String>().to_uppercase();
    let name = written.split('[').next().unwrap_or("");
    if name.is_empty() || name.starts_with('$') || state.is_axis(name) || state.is_block_address(name) {
        return Err(());
    }
    Ok(written)
}

/// Whether a definition-file assignment target exists: declared in this file
/// or an earlier one, an R parameter, or allowed by allow_undefined_variables.
fn is_declared(target: &str, state: &State, declared: &HashSet<String>) -> bool {
    let base = crate::subprogram::base_name(target);
    declared.contains(base)
        || state.var_types.contains_key(base)
        || state.symbol_table.contains_key(target)
        || state.string_table.contains_key(target)
        || is_r_parameter(target)
        || state.allow_undefined_variables
}

/// `R1`, `R[1]`: arithmetic parameters exist without a DEF.
fn is_r_parameter(key: &str) -> bool {
    let Some(rest) = key.strip_prefix('R') else {
        return false;
    };
    let digits = rest
        .strip_prefix('[')
        .and_then(|inner| inner.strip_suffix(']'))
        .unwrap_or(rest);
    !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit())
}

fn describe(value: &VariableValue) -> String {
    match value {
        VariableValue::Real(v) => format!("{v}"),
        VariableValue::Int(v) => format!("{v}"),
        VariableValue::Bool(v) => format!("{v}"),
        VariableValue::Text(v) => format!("{v:?}"),
    }
}

/// The number a numeric variable of type `declared` stores for `value`, or
/// why the value does not fit.
fn numeric_value(declared: VarType, value: &VariableValue) -> Result<f64, String> {
    let shown = describe(value);
    let number = match value {
        VariableValue::Real(v) => *v,
        VariableValue::Int(v) => *v as f64,
        VariableValue::Bool(v) => f64::from(u8::from(*v)),
        VariableValue::Text(text) => {
            let mut chars = text.chars();
            return match (declared, chars.next(), chars.next()) {
                (VarType::Char, Some(c), None) if c.is_ascii() => Ok(f64::from(c as u8)),
                (VarType::Char, ..) => Err(format!("{shown} does not fit a CHAR variable (one ASCII character)")),
                _ => Err(format!("{shown} is a string; the variable is numeric")),
            };
        }
    };
    if !number.is_finite() {
        return Err(format!("{shown} is not a finite number"));
    }
    match declared {
        VarType::Real => Ok(number),
        VarType::Int => {
            // INT is a signed 32-bit integer on the control.
            if number.fract() != 0.0 || number < f64::from(i32::MIN) || number > f64::from(i32::MAX) {
                Err(format!(
                    "{shown} does not fit an INT variable (a whole number in the 32-bit range)"
                ))
            } else {
                Ok(number)
            }
        }
        VarType::Bool => {
            if number == 0.0 || number == 1.0 {
                Ok(number)
            } else {
                Err(format!("{shown} does not fit a BOOL variable (true, false, 0 or 1)"))
            }
        }
        VarType::Char => {
            if number.fract() == 0.0 && (0.0..=255.0).contains(&number) {
                Ok(number)
            } else {
                Err(format!(
                    "{shown} does not fit a CHAR variable (0 to 255 or one character)"
                ))
            }
        }
        VarType::Str(_) => Err(format!("{shown} is a number; the variable is a STRING")),
    }
}

/// Write `value` into the variable `key`. `snapshot` marks a value from the
/// start state, which may create a variable no definition declared.
fn set_variable(state: &mut State, key: &str, value: &VariableValue, snapshot: bool) -> Result<(), String> {
    if key.is_empty() {
        return Err("the name is empty".to_string());
    }
    if state.is_axis(key) {
        return Err(format!("{key} is an axis; set axis positions in start['axes']"));
    }
    if state.is_block_address(key) {
        return Err(format!("{key} is a block address, not a variable"));
    }
    if key == "TRUE" || key == "FALSE" {
        let constant = if key == "TRUE" { 1.0 } else { 0.0 };
        return match (snapshot, numeric_value(VarType::Real, value)) {
            (true, Ok(number)) if number == constant => Ok(()),
            _ => Err(format!("{key} is a constant")),
        };
    }
    let declared = state.var_types.get(crate::subprogram::base_name(key)).copied();
    let is_number = state.symbol_table.contains_key(key);
    let is_text = state.string_table.contains_key(key);
    if is_text || matches!(declared, Some(VarType::Str(_))) && !is_number {
        let VariableValue::Text(text) = value else {
            return Err(format!("{} is a number; the variable is a STRING", describe(value)));
        };
        if let Some(VarType::Str(max)) = declared {
            if text.chars().count() > max {
                return Err(format!("{text:?} is longer than the STRING[{max}] it is assigned to"));
            }
        }
        state.string_table.insert(key.to_string(), text.clone());
        return Ok(());
    }
    if !is_number {
        let known = declared.is_some() || is_r_parameter(key) || key.starts_with('$');
        if !(known || snapshot || state.allow_undefined_variables) {
            return Err(format!(
                "{key} is not defined; declare it in a definition file, or pass allow_undefined_variables"
            ));
        }
        if let VariableValue::Text(text) = value {
            if declared.is_none() {
                state.string_table.insert(key.to_string(), text.clone());
                return Ok(());
            }
        }
    }
    let number = numeric_value(declared.unwrap_or(VarType::Real), value)?;
    state.symbol_table.insert(key.to_string(), number);
    Ok(())
}

fn axis_key(state: &State, field: &str, axis: &str) -> Result<String, ParsingError> {
    let key = axis.trim().to_uppercase();
    if state.is_axis(&key) {
        Ok(key)
    } else {
        Err(invalid(
            field,
            &format!(
                "{axis} is not a configured axis ({})",
                state.axis_identifiers.join(", ")
            ),
        ))
    }
}

fn check_rotation(rotation: &Matrix3) -> Result<(), String> {
    for (row_index, row) in rotation.iter().enumerate() {
        if row.iter().any(|value| !value.is_finite()) {
            return Err(format!("row {row_index} holds a value that is not a finite number"));
        }
    }
    // R * R^T must be the identity and det(R) = +1: no scaling, no mirroring.
    for i in 0..3 {
        for j in 0..3 {
            let dot: f64 = (0..3).map(|k| rotation[i][k] * rotation[j][k]).sum();
            let expected = if i == j { 1.0 } else { 0.0 };
            if (dot - expected).abs() > 1e-6 {
                return Err("the matrix is not orthonormal, so it is not a rotation".to_string());
            }
        }
    }
    let r = rotation;
    let det = r[0][0] * (r[1][1] * r[2][2] - r[1][2] * r[2][1]) - r[0][1] * (r[1][0] * r[2][2] - r[1][2] * r[2][0])
        + r[0][2] * (r[1][0] * r[2][1] - r[1][1] * r[2][0]);
    if (det - 1.0).abs() > 1e-6 {
        return Err("the matrix mirrors (determinant -1), so it is not a rotation".to_string());
    }
    Ok(())
}

fn modal_group(field: &str, group: &str, code: &str) -> Result<(&'static str, String), ParsingError> {
    let Some(&column) = MODAL_G_GROUPS.iter().find(|name| **name == group) else {
        let reason = if NON_MODAL_G_GROUPS.contains(&group) {
            format!("{group} is not a modal G group; only modal groups have a start value")
        } else {
            format!("{group} is not a G group column (e.g. gg01_motion)")
        };
        return Err(invalid(field, &reason));
    };
    let code = code.trim().to_uppercase();
    match classify_g_command(&code) {
        Some((found, _)) if found == column => Ok((column, code)),
        Some((found, _)) => Err(invalid(field, &format!("{code} belongs to {found}, not to {column}"))),
        None => Err(invalid(field, &format!("{code} is not a known G code"))),
    }
}

fn apply_start(start: &StartState, state: &mut State) -> Result<Seed, ParsingError> {
    let mut seed: Seed = Vec::new();
    for (axis, value) in &start.translation {
        let field = format!("start['translation']['{axis}']");
        let key = axis_key(state, &field, axis)?;
        state.translation.insert(key, *value);
    }
    if let Some(rotation) = &start.rotation {
        check_rotation(rotation).map_err(|message| invalid("start['rotation']", &message))?;
        state.rotation = *rotation;
        state.rotated = !frame::is_identity(rotation);
    }
    for (axis, value) in &start.axes {
        let field = format!("start['axes']['{axis}']");
        let key = axis_key(state, &field, axis)?;
        state.axes.insert(key, *value);
    }
    for (name, value) in &start.symbol_table {
        let field = format!("start['symbol_table']['{name}']");
        set_variable(state, &name.trim().to_uppercase(), &VariableValue::Real(*value), true)
            .map_err(|message| invalid(&field, &message))?;
    }
    for (name, value) in &start.string_table {
        let field = format!("start['string_table']['{name}']");
        set_variable(
            state,
            &name.trim().to_uppercase(),
            &VariableValue::Text(value.clone()),
            true,
        )
        .map_err(|message| invalid(&field, &message))?;
    }
    for (group, code) in &start.g_groups {
        let field = format!("start['g_groups']['{group}']");
        let (column, code) = modal_group(&field, group, code)?;
        if column == "gg06_plane_select" {
            state.set_plane(&code);
        }
        // A PROC with SAVE restores the groups to these values.
        state.ext.modal_g.insert(column, code.clone());
        seed.push((column, Value::Str(code)));
    }
    if let Some(feed) = start.feed {
        let key = axis_key(state, "start['feed']", "F")?;
        if !feed.is_finite() || feed <= 0.0 {
            return Err(invalid("start['feed']", "the feed must be a number greater than 0"));
        }
        // The F column carries the programmed value; the axis holds it in local terms.
        let local = feed - state.get_translation(&key);
        state.axes.insert(key, local);
        let column = state
            .resolve_output_key("F")
            .map(|(_, column)| column)
            .unwrap_or_else(|| crate::output::intern_column("F"));
        seed.push((column, Value::Float(feed)));
    }
    if let Some(tool) = &start.tool {
        if tool.trim().is_empty() {
            return Err(invalid("start['tool']", "the tool is empty"));
        }
        seed.push((crate::output::intern_column("T"), Value::Str(tool.trim().to_string())));
    }
    if let Some(call) = &start.transformation {
        let value = crate::output::transformation_value(call).ok_or_else(|| {
            invalid(
                "start['transformation']",
                &format!("{call} is not a transformation call (TRAORI, TRANSMIT, TRACYL, TRAANG, TRACON, TRAFOOF)"),
            )
        })?;
        seed.push((
            crate::output::intern_column(crate::output::TRANSFORMATION_COLUMN),
            Value::Str(value),
        ));
    }
    Ok(seed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::interpreter::nc_to_table_with;
    use crate::output::{Column, Table};
    use crate::subprogram::Extensions;

    fn run(program: &str, inputs: StartInputs) -> Result<(Table, State), ParsingError> {
        nc_to_table_with(
            program,
            Some(&inputs),
            None,
            None,
            1000,
            false,
            None,
            false,
            None,
            Extensions::default(),
        )
    }

    fn floats(table: &Table, name: &str) -> Vec<Option<f64>> {
        match table.columns.iter().find(|(n, _)| n == name).map(|(_, c)| c) {
            Some(Column::Float(v)) => v.clone(),
            other => panic!("no float column {name}: {other:?}"),
        }
    }

    fn texts(table: &Table, name: &str) -> Vec<Option<String>> {
        match table.columns.iter().find(|(n, _)| n == name).map(|(_, c)| c) {
            Some(Column::Str(v)) => v.clone(),
            Some(Column::Dict(d)) => d.iter().map(|v| v.map(str::to_string)).collect(),
            other => panic!("no string column {name}: {other:?}"),
        }
    }

    fn definitions(files: &[&str]) -> StartInputs {
        StartInputs {
            definitions: files.iter().map(|f| f.to_string()).collect(),
            ..StartInputs::default()
        }
    }

    #[test]
    fn every_definition_file_takes_effect_past_its_end_code() {
        let files = [
            "%_N_MGUD_DEF\n$PATH=/_N_DEF_DIR\nDEF NCK REAL FIRST=1.5\nM30\n",
            "; second file\nDEF CHAN INT SECOND=2\nSECOND=SECOND+1\nM17\nG1 X99\n",
        ];
        let (table, state) = run("X=FIRST+SECOND", definitions(&files)).unwrap();
        assert_eq!(floats(&table, "X"), vec![Some(4.5)]);
        assert_eq!(state.symbol_table["SECOND"], 3.0);
    }

    #[test]
    fn motion_in_a_definition_file_names_the_file_and_line() {
        let error = run("X1", definitions(&["DEF REAL FIRST", "DEF REAL SECOND\n\nG1 X10\n"])).unwrap_err();
        assert_eq!(error.definition_index(), Some(1));
        assert_eq!(error.location().unwrap().line, 3);
        assert!(error.to_string().contains("definitions[1]"), "{error}");
        for line in [
            "X=5",
            "R1=2 G1",
            "TRANS X1",
            "LABEL: R1=1",
            "IF R1==1 GOTOF END",
            "$P_UIFR[1]=0",
            "T=\"A\"",
        ] {
            assert!(run("X1", definitions(&[line])).is_err(), "{line} was accepted");
        }
    }

    #[test]
    fn end_code_must_stand_alone() {
        assert!(run("X1", definitions(&["DEF REAL A=1 M30"])).is_err());
        assert!(run("X1", definitions(&["N10 M30 ; end"])).is_ok());
    }

    #[test]
    fn variables_are_checked_against_the_declared_type() {
        let base = |name: &str, value: VariableValue| StartInputs {
            definitions: vec!["DEF INT COUNT\nDEF REAL LEN\nDEF STRING[4] TAG\nDEF BOOL ON\nDEF CHAR CH".to_string()],
            variables: vec![(name.to_string(), value)],
            start: None,
        };
        let ok = [
            ("COUNT", VariableValue::Int(3)),
            ("count", VariableValue::Real(4.0)),
            ("LEN", VariableValue::Real(2.5)),
            ("TAG", VariableValue::Text("abcd".into())),
            ("ON", VariableValue::Bool(true)),
            ("CH", VariableValue::Text("A".into())),
            ("R7", VariableValue::Real(1.0)),
            ("$A_YEAR", VariableValue::Int(24)),
        ];
        for (name, value) in ok {
            run("X1", base(name, value.clone())).unwrap_or_else(|e| panic!("{name}={value:?}: {e}"));
        }
        let bad = [
            ("COUNT", VariableValue::Real(2.5)),
            ("LEN", VariableValue::Text("x".into())),
            ("TAG", VariableValue::Real(1.0)),
            ("TAG", VariableValue::Text("abcde".into())),
            ("ON", VariableValue::Int(2)),
            ("UNDEFINED", VariableValue::Real(1.0)),
            ("X", VariableValue::Real(1.0)),
            ("TRUE", VariableValue::Real(1.0)),
        ];
        for (name, value) in bad {
            let error = run("X1", base(name, value.clone())).expect_err(&format!("{name}={value:?}"));
            assert_eq!(error.kind(), "invalid_input");
            assert!(error.to_string().contains(&format!("variables['{name}']")), "{error}");
        }
        let (_, state) = run("X1", base("TAG", VariableValue::Text("ab".into()))).unwrap();
        assert_eq!(state.string_table["TAG"], "ab");
    }

    #[test]
    fn a_def_in_the_main_program_reinitialises_a_passed_value() {
        let inputs = StartInputs {
            definitions: vec!["DEF REAL SPEED".to_string()],
            variables: vec![("SPEED".to_string(), VariableValue::Real(5.0))],
            start: None,
        };
        let (table, _) = run("X=SPEED\nDEF REAL SPEED\nX=SPEED", inputs).unwrap();
        assert_eq!(floats(&table, "X"), vec![Some(5.0), Some(0.0)]);
    }

    fn start(start: StartState) -> StartInputs {
        StartInputs {
            start: Some(start),
            ..StartInputs::default()
        }
    }

    #[test]
    fn start_seeds_the_first_row_with_g_groups_and_feed() {
        let state = StartState {
            g_groups: vec![
                ("gg01_motion".into(), "g1".into()),
                ("gg14_wp_measure_mode".into(), "G90".into()),
            ],
            feed: Some(1200.0),
            axes: vec![("X".into(), 3.0), ("Y".into(), 4.0)],
            ..StartState::default()
        };
        let (table, end) = run("X5\nG0 X6 F50", start(state)).unwrap();
        assert_eq!(texts(&table, "gg01_motion"), vec![Some("G1".into()), Some("G0".into())]);
        assert_eq!(floats(&table, "F"), vec![Some(1200.0), Some(50.0)]);
        // Positions seed the state only: no Y column appears.
        assert!(table.columns.iter().all(|(name, _)| name != "Y"));
        assert_eq!(end.axes["Y"], 4.0);
        assert_eq!(end.feed, Some(50.0));
        assert!(end.g_groups.contains(&("gg01_motion".to_string(), "G0".to_string())));
        assert!(end
            .g_groups
            .contains(&("gg14_wp_measure_mode".to_string(), "G90".to_string())));
    }

    #[test]
    fn start_g_groups_are_validated() {
        for (group, code) in [
            ("gg01_motion", "G90"),
            ("gg01_motion", "G7777"),
            ("gg02_wait", "G4"),
            ("motion", "G1"),
        ] {
            let state = StartState {
                g_groups: vec![(group.into(), code.into())],
                ..StartState::default()
            };
            let error = run("X1", start(state)).expect_err(&format!("{group}={code}"));
            assert_eq!(error.kind(), "invalid_input", "{error}");
        }
    }

    #[test]
    fn start_frame_and_axes_are_validated() {
        let bad = [
            StartState {
                axes: vec![("Q".into(), 1.0)],
                ..StartState::default()
            },
            StartState {
                translation: vec![("Q".into(), 1.0)],
                ..StartState::default()
            },
            StartState {
                rotation: Some([[2.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]]),
                ..StartState::default()
            },
        ];
        for state in bad {
            assert_eq!(run("X1", start(state)).unwrap_err().kind(), "invalid_input");
        }
    }

    #[test]
    fn start_frame_applies_to_the_main_program() {
        let state = StartState {
            translation: vec![("X".into(), 10.0)],
            rotation: Some([[0.0, -1.0, 0.0], [1.0, 0.0, 0.0], [0.0, 0.0, 1.0]]),
            ..StartState::default()
        };
        let (table, _) = run("X1 Y0", start(state)).unwrap();
        assert!((floats(&table, "X")[0].unwrap() - 10.0).abs() < 1e-9);
        assert!((floats(&table, "Y")[0].unwrap() - 1.0).abs() < 1e-9);
    }

    #[test]
    fn a_name_in_variables_and_start_is_an_error() {
        let inputs = StartInputs {
            definitions: vec!["DEF REAL SHARED".to_string()],
            variables: vec![("SHARED".to_string(), VariableValue::Real(1.0))],
            start: Some(StartState {
                symbol_table: vec![("shared".into(), 2.0)],
                ..StartState::default()
            }),
        };
        assert_eq!(run("X1", inputs).unwrap_err().kind(), "invalid_input");
    }

    #[test]
    fn start_variables_restore_a_snapshot() {
        let snapshot = StartState {
            symbol_table: vec![("TRUE".into(), 1.0), ("LOCAL_COPY".into(), 7.0)],
            string_table: vec![("NAME".into(), "abc".into())],
            ..StartState::default()
        };
        let (table, state) = run("X=LOCAL_COPY", start(snapshot)).unwrap();
        assert_eq!(floats(&table, "X"), vec![Some(7.0)]);
        assert_eq!(state.string_table["NAME"], "abc");
    }

    #[test]
    fn returned_state_seeds_the_same_first_row() {
        let program = "G1 G17 G90 X1 Y2 F300\nG2 X3 Y4 I1 J0\n";
        let (_, first) = run(program, StartInputs::default()).unwrap();
        let snapshot = StartState {
            axes: first.axes.iter().map(|(k, v)| (k.clone(), *v)).collect(),
            g_groups: first.g_groups.clone(),
            feed: first.feed,
            ..StartState::default()
        };
        let (table, second) = run("X5", start(snapshot)).unwrap();
        assert_eq!(texts(&table, "gg01_motion"), vec![Some("G2".into())]);
        assert_eq!(texts(&table, "gg06_plane_select"), vec![Some("G17".into())]);
        assert_eq!(floats(&table, "F"), vec![Some(300.0)]);
        assert_eq!(second.g_groups, first.g_groups);
    }
    #[test]
    fn chandata_one_is_ignored_and_other_channels_fail() {
        let ok = definitions(&["CHANDATA(1)\nDEF CHAN REAL FIRST=2\nM30\n"]);
        let (table, _) = run("X=FIRST", ok).unwrap();
        assert_eq!(floats(&table, "X"), vec![Some(2.0)]);
        let error = run("X1", definitions(&["DEF REAL FIRST\nCHANDATA(2)\n"])).unwrap_err();
        assert_eq!(error.location().unwrap().line, 2);
        assert!(error.to_string().contains("CHANDATA(2)"), "{error}");
    }

    #[test]
    fn assigning_an_undeclared_name_in_a_definition_file_fails() {
        let error = run("X1", definitions(&["DEF REAL KNOWN\nKNOWN=1\nUNKNOWN=2\n"])).unwrap_err();
        assert_eq!(error.location().unwrap().line, 3);
        assert!(
            error.to_string().contains("'UNKNOWN' is assigned but not declared"),
            "{error}"
        );
        // Declared in an earlier file, an R parameter, or allowed by the flag.
        assert!(run("X1", definitions(&["DEF REAL KNOWN", "KNOWN=1\nR5=2\nR[6]=3"])).is_ok());
        let inputs = definitions(&["LOOSE=1"]);
        let allowed = nc_to_table_with(
            "X1",
            Some(&inputs),
            None,
            None,
            1000,
            false,
            None,
            true,
            None,
            Extensions::default(),
        );
        assert!(allowed.is_ok());
    }

    #[test]
    fn tool_and_transformation_seed_the_first_row_and_round_trip() {
        let state = StartState {
            tool: Some("CUTTER".into()),
            transformation: Some("traori".into()),
            ..StartState::default()
        };
        let (table, end) = run("G0 X1\nX2", start(state)).unwrap();
        assert_eq!(texts(&table, "T"), vec![Some("CUTTER".into()), None]);
        assert_eq!(
            texts(&table, "transformation"),
            vec![Some("TRAORI".into()), Some("TRAORI".into())]
        );
        assert_eq!(end.tool.as_deref(), Some("CUTTER"));
        assert_eq!(end.transformation.as_deref(), Some("TRAORI"));
        let bad = StartState {
            transformation: Some("NOT_A_TRAFO".into()),
            ..StartState::default()
        };
        assert_eq!(run("X1", start(bad)).unwrap_err().kind(), "invalid_input");
    }
}
