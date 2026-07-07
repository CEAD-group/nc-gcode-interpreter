use crate::errors::ParsingError;
use crate::state::{ColKind, State};
use crate::types::Pair;
use crate::types::Rule;
use crate::types::Value;
use std::collections::HashMap;

type Output = crate::output::OutputRows;

/// Control-flow signal returned by block interpretation: either fall through
/// to the next block, or a pending GOTO that must be resolved against the
/// block list of the current scope or, failing that, an enclosing scope
/// (which is how a jump leaves an IF body or a LOOP).
#[derive(Debug, Clone)]
pub enum BlockFlow {
    Continue,
    Jump(JumpRequest),
    /// M2/M17/M30 executed: end of program. With jumps in play the end
    /// marker is not necessarily the last block (manual 4.1.5.2), so it must
    /// terminate interpretation instead of falling through to later blocks.
    EndProgram,
}

#[derive(Debug, Clone)]
pub struct JumpRequest {
    /// Canonical target key, comparable against `scan_jump_targets` keys.
    key: String,
    /// The destination as written in the program, for error messages.
    display: String,
    direction: JumpDirection,
    line_no: usize,
    preview: String,
}

impl JumpRequest {
    pub fn into_not_found_error(self, state: &State) -> ParsingError {
        // When the destination exists as written, "did you mean" would
        // absurdly suggest the very name the user typed; the actual problem
        // is the search direction or the scope it is defined in.
        let hint = if state.seen_jump_targets.contains(&self.key) {
            let reason = match self.direction {
                JumpDirection::Forward => {
                    "GOTOF only searches toward the end of the program; use GOTOB or GOTO for a destination that lies before this block"
                }
                JumpDirection::Backward => {
                    "GOTOB only searches toward the beginning of the program; use GOTOF or GOTO for a destination that lies after this block"
                }
                JumpDirection::BothForwardFirst => "a jump cannot enter an IF or loop body from outside",
            };
            format!(
                "\n'{}' is defined, but not where this jump searches: {}.",
                self.display, reason
            )
        } else {
            suggest_jump_target(&self.key, &state.seen_jump_targets)
                .map(|s| format!("\nDid you mean '{}'?", s))
                .unwrap_or_default()
        };
        ParsingError::JumpTargetNotFound {
            line_no: self.line_no,
            preview: self.preview,
            target: self.display,
            search_direction: self.direction.search_description().to_string(),
            hint,
        }
    }
}

/// Suggest the closest jump target of the same kind (label or block number)
/// within a small edit distance, for "did you mean" hints.
fn suggest_jump_target(key: &str, seen: &std::collections::HashSet<String>) -> Option<String> {
    let mut best: Option<(usize, &str)> = None;
    for candidate in seen {
        // Keys are prefixed "LABEL:"/"N:", so kinds can never cross-match
        // within distance 2 unless the names themselves are close.
        // Distance 0 (the target exists) is handled by the caller with a
        // direction/scope hint; suggesting the same name would be absurd.
        let distance = edit_distance(key, candidate);
        if (1..=2).contains(&distance) && best.map_or(true, |(d, _)| distance < d) {
            best = Some((distance, candidate));
        }
    }
    best.map(|(_, c)| {
        c.strip_prefix("LABEL:")
            .map(str::to_string)
            .or_else(|| c.strip_prefix("N:").map(|n| format!("N{}", n)))
            .unwrap_or_else(|| c.to_string())
    })
}

fn edit_distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut current = vec![0usize; b.len() + 1];
    for (i, &ca) in a.iter().enumerate() {
        current[0] = i + 1;
        for (j, &cb) in b.iter().enumerate() {
            let substitution = prev[j] + usize::from(ca != cb);
            current[j + 1] = substitution.min(prev[j + 1] + 1).min(current[j] + 1);
        }
        std::mem::swap(&mut prev, &mut current);
    }
    prev[b.len()]
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum JumpDirection {
    /// GOTOB: toward the beginning of the program.
    Backward,
    /// GOTOF: toward the end of the program.
    Forward,
    /// GOTO / GOTOC: first toward the end, then toward the beginning.
    BothForwardFirst,
}

impl JumpDirection {
    fn search_description(self) -> &'static str {
        match self {
            JumpDirection::Backward => "toward the beginning of the program (GOTOB)",
            JumpDirection::Forward => "toward the end of the program (GOTOF)",
            JumpDirection::BothForwardFirst => "in both directions (GOTO/GOTOC)",
        }
    }
}

/// Canonicalize a jump destination as written (`goto_target` lexeme) into a
/// key that matches `scan_jump_targets`. Labels are case-insensitive; block
/// numbers may be written as `200` or `N200` (manual 4.1.5.2) and are
/// normalized so `N020` and `20` compare equal.
pub(crate) fn canonical_jump_target(raw: &str) -> String {
    let trimmed = raw.trim();
    let digits = trimmed.strip_prefix(['N', 'n']).unwrap_or(trimmed);
    if !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()) {
        format!("N:{}", canonical_block_number(digits))
    } else {
        format!("LABEL:{}", trimmed.to_uppercase())
    }
}

pub(crate) fn canonical_block_number(digits: &str) -> &str {
    let stripped = digits.trim_start_matches('0');
    if stripped.is_empty() { "0" } else { stripped }
}

/// Collect the jump targets (labels and block numbers) defined by each block
/// of a scope, mapping the canonical key to the (ascending) block indices
/// where it is defined.
pub(crate) fn scan_jump_targets(block_pairs: &[Pair<Rule>]) -> HashMap<String, Vec<usize>> {
    let mut targets: HashMap<String, Vec<usize>> = HashMap::new();
    for (index, block) in block_pairs.iter().enumerate() {
        for item in block.clone().into_inner() {
            match item.as_rule() {
                Rule::block_number => {
                    let key = format!("N:{}", canonical_block_number(item.as_str().trim()));
                    targets.entry(key).or_default().push(index);
                }
                Rule::label_def => {
                    let name = item
                        .into_inner()
                        .next()
                        .expect("label_def always contains a label_name")
                        .as_str()
                        .to_uppercase();
                    targets.entry(format!("LABEL:{}", name)).or_default().push(index);
                }
                // Block numbers and labels can only appear at the start of a
                // block; stop peeking once the payload begins.
                _ => break,
            }
        }
    }
    targets
}

/// Resolve a jump request against the targets of one scope: GOTOB searches
/// backward from the current block (inclusive), GOTOF forward (exclusive),
/// GOTO/GOTOC forward first and then backward. Returns the destination block
/// index, or None when the target is not reachable in this scope.
pub(crate) fn resolve_jump(
    targets: &HashMap<String, Vec<usize>>,
    current: usize,
    request: &JumpRequest,
) -> Option<usize> {
    let positions = targets.get(&request.key)?;
    let forward = || positions.iter().copied().find(|&p| p > current);
    let backward = || positions.iter().copied().rev().find(|&p| p <= current);
    match request.direction {
        JumpDirection::Forward => forward(),
        JumpDirection::Backward => backward(),
        JumpDirection::BothForwardFirst => forward().or_else(backward),
    }
}

