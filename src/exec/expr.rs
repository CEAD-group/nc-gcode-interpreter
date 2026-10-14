//! Expression and string evaluation for the statement-AST executor, ported
//! from evaluate_expression, interpret_primary, evaluate_arithmetic_function,
//! the string functions, interpret_indices and interpret_variable_array in
//! interpret_rules.rs. `line_no` is the block's line, for error context. An
//! `Arg::Omitted` is skipped: the tree walker never sees empty positions.

use crate::ast::{self, BinaryOp, Expr, UnaryOp};
use crate::errors::ParsingError;
use crate::interpret_rules::{format_nc_concat_number, parse_nc_number, reals_equal};
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
        Expr::Element { name, indices } => read_element(name, indices, state, line_no),
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
        Expr::Call { name, args } => eval_function(name, args, state, line_no),
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

/// `interpret_primary` on a `variable_array`; unlike a plain variable, an
/// undefined element is `UnknownVariable` and a STRING name is not checked.
fn read_element(name: &str, indices: &[ast::Index], state: &mut State, line_no: usize) -> Result<f64, ParsingError> {
    if let Some(value) = read_actual_position(name, indices, state, line_no)? {
        return Ok(value);
    }
    let key = element_key(name, indices, state, line_no)?;
    if let Some(value) = state.symbol_table.get(&key) {
        return Ok(*value);
    }
    if state.allow_undefined_variables {
        emit_warning(format_args!(
            "Warning: Variable array element '{}' is undefined, initializing to 0.0",
            key
        ));
        state.symbol_table.insert(key, 0.0);
        return Ok(0.0);
    }
    Err(ParsingError::UnknownVariable {
        line_no,
        preview: preview(state, line_no),
        variable: key,
    })
}

/// `read_actual_position_sysvar`: `$AA_IW[<axis>]` is the local coordinate,
/// `$AA_IM[<axis>]` the machine one. `None` sends any other name or index
/// shape to the generic element read.
fn read_actual_position(
    name: &str,
    indices: &[ast::Index],
    state: &mut State,
    line_no: usize,
) -> Result<Option<f64>, ParsingError> {
    let work = match name {
        "$AA_IW" => true,
        "$AA_IM" => false,
        _ => return Ok(None),
    };
    let [index] = indices else {
        return Ok(None);
    };
    let axis = index.text.trim().to_uppercase();
    if !state.is_axis(&axis) {
        return Ok(None);
    }
    let value = if work {
        state.get_axis_local(&axis)
    } else {
        state.get_axis_machine(&axis)
    };
    if value.is_some() {
        return Ok(value);
    }
    // No interpreted position yet; a real control always has an actual value.
    if state.allow_undefined_variables {
        emit_warning(format_args!(
            "Warning [line {}]: {}[{}] read before axis {} was positioned; returning 0.0",
            line_no, name, axis, axis
        ));
        return Ok(Some(0.0));
    }
    Err(context_error(
        state,
        line_no,
        "actual-position system variable",
        format!(
            "{name}[{axis}] is read before axis {axis} has a position; set a start position (e.g. via an initial-state file) or pass allow_undefined_variables"
        ),
    ))
}

/// The last name `interpret_variable_array` builds, without building the
/// others (a read of `A[100,100]` would build 10,201).
fn element_key(name: &str, indices: &[ast::Index], state: &mut State, line_no: usize) -> Result<String, ParsingError> {
    match eval_indices(indices, state, line_no)?.as_slice() {
        [i] => Ok(format!("{}[{}]", name, *i as i32)),
        [i, j] => Ok(format!("{}[{},{}]", name, *i as i32, *j as i32)),
        [i, j, k] => Ok(format!("{}[{},{},{}]", name, *i as i32, *j as i32, *k as i32)),
        _ => Err(too_many_indices()),
    }
}

fn too_many_indices() -> ParsingError {
    ParsingError::ParseError {
        message: "Invalid number of indices for variable array".to_string(),
    }
}

/// A function argument the tree walker sees: `function_arguments` yields no
/// pair for an omitted position.
#[derive(Clone, Copy)]
enum ArgRef<'a> {
    Num(&'a Expr),
    /// A quoted string or `<<` chain, with its source span.
    Str(&'a ast::StringExpr, &'a str),
}

fn present_args(args: &[ast::Arg]) -> impl Iterator<Item = ArgRef<'_>> {
    args.iter().filter_map(|arg| match arg {
        ast::Arg::Expr(expr) => Some(ArgRef::Num(expr)),
        ast::Arg::Str { value, text } => Some(ArgRef::Str(value, text)),
        ast::Arg::Omitted => None,
    })
}

/// `evaluate_string` on a raw function argument.
fn arg_text(arg: ArgRef, state: &mut State, line_no: usize) -> Result<String, ParsingError> {
    match arg {
        ArgRef::Num(expr) => eval_expr_string(expr, state, line_no),
        ArgRef::Str(value, _) => eval_string(value, state, line_no),
    }
}

/// `arg_is_string`.
fn arg_is_string(arg: ArgRef, state: &State) -> bool {
    match arg {
        ArgRef::Num(expr) => expr_is_string(expr, state),
        ArgRef::Str(..) => true,
    }
}

/// `eval_number_arg`: a quoted-string argument is a type error, not a 0.
fn eval_number_arg(arg: ArgRef, state: &mut State, line_no: usize) -> Result<f64, ParsingError> {
    match arg {
        ArgRef::Num(expr) => eval_expr(expr, state, line_no),
        ArgRef::Str(_, text) => Err(context_error(
            state,
            line_no,
            "function argument",
            format!("expected a number but got the string {text}"),
        )),
    }
}

/// `eval_index_arg`: a 0-based, non-negative integer (SUBSTR).
fn eval_index_arg(arg: ArgRef, state: &mut State, line_no: usize) -> Result<usize, ParsingError> {
    let n = eval_number_arg(arg, state, line_no)?;
    if n < 0.0 || n.fract() != 0.0 {
        return Err(context_error(
            state,
            line_no,
            "string index",
            format!("expected a non-negative integer index, got {n}"),
        ));
    }
    Ok(n as usize)
}

/// The number-returning string functions (manual 4.1.4), which take their
/// string arguments as text.
#[derive(Clone, Copy)]
enum StringQuery {
    Index,
    RIndex,
    MIndex,
    Match,
    Number,
    StrLen,
    IsNumber,
}

impl StringQuery {
    fn from_upper(name: &str) -> Option<StringQuery> {
        Some(match name {
            "INDEX" => StringQuery::Index,
            "RINDEX" => StringQuery::RIndex,
            "MINDEX" => StringQuery::MIndex,
            "MATCH" => StringQuery::Match,
            "NUMBER" => StringQuery::Number,
            "STRLEN" => StringQuery::StrLen,
            "ISNUMBER" => StringQuery::IsNumber,
            _ => return None,
        })
    }
}

