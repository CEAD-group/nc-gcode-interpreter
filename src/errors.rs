use thiserror::Error;

#[derive(Error, Debug)]
pub enum ParsingError {
    #[error(
        r#"
Parse error in {context} on line {line_no}
----------------------------------------
Line: {preview}

Details: {message}
"#
    )]
    ParsingContext {
        line_no: usize,
        /// 1-based column of the offending token when known (syntax errors
        /// carry it from the pest span); `None` for errors located only to a
        /// line. Not shown in the formatted message, only exposed as data.
        column: Option<usize>,
        preview: String,
        context: String,
        message: String,
    },
    #[error(
        r#"
Parse error in array indexing on line {line_no}
----------------------------------------
Line: {preview}

Details: Invalid array index '{variable}'.
This error occurs when using a variable as an array index, but the variable is not defined.
"#
    )]
    UnknownVariable {
        line_no: usize,
        preview: String,
        variable: String,
    },
    #[error(
        r#"
Parse error in assignment on line {line_no}
----------------------------------------
Line: {preview}

Details: Undefined variable '{name}'.
This error occurs when using an undefined variable in an expression.
To fix this, make sure to define the variable before using it. This can 
be done by adding common definitions to an initial state file, or by
setting the `allow_undefined_variables` flag to true (this will initialize
undefined variables to 0.0).
"#
    )]
    UndefinedVariable {
        line_no: usize,
        preview: String,
        name: String,
    },
    #[error(
        r#"
Unexpected rule '{rule:?}' encountered in {context} on line {line_no}
----------------------------------------
Line: {preview}

Details: {message}
"#
    )]
    UnexpectedRule {
        rule: crate::types::Rule,
        context: String,
        line_no: usize,
        preview: String,
        message: String,
    },
    #[error("Parse error: {message}")]
    ParseError { message: String },
    #[error("Expected {expected} elements, found {actual}")]
    InvalidElementCount { expected: usize, actual: usize },
    #[error("Invalid condition")]
    InvalidCondition,
    #[error("Unexpected operator: {operator}")]
    UnexpectedOperator { operator: String },
    #[error("Loop limit of {limit} reached")]
    LoopLimit { limit: String },
    #[error("row stream closed by the consumer")]
    StreamClosed,
    #[error(
        r#"
Too many M commands in a single block on line {line_no}
----------------------------------------
Line: {preview}

Details: {message}
To fix this, ensure that each block contains at most one M command.
"#
    )]
    TooManyMCommands {
        line_no: usize,
        preview: String,
        message: String,
    },
    #[error("Unexpected axis '{axis}'. Valid axes are: {axes}")]
    UnexpectedAxis {
        axis: String,
        axes: String,
        /// Source location of the offending axis word. Not shown in the
        /// formatted message, only exposed as data (see [`ParsingError::location`]).
        line_no: usize,
        preview: String,
    },
    #[error("Cannot define a variable named '{name}', as it conflicts with an axis name")]
    AxisUsedAsVariable {
        name: String,
        line_no: usize,
        preview: String,
    },
    #[error("Cannot define a variable named '{name}', as it is a reserved block address (spline PW/SD/PL)")]
    ReservedNameUsedAsVariable {
        name: String,
        line_no: usize,
        preview: String,
    },
    #[error(
        r#"
Missing axis mapping on line {line_no}
----------------------------------------
Line: {preview}

Details: No mapping found for axis '{axis}' in array indexing operation.
To fix this, provide an axis_index_map that includes '{axis}'."#
    )]
    MissingAxisMapping {
        line_no: usize,
        preview: String,
        axis: String,
    },
    #[error(
        r#"
Invalid axis mapping on line {line_no}
----------------------------------------
Line: {preview}

Details: Invalid index {index} for axis '{axis}' in array indexing operation.
Array indices must be non-negative and within the valid range."#
    )]
    InvalidAxisIndex {
        line_no: usize,
        preview: String,
        axis: String,
        index: usize,
    },
    #[error(
        r#"
Unsupported statement on line {line_no}
----------------------------------------
Line: {preview}

Details: {statement} is not supported by this interpreter.
{hint}
"#
    )]
    UnsupportedStatement {
        line_no: usize,
        preview: String,
        statement: String,
        hint: String,
    },
    #[error(
        r#"
