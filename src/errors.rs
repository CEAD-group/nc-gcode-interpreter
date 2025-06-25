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
    #[error("Unknown variable or missing axis-to-index mapping: {variable}.\n\
This error may occur if you are assigning to an array at index '{variable}', but no axis-to-index mapping was provided for this axis.\n\
To fix this, pass an appropriate axis_index_map (e.g., axis_index_map={{ '{variable}': 4 }}) to the interpreter.")]
    UnknownVariable { variable: String },
    #[error("Unexpected rule '{rule:?}' encountered in {context}")]
    UnexpectedRule { rule: crate::types::Rule, context: String },
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
    #[error("Too many M commands in a single block")]
    TooManyMCommands,
    #[error("Unexpected axis '{axis}'. Valid axes are: {axes}")]
    UnexpectedAxis { axis: String, axes: String },
    #[error("Cannot define a variable named '{name}', as it conflicts with an axis name")]
    AxisUsedAsVariable { name: String },
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
