//! Statement-AST executor (NC_EXEC=ast): runs one `ast::Block` against
//! `State` and `OutputRows` with the semantics of interpret_rules.rs (same
//! warnings, error kinds, messages and line context; previews come from
//! `state.get_line(block.line_no)`). The executor owns every semantic; the
//! driver (drive.rs) owns line order and the control stack. Expressions and
//! strings are evaluated in `expr`.
//!
//! `exec_block` runs what the tree walker runs when control reaches a line in
//! sequence or by a jump. A plain block starts its row, runs its body, the
//! end-of-block frame hook when it has statements, and returns Continue, Jump
//! or EndProgram. Structure lines are split as the whole-file grammar splits
//! them: on a head line the block number and label belong to the structure's
//! own block; on an ELSE or closer line they are the last block of the body
//! before it. So for every structure line `exec_block` starts the line's row
//! and inserts its block number, then IfHead inserts the comment and returns
//! Branch, WhileHead returns Branch, ForHead runs the counter assignment and
//! returns ForInit, LoopHead returns LoopStart, RepeatHead returns Repeat, and
//! Else and every closer (UNTIL included) return ScopeEnd. The other pieces are
//! separate calls the driver makes at the matching events: `begin_body`,
//! `eval_condition`, `for_continues`, `for_step` and `leave_structure`.

#![allow(unused_variables)] // structure-line entry points are still todo!()

use crate::ast::{
    AssignMulti, Assignment, Block, Body, Call, DataType, DefItem, Definition, ElementRhs, Expr, Index, MultiValues,
    Rhs, Statement,
};
use crate::errors::ParsingError;
use crate::interpret_rules::{
    insert_m_key, is_end_of_program_m_code, looks_like_axis_word_typo, JumpRequest, FRAME_KEYWORDS,
};
use crate::output::{CellMap, OutputRows};
use crate::state::{emit_warning, ColKind, State};
use crate::types::{Rule, Value};
use std::borrow::Cow;

pub(crate) mod expr;

/// What the driver does after `exec_block`.
#[derive(Debug, Clone)]
pub(crate) enum Flow {
    /// Go on with the next line.
    Continue,
    /// A jump to resolve through the scope chain (a GOTOC whose target is on
    /// no active scope already warned and returned Continue).
    Jump(JumpRequest),
    /// M2/M17/M30 ran; the rest of its block ran too, nothing after it runs.
    EndProgram,
    /// The condition of an IfHead, or the entry test of a WhileHead.
    Branch(bool),
    /// ForHead initialized counter `var`; the body runs while
    /// `for_continues(var, to)`. `to` is evaluated once, here.
    ForInit { var: String, to: f64 },
    /// LoopHead reached.
    LoopStart,
    /// RepeatHead reached.
    Repeat,
    /// Else or a closer reached: the body before it is done.
    ScopeEnd,
}

/// Run a block reached in sequence or by a jump (see the module doc for
/// structure lines).
pub(crate) fn exec_block(block: &Block, state: &mut State, output: &mut OutputRows) -> Result<Flow, ParsingError> {
    let line_no = block.line_no;
    output.start_row(line_no)?;
    if let Some(number) = &block.block_number {
        row(output).insert("N", Value::Str(number.clone()));
    }
    // A jump label is inert at run time: the driver indexes it before the run.
    match &block.body {
        Body::Empty => {}
        Body::Definition(definition) => define(definition, state, output, line_no)?,
        Body::FrameOp(_) => todo!("phase 2: frame instructions"),
        Body::Statements(statements) => {
            let mut ends_program = false;
            for statement in statements {
                ends_program |= exec_statement(statement, state, output, line_no)?;
            }
            insert_comment(block, output);
            // Only a statement row can carry arc words (interpret_block's had_statements).
            state.finish_block_frame(row(output), line_no)?;
            return Ok(if ends_program { Flow::EndProgram } else { Flow::Continue });
        }
        Body::Control(_) => todo!("phase 2: control statements"),
    }
    insert_comment(block, output);
    Ok(Flow::Continue)
}

/// Start one pass of a loop body, or the ELSE branch: the comment of the head
/// or ELSE line is the body's first block, so this starts a row on that line
/// and inserts the comment (an empty row when there is none).
pub(crate) fn begin_body(head: &Block, output: &mut OutputRows) -> Result<(), ParsingError> {
    todo!("phase 2")
}

/// Evaluate the condition of a WhileHead (each re-test after ENDWHILE) or an
/// UntilTail (after the driver's loop-limit check), without starting a row.
pub(crate) fn eval_condition(block: &Block, state: &mut State) -> Result<bool, ParsingError> {
    todo!("phase 2")
}

/// The FOR test: `var` is in the symbol table and not greater than `to` (an
/// axis counter is not, so its body never runs, as in the tree walker).
pub(crate) fn for_continues(var: &str, to: f64, state: &State) -> bool {
    todo!("phase 2")
}

/// The ENDFOR increment: the counter plus one, recorded on the current row
/// (the closer line's, which `exec_block` started).
pub(crate) fn for_step(var: &str, state: &mut State, output: &mut OutputRows) -> Result<(), ParsingError> {
    todo!("phase 2")
}