Jump destination not found on line {line_no}
----------------------------------------
Line: {preview}

Details: No block with the jump label or block number '{target}' was found
searching {search_direction} (alarm 14080 on a real control).{hint}
Note: jump destinations inside IF/LOOP/FOR/WHILE/REPEAT bodies cannot be
reached from outside those bodies.
"#
    )]
    JumpTargetNotFound {
        line_no: usize,
        preview: String,
        target: String,
        search_direction: String,
        /// Either empty or a "\nDid you mean '...'?" suggestion line.
        hint: String,
    },
    #[error(
        r#"
Unmatched control structure on line {line_no}
----------------------------------------
Line: {preview}

Details: {message}.
"#
    )]
    UnmatchedStructure {
        line_no: usize,
        preview: String,
        message: String,
    },
    #[error(
        r#"
Unknown G code on line {line_no}
----------------------------------------
Line: {preview}

Details: '{code}' is not a G code known to this interpreter (a real control
raises alarm 12470 "undefined G function"). If this is meant to be a
subprogram call, note that program names cannot look like a G code.
"#
    )]
    UnknownGCommand {
        line_no: usize,
        preview: String,
        code: String,
    },
    #[error(
        r#"
Invalid function call on line {line_no}
----------------------------------------
Line: {preview}

Details: Function {name} expects {expected} argument(s), but received {actual}.
"#
    )]
    InvalidFunctionArity {
        line_no: usize,
        preview: String,
        name: String,
        expected: usize,
        actual: usize,
    },
}

/// Structured location of an error, for callers that want the position as data
/// rather than parsing it out of the formatted message (e.g. an editor
/// highlighting the offending token). Exposed to Python as attributes on the
/// `NcError` exception.
#[derive(Debug, Clone, Default)]
pub struct ErrorLocation {
    /// 1-based source line the error is anchored to.
    pub line: usize,
    /// 1-based column when known (syntax errors), else `None`.
    pub column: Option<usize>,
    /// Which stage/construct was being parsed, when the variant records it.
    pub context: Option<String>,
    /// The source line's text, when captured.
    pub line_text: Option<String>,
}

impl ParsingError {
    pub fn with_context<T: AsRef<str>>(line_no: usize, preview: T, context: T, message: T) -> Self {
        Self::ParsingContext {
            line_no,
            column: None,
            preview: preview.as_ref().to_string(),
            context: context.as_ref().to_string(),
            message: message.as_ref().to_string(),
        }
    }

    /// The error's source location as structured data, or `None` for errors
    /// not tied to a specific line (stream closed, element-count mismatches,
    /// etc.). `line_text`/`context`/`column` are populated when the variant
    /// carries them.
    pub fn location(&self) -> Option<ErrorLocation> {
        let some = |line: usize, column: Option<usize>, context: Option<&str>, preview: Option<&str>| {
            Some(ErrorLocation {
                line,
                column,
                context: context.map(str::to_string),
                line_text: preview.map(str::to_string),
            })
        };
        match self {
            Self::ParsingContext {
                line_no,
                column,
                preview,
                context,
                ..
            } => some(*line_no, *column, Some(context), Some(preview)),
            Self::UnexpectedRule {
                line_no,
                preview,
                context,
                ..
            } => some(*line_no, None, Some(context), Some(preview)),
            Self::UnknownVariable { line_no, preview, .. }
            | Self::UndefinedVariable { line_no, preview, .. }
            | Self::TooManyMCommands { line_no, preview, .. }
            | Self::MissingAxisMapping { line_no, preview, .. }
            | Self::InvalidAxisIndex { line_no, preview, .. }
            | Self::UnsupportedStatement { line_no, preview, .. }
            | Self::JumpTargetNotFound { line_no, preview, .. }
            | Self::UnmatchedStructure { line_no, preview, .. }
            | Self::UnknownGCommand { line_no, preview, .. }
            | Self::InvalidFunctionArity { line_no, preview, .. }
            // Semantic/validation errors: the offending block is known at raise
            // time, so they anchor to a line (no column) like the others above.
            | Self::UnexpectedAxis { line_no, preview, .. }
            | Self::AxisUsedAsVariable { line_no, preview, .. }
            | Self::ReservedNameUsedAsVariable { line_no, preview, .. } => some(*line_no, None, None, Some(preview)),
            Self::ParseError { .. }
            | Self::InvalidElementCount { .. }
            | Self::InvalidCondition
            | Self::UnexpectedOperator { .. }
            | Self::LoopLimit { .. }
            | Self::StreamClosed => None,
        }
    }