/// The string-returning functions.
#[derive(Clone, Copy)]
enum StringFn {
    Sprint,
    Substr,
}

impl StringFn {
    fn from_name(name: &str) -> Option<StringFn> {
        if name.eq_ignore_ascii_case("SPRINT") {
            Some(StringFn::Sprint)
        } else if name.eq_ignore_ascii_case("SUBSTR") {
            Some(StringFn::Substr)
        } else {
            None
        }
    }
}

/// `evaluate_arithmetic_function`: every argument is evaluated before the
/// arity check and before an unknown name is reported, as in the walker.
fn eval_function(name: &str, args: &[ast::Arg], state: &mut State, line_no: usize) -> Result<f64, ParsingError> {
    let upper = name.to_uppercase();
    if let Some(query) = StringQuery::from_upper(&upper) {
        let args: Vec<ArgRef> = present_args(args).collect();
        return eval_string_query(query, &upper, &args, state, line_no);
    }
    if StringFn::from_name(name).is_some() {
        return Err(context_error(
            state,
            line_no,
            "expression",
            format!("'{upper}' returns a string; it cannot be used where a number is required"),
        ));
    }
    let mut values = Vec::with_capacity(args.len());
    for arg in present_args(args) {
        values.push(eval_number_arg(arg, state, line_no)?);
    }
    let check_args = |expected: usize, state: &State| -> Result<(), ParsingError> {
        if values.len() == expected {
            return Ok(());
        }
        Err(ParsingError::InvalidFunctionArity {
            line_no,
            preview: preview(state, line_no),
            name: name.to_string(),
            expected,
            actual: values.len(),
        })
    };
    // Trigonometric functions work in degrees (manual 4.1.3.1).
    let unary = |f: fn(f64) -> f64, state: &State| check_args(1, state).map(|()| f(values[0]));
    match upper.as_str() {
        "SIN" => unary(|v| v.to_radians().sin(), state),
        "COS" => unary(|v| v.to_radians().cos(), state),
        "TAN" => unary(|v| v.to_radians().tan(), state),
        "ASIN" => unary(|v| v.asin().to_degrees(), state),
        "ACOS" => unary(|v| v.acos().to_degrees(), state),
        // The angular reference is the second value: ATAN2(30.5, 80.1) = 20.8455.
        "ATAN2" => check_args(2, state).map(|()| values[0].atan2(values[1]).to_degrees()),
        "SQRT" => unary(f64::sqrt, state),
        "ABS" => unary(f64::abs, state),
        "POT" => unary(|v| v.powi(2), state),
        "TRUNC" => unary(f64::trunc, state),
        "ROUND" => unary(f64::round, state),
        "ROUNDUP" => unary(f64::ceil, state),
        "MINVAL" => check_args(2, state).map(|()| values[0].min(values[1])),
        "MAXVAL" => check_args(2, state).map(|()| values[0].max(values[1])),
        // BOUND(<minimum>, <maximum>, <check value>) (manual 4.1.1.13).
        "BOUND" => {
            check_args(3, state)?;
            let (min, max, value) = (values[0], values[1], values[2]);
            if min > max {
                return Err(context_error(
                    state,
                    line_no,
                    "BOUND",
                    format!("BOUND minimum ({}) is greater than maximum ({})", min, max),
                ));
            }
            Ok(value.clamp(min, max))
        }
        "LN" => unary(f64::ln, state),
        "EXP" => unary(f64::exp, state),
        other => Err(context_error(
            state,
            line_no,
            "expression",
            format!("'{}' is not a known arithmetic function", other),
        )),
    }
}

/// `evaluate_string_query_function`: 0-based positions, -1 when not found.
fn eval_string_query(
    query: StringQuery,
    name: &str,
    args: &[ArgRef],
    state: &mut State,
    line_no: usize,
) -> Result<f64, ParsingError> {
    let expected = match query {
        StringQuery::Index | StringQuery::RIndex | StringQuery::MIndex | StringQuery::Match => 2,
        StringQuery::Number | StringQuery::StrLen | StringQuery::IsNumber => 1,
    };
    if args.len() != expected {
        return Err(ParsingError::InvalidFunctionArity {
            line_no,
            preview: preview(state, line_no),
            name: name.to_string(),
            expected,
            actual: args.len(),
        });
    }
    let found = |pos: Option<usize>| pos.map(|p| p as f64).unwrap_or(-1.0);
    match query {
        StringQuery::Index | StringQuery::RIndex => {
            let haystack = arg_text(args[0], state, line_no)?;
            let needle = arg_text(args[1], state, line_no)?;
            let Some(target) = needle.chars().next() else {
                return Err(context_error(
                    state,
                    line_no,
                    name,
                    format!("{name} needs a non-empty search character"),
                ));
            };
            let chars: Vec<char> = haystack.chars().collect();
            Ok(found(match query {
                StringQuery::Index => chars.iter().position(|&c| c == target),
                _ => chars.iter().rposition(|&c| c == target),
            }))
        }
        // The list is taken character by character, commas included (manual 4.1.4.6).
        StringQuery::MIndex => {
            let haystack = arg_text(args[0], state, line_no)?;
            let list: Vec<char> = arg_text(args[1], state, line_no)?.chars().collect();
            Ok(found(haystack.chars().position(|c| list.contains(&c))))
        }
        // An empty search string occurs at position 0, as with C's strstr.
        StringQuery::Match => {
            let haystack: Vec<char> = arg_text(args[0], state, line_no)?.chars().collect();
            let needle: Vec<char> = arg_text(args[1], state, line_no)?.chars().collect();
            Ok(found(if needle.is_empty() {
                Some(0)
            } else {
                haystack
                    .windows(needle.len())
                    .position(|window| window == needle.as_slice())
            }))
        }
        StringQuery::Number => {
            let s = arg_text(args[0], state, line_no)?;
            parse_nc_number(&s).ok_or_else(|| {
                context_error(
                    state,
                    line_no,
                    "NUMBER",
                    format!("NUMBER(\"{s}\"): the string is not a valid number"),
                )
            })
        }
        // The internal 0 character terminates a string (manual 4.1.4).
        StringQuery::StrLen => {
            let s = arg_text(args[0], state, line_no)?;
            Ok(s.chars().take_while(|&c| c != '\0').count() as f64)
        }
        StringQuery::IsNumber => {
            let s = arg_text(args[0], state, line_no)?;
            Ok(parse_nc_number(&s).is_some() as u8 as f64)
        }
    }
}