/// The frame instruction family, normally captured by the frame_op grammar
/// rule at block start. Also present in G-group 3, where they can only be
/// reached when they FOLLOW another statement in the block - which is invalid
/// (frame instructions must be alone in the block) and rejected loudly.
pub(crate) const FRAME_KEYWORDS: &[&str] = &[
    "TRANS", "ATRANS", "SCALE", "ASCALE", "ROT", "AROT", "ROTS", "AROTS", "CROTS", "MIRROR", "AMIRROR",
];
fn interpret_primary(primary: Pair<Rule>, state: &mut State) -> Result<f64, ParsingError> {
    let inner_pair = primary.into_inner().next().expect("Error");
    match inner_pair.as_rule() {
        Rule::value => inner_pair.as_str().parse::<f64>().map_err(|_| {
            annotate_error(
                &inner_pair,
                "numeric literal",
                format!("'{}' is not a valid number", inner_pair.as_str()),
                state,
            )
        }),
        Rule::variable => {
            let (line_no, preview) = get_error_context(&inner_pair, state);
            interpret_variable(inner_pair, state).and_then(|key| {
                if let Some(value) = state.symbol_table.get(&key).cloned() {
                    Ok(value)
                } else if state.allow_undefined_variables {
                    crate::state::emit_warning(format_args!("Warning: Variable '{}' is undefined, initializing to 0.0", key));
                    state.symbol_table.insert(key, 0.0);
                    Ok(0.0)
                } else {
                    Err(ParsingError::UndefinedVariable { 
                        line_no,
                        preview,
                        name: key 
                    })
                }
            })
        },
        Rule::variable_array => {
            let (line_no, preview) = get_error_context(&inner_pair, state);
            interpret_variable_array(inner_pair, state).and_then(|keys| {
                let key = &keys[keys.len() - 1];
                if let Some(value) = state.symbol_table.get(key).cloned() {
                    Ok(value)
                } else if state.allow_undefined_variables {
                    crate::state::emit_warning(format_args!("Warning: Variable array element '{}' is undefined, initializing to 0.0", key));
                    state.symbol_table.insert(key.clone(), 0.0);
                    Ok(0.0)
                } else {
                    Err(ParsingError::UnknownVariable {
                        line_no,
                        preview,
                        variable: key.clone(),
                    })
                }
            })
        },
        Rule::expression => evaluate_expression(inner_pair, state),
        Rule::arith_fun => evaluate_arithmetic_function(inner_pair, state),
        _ => {
            let (line_no, preview) = get_error_context(&inner_pair, state);
            Err(ParsingError::UnexpectedRule {
                rule: inner_pair.as_rule(),
                context: "interpret_primary".to_string(),
                line_no,
                preview,
                message: format!("Unexpected rule in interpret_primary: {:?}", inner_pair.as_rule()),
            })
        }
    }
}
fn evaluate_arithmetic_function(pair: Pair<Rule>, state: &mut State) -> Result<f64, ParsingError> {
    let (line_no, preview) = get_error_context(&pair, state);
    let mut pairs = pair.into_inner();
    
    // Get function name
    let func_name = pairs.next()
        .ok_or_else(|| ParsingError::ParsingContext {
            line_no,
            preview: preview.clone(),
            context: "function evaluation".to_string(),
            message: "Missing function name".to_string(),
        })?;
    
    // Get arguments pair
    let args_pair = pairs.next()
        .ok_or_else(|| ParsingError::ParsingContext {
            line_no,
            preview: preview.clone(),
            context: "function evaluation".to_string(),
            message: "Missing function arguments".to_string(),
        })?;
    
    // Parse arguments
    let mut args = Vec::new();
    for arg in args_pair.into_inner() {
        if arg.as_rule() == Rule::expression {
            args.push(evaluate_expression(arg, state)?);
        }
    }

    // Helper to validate argument count
    let check_args = |expected: usize| -> Result<(), ParsingError> {
        if args.len() != expected {
            return Err(ParsingError::InvalidFunctionArity {
                line_no,
                preview: preview.clone(),
                name: func_name.as_str().to_string(),
                expected,
                actual: args.len(),
            });
        }
        Ok(())
    };

    // Apply the function.
    // Sinumerik trigonometric functions work in degrees, not radians
    // (NC programming manual, "Operators and arithmetic functions").
    // Names are case-insensitive like the rest of the NC language.
    match func_name.as_str().to_uppercase().as_str() {
        "SIN" => {
            check_args(1)?;
            Ok(args[0].to_radians().sin())
        },
        "COS" => {
            check_args(1)?;
            Ok(args[0].to_radians().cos())
        },
        "TAN" => {
            check_args(1)?;
            Ok(args[0].to_radians().tan())
        },
        "ASIN" => {
            check_args(1)?;
            Ok(args[0].asin().to_degrees())
        },
        "ACOS" => {
            check_args(1)?;
            Ok(args[0].acos().to_degrees())
        },
        "ATAN2" => {
            check_args(2)?;
            // ATAN2(a, b): angle of the vector sum of two perpendicular vectors,
            // in degrees (-180..180]. The angular reference is the SECOND value,
            // so e.g. ATAN2(30.5, 80.1) = 20.8455 (manual 4.1.3.1).
            Ok(args[0].atan2(args[1]).to_degrees())
        },
        "SQRT" => {
            check_args(1)?;
            Ok(args[0].sqrt())
        },
        "ABS" => {
            check_args(1)?;
            Ok(args[0].abs())
        },
        "POT" => {
            check_args(1)?;
            Ok(args[0].powi(2))
        },
        "TRUNC" => {
            check_args(1)?;
            Ok(args[0].trunc())
        },
        "ROUND" => {
            check_args(1)?;
            Ok(args[0].round())
        },
        "ROUNDUP" => {
            // Round up to the next higher integer (manual 4.1.3.5).
            check_args(1)?;
            Ok(args[0].ceil())
        },
        "MINVAL" => {
            check_args(2)?;
            Ok(args[0].min(args[1]))
        },
        "MAXVAL" => {
            check_args(2)?;
            Ok(args[0].max(args[1]))
        },
        "BOUND" => {
            // BOUND(<minimum>, <maximum>, <check value>): the check value
            // bounded to [minimum, maximum] (manual 4.1.1.13).
            check_args(3)?;
            let (min, max, value) = (args[0], args[1], args[2]);
            if min > max {
                return Err(ParsingError::ParsingContext {
                    line_no,
                    preview,
                    context: "BOUND".to_string(),
                    message: format!("BOUND minimum ({}) is greater than maximum ({})", min, max),
                });
            }
            Ok(value.clamp(min, max))
        },
        "LN" => {
            check_args(1)?;
            Ok(args[0].ln())
        },
        "EXP" => {
            check_args(1)?;
            Ok(args[0].exp())
        },
        other => Err(ParsingError::ParsingContext {
            line_no,
            preview,
            context: "expression".to_string(),
            message: format!("'{}' is not a known arithmetic function", other),
        }),
    }
}
/// Evaluate an expression with the operator priorities of the Sinumerik NC
/// language: *, /, DIV and MOD bind more strongly than + and -; operators of
/// equal priority evaluate left to right; unary minus binds most strongly.
fn evaluate_expression(expression: Pair<Rule>, state: &mut State) -> Result<f64, ParsingError> {
    let pairs: Vec<Pair<Rule>> = expression.into_inner().collect();
    let mut pos = 0;
    let value = evaluate_comparison(&pairs, &mut pos, state)?;
    if let Some(pair) = pairs.get(pos) {
        let (line_no, preview) = get_error_context(pair, state);
        return Err(ParsingError::UnexpectedRule {
            rule: pair.as_rule(),
            context: "evaluate_expression".to_string(),
            line_no,
            preview,
            message: format!("Unexpected trailing rule in expression: {:?}", pair.as_rule()),
        });
    }
    Ok(value)
}

/// Operator tiers below additive, from the manual's priority table
/// (4.1.3.3, highest to lowest): ... 4. B_AND, 5. B_XOR, 6. B_OR, 7. AND,
/// 8. XOR, 9. OR, 11. comparisons. Same-priority operators evaluate left to
/// right; truth semantics are 0 = FALSE, anything else = TRUE (4.1.3.2).
/// All tiers produce f64 (BOOL results as 1.0/0.0, like the control's
/// implicit type conversion).
fn evaluate_comparison(pairs: &[Pair<Rule>], pos: &mut usize, state: &mut State) -> Result<f64, ParsingError> {
    let mut lhs = evaluate_or(pairs, pos, state)?;
    while let Some(pair) = pairs.get(*pos) {
        if pair.as_rule() != Rule::relational_operator {
            break;
        }
        let operator = pair.clone();
        *pos += 1;
        let rhs = evaluate_or(pairs, pos, state)?;
        lhs = evaluate_relational_operator(operator, lhs, rhs)? as u8 as f64;
    }
    Ok(lhs)
}

/// Validate a bit-by-bit operand: the manual restricts B_AND/B_XOR/B_OR to
/// CHAR/INT (with automatic conversion); a fractional, non-finite or
/// out-of-range REAL has no defined bit pattern, and `as i64` would silently
/// truncate/saturate it - loud error instead.
fn bit_operand(value: f64, operator: &Pair<Rule>, state: &State) -> Result<i64, ParsingError> {
    if !value.is_finite() || value.fract() != 0.0 || value.abs() >= 9_007_199_254_740_992.0 {
        let (line_no, preview) = get_error_context(operator, state);
        return Err(ParsingError::with_context(
            line_no,
            preview,
            "bit-by-bit operator".to_string(),
            format!(
                "{} requires integer operands (manual 4.1.3.2: types CHAR/INT), got {value}",
                operator.as_str()
            ),
        ));
    }
    Ok(value as i64)
}

fn evaluate_or(pairs: &[Pair<Rule>], pos: &mut usize, state: &mut State) -> Result<f64, ParsingError> {
    let mut lhs = evaluate_xor(pairs, pos, state)?;
    while matches!(pairs.get(*pos).map(|p| p.as_rule()), Some(Rule::op_or)) {
        *pos += 1;
        let rhs = evaluate_xor(pairs, pos, state)?;
        lhs = ((lhs != 0.0) || (rhs != 0.0)) as u8 as f64;
    }
    Ok(lhs)
}

fn evaluate_xor(pairs: &[Pair<Rule>], pos: &mut usize, state: &mut State) -> Result<f64, ParsingError> {
    let mut lhs = evaluate_and(pairs, pos, state)?;
    while matches!(pairs.get(*pos).map(|p| p.as_rule()), Some(Rule::op_xor)) {
        *pos += 1;
        let rhs = evaluate_and(pairs, pos, state)?;
        lhs = ((lhs != 0.0) != (rhs != 0.0)) as u8 as f64;
    }
    Ok(lhs)
}