    /// A stable, machine-readable discriminator for the error class, so a
    /// consumer can branch on the kind of error without string-matching the
    /// formatted message. Exposed to Python as the `NcError.kind` attribute.
    /// These strings are part of the public API: keep them stable.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::ParsingContext { .. } => "parse_context",
            Self::UnknownVariable { .. } => "unknown_variable",
            Self::UndefinedVariable { .. } => "undefined_variable",
            Self::UnexpectedRule { .. } => "unexpected_rule",
            Self::ParseError { .. } => "parse_error",
            Self::InvalidElementCount { .. } => "invalid_element_count",
            Self::InvalidCondition => "invalid_condition",
            Self::UnexpectedOperator { .. } => "unexpected_operator",
            Self::LoopLimit { .. } => "loop_limit",
            Self::StreamClosed => "stream_closed",
            Self::TooManyMCommands { .. } => "too_many_m_commands",
            Self::UnexpectedAxis { .. } => "unexpected_axis",
            Self::AxisUsedAsVariable { .. } => "axis_used_as_variable",
            Self::ReservedNameUsedAsVariable { .. } => "reserved_name_used_as_variable",
            Self::MissingAxisMapping { .. } => "missing_axis_mapping",
            Self::InvalidAxisIndex { .. } => "invalid_axis_index",
            Self::UnsupportedStatement { .. } => "unsupported_statement",
            Self::JumpTargetNotFound { .. } => "jump_target_not_found",
            Self::UnmatchedStructure { .. } => "unmatched_structure",
            Self::UnknownGCommand { .. } => "unknown_g_command",
            Self::InvalidFunctionArity { .. } => "invalid_function_arity",
        }
    }
}

