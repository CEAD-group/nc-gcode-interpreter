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

Details: Invalid array index '{variable}'.
This error occurs when using a variable as an array index, but the variable is not defined.
"#)]
    UnknownVariable { 
        line_no: usize,
        preview: String,
        variable: String 
    },
    #[error(r#"
Parse error in assignment on line {line_no}
----------------------------------------
Line: {preview}

Details: Undefined variable '{name}'.
This error occurs when using an undefined variable in an expression.
To fix this, make sure to define the variable before using it.
"#)]
    UndefinedVariable {
        line_no: usize,
        preview: String,
        name: String,
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
    #[error(r#"
Invalid function call on line {line_no}
----------------------------------------
Line: {preview}

Details: Function {name} expects {expected} argument(s), but received {actual}.
"#)]
    InvalidFunctionArity {
        line_no: usize,
        preview: String,
        name: String,
        expected: usize,
        actual: usize,
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