fn evaluate_and(pairs: &[Pair<Rule>], pos: &mut usize, state: &mut State) -> Result<f64, ParsingError> {
    let mut lhs = evaluate_b_or(pairs, pos, state)?;
    while matches!(pairs.get(*pos).map(|p| p.as_rule()), Some(Rule::op_and)) {
        *pos += 1;
        let rhs = evaluate_b_or(pairs, pos, state)?;
        lhs = ((lhs != 0.0) && (rhs != 0.0)) as u8 as f64;
    }
    Ok(lhs)
}

fn evaluate_b_or(pairs: &[Pair<Rule>], pos: &mut usize, state: &mut State) -> Result<f64, ParsingError> {
    let mut lhs = evaluate_b_xor(pairs, pos, state)?;
    while matches!(pairs.get(*pos).map(|p| p.as_rule()), Some(Rule::op_b_or)) {
        let operator = pairs[*pos].clone();
        *pos += 1;
        let rhs = evaluate_b_xor(pairs, pos, state)?;
        lhs = (bit_operand(lhs, &operator, state)? | bit_operand(rhs, &operator, state)?) as f64;
    }
    Ok(lhs)
}

fn evaluate_b_xor(pairs: &[Pair<Rule>], pos: &mut usize, state: &mut State) -> Result<f64, ParsingError> {
    let mut lhs = evaluate_b_and(pairs, pos, state)?;
    while matches!(pairs.get(*pos).map(|p| p.as_rule()), Some(Rule::op_b_xor)) {
        let operator = pairs[*pos].clone();
        *pos += 1;
        let rhs = evaluate_b_and(pairs, pos, state)?;
        lhs = (bit_operand(lhs, &operator, state)? ^ bit_operand(rhs, &operator, state)?) as f64;
    }
    Ok(lhs)
}

fn evaluate_b_and(pairs: &[Pair<Rule>], pos: &mut usize, state: &mut State) -> Result<f64, ParsingError> {
    let mut lhs = evaluate_additive(pairs, pos, state)?;
    while matches!(pairs.get(*pos).map(|p| p.as_rule()), Some(Rule::op_b_and)) {
        let operator = pairs[*pos].clone();
        *pos += 1;
        let rhs = evaluate_additive(pairs, pos, state)?;
        lhs = (bit_operand(lhs, &operator, state)? & bit_operand(rhs, &operator, state)?) as f64;
    }
    Ok(lhs)
}

fn evaluate_additive(pairs: &[Pair<Rule>], pos: &mut usize, state: &mut State) -> Result<f64, ParsingError> {
    let mut lhs = evaluate_multiplicative(pairs, pos, state)?;
    while let Some(pair) = pairs.get(*pos) {
        match pair.as_rule() {
            Rule::op_add => {
                *pos += 1;
                lhs += evaluate_multiplicative(pairs, pos, state)?;
            }
            Rule::op_sub => {
                *pos += 1;
                lhs -= evaluate_multiplicative(pairs, pos, state)?;
            }
            _ => break,
        }
    }
    Ok(lhs)
}

fn evaluate_multiplicative(pairs: &[Pair<Rule>], pos: &mut usize, state: &mut State) -> Result<f64, ParsingError> {
    let mut lhs = evaluate_unary(pairs, pos, state)?;
    while let Some(pair) = pairs.get(*pos) {
        match pair.as_rule() {
            Rule::op_mul => {
                *pos += 1;
                lhs *= evaluate_unary(pairs, pos, state)?;
            }
            Rule::op_div => {
                *pos += 1;
                lhs /= evaluate_unary(pairs, pos, state)?;
            }
            Rule::op_int_div => {
                let (line_no, preview) = get_error_context(pair, state);
                *pos += 1;
                let rhs = evaluate_unary(pairs, pos, state)?;
                if rhs == 0.0 {
                    return Err(ParsingError::ParsingContext {
                        line_no,
                        preview,
                        context: "integer division".to_string(),
                        message: "Integer division (DIV) by zero".to_string(),
                    });
                }
                // DIV divides the REAL operands and truncates the result
                // (manual 4.1.3.1: 7 DIV 4.1 = 1); operands are NOT
                // truncated first.
                lhs = (lhs / rhs).trunc();
            }
            Rule::op_mod => {
                *pos += 1;
                lhs %= evaluate_unary(pairs, pos, state)?;
            }
            _ => break,
        }
    }
    Ok(lhs)
}

fn evaluate_unary(pairs: &[Pair<Rule>], pos: &mut usize, state: &mut State) -> Result<f64, ParsingError> {
    let mut prefixes: Vec<Rule> = Vec::new();
    while let Some(rule) = pairs
        .get(*pos)
        .map(|p| p.as_rule())
        .filter(|r| matches!(r, Rule::neg | Rule::not_op))
    {
        prefixes.push(rule);
        *pos += 1;
    }
    let pair = pairs.get(*pos).ok_or_else(|| {
        let (line_no, preview) = pairs
            .last()
            .map(|p| get_error_context(p, state))
            .unwrap_or((0, "(could not retrieve line)".to_string()));
        ParsingError::ParsingContext {
            line_no,
            preview,
            context: "evaluate_unary".to_string(),
            message: "Expected an operand at end of expression".to_string(),
        }
    })?;
    if pair.as_rule() != Rule::primary {
        let (line_no, preview) = get_error_context(pair, state);
        return Err(ParsingError::UnexpectedRule {
            rule: pair.as_rule(),
            context: "evaluate_unary".to_string(),
            line_no,
            preview,
            message: format!("Expected an operand, found {:?}", pair.as_rule()),
        });
    }
    *pos += 1;
    let mut value = interpret_primary(pair.clone(), state)?;
    // Prefixes apply innermost-first: in `NOT -X` the negation happens
    // before the logical NOT, so fold over them right to left.
    for applied in prefixes.iter().rev() {
        value = match applied {
            Rule::neg => -value,
            Rule::not_op => {
                if value == 0.0 {
                    1.0
                } else {
                    0.0
                }
            }
            _ => unreachable!("prefix loop only collects neg/not_op"),
        };
    }
    Ok(value)
}
/// Classify a lexically G-shaped command (`G<digits>` or `GFRAME[<n>]`)
/// against the vocabulary table: the column key is the G-group name, the
/// value the command as written. An unknown G number is a hard error, like
/// alarm 12470 "undefined G function" on a real control.
fn interpret_g_command(g_command: Pair<Rule>, state: &State) -> Result<(&'static str, String), ParsingError> {
    let command_str = g_command.as_str().trim().to_string();
    match crate::modal_groups::classify_g_command(&command_str.to_uppercase()) {
        Some((group, _modal)) => Ok((group, command_str)),
        None => {
            let (line_no, preview) = get_error_context(&g_command, state);
            Err(ParsingError::UnknownGCommand {
                line_no,
                preview,
                code: command_str.to_uppercase(),
            })
        }
    }
}
fn interpret_m_command(m_command: Pair<Rule>) -> (String, String) {
    // Log the interpretd M command for debugging
    //println!("Parsed M command: {:?}", m_command);

    // Initially, set the command string to the entire M command (e.g., "M3")
    // the parser should ommit trailing spaces, however, if there are any, remove them
    let command_str = m_command.as_str().trim_end().to_string();

    // Return the tuple with the rule name as the column header and the specific M command as the value
    ("M".to_string(), command_str)
}
fn interpret_tool_selection(
    tool_selection: Pair<Rule>,
    output: &mut Output,
    _state: &mut State,
) -> Result<(), ParsingError> {
    // Get the last HashMap from the output vector to insert the tool selection.
    let last = output.last_mut().expect("Output vector should not be empty");

    // Since `tool_selection` = { ^"T" ~ "=" ~ quoted_string }
    // and `quoted_string` is silent, the `tool_selection` pair will directly contain the `string` pairs.
    let mut tool_name = String::new();

    // Iterate over the inner pairs to collect the strings.
    for pair in tool_selection.into_inner() {
        match pair.as_rule() {
            Rule::string => {
                tool_name.push_str(pair.as_str());
            }
            _ => {
                // Handle unexpected rules.
                return Err(ParsingError::UnexpectedRule {
                    rule: pair.as_rule(),
                    context: "interpret_tool_selection".to_string(),
                    line_no: pair.line_col().0,
                    preview: String::from("(state not available)"),
                    message: format!("Unexpected rule in interpret_tool_selection: {:?}", pair.as_rule()),
                });
            }
        }
    }

    // Insert the tool name into the output.
    last.insert("T", Value::Str(tool_name));

    Ok(())
}
fn interpret_non_returning_function_call(function_call: Pair<Rule>) -> (&'static str, String) {
    // Log the interpretd function call for debugging
    //println!("Parsed function call: {:?}", function_call);

    let command_str = function_call.as_str().to_string();

    // Return the tuple with the rule name as the column header and the specific function call as the value
    ("non_returning_function_call", command_str)
}