/// A string expression: a literal or a `<<` chain (`evaluate_string`).
pub(crate) fn eval_string(expr: &ast::StringExpr, state: &mut State, line_no: usize) -> Result<String, ParsingError> {
    match expr {
        ast::StringExpr::Literal(text) => Ok(text.clone()),
        ast::StringExpr::Concat(first, rest) => {
            let mut out = String::new();
            for operand in std::iter::once(first).chain(rest) {
                match operand {
                    ast::Operand::Literal(text) => out.push_str(text),
                    ast::Operand::Expr(expr) => out.push_str(&eval_expr_string(expr, state, line_no)?),
                }
            }
            Ok(out)
        }
    }
}

/// What a string-valued expression reads, under any parentheses.
enum StringSource<'e> {
    Var(&'e str),
    Call(StringFn, &'e [ast::Arg]),
}

/// AST names are uppercased, so `normalize_reserved_case` is the identity on
/// them and the string table is keyed by the name itself.
fn string_source<'e>(expr: &'e Expr, state: &State) -> Option<StringSource<'e>> {
    match expr {
        Expr::Var(name) => state.string_table.contains_key(name).then_some(StringSource::Var(name)),
        Expr::Call { name, args } => StringFn::from_name(name).map(|f| StringSource::Call(f, args)),
        Expr::Paren(inner) => string_source(inner, state),
        Expr::Number(_) | Expr::Element { .. } | Expr::Unary(..) | Expr::Binary(..) => None,
    }
}

/// Whether a plain expression yields a string at run time: a STRING variable
/// read, SPRINT/SUBSTR, or such a value in parentheses (`expression_is_string`).
pub(crate) fn expr_is_string(expr: &ast::Expr, state: &State) -> bool {
    string_source(expr, state).is_some()
}

/// A plain expression as text: the string value when `expr_is_string`, else
/// the number in `<<` notation (`evaluate_string` on an expression).
pub(crate) fn eval_expr_string(expr: &ast::Expr, state: &mut State, line_no: usize) -> Result<String, ParsingError> {
    match string_source(expr, state) {
        Some(StringSource::Var(name)) => Ok(state.string_table.get(name).cloned().unwrap_or_default()),
        Some(StringSource::Call(function, args)) => {
            let args: Vec<ArgRef> = present_args(args).collect();
            match function {
                StringFn::Sprint => eval_sprint(&args, state, line_no),
                StringFn::Substr => eval_substr(&args, state, line_no),
            }
        }
        // A number joined by << (manual 4.1.4.1).
        None => Ok(format_nc_concat_number(eval_expr(expr, state, line_no)?)),
    }
}

/// SUBSTR(<string>, <index>[, <length>]) (manual 4.1.4.7): a start past the
/// end is empty, an over-long length is clamped.
fn eval_substr(args: &[ArgRef], state: &mut State, line_no: usize) -> Result<String, ParsingError> {
    if args.len() != 2 && args.len() != 3 {
        return Err(ParsingError::InvalidFunctionArity {
            line_no,
            preview: preview(state, line_no),
            name: "SUBSTR".to_string(),
            expected: 2,
            actual: args.len(),
        });
    }
    let chars: Vec<char> = arg_text(args[0], state, line_no)?.chars().collect();
    let start = eval_index_arg(args[1], state, line_no)?.min(chars.len());
    let end = match args.get(2) {
        Some(&length) => start
            .saturating_add(eval_index_arg(length, state, line_no)?)
            .min(chars.len()),
        None => chars.len(),
    };
    Ok(chars[start..end].iter().collect())
}

fn sprint_err(state: &State, line_no: usize, message: String) -> ParsingError {
    context_error(state, line_no, "SPRINT", message)
}

/// SPRINT(<format>, <value>...) (manual 4.1.4.9): each `%` takes the next
/// value, `%%` is a literal percent, there are no flags.
fn eval_sprint(args: &[ArgRef], state: &mut State, line_no: usize) -> Result<String, ParsingError> {
    let Some((&format_arg, value_args)) = args.split_first() else {
        return Err(sprint_err(state, line_no, "SPRINT needs a format string".to_string()));
    };
    let format = arg_text(format_arg, state, line_no)?;
    let mut values = value_args.iter();
    let mut out = String::new();
    let mut chars = format.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '%' {
            out.push(c);
            continue;
        }
        if chars.peek() == Some(&'%') {
            chars.next();
            out.push('%');
            continue;
        }
        let mut spec = String::new();
        while let Some(&pc) = chars.peek() {
            if pc.is_ascii_digit() || pc == '.' {
                spec.push(pc);
                chars.next();
            } else {
                break;
            }
        }
        let Some(conversion) = chars.next() else {
            return Err(sprint_err(
                state,
                line_no,
                "dangling '%' at end of SPRINT format".to_string(),
            ));
        };
        let Some(&arg) = values.next() else {
            return Err(sprint_err(
                state,
                line_no,
                "SPRINT format has more conversions than arguments".to_string(),
            ));
        };
        format_sprint_spec(&mut out, &spec, conversion, arg, state, line_no)?;
    }
    Ok(out)
}

/// One SPRINT conversion; `spec` is the "[width][.precision]" text.
fn format_sprint_spec(
    out: &mut String,
    spec: &str,
    conversion: char,
    arg: ArgRef,
    state: &mut State,
    line_no: usize,
) -> Result<(), ParsingError> {
    let (width_str, prec_str) = match spec.split_once('.') {
        Some((w, p)) => (w, Some(p)),
        None => (spec, None),
    };
    let width: usize = if width_str.is_empty() {
        0
    } else {
        width_str
            .parse()
            .map_err(|_| sprint_err(state, line_no, format!("invalid SPRINT field width '{width_str}'")))?
    };
    let prec: Option<usize> = match prec_str {
        Some(p) => Some(
            p.parse()
                .map_err(|_| sprint_err(state, line_no, format!("invalid SPRINT precision '{p}'")))?,
        ),
        None => None,
    };
    let body = match conversion.to_ascii_uppercase() {
        'D' => format!("{}", eval_number_arg(arg, state, line_no)?.round() as i64),
        'F' => format!("{:.*}", prec.unwrap_or(6), eval_number_arg(arg, state, line_no)?),
        'X' => format!("{:X}", eval_number_arg(arg, state, line_no)?.round() as i64),
        'S' => {
            let s = arg_text(arg, state, line_no)?;
            match prec {
                Some(p) => s.chars().take(p).collect(),
                None => s,
            }
        }
        'B' => {
            let truthy = if arg_is_string(arg, state) {
                !arg_text(arg, state, line_no)?.is_empty()
            } else {
                eval_number_arg(arg, state, line_no)? != 0.0
            };
            (if truthy { "TRUE" } else { "FALSE" }).to_string()
        }
        'C' => {
            if arg_is_string(arg, state) {
                arg_text(arg, state, line_no)?
                    .chars()
                    .next()
                    .map(String::from)
                    .unwrap_or_default()
            } else {
                let code = eval_number_arg(arg, state, line_no)?.round();
                if !(0.0..=255.0).contains(&code) {
                    return Err(sprint_err(
                        state,
                        line_no,
                        format!("%c character code {code} is outside 0..255"),
                    ));
                }
                ((code as u8) as char).to_string()
            }
        }
        other => {
            return Err(sprint_err(
                state,
                line_no,
                format!("SPRINT conversion '%{other}' is not supported (supported: %d %f %s %x %b %c)"),
            ))
        }
    };
    // Missing positions are filled with spaces, right-justified (manual 4.1.4.9).
    let len = body.chars().count();
    if len < width {
        out.extend(std::iter::repeat_n(' ', width - len));
    }
    out.push_str(&body);
    Ok(())
}

