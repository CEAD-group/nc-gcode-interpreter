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
    UnexpectedAxis { axis: String, axes: String },
    #[error("Cannot define a variable named '{name}', as it conflicts with an axis name")]
    AxisUsedAsVariable { name: String },
    #[error("Cannot define a variable named '{name}', as it is a reserved block address (spline PW/SD/PL)")]
    ReservedNameUsedAsVariable { name: String },
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
            | Self::InvalidFunctionArity { line_no, preview, .. } => some(*line_no, None, None, Some(preview)),
            Self::ParseError { .. }
            | Self::InvalidElementCount { .. }
            | Self::InvalidCondition
            | Self::UnexpectedOperator { .. }
            | Self::LoopLimit { .. }
            | Self::StreamClosed
            | Self::UnexpectedAxis { .. }
            | Self::AxisUsedAsVariable { .. }
            | Self::ReservedNameUsedAsVariable { .. } => None,
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
}
