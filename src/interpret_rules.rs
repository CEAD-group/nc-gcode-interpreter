use crate::errors::ParsingError;
use crate::state::State;
use crate::types::Pair;
use crate::types::Rule;
use crate::types::Value;
use std::collections::HashMap;

type Output = Vec<HashMap<String, Value>>;
fn interpret_primary(primary: Pair<Rule>, state: &mut State) -> Result<f32, ParsingError> {
    let inner_pair = primary.into_inner().next().expect("Error");
    match inner_pair.as_rule() {
        Rule::value => {
            return Ok(inner_pair.as_str().parse::<f32>().expect("Failed to interpret value"));
        }
        Rule::variable => {
            let (line_no, preview) = get_error_context(&inner_pair, state);
            interpret_variable(inner_pair, state).and_then(|key| {
                state
                    .symbol_table
                    .get(&key)
                    .cloned()
                    .ok_or(ParsingError::UndefinedVariable { 
                        line_no,
                        preview,
                        name: key 
                    })
            })
        },
        Rule::variable_array => {
            let (line_no, preview) = get_error_context(&inner_pair, state);
            interpret_variable_array(inner_pair, state).and_then(|keys| {
                state
                    .symbol_table
                    .get(&keys[keys.len() - 1])
                    .cloned()
                    .ok_or(ParsingError::UnknownVariable {
                        line_no,
                        preview,
                        variable: keys[keys.len() - 1].clone(),
                    })
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
fn evaluate_arithmetic_function(pair: Pair<Rule>, state: &mut State) -> Result<f32, ParsingError> {
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

    // Apply the function
    match func_name.as_str() {
        "SIN" => {
            check_args(1)?;
            Ok(args[0].sin())
        },
        "COS" => {
            check_args(1)?;
            Ok(args[0].cos())
        },
        "TAN" => {
            check_args(1)?;
            Ok(args[0].tan())
        },
        "ASIN" => {
            check_args(1)?;
            Ok(args[0].asin())
        },
        "ACOS" => {
            check_args(1)?;
            Ok(args[0].acos())
        },
        "ATAN2" => {
            check_args(2)?;
            Ok(args[1].atan2(args[0]))
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
        "LN" => {
            check_args(1)?;
            Ok(args[0].ln())
        },
        "EXP" => {
            check_args(1)?;
            Ok(args[0].exp())
        },
        _ => Err(ParsingError::ParseError {
            message: format!("Unknown arithmetic function: {}", func_name.as_str()),
        }),
    }
}
fn evaluate_expression(expression: Pair<Rule>, state: &mut State) -> Result<f32, ParsingError> {
    // check if the first inner_pair is a negative sign
    let mut inner_pairs = expression.clone().into_inner();
    let mut inner_pair = inner_pairs.next().expect("Error");
    let mut lhs: f32 = 1.0;

    if inner_pair.as_rule() == Rule::neg {
        lhs = -1.0;
        inner_pair = inner_pairs.next().expect("Error");
    }

    // the next inner_pair should be a value or identifier
    lhs *= interpret_primary(inner_pair, state)?;

    // check if there is a next inner pair. If so it should be an operator
    // if not, return the result
    // if there is, evaluate the next inner pair and apply the operator

    while let Some(inner_pair) = inner_pairs.next() {
        let operator_rule = inner_pair.as_rule();
        let inner_pair = inner_pairs.next().expect("Expected an operand after an operator");

        let rhs: f32 = match inner_pair.as_rule() {
            Rule::neg => -interpret_primary(
                inner_pairs.next().expect("Expected a primary expression after 'neg'"),
                state,
            )?,
            Rule::primary => interpret_primary(inner_pair.clone(), state)?,
            // _ => panic!("Unexpected rule: {:?}", inner_pair.as_rule()),
            _ => Err(ParsingError::UnexpectedRule {
                rule: inner_pair.as_rule(),
                context: "evaluate_expression::rhs".to_string(),
                line_no: inner_pair.line_col().0,
                preview: state.get_line(inner_pair.line_col().0).unwrap_or("").to_string(),
                message: format!("Unexpected rule in evaluate_expression::rhs: {:?}", inner_pair.as_rule()),
            })?,
        };

        // Match against the operator rules directly
        match operator_rule {
            Rule::op_add => lhs += rhs,
            Rule::op_sub => lhs -= rhs,
            Rule::op_mul => lhs *= rhs,
            Rule::op_div => lhs /= rhs,
            Rule::op_int_div => {
                let lhs_int = lhs as i32; // Cast the left-hand side to an integer
                let rhs_int = rhs as i32; // Cast the right-hand side to an integer
                lhs = (lhs_int / rhs_int) as f32
            }
            Rule::op_mod => {
                lhs %= rhs;
            }
            // _ => panic!("Unexpected operator rule: {:?}", operator_rule),
            _ => Err(ParsingError::UnexpectedRule {
                rule: operator_rule,
                context: "evaluate_expression::operator".to_string(),
                line_no: inner_pair.line_col().0,
                preview: state.get_line(inner_pair.line_col().0).unwrap_or("").to_string(),
                message: format!("Unexpected operator rule: {:?}", operator_rule),
            })?,
        }
    }

    Ok(lhs)
}
fn interpret_g_command(g_command: Pair<Rule>) -> (String, String) {
    let inner_pair = g_command.into_inner().next().expect("Error");
    let mut rule_name = format!("{:?}", inner_pair.as_rule());
    let command_str = inner_pair.as_str().to_string();
    if rule_name.is_empty() {
        rule_name = command_str.clone();
    }
    (rule_name, command_str)
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
    last.insert("T".to_string(), Value::Str(tool_name));

    Ok(())
}
fn interpret_non_returning_function_call(function_call: Pair<Rule>) -> (String, String) {
    // Log the interpretd function call for debugging
    //println!("Parsed function call: {:?}", function_call);

    let command_str = function_call.as_str().to_string();

    // Return the tuple with the rule name as the column header and the specific function call as the value
    ("non_returning_function_call".to_string(), command_str)
}
fn interpret_assignment(element: Pair<Rule>, state: &mut State) -> Result<(String, f32), ParsingError> {
    let mut inner_pairs = element.into_inner();

    let variable_pair = inner_pairs
        .next()
        .ok_or_else(|| ParsingError::InvalidElementCount { expected: 2, actual: 0 })?;

    let expression_pair = inner_pairs
        .next()
        .ok_or_else(|| ParsingError::InvalidElementCount { expected: 2, actual: 1 })?;

    let (key, value, translate) = match (variable_pair.as_rule(), expression_pair.as_rule()) {
        (Rule::variable_single_char, Rule::value) => {
            let key = variable_pair.as_str().to_string();
            let value = expression_pair
                .as_str()
                .parse::<f32>()
                .expect("Failed to interpret value");
            (key, value, true)
        }
        (Rule::variable, Rule::axis_increment) => {
            let key = interpret_variable(variable_pair.clone(), state)?;
            let value = interpret_axis_increment(expression_pair, state, key.clone())?;
            (key, value, false)
        }
        (Rule::variable, Rule::expression) => {
            let key = interpret_variable(variable_pair.clone(), state)?;
            let value = evaluate_expression(expression_pair, state)?;
            (key, value, false)
        }
        (Rule::variable_array, Rule::expression) => {
            let keys = interpret_variable_array(variable_pair, state)?;
            let value = evaluate_expression(expression_pair, state)?;
            (keys[keys.len() - 1].clone(), value, true)
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
        state.update_axis(&key, value, translate)?;
    } else {
        state.symbol_table.insert(key.clone(), value);
    }

    Ok((key, value))
}
fn interpret_axis_increment(pair: Pair<Rule>, state: &mut State, key: String) -> Result<f32, ParsingError> {
    // axis_increment = { "IC" ~ "(" ~ expression ~ ")" }
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
    match state.axes.get(&key) {
        Some(val) => Ok(*val + increment),
        None => {
            eprintln!(
                "Warning: The axis '{}' is incremented before a fixed value is set, the G-code behavior may be indeterminate.",
                key
            );
            Ok(increment)
        }
    }
}
fn interpret_assignment_multi(element: Pair<Rule>, state: &mut State) -> Result<Vec<String>, ParsingError> {
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
    for (i, key) in keys.iter().enumerate() {
        if let Some(value) = values.get(i).cloned().flatten() {
            state.symbol_table.insert(key.clone(), value);
        } else {
            // do nothing, the value is not set
        }
    }
    Ok(keys)
}
fn interpret_value_array(pair: Pair<Rule>, state: &mut State) -> Result<Vec<Option<f32>>, ParsingError> {
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
        Rule::identifier => Ok(inner.as_str().to_string()),
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
fn interpret_indices(pair: Pair<Rule>, state: &mut State) -> Result<Vec<f32>, ParsingError> {
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
                    indices.push(index as f32);
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
    
    if pair.as_rule() == Rule::identifier {
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
fn interpret_definition(element: Pair<Rule>, state: &mut State) -> Result<(), ParsingError> {
    let pairs = element.into_inner();
    for pair in pairs {
        match pair.as_rule() {
            Rule::assignment => {
                let res = interpret_assignment(pair, state)?;
                if state.is_axis(res.0.as_str()) {
                    Err(ParsingError::AxisUsedAsVariable { name: res.0 })?;
                }
            }
            Rule::assignment_multi => {
                interpret_assignment_multi(pair, state)?;
            }
            Rule::variable => {
                let key = interpret_variable(pair, state)?;
                state.symbol_table.insert(key, 0.0);
            }
            Rule::variable_array => {
                let keys = interpret_variable_array(pair, state)?;
                for key in keys {
                    state.symbol_table.insert(key, 0.0);
                }
            }
            Rule::data_type => {
                // Ignore the type definition, as we are treating all variables as f32
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

    let inner_elements: Vec<Pair<Rule>> = condition.into_inner().collect();
    match inner_elements.as_slice() {
        [expression] => {
            let result = evaluate_expression(expression.clone(), state)?;
            Ok(result != 0.0)
        }
        [left_expression, operator, right_expression] => {
            let left_value = evaluate_expression(left_expression.clone(), state)?;
            let right_value = evaluate_expression(right_expression.clone(), state)?;
            evaluate_relational_operator(operator.clone(), left_value, right_value)
        }
        _ => Err(ParsingError::InvalidCondition),
    }
}
fn evaluate_relational_operator(operator: Pair<Rule>, lhs: f32, rhs: f32) -> Result<bool, ParsingError> {
    match operator.as_str() {
        "<" => Ok(lhs < rhs),
        ">" => Ok(lhs > rhs),
        "==" => Ok(lhs == rhs),
        "<>" => Ok(lhs != rhs),
        "<=" => Ok(lhs <= rhs),
        ">=" => Ok(lhs >= rhs),
        _ => Err(ParsingError::UnexpectedOperator {
            operator: operator.as_str().to_string(),
        }),
    }
}
fn interpret_statement_if(
    element: Pair<Rule>,
    output: &mut Output,
    state: &mut State,
) -> Result<(), ParsingError> {
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
        last.insert("comment".to_string(), Value::Str(comment_pair.as_str().to_string()));
    }

    // Evaluate the condition and execute the appropriate block
    if evaluate_condition(condition, state)? {
        interpret_blocks(true_block, output, state)?;
    } else if let Some(false_block) = false_block {
        interpret_blocks(false_block, output, state)?;
    }

    Ok(())
}
fn interpret_statement_while(
    element: Pair<Rule>,
    output: &mut Output,
    state: &mut State,
) -> Result<(), ParsingError> {
    let mut pairs = element.into_inner();
    let condition = pairs.next().expect("Expected a pair, got none");
    let blocks = pairs.next().expect("Expected a pair, got none");
    let mut loop_count = 0;
    while evaluate_condition(condition.clone(), state)? && loop_count < state.iteration_limit {
        loop_count += 1;
        interpret_blocks(blocks.clone(), output, state)?;
    }
    if loop_count >= state.iteration_limit {
        return Err(ParsingError::LoopLimit {
            limit: state.iteration_limit.to_string(),
        });
    }
    Ok(())
}
fn interpret_statement_for(
    element: Pair<Rule>,
    output: &mut Output,
    state: &mut State,
) -> Result<(), ParsingError> {
    let mut pairs = element.into_inner();

    // Parse and execute the assignment statement
    let assignment = pairs.next().expect("Expected an assignment, got none");
    let (variable_name, _) = interpret_assignment(assignment, state)?;

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
        interpret_blocks(blocks.clone(), output, state)?;

        // After parsing blocks, increment the loop control variable
        // Ensure this is done in a separate scope to avoid mutable borrow conflict
        {
            let loop_control_value = state
                .symbol_table
                .get_mut(&variable_name)
                .expect("Variable should exist");
            *loop_control_value += 1.0; // Increment value directly
        }
    }
    Ok(())
}
fn interpret_statement_repeat_until(
    element: Pair<Rule>,
    output: &mut Output,
    state: &mut State,
) -> Result<(), ParsingError> {
    let mut pairs = element.into_inner();
    let first_pair = pairs.next().expect("Expected a pair, got none");
    let blocks;
    match first_pair.as_rule() {
        Rule::comment => {
            let last = output.last_mut().expect("Output vector should not be empty");
            last.insert("comment".to_string(), Value::Str(first_pair.as_str().to_string()));

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
        interpret_blocks(blocks.clone(), output, state)?;
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
    Ok(())
}
fn interpret_control(
    element: Pair<Rule>,
    output: &mut Output,
    state: &mut State,
) -> Result<(), ParsingError> {
    let mut pairs = element.into_inner();
    let pair = pairs.next().expect("Expected a pair, got none");
    match pair.as_rule() {
        // Rule::goto_statement => println!("Goto statement: {:?}", pair),
        // Rule::gotob_statement => println!("Gotob statement: {:?}", pair),
        // Rule::gotof_statement => println!("Gotof statement: {:?}", pair),
        // Rule::gotoc_statement => println!("Gotoc statement: {:?}", pair),
        Rule::if_statement => interpret_statement_if(pair, output, state),
        // Rule::loop_statement => println!("Loop statement: {:?}", pair),
        Rule::for_statement => interpret_statement_for(pair, output, state),
        Rule::while_statement => interpret_statement_while(pair, output, state),
        Rule::repeat_until_statement => interpret_statement_repeat_until(pair, output, state),
        _ => panic!("Unexpected rule: {:?}", pair.as_rule()),
    }
}
fn insert_m_key(last: &mut HashMap<String, Value>, value: &str, line_no: usize, preview: String) -> Result<(), ParsingError> {
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
            last.insert(m_key.to_owned(), Value::StrList(vec![value.to_string()]));
            return Ok(()); // Exit early after insertion
        }
    }
    Err(ParsingError::TooManyMCommands {
        line_no,
        preview,
        message: "Too many M commands in a single block".to_string(),
    })
}
fn interpret_statement(
    element: Pair<Rule>,
    output: &mut Output,
    state: &mut State,
) -> Result<(), ParsingError> {
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

    for statement in element.into_inner() {
        let last = output.last_mut().expect("Output vector should not be empty");
        match statement.as_rule() {
            Rule::non_returning_function_call => {
                let (key, value) = interpret_non_returning_function_call(statement);
                last.insert(key, Value::Str(value));
            }
            Rule::g_command => {
                let (key, value) = interpret_g_command(statement);
                last.insert(key, Value::Str(value));
            }
            Rule::g_command_numbered => {
                let (key, value) =
                    interpret_g_command(statement.into_inner().next().expect("Error parsing g_command_numbered"));
                last.insert(key, Value::Str(value));
            }
            Rule::m_command => {
                let (line_no, preview) = get_error_context(&statement, state);
                let (_key, value) = interpret_m_command(statement);
                // there are 5 M codes allowed in a block. Store them in separate columns in the output
                insert_m_key(last, &value, line_no, preview)?;
            }
            Rule::assignment => {
                let (key, value) = interpret_assignment(statement, state)?;
                if state.is_axis(&key) {
                    let _updated_value = state.update_axis(&key, value, true)?;
                    last.insert(key, Value::Float(_updated_value));
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
    Ok(())
}
fn interpret_frame_op(element: Pair<Rule>, state: &mut State) -> Result<(), ParsingError> {
    let mut pairs = element.into_inner();
    let pair = pairs.next().expect("Expected a pair, got none");
    match pair.as_rule() {
        Rule::frame_trans => {
            for pair in pair.into_inner() {
                let (key, value) = interpret_assignment(pair, state)?;
                if state.is_axis(&key) {
                    state.update_translation(&key, value)?;
                } else {
                    return Err(ParsingError::UnexpectedAxis {
                        axis: key,
                        axes: state.axes.keys().cloned().collect(),
                    });
                }
            }
            Ok(())
        }
        Rule::frame_atrans => {
            for pair in pair.into_inner() {
                let (key, value) = interpret_assignment(pair, state)?;
                if state.is_axis(&key) {
                    let current_translation = state.get_translation(&key);
                    state.update_translation(&key, current_translation + value)?;
                } else {
                    return Err(ParsingError::UnexpectedAxis {
                        axis: key,
                        axes: state.axes.keys().cloned().collect(),
                    });
                }
            }
            Ok(())
        }
        _ => {
            return Err(ParsingError::UnexpectedRule {
                rule: pair.as_rule(),
                context: "interpret_frame_op".to_string(),
                line_no: pair.line_col().0,
                preview: state.get_line(pair.line_col().0).unwrap_or("").to_string(),
                message: format!("Unexpected rule in interpret_frame_op: {:?}", pair.as_rule()),
            })
        }
    }
}
fn interpret_block_number(element: Pair<Rule>, output: &mut Output) {
    let mut pairs = element.into_inner();
    let pair = pairs.next().expect("Expected a pair, got none");

    let last = output.last_mut().expect("Output vector should not be empty");
    let value: f32 = match pair.as_rule() {
        Rule::integer => pair.as_str().parse::<f32>().expect("Failed to interpret value"),
        _ => panic!("Unexpected rule: {:?}", pair.as_rule()),
    };
    last.insert("N".to_string(), Value::Str(value.to_string()));
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

fn interpret_block(
    element: Pair<Rule>,
    output: &mut Output,
    state: &mut State,
) -> Result<(), ParsingError> {
    match element.as_rule() {
        Rule::block => {
            // Create a new HashMap for this block
            output.push(HashMap::new());
            
            for item in element.into_inner() {
                match item.as_rule() {
                    Rule::statement => interpret_statement(item, output, state)?,
                    Rule::block_number => interpret_block_number(item, output),
                    Rule::control => interpret_control(item, output, state)?,
                    Rule::definition => interpret_definition(item, state)?,
                    Rule::frame_op => interpret_frame_op(item, state)?,
                    Rule::comment => {
                        let last = output.last_mut().expect("Output vector should not be empty");
                        last.insert("comment".to_string(), Value::Str(item.as_str().to_string()));
                    },
                    _ => return Err(annotate_error(&item, "block interpretation", 
                        format!("Unexpected rule: {:?}", item.as_rule()), state)),
                }
            }
            Ok(())
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
) -> Result<(), ParsingError> {
    match blocks.as_rule() {
        Rule::blocks => {
            for block in blocks.into_inner() {
                interpret_block(block, output, state)?;
            }
            Ok(())
        }
        _ => {
            return Err(annotate_error(&blocks, "blocks interpretation",
                format!("Expected blocks, found {:?}", blocks.as_rule()), state));
        }
    }
}
