//! pest `Pair<Rule>` to `ast::Block`, one `block` or `line_block` pair at a
//! time, with the operator priorities and associativity of
//! `evaluate_expression`. Lowering never evaluates anything, so it is total
//! over what the grammar accepts; a pair it cannot place is a ParsingError
//! with the block's line, never a panic.

use crate::ast::{
    Arg, AssignMulti, Assignment, BinaryOp, Block, Body, Call, Control, DataType, DefItem, Definition, ElementRhs,
    Expr, FrameKind, FrameOp, Goto, GotoKind, Index, MultiValues, Operand, Rhs, Statement, StringExpr, UnaryOp,
};
use crate::errors::ParsingError;
use crate::state::State;
use crate::types::{NCParser, Pair, Rule};
use pest::iterators::Pairs;
use pest::Parser;

/// Parse one source line with the `line` entry and lower it. Errors carry the
/// whole-file line number and the column within the line, like the fast path.
pub(crate) fn parse_line(text: &str, line_no: usize, state: &State) -> Result<Block, ParsingError> {
    let mut pairs = NCParser::parse(Rule::line, text).map_err(|e| {
        let column = match &e.line_col {
            pest::error::LineColLocation::Pos((_, col)) | pest::error::LineColLocation::Span((_, col), _) => *col,
        };
        ParsingError::ParsingContext {
            line_no,
            column: Some(column),
            preview: state
                .get_line(line_no)
                .unwrap_or("(could not retrieve line)")
                .to_string(),
            context: "line parsing".to_string(),
            message: crate::interpreter::describe_parse_error(&e),
        }
    })?;
    let cx = Cx { line_no, preview: text };
    let line = cx.next(&mut pairs, "the line")?;
    let block = cx.expect(&mut line.into_inner(), Rule::line_block, "the line")?;
    lower_block(block, line_no)
}

/// Lower a `block` (whole-file or sparse parse) or `line_block` pair. A
/// `block` holding a multi-line IF/WHILE/FOR/LOOP/REPEAT is an error: those
/// are lowered line by line through `line_block`.
pub(crate) fn lower_block(pair: Pair<Rule>, line_no: usize) -> Result<Block, ParsingError> {
    let cx = Cx {
        line_no,
        preview: pair.as_str().lines().next().unwrap_or(""),
    };
    if !matches!(pair.as_rule(), Rule::block | Rule::line_block) {
        return Err(cx.unexpected(&pair, "a block"));
    }
    let mut block = Block {
        line_no,
        block_number: None,
        label: None,
        body: Body::Empty,
        comment: None,
    };
    let mut statements = Vec::new();
    for item in pair.into_inner() {
        match item.as_rule() {
            Rule::block_number => {
                let integer = cx.expect(&mut item.into_inner(), Rule::integer, "the block number")?;
                block.block_number = Some(integer.as_str().to_string());
            }
            Rule::label_def => {
                let name = cx.expect(&mut item.into_inner(), Rule::label_name, "the jump label")?;
                block.label = Some(name.as_str().to_uppercase());
            }
            Rule::definition => block.body = Body::Definition(lower_definition(&cx, item)?),
            Rule::frame_op => block.body = Body::FrameOp(lower_frame_op(&cx, item)?),
            Rule::statement => statements.push(lower_statement(&cx, item)?),
            Rule::control | Rule::line_control => block.body = Body::Control(lower_control(&cx, item)?),
            Rule::comment => block.comment = Some(item.as_str().to_string()),
            _ => return Err(cx.unexpected(&item, "a block")),
        }
    }
    if !statements.is_empty() {
        block.body = Body::Statements(statements);
    }
    Ok(block)
}

/// Error context: the block's line and its source text as the preview.
struct Cx<'s> {
    line_no: usize,
    preview: &'s str,
}

impl Cx<'_> {
    fn unexpected(&self, pair: &Pair<Rule>, place: &str) -> ParsingError {
        ParsingError::UnexpectedRule {
            rule: pair.as_rule(),
            context: "AST lowering".to_string(),
            line_no: self.line_no,
            preview: self.preview.to_string(),
            message: format!("unexpected {:?} in {place}", pair.as_rule()),
        }
    }

    fn error(&self, context: &str, message: String) -> ParsingError {
        ParsingError::with_context(self.line_no, self.preview.to_string(), context.to_string(), message)
    }

    fn next<'i>(
        &self,
        pairs: &mut impl Iterator<Item = Pair<'i, Rule>>,
        place: &str,
    ) -> Result<Pair<'i, Rule>, ParsingError> {
        pairs
            .next()
            .ok_or_else(|| self.error("AST lowering", format!("{place} ends early")))
    }

    fn expect<'i>(&self, pairs: &mut Pairs<'i, Rule>, rule: Rule, place: &str) -> Result<Pair<'i, Rule>, ParsingError> {
        let pair = self.next(pairs, place)?;
        if pair.as_rule() == rule {
            Ok(pair)
        } else {
            Err(self.unexpected(&pair, place))
        }
    }

    fn done(&self, pairs: &mut Pairs<Rule>, place: &str) -> Result<(), ParsingError> {
        match pairs.next() {
            Some(extra) => Err(self.unexpected(&extra, place)),
            None => Ok(()),
        }
    }

    fn number(&self, pair: &Pair<Rule>) -> Result<f64, ParsingError> {
        pair.as_str()
            .trim()
            .parse::<f64>()
            .map_err(|_| self.error("numeric literal", format!("'{}' is not a valid number", pair.as_str())))
    }
}

fn lower_statement(cx: &Cx, pair: Pair<Rule>) -> Result<Statement, ParsingError> {
    let inner = cx.next(&mut pair.into_inner(), "a statement")?;
    match inner.as_rule() {
        Rule::axis_word => {
            let mut parts = inner.into_inner();
            let letter = cx.expect(&mut parts, Rule::variable_single_char, "an axis word")?;
            let value = cx.expect(&mut parts, Rule::value, "an axis word")?;
            Ok(Statement::Assign(Assignment::Var {
                name: letter.as_str().to_uppercase(),
                rhs: Rhs::Expr(Expr::Number(cx.number(&value)?)),
            }))
        }
        Rule::g_command_numbered => {
            let g = cx.expect(&mut inner.into_inner(), Rule::g_command, "a G command")?;
            Ok(Statement::G(g.as_str().trim().to_uppercase()))
        }
        Rule::g_command => Ok(Statement::G(inner.as_str().trim().to_uppercase())),
        Rule::m_command => Ok(Statement::M(inner.as_str().trim_end().to_uppercase())),
        Rule::tool_selection => {
            let value = cx.expect(&mut inner.into_inner(), Rule::string_value, "a tool selection")?;
            Ok(Statement::Tool(string_value_text(value)))
        }
        Rule::assignment_multi => Ok(Statement::AssignMulti(lower_assign_multi(cx, inner)?)),
        Rule::assignment => Ok(Statement::Assign(lower_assignment(cx, inner)?)),
        Rule::non_returning_function_call => lower_call(cx, inner),
        _ => Err(cx.unexpected(&inner, "a statement")),
    }
}

