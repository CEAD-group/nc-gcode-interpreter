//! Expression and string evaluation for the statement-AST executor, ported
//! from evaluate_expression, interpret_primary, evaluate_arithmetic_function,
//! the string functions, interpret_indices and interpret_variable_array in
//! interpret_rules.rs. `line_no` is the block's line, for error context. An
//! `Arg::Omitted` is skipped: the tree walker never sees empty positions.

#![allow(unused_variables)] // phase 1 skeleton: the bodies are todo!()

use crate::ast;
use crate::errors::ParsingError;
use crate::state::State;

/// A numeric expression (`evaluate_expression`).
pub(crate) fn eval_expr(expr: &ast::Expr, state: &mut State, line_no: usize) -> Result<f64, ParsingError> {
    todo!("phase 2")
}

/// A string expression: a literal or a `<<` chain (`evaluate_string`).
pub(crate) fn eval_string(expr: &ast::StringExpr, state: &mut State, line_no: usize) -> Result<String, ParsingError> {
    todo!("phase 2")
}

/// Whether a plain expression yields a string at run time: a STRING variable
/// read, SPRINT/SUBSTR, or such a value in parentheses (`expression_is_string`).
pub(crate) fn expr_is_string(expr: &ast::Expr, state: &State) -> bool {
    todo!("phase 2")
}

/// A plain expression as text: the string value when `expr_is_string`, else
/// the number in `<<` notation (`evaluate_string` on an expression).
pub(crate) fn eval_expr_string(expr: &ast::Expr, state: &mut State, line_no: usize) -> Result<String, ParsingError> {
    todo!("phase 2")
}

/// Array indices, an index that names an axis mapped through the axis index
/// map (`interpret_indices`).
pub(crate) fn eval_indices(
    indices: &[ast::Index],
    state: &mut State,
    line_no: usize,
) -> Result<Vec<f64>, ParsingError> {
    todo!("phase 2")
}

/// Every element name up to the given indices, in the tree walker's order;
/// the last one is the element itself (`interpret_variable_array`).
pub(crate) fn element_keys(
    name: &str,
    indices: &[ast::Index],
    state: &mut State,
    line_no: usize,
) -> Result<Vec<String>, ParsingError> {
    todo!("phase 2")
}
