//! Plain-Rust columnar output.
//!
//! The interpreter used to assemble a polars DataFrame in Rust, which coupled
//! the crate to a specific polars / pyo3-polars / Python-polars version trio.
//! Instead, the core now produces this simple `Table`; the Python wrapper
//! turns it into a polars DataFrame on the Python side, and the CLI writes
//! CSV directly with the `csv` crate.

use crate::modal_groups::{MODAL_G_GROUPS, NON_MODAL_G_GROUPS};
use crate::types::Value;
use std::collections::{HashMap, HashSet};

/// A single typed column. Values are optional: `None` is a null cell.
#[derive(Debug, Clone, PartialEq)]
pub enum Column {
    Float(Vec<Option<f32>>),
    Int(Vec<Option<i64>>),
    Str(Vec<Option<String>>),
    StrList(Vec<Option<Vec<String>>>),
}

impl Column {
    pub fn len(&self) -> usize {
        match self {
            Column::Float(v) => v.len(),
            Column::Int(v) => v.len(),
            Column::Str(v) => v.len(),
            Column::StrList(v) => v.len(),
        }
    }

    #[allow(dead_code)] // used via the rlib API
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Name of the dtype as understood by the Python wrapper.
    #[allow(dead_code)] // used by the python-feature bindings, not the bin
    pub fn dtype_name(&self) -> &'static str {
        match self {
            Column::Float(_) => "f64",
            Column::Int(_) => "i64",
            Column::Str(_) => "str",
            Column::StrList(_) => "list[str]",
        }
    }

    /// Forward-fill null cells with the last non-null value.
    fn forward_fill(&mut self) {
        fn fill<T: Clone>(v: &mut [Option<T>]) {
            let mut last: Option<T> = None;
            for cell in v.iter_mut() {
                match cell {
                    Some(value) => last = Some(value.clone()),
                    None => *cell = last.clone(),
                }
            }
        }
        match self {
            Column::Float(v) => fill(v),
            Column::Int(v) => fill(v),
            Column::Str(v) => fill(v),
            Column::StrList(v) => fill(v),
        }
    }
}

/// Ordered collection of named, typed, equal-length columns.
#[derive(Debug, Clone, Default)]
pub struct Table {
    pub columns: Vec<(String, Column)>,
}

/// The dedicated axis columns that get a fixed position in the output.
const KNOWN_AXIS_COLUMNS: &[&str] = &[
    "X", "Y", "Z", "A", "B", "C", "D", "E", "F", "S", "U", "V", "RA1", "RA2", "RA3", "RA4", "RA5", "RA6",
];

fn is_string_column(name: &str) -> bool {
    MODAL_G_GROUPS.contains(&name)
        || NON_MODAL_G_GROUPS.contains(&name)
        || matches!(name, "T" | "non_returning_function_call" | "comment")
}

impl Table {
    /// Build a sanitized table from interpreter rows: typed columns in the
    /// canonical order (N, G-group columns, axes, other value columns, T, M,
    /// function calls, comment), with axis and modal G-group columns
    /// forward-filled unless disabled.
    pub fn from_rows(rows: &[HashMap<String, Value>], disable_forward_fill: bool) -> Table {
        // Skip rows that carry no values at all (blocks that only affected
        // internal state, e.g. definitions).
        let rows: Vec<&HashMap<String, Value>> = rows.iter().filter(|r| !r.is_empty()).collect();

        let present: HashSet<&str> = rows.iter().flat_map(|r| r.keys().map(|k| k.as_str())).collect();

        // Canonical column order.
        let mut ordered: Vec<String> = Vec::new();
        let push_if_present = |name: &str, ordered: &mut Vec<String>| {
            if present.contains(name) && !ordered.iter().any(|c| c == name) {
                ordered.push(name.to_string());
            }
        };
        push_if_present("N", &mut ordered);
        for &name in MODAL_G_GROUPS.iter().chain(NON_MODAL_G_GROUPS.iter()) {
            push_if_present(name, &mut ordered);
        }
        for &name in KNOWN_AXIS_COLUMNS {
            push_if_present(name, &mut ordered);
        }
        // Any remaining value columns (e.g. user-configured extra axes), in
        // deterministic alphabetical order.
        let mut extra: Vec<&str> = present
            .iter()
            .copied()
            .filter(|name| {
                !ordered.iter().any(|c| c == name)
                    && !matches!(*name, "T" | "M" | "non_returning_function_call" | "comment")
            })
            .collect();
        extra.sort_unstable();
        for name in extra {
            ordered.push(name.to_string());
        }
        for name in ["T", "M", "non_returning_function_call", "comment"] {
            push_if_present(name, &mut ordered);
        }

        // The forward-fill set: value columns (axes, block numbers - anything
        // that is not a known string/list column) plus the modal G-group
        // columns, matching the previous DataFrame sanitization.
        let modal: HashSet<&str> = MODAL_G_GROUPS.iter().copied().collect();

        let mut columns: Vec<(String, Column)> = Vec::with_capacity(ordered.len());
        for name in ordered {
            let mut column = build_column(&name, &rows);
            let is_value_column = matches!(column, Column::Float(_) | Column::Int(_));
            if !disable_forward_fill && (is_value_column || modal.contains(name.as_str())) {
                column.forward_fill();
            }
            columns.push((name, column));
        }
        Table { columns }
    }