/// A parenless call that names a G-vocabulary keyword is that G command, as
/// in `interpret_statement`; frame keywords stay calls (the executor rejects
/// them after another statement).
fn lower_call(cx: &Cx, pair: Pair<Rule>) -> Result<Statement, ParsingError> {
    let text = pair.as_str();
    let mut parts = pair.into_inner();
    let name = cx.expect(&mut parts, Rule::identifier, "a call")?.as_str();
    let args = match parts.next() {
        Some(args) if args.as_rule() == Rule::function_arguments => Some(lower_arguments(cx, args)?),
        Some(other) => return Err(cx.unexpected(&other, "a call")),
        None if text.contains('(') => Some(Vec::new()),
        None => None,
    };
    if args.is_none()
        && !crate::interpret_rules::FRAME_KEYWORDS
            .iter()
            .any(|kw| kw.eq_ignore_ascii_case(name))
    {
        let upper = name.to_uppercase();
        if crate::modal_groups::classify_g_command(&upper).is_some() {
            return Ok(Statement::G(upper));
        }
    }
    Ok(Statement::Call(Call {
        name: name.to_string(),
        args,
        text: text.to_string(),
    }))
}

fn lower_assignment(cx: &Cx, pair: Pair<Rule>) -> Result<Assignment, ParsingError> {
    let mut parts = pair.into_inner();
    let target = cx.next(&mut parts, "an assignment")?;
    let value = cx.next(&mut parts, "an assignment")?;
    cx.done(&mut parts, "an assignment")?;
    match (target.as_rule(), value.as_rule()) {
        (Rule::variable_single_char, Rule::value) => Ok(Assignment::Var {
            name: target.as_str().to_uppercase(),
            rhs: Rhs::Expr(Expr::Number(cx.number(&value)?)),
        }),
        (Rule::variable, Rule::axis_increment) => {
            let expression = cx.expect(&mut value.into_inner(), Rule::expression, "IC()")?;
            Ok(Assignment::Var {
                name: variable_name(cx, target)?,
                rhs: Rhs::Increment(lower_expression(cx, expression)?),
            })
        }
        (Rule::variable, Rule::string_expression) => Ok(Assignment::Var {
            name: variable_name(cx, target)?,
            rhs: match lower_string_expression(cx, value)? {
                Lowered::Expr(expr) => Rhs::Expr(expr),
                Lowered::Str(value, _) => Rhs::Str(value),
            },
        }),
        (Rule::variable_array, Rule::string_expression) => {
            let (name, indices) = lower_variable_array(cx, target)?;
            Ok(Assignment::Element {
                name,
                indices,
                rhs: match lower_string_expression(cx, value)? {
                    Lowered::Expr(expr) => ElementRhs::Expr(expr),
                    Lowered::Str(value, _) => ElementRhs::Str(value),
                },
            })
        }
        _ => Err(cx.unexpected(&value, "an assignment")),
    }
}

fn lower_assign_multi(cx: &Cx, pair: Pair<Rule>) -> Result<AssignMulti, ParsingError> {
    let mut parts = pair.into_inner();
    let target = cx.expect(&mut parts, Rule::variable_array, "a SET/REP assignment")?;
    let (name, indices) = lower_variable_array(cx, target)?;
    let values_pair = cx.next(&mut parts, "a SET/REP assignment")?;
    cx.done(&mut parts, "a SET/REP assignment")?;
    let values = match values_pair.as_rule() {
        Rule::value_array => {
            let mut values = Vec::new();
            for item in values_pair.into_inner() {
                match item.as_rule() {
                    Rule::expression => values.push(Some(lower_expression(cx, item)?)),
                    Rule::value_none => values.push(None),
                    _ => return Err(cx.unexpected(&item, "SET()")),
                }
            }
            MultiValues::Set(values)
        }
        Rule::value_repeating => {
            let mut items = values_pair.into_inner();
            let value = lower_expression(cx, cx.expect(&mut items, Rule::expression, "REP()")?)?;
            let count = match items.next() {
                Some(count) if count.as_rule() == Rule::expression => Some(lower_expression(cx, count)?),
                Some(other) => return Err(cx.unexpected(&other, "REP()")),
                None => None,
            };
            MultiValues::Rep { value, count }
        }
        _ => return Err(cx.unexpected(&values_pair, "a SET/REP assignment")),
    };
    Ok(AssignMulti { name, indices, values })
}

/// `variable = { nc_variable | identifier }`, uppercased like `interpret_variable`.
fn variable_name(cx: &Cx, pair: Pair<Rule>) -> Result<String, ParsingError> {
    let inner = cx.next(&mut pair.into_inner(), "a variable")?;
    match inner.as_rule() {
        Rule::identifier | Rule::nc_variable => Ok(inner.as_str().to_uppercase()),
        _ => Err(cx.unexpected(&inner, "a variable")),
    }
}

fn lower_variable_array(cx: &Cx, pair: Pair<Rule>) -> Result<(String, Vec<Index>), ParsingError> {
    let mut parts = pair.into_inner();
    let name = cx.next(&mut parts, "an array element")?;
    if !matches!(name.as_rule(), Rule::identifier | Rule::nc_variable) {
        return Err(cx.unexpected(&name, "an array element"));
    }
    let indices_pair = cx.expect(&mut parts, Rule::indices, "an array element")?;
    cx.done(&mut parts, "an array element")?;
    let mut indices = Vec::new();
    for index in indices_pair.into_inner() {
        if index.as_rule() != Rule::expression {
            return Err(cx.unexpected(&index, "array indices"));
        }
        let text = index.as_str().trim().to_string();
        indices.push(Index {
            expr: lower_expression(cx, index)?,
            text,
        });
    }
    Ok((name.as_str().to_uppercase(), indices))
}

/// `string_value` content; the inner `string` may be absent for `""`.
fn string_value_text(pair: Pair<Rule>) -> String {
    pair.into_inner()
        .next()
        .map(|s| s.as_str().to_string())
        .unwrap_or_default()
}

/// A lowered `string_expression`: a plain expression, or a string with its
/// source text.
enum Lowered<'i> {
    Expr(Expr),
    Str(StringExpr, &'i str),
}

/// Mirrors `unwrap_lone_operand`: one operand without a leading `<<` is that
/// operand itself; anything else is a concatenation.
fn lower_string_expression<'i>(cx: &Cx, pair: Pair<'i, Rule>) -> Result<Lowered<'i>, ParsingError> {
    let start = pair.as_span().start();
    let text = pair.as_str();
    let mut operands = pair.into_inner().peekable();
    let first = cx.next(&mut operands, "a << chain")?;
    if operands.peek().is_none() && first.as_span().start() == start {
        let inner = cx.next(&mut first.into_inner(), "a string operand")?;
        return match inner.as_rule() {
            Rule::expression => Ok(Lowered::Expr(lower_expression(cx, inner)?)),
            Rule::string_value => {
                let quoted = inner.as_str();
                Ok(Lowered::Str(StringExpr::Literal(string_value_text(inner)), quoted))
            }
            _ => Err(cx.unexpected(&inner, "a string operand")),
        };
    }
    let first = lower_operand(cx, first)?;
    let rest = operands
        .map(|operand| lower_operand(cx, operand))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Lowered::Str(StringExpr::Concat(first, rest), text))
}

fn lower_operand(cx: &Cx, pair: Pair<Rule>) -> Result<Operand, ParsingError> {
    if pair.as_rule() != Rule::concat_operand {
        return Err(cx.unexpected(&pair, "a << chain"));
    }
    let inner = cx.next(&mut pair.into_inner(), "a string operand")?;
    match inner.as_rule() {
        Rule::expression => Ok(Operand::Expr(lower_expression(cx, inner)?)),
        Rule::string_value => Ok(Operand::Literal(string_value_text(inner))),
        _ => Err(cx.unexpected(&inner, "a string operand")),
    }
}

