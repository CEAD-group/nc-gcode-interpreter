//! Expression and string evaluation for the statement-AST executor, ported
//! from evaluate_expression, interpret_primary, evaluate_arithmetic_function,
//! the string functions, interpret_indices and interpret_variable_array in
//! interpret_rules.rs. `line_no` is the block's line, for error context. An
//! `Arg::Omitted` is skipped: the tree walker never sees empty positions.

#![allow(unused_variables)] // phase 2 in progress: some bodies are still todo!()

use crate::ast::{self, BinaryOp, Expr, UnaryOp};
use crate::errors::ParsingError;
use crate::interpret_rules::reals_equal;
use crate::state::{emit_warning, State};

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

/// A numeric expression (`evaluate_expression`).
pub(crate) fn eval_expr(expr: &ast::Expr, state: &mut State, line_no: usize) -> Result<f64, ParsingError> {
    match expr {
        Expr::Number(value) => Ok(*value),
        Expr::Var(name) => read_variable(name, state, line_no),
        Expr::Element { name, indices } => todo!("phase 2: array element reads"),
        Expr::Unary(op, operand) => {
            let value = eval_expr(operand, state, line_no)?;
            Ok(match op {
                UnaryOp::Neg => -value,
                UnaryOp::Not => {
                    if value == 0.0 {
                        1.0
                    } else {
                        0.0
                    }
                }
            })
        }
        Expr::Binary(op, lhs, rhs) => eval_binary(*op, lhs, rhs, state, line_no),
        Expr::Call { name, args } => todo!("phase 2: functions"),
        Expr::Paren(inner) => eval_expr(inner, state, line_no),
    }
}

/// `interpret_primary` on a `variable`: TRUE and FALSE are symbol-table entries.
fn read_variable(name: &str, state: &mut State, line_no: usize) -> Result<f64, ParsingError> {
    if let Some(value) = state.symbol_table.get(name) {
        return Ok(*value);
    }
    if state.string_table.contains_key(name) {
        return Err(context_error(
            state,
            line_no,
            "expression evaluation",
            format!("'{name}' is a STRING variable; string values cannot be used in numeric expressions (string expressions are not supported)"),
        ));
    }
    if state.allow_undefined_variables {
        emit_warning(format_args!(
            "Warning: Variable '{}' is undefined, initializing to 0.0",
            name
        ));
        state.symbol_table.insert(name.to_string(), 0.0);
        return Ok(0.0);
    }
    Err(ParsingError::UndefinedVariable {
        line_no,
        preview: preview(state, line_no),
        name: name.to_string(),
    })
}

/// Both operands are evaluated, left first, before the operator applies (AND
/// and OR do not short-circuit in the tree walker either).
fn eval_binary(op: BinaryOp, lhs: &Expr, rhs: &Expr, state: &mut State, line_no: usize) -> Result<f64, ParsingError> {
    let a = eval_expr(lhs, state, line_no)?;
    let b = eval_expr(rhs, state, line_no)?;
    let truth = |value: bool| value as u8 as f64;
    Ok(match op {
        BinaryOp::Add => a + b,
        BinaryOp::Sub => a - b,
        BinaryOp::Mul => a * b,
        BinaryOp::Div => a / b,
        BinaryOp::IntDiv => {
            if b == 0.0 {
                return Err(ParsingError::ParsingContext {
                    line_no,
                    column: None,
                    preview: preview(state, line_no),
                    context: "integer division".to_string(),
                    message: "Integer division (DIV) by zero".to_string(),
                });
            }
            // DIV truncates the REAL quotient, not the operands (7 DIV 4.1 = 1).
            (a / b).trunc()
        }
        BinaryOp::Mod => a % b,
        BinaryOp::BitAnd => (bit_operand(a, op, state, line_no)? & bit_operand(b, op, state, line_no)?) as f64,
        BinaryOp::BitXor => (bit_operand(a, op, state, line_no)? ^ bit_operand(b, op, state, line_no)?) as f64,
        BinaryOp::BitOr => (bit_operand(a, op, state, line_no)? | bit_operand(b, op, state, line_no)?) as f64,
        BinaryOp::And => truth(a != 0.0 && b != 0.0),
        BinaryOp::Xor => truth((a != 0.0) != (b != 0.0)),
        BinaryOp::Or => truth(a != 0.0 || b != 0.0),
        BinaryOp::Lt => truth(a < b && !reals_equal(a, b)),
        BinaryOp::Gt => truth(a > b && !reals_equal(a, b)),
        BinaryOp::Le => truth(a < b || reals_equal(a, b)),
        BinaryOp::Ge => truth(a > b || reals_equal(a, b)),
        BinaryOp::Eq => truth(reals_equal(a, b)),
        BinaryOp::Ne => truth(!reals_equal(a, b)),
    })
}

