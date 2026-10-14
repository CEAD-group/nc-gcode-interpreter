//! Streaming driver of the statement-AST path (NC_EXEC=ast): interprets any
//! program line by line through `ast::Block`s, without a whole-file tree.
//!
//! Per line: `line_driver::decode_line` then `decode_ast::decode_ast` for a
//! trivial line, `lower::parse_line` for a grammar-bound one, nothing for a
//! blank one; then `exec::exec_block` and the `exec::Flow` it returns. Nesting
//! comes only from `structure_scan::scan_structure_table`, which also raises
//! the structure errors of check_structures. Jumps and loop iterations go to
//! line indices and re-decode the lines they revisit; nothing is kept per line.
//!
//! The driver owns the control stack and keeps the tree walker's scoping: each
//! IF branch and each loop body is a jump scope. A head line's label belongs to
//! the enclosing scope, an ELSE or closer line's label to the body before it.
//! A jump resolves in the innermost scope first (the `resolve_jump` directions,
//! relative to the current line), then in each enclosing scope relative to the
//! structure's head line, so it can leave a body but never enter one. Entering
//! a scope (every iteration) extends `state.seen_jump_targets` and pushes its
//! labels on `state.jump_scopes`; backward jumps count per scope entry against
//! `iteration_limit`. Loop limits are interpret_rules' quirks included: WHILE
//! fails once its count reaches the limit even if the condition then fails;
//! LOOP and REPEAT count after each pass, REPEAT before its UNTIL test; FOR
//! has none. A jump unresolved at top level is `into_not_found_error`.

#![allow(unused_variables)] // phase 1 skeleton: the body is todo!()

use crate::errors::ParsingError;
use crate::output::OutputRows;
use crate::state::State;

/// Interpret `input` into `output` (the NC_EXEC=ast counterpart of
/// `interpret_file`): sets the input on `state`, scans the structure, and runs
/// every line.
pub(crate) fn interpret_ast(input: &str, state: &mut State, output: &mut OutputRows) -> Result<(), ParsingError> {
    todo!("phase 2: the streaming driver")
}