/// A structure was left by falling out of it, a jump out of its body, or an
/// end of program inside it (never on error): the comment of its closer line
/// goes into the current row, innermost structure first.
pub(crate) fn leave_structure(closer: &Block, output: &mut OutputRows) {
    todo!("phase 2")
}

/// The row being filled; `OutputRows` always has one.
fn row(output: &mut OutputRows) -> &mut CellMap {
    output.last_mut().expect("OutputRows always has a current row")
}

fn insert_comment(block: &Block, output: &mut OutputRows) {
    if let Some(comment) = &block.comment {
        row(output).insert("comment", Value::Str(comment.clone()));
    }
}

/// The error preview, as `get_error_context` builds it.
fn preview(state: &State, line_no: usize) -> String {
    state
        .get_line(line_no)
        .unwrap_or("(could not retrieve line)")
        .to_string()
}

fn context_error(state: &State, line_no: usize, context: &str, message: String) -> ParsingError {
    ParsingError::with_context(line_no, preview(state, line_no), context.to_string(), message)
}

// ---------------------------------------------------------------------------
// Statements (interpret_statement) and assignments (interpret_assignment)
// ---------------------------------------------------------------------------

/// One statement of a block; true when it is an end-of-program M code.
fn exec_statement(
    statement: &Statement,
    state: &mut State,
    output: &mut OutputRows,
    line_no: usize,
) -> Result<bool, ParsingError> {
    match statement {
        Statement::Assign(assignment) => {
            let assigned = assign(assignment, state, line_no)?;
            // A string assignment lives in the string table only.
            let Some(value) = assigned.value else {
                return Ok(false);
            };
            match assigned.column {
                // State keeps the local coordinate; the row gets the machine one.
                Some((ColKind::Axis, column)) => state.insert_machine_axis(column, value, row(output)),
                Some((ColKind::Block, column)) => {
                    row(output).insert(column, Value::Float(value));
                }
                None => {
                    state.warn_unsupported_address(&assigned.key, line_no);
                    output.record_variable_change(&assigned.key, value);
                }
            }
        }
        Statement::AssignMulti(_) => {
            // interpret_statement has no SET/REP arm: they are valid in a DEF only.
            return Err(ParsingError::UnexpectedRule {
                rule: Rule::assignment_multi,
                context: "interpret_statement".to_string(),
                line_no,
                preview: state.get_line(line_no).unwrap_or("").to_string(),
                message: format!("Unexpected rule in interpret_statement: {:?}", Rule::assignment_multi),
            });
        }
        Statement::G(code) => exec_g_command(code, state, output, line_no)?,
        Statement::M(code) => {
            insert_m_key(row(output), code, line_no, preview(state, line_no))?;
            // The rest of the block still runs; the program ends after it.
            return Ok(is_end_of_program_m_code(code));
        }
        Statement::Tool(name) => {
            row(output).insert("T", Value::Str(name.clone()));
        }
        Statement::Call(call) => exec_call(call, state, output, line_no)?,
    }
    Ok(false)
}

/// A G command; the canonical form already turned G-vocabulary keywords into
/// `Statement::G`, so an unknown code here is alarm 12470.
fn exec_g_command(code: &str, state: &mut State, output: &mut OutputRows, line_no: usize) -> Result<(), ParsingError> {
    let Some((group, _modal)) = crate::modal_groups::classify_g_command(code) else {
        return Err(ParsingError::UnknownGCommand {
            line_no,
            preview: preview(state, line_no),
            code: code.to_string(),
        });
    };
    // The frame model needs the active plane for RPL and the arc check.
    if group == "gg06_plane_select" {
        state.set_plane(code);
    }
    row(output).insert(group, Value::Str(code.to_string()));
    Ok(())
}

/// A subprogram-like call, stored as written; a parenless G keyword never gets
/// here (canonical form), a parenless frame keyword is an error.
fn exec_call(call: &Call, state: &State, output: &mut OutputRows, line_no: usize) -> Result<(), ParsingError> {
    if call.args.is_none() {
        // A frame instruction must be alone in its block; after another statement it would move an axis.
        if FRAME_KEYWORDS
            .iter()
            .any(|keyword| keyword.eq_ignore_ascii_case(&call.name))
        {
            return Err(ParsingError::UnsupportedStatement {
                line_no,
                preview: preview(state, line_no),
                statement: format!("The frame instruction {} after another statement", call.name),
                hint: "Frame instructions must be programmed in a separate NC block (manual 3.12.2.1).".to_string(),
            });
        }
        if looks_like_axis_word_typo(&call.name) {
            emit_warning(format_args!(
                "Warning: '{}' (line {}) is interpreted as a subprogram call; did you mean one or more axis words (e.g. Y20)?",
                call.name, line_no
            ));
        }
    }
    row(output).insert("non_returning_function_call", Value::Str(call.text.clone()));
    Ok(())
}

/// The outcome of one assignment: the target key, the number stored (`None`
/// for a string), and the key's output column (`None` for a user variable).
struct Assigned<'a> {
    key: Cow<'a, str>,
    value: Option<f64>,
    column: Option<(ColKind, &'static str)>,
}