/// Axis and block-address names are case-insensitive; normalize them to
/// uppercase so state lookups and output columns are consistent regardless
/// of the case used in the program (`x=10` must hit the same axis, column and
/// translation as `X=10`; the bare word form `X10` is uppercase-only in the
/// grammar).
fn normalize_reserved_case(key: String, state: &State) -> String {
    if state.is_axis(&key) || state.is_block_address(&key) {
        key.to_uppercase()
    } else {
        key
    }
}

fn interpret_assignment(element: Pair<Rule>, state: &mut State) -> Result<(String, f64), ParsingError> {
    let mut inner_pairs = element.into_inner();

    let variable_pair = inner_pairs
        .next()
        .ok_or_else(|| ParsingError::InvalidElementCount { expected: 2, actual: 0 })?;

    let expression_pair = inner_pairs
        .next()
        .ok_or_else(|| ParsingError::InvalidElementCount { expected: 2, actual: 1 })?;

    // All axis values are now stored as LOCAL coordinates.
    // Translation is applied at output time, not storage time.
    let (key, local_value) = match (variable_pair.as_rule(), expression_pair.as_rule()) {
        (Rule::variable_single_char, Rule::value) => {
            let key = variable_pair.as_str().to_string();
            let value = expression_pair.as_str().parse::<f64>().map_err(|_| {
                annotate_error(
                    &expression_pair,
                    "numeric literal",
                    format!("'{}' is not a valid number", expression_pair.as_str()),
                    state,
                )
            })?;
            (key, value)
        }
        (Rule::variable, Rule::axis_increment) => {
            let key = normalize_reserved_case(interpret_variable(variable_pair.clone(), state)?, state);
            let value = interpret_axis_increment(expression_pair, state, key.clone())?;
            (key, value)
        }
        (Rule::variable, Rule::expression) => {
            let key = normalize_reserved_case(interpret_variable(variable_pair.clone(), state)?, state);
            let value = evaluate_expression(expression_pair, state)?;
            (key, value)
        }
        (Rule::variable_array, Rule::expression) => {
            let keys = interpret_variable_array(variable_pair, state)?;
            let value = evaluate_expression(expression_pair, state)?;
            (keys[keys.len() - 1].clone(), value)
        }
        _ => {
            return Err(ParsingError::UnexpectedRule {
                rule: expression_pair.as_rule(),
                context: "interpret_assignment".to_string(),
                line_no: expression_pair.line_col().0,
                preview: state.get_line(expression_pair.line_col().0).unwrap_or("").to_string(),
                message: format!("Unexpected rule in interpret_assignment: {:?}", expression_pair.as_rule()),
            })
        }
    };

    if state.is_axis(&key) {
        state.update_axis(&key, local_value)?;
    } else if state.is_block_address(&key) {
        // Block addresses (e.g. spline PW/SD/PL) only appear in the output row;
        // they are neither axes nor user variables, so nothing is stored.
    } else {
        state.symbol_table.insert(key.clone(), local_value);
    }

    Ok((key, local_value))
}
fn interpret_axis_increment(pair: Pair<Rule>, state: &mut State, key: String) -> Result<f64, ParsingError> {
    // axis_increment = { "IC" ~ "(" ~ expression ~ ")" }
    // Returns the new LOCAL coordinate. Since axes now store local coordinates,
    // we simply add the increment to the current local value.
    // Note: when the frame changed since the previous move, the machine-space
    // delta is increment + frame change. This matches the control's factory
    // default SD42440 $SC_FRAME_OFFSET_INCR_PROG = TRUE ("changes to work
    // offsets are traversed after a frame change"); machines configured with
    // FALSE traverse the pure increment instead, which is not modeled here.
    let pair_clone = pair.clone();
    let inner_pair = pair.into_inner().next().expect("Expected an expression inside axis_increment, found none");
    if inner_pair.as_rule() != Rule::expression {
        return Err(ParsingError::UnexpectedRule {
            rule: inner_pair.as_rule(),
            context: "interpret_axis_increment::axis_increment".to_string(),
            line_no: pair_clone.line_col().0,
            preview: state.get_line(pair_clone.line_col().0).unwrap_or("").to_string(),
            message: format!("Unexpected rule in interpret_axis_increment: {:?}", inner_pair.as_rule()),
        });
    }
    let increment = evaluate_expression(inner_pair, state)?;
    match state.get_axis_local(&key) {
        Some(local_coord) => {
            // Add increment to current local coordinate
            Ok(local_coord + increment)
        },
        None => {
            crate::state::emit_warning(format_args!(
                "Warning: The axis '{}' is incremented before a fixed value is set, the G-code behavior may be indeterminate.",
                key
            ));
            Ok(increment)
        }
    }
}
/// Returns each target key with the value assigned to it, or `None` for
/// keys a gap in the value array left untouched.
fn interpret_assignment_multi(element: Pair<Rule>, state: &mut State) -> Result<Vec<(String, Option<f64>)>, ParsingError> {
    // assignment_multi =  { variable_array ~ "=" ~ (value_array | value_repeating) }
    let mut inner_pairs = element.into_inner();
    let variable_pair = inner_pairs
        .next()
        .ok_or(ParsingError::InvalidElementCount { expected: 2, actual: 0 })?;
    let keys = interpret_variable_array(variable_pair, state)?;
    let value_pair = inner_pairs
        .next()
        .ok_or(ParsingError::InvalidElementCount { expected: 2, actual: 1 })?;

    let values = interpret_value_array(value_pair, state)?;
    if values.len() > keys.len() {
        return Err(ParsingError::InvalidElementCount {
            expected: keys.len(),
            actual: values.len(),
        });
    }
    Ok(keys
        .into_iter()
        .enumerate()
        .map(|(i, key)| {
            let value = values.get(i).copied().flatten();
            if let Some(value) = value {
                state.symbol_table.insert(key.clone(), value);
            }
            (key, value)
        })
        .collect())
}
fn interpret_value_array(pair: Pair<Rule>, state: &mut State) -> Result<Vec<Option<f64>>, ParsingError> {
    let mut values = Vec::new();
    for inner in pair.into_inner() {
        match inner.as_rule() {
            Rule::expression => {
                let value = evaluate_expression(inner, state)?;
                values.push(Some(value));
            }
            Rule::value_none => {
                values.push(None);
            }
            _ => {
                return Err(ParsingError::UnexpectedRule {
                    rule: inner.as_rule(),
                    context: "interpret_value_array".to_string(),
                    line_no: inner.line_col().0,
                    preview: state.get_line(inner.line_col().0).unwrap_or("").to_string(),
                    message: format!("Unexpected rule in interpret_value_array: {:?}", inner.as_rule()),
                })
            }
        }
    }

    Ok(values)
}
fn interpret_variable(pair: Pair<Rule>, state: &State) -> Result<String, ParsingError> {
    let inner = pair.clone().into_inner().next()
        .ok_or_else(|| annotate_error(&pair, "variable parsing", 
            "Expected inner pair, found none".to_string(), state))?;
    match inner.as_rule() {
        // A plain user variable, or a `$`-prefixed system variable such as
        // `$AC_TIMER`. There is no system-variable model, so system variables
        // are keyed and stored like ordinary variables by their full name.
        Rule::identifier | Rule::nc_variable => Ok(inner.as_str().to_string()),
        _ => Err(annotate_error(&pair, "variable parsing",
            format!("Expected identifier, found '{:?}'", inner.as_rule()), state)),
    }
}
fn interpret_variable_array(inner: Pair<Rule>, state: &mut State) -> Result<Vec<String>, ParsingError> {
    // variable_array = { (nc_variable | identifier) ~ "[" ~ indices ~ "]" }
    let mut inner_pairs = inner.into_inner();
    let identifier = interpret_identifier(inner_pairs.next().expect("Expected an identifier"))?;
    let indices_pair = inner_pairs.next().expect("Expected indices");
    let indices = interpret_indices(indices_pair, state)?;

    // Generate each variable name in the array based on the indices length
    let mut variable_names = Vec::new();
    match indices.len() {
        1 => {
            for i in 0..(indices[0] as i32 + 1) {
                variable_names.push(format!("{}[{}]", identifier, i));
            }
        }
        2 => {
            for i in 0..(indices[0] as i32 + 1) {
                for j in 0..(indices[1] as i32 + 1) {
                    variable_names.push(format!("{}[{},{}]", identifier, i, j));
                }
            }
        }
        3 => {
            for i in 0..(indices[0] as i32 + 1) {
                for j in 0..(indices[1] as i32 + 1) {
                    for k in 0..(indices[2] as i32 + 1) {
                        variable_names.push(format!("{}[{},{},{}]", identifier, i, j, k));
                    }
                }
            }
        }
        _ => {
            return Err(ParsingError::ParseError {
                message: "Invalid number of indices for variable array".to_string(),
            })
        }
    }
    Ok(variable_names)
}
fn interpret_indices(pair: Pair<Rule>, state: &mut State) -> Result<Vec<f64>, ParsingError> {
    let mut indices = Vec::new();
    // Get error context before consuming pair
    let (pair_line_no, pair_preview) = get_error_context(&pair, state);
    
    for inner in pair.into_inner() {
        match inner.as_rule() {
            Rule::expression => {
                // Try to resolve axis identifier to index if possible
                let expr_str = inner.as_str().trim().to_string();
                
                if state.is_axis(&expr_str) {
                    let (line_no, preview) = get_error_context(&inner, state);
                    let index = state.get_axis_index(&expr_str, line_no, &preview)?;
                    indices.push(index as f64);
                } else {
                    let value = evaluate_expression(inner, state)?;
                    // Validate the index value
                    if value < 0.0 || value.fract() != 0.0 {
                        return Err(ParsingError::InvalidAxisIndex {
                            line_no: pair_line_no,
                            preview: pair_preview,
                            axis: expr_str,
                            index: value as usize,
                        });
                    }
                    indices.push(value);
                }
            }
            _ => {
                let (line_no, preview) = get_error_context(&inner, state);
                return Err(ParsingError::UnexpectedRule {
                    rule: inner.as_rule(),
                    context: "array index expression".to_string(),
                    line_no,
                    preview,
                    message: "Expected a valid array index expression".to_string(),
                });
            }
        }
    }
    Ok(indices)
}
fn interpret_identifier(pair: Pair<Rule>) -> Result<String, ParsingError> {
    let line_no = pair.line_col().0;
    let preview = pair.as_str().to_string();
    
    // Accept both plain identifiers and `$`-prefixed system variables (e.g.
    // `$AC_TIMER[1]`); the latter are stored by their full name like ordinary
    // variables, since there is no dedicated system-variable model.
    if pair.as_rule() == Rule::identifier || pair.as_rule() == Rule::nc_variable {
        Ok(pair.as_str().to_string())
    } else {
        Err(ParsingError::ParsingContext {
            line_no,
            preview,
            context: "identifier parsing".to_string(),
            message: format!("Found '{:?}' but expected an identifier", pair.as_rule()),
        })
    }
}
fn interpret_definition(element: Pair<Rule>, output: &mut Output, state: &mut State) -> Result<(), ParsingError> {
    let pairs = element.into_inner();
    for pair in pairs {
        match pair.as_rule() {
            Rule::assignment => {
                let res = interpret_assignment(pair, state)?;
                if state.is_axis(res.0.as_str()) {
                    return Err(ParsingError::AxisUsedAsVariable { name: res.0 });
                } else if state.is_block_address(res.0.as_str()) {
                    return Err(ParsingError::ReservedNameUsedAsVariable { name: res.0 });
                }
                output.record_variable_change(&res.0, res.1);
            }
            Rule::assignment_multi => {
                for (key, value) in interpret_assignment_multi(pair, state)? {
                    if let Some(value) = value {
                        output.record_variable_change(&key, value);
                    }
                }
            }
            Rule::variable => {
                let key = interpret_variable(pair, state)?;
                if state.is_axis(&key) {
                    return Err(ParsingError::AxisUsedAsVariable { name: key });
                } else if state.is_block_address(&key) {
                    return Err(ParsingError::ReservedNameUsedAsVariable { name: key });
                }
                state.symbol_table.insert(key.clone(), 0.0);
                output.record_variable_change(&key, 0.0);
            }
            Rule::variable_array => {
                let keys = interpret_variable_array(pair, state)?;
                for key in keys {
                    state.symbol_table.insert(key.clone(), 0.0);
                    output.record_variable_change(&key, 0.0);
                }
            }
            Rule::data_type => {
                // Ignore the type definition, as we are treating all variables as f64
            }
            _ => Err(ParsingError::UnexpectedRule {
                rule: pair.as_rule(),
                context: "interpret_definition".to_string(),
                line_no: pair.line_col().0,
                preview: state.get_line(pair.line_col().0).unwrap_or("").to_string(),
                message: format!("Unexpected rule in interpret_definition: {:?}", pair.as_rule()),
            })?,
        }
    }
    Ok(())
}
fn evaluate_condition(condition: Pair<Rule>, state: &mut State) -> Result<bool, ParsingError> {
    assert_eq!(
        condition.as_rule(),
        Rule::condition,
        "Expected condition pair to be of type Rule::condition"
    );

    // condition = { expression }: comparisons and logic operators are
    // ordinary expression operators; TRUE is any non-zero value (4.1.3.2).
    let inner_elements: Vec<Pair<Rule>> = condition.into_inner().collect();
    match inner_elements.as_slice() {
        [expression] => {
            let result = evaluate_expression(expression.clone(), state)?;
            Ok(result != 0.0)
        }
        _ => Err(ParsingError::InvalidCondition),
    }
}
/// Sinumerik REAL comparisons check for relative rather than absolute
/// equality, with a relative tolerance of 10^-12 (NC programming manual,
/// "Precision correction on comparison errors (TRUNC)"). This applies to
/// ==, <>, <= and >=, and by default also excludes relatively-equal values
/// from < and >.
const REAL_EQUALITY_RELATIVE_TOLERANCE: f64 = 1e-12;