/// Array indices, an index that names an axis mapped through the axis index
/// map (`interpret_indices`).
pub(crate) fn eval_indices(
    indices: &[ast::Index],
    state: &mut State,
    line_no: usize,
) -> Result<Vec<f64>, ParsingError> {
    let mut values = Vec::with_capacity(indices.len());
    for index in indices {
        // The walker matches the index text, so `(X)` is not the axis X.
        if state.is_axis(&index.text) {
            let preview = preview(state, line_no);
            values.push(state.get_axis_index(&index.text, line_no, &preview)? as f64);
            continue;
        }
        let value = eval_expr(&index.expr, state, line_no)?;
        if value < 0.0 || value.fract() != 0.0 {
            return Err(ParsingError::InvalidAxisIndex {
                line_no,
                preview: preview(state, line_no),
                axis: index.text.clone(),
                index: value as usize,
            });
        }
        values.push(value);
    }
    Ok(values)
}

/// Every element name up to the given indices, in the tree walker's order;
/// the last one is the element itself (`interpret_variable_array`).
pub(crate) fn element_keys(
    name: &str,
    indices: &[ast::Index],
    state: &mut State,
    line_no: usize,
) -> Result<Vec<String>, ParsingError> {
    let mut keys = Vec::new();
    match eval_indices(indices, state, line_no)?.as_slice() {
        [i] => {
            for a in 0..=(*i as i32) {
                keys.push(format!("{}[{}]", name, a));
            }
        }
        [i, j] => {
            for a in 0..=(*i as i32) {
                for b in 0..=(*j as i32) {
                    keys.push(format!("{}[{},{}]", name, a, b));
                }
            }
        }
        [i, j, k] => {
            for a in 0..=(*i as i32) {
                for b in 0..=(*j as i32) {
                    for c in 0..=(*k as i32) {
                        keys.push(format!("{}[{},{},{}]", name, a, b, c));
                    }
                }
            }
        }
        _ => return Err(too_many_indices()),
    }
    Ok(keys)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::{Assignment, Body, Index, Rhs, Statement};
    use std::collections::HashMap;

    #[derive(Debug, PartialEq)]
    enum Outcome {
        /// f64 bits, NaN canonicalized.
        Num(u64),
        Str(String),
        /// The error's Debug form: kind, line, preview and message.
        Err(String),
    }

    fn num(value: f64) -> Outcome {
        Outcome::Num(if value.is_nan() { f64::NAN } else { value }.to_bits())
    }

    #[derive(Default, Clone)]
    struct Opts {
        allow_undefined: bool,
        axis_index_map: Option<HashMap<String, usize>>,
    }

    fn reference(program: &str, opts: &Opts) -> Result<State, ParsingError> {
        crate::interpreter::nc_to_table(
            program,
            None,
            None,
            None,
            1000,
            false,
            opts.axis_index_map.clone(),
            opts.allow_undefined,
            None,
        )
        .map(|(_, state)| state)
    }

    /// The last line of `program` is `NAME=<rhs>`. The lines before it run on
    /// the default path; the right-hand side is evaluated here.
    fn ours_with(program: &str, opts: &Opts) -> (Outcome, State) {
        let lines: Vec<&str> = program.lines().collect();
        let line_no = lines.len();
        let setup: String = lines[..line_no - 1].iter().map(|line| format!("{line}\n")).collect();
        let mut state = reference(&setup, opts).unwrap_or_else(|e| panic!("setup of {program:?} fails: {e}"));
        state.set_input(program);
        let text = lines[line_no - 1];
        let block = crate::lower::parse_line(text, line_no, &state).unwrap_or_else(|e| panic!("{text:?}: {e}"));
        let Body::Statements(statements) = block.body else {
            panic!("{text:?} is not a statement block");
        };
        let [Statement::Assign(Assignment::Var { rhs, .. })] = statements.as_slice() else {
            panic!("{text:?} is not one assignment");
        };
        let result = match rhs {
            Rhs::Expr(expr) if expr_is_string(expr, &state) => {
                eval_expr_string(expr, &mut state, line_no).map(Outcome::Str)
            }
            Rhs::Expr(expr) => eval_expr(expr, &mut state, line_no).map(num),
            Rhs::Str(value) => eval_string(value, &mut state, line_no).map(Outcome::Str),
            Rhs::Increment(_) => panic!("{text:?}: IC() is not an expression"),
        };
        (result.unwrap_or_else(|e| Outcome::Err(format!("{e:?}"))), state)
    }

    fn ours(program: &str, opts: &Opts) -> Outcome {
        ours_with(program, opts).0
    }

    /// The default path on `program` plus `suffix`: the value the last line of
    /// `program` assigned, or the error.
    fn theirs(program: &str, suffix: &str, opts: &Opts) -> Outcome {
        let last = program.lines().last().expect("a line");
        let name = last.split('=').next().expect("an assignment").trim().to_uppercase();
        match reference(&format!("{program}\n{suffix}"), opts) {
            Ok(state) => match state.string_table.get(&name) {
                Some(text) => Outcome::Str(text.clone()),
                None => num(state.symbol_table[&name]),
            },
            Err(e) => Outcome::Err(format!("{e:?}")),
        }
    }

    #[track_caller]
    fn parity_with(program: &str, opts: &Opts) -> Outcome {
        let ast = ours(program, opts);
        let default_path = theirs(program, "", opts);
        assert_eq!(
            ast, default_path,
            "{program:?}: ast (left) and default path (right) differ"
        );
        // A block structure sends the whole program through the tree walker.
        let walker = theirs(program, "IF 1\nENDIF", opts);
        assert_eq!(ast, walker, "{program:?}: ast (left) and tree walker (right) differ");
        ast
    }

    #[track_caller]
    fn parity(program: &str) -> Outcome {
        parity_with(program, &Opts::default())
    }

    /// `R1=<expr>` on a fresh state.
    #[track_caller]
    fn value(expr: &str) -> f64 {
        match parity(&format!("R1={expr}")) {
            Outcome::Num(bits) => f64::from_bits(bits),
            other => panic!("{expr}: {other:?}"),
        }
    }

    #[track_caller]
    fn text(program: &str) -> String {
        match parity(program) {
            Outcome::Str(text) => text,
            other => panic!("{program:?}: {other:?}"),
        }
    }

    /// The parity-checked error message of the last line.
    #[track_caller]
    fn error(program: &str) -> String {
        match parity(program) {
            Outcome::Err(message) => message,
            other => panic!("{program:?} does not fail: {other:?}"),
        }
    }

    #[test]
    fn numbers_unary_and_symbols() {
        assert_eq!(value("5"), 5.0);
        assert_eq!(value("-5"), -5.0);
        assert_eq!(value("--5"), 5.0);
        assert_eq!(value("NOT 0"), 1.0);
        assert_eq!(value("NOT 3"), 0.0);
        // NOT -X is NOT(-X): the negation applies first.
        assert_eq!(value("NOT -0"), 1.0);
        assert_eq!(value("TRUE"), 1.0);
        assert_eq!(value("false"), 0.0);
        assert_eq!(value("NOT TRUE"), 0.0);
        assert_eq!(value("2*-3"), -6.0);
        assert_eq!(value("-SIN(30)*2"), -(30f64.to_radians().sin()) * 2.0);
    }

    #[test]
    fn precedence_levels_match_the_walker() {
        for (expr, expected) in [
            ("1+2*3", 7.0),
            ("(1+2)*3", 9.0),
            ("10-4-3", 3.0),
            ("2*3/4", 1.5),
            ("8/2/2", 2.0),
            ("-2*3", -6.0),
            ("1+2<4", 1.0),
            ("3<2+2", 1.0),
            ("1 OR 0 AND 0", 1.0),
            ("1 XOR 1 OR 1", 1.0),
            ("0 AND 1 XOR 1", 1.0),
            ("1 B_OR 2 AND 0", 0.0),
            ("6 B_AND 3 B_XOR 1", 3.0),
            ("6 B_XOR 3 B_OR 8", 13.0),
            ("1 B_AND 3+4", 1.0),
            ("7 DIV 2*2", 6.0),
            ("7 MOD 4+1", 4.0),
            ("1==1==1", 1.0),
            ("2<3<1", 0.0),
            ("NOT 1+1", 1.0),
            ("NOT (1+1)", 0.0),
            ("-(2+3)*4", -20.0),
            ("1<>2 AND 2>=2 OR 3<=1", 1.0),
        ] {
            assert_eq!(value(expr), expected, "{expr}");
        }
    }

    #[test]
    fn comparisons_use_relative_equality() {
        let program = "R2=0.1\nR3=0.2\nR4=0.3\nR1=R2+R3";
        assert_eq!(parity(program), num(0.1 + 0.2));
        for (op, expected) in [
            ("==", 1.0),
            ("<>", 0.0),
            ("<", 0.0),
            (">", 0.0),
            ("<=", 1.0),
            (">=", 1.0),
        ] {
            let program = format!("R2=0.1\nR3=0.2\nR4=0.3\nR1=R2+R3{op}R4");
            assert_eq!(parity(&program), num(expected), "{op}");
        }
    }

    #[test]
    fn div_mod_and_bit_operators() {
        assert_eq!(value("7 DIV 4.1"), 1.0);
        assert_eq!(value("-7 DIV 2"), -3.0);
        assert_eq!(value("7 MOD 3"), 1.0);
        assert_eq!(value("-7 MOD 3"), -1.0);
        assert_eq!(value("7.5 MOD 2"), 1.5);
        assert_eq!(value("12 B_AND 10"), 8.0);
        assert_eq!(value("12 B_OR 10"), 14.0);
        assert_eq!(value("12 B_XOR 10"), 6.0);
        assert_eq!(value("12 b_and 10"), 8.0);
        assert!(error("R1=7 DIV 0").contains("Integer division (DIV) by zero"));
        assert!(error("R2=0\nR1=7 DIV R2").contains("line_no: 2"));
        assert_eq!(parity("R1=7 MOD 0"), num(f64::NAN));
        for program in [
            "R1=1.5 B_AND 1",
            "R1=1 B_OR 2.5",
            "R1=1 B_XOR (1/0)",
            "R1=9007199254740992 B_AND 1",
            "R1=-1.5 B_AND 2.5",
        ] {
            assert!(error(program).contains("requires integer operands"), "{program}");
        }
    }

    /// Accepted deviation: the AST keeps the operator, not its spelling, so
    /// the message quotes it uppercase where the walker quotes it as written.
    #[test]
    fn lowercase_bit_operator_message_is_uppercase() {
        let program = "R1=1.5 b_and 1";
        let Outcome::Err(ast) = ours(program, &Opts::default()) else {
            panic!("{program} does not fail");
        };
        let Outcome::Err(walker) = theirs(program, "", &Opts::default()) else {
            panic!("{program} does not fail on the default path");
        };
        assert!(ast.contains("B_AND requires integer operands"), "{ast}");
        assert!(walker.contains("b_and requires integer operands"), "{walker}");
        assert_eq!(ast.replace("B_AND requires", "b_and requires"), walker);
    }

    #[test]
    fn undefined_variables_both_modes() {
        let message = error("R1=UNDEF+1");
        assert!(message.starts_with("UndefinedVariable"), "{message}");
        assert!(message.contains("name: \"UNDEF\""), "{message}");
        error("R1=undef");
        // An axis value is not a variable read.
        assert!(error("X10\nR1=X+1").starts_with("UndefinedVariable"));

        let allow = Opts {
            allow_undefined: true,
            ..Opts::default()
        };
        assert_eq!(parity_with("R1=UNDEF+1", &allow), num(1.0));
        let (outcome, state) = ours_with("R1=UNDEF+1", &allow);
        assert_eq!(outcome, num(1.0));
        assert_eq!(state.symbol_table["UNDEF"], 0.0);
    }

    #[test]
    fn string_variable_in_numeric_expression_errors() {
        let message = error("DEF STRING[8] STR=\"abc\"\nR1=STR+1");
        assert!(message.contains("'STR' is a STRING variable"), "{message}");
        assert!(message.contains("expression evaluation"), "{message}");
        error("DEF STRING[8] STR=\"abc\"\nR1=-STR");
    }

    #[test]
    fn numeric_functions() {
        for (expr, expected) in [
            ("SIN(30)", 30f64.to_radians().sin()),
            ("COS(60)", 60f64.to_radians().cos()),
            ("TAN(45)", 45f64.to_radians().tan()),
            ("ASIN(0.5)", 0.5f64.asin().to_degrees()),
            ("ACOS(0.5)", 0.5f64.acos().to_degrees()),
            ("ATAN2(30.5,80.1)", 30.5f64.atan2(80.1).to_degrees()),
            ("SQRT(2)", 2f64.sqrt()),
            ("ABS(-3)", 3.0),
            ("POT(3)", 9.0),
            ("TRUNC(-2.7)", -2.0),
            ("ROUND(2.5)", 3.0),
            ("ROUND(-2.5)", -3.0),
            ("ROUNDUP(2.1)", 3.0),
            ("MINVAL(1,2)", 1.0),
            ("MAXVAL(1,2)", 2.0),
            ("BOUND(0,10,15)", 10.0),
            ("BOUND(0,10,-5)", 0.0),
            ("LN(2)", 2f64.ln()),
            ("EXP(1)", 1f64.exp()),
            ("sin(30)", 30f64.to_radians().sin()),
            (
                "SIN(30)+COS(60)*2",
                30f64.to_radians().sin() + 60f64.to_radians().cos() * 2.0,
            ),
        ] {
            assert_eq!(value(expr), expected, "{expr}");
        }
        assert_eq!(parity("R1=SQRT(-1)"), num(f64::NAN));
        assert!(error("R1=BOUND(10,0,5)").contains("BOUND minimum (10) is greater than maximum (0)"));
    }

    #[test]
    fn omitted_arguments_are_skipped() {
        assert_eq!(value("ATAN2(1,,1)"), 45.0);
        assert_eq!(value("SIN(,30)"), 30f64.to_radians().sin());
        assert_eq!(value("MAXVAL(1,,,2)"), 2.0);
        assert!(error("R1=SIN(1,,2)").contains("actual: 2"));
    }

    #[test]
    fn function_arity_errors() {
        for (call, expected) in [
            ("SIN(1,2)", 1),
            ("COS(1,2)", 1),
            ("TAN(1,2)", 1),
            ("ASIN(1,2)", 1),
            ("ACOS(1,2)", 1),
            ("ATAN2(1)", 2),
            ("ATAN2(1,2,3)", 2),
            ("SQRT(1,2)", 1),
            ("ABS(1,2)", 1),
            ("POT(1,2)", 1),
            ("TRUNC(1,2)", 1),
            ("ROUND(1,2)", 1),
            ("ROUNDUP(1,2)", 1),
            ("MINVAL(1)", 2),
            ("MAXVAL(1,2,3)", 2),
            ("BOUND(0,10)", 3),
            ("LN(1,2)", 1),
            ("EXP(1,2)", 1),
            ("sin(1,2)", 1),
            ("INDEX(\"a\")", 2),
            ("RINDEX(\"a\",\"b\",\"c\")", 2),
            ("MINDEX(\"a\")", 2),
            ("MATCH(\"a\")", 2),
            ("NUMBER(\"1\",\"2\")", 1),
            ("STRLEN(\"a\",\"b\")", 1),
            ("isnumber(\"a\",\"b\")", 1),
        ] {
            let message = error(&format!("R1={call}"));
            assert!(message.starts_with("InvalidFunctionArity"), "{call}: {message}");
            assert!(message.contains(&format!("expected: {expected}")), "{call}: {message}");
        }
        // The numeric family quotes the name as written, the string family uppercased.
        assert!(error("R1=sin(1,2)").contains("name: \"sin\""));
        assert!(error("R1=isnumber(\"a\",\"b\")").contains("name: \"ISNUMBER\""));
    }

    #[test]
    fn function_type_and_name_errors() {
        assert!(error("R1=SIN(\"a\")").contains("expected a number but got the string \\\"a\\\""));
        error("R1=SIN(\"a\" << 1)");
        error("R1=MINVAL(1, << 2)");
        // Arguments are evaluated before the arity check and the name lookup.
        assert!(error("R1=SIN(1,UNDEF)").starts_with("UndefinedVariable"));
        assert!(error("R1=FOO(UNDEF)").starts_with("UndefinedVariable"));
        assert!(error("R1=FOO(1)").contains("'FOO' is not a known arithmetic function"));
        assert!(error("R1=foo(\"x\")").contains("expected a number"));
        assert!(error("R1=SPRINT(\"%d\",1)+1").contains("'SPRINT' returns a string"));
        assert!(error("R1=1+substr(\"abc\",1)").contains("'SUBSTR' returns a string"));
    }

    #[test]
    fn string_query_functions() {
        assert_eq!(value("INDEX(\"abcb\",\"b\")"), 1.0);
        assert_eq!(value("RINDEX(\"abcb\",\"b\")"), 3.0);
        assert_eq!(value("INDEX(\"abc\",\"z\")"), -1.0);
        assert_eq!(value("INDEX(\"abc\",\"cd\")"), 2.0);
        assert_eq!(value("INDEX(12345,3)"), 2.0);
        assert_eq!(value("MINDEX(\"ABCDEFG\",\"M,N,O,D\")"), 3.0);
        assert_eq!(value("MINDEX(\"A,B\",\"M,N\")"), 1.0);
        assert_eq!(value("MINDEX(\"ABC\",\"XYZ\")"), -1.0);
        assert_eq!(value("MATCH(\"Hello World\",\"World\")"), 6.0);
        assert_eq!(value("MATCH(\"abc\",\"\")"), 0.0);
        assert_eq!(value("MATCH(\"abc\",\"zz\")"), -1.0);
        assert_eq!(value("NUMBER(\"1234.9876EX-7\")"), 1234.9876e-7);
        assert_eq!(value("NUMBER(\" 42 \")"), 42.0);
        assert_eq!(value("STRLEN(\"hello\")"), 5.0);
        assert_eq!(value("STRLEN(\"\")"), 0.0);
        assert_eq!(value("STRLEN(1.5)"), 3.0);
        assert_eq!(value("ISNUMBER(\"12\")"), 1.0);
        assert_eq!(value("ISNUMBER(\"x\")"), 0.0);
        assert_eq!(value("STRLEN(\"ab\" << 12)+1"), 5.0);
        assert!(error("R1=INDEX(\"abc\",\"\")").contains("INDEX needs a non-empty search character"));
        assert!(error("R1=rindex(\"abc\",\"\")").contains("RINDEX needs a non-empty search character"));
        assert!(error("R1=NUMBER(\"abc\")").contains("NUMBER(\\\"abc\\\"): the string is not a valid number"));
        assert!(error("R1=STRLEN(UNDEF)").starts_with("UndefinedVariable"));

        let setup = "DEF STRING[20] STR=\"hello\"\n";
        assert_eq!(parity(&format!("{setup}R1=INDEX(STR,\"l\")")), num(2.0));
        assert_eq!(parity(&format!("{setup}R1=RINDEX((STR),\"l\")")), num(3.0));
        assert_eq!(parity(&format!("{setup}R1=STRLEN(STR << \"!\")")), num(6.0));
        assert_eq!(parity(&format!("{setup}R1=MATCH(SUBSTR(STR,1),\"llo\")")), num(1.0));
    }

    #[test]
    fn sprint_formats() {
        let setup = "DEF STRING[80] STR\n";
        let sprint = |args: &str| text(&format!("{setup}STR=SPRINT({args})"));
        assert_eq!(
            sprint("\"%d|%5.2F|%s|%x|%b|%c\", 3.6, 3.14159, \"ab\", 255, 0, 65"),
            "4| 3.14|ab|FF|FALSE|A"
        );
        assert_eq!(sprint("\"%F\", 2"), "2.000000");
        assert_eq!(sprint("\"%.0f\", 2.5"), "2");
        assert_eq!(sprint("\"100%%\""), "100%");
        assert_eq!(sprint("\"[%6S]\", \"ab\""), "[    ab]");
        assert_eq!(sprint("\"%.2S\", \"abcdef\""), "ab");
        assert_eq!(sprint("\"%S\", 1.25"), "1.25");
        assert_eq!(sprint("\"%B %B\", \"\", \"x\""), "FALSE TRUE");
        assert_eq!(sprint("\"%B\", 2"), "TRUE");
        assert_eq!(sprint("\"%C\", \"xyz\""), "x");
        assert_eq!(sprint("\"%3D\", -4"), " -4");
        assert_eq!(sprint("\"no conversions\""), "no conversions");
        assert_eq!(sprint("\"%s-%s-%s\", \"a\" << 1, << 2, \"c\" << \"d\""), "a1-2-cd");
        for (args, message) in [
            ("\"%c\", 300", "%c character code 300 is outside 0..255"),
            ("\"%c\", -1", "%c character code -1 is outside 0..255"),
            ("\"abc %\"", "dangling '%' at end of SPRINT format"),
            ("\"%d %d\", 1", "SPRINT format has more conversions than arguments"),
            ("\"%q\", 1", "SPRINT conversion '%Q' is not supported"),
            ("\"%1.2.3F\", 1", "invalid SPRINT precision '2.3'"),
            ("\"%d\", \"a\"", "expected a number but got the string \\\"a\\\""),
        ] {
            let message_got = error(&format!("{setup}STR=SPRINT({args})"));
            assert!(message_got.contains(message), "{args}: {message_got}");
            assert!(message_got.contains("line_no: 2"), "{args}: {message_got}");
        }
    }

    #[test]
    fn substr() {
        let setup = "DEF STRING[80] STR\n";
        let substr = |args: &str| text(&format!("{setup}STR=SUBSTR({args})"));
        assert_eq!(substr("\"ABCDEF\",2,3"), "CDE");
        assert_eq!(substr("\"ABCDEF\",2"), "CDEF");
        assert_eq!(substr("\"ABC\",5"), "");
        assert_eq!(substr("\"ABC\",1,99"), "BC");
        assert_eq!(substr("\"ABC\",3,0"), "");
        assert_eq!(substr("1234,1,2"), "23");
        assert_eq!(text(&format!("{setup}STR=(SUBSTR(\"ABC\",1))")), "BC");
        for (args, message) in [
            ("\"ABC\"", "InvalidFunctionArity"),
            ("\"ABC\",1,1,1", "InvalidFunctionArity"),
            ("\"ABC\",-1", "expected a non-negative integer index, got -1"),
            ("\"ABC\",1.5", "expected a non-negative integer index, got 1.5"),
            ("\"ABC\",0,0.5", "expected a non-negative integer index, got 0.5"),
            ("\"ABC\",\"1\"", "expected a number but got the string"),
        ] {
            let got = error(&format!("{setup}STR=SUBSTR({args})"));
            assert!(got.contains(message), "{args}: {got}");
        }
    }

    #[test]
    fn concat_chains_format_numbers() {
        let setup = "DEF STRING[80] STR\nDEF STRING[10] TXT=\"tt\"\n";
        let concat = |rhs: &str| text(&format!("{setup}STR={rhs}"));
        assert_eq!(concat("\"X=\" << 1.5"), "X=1.5");
        assert_eq!(concat("\"A\" << 2 << \"B\""), "A2B");
        assert_eq!(concat("<< 3.25"), "3.25");
        assert_eq!(concat("<< \"a\" << 1"), "a1");
        assert_eq!(concat("\"n\" << (1+2)"), "n3");
        assert_eq!(concat("\"v\" << -0.5"), "v-0.5");
        assert_eq!(concat("\"\" << 1/3"), "0.3333333333");
        assert_eq!(concat("\"\" << 2/3"), "0.6666666667");
        assert_eq!(concat("\"\" << 1.0000000000001"), "1");
        assert_eq!(concat("\"\" << 100000000000000000000"), "100000000000000000000");
        assert_eq!(concat("\"\" << -7"), "-7");
        assert_eq!(concat("\"a\" << TXT << \"b\""), "attb");
        assert_eq!(concat("TXT << SPRINT(\"%d\", 3)"), "tt3");
        assert_eq!(concat("\"x\" << TRUE"), "x1");
        assert_eq!(concat("TXT"), "tt");
        assert_eq!(concat("((TXT))"), "tt");
        assert_eq!(concat("\"lit\""), "lit");
        assert!(error(&format!("{setup}STR=\"x\" << UNDEF")).starts_with("UndefinedVariable"));
        error(&format!("{setup}STR=\"x\" << TXT+1"));
    }

    #[test]
    fn expression_string_detection() {
        let mut state = State::new(vec!["X".to_string()], 1000, None, false);
        state.string_table.insert("S".to_string(), "abc".to_string());
        state.symbol_table.insert("R".to_string(), 1.0);
        let var = |name: &str| Expr::Var(name.to_string());
        let call = |name: &str| Expr::Call {
            name: name.to_string(),
            args: vec![ast::Arg::Expr(Expr::Number(1.0))],
        };
        let paren = |expr: Expr| Expr::Paren(Box::new(expr));
        assert!(expr_is_string(&var("S"), &state));
        assert!(expr_is_string(&paren(paren(var("S"))), &state));
        assert!(expr_is_string(&call("sprint"), &state));
        assert!(expr_is_string(&paren(call("SUBSTR")), &state));
        assert!(!expr_is_string(&var("R"), &state));
        assert!(!expr_is_string(&var("UNDEF"), &state));
        assert!(!expr_is_string(&call("INDEX"), &state));
        assert!(!expr_is_string(&Expr::Unary(UnaryOp::Neg, Box::new(var("S"))), &state));
        assert!(!expr_is_string(
            &Expr::Binary(BinaryOp::Add, Box::new(var("S")), Box::new(var("S"))),
            &state
        ));
        let element = Expr::Element {
            name: "S".to_string(),
            indices: vec![Index {
                expr: Expr::Number(0.0),
                text: "0".to_string(),
            }],
        };
        assert!(!expr_is_string(&element, &state));
    }

    #[test]
    fn array_element_reads() {
        let setup = "DEF REAL VEC[3]\nDEF REAL MAT[2,3]\nDEF REAL CUBE[1,2,3]\nVEC[2]=5\nMAT[1,2]=6\nCUBE[1,1,2]=7\n";
        assert_eq!(parity(&format!("{setup}R1=VEC[2]")), num(5.0));
        assert_eq!(parity(&format!("{setup}R1=VEC[0]")), num(0.0));
        assert_eq!(parity(&format!("{setup}R1=MAT[1,2]")), num(6.0));
        assert_eq!(parity(&format!("{setup}R1=cube[1,1,2]+1")), num(8.0));
        assert_eq!(parity(&format!("{setup}R2=1\nR1=VEC[R2+1]")), num(5.0));
        assert_eq!(parity(&format!("{setup}R1=VEC[(2)]")), num(5.0));
        for program in [
            format!("{setup}R1=VEC[5]"),
            format!("{setup}R1=MAT[2,4]"),
            format!("{setup}R1=B[0]"),
        ] {
            assert!(error(&program).starts_with("UnknownVariable"), "{program}");
        }
        assert!(error(&format!("{setup}R1=VEC[1,1,1,1]")).contains("Invalid number of indices for variable array"));
        assert!(error(&format!("{setup}R1=VEC[UNDEF,1,1,1]")).starts_with("UndefinedVariable"));
        assert!(error(&format!("{setup}R1=VEC[-1]")).starts_with("InvalidAxisIndex"));
        assert!(error(&format!("{setup}R1=VEC[1.5]")).contains("axis: \"1.5\""));
        // An undefined element warns and becomes 0.0 when allowed.
        let allow = Opts {
            allow_undefined: true,
            ..Opts::default()
        };
        assert_eq!(parity_with("R1=B[2,1]+1", &allow), num(1.0));
        let (_, state) = ours_with("R1=B[2,1]+1", &allow);
        assert_eq!(state.symbol_table["B[2,1]"], 0.0);
    }

    #[test]
    fn axis_named_indices() {
        let mapped = Opts {
            axis_index_map: Some(HashMap::from([("X".to_string(), 1), ("Y".to_string(), 2)])),
            ..Opts::default()
        };
        let setup = "DEF REAL A[3]\nA[1]=11\nA[2]=22\n";
        assert_eq!(parity_with(&format!("{setup}R1=A[X]"), &mapped), num(11.0));
        assert_eq!(parity_with(&format!("{setup}R1=A[ Y ]+A[X]"), &mapped), num(33.0));
        // The lookup is case-sensitive although the axis test is not.
        let message = parity_with(&format!("{setup}R1=A[x]"), &mapped);
        assert!(
            matches!(&message, Outcome::Err(m) if m.starts_with("MissingAxisMapping")),
            "{message:?}"
        );
        // `(X)` is an expression, not the axis name: it reads X as a variable.
        let message = parity_with(&format!("{setup}R1=A[(X)]"), &mapped);
        assert!(
            matches!(&message, Outcome::Err(m) if m.starts_with("UndefinedVariable")),
            "{message:?}"
        );
        let message = parity_with(&format!("{setup}R1=A[Z]"), &mapped);
        assert!(
            matches!(&message, Outcome::Err(m) if m.starts_with("MissingAxisMapping")),
            "{message:?}"
        );
        assert!(error(&format!("{setup}R1=A[X]")).starts_with("MissingAxisMapping"));
    }

    #[test]
    fn actual_position_reads() {
        assert_eq!(parity("X10\nR1=$AA_IW[X]"), num(10.0));
        assert_eq!(parity("X10\nR1=$aa_iw[ x ]+1"), num(11.0));
        assert_eq!(parity("X10\nR1=$AA_IM[X]"), num(10.0));
        assert_eq!(parity("X10\nTRANS X5\nX10\nR1=$AA_IW[X]"), num(10.0));
        assert_eq!(parity("X10\nTRANS X5\nX10\nR1=$AA_IM[X]"), num(15.0));
        assert_eq!(
            parity("X10 Y0\nROT Z90\nX10 Y0\nR1=$AA_IM[Y]"),
            parity("X10 Y0\nROT Z90\nX10 Y0\nR1=$AA_IM[Y]")
        );
        let message = error("X10\nR1=$AA_IW[Y]");
        assert!(
            message.contains("$AA_IW[Y] is read before axis Y has a position"),
            "{message}"
        );
        assert!(message.contains("actual-position system variable"), "{message}");
        assert!(message.contains("line_no: 2"), "{message}");
        error("R1=$AA_IM[Z]+1");
        // Not an axis index: the generic element read of the system variable name.
        assert!(error("R1=$AA_IW[1]").contains("variable: \"$AA_IW[1]\""));
        assert!(error("R1=$AA_IW[X,Y]").starts_with("MissingAxisMapping"));
        let allow = Opts {
            allow_undefined: true,
            ..Opts::default()
        };
        assert_eq!(parity_with("R1=$AA_IW[Y]+2", &allow), num(2.0));
        let (_, state) = ours_with("R1=$AA_IW[Y]+2", &allow);
        assert!(!state.symbol_table.contains_key("$AA_IW[Y]"));
    }

    fn index(text: &str) -> Index {
        Index {
            expr: Expr::Number(text.parse().expect("a number")),
            text: text.to_string(),
        }
    }

    #[test]
    fn element_keys_in_walker_order() {
        let mut state = State::new(vec!["X".to_string()], 1000, None, false);
        assert_eq!(
            element_keys("A", &[index("2")], &mut state, 1).unwrap(),
            ["A[0]", "A[1]", "A[2]"]
        );
        assert_eq!(
            element_keys("A", &[index("1"), index("1")], &mut state, 1).unwrap(),
            ["A[0,0]", "A[0,1]", "A[1,0]", "A[1,1]"]
        );
        assert_eq!(
            element_keys("A", &[index("1"), index("0"), index("1")], &mut state, 1).unwrap(),
            ["A[0,0,0]", "A[0,0,1]", "A[1,0,0]", "A[1,0,1]"]
        );
        let err = element_keys("A", &[index("1"), index("1"), index("1"), index("1")], &mut state, 1).unwrap_err();
        assert_eq!(
            format!("{err:?}"),
            "ParseError { message: \"Invalid number of indices for variable array\" }"
        );
        assert_eq!(
            eval_indices(&[index("3"), index("0")], &mut state, 1).unwrap(),
            [3.0, 0.0]
        );
    }

    #[test]
    fn element_keys_match_a_definition() {
        let walker = reference("DEF REAL A[2,1,1]\n", &Opts::default()).expect("runs");
        let mut defined: Vec<&String> = walker.symbol_table.keys().filter(|k| k.starts_with("A[")).collect();
        defined.sort();
        let mut state = State::new(vec!["X".to_string()], 1000, None, false);
        let keys = element_keys("A", &[index("2"), index("1"), index("1")], &mut state, 1).unwrap();
        let mut sorted: Vec<&String> = keys.iter().collect();
        sorted.sort();
        assert_eq!(sorted, defined);
        assert_eq!(keys.last().map(String::as_str), Some("A[2,1,1]"));
    }
}