    pub fn height(&self) -> usize {
        self.columns.first().map_or(0, |(_, c)| c.len())
    }
}

fn build_column(name: &str, rows: &[&HashMap<String, Value>]) -> Column {
    if name == "N" {
        // Block numbers are parsed as numbers but stored as strings; expose
        // them as integers.
        let data = rows
            .iter()
            .map(|r| match r.get(name) {
                Some(Value::Str(s)) => s.parse::<f64>().ok().map(|v| v as i64),
                Some(Value::Float(f)) => Some(*f as i64),
                _ => None,
            })
            .collect();
        return Column::Int(data);
    }
    if name == "M" {
        let data = rows
            .iter()
            .map(|r| match r.get(name) {
                Some(Value::StrList(l)) => Some(l.clone()),
                Some(Value::Str(s)) => Some(vec![s.clone()]),
                _ => None,
            })
            .collect();
        return Column::StrList(data);
    }
    if is_string_column(name) {
        let data = rows
            .iter()
            .map(|r| match r.get(name) {
                Some(Value::Str(s)) => Some(s.clone()),
                Some(Value::Float(f)) => Some(f.to_string()),
                _ => None,
            })
            .collect();
        return Column::Str(data);
    }
    // Everything else is a value column (axes and friends).
    let data = rows
        .iter()
        .map(|r| match r.get(name) {
            Some(Value::Float(f)) => Some(*f),
            Some(Value::Str(s)) => s.parse::<f32>().ok(),
            _ => None,
        })
        .collect();
    Column::Float(data)
}

/// Write the table as CSV. List columns (M codes) are exploded: a row with a
/// multi-element list is written as multiple CSV rows, duplicating the other
/// cells, matching the behavior of the previous polars-based writer. Floats
/// are written with three decimals, nulls as empty fields.
pub fn write_csv<W: std::io::Write>(table: &Table, writer: W) -> Result<(), std::io::Error> {
    let mut w = csv::Writer::from_writer(writer);
    w.write_record(table.columns.iter().map(|(name, _)| name.as_str()))?;

    let height = table.height();
    for row in 0..height {
        // How many CSV rows does this table row explode into?
        let copies = table
            .columns
            .iter()
            .map(|(_, c)| match c {
                Column::StrList(v) => v[row].as_ref().map_or(1, |l| l.len().max(1)),
                _ => 1,
            })
            .max()
            .unwrap_or(1);

        for copy in 0..copies {
            let mut record: Vec<String> = Vec::with_capacity(table.columns.len());
            for (_, column) in &table.columns {
                record.push(match column {
                    Column::Float(v) => v[row].map_or(String::new(), |f| format!("{:.3}", f)),
                    Column::Int(v) => v[row].map_or(String::new(), |i| i.to_string()),
                    Column::Str(v) => v[row].clone().unwrap_or_default(),
                    Column::StrList(v) => v[row]
                        .as_ref()
                        .and_then(|l| l.get(copy))
                        .cloned()
                        .unwrap_or_default(),
                });
            }
            w.write_record(&record)?;
        }
    }
    w.flush()?;
    Ok(())
}