fn reals_equal(lhs: f64, rhs: f64) -> bool {
    lhs == rhs || (lhs - rhs).abs() <= REAL_EQUALITY_RELATIVE_TOLERANCE * lhs.abs().max(rhs.abs())
}

fn evaluate_relational_operator(operator: Pair<Rule>, lhs: f64, rhs: f64) -> Result<bool, ParsingError> {
    match operator.as_str() {
        "<" => Ok(lhs < rhs && !reals_equal(lhs, rhs)),
        ">" => Ok(lhs > rhs && !reals_equal(lhs, rhs)),
        "==" => Ok(reals_equal(lhs, rhs)),
        "<>" => Ok(!reals_equal(lhs, rhs)),
        "<=" => Ok(lhs < rhs || reals_equal(lhs, rhs)),
        ">=" => Ok(lhs > rhs || reals_equal(lhs, rhs)),
        _ => Err(ParsingError::UnexpectedOperator {
            operator: operator.as_str().to_string(),
        }),
    }
}
fn interpret_statement_if(
    element: Pair<Rule>,
    output: &mut Output,
    state: &mut State,
) -> Result<BlockFlow, ParsingError> {
    let mut pairs = element.into_inner();

    // Match the condition
    let condition = pairs.next().ok_or_else(|| ParsingError::InvalidElementCount {
        expected: 1,
        actual: 0,
    })?;
    if condition.as_rule() != Rule::condition {
        return Err(ParsingError::UnexpectedRule {
            rule: condition.as_rule(),
            context: "interpret_statement_if".to_string(),
            line_no: condition.line_col().0,
            preview: state.get_line(condition.line_col().0).unwrap_or("").to_string(),
            message: format!("Unexpected rule in interpret_statement_if: {:?}", condition.as_rule()),
        });
    }

    // Optionally match a comment
    let mut comment: Option<Pair<Rule>> = None;
    if let Some(next_pair) = pairs.peek() {
        if next_pair.as_rule() == Rule::comment {
            comment = Some(pairs.next().unwrap());
        }
    }

    // Match the true block
    let true_block = pairs.next().ok_or_else(|| ParsingError::InvalidElementCount {
        expected: 1,
        actual: 0,
    })?;
    if true_block.as_rule() != Rule::blocks {
        return Err(ParsingError::UnexpectedRule {
            rule: true_block.as_rule(),
            context: "interpret_statement_if".to_string(),
            line_no: true_block.line_col().0,
            preview: state.get_line(true_block.line_col().0).unwrap_or("").to_string(),
            message: format!("Unexpected rule in interpret_statement_if: {:?}", true_block.as_rule()),
        });
    }

    // Optionally match the false block
    let false_block = if let Some(next_pair) = pairs.next() {
        if next_pair.as_rule() == Rule::blocks {
            Some(next_pair)
        } else {
            return Err(ParsingError::UnexpectedRule {
                rule: next_pair.as_rule(),
                context: "interpret_statement_if::else".to_string(),
                line_no: next_pair.line_col().0,
                preview: state.get_line(next_pair.line_col().0).unwrap_or("").to_string(),
                message: format!("Unexpected rule in else block: {:?}", next_pair.as_rule()),
            });
        }
    } else {
        None
    };

    // Ensure no extra rules are present
    if pairs.next().is_some() {
        return Err(ParsingError::InvalidElementCount {
            expected: 3,
            actual: 4,
        });
    }

    // Handle the comment
    if let Some(comment_pair) = comment {
        let last = output.last_mut().expect("Output vector should not be empty");
        last.insert("comment", Value::Str(comment_pair.as_str().to_string()));
    }

    // Evaluate the condition and execute the appropriate block. A jump out
    // of either branch propagates to the enclosing scope.
    if evaluate_condition(condition, state)? {
        interpret_blocks(true_block, output, state)
    } else if let Some(false_block) = false_block {
        interpret_blocks(false_block, output, state)
    } else {
        Ok(BlockFlow::Continue)
    }
}
fn interpret_statement_while(
    element: Pair<Rule>,
    output: &mut Output,
    state: &mut State,
) -> Result<BlockFlow, ParsingError> {
    let mut pairs = element.into_inner();
    let condition = pairs.next().expect("Expected a pair, got none");
    let blocks = pairs.next().expect("Expected a pair, got none");
    let mut loop_count = 0;
    while evaluate_condition(condition.clone(), state)? && loop_count < state.iteration_limit {
        loop_count += 1;
        match interpret_blocks(blocks.clone(), output, state)? {
            BlockFlow::Continue => {}
            // A jump leaves the loop (resolving in an enclosing scope), and
            // an executed end-of-program M code stops it outright.
            other => return Ok(other),
        }
    }
    if loop_count >= state.iteration_limit {
        return Err(ParsingError::LoopLimit {
            limit: state.iteration_limit.to_string(),
        });
    }
    Ok(BlockFlow::Continue)
}
/// LOOP ... ENDLOOP: an endless loop that can only be left with a jump out
/// of its body (manual 4.1.7.2); without one the iteration limit trips.
fn interpret_statement_loop(
    element: Pair<Rule>,
    output: &mut Output,
    state: &mut State,
) -> Result<BlockFlow, ParsingError> {
    let mut pairs = element.into_inner();
    let blocks = pairs.next().expect("Expected a pair, got none");
    let mut loop_count = 0;
    loop {
        match interpret_blocks(blocks.clone(), output, state)? {
            BlockFlow::Continue => {}
            other => return Ok(other),
        }
        loop_count += 1;
        if loop_count >= state.iteration_limit {
            return Err(ParsingError::LoopLimit {
                limit: state.iteration_limit.to_string(),
            });
        }
    }
}
fn interpret_statement_for(
    element: Pair<Rule>,
    output: &mut Output,
    state: &mut State,
) -> Result<BlockFlow, ParsingError> {
    let mut pairs = element.into_inner();

    // Parse and execute the assignment statement
    let assignment = pairs.next().expect("Expected an assignment, got none");
    let (variable_name, initial_value) = interpret_assignment(assignment, state)?;
    output.record_variable_change(&variable_name, initial_value);

    // Evaluate the TO expression to determine the loop's end value
    let to_expression = pairs.next().expect("Expected a TO expression, got none");
    let end_value = evaluate_expression(to_expression, state)?;

    // Retrieve the blocks to execute within the loop
    let blocks = pairs.next().expect("Expected blocks, got none");

    // Loop control
    while let Some(&current_value) = state.symbol_table.get(&variable_name) {
        if current_value > end_value {
            break; // Exit loop if current value exceeds end value
        }

        // Parse and execute the blocks
        match interpret_blocks(blocks.clone(), output, state)? {
            BlockFlow::Continue => {}
            // A jump leaves the loop (resolving in an enclosing scope), and
            // an executed end-of-program M code stops it outright.
            other => return Ok(other),
        }

        // After parsing blocks, increment the loop control variable
        // Ensure this is done in a separate scope to avoid mutable borrow conflict
        {
            let loop_control_value = state
                .symbol_table
                .get_mut(&variable_name)
                .expect("Variable should exist");
            *loop_control_value += 1.0; // Increment value directly
            let new_value = *loop_control_value;
            // Recorded on the row current at this point: the ENDFOR line,
            // whose (otherwise empty) block is the loop body's last row -
            // exactly where the control performs the increment.
            output.record_variable_change(&variable_name, new_value);
        }
    }
    Ok(BlockFlow::Continue)
}
fn interpret_statement_repeat_until(
    element: Pair<Rule>,
    output: &mut Output,
    state: &mut State,
) -> Result<BlockFlow, ParsingError> {
    let mut pairs = element.into_inner();
    let first_pair = pairs.next().expect("Expected a pair, got none");
    let blocks;
    match first_pair.as_rule() {
        Rule::comment => {
            let last = output.last_mut().expect("Output vector should not be empty");
            last.insert("comment", Value::Str(first_pair.as_str().to_string()));

            // The next rule are the block
            match pairs.next().expect("Expected a pair, got none").as_rule() {
                Rule::blocks => {
                    blocks = first_pair;
                }
                _ => {
                    return Err(ParsingError::UnexpectedRule {
                        rule: first_pair.as_rule(),
                        context: "interpret_statement_repeat_until".to_string(),
                        line_no: first_pair.line_col().0,
                        preview: state.get_line(first_pair.line_col().0).unwrap_or("").to_string(),
                        message: format!("Unexpected rule in interpret_statement_repeat_until: {:?}", first_pair.as_rule()),
                    });
                }
            }
        }
        Rule::blocks => {
            blocks = first_pair;
        }
        _ => {
            return Err(ParsingError::UnexpectedRule {
                rule: first_pair.as_rule(),
                context: "interpret_statement_repeat_until".to_string(),
                line_no: first_pair.line_col().0,
                preview: state.get_line(first_pair.line_col().0).unwrap_or("").to_string(),
                message: format!("Unexpected rule in interpret_statement_repeat_until: {:?}", first_pair.as_rule()),
            });
        }
    }
    let condition = pairs.next().expect("Expected condition, got none");
    let mut loop_count = 0;
    loop {
        match interpret_blocks(blocks.clone(), output, state)? {
            BlockFlow::Continue => {}
            // A jump leaves the loop (resolving in an enclosing scope), and
            // an executed end-of-program M code stops it outright.
            other => return Ok(other),
        }
        loop_count += 1;
        if loop_count >= state.iteration_limit {
            return Err(ParsingError::LoopLimit {
                limit: state.iteration_limit.to_string(),
            });
        }
        if evaluate_condition(condition.clone(), state)? {
            break;
        }
    }
    Ok(BlockFlow::Continue)
}
/// Interpret an unconditional jump statement into a pending jump request.
/// GOTOC is the exception: when its destination does not exist anywhere on
/// the active scope chain, the alarm is suppressed and execution simply
/// continues with the next block (manual 4.1.5.2).
fn interpret_goto(pair: Pair<Rule>, state: &State) -> Result<BlockFlow, ParsingError> {
    let (line_no, preview) = get_error_context(&pair, state);
    let mut pairs = pair.into_inner();
    let keyword_pair = pairs.next().expect("goto_statement starts with its keyword");
    let keyword = keyword_pair.as_str().to_uppercase();
    let target_pair = pairs.next().expect("goto_statement contains a goto_target");
    let display = target_pair.as_str().trim().to_string();
    let key = canonical_jump_target(&display);

    let direction = match keyword.as_str() {
        "GOTOB" => JumpDirection::Backward,
        "GOTOF" => JumpDirection::Forward,
        "GOTO" | "GOTOC" => JumpDirection::BothForwardFirst,
        other => {
            return Err(ParsingError::ParsingContext {
                line_no,
                preview,
                context: "jump statement".to_string(),
                message: format!("Unexpected jump keyword '{}'", other),
            })
        }
    };

    if keyword == "GOTOC" && !state.jump_target_visible(&key) {
        crate::state::emit_warning(format_args!(
            "Warning: GOTOC destination '{}' not found; alarm suppressed, continuing with the next block (line {})",
            display, line_no
        ));
        return Ok(BlockFlow::Continue);
    }

    Ok(BlockFlow::Jump(JumpRequest {
        key,
        display,
        direction,
        line_no,
        preview,
    }))
}

