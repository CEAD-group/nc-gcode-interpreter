//errors.rs
use thiserror::Error;

use crate::types::Rule; // Import Parser trait here

#[derive(Error, Debug)]
pub enum ParsingError {
    #[error("expected a pair, got none")]
    ExpectedPair,

    #[error("unexpected axis for translation: {axis}, expected one of the configured axes {axes}")]
    UnexpectedAxis { axis: String, axes: String },

    #[error("cannot define a variable named: {name}, as it is conflicts with one of the axis names")]
    AxisUsedAsVariable { name: String },

    #[error("unexpected rule: '{rule:?}' encountered in {context}")]
    UnexpectedRule { rule: Rule, context: String },

    #[error("parse error: {message}")]
    ParseError { message: String },

    #[error("unexpected operator: {operator}")]
    UnexpectedOperator { operator: String },

    #[error("invalid number of elements in condition")]
    InvalidCondition,

    #[error("expected {expected} elements in the statement, found {actual}")]
    InvalidElementCount { expected: usize, actual: usize },

    #[error("unknown variable: {variable}")]
    UnknownVariable { variable: String },

    #[error("missing inner element in {context}")]
    MissingInnerElement { context: String },

    #[error("loop limit of {limit} reached. Check the input for infinite loops or increase the limit")]
    LoopLimit { limit: String },

    #[error("too many M commands in a single block, a maximum of 5 is allowed")]
    TooManyMCommands,

    #[error(transparent)]
    IOError(#[from] std::io::Error),

    #[error("Error in block '{block}': {source}")]
    AnnotatedError {
        block: String,
        #[source]
        source: Box<ParsingError>,
    },
    // Add more error variants as needed
}
impl From<ParsingError> for std::io::Error {
    fn from(err: ParsingError) -> std::io::Error {
        std::io::Error::new(std::io::ErrorKind::Other, err.to_string())
    }
}