impl From<ParsingError> for std::io::Error {
    fn from(err: ParsingError) -> std::io::Error {
        std::io::Error::new(std::io::ErrorKind::Other, err.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn location_extracts_line_column_context_and_text() {
        // A syntax error carries a column; the others locate to a line only.
        let syntax = ParsingError::ParsingContext {
            line_no: 2,
            column: Some(8),
            preview: "X20 Y((".to_string(),
            context: "line parsing".to_string(),
            message: "unexpected".to_string(),
        };
        let loc = syntax.location().expect("has location");
        assert_eq!(loc.line, 2);
        assert_eq!(loc.column, Some(8));
        assert_eq!(loc.context.as_deref(), Some("line parsing"));
        assert_eq!(loc.line_text.as_deref(), Some("X20 Y(("));

        let semantic = ParsingError::UndefinedVariable {
            line_no: 1,
            preview: "X=R99".to_string(),
            name: "R99".to_string(),
        };
        let loc = semantic.location().expect("has location");
        assert_eq!((loc.line, loc.column), (1, None));
        assert_eq!(loc.line_text.as_deref(), Some("X=R99"));

        // Errors with no source anchor return None.
        assert!(ParsingError::StreamClosed.location().is_none());
        assert!(ParsingError::InvalidCondition.location().is_none());
    }

    #[test]
    fn validation_errors_now_carry_a_location() {
        // #56: semantic/validation errors (previously location-less) anchor to
        // the offending line so an editor can mark the spot.
        let axis = ParsingError::UnexpectedAxis {
            axis: "QQ".to_string(),
            axes: "X, Y, Z".to_string(),
            line_no: 4,
            preview: "TRANS QQ10".to_string(),
        };
        let loc = axis.location().expect("UnexpectedAxis now has a location");
        assert_eq!((loc.line, loc.column), (4, None));
        assert_eq!(loc.line_text.as_deref(), Some("TRANS QQ10"));

        let dup = ParsingError::AxisUsedAsVariable {
            name: "X".to_string(),
            line_no: 2,
            preview: "DEF REAL X".to_string(),
        };
        assert_eq!(dup.location().expect("has location").line, 2);
    }

    #[test]
    fn kind_is_a_stable_per_variant_discriminator() {
        // Exhaustive: `kind()` strings are public API, so pin every variant's
        // value. The wildcard-free match in `kind()` forces a new variant to be
        // handled there; this list forces its string to be chosen deliberately.
        use crate::types::Rule;
        let s = String::new;
        let cases: Vec<(ParsingError, &str)> = vec![
            (
                ParsingError::ParsingContext {
                    line_no: 1,
                    column: None,
                    preview: s(),
                    context: s(),
                    message: s(),
                },
                "parse_context",
            ),
            (
                ParsingError::UnknownVariable {
                    line_no: 1,
                    preview: s(),
                    variable: s(),
                },
                "unknown_variable",
            ),
            (
                ParsingError::UndefinedVariable {
                    line_no: 1,
                    preview: s(),
                    name: s(),
                },
                "undefined_variable",
            ),
            (
                ParsingError::UnexpectedRule {
                    rule: Rule::EOI,
                    context: s(),
                    line_no: 1,
                    preview: s(),
                    message: s(),
                },
                "unexpected_rule",
            ),
            (ParsingError::ParseError { message: s() }, "parse_error"),
            (
                ParsingError::InvalidElementCount { expected: 1, actual: 2 },
                "invalid_element_count",
            ),
            (ParsingError::InvalidCondition, "invalid_condition"),
            (
                ParsingError::UnexpectedOperator { operator: s() },
                "unexpected_operator",
            ),
            (ParsingError::LoopLimit { limit: s() }, "loop_limit"),
            (ParsingError::StreamClosed, "stream_closed"),
            (
                ParsingError::TooManyMCommands {
                    line_no: 1,
                    preview: s(),
                    message: s(),
                },
                "too_many_m_commands",
            ),
            (
                ParsingError::UnexpectedAxis {
                    axis: s(),
                    axes: s(),
                    line_no: 1,
                    preview: s(),
                },
                "unexpected_axis",
            ),
            (
                ParsingError::AxisUsedAsVariable {
                    name: s(),
                    line_no: 1,
                    preview: s(),
                },
                "axis_used_as_variable",
            ),
            (
                ParsingError::ReservedNameUsedAsVariable {
                    name: s(),
                    line_no: 1,
                    preview: s(),
                },
                "reserved_name_used_as_variable",
            ),
            (
                ParsingError::MissingAxisMapping {
                    line_no: 1,
                    preview: s(),
                    axis: s(),
                },
                "missing_axis_mapping",
            ),
            (
                ParsingError::InvalidAxisIndex {
                    line_no: 1,
                    preview: s(),
                    axis: s(),
                    index: 0,
                },
                "invalid_axis_index",
            ),
            (
                ParsingError::UnsupportedStatement {
                    line_no: 1,
                    preview: s(),
                    statement: s(),
                    hint: s(),
                },
                "unsupported_statement",
            ),
            (
                ParsingError::JumpTargetNotFound {
                    line_no: 1,
                    preview: s(),
                    target: s(),
                    search_direction: s(),
                    hint: s(),
                },
                "jump_target_not_found",
            ),
            (
                ParsingError::UnmatchedStructure {
                    line_no: 1,
                    preview: s(),
                    message: s(),
                },
                "unmatched_structure",
            ),
            (
                ParsingError::UnknownGCommand {
                    line_no: 1,
                    preview: s(),
                    code: s(),
                },
                "unknown_g_command",
            ),
            (
                ParsingError::InvalidFunctionArity {
                    line_no: 1,
                    preview: s(),
                    name: s(),
                    expected: 1,
                    actual: 2,
                },
                "invalid_function_arity",
            ),
        ];
        for (err, expected) in &cases {
            assert_eq!(err.kind(), *expected, "kind mismatch for {err:?}");
        }
        // Every kind string is distinct (no two variants share a discriminator).
        let mut kinds: Vec<&str> = cases.iter().map(|(_, k)| *k).collect();
        kinds.sort_unstable();
        let unique = kinds.len();
        kinds.dedup();
        assert_eq!(kinds.len(), unique, "duplicate kind discriminator");
    }
}