/// Function arguments in position order. Empty positions produce no pair, so
/// they are recovered from the commas between the argument spans.
fn lower_arguments(cx: &Cx, pair: Pair<Rule>) -> Result<Vec<Arg>, ParsingError> {
    let base = pair.as_span().start();
    let source = pair.as_str();
    let mut cursor = 0;
    let mut args = Vec::new();
    for (position, argument) in pair.into_inner().enumerate() {
        if argument.as_rule() != Rule::string_expression {
            return Err(cx.unexpected(&argument, "function arguments"));
        }
        let span = argument.as_span();
        let out_of_order = || cx.error("AST lowering", "argument spans out of order".to_string());
        let start = span.start().checked_sub(base).ok_or_else(out_of_order)?;
        let gap = source.get(cursor..start).ok_or_else(out_of_order)?;
        let commas = gap.bytes().filter(|b| *b == b',').count();
        // The first argument's leading commas are all gaps; later ones keep one separator.
        let omitted = if position == 0 {
            commas
        } else {
            commas.saturating_sub(1)
        };
        args.extend(std::iter::repeat_n(Arg::Omitted, omitted));
        cursor = span.end().checked_sub(base).ok_or_else(out_of_order)?;
        args.push(match lower_string_expression(cx, argument)? {
            Lowered::Expr(expr) => Arg::Expr(expr),
            Lowered::Str(value, text) => Arg::Str {
                value,
                text: text.to_string(),
            },
        });
    }
    Ok(args)
}

fn lower_condition(cx: &Cx, pair: Pair<Rule>) -> Result<Expr, ParsingError> {
    let mut parts = pair.into_inner();
    let expression = cx.expect(&mut parts, Rule::expression, "a condition")?;
    cx.done(&mut parts, "a condition")?;
    lower_expression(cx, expression)
}

/// Lower the flat `prefix* primary (op prefix* primary)*` sequence into a
/// tree with the evaluator's priorities: tier 0 (comparisons) binds weakest,
/// then OR, XOR, AND, B_OR, B_XOR, B_AND, + -, and * / DIV MOD; each tier is
/// left-associative, prefixes bind to their primary only. Precedence climbing
/// builds the same tree as the evaluator's one-function-per-tier descent.
fn lower_expression(cx: &Cx, pair: Pair<Rule>) -> Result<Expr, ParsingError> {
    let mut lowering = ExprLowering {
        cx,
        pairs: pair.into_inner().peekable(),
    };
    let expr = lowering.climb(0)?;
    if let Some(extra) = lowering.pairs.next() {
        return Err(cx.unexpected(&extra, "an expression"));
    }
    Ok(expr)
}

/// The binary operator a pair stands for, with its tier.
fn binary_operator(pair: &Pair<Rule>) -> Option<(usize, BinaryOp)> {
    Some(match pair.as_rule() {
        Rule::relational_operator => (
            0,
            match pair.as_str() {
                "<" => BinaryOp::Lt,
                ">" => BinaryOp::Gt,
                "<=" => BinaryOp::Le,
                ">=" => BinaryOp::Ge,
                "==" => BinaryOp::Eq,
                "<>" => BinaryOp::Ne,
                _ => return None,
            },
        ),
        Rule::op_or => (1, BinaryOp::Or),
        Rule::op_xor => (2, BinaryOp::Xor),
        Rule::op_and => (3, BinaryOp::And),
        Rule::op_b_or => (4, BinaryOp::BitOr),
        Rule::op_b_xor => (5, BinaryOp::BitXor),
        Rule::op_b_and => (6, BinaryOp::BitAnd),
        Rule::op_add => (7, BinaryOp::Add),
        Rule::op_sub => (7, BinaryOp::Sub),
        Rule::op_mul => (8, BinaryOp::Mul),
        Rule::op_div => (8, BinaryOp::Div),
        Rule::op_int_div => (8, BinaryOp::IntDiv),
        Rule::op_mod => (8, BinaryOp::Mod),
        _ => return None,
    })
}

struct ExprLowering<'c, 's, 'i> {
    cx: &'c Cx<'s>,
    pairs: std::iter::Peekable<Pairs<'i, Rule>>,
}

impl ExprLowering<'_, '_, '_> {
    /// An operand followed by every operator of tier `min_tier` or tighter;
    /// the right operand of a tier-t operator only takes tighter operators,
    /// which makes equal tiers left-associative.
    fn climb(&mut self, min_tier: usize) -> Result<Expr, ParsingError> {
        let mut lhs = self.unary()?;
        while let Some((tier, op)) = self.pairs.peek().and_then(binary_operator) {
            if tier < min_tier {
                break;
            }
            self.pairs.next();
            let rhs = self.climb(tier + 1)?;
            lhs = Expr::binary(op, lhs, rhs);
        }
        Ok(lhs)
    }

    fn unary(&mut self) -> Result<Expr, ParsingError> {
        let mut prefixes = Vec::new();
        while let Some(op) = self.pairs.peek().and_then(|pair| match pair.as_rule() {
            Rule::neg => Some(UnaryOp::Neg),
            Rule::not_op => Some(UnaryOp::Not),
            _ => None,
        }) {
            prefixes.push(op);
            self.pairs.next();
        }
        let primary = self.pairs.next().ok_or_else(|| {
            self.cx
                .error("AST lowering", "an expression ends without an operand".to_string())
        })?;
        if primary.as_rule() != Rule::primary {
            return Err(self.cx.unexpected(&primary, "an expression operand"));
        }
        let mut value = lower_primary(self.cx, primary)?;
        // Innermost first, as evaluate_unary folds them: NOT -X is NOT(-X).
        for op in prefixes.into_iter().rev() {
            value = Expr::unary(op, value);
        }
        Ok(value)
    }
}

fn lower_primary(cx: &Cx, pair: Pair<Rule>) -> Result<Expr, ParsingError> {
    let mut parts = pair.into_inner();
    let inner = cx.next(&mut parts, "an operand")?;
    cx.done(&mut parts, "an operand")?;
    match inner.as_rule() {
        Rule::value => Ok(Expr::Number(cx.number(&inner)?)),
        Rule::variable => Ok(Expr::Var(variable_name(cx, inner)?)),
        Rule::variable_array => {
            let (name, indices) = lower_variable_array(cx, inner)?;
            Ok(Expr::Element { name, indices })
        }
        Rule::expression => Ok(Expr::Paren(Box::new(lower_expression(cx, inner)?))),
        Rule::arith_fun => {
            let mut parts = inner.into_inner();
            let name = cx.expect(&mut parts, Rule::arith_fun_name, "a function call")?;
            let args = cx.expect(&mut parts, Rule::function_arguments, "a function call")?;
            Ok(Expr::Call {
                name: name.as_str().to_string(),
                args: lower_arguments(cx, args)?,
            })
        }
        _ => Err(cx.unexpected(&inner, "an operand")),
    }
}

