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

#![allow(unused_variables)] // phase 1 skeleton: the bodies are todo!()

use crate::ast;
use crate::errors::ParsingError;
use crate::interpret_rules::JumpRequest;
use crate::output::OutputRows;
use crate::state::State;

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
pub(crate) fn exec_block(block: &ast::Block, state: &mut State, output: &mut OutputRows) -> Result<Flow, ParsingError> {
    todo!("phase 2: port interpret_block and its callees")
}

/// Start one pass of a loop body, or the ELSE branch: the comment of the head
/// or ELSE line is the body's first block, so this starts a row on that line
/// and inserts the comment (an empty row when there is none).
pub(crate) fn begin_body(head: &ast::Block, output: &mut OutputRows) -> Result<(), ParsingError> {
    todo!("phase 2")
}

/// Evaluate the condition of a WhileHead (each re-test after ENDWHILE) or an
/// UntilTail (after the driver's loop-limit check), without starting a row.
pub(crate) fn eval_condition(block: &ast::Block, state: &mut State) -> Result<bool, ParsingError> {
    todo!("phase 2: port evaluate_condition")
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
pub(crate) fn leave_structure(closer: &ast::Block, output: &mut OutputRows) {
    todo!("phase 2")
}