/// A fractional, non-finite or out-of-range REAL has no bit pattern; `as i64`
/// would truncate or saturate it silently.
fn bit_operand(value: f64, op: BinaryOp, state: &State, line_no: usize) -> Result<i64, ParsingError> {
    if !value.is_finite() || value.fract() != 0.0 || value.abs() >= 9_007_199_254_740_992.0 {
        return Err(context_error(
            state,
            line_no,
            "bit-by-bit operator",
            format!(
                "{} requires integer operands (manual 4.1.3.2: types CHAR/INT), got {value}",
                op.symbol()
            ),
        ));
    }
    Ok(value as i64)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::{Assignment, Body, Rhs, Statement};

    const AXES: [&str; 3] = ["X", "Y", "Z"];

    /// A state whose input is `program`, so previews resolve like the walker's.
    fn state_with(program: &str, allow_undefined: bool) -> State {
        let mut state = State::new(AXES.map(String::from).to_vec(), 1000, None, allow_undefined);
        state.set_input(program);
        state
    }

    /// The right-hand side of the assignment on `line` (1-based) of `program`.
    fn rhs_of(program: &str, line: usize) -> Expr {
        let text = program.lines().nth(line - 1).expect("line exists");
        let block = crate::lower::parse_line(text, line, &state_with(program, false))
            .unwrap_or_else(|e| panic!("{text:?} does not lower: {e}"));
        match block.body {
            Body::Statements(statements) => match statements.as_slice() {
                [Statement::Assign(Assignment::Var {
                    rhs: Rhs::Expr(expr), ..
                })] => expr.clone(),
                other => panic!("{text:?}: {other:?}"),
            },
            other => panic!("{text:?}: {other:?}"),
        }
    }

    /// `R1=<expr>` evaluated on a fresh state.
    fn eval(expr: &str) -> Result<f64, ParsingError> {
        let program = format!("R1={expr}\n");
        let mut state = state_with(&program, false);
        eval_expr(&rhs_of(&program, 1), &mut state, 1)
    }

    fn value(expr: &str) -> f64 {
        eval(expr).unwrap_or_else(|e| panic!("{expr}: {e}"))
    }

    /// The default path's value of R1 after `R1=<expr>`, or its error.
    fn walker(expr: &str) -> Result<f64, String> {
        let program = format!("R1={expr}\n");
        crate::interpreter::nc_to_table(&program, None, None, None, 1000, false, None, false, None)
            .map(|(_, state)| state.symbol_table["R1"])
            .map_err(|e| format!("{e:?}"))
    }

    /// Value and error parity with the default path, compared bit for bit.
    fn assert_parity(expr: &str) {
        let ours = eval(expr).map_err(|e| format!("{e:?}"));
        let theirs = walker(expr);
        match (&ours, &theirs) {
            (Ok(a), Ok(b)) if a.to_bits() == b.to_bits() || (a.is_nan() && b.is_nan()) => {}
            (Err(a), Err(b)) if a == b => {}
            _ => panic!("{expr}: ast {ours:?}, walker {theirs:?}"),
        }
    }

    #[test]
    fn numbers_and_unary() {
        assert_eq!(value("5"), 5.0);
        assert_eq!(value("-5"), -5.0);
        assert_eq!(value("--5"), 5.0);
        assert_eq!(value("NOT 0"), 1.0);
        assert_eq!(value("NOT 3"), 0.0);
        // NOT -X is NOT(-X): the negation applies first.
        assert_eq!(value("NOT -0"), 1.0);
        assert_eq!(value("TRUE"), 1.0);
        assert_eq!(value("FALSE"), 0.0);
    }

    #[test]
    fn precedence_levels_match_the_walker() {
        for expr in [
            "1+2*3",
            "(1+2)*3",
            "10-4-3",
            "2*3/4",
            "8/2/2",
            "-2*3",
            "1+2<4",
            "3<2+2",
            "1 OR 0 AND 0",
            "1 XOR 1 OR 1",
            "0 AND 1 XOR 1",
            "1 B_OR 2 AND 0",
            "6 B_AND 3 B_XOR 1",
            "6 B_XOR 3 B_OR 8",
            "1 B_AND 3+4",
            "7 DIV 2*2",
            "7 MOD 4+1",
            "1==1==1",
            "2<3<1",
            "NOT 1+1",
            "NOT (1+1)",
            "-(2+3)*4",
            "1<>2 AND 2>=2 OR 3<=1",
            "0.1+0.2==0.3",
            "0.3<0.1+0.2",
            "0.3>0.1+0.2",
        ] {
            assert_parity(expr);
        }
    }

    #[test]
    fn div_mod_and_bit_operators() {
        assert_eq!(value("7 DIV 4.1"), 1.0);
        assert_eq!(value("-7 DIV 2"), -3.0);
        assert_eq!(value("7 MOD 3"), 1.0);
        assert_eq!(value("-7 MOD 3"), -1.0);
        assert_eq!(value("12 B_AND 10"), 8.0);
        assert_eq!(value("12 B_OR 10"), 14.0);
        assert_eq!(value("12 B_XOR 10"), 6.0);
        for expr in [
            "7 DIV 4.1",
            "7 DIV 0",
            "7 MOD 0",
            "1.5 B_AND 1",
            "1 B_OR 2.5",
            "1 B_XOR (1/0)",
            "9007199254740992 B_AND 1",
            "12 b_and 10",
        ] {
            assert_parity(expr);
        }
    }

    #[test]
    fn undefined_variables() {
        let err = eval("UNDEF+1").unwrap_err();
        assert!(matches!(&err, ParsingError::UndefinedVariable { name, line_no: 1, .. } if name == "UNDEF"));
        assert_parity("UNDEF+1");
        assert_parity("undef");

        let program = "R1=UNDEF+1\n";
        let mut state = state_with(program, true);
        assert_eq!(eval_expr(&rhs_of(program, 1), &mut state, 1).unwrap(), 1.0);
        assert_eq!(state.symbol_table["UNDEF"], 0.0);
    }

    #[test]
    fn string_variable_in_numeric_expression_errors() {
        let program = "R1=S+1\n";
        let mut state = state_with(program, false);
        state.string_table.insert("S".to_string(), "abc".to_string());
        let err = eval_expr(&rhs_of(program, 1), &mut state, 1).unwrap_err();
        assert_eq!(
            err.to_string(),
            ParsingError::with_context(
                1,
                "R1=S+1",
                "expression evaluation",
                "'S' is a STRING variable; string values cannot be used in numeric expressions (string expressions are not supported)"
            )
            .to_string()
        );
    }
}
