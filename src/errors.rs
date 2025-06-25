use thiserror::Error;

#[derive(Error, Debug)]
pub enum ParsingError {
    #[error(r#"
Parse error in {context} on line {line_no}
----------------------------------------
Line: {preview}

Details: {message}
"#)]
    ParsingContext {
        line_no: usize,
        preview: String,
        context: String,
        message: String,
    },
    #[error(r#"
Parse error in array indexing on line {line_no}
----------------------------------------
Line: {preview}

Details: Unknown variable or missing axis-to-index mapping: {variable}.
This error may occur if you are assigning to an array at index '{variable}', but no axis-to-index mapping was provided for this axis.
To fix this, pass an appropriate axis_index_map (e.g., axis_index_map={{ '{variable}': 4 }}) to the interpreter.
"#)]
    UnknownVariable { 
        line_no: usize,
        preview: String,
        variable: String 
    },
    #[error(r#"
Unexpected rule '{rule:?}' encountered in {context} on line {line_no}
----------------------------------------
Line: {preview}

Details: {message}
"#)]
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
    #[error(r#"
Too many M commands in a single block on line {line_no}
----------------------------------------
Line: {preview}

Details: {message}
To fix this, ensure that each block contains at most one M command.
"#)]
    TooManyMCommands {
        line_no: usize,
        preview: String,
        message: String,
    },
    #[error("Unexpected axis '{axis}'. Valid axes are: {axes}")]
    UnexpectedAxis { axis: String, axes: String },
    #[error("Cannot define a variable named '{name}', as it conflicts with an axis name")]
    AxisUsedAsVariable { name: String },
    #[error(r#"
Missing axis mapping on line {line_no}
----------------------------------------
Line: {preview}

Details: No mapping found for axis '{axis}' in array indexing operation.
To fix this, provide an axis_index_map that includes '{axis}'."#)]
    MissingAxisMapping {
        line_no: usize,
        preview: String,
        axis: String,
    },
    #[error(r#"
Invalid axis mapping on line {line_no}
----------------------------------------
Line: {preview}

Details: Invalid index {index} for axis '{axis}' in array indexing operation.
Array indices must be non-negative and within the valid range."#)]
    InvalidAxisIndex {
        line_no: usize,
        preview: String,
        axis: String,
        index: usize,
    },
}

impl ParsingError {
    pub fn with_context<T: AsRef<str>>(
        line_no: usize,
        preview: T,
        context: T,
        message: T,
    ) -> Self {
        Self::ParsingContext {
            line_no,
            preview: preview.as_ref().to_string(),
            context: context.as_ref().to_string(),
            message: message.as_ref().to_string(),
        }
    }
}

impl From<ParsingError> for std::io::Error {
    fn from(err: ParsingError) -> std::io::Error {
        std::io::Error::new(std::io::ErrorKind::Other, err.to_string())
    }
}