/// IF <condition> GOTO... <target>: single-block conditional jump.
fn interpret_if_goto(pair: Pair<Rule>, state: &mut State) -> Result<BlockFlow, ParsingError> {
    let mut pairs = pair.into_inner();
    let condition = pairs.next().expect("if_goto_statement starts with a condition");
    let goto = pairs.next().expect("if_goto_statement contains a goto_statement");
    if evaluate_condition(condition, state)? {
        interpret_goto(goto, state)
    } else {
        Ok(BlockFlow::Continue)
    }
}

/// CASE(<expr>) OF <const> GOTO... <target> ... DEFAULT GOTO... <target>:
/// jump to the arm whose constant equals the expression; without a matching
/// arm the DEFAULT applies, and without a DEFAULT execution falls through to
/// the next block (manual 4.1.5.3).
fn interpret_case(pair: Pair<Rule>, state: &mut State) -> Result<BlockFlow, ParsingError> {
    let mut pairs = pair.into_inner();
    // Skip the atomic case_kw pair preceding the expression.
    let expression = pairs
        .find(|p| p.as_rule() == Rule::expression)
        .expect("case_statement contains an expression");
    let value = evaluate_expression(expression, state)?;

    for arm in pairs {
        match arm.as_rule() {
            Rule::case_arm => {
                let mut arm_pairs = arm.clone().into_inner();
                let constant_pair = arm_pairs.next().expect("case_arm starts with a value");
                let constant = constant_pair.as_str().trim().parse::<f64>().map_err(|_| {
                    annotate_error(
                        &constant_pair,
                        "CASE constant",
                        format!("'{}' is not a valid number", constant_pair.as_str()),
                        state,
                    )
                })?;
                if reals_equal(value, constant) {
                    let goto = arm_pairs.next().expect("case_arm contains a goto_statement");
                    return interpret_goto(goto, state);
                }
            }
            Rule::case_default => {
                // Skip the atomic default_kw pair preceding the jump.
                let goto = arm
                    .into_inner()
                    .find(|p| p.as_rule() == Rule::goto_statement)
                    .expect("case_default contains a goto_statement");
                return interpret_goto(goto, state);
            }
            // The atomic keyword pairs (of_kw etc.) carry no content.
            _ => {}
        }
    }
    Ok(BlockFlow::Continue)
}