fn lower_definition(cx: &Cx, pair: Pair<Rule>) -> Result<Definition, ParsingError> {
    let mut parts = pair.into_inner();
    let data_type_pair = cx.expect(&mut parts, Rule::data_type, "DEF")?;
    let type_text = data_type_pair.as_str().to_ascii_uppercase();
    let data_type = match type_text.as_str() {
        "INT" => DataType::Int,
        "REAL" => DataType::Real,
        "BOOL" => DataType::Bool,
        text if text.starts_with("STRING") => DataType::String(text.chars().filter(char::is_ascii_digit).collect()),
        _ => return Err(cx.unexpected(&data_type_pair, "DEF")),
    };
    let mut items = Vec::new();
    for item in parts {
        items.push(match item.as_rule() {
            Rule::assignment_multi => DefItem::Multi(lower_assign_multi(cx, item)?),
            Rule::assignment => DefItem::Init(lower_assignment(cx, item)?),
            Rule::variable_array => {
                let (name, indices) = lower_variable_array(cx, item)?;
                DefItem::Array { name, indices }
            }
            Rule::variable => DefItem::Var(variable_name(cx, item)?),
            _ => return Err(cx.unexpected(&item, "DEF")),
        });
    }
    Ok(Definition { data_type, items })
}

fn lower_frame_op(cx: &Cx, pair: Pair<Rule>) -> Result<FrameOp, ParsingError> {
    let mut parts = pair.into_inner();
    let keyword = cx.expect(&mut parts, Rule::frame_kw, "a frame instruction")?;
    let kind =
        FrameKind::from_keyword(keyword.as_str()).ok_or_else(|| cx.unexpected(&keyword, "a frame instruction"))?;
    let mut assignments = Vec::new();
    for assignment in parts {
        if assignment.as_rule() != Rule::assignment {
            return Err(cx.unexpected(&assignment, "a frame instruction"));
        }
        assignments.push(lower_assignment(cx, assignment)?);
    }
    Ok(FrameOp { kind, assignments })
}

fn lower_goto(cx: &Cx, pair: Pair<Rule>) -> Result<Goto, ParsingError> {
    let mut parts = pair.into_inner();
    let keyword = cx.expect(&mut parts, Rule::goto_kw, "a jump")?;
    let target = cx.expect(&mut parts, Rule::goto_target, "a jump")?;
    cx.done(&mut parts, "a jump")?;
    let kind = match keyword.as_str().to_ascii_uppercase().as_str() {
        "GOTO" => GotoKind::Goto,
        "GOTOF" => GotoKind::GotoF,
        "GOTOB" => GotoKind::GotoB,
        "GOTOC" => GotoKind::GotoC,
        _ => return Err(cx.unexpected(&keyword, "a jump")),
    };
    Ok(Goto {
        kind,
        target: target.as_str().trim().to_string(),
    })
}

fn lower_if_goto(cx: &Cx, pair: Pair<Rule>) -> Result<(Expr, Goto), ParsingError> {
    let mut parts = pair.into_inner();
    let condition = lower_condition(cx, cx.expect(&mut parts, Rule::condition, "IF .. GOTO")?)?;
    let goto = lower_goto(cx, cx.expect(&mut parts, Rule::goto_statement, "IF .. GOTO")?)?;
    cx.done(&mut parts, "IF .. GOTO")?;
    Ok((condition, goto))
}

fn lower_case(cx: &Cx, pair: Pair<Rule>) -> Result<Control, ParsingError> {
    let mut parts = pair.into_inner();
    cx.expect(&mut parts, Rule::case_kw, "CASE")?;
    let selector = lower_expression(cx, cx.expect(&mut parts, Rule::expression, "CASE")?)?;
    cx.expect(&mut parts, Rule::of_kw, "CASE")?;
    let mut arms = Vec::new();
    let mut default = None;
    for part in parts {
        match part.as_rule() {
            Rule::case_arm if default.is_none() => {
                let mut arm = part.into_inner();
                let constant = cx.expect(&mut arm, Rule::value, "a CASE arm")?;
                let constant = constant.as_str().trim().parse::<f64>().map_err(|_| {
                    cx.error(
                        "CASE constant",
                        format!("'{}' is not a valid number", constant.as_str()),
                    )
                })?;
                let goto = lower_goto(cx, cx.expect(&mut arm, Rule::goto_statement, "a CASE arm")?)?;
                arms.push((constant, goto));
            }
            Rule::case_default if default.is_none() => {
                let mut arm = part.into_inner();
                cx.expect(&mut arm, Rule::default_kw, "DEFAULT")?;
                default = Some(lower_goto(cx, cx.expect(&mut arm, Rule::goto_statement, "DEFAULT")?)?);
            }
            _ => return Err(cx.unexpected(&part, "CASE")),
        }
    }
    Ok(Control::Case {
        selector,
        arms,
        default,
    })
}

