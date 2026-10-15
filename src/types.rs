// src/types.rs

/// A grammar pair with an explicit source-line offset. Children inherit the
/// offset, so execution and diagnostics use original source coordinates even
/// when only one unpadded line was parsed.
#[derive(Debug, Clone)]
pub struct Pair<'i, R: pest::RuleType> {
    inner: pest::iterators::Pair<'i, R>,
    line_offset: usize,
}

impl<'i, R: pest::RuleType> Pair<'i, R> {
    pub fn at_line_offset(inner: pest::iterators::Pair<'i, R>, line_offset: usize) -> Self {
        Self { inner, line_offset }
    }

    pub fn as_rule(&self) -> R {
        self.inner.as_rule()
    }

    pub fn as_str(&self) -> &'i str {
        self.inner.as_str()
    }

    pub fn as_span(&self) -> pest::Span<'i> {
        self.inner.as_span()
    }

    pub fn line_col(&self) -> (usize, usize) {
        let (line, column) = self.inner.line_col();
        (line + self.line_offset, column)
    }

    pub fn into_inner(self) -> impl Iterator<Item = Self> + Clone {
        let offset = self.line_offset;
        self.inner
            .into_inner()
            .map(move |inner| Self::at_line_offset(inner, offset))
    }
}

impl<'i, R: pest::RuleType> From<pest::iterators::Pair<'i, R>> for Pair<'i, R> {
    fn from(inner: pest::iterators::Pair<'i, R>) -> Self {
        Self::at_line_offset(inner, 0)
    }
}

#[derive(Parser)]
#[grammar = "grammar.pest"]
pub struct NCParser;

#[derive(Debug, Clone)]
pub enum Value {
    Str(String),
    Float(f64),
    StrList(Vec<String>),
}