fn interpret_control(
    element: Pair<Rule>,
    output: &mut Output,
    state: &mut State,
) -> Result<BlockFlow, ParsingError> {
    // A control element is a single statement, except for conditional jumps
    // where several may share one block; the first satisfied jump wins.
    for pair in element.into_inner() {
        let flow = match pair.as_rule() {
            Rule::if_statement => interpret_statement_if(pair, output, state)?,
            Rule::for_statement => interpret_statement_for(pair, output, state)?,
            Rule::while_statement => interpret_statement_while(pair, output, state)?,
            Rule::repeat_until_statement => interpret_statement_repeat_until(pair, output, state)?,
            Rule::loop_statement => interpret_statement_loop(pair, output, state)?,
            Rule::goto_statement => interpret_goto(pair, state)?,
            Rule::if_goto_statement => interpret_if_goto(pair, state)?,
            Rule::case_statement => interpret_case(pair, state)?,
            Rule::gotos_statement => {
                // GOTOS repeats the program only when the PLC requests it via
                // <Chan>.basic.out.enableGoToStart; without that request the
                // control continues with the next block (Basic Functions
                // manual 3.5.10.1). An offline interpreter has no PLC, and a
                // restart would produce an unbounded trace, so the
                // no-request behavior is modeled.
                let (line_no, _) = get_error_context(&pair, state);
                crate::state::emit_warning(format_args!(
                    "Warning: GOTOS ignored (line {}): the program restart depends on the PLC signal enableGoToStart; continuing with the next block",
                    line_no
                ));
                BlockFlow::Continue
            }
            _ => {
                return Err(annotate_error(
                    &pair,
                    "control statement",
                    format!("Unexpected rule in interpret_control: {:?}", pair.as_rule()),
                    state,
                ))
            }
        };
        match flow {
            BlockFlow::Continue => {}
            other => return Ok(other),
        }
    }
    Ok(BlockFlow::Continue)
}
pub(crate) fn insert_m_key(last: &mut crate::output::CellMap, value: &str, line_no: usize, preview: String) -> Result<(), ParsingError> {
    let m_key = "M";
    for _i in 1..=5 {
        if let Some(existing_value) = last.get_mut(m_key) {
            // If the key already exists and is a list, append the new value
            if let Value::StrList(ref mut vec) = existing_value {
                if vec.len() < 5 {
                    vec.push(value.to_string());
                    return Ok(()); // Successfully added to the list
                }
            } else {
                // If the key exists but is not a list, return an error
                return Err(ParsingError::ParseError {
                    message: format!("M command key '{}' is not a list", m_key),
                });
            }
        } else {
            // If the key doesn't exist, insert a new StrList with the first value
            last.insert(m_key, Value::StrList(vec![value.to_string()]));
            return Ok(()); // Exit early after insertion
        }
    }
    Err(ParsingError::TooManyMCommands {
        line_no,
        preview,
        message: "Too many M commands in a single block".to_string(),
    })
}
/// M codes that end the program: M2/M02 and M30 end a main program, M17 a
/// subprogram. Execution of one of these stops interpretation; the rest of
/// the containing block still executes.
pub(crate) fn is_end_of_program_m_code(code: &str) -> bool {
    let digits = code.trim_start_matches(['M', 'm']);
    matches!(digits.trim_start_matches('0'), "2" | "17" | "30")
}

/// True for words that look like an axis word with a typo or a missing
/// space: a letter directly followed by a digit, with more letters later
/// (Y2O, X10Y20). Legitimate subprogram names rarely have this shape.
pub(crate) fn looks_like_axis_word_typo(name: &str) -> bool {
    let chars: Vec<char> = name.chars().collect();
    chars.len() >= 3
        && chars[0].is_ascii_alphabetic()
        && chars[1].is_ascii_digit()
        && chars[2..].iter().any(|c| c.is_ascii_alphabetic())
}

fn interpret_statement(
    element: Pair<Rule>,
    output: &mut Output,
    state: &mut State,
) -> Result<BlockFlow, ParsingError> {
    // Grammar:
    // statement           =  {
    //     g_command_numbered
    //   | m_command
    //   | assignment_multi
    //   | assignment
    //   | g_command
    //   | non_returning_function_call
    //   | tool_selection
    // }

    let mut flow = BlockFlow::Continue;
    for statement in element.into_inner() {
        let last = output.last_mut().expect("Output vector should not be empty");
        match statement.as_rule() {
            Rule::non_returning_function_call => {
                let (line_no, preview) = get_error_context(&statement, state);
                let (key, value) = interpret_non_returning_function_call(statement);
                let name = value.split('(').next().unwrap_or("").trim();
                if !value.contains('(') {
                    // Frame instructions are only interpreted as frame_op at
                    // the start of a block ("alone in the block" per manual
                    // 3.12.2.1). Reaching this point means one followed
                    // another statement, and e.g. `G1 MIRROR X0` would
                    // silently move X to 0. Error loudly - independent of the
                    // G-vocabulary table, which does not carry every frame
                    // keyword (e.g. CROTS).
                    if FRAME_KEYWORDS.iter().any(|kw| kw.eq_ignore_ascii_case(name)) {
                        return Err(ParsingError::UnsupportedStatement {
                            line_no,
                            preview,
                            statement: format!("The frame instruction {} after another statement", name),
                            hint: "Frame instructions must be programmed in a separate NC block \
                                   (manual 3.12.2.1)."
                                .to_string(),
                        });
                    }
                    // Keyword-shaped G commands (POLY, BSPLINE, SOFT, CP, ...)
                    // parse as bare words; the vocabulary table decides
                    // whether this is a G command or a subprogram call.
                    if let Some((group, _modal)) = crate::modal_groups::classify_g_command(&name.to_uppercase()) {
                        // `value` may carry trailing whitespace from the
                        // backtracked optional argument list; store the
                        // trimmed word like the g_command path does.
                        last.insert(group, Value::Str(name.to_string()));
                        continue;
                    }
                    // A parenless word like Y2O or X10Y20 is far more likely
                    // a mistyped axis word than a subprogram call; the
                    // toolpath would silently lose a move. Warn, but keep the
                    // call semantics: only the user can tell.
                    if looks_like_axis_word_typo(name) {
                        crate::state::emit_warning(format_args!(
                            "Warning: '{}' (line {}) is interpreted as a subprogram call; did you mean one or more axis words (e.g. Y20)?",
                            name, line_no
                        ));
                    }
                }
                last.insert(key, Value::Str(value));
            }
            Rule::g_command => {
                let (key, value) = interpret_g_command(statement, state)?;
                last.insert(key, Value::Str(value));
            }
            Rule::g_command_numbered => {
                let inner = statement.into_inner().next().expect("Error parsing g_command_numbered");
                let (key, value) = interpret_g_command(inner, state)?;
                last.insert(key, Value::Str(value));
            }
            Rule::m_command => {
                let (line_no, preview) = get_error_context(&statement, state);
                let (_key, value) = interpret_m_command(statement);
                // there are 5 M codes allowed in a block. Store them in separate columns in the output
                insert_m_key(last, &value, line_no, preview)?;
                // The whole block still executes; interpretation stops after it.
                if is_end_of_program_m_code(&value) {
                    flow = BlockFlow::EndProgram;
                }
            }
            // axis_word is the hoisted fast-path form of assignment's first
            // alternative; both carry (variable_single_char, value) inners.
            Rule::assignment | Rule::axis_word => {
                let (key, local_value) = interpret_assignment(statement, state)?;
                match state.resolve_output_key(&key) {
                    Some((ColKind::Axis, skey)) => {
                        // State keeps local coordinates; the output row gets the machine
                        // coordinate under the translation active at this point in the program.
                        let machine_value = state.get_axis_machine(skey).unwrap_or(local_value);
                        last.insert(skey, Value::Float(machine_value));
                    }
                    Some((ColKind::Block, skey)) => {
                        last.insert(skey, Value::Float(local_value));
                    }
                    None => {
                        output.record_variable_change(&key, local_value);
                    }
                }
            }
            Rule::tool_selection => interpret_tool_selection(statement, output, state)?,
            _ => Err(ParsingError::UnexpectedRule {
                rule: statement.as_rule(),
                context: "interpret_statement".to_string(),
                line_no: statement.line_col().0,
                preview: state.get_line(statement.line_col().0).unwrap_or("").to_string(),
                message: format!("Unexpected rule in interpret_statement: {:?}", statement.as_rule()),
            })?,
        }
    }
    Ok(flow)
}
/// Evaluate the assignments of a frame instruction without moving any axis:
/// the axis state is saved and restored around parsing, and each assignment
/// must target a valid axis.
fn frame_assignments(
    pairs: Vec<Pair<Rule>>,
    state: &mut State,
) -> Result<Vec<(String, f64)>, ParsingError> {
    let mut result = Vec::with_capacity(pairs.len());
    // Save the axis state once for the whole instruction; interpret_assignment
    // mutates it as a side effect and frame instructions must not move axes.
    let saved_axes = state.axes.clone();
    for pair in pairs {
        let (key, value) = interpret_assignment(pair, state)?;
        if !state.is_axis(&key) {
            state.axes = saved_axes;
            return Err(ParsingError::UnexpectedAxis {
                axis: key,
                axes: state.axis_identifiers.join(", "),
            });
        }
        result.push((key, value));
    }
    // Undo the axis-position side effects of interpret_assignment
    state.axes = saved_axes;
    Ok(result)
}

