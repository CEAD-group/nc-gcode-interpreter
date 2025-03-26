use thiserror::Error;
use crate::types::Rule;

#[derive(Error, Debug)]
pub enum ParsingError {
    #[error("Expected a pair, got none")]
    ExpectedPair,

    #[error("Unexpected axis '{axis}'. Valid axes are: {axes}")]
    UnexpectedAxis { axis: String, axes: String },

    #[error("Cannot define a variable named '{name}', as it conflicts with one of the axis names")]
    AxisUsedAsVariable { name: String },

    #[error("Unexpected rule '{rule:?}' encountered in {context}")]
    UnexpectedRule { rule: Rule, context: String },

    #[error("Parse error: {message}")]
    ParseError { message: String },

    #[error("Unexpected operator: {operator}")]
    UnexpectedOperator { operator: String },

    #[error("Invalid number of elements in condition")]
    InvalidCondition,

    #[error("Expected {expected} elements in the statement, found {actual}")]
    InvalidElementCount { expected: usize, actual: usize },

    #[error("Unknown variable: {variable}")]
    UnknownVariable { variable: String },

    #[error("Missing inner element in {context}")]
    MissingInnerElement { context: String },

    #[error("Loop limit of {limit} reached. Check the input for infinite loops or increase the limit")]
    LoopLimit { limit: String },

    #[error("Too many M commands in a single block, a maximum of 5 is allowed")]
    TooManyMCommands,

    #[error("Arithmetic error: {message}")]
    ArithmeticError { message: String },

    #[error(transparent)]
    IOError(#[from] std::io::Error),

    #[error("Error in block at line {line_no}:\n{preview}\n\nCaused by: {source}")]
    AnnotatedError {
        line_no: usize,
        preview: String,
        #[source]
        source: Box<ParsingError>,
    },

    #[error("Parse error on line {line_no}:\n{preview}\n\nExpected {expected:?}, got {actual:?}")]
    RuleAssertion {
        line_no: usize,
        preview: String,
        expected: Rule,
        actual: Rule,
    },

    #[error(r#"
Parse error in {context} on line {line_no}
----------------------------------------
Line: {preview}

Details: 
{message}
"#)]
    ParsingContext {
        line_no: usize,
        preview: String,
        context: String,
        message: String,
    },
}

impl From<ParsingError> for std::io::Error {
    fn from(err: ParsingError) -> std::io::Error {
        std::io::Error::new(std::io::ErrorKind::Other, err.to_string())
    }
}