fn lower_control(cx: &Cx, pair: Pair<Rule>) -> Result<Control, ParsingError> {
    let mut parts = pair.into_inner();
    let first = cx.next(&mut parts, "a control statement")?;
    let control = match first.as_rule() {
        Rule::gotos_statement => Control::GotoS,
        Rule::goto_statement => Control::Goto(lower_goto(cx, first)?),
        Rule::case_statement => lower_case(cx, first)?,
        Rule::if_goto_statement => {
            let mut jumps = vec![lower_if_goto(cx, first)?];
            for next in parts.by_ref() {
                if next.as_rule() != Rule::if_goto_statement {
                    return Err(cx.unexpected(&next, "IF .. GOTO"));
                }
                jumps.push(lower_if_goto(cx, next)?);
            }
            Control::IfGoto(jumps)
        }
        Rule::if_head => {
            let mut head = first.into_inner();
            cx.expect(&mut head, Rule::if_kw, "IF")?;
            let condition = lower_condition(cx, cx.expect(&mut head, Rule::condition, "IF")?)?;
            cx.done(&mut head, "IF")?;
            Control::IfHead(condition)
        }
        Rule::while_head => {
            let mut head = first.into_inner();
            cx.expect(&mut head, Rule::while_kw, "WHILE")?;
            let condition = lower_condition(cx, cx.expect(&mut head, Rule::condition, "WHILE")?)?;
            cx.done(&mut head, "WHILE")?;
            Control::WhileHead(condition)
        }
        Rule::for_head => {
            let mut head = first.into_inner();
            cx.expect(&mut head, Rule::for_kw, "FOR")?;
            let assignment = lower_assignment(cx, cx.expect(&mut head, Rule::assignment, "FOR")?)?;
            let to = lower_expression(cx, cx.expect(&mut head, Rule::expression, "FOR")?)?;
            cx.done(&mut head, "FOR")?;
            Control::ForHead { assignment, to }
        }
        Rule::until_tail => {
            let mut tail = first.into_inner();
            cx.expect(&mut tail, Rule::until_kw, "UNTIL")?;
            let condition = lower_condition(cx, cx.expect(&mut tail, Rule::condition, "UNTIL")?)?;
            cx.done(&mut tail, "UNTIL")?;
            Control::UntilTail(condition)
        }
        Rule::else_kw => Control::Else,
        Rule::endif_kw => Control::EndIf,
        Rule::endwhile_kw => Control::EndWhile,
        Rule::endfor_kw => Control::EndFor,
        Rule::repeat_kw => Control::RepeatHead,
        Rule::loop_kw => Control::LoopHead,
        Rule::endloop_kw => Control::EndLoop,
        Rule::if_statement
        | Rule::loop_statement
        | Rule::for_statement
        | Rule::while_statement
        | Rule::repeat_until_statement => {
            return Err(cx.error(
                "AST lowering",
                format!(
                    "{:?} spans several lines; the statement AST takes its head and closer lines one at a time",
                    first.as_rule()
                ),
            ))
        }
        _ => return Err(cx.unexpected(&first, "a control statement")),
    };
    cx.done(&mut parts, "a control statement")?;
    Ok(control)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::{BinaryOp::*, UnaryOp::*};
    use std::collections::HashMap;

    fn state_for(text: &str) -> State {
        let mut state = State::new(
            vec!["X".to_string(), "Y".to_string(), "Z".to_string()],
            1000,
            None,
            false,
        );
        state.set_input(text);
        state
    }

    fn block(text: &str) -> Block {
        parse_line(text, 1, &state_for(text)).unwrap_or_else(|e| panic!("{text:?} does not lower: {e}"))
    }

    fn statements(text: &str) -> Vec<Statement> {
        match block(text).body {
            Body::Statements(statements) => statements,
            other => panic!("{text:?} is not a statement block: {other:?}"),
        }
    }

    fn control(text: &str) -> Control {
        match block(text).body {
            Body::Control(control) => control,
            other => panic!("{text:?} is not a control block: {other:?}"),
        }
    }

    /// The right-hand side of `R1=<text>`.
    fn rhs(text: &str) -> Expr {
        match statements(&format!("R1={text}")).as_slice() {
            [Statement::Assign(Assignment::Var {
                rhs: Rhs::Expr(expr), ..
            })] => expr.clone(),
            other => panic!("{text:?}: {other:?}"),
        }
    }

    fn n(value: f64) -> Expr {
        Expr::Number(value)
    }
    fn v(name: &str) -> Expr {
        Expr::Var(name.to_string())
    }
    fn bin(op: BinaryOp, lhs: Expr, rhs: Expr) -> Expr {
        Expr::Binary(op, Box::new(lhs), Box::new(rhs))
    }
    fn un(op: UnaryOp, operand: Expr) -> Expr {
        Expr::Unary(op, Box::new(operand))
    }
    fn paren(inner: Expr) -> Expr {
        Expr::Paren(Box::new(inner))
    }
    fn assign(name: &str, rhs: Rhs) -> Statement {
        Statement::Assign(Assignment::Var {
            name: name.to_string(),
            rhs,
        })
    }
    fn goto(kind: GotoKind, target: &str) -> Goto {
        Goto {
            kind,
            target: target.to_string(),
        }
    }

    #[test]
    fn multiplicative_binds_tighter_than_additive() {
        assert_eq!(rhs("A+B*C"), bin(Add, v("A"), bin(Mul, v("B"), v("C"))));
        assert_eq!(rhs("A*B-C"), bin(Sub, bin(Mul, v("A"), v("B")), v("C")));
        assert_eq!(rhs("A+B DIV C"), bin(Add, v("A"), bin(IntDiv, v("B"), v("C"))));
        assert_eq!(rhs("A-B MOD C"), bin(Sub, v("A"), bin(Mod, v("B"), v("C"))));
        assert_eq!(rhs("A+B/C"), bin(Add, v("A"), bin(Div, v("B"), v("C"))));
    }

    #[test]
    fn additive_binds_tighter_than_b_and() {
        assert_eq!(rhs("A B_AND B+C"), bin(BitAnd, v("A"), bin(Add, v("B"), v("C"))));
        assert_eq!(rhs("A-B B_AND C"), bin(BitAnd, bin(Sub, v("A"), v("B")), v("C")));
    }

    #[test]
    fn b_and_binds_tighter_than_b_xor() {
        assert_eq!(
            rhs("A B_XOR B B_AND C"),
            bin(BitXor, v("A"), bin(BitAnd, v("B"), v("C")))
        );
    }

    #[test]
    fn b_xor_binds_tighter_than_b_or() {
        assert_eq!(rhs("A B_OR B B_XOR C"), bin(BitOr, v("A"), bin(BitXor, v("B"), v("C"))));
    }

    #[test]
    fn b_or_binds_tighter_than_and() {
        assert_eq!(rhs("A AND B B_OR C"), bin(And, v("A"), bin(BitOr, v("B"), v("C"))));
    }

    #[test]
    fn and_binds_tighter_than_xor() {
        assert_eq!(rhs("A XOR B AND C"), bin(Xor, v("A"), bin(And, v("B"), v("C"))));
    }

    #[test]
    fn xor_binds_tighter_than_or() {
        assert_eq!(rhs("A OR B XOR C"), bin(Or, v("A"), bin(Xor, v("B"), v("C"))));
    }

    #[test]
    fn or_binds_tighter_than_comparisons() {
        // Comparisons have the lowest priority (manual 4.1.3.3), so this is A == (B OR C).
        assert_eq!(rhs("A == B OR C"), bin(Eq, v("A"), bin(Or, v("B"), v("C"))));
        assert_eq!(rhs("A<>B"), bin(Ne, v("A"), v("B")));
        for (text, op) in [("<", Lt), (">", Gt), ("<=", Le), (">=", Ge), ("==", Eq), ("<>", Ne)] {
            assert_eq!(rhs(&format!("A+1{text}B")), bin(op, bin(Add, v("A"), n(1.0)), v("B")));
        }
    }

    #[test]
    fn prefixes_bind_to_their_operand_only() {
        assert_eq!(rhs("-A*B"), bin(Mul, un(Neg, v("A")), v("B")));
        assert_eq!(rhs("NOT A AND B"), bin(And, un(Not, v("A")), v("B")));
        assert_eq!(rhs("NOT -A"), un(Not, un(Neg, v("A"))));
        assert_eq!(rhs("- -A"), un(Neg, un(Neg, v("A"))));
        assert_eq!(rhs("A*-B"), bin(Mul, v("A"), un(Neg, v("B"))));
        assert_eq!(rhs("NOT(A)"), un(Not, paren(v("A"))));
    }

    #[test]
    fn equal_priority_operators_associate_left() {
        assert_eq!(rhs("A-B-C"), bin(Sub, bin(Sub, v("A"), v("B")), v("C")));
        assert_eq!(rhs("A/B*C"), bin(Mul, bin(Div, v("A"), v("B")), v("C")));
        assert_eq!(rhs("A DIV B MOD C"), bin(Mod, bin(IntDiv, v("A"), v("B")), v("C")));
        assert_eq!(rhs("A<B<C"), bin(Lt, bin(Lt, v("A"), v("B")), v("C")));
        assert_eq!(rhs("A OR B OR C"), bin(Or, bin(Or, v("A"), v("B")), v("C")));
        assert_eq!(
            rhs("A B_AND B B_AND C"),
            bin(BitAnd, bin(BitAnd, v("A"), v("B")), v("C"))
        );
    }

    #[test]
    fn parentheses_are_kept() {
        assert_eq!(rhs("A-(B-C)"), bin(Sub, v("A"), paren(bin(Sub, v("B"), v("C")))));
        assert_eq!(rhs("(A)"), paren(v("A")));
    }

    #[test]
    fn literal_arithmetic_is_folded() {
        assert_eq!(rhs("1+2*3"), n(7.0));
        assert_eq!(rhs("2- -3"), n(5.0));
        assert_eq!(rhs("-2*3"), n(-6.0));
        assert_eq!(rhs("10/4/5"), n(0.5));
        assert_eq!(rhs("1+2+A"), bin(Add, n(3.0), v("A")));
        assert_eq!(rhs("A+1+2"), bin(Add, bin(Add, v("A"), n(1.0)), n(2.0)));
        // Only + - * / and unary minus fold; DIV can fail and NOT is not folded.
        assert_eq!(rhs("1 DIV 0"), bin(IntDiv, n(1.0), n(0.0)));
        assert_eq!(rhs("NOT 1"), un(Not, n(1.0)));
        assert_eq!(rhs("-(1)"), un(Neg, paren(n(1.0))));
    }

    #[test]
    fn operands_and_function_calls() {
        assert_eq!(rhs("$AC_TIMER"), v("$AC_TIMER"));
        assert_eq!(rhs("lower_case"), v("LOWER_CASE"));
        assert_eq!(
            rhs("$AA_IW[ x ]"),
            Expr::Element {
                name: "$AA_IW".to_string(),
                indices: vec![Index {
                    expr: v("X"),
                    text: "x".to_string()
                }],
            }
        );
        assert_eq!(
            rhs("Sin(30) + ATAN2(1, A)"),
            bin(
                Add,
                Expr::Call {
                    name: "Sin".to_string(),
                    args: vec![Arg::Expr(n(30.0))]
                },
                Expr::Call {
                    name: "ATAN2".to_string(),
                    args: vec![Arg::Expr(n(1.0)), Arg::Expr(v("A"))]
                }
            )
        );
        assert_eq!(
            rhs("MATCH(S, <<NAME)"),
            Expr::Call {
                name: "MATCH".to_string(),
                args: vec![
                    Arg::Expr(v("S")),
                    Arg::Str {
                        value: StringExpr::Concat(Operand::Expr(v("NAME")), vec![]),
                        text: "<<NAME".to_string()
                    }
                ]
            }
        );
        assert_eq!(
            rhs("INDEX(\"a b\", \" \")"),
            Expr::Call {
                name: "INDEX".to_string(),
                args: vec![
                    Arg::Str {
                        value: StringExpr::Literal("a b".to_string()),
                        text: "\"a b\"".to_string()
                    },
                    Arg::Str {
                        value: StringExpr::Literal(" ".to_string()),
                        text: "\" \"".to_string()
                    }
                ]
            }
        );
    }

    #[test]
    fn omitted_arguments_keep_their_positions() {
        let args = |text: &str| match rhs(text) {
            Expr::Call { args, .. } => args,
            other => panic!("{other:?}"),
        };
        assert_eq!(
            args("F(1,,3)"),
            vec![Arg::Expr(n(1.0)), Arg::Omitted, Arg::Expr(n(3.0))]
        );
        assert_eq!(args("F(,,2)"), vec![Arg::Omitted, Arg::Omitted, Arg::Expr(n(2.0))]);
        assert_eq!(
            args("F( , 2 , , 3)"),
            vec![Arg::Omitted, Arg::Expr(n(2.0)), Arg::Omitted, Arg::Expr(n(3.0))]
        );
        assert_eq!(args("F(G(1,,2),3)").len(), 2);
    }

    #[test]
    fn assignments_and_string_values() {
        assert_eq!(
            statements("X10 y=-2 E=IC(E_M*0.5) TXT1=\"txt\" TXT2=\"a\" << B TXT3=<<B TXT4=T"),
            vec![
                assign("X", Rhs::Expr(n(10.0))),
                assign("Y", Rhs::Expr(n(-2.0))),
                assign("E", Rhs::Increment(bin(Mul, v("E_M"), n(0.5)))),
                assign("TXT1", Rhs::Str(StringExpr::Literal("txt".to_string()))),
                assign(
                    "TXT2",
                    Rhs::Str(StringExpr::Concat(
                        Operand::Literal("a".to_string()),
                        vec![Operand::Expr(v("B"))]
                    ))
                ),
                assign("TXT3", Rhs::Str(StringExpr::Concat(Operand::Expr(v("B")), vec![]))),
                assign("TXT4", Rhs::Expr(v("T"))),
            ]
        );
        assert_eq!(
            statements("ARR[1, R1+1] = 5 STR[0]=\"c\""),
            vec![
                Statement::Assign(Assignment::Element {
                    name: "ARR".to_string(),
                    indices: vec![
                        Index {
                            expr: n(1.0),
                            text: "1".to_string()
                        },
                        Index {
                            expr: bin(Add, v("R1"), n(1.0)),
                            text: "R1+1".to_string()
                        },
                    ],
                    rhs: ElementRhs::Expr(n(5.0)),
                }),
                Statement::Assign(Assignment::Element {
                    name: "STR".to_string(),
                    indices: vec![Index {
                        expr: n(0.0),
                        text: "0".to_string()
                    }],
                    rhs: ElementRhs::Str(StringExpr::Literal("c".to_string())),
                }),
            ]
        );
        let index = |expr| Index {
            expr,
            text: "2".to_string(),
        };
        assert_eq!(
            statements("A[2]=SET(1,,3) B[2]=REP(7) C[2]=REP(7, 2)"),
            vec![
                Statement::AssignMulti(AssignMulti {
                    name: "A".to_string(),
                    indices: vec![index(n(2.0))],
                    values: MultiValues::Set(vec![Some(n(1.0)), None, Some(n(3.0))]),
                }),
                Statement::AssignMulti(AssignMulti {
                    name: "B".to_string(),
                    indices: vec![index(n(2.0))],
                    values: MultiValues::Rep {
                        value: n(7.0),
                        count: None
                    },
                }),
                Statement::AssignMulti(AssignMulti {
                    name: "C".to_string(),
                    indices: vec![index(n(2.0))],
                    values: MultiValues::Rep {
                        value: n(7.0),
                        count: Some(n(2.0))
                    },
                }),
            ]
        );
    }

    #[test]
    fn commands_and_calls() {
        assert_eq!(
            statements("g1 G17 GFRAME[2] m30 T=\"DRILL\" soft MY_SUB CYCLE(1,,3) Y2O G1 TRANS"),
            vec![
                Statement::G("G1".to_string()),
                Statement::G("G17".to_string()),
                Statement::G("GFRAME[2]".to_string()),
                Statement::M("M30".to_string()),
                Statement::Tool("DRILL".to_string()),
                Statement::G("SOFT".to_string()),
                Statement::Call(Call {
                    name: "MY_SUB".to_string(),
                    args: None,
                    text: "MY_SUB ".to_string()
                }),
                Statement::Call(Call {
                    name: "CYCLE".to_string(),
                    args: Some(vec![Arg::Expr(n(1.0)), Arg::Omitted, Arg::Expr(n(3.0))]),
                    text: "CYCLE(1,,3)".to_string()
                }),
                Statement::Call(Call {
                    name: "Y2O".to_string(),
                    args: None,
                    text: "Y2O ".to_string()
                }),
                Statement::G("G1".to_string()),
                // A frame keyword after another statement stays a call; the executor rejects it.
                Statement::Call(Call {
                    name: "TRANS".to_string(),
                    args: None,
                    text: "TRANS".to_string()
                }),
            ]
        );
        assert_eq!(
            statements("SOFT() NAME ( )"),
            vec![
                Statement::Call(Call {
                    name: "SOFT".to_string(),
                    args: Some(vec![]),
                    text: "SOFT()".to_string()
                }),
                Statement::Call(Call {
                    name: "NAME".to_string(),
                    args: Some(vec![]),
                    text: "NAME ( )".to_string()
                }),
            ]
        );
    }

    #[test]
    fn block_prefix_and_comment() {
        assert_eq!(
            block("N0010 start_1: X1 ; go"),
            Block {
                line_no: 1,
                block_number: Some("0010".to_string()),
                label: Some("START_1".to_string()),
                body: Body::Statements(vec![assign("X", Rhs::Expr(n(1.0)))]),
                comment: Some("; go".to_string()),
            }
        );
        assert_eq!(
            block("  "),
            Block {
                line_no: 1,
                block_number: None,
                label: None,
                body: Body::Empty,
                comment: None
            }
        );
        assert_eq!(block("LBL: ;x").label.as_deref(), Some("LBL"));
    }

    #[test]
    fn definitions() {
        let def = |text: &str| match block(text).body {
            Body::Definition(definition) => definition,
            other => panic!("{other:?}"),
        };
        assert_eq!(
            def("DEF REAL A, B=2, C[3], D[1]=SET(1,2)"),
            Definition {
                data_type: DataType::Real,
                items: vec![
                    DefItem::Var("A".to_string()),
                    DefItem::Init(Assignment::Var {
                        name: "B".to_string(),
                        rhs: Rhs::Expr(n(2.0))
                    }),
                    DefItem::Array {
                        name: "C".to_string(),
                        indices: vec![Index {
                            expr: n(3.0),
                            text: "3".to_string()
                        }]
                    },
                    DefItem::Multi(AssignMulti {
                        name: "D".to_string(),
                        indices: vec![Index {
                            expr: n(1.0),
                            text: "1".to_string()
                        }],
                        values: MultiValues::Set(vec![Some(n(1.0)), Some(n(2.0))]),
                    }),
                ],
            }
        );
        assert_eq!(def("def int I").data_type, DataType::Int);
        assert_eq!(def("DEF BOOL FLAG").data_type, DataType::Bool);
        assert_eq!(
            def("DEF STRING[32] S=\"x\"").data_type,
            DataType::String("32".to_string())
        );
    }

    #[test]
    fn frame_instructions() {
        assert_eq!(
            block("N5 AROT RPL=30").body,
            Body::FrameOp(FrameOp {
                kind: FrameKind::ARot,
                assignments: vec![Assignment::Var {
                    name: "RPL".to_string(),
                    rhs: Rhs::Expr(n(30.0))
                }],
            })
        );
        assert_eq!(
            block("trans").body,
            Body::FrameOp(FrameOp {
                kind: FrameKind::Trans,
                assignments: vec![]
            })
        );
        assert_eq!(
            block("TRANS X10 Y=R1").body,
            Body::FrameOp(FrameOp {
                kind: FrameKind::Trans,
                assignments: vec![
                    Assignment::Var {
                        name: "X".to_string(),
                        rhs: Rhs::Expr(n(10.0))
                    },
                    Assignment::Var {
                        name: "Y".to_string(),
                        rhs: Rhs::Expr(v("R1"))
                    },
                ],
            })
        );
    }

    #[test]
    fn single_line_control() {
        assert_eq!(control("GOTOS"), Control::GotoS);
        assert_eq!(control("gotof Skip_1"), Control::Goto(goto(GotoKind::GotoF, "Skip_1")));
        assert_eq!(control("GOTOB N200"), Control::Goto(goto(GotoKind::GotoB, "N200")));
        assert_eq!(control("GOTO 200"), Control::Goto(goto(GotoKind::Goto, "200")));
        assert_eq!(
            control("GOTOC MISSING"),
            Control::Goto(goto(GotoKind::GotoC, "MISSING"))
        );
        assert_eq!(
            control("IF R1>0 GOTOF A1 IF R2>0 GOTOB A2"),
            Control::IfGoto(vec![
                (bin(Gt, v("R1"), n(0.0)), goto(GotoKind::GotoF, "A1")),
                (bin(Gt, v("R2"), n(0.0)), goto(GotoKind::GotoB, "A2")),
            ])
        );
        assert_eq!(
            control("CASE(R1) OF 1 GOTOF L1 2.5 GOTOF L2 DEFAULT GOTOB L0"),
            Control::Case {
                selector: paren(v("R1")),
                arms: vec![(1.0, goto(GotoKind::GotoF, "L1")), (2.5, goto(GotoKind::GotoF, "L2"))],
                default: Some(goto(GotoKind::GotoB, "L0")),
            }
        );
    }

    #[test]
    fn line_level_structure_forms() {
        assert_eq!(control("IF R1==1"), Control::IfHead(bin(Eq, v("R1"), n(1.0))));
        assert_eq!(control("else"), Control::Else);
        assert_eq!(control("ENDIF"), Control::EndIf);
        assert_eq!(control("WHILE R1<5"), Control::WhileHead(bin(Lt, v("R1"), n(5.0))));
        assert_eq!(control("ENDWHILE"), Control::EndWhile);
        assert_eq!(
            control("FOR I=1 TO N_MAX"),
            Control::ForHead {
                assignment: Assignment::Var {
                    name: "I".to_string(),
                    rhs: Rhs::Expr(n(1.0))
                },
                to: v("N_MAX"),
            }
        );
        assert_eq!(control("ENDFOR"), Control::EndFor);
        assert_eq!(control("REPEAT"), Control::RepeatHead);
        assert_eq!(control("UNTIL $AA_IW[Z] > 5").clone(), {
            Control::UntilTail(bin(
                Gt,
                Expr::Element {
                    name: "$AA_IW".to_string(),
                    indices: vec![Index {
                        expr: v("Z"),
                        text: "Z".to_string(),
                    }],
                },
                n(5.0),
            ))
        });
        assert_eq!(control("LOOP"), Control::LoopHead);
        assert_eq!(control("ENDLOOP"), Control::EndLoop);
        let head = block("N10 LBL: WHILE R1 ; c");
        assert_eq!(head.block_number.as_deref(), Some("10"));
        assert_eq!(head.label.as_deref(), Some("LBL"));
        assert_eq!(head.comment.as_deref(), Some("; c"));
    }

    #[test]
    fn line_keywords_are_word_bounded() {
        assert!(matches!(statements("ELSEX2").as_slice(), [Statement::Call(_)]));
        assert!(matches!(statements("LOOPS").as_slice(), [Statement::Call(_)]));
        assert!(matches!(statements("ENDIF_FLAG=1").as_slice(), [Statement::Assign(_)]));
        assert!(matches!(statements("IF_FLAG=1").as_slice(), [Statement::Assign(_)]));
    }

    #[test]
    fn rejected_lines_are_parse_errors_with_line_context() {
        // Same-line structure bodies and a mid-line ELSE parse in the whole-file
        // grammar but not as one line; they must fail loudly, not run differently.
        for text in [
            "WHILE R1<2 X=R1",
            "LOOP X1",
            "REPEAT X1",
            "ELSE X2",
            "X1 ELSE",
            "FOR I=1 TO 3 X=I",
            "IF R1>0 GOTOF",
            "IF R1>0 X1",
            "ENDWHILE X1",
        ] {
            let error = parse_line(text, 7, &state_for(&format!("\n\n\n\n\n\n{text}"))).expect_err(text);
            assert_eq!(error.location().map(|l| l.line), Some(7), "{text}: {error}");
        }
    }

    #[test]
    fn unlowerable_pairs_are_errors_not_panics() {
        let program = "R1=1\nIF R1==1\nX1\nENDIF\n";
        let mut file = NCParser::parse(Rule::file, program).expect("parses");
        let blocks = file.next().expect("file").into_inner().next().expect("blocks");
        let if_block = blocks.into_inner().nth(1).expect("the IF block");
        let error = lower_block(if_block, 2).expect_err("a multi-line IF is not one block");
        assert_eq!(error.location().map(|l| l.line), Some(2));
        let expression = NCParser::parse(Rule::expression, "1+2")
            .expect("parses")
            .next()
            .expect("pair");
        assert!(lower_block(expression, 1).is_err());
    }

    #[test]
    fn sparse_line_blocks_give_one_block_per_line() {
        let sparse = "WHILE R1<3\n\nX=SIN(30)\n\nENDWHILE ; done\n";
        let parsed = NCParser::parse(Rule::sparse_line_blocks, sparse)
            .expect("parses")
            .next()
            .expect("program");
        let blocks: Vec<Block> = parsed
            .into_inner()
            .filter(|pair| pair.as_rule() == Rule::line_block)
            .map(|pair| {
                let line_no = pair.line_col().0;
                lower_block(pair, line_no).expect("lowers")
            })
            .collect();
        let lines: Vec<usize> = blocks.iter().map(|b| b.line_no).collect();
        assert_eq!(lines, vec![1, 3, 5]);
        assert_eq!(blocks[2].body, Body::Control(Control::EndWhile));
        assert_eq!(blocks[2].comment.as_deref(), Some("; done"));
    }

    #[test]
    fn every_example_line_lowers() {
        let mut failures = Vec::new();
        let mut lines = 0;
        for entry in std::fs::read_dir(concat!(env!("CARGO_MANIFEST_DIR"), "/examples")).expect("examples") {
            let path = entry.expect("entry").path();
            if path.extension().and_then(|e| e.to_str()) != Some("mpf") {
                continue;
            }
            let program = std::fs::read_to_string(&path).expect("readable");
            let state = state_for(&program);
            for (index, text) in program.lines().enumerate() {
                lines += 1;
                if let Err(error) = parse_line(text, index + 1, &state) {
                    failures.push(format!("{}:{}: {text}\n{error}", path.display(), index + 1));
                }
            }
        }
        assert!(lines > 400, "only {lines} example lines");
        assert!(failures.is_empty(), "{}", failures.join("\n"));
    }

    /// Deterministic generator for the evaluation cross-check.
    struct Lcg(u64);
    impl Lcg {
        fn below(&mut self, bound: usize) -> usize {
            self.0 = self
                .0
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            ((self.0 >> 33) % bound as u64) as usize
        }
    }

    const OPERATORS: &[&str] = &[
        "+", "-", "*", "/", "DIV", "MOD", "B_AND", "B_XOR", "B_OR", "AND", "XOR", "OR", "==", "<>", "<", ">", "<=",
        ">=",
    ];

    fn random_expression(rng: &mut Lcg, depth: usize) -> String {
        let mut text = random_operand(rng, depth);
        for _ in 0..rng.below(4) {
            text.push_str(&format!(" {} ", OPERATORS[rng.below(OPERATORS.len())]));
            text.push_str(&random_operand(rng, depth));
        }
        text
    }

    fn random_operand(rng: &mut Lcg, depth: usize) -> String {
        let prefix = ["", "", "", "-", "NOT ", "NOT -"][rng.below(6)];
        let core = match rng.below(if depth > 0 { 9 } else { 7 }) {
            0 => "0".to_string(),
            1 => "1".to_string(),
            2 => "2".to_string(),
            3 => "3.5".to_string(),
            4 => "VA".to_string(),
            5 => "VB".to_string(),
            6 => "VC".to_string(),
            _ => format!("({})", random_expression(rng, depth - 1)),
        };
        format!("{prefix}{core}")
    }

    /// A minimal evaluator of the operator subset, following evaluate_*; None
    /// where the tree walker raises an error.
    fn evaluate(expr: &Expr, vars: &HashMap<&str, f64>) -> Option<f64> {
        let truth = |b: bool| if b { 1.0 } else { 0.0 };
        let bits = |value: f64| {
            (value.is_finite() && value.fract() == 0.0 && value.abs() < 9_007_199_254_740_992.0).then_some(value as i64)
        };
        let equal = crate::interpret_rules::reals_equal;
        Some(match expr {
            Expr::Number(value) => *value,
            Expr::Var(name) => *vars.get(name.as_str())?,
            Expr::Paren(inner) => evaluate(inner, vars)?,
            Expr::Unary(Neg, inner) => -evaluate(inner, vars)?,
            Expr::Unary(Not, inner) => truth(evaluate(inner, vars)? == 0.0),
            Expr::Binary(op, lhs, rhs) => {
                let (a, b) = (evaluate(lhs, vars)?, evaluate(rhs, vars)?);
                match op {
                    Add => a + b,
                    Sub => a - b,
                    Mul => a * b,
                    Div => a / b,
                    IntDiv if b == 0.0 => return None,
                    IntDiv => (a / b).trunc(),
                    Mod => a % b,
                    BitAnd => (bits(a)? & bits(b)?) as f64,
                    BitXor => (bits(a)? ^ bits(b)?) as f64,
                    BitOr => (bits(a)? | bits(b)?) as f64,
                    And => truth(a != 0.0 && b != 0.0),
                    Xor => truth((a != 0.0) != (b != 0.0)),
                    Or => truth(a != 0.0 || b != 0.0),
                    Lt => truth(a < b && !equal(a, b)),
                    Gt => truth(a > b && !equal(a, b)),
                    Le => truth(a < b || equal(a, b)),
                    Ge => truth(a > b || equal(a, b)),
                    Eq => truth(equal(a, b)),
                    Ne => truth(!equal(a, b)),
                }
            }
            _ => return None,
        })
    }

    #[test]
    fn lowered_trees_evaluate_like_the_tree_walker() {
        let vars = HashMap::from([("VA", 2.0), ("VB", -3.0), ("VC", 0.25)]);
        let mut rng = Lcg(7);
        let mut evaluated = 0;
        for _ in 0..600 {
            let expression = random_expression(&mut rng, 2);
            let program = format!("VA=2\nVB=-3\nVC=0.25\nR1={expression}\n");
            let walker = crate::interpreter::nc_to_table(&program, None, None, None, 1000, false, None, false, None)
                .map(|(_, state)| state.symbol_table["R1"]);
            let tree = rhs(&expression);
            match (walker, evaluate(&tree, &vars)) {
                (Ok(expected), Some(actual)) => {
                    assert!(
                        expected.to_bits() == actual.to_bits() || (expected.is_nan() && actual.is_nan()),
                        "{expression}: tree walker {expected}, lowered tree {actual}\n{tree:?}"
                    );
                    evaluated += 1;
                }
                (Err(_), None) => {}
                (walker, actual) => panic!("{expression}: tree walker {walker:?}, lowered tree {actual:?}"),
            }
        }
        assert!(evaluated > 300, "only {evaluated} expressions evaluated");
    }
}