fn interpret_frame_op(element: Pair<Rule>, state: &mut State) -> Result<(), ParsingError> {
    let (line_no, preview) = get_error_context(&element, state);
    let mut pairs = element.into_inner();
    let kw = pairs.next().expect("frame_op must start with a frame keyword");
    let op = kw.as_str().to_uppercase();
    let assignments: Vec<Pair<Rule>> = pairs.collect();

    match op.as_str() {
        "TRANS" => {
            // TRANS is a substituting frame instruction: it deletes ALL
            // previously programmed frame components, including offsets on
            // axes not mentioned in this block (manual 3.12.2.2, and the
            // Notice "Absolute frame instructions delete all programmed
            // frames"). Bare TRANS is therefore just the reset.
            state.reset_translations();
            for (key, value) in frame_assignments(assignments, state)? {
                state.update_translation(&key, value)?;
            }
            Ok(())
        }
        "ATRANS" => {
            for (key, value) in frame_assignments(assignments, state)? {
                let current_translation = state.get_translation(&key);
                state.update_translation(&key, current_translation + value)?;
            }
            Ok(())
        }
        // Rotation, scaling and mirroring change the geometry in ways this
        // interpreter does not model, so anything with parameters must fail
        // loudly instead of producing wrong coordinates. The bare forms are
        // frame resets: a bare ABSOLUTE instruction (ROT/ROTS/CROTS/SCALE/
        // MIRROR) deletes the whole programmable frame including the
        // translation (manual 3.12.2.1), while a bare ADDITIVE instruction
        // adds nothing and is a no-op.
        _ => {
            if assignments.is_empty() {
                if !op.starts_with('A') {
                    // Bare absolute frame instruction: delete the programmable
                    // frame. (CROTS is absolute despite the leading C.)
                    state.reset_translations();
                }
                Ok(())
            } else {
                Err(ParsingError::UnsupportedStatement {
                    line_no,
                    preview,
                    statement: format!("The frame instruction {}", op),
                    hint: "Rotation, scaling and mirroring frames are not modeled; interpreting \
                           this program would produce wrong coordinates. Only TRANS and ATRANS \
                           are supported."
                        .to_string(),
                })
            }
        }
    }
}
fn interpret_block_number(element: Pair<Rule>, output: &mut Output) {
    let mut pairs = element.into_inner();
    let pair = pairs.next().expect("Expected a pair, got none");

    let last = output.last_mut().expect("Output vector should not be empty");
    // The grammar guarantees an integer token; keep the original lexeme so
    // large block numbers survive without float round-tripping.
    let value = match pair.as_rule() {
        Rule::integer => pair.as_str().to_string(),
        _ => panic!("Unexpected rule: {:?}", pair.as_rule()),
    };
    last.insert("N", Value::Str(value));
}
fn get_error_context(pair: &Pair<Rule>, state: &State) -> (usize, String) {
    let (line_no, _) = pair.line_col();
    let preview = state.get_line(line_no).unwrap_or("(could not retrieve line)").to_string();
    (line_no, preview)
}

fn annotate_error(pair: &Pair<Rule>, context: &str, message: String, state: &State) -> ParsingError {
    let (line_no, preview) = get_error_context(pair, state);
    ParsingError::with_context(
        line_no,
        preview,
        context.to_string(),
        message,
    )
}

pub(crate) fn interpret_block(
    element: Pair<Rule>,
    output: &mut Output,
    state: &mut State,
) -> Result<BlockFlow, ParsingError> {
    match element.as_rule() {
        Rule::block => {
            // Start this block's output row, flushing the previous one.
            output.start_row(element.line_col().0)?;

            let mut flow = BlockFlow::Continue;
            for item in element.into_inner() {
                match item.as_rule() {
                    Rule::statement => {
                        if let BlockFlow::EndProgram = interpret_statement(item, output, state)? {
                            flow = BlockFlow::EndProgram;
                        }
                    }
                    Rule::block_number => interpret_block_number(item, output),
                    // Jump labels are collected by scan_jump_targets before
                    // execution; at execution time they are inert.
                    Rule::label_def => {}
                    Rule::control => flow = interpret_control(item, output, state)?,
                    Rule::definition => interpret_definition(item, output, state)?,
                    Rule::frame_op => interpret_frame_op(item, state)?,
                    Rule::comment => {
                        let last = output.last_mut().expect("Output vector should not be empty");
                        last.insert("comment", Value::Str(item.as_str().to_string()));
                    },
                    _ => return Err(annotate_error(&item, "block interpretation",
                        format!("Unexpected rule: {:?}", item.as_rule()), state)),
                }
            }
            Ok(flow)
        }
        _ => {
            return Err(annotate_error(&element, "blocks interpretation",
                format!("Expected blocks, found {:?}", element.as_rule()), state));
        }
    }
}

pub fn interpret_blocks(
    blocks: Pair<Rule>,
    output: &mut Output,
    state: &mut State,
) -> Result<BlockFlow, ParsingError> {
    if blocks.as_rule() != Rule::blocks {
        return Err(annotate_error(&blocks, "blocks interpretation",
            format!("Expected blocks, found {:?}", blocks.as_rule()), state));
    }
    let block_pairs: Vec<Pair<Rule>> = blocks.into_inner().collect();
    let targets = scan_jump_targets(&block_pairs);
    state.seen_jump_targets.extend(targets.keys().cloned());
    state.jump_scopes.push(targets.keys().cloned().collect());
    let result = run_blocks(&block_pairs, &targets, output, state);
    state.jump_scopes.pop();
    result
}

/// Execute the blocks of one scope in order, resolving jumps against the
/// scope's own labels and block numbers. A jump that cannot be resolved here
/// is handed to the enclosing scope (that is how a jump leaves an IF body or
/// a LOOP); the outermost caller turns an unresolved jump into an error.
fn run_blocks(
    block_pairs: &[Pair<Rule>],
    targets: &HashMap<String, Vec<usize>>,
    output: &mut Output,
    state: &mut State,
) -> Result<BlockFlow, ParsingError> {
    let mut index = 0;
    let mut jumps_taken = 0;
    while index < block_pairs.len() {
        match interpret_block(block_pairs[index].clone(), output, state)? {
            BlockFlow::Continue => index += 1,
            BlockFlow::EndProgram => return Ok(BlockFlow::EndProgram),
            BlockFlow::Jump(request) => match resolve_jump(targets, index, &request) {
                Some(destination) => {
                    // Only backward jumps can form cycles; bound them like the
                    // loop statements (tripping at the same >= threshold).
                    // Forward jumps strictly advance and are fine in any number.
                    if destination <= index {
                        jumps_taken += 1;
                        if jumps_taken >= state.iteration_limit {
                            return Err(ParsingError::LoopLimit {
                                limit: state.iteration_limit.to_string(),
                            });
                        }
                    }
                    index = destination;
                }
                None => return Ok(BlockFlow::Jump(request)),
            },
        }
    }
    Ok(BlockFlow::Continue)
}

