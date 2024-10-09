// src/types.rs

pub use pest::iterators::Pair;
use polars::prelude::AnyValue;

#[derive(Parser)]
#[grammar = "grammar.pest"]
pub struct NCParser;

#[derive(Debug, Clone)]
pub enum Value {
    Str(String),
    Float(f32),
    StrList(Vec<String>),
}

impl Value {
    pub fn to_polars_value(&self) -> AnyValue {
        match self {
            Value::Str(s) => AnyValue::String(s),
            Value::Float(f) => AnyValue::Float32(*f),
            Value::StrList(vec) => AnyValue::List(vec.iter().map(|s| s.as_str()).collect()),
        }
    }
}
