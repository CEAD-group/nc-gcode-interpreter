// src/types.rs

pub use pest::iterators::Pair;

#[derive(Parser)]
#[grammar = "grammar.pest"]
pub struct NCParser;

#[derive(Debug, Clone)]
pub enum Value {
    Str(String),
    Float(f32),
    StrList(Vec<String>),
}