/// `interpret_assignment`: a string right-hand side goes to the string table;
/// a number moves an axis (local coordinate), is a block address (row only),
/// or is a user variable. Names are uppercase already (canonical form).
fn assign<'a>(assignment: &'a Assignment, state: &mut State, line_no: usize) -> Result<Assigned<'a>, ParsingError> {
    match assignment {
        Assignment::Var { name, rhs } => {
            let value = match rhs {
                // The axis-word hot path: a literal is never string-valued.
                Rhs::Expr(Expr::Number(value)) => *value,
                Rhs::Expr(value) if expr::expr_is_string(value, state) => {
                    let text = expr::eval_expr_string(value, state, line_no)?;
                    return assign_string(name, text, state, line_no);
                }
                Rhs::Expr(value) => expr::eval_expr(value, state, line_no)?,
                Rhs::Increment(increment) => incremented(name, increment, state, line_no)?,
                Rhs::Str(value) => {
                    let text = expr::eval_string(value, state, line_no)?;
                    return assign_string(name, text, state, line_no);
                }
            };
            store_number(Cow::Borrowed(name), value, state, line_no)
        }
        Assignment::Element { name, indices, rhs } => {
            let value = match rhs {
                ElementRhs::Expr(value) if !expr::expr_is_string(value, state) => value,
                ElementRhs::Expr(value) => {
                    let text = expr::eval_expr_string(value, state, line_no)?;
                    return write_char(name, indices, text, state, line_no);
                }
                ElementRhs::Str(value) => {
                    let text = expr::eval_string(value, state, line_no)?;
                    return write_char(name, indices, text, state, line_no);
                }
            };
            if expr::is_actual_position_sysvar(name) {
                return Err(context_error(
                    state,
                    line_no,
                    "actual-position system variable",
                    format!("{name} is a read-only actual position; it cannot be assigned"),
                ));
            }
            let key = expr::element_key(name, indices, state, line_no)?;
            let value = expr::eval_expr(value, state, line_no)?;
            store_number(Cow::Owned(key), value, state, line_no)
        }
    }
}

/// Store a number under `key`. Axis and block-address classification goes
/// through the interned column registry, one lookup for both checks.
fn store_number<'a>(
    key: Cow<'a, str>,
    value: f64,
    state: &mut State,
    line_no: usize,
) -> Result<Assigned<'a>, ParsingError> {
    let column = state.resolve_output_key(&key);
    match column {
        Some((ColKind::Axis, axis)) => state.update_axis(axis, value)?,
        // Block addresses (I/J/K, CR, spline PW/SD/PL) only appear in the row.
        Some((ColKind::Block, _)) => {}
        None => {
            // One type per name: a STRING variable must not silently become numeric.
            if state.string_table.contains_key(key.as_ref()) {
                return Err(context_error(
                    state,
                    line_no,
                    "assignment",
                    format!("'{key}' is a STRING variable; it cannot be assigned a number"),
                ));
            }
            match state.symbol_table.get_mut(key.as_ref()) {
                Some(slot) => *slot = value,
                None => {
                    state.symbol_table.insert(key.to_string(), value);
                }
            }
        }
    }
    Ok(Assigned {
        key,
        value: Some(value),
        column,
    })
}

/// A whole-string assignment to a STRING variable.
fn assign_string<'a>(
    name: &'a str,
    text: String,
    state: &mut State,
    line_no: usize,
) -> Result<Assigned<'a>, ParsingError> {
    if state.is_axis(name) || state.is_block_address(name) {
        return Err(context_error(
            state,
            line_no,
            "a numeric value",
            format!("cannot assign a string to '{name}'"),
        ));
    }
    if state.symbol_table.contains_key(name) {
        return Err(context_error(
            state,
            line_no,
            "a numeric value",
            format!("'{name}' is a numeric variable; it cannot be assigned a string"),
        ));
    }
    state.string_table.insert(name.to_string(), text);
    Ok(Assigned {
        key: Cow::Borrowed(name),
        value: None,
        column: None,
    })
}

/// `IC()`: the new local coordinate is the current one plus the increment, or
/// the bare increment (with a warning) for a key that has no position yet.
fn incremented(name: &str, increment: &Expr, state: &mut State, line_no: usize) -> Result<f64, ParsingError> {
    let increment = expr::eval_expr(increment, state, line_no)?;
    Ok(match state.get_axis_local(name) {
        Some(local) => local + increment,
        None => {
            emit_warning(format_args!(
                "Warning: axis '{}' is incremented with IC() before any absolute position was set; assuming it starts at 0 (a real control would start from the actual axis position).",
                name
            ));
            increment
        }
    })
}

/// `STRING[<index>] = "<char>"` (manual 4.1.4.8): replace one character of a
/// user STRING variable; the index is 0-based.
fn write_char<'a>(
    name: &'a str,
    indices: &[Index],
    text: String,
    state: &mut State,
    line_no: usize,
) -> Result<Assigned<'a>, ParsingError> {
    const CONTEXT: &str = "single-character string write";
    // A real control raises alarm 12620 for a system variable.
    if name.starts_with('$') {
        return Err(context_error(
            state,
            line_no,
            CONTEXT,
            format!("'{name}' is a system variable; single-character write is only allowed for user-defined variables"),
        ));
    }
    if !state.string_table.contains_key(name) {
        return Err(context_error(
            state,
            line_no,
            CONTEXT,
            format!(
                "'{name}' is not a STRING variable; only strings support single-character (STRING[i] = \"c\") writes"
            ),
        ));
    }
    let index = match expr::eval_indices(indices, state, line_no)?.as_slice() {
        [index] => *index as usize,
        _ => {
            return Err(context_error(
                state,
                line_no,
                CONTEXT,
                format!("single-character write into '{name}' requires exactly one index"),
            ))
        }
    };
    let mut rhs_chars = text.chars();
    let new_char = match (rhs_chars.next(), rhs_chars.next()) {
        (Some(c), None) => c,
        _ => {
            return Err(context_error(
                state,
                line_no,
                CONTEXT,
                format!("single-character write into '{name}' expects exactly one character on the right-hand side, got \"{text}\""),
            ))
        }
    };
    let mut chars: Vec<char> = state.string_table[name].chars().collect();
    if index >= chars.len() {
        return Err(context_error(
            state,
            line_no,
            CONTEXT,
            format!(
                "index {index} is out of range for '{name}' (length {}); valid range is 0..{}",
                chars.len(),
                chars.len().saturating_sub(1)
            ),
        ));
    }
    chars[index] = new_char;
    state.string_table.insert(name.to_string(), chars.into_iter().collect());
    Ok(Assigned {
        key: Cow::Borrowed(name),
        value: None,
        column: None,
    })
}

/// `NAME[..]=SET(..)`/`REP(..)` (interpret_assignment_multi): each element
/// key with the value stored, `None` where a SET gap left it untouched.
fn assign_multi(
    multi: &AssignMulti,
    state: &mut State,
    line_no: usize,
) -> Result<Vec<(String, Option<f64>)>, ParsingError> {
    let keys = expr::element_keys(&multi.name, &multi.indices, state, line_no)?;
    let values: Vec<Option<f64>> = match &multi.values {
        MultiValues::Set(items) => items
            .iter()
            .map(|item| {
                item.as_ref()
                    .map(|value| expr::eval_expr(value, state, line_no))
                    .transpose()
            })
            .collect::<Result<_, _>>()?,
        // interpret_value_array reads REP's value and count as a two-item SET.
        MultiValues::Rep { value, count } => {
            let mut values = vec![Some(expr::eval_expr(value, state, line_no)?)];
            if let Some(count) = count {
                values.push(Some(expr::eval_expr(count, state, line_no)?));
            }
            values
        }
    };
    if values.len() > keys.len() {
        return Err(ParsingError::InvalidElementCount {
            expected: keys.len(),
            actual: values.len(),
        });
    }
    Ok(keys
        .into_iter()
        .enumerate()
        .map(|(position, key)| {
            let value = values.get(position).copied().flatten();
            if let Some(value) = value {
                state.symbol_table.insert(key.clone(), value);
            }
            (key, value)
        })
        .collect())
}

// ---------------------------------------------------------------------------
// Definitions (interpret_definition)
// ---------------------------------------------------------------------------

fn define(
    definition: &Definition,
    state: &mut State,
    output: &mut OutputRows,
    line_no: usize,
) -> Result<(), ParsingError> {
    // INT, REAL and BOOL are all f64; STRING[n] routes the names to the string table.
    let is_string = match definition.data_type {
        DataType::String(_) => true,
        DataType::Int | DataType::Real | DataType::Bool => false,
    };
    for item in &definition.items {
        match item {
            DefItem::Init(assignment) => {
                let (Assignment::Var { name, .. } | Assignment::Element { name, .. }) = assignment;
                // Check the declared name first, so the collision and not a type error surfaces.
                check_declared_name(name, state, line_no)?;
                let assigned = assign(assignment, state, line_no)?;
                check_declared_name(&assigned.key, state, line_no)?;
                match assigned.value {
                    Some(_) if is_string => {
                        return Err(context_error(
                            state,
                            line_no,
                            "interpret_definition",
                            format!("DEF STRING variable '{}' initialized with a number", assigned.key),
                        ))
                    }
                    Some(value) => output.record_variable_change(&assigned.key, value),
                    None if !is_string => {
                        return Err(context_error(
                            state,
                            line_no,
                            "interpret_definition",
                            format!(
                                "numeric variable '{}' initialized with a string (declare it DEF STRING[n])",
                                assigned.key
                            ),
                        ))
                    }
                    None => {}
                }
            }
            DefItem::Multi(multi) => {
                for (key, value) in assign_multi(multi, state, line_no)? {
                    if let Some(value) = value {
                        output.record_variable_change(&key, value);
                    }
                }
            }
            DefItem::Var(name) => {
                check_declared_name(name, state, line_no)?;
                if is_string {
                    state.string_table.insert(name.clone(), String::new());
                } else {
                    state.symbol_table.insert(name.clone(), 0.0);
                    output.record_variable_change(name, 0.0);
                }
            }
            // The walker declares array elements numeric whatever the DEF type.
            DefItem::Array { name, indices } => {
                for key in expr::element_keys(name, indices, state, line_no)? {
                    output.record_variable_change(&key, 0.0);
                    state.symbol_table.insert(key, 0.0);
                }
            }
        }
    }
    Ok(())
}

/// A DEF must not declare an axis letter or a block address (a real control
/// rejects `DEF STRING[13] S`, S being the spindle).
fn check_declared_name(name: &str, state: &State, line_no: usize) -> Result<(), ParsingError> {
    if state.is_axis(name) {
        Err(ParsingError::AxisUsedAsVariable {
            name: name.to_string(),
            line_no,
            preview: preview(state, line_no),
        })
    } else if state.is_block_address(name) {
        Err(ParsingError::ReservedNameUsedAsVariable {
            name: name.to_string(),
            line_no,
            preview: preview(state, line_no),
        })
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::Control;
    use crate::interpret_rules::{canonical_block_number, interpret_blocks, resolve_jump, BlockFlow};
    use crate::output::{Row, Table};
    use crate::types::NCParser;
    use pest::Parser;
    use std::collections::HashMap;

    #[derive(Clone, Copy)]
    struct Opts {
        allow_undefined: bool,
        limit: usize,
    }

    const OPTS: Opts = Opts {
        allow_undefined: false,
        limit: 1000,
    };

    /// Sorted rows (cell order is irrelevant to the output) and final state, or the error.
    type Outcome = Result<(Vec<String>, String), String>;

    fn new_state(program: &str, opts: Opts) -> State {
        let mut state = crate::interpreter::build_state(None, None, opts.limit, None, opts.allow_undefined);
        state.set_input(program);
        state
    }

    fn view_rows(rows: &[Row]) -> Vec<String> {
        rows.iter()
            .map(|row| {
                let mut cells: Vec<String> = row
                    .cells
                    .iter()
                    .map(|(key, value)| format!("{key}={value:?}"))
                    .collect();
                cells.sort();
                format!("line {} {cells:?} vars {:?}", row.line_no, row.variable_changes)
            })
            .collect()
    }

    fn view_state(state: &State) -> String {
        fn sorted<V: std::fmt::Debug>(map: impl Iterator<Item = (String, V)>) -> Vec<String> {
            let mut entries: Vec<String> = map.map(|(key, value)| format!("{key}={value:?}")).collect();
            entries.sort();
            entries
        }
        format!(
            "axes {:?}\nsymbols {:?}\nstrings {:?}\ntranslation {:?}\nrotation {:?}\nplane {:?}",
            sorted(state.axes.iter().map(|(k, v)| (k.clone(), *v))),
            sorted(state.symbol_table.iter().map(|(k, v)| (k.clone(), *v))),
            sorted(state.string_table.iter().map(|(k, v)| (k.clone(), v.clone()))),
            sorted(state.translation.iter().map(|(k, v)| (k.clone(), *v))),
            state.rotation,
            state.plane,
        )
    }

    /// Run `program` with a streaming sink, so rows keep their variable deltas.
    fn outcome(
        program: &str,
        opts: Opts,
        run: fn(&str, &mut State, &mut OutputRows) -> Result<(), ParsingError>,
    ) -> Outcome {
        let mut state = new_state(program, opts);
        let (sender, receiver) = std::sync::mpsc::sync_channel(1 << 16);
        let mut output = OutputRows::stream(sender);
        run(program, &mut state, &mut output)
            .and_then(|()| output.finish())
            .map_err(|e| format!("{e:?}"))?;
        let rows: Vec<Row> = receiver.try_iter().collect();
        Ok((view_rows(&rows), view_state(&state)))
    }

    /// The tree walker over the whole-file parse, the code exec ports.
    fn run_walker(program: &str, state: &mut State, output: &mut OutputRows) -> Result<(), ParsingError> {
        let file = NCParser::parse(Rule::file, program)
            .map_err(|e| ParsingError::ParseError { message: e.to_string() })?
            .next()
            .expect("file pair");
        let blocks = file.into_inner().next().expect("blocks pair");
        match interpret_blocks(blocks, output, state)? {
            BlockFlow::Continue | BlockFlow::EndProgram => Ok(()),
            BlockFlow::Jump(request) => Err(request.into_not_found_error(state)),
        }
    }

    fn run_ast(program: &str, state: &mut State, output: &mut OutputRows) -> Result<(), ParsingError> {
        Driver::run(program, state, output)
    }

    /// The default path's table, against the table of exec's rows (collect sink).
    fn tables(program: &str, opts: Opts) -> (Result<String, String>, Result<String, String>) {
        let ours = (|| {
            let mut state = new_state(program, opts);
            let mut output = OutputRows::collect();
            run_ast(program, &mut state, &mut output)?;
            let rows = output.finish()?;
            Ok::<_, ParsingError>(format!("{:?}", Table::from_rows(&rows, true).columns))
        })()
        .map_err(|e| format!("{e:?}"));
        let default = crate::interpreter::nc_to_table(
            program,
            None,
            None,
            None,
            opts.limit,
            true,
            None,
            opts.allow_undefined,
            None,
        )
        .map(|(table, _)| format!("{:?}", table.columns))
        .map_err(|e| format!("{e:?}"));
        (ours, default)
    }

    fn assert_parity_with(program: &str, opts: Opts) {
        let walker = outcome(program, opts, run_walker);
        if let Err(e) = &walker {
            panic!("the walker rejects:\n{program}\n{e}");
        }
        compare(program, opts, walker);
    }

    fn compare(program: &str, opts: Opts, walker: Outcome) {
        let ours = outcome(program, opts, run_ast);
        assert_eq!(ours, walker, "rows and state differ from the walker for:\n{program}");
        let (ours, default) = tables(program, opts);
        assert_eq!(ours, default, "table differs from nc_to_table for:\n{program}");
    }

    /// Parity on a program the walker runs without error.
    fn assert_parity(program: &str) {
        assert_parity_with(program, OPTS);
    }

    /// Parity on a program the walker rejects (an error path).
    fn assert_error_parity(program: &str) {
        let walker = outcome(program, OPTS, run_walker);
        assert!(walker.is_err(), "the walker accepts:\n{program}");
        compare(program, OPTS, walker);
    }

    /// Just enough driver to run exec: one flat jump scope (run_blocks), and
    /// structures run in place; a jump out of a structure resolves at top level
    /// from its head line, as the walker resolves it from the structure's block.
    struct Driver<'a> {
        blocks: Vec<Block>,
        state: &'a mut State,
        output: &'a mut OutputRows,
    }

    enum Stop {
        Eof,
        End,
        Scope(usize),
        Jump(JumpRequest, usize),
        Next(usize),
    }

    impl Driver<'_> {
        fn run(program: &str, state: &mut State, output: &mut OutputRows) -> Result<(), ParsingError> {
            let blocks = program
                .lines()
                .enumerate()
                .map(|(index, line)| crate::lower::parse_line(line, index + 1, state))
                .collect::<Result<Vec<_>, _>>()?;
            let mut targets: HashMap<String, Vec<usize>> = HashMap::new();
            for (index, block) in blocks.iter().enumerate() {
                if let Some(number) = &block.block_number {
                    let key = format!("N:{}", canonical_block_number(number));
                    targets.entry(key).or_default().push(index);
                }
                if let Some(label) = &block.label {
                    targets.entry(format!("LABEL:{label}")).or_default().push(index);
                }
            }
            state.seen_jump_targets.extend(targets.keys().cloned());
            state.jump_scopes.push(targets.keys().cloned().collect());
            let limit = state.iteration_limit;
            let mut driver = Driver { blocks, state, output };
            let mut index = 0;
            let mut backward_jumps = 0;
            loop {
                match driver.body(index)? {
                    Stop::Eof | Stop::End => break,
                    Stop::Jump(request, from) => match resolve_jump(&targets, from, &request) {
                        Some(destination) => {
                            if destination <= from {
                                backward_jumps += 1;
                                if backward_jumps >= limit {
                                    return Err(ParsingError::LoopLimit {
                                        limit: limit.to_string(),
                                    });
                                }
                            }
                            index = destination;
                        }
                        None => return Err(request.into_not_found_error(driver.state)),
                    },
                    Stop::Scope(line) | Stop::Next(line) => panic!("stray stop at line index {line}"),
                }
            }
            driver.state.jump_scopes.pop();
            Ok(())
        }

        /// Lines from `index` until a closer (Scope) or an exit.
        fn body(&mut self, mut index: usize) -> Result<Stop, ParsingError> {
            while index < self.blocks.len() {
                let stop = match exec_block(&self.blocks[index], self.state, self.output)? {
                    Flow::Continue => Stop::Next(index + 1),
                    Flow::EndProgram => Stop::End,
                    Flow::Jump(request) => Stop::Jump(request, index),
                    Flow::ScopeEnd => Stop::Scope(index),
                    Flow::Branch(taken) => match &self.blocks[index].body {
                        Body::Control(Control::IfHead(_)) => self.if_block(index, taken)?,
                        _ => self.while_loop(index, taken)?,
                    },
                    Flow::ForInit { var, to } => self.for_loop(index, &var, to)?,
                    Flow::LoopStart => self.endless(index)?,
                    Flow::Repeat => self.repeat(index)?,
                };
                match stop {
                    Stop::Next(next) => index = next,
                    other => return Ok(other),
                }
            }
            Ok(Stop::Eof)
        }

        /// One body run; `Some` when it left by an end of program or a jump.
        fn pass(&mut self, start: usize) -> Result<Option<Stop>, ParsingError> {
            match self.body(start)? {
                Stop::Scope(_) => Ok(None),
                stop @ (Stop::End | Stop::Jump(..)) => Ok(Some(stop)),
                Stop::Eof | Stop::Next(_) => panic!("body from line index {start} has no closer"),
            }
        }

        fn leave(&mut self, head: usize, closer: usize, left: Option<Stop>) -> Result<Stop, ParsingError> {
            leave_structure(&self.blocks[closer], self.output);
            Ok(match left {
                None => Stop::Next(closer + 1),
                Some(Stop::Jump(request, _)) => Stop::Jump(request, head),
                Some(stop) => stop,
            })
        }

        /// The ELSE at depth zero and the closer of the structure opened on `head`.
        fn closer(&self, head: usize) -> (Option<usize>, usize) {
            let mut depth = 0;
            let mut else_line = None;
            for (index, block) in self.blocks.iter().enumerate().skip(head + 1) {
                let Body::Control(control) = &block.body else { continue };
                match control {
                    Control::IfHead(_)
                    | Control::WhileHead(_)
                    | Control::ForHead { .. }
                    | Control::RepeatHead
                    | Control::LoopHead => depth += 1,
                    Control::EndIf | Control::EndWhile | Control::EndFor | Control::UntilTail(_) | Control::EndLoop => {
                        if depth == 0 {
                            return (else_line, index);
                        }
                        depth -= 1;
                    }
                    Control::Else if depth == 0 => else_line = Some(index),
                    _ => {}
                }
            }
            panic!("no closer for line index {head}")
        }

        fn if_block(&mut self, head: usize, taken: bool) -> Result<Stop, ParsingError> {
            let (else_line, closer) = self.closer(head);
            let left = if taken {
                self.pass(head + 1)?
            } else if let Some(else_line) = else_line {
                begin_body(&self.blocks[else_line], self.output)?;
                self.pass(else_line + 1)?
            } else {
                None
            };
            self.leave(head, closer, left)
        }

        fn while_loop(&mut self, head: usize, mut condition: bool) -> Result<Stop, ParsingError> {
            let (_, closer) = self.closer(head);
            let mut count = 0;
            let mut left = None;
            while condition && count < self.state.iteration_limit {
                count += 1;
                begin_body(&self.blocks[head], self.output)?;
                left = self.pass(head + 1)?;
                if left.is_some() {
                    break;
                }
                condition = eval_condition(&self.blocks[head], self.state)?;
            }
            if left.is_none() && count >= self.state.iteration_limit {
                return Err(ParsingError::LoopLimit {
                    limit: self.state.iteration_limit.to_string(),
                });
            }
            self.leave(head, closer, left)
        }

        fn for_loop(&mut self, head: usize, var: &str, to: f64) -> Result<Stop, ParsingError> {
            let (_, closer) = self.closer(head);
            let mut left = None;
            while for_continues(var, to, self.state) {
                begin_body(&self.blocks[head], self.output)?;
                left = self.pass(head + 1)?;
                if left.is_some() {
                    break;
                }
                for_step(var, self.state, self.output)?;
            }
            self.leave(head, closer, left)
        }

        fn endless(&mut self, head: usize) -> Result<Stop, ParsingError> {
            let (_, closer) = self.closer(head);
            let mut count = 0;
            loop {
                begin_body(&self.blocks[head], self.output)?;
                if let Some(stop) = self.pass(head + 1)? {
                    return self.leave(head, closer, Some(stop));
                }
                count += 1;
                if count >= self.state.iteration_limit {
                    return Err(ParsingError::LoopLimit {
                        limit: self.state.iteration_limit.to_string(),
                    });
                }
            }
        }

        fn repeat(&mut self, head: usize) -> Result<Stop, ParsingError> {
            let (_, closer) = self.closer(head);
            let mut count = 0;
            loop {
                begin_body(&self.blocks[head], self.output)?;
                if let Some(stop) = self.pass(head + 1)? {
                    return self.leave(head, closer, Some(stop));
                }
                count += 1;
                if count >= self.state.iteration_limit {
                    return Err(ParsingError::LoopLimit {
                        limit: self.state.iteration_limit.to_string(),
                    });
                }
                if eval_condition(&self.blocks[closer], self.state)? {
                    return self.leave(head, closer, None);
                }
            }
        }
    }

    // -----------------------------------------------------------------------
    // Statements and assignments
    // -----------------------------------------------------------------------

    #[test]
    fn axis_words_and_variables() {
        assert_parity("G1 X1 Y2 Z3\nX=4\nx5 y=6\nR1=5\nR2=R1*2 X=R2\nF2400 S1000\nN10 X7 ;done\n");
        assert_parity("DEF REAL Q\nQ=3\nlower_case=Q+1\nX=lower_case\n");
    }

    #[test]
    fn incremental_positions() {
        assert_parity("X10 Y5\nX=IC(2) Y=IC(-1)\nX=IC(2*3)\n");
        // IC() before any position warns and starts from 0, also on a user variable.
        assert_parity("X=IC(5)\nR1=IC(2)\nR1=IC(3)\n");
    }

    #[test]
    fn block_addresses_and_unsupported_addresses() {
        assert_parity("G17 G1 X0 Y0\nG2 X1 Y1 I0.5 J0.5\nG3 X2 Y0 CR=1 TURN=1\nPW=2 SD=3 PL=0.5\n");
        assert_parity("G2 X1 Y1 AR=90\nAP=30 RP=5\n");
    }

    #[test]
    fn undefined_variables() {
        assert_error_parity("X=R9\n");
        assert_parity_with(
            "X=R9 Y=R9+1\n",
            Opts {
                allow_undefined: true,
                ..OPTS
            },
        );
    }

    #[test]
    fn string_variables() {
        assert_parity("DEF STRING[10] NAME\nNAME=\"abc\"\nNAME[1]=\"X\"\nR1=STRLEN(NAME)\n");
        assert_parity("DEF STRING[10] A_S, B_S=\"x\"\nA_S=B_S << \"y\" << 2.5\nB_S=A_S\n");
        assert_parity("DEF STRING[20] TXT\nTXT=SPRINT(\"%D-%S\", 7, \"q\")\nTXT[0]=SUBSTR(TXT, 2, 1)\n");
    }

    #[test]
    fn string_errors() {
        // One type per name, both ways, and strings never reach axes or block addresses.
        assert_error_parity("DEF STRING[4] NAME\nNAME=5\n");
        assert_error_parity("R1=5\nR1=\"a\"\n");
        assert_error_parity("DEF STRING[4] NAME\nX=NAME\n");
        assert_error_parity("I=\"a\"\n");
        assert_error_parity("DEF STRING[4] NAME\nX=IC(NAME)\n");
    }

    #[test]
    fn single_character_writes() {
        assert_error_parity("$SYS[0]=\"a\"\n");
        assert_error_parity("DEF REAL ARR[2]\nARR[0]=\"a\"\n");
        assert_error_parity("DEF STRING[4] NAME=\"ab\"\nNAME[0,1]=\"a\"\n");
        assert_error_parity("DEF STRING[4] NAME=\"ab\"\nNAME[0]=\"cd\"\n");
        assert_error_parity("DEF STRING[4] NAME=\"ab\"\nNAME[5]=\"c\"\n");
        assert_parity("DEF STRING[4] NAME=\"ab\"\nDEF STRING[1] C_S=\"z\"\nNAME[1]=C_S\n");
    }

    #[test]
    fn array_elements() {
        assert_parity("DEF REAL ARR[3]\nARR[2]=5\nX=ARR[2]\nARR[1]=ARR[2]*2\n");
        assert_parity("DEF REAL GRID[2,3]\nGRID[1,2]=4\nY=GRID[1,2]\n");
        assert_error_parity("$AA_IW[X]=5\n");
        assert_error_parity("X5\n$AA_IM[X]=1\n");
        assert_parity("X5\nR1=$AA_IW[X]\n");
    }

    #[test]
    fn g_codes() {
        assert_parity("G0\nG1 X1\nG17\nG18 G2 X2 Z1 I1 K0\nG19\nG90 G54\nBSPLINE\nG64 SOFT\n");
        assert_error_parity("G999\n");
        assert_error_parity("X1 G1234 Y2\n");
    }

    #[test]
    fn m_codes_and_end_of_program() {
        assert_parity("M3\nM3 M8 X1\nM30\nX99\n");
        // The rest of the ending block still runs.
        assert_parity("X1 M30 Y2 ;end\nY99\n");
        assert_parity("M02\nX99\n");
        assert_parity("M17\nX99\n");
        assert_error_parity("M1 M3 M4 M5 M6 M7\n");
    }

    #[test]
    fn tool_selection() {
        assert_parity("T=\"DRILL_10\"\nT=\"\"\n");
    }

    #[test]
    fn calls() {
        assert_parity("MYSUB\nCYCLE81(1, 2, , 3)\nLAYER_CHANGE ;c\nL100\nFOO()\n");
        // Y2O looks like an axis word with a typo: a warning, still a call.
        assert_parity("Y2O\nX10Y20\n");
        assert_parity("SUB(\"txt\", 2+3)\n");
    }

    #[test]
    fn frame_keyword_after_another_statement() {
        assert_error_parity("G1 X1 TRANS\n");
        assert_error_parity("X1 MIRROR\n");
        assert_error_parity("G0 CROTS\n");
    }

    #[test]
    fn set_and_rep_outside_def() {
        assert_error_parity("DEF REAL ARR[3]\nARR[0]=SET(1,2)\n");
        assert_error_parity("DEF REAL ARR[3]\nX1 ARR[0]=REP(1)\n");
    }

    // -----------------------------------------------------------------------
    // Definitions
    // -----------------------------------------------------------------------

    #[test]
    fn definitions() {
        assert_parity("DEF REAL R_A, R_B=2, ARR[2], MAT[1,1]=SET(1,,3)\nDEF INT I_C=5\nDEF BOOL FLAG=TRUE\n");
        // REP is a SET of its one or two values.
        assert_parity("DEF REAL QQ[2]=REP(7,2)\nDEF REAL PP[3]=REP(4)\n");
        assert_parity("DEF REAL CUBE[1,1,1]\nDEF STRING[8] TXT, TXT2=\"ab\"\n");
        assert_parity("N10 DEF REAL R_A ;declared\n");
    }

    #[test]
    fn definition_errors() {
        assert_error_parity("DEF REAL X\n");
        assert_error_parity("DEF REAL X1\n");
        assert_error_parity("DEF REAL CR\n");
        assert_error_parity("DEF REAL I=5\n");
        assert_error_parity("DEF INT X[2]=5\n");
        assert_error_parity("DEF STRING[4] NAME=5\n");
        assert_error_parity("DEF REAL NAME=\"a\"\n");
        assert_error_parity("DEF STRING[4] NAME\nDEF REAL NAME=1\n");
        assert_error_parity("DEF REAL ARR[1]=SET(1,2,3)\n");
        assert_error_parity("DEF REAL ARR[1,1,1,1]\n");
    }
}
