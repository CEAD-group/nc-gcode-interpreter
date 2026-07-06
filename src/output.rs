//! Plain-Rust columnar output.
//!
//! The interpreter used to assemble a polars DataFrame in Rust, which coupled
//! the crate to a specific polars / pyo3-polars / Python-polars version trio.
//! Instead, the core now produces this simple `Table`; the Python wrapper
//! turns it into a polars DataFrame on the Python side, and the CLI writes
//! CSV directly with the `csv` crate.

use crate::errors::ParsingError;
use crate::modal_groups::{MODAL_G_GROUPS, NON_MODAL_G_GROUPS};
use crate::state::BLOCK_ADDRESSES;
use crate::types::Value;
use std::collections::{HashMap, HashSet};

/// One interpreter output row: the source line it came from, the values the
/// block produced, and the variables the block assigned. Loops and jumps emit
/// repeated / non-monotonic line numbers - exactly what a visualizer needs to
/// map trace rows to source.
#[derive(Debug, Default, Clone)]
pub struct Row {
    pub line_no: usize,
    pub cells: HashMap<String, Value>,
    /// Variable assignments this block performed (`R1=R1+1`, `DEF REAL Q=5`,
    /// FOR-loop counter updates), in program order with repeats preserved.
    /// Replaying `variable_changes` row by row reconstructs the symbol table
    /// as it stood at any point of the stream; the batch table ignores them.
    pub variable_changes: Vec<(String, f64)>,
}

/// Where finished rows go: collected for the batch table, or pushed into a
/// bounded channel that a streaming consumer drains while interpretation is
/// still running.
enum RowSink {
    Collect(Vec<Row>),
    #[allow(dead_code)] // constructed by the python-feature bindings, not the bin
    Stream(std::sync::mpsc::SyncSender<Row>),
}

/// The interpreter's output handle. A block starts a row with `start_row`;
/// statements fill it via `last_mut`; starting the next row (or finishing)
/// flushes the previous one to the sink. Empty rows - blocks that only
/// affected internal state - are dropped at flush time, mirroring the old
/// whole-table pruning.
pub struct OutputRows {
    current: Row,
    sink: RowSink,
}

impl OutputRows {
    pub fn collect() -> Self {
        OutputRows {
            current: Row::default(),
            sink: RowSink::Collect(Vec::new()),
        }
    }

    #[allow(dead_code)] // used by the python-feature bindings, not the bin
    pub fn stream(sender: std::sync::mpsc::SyncSender<Row>) -> Self {
        OutputRows {
            current: Row::default(),
            sink: RowSink::Stream(sender),
        }
    }

    fn flush(&mut self) -> Result<(), ParsingError> {
        if self.current.cells.is_empty() && self.current.variable_changes.is_empty() {
            return Ok(());
        }
        let row = std::mem::take(&mut self.current);
        match &mut self.sink {
            RowSink::Collect(rows) => {
                rows.push(row);
                Ok(())
            }
            // The receiver hung up: the consumer stopped iterating. Abort
            // interpretation instead of running the rest of the program.
            RowSink::Stream(sender) => sender.send(row).map_err(|_| ParsingError::StreamClosed),
        }
    }

    /// Begin the row for the block at `line_no`, flushing the previous row.
    pub fn start_row(&mut self, line_no: usize) -> Result<(), ParsingError> {
        self.flush()?;
        self.current.line_no = line_no;
        Ok(())
    }

    /// The row currently being filled. Named after `Vec::last_mut`, which
    /// this type replaced; always `Some`.
    pub fn last_mut(&mut self) -> Option<&mut HashMap<String, Value>> {
        Some(&mut self.current.cells)
    }

    /// Record a variable assignment on the row currently being filled.
    /// Only the streaming iterator consumes variable deltas, so this is a
    /// no-op in collect mode: the batch path keeps pruning variable-only
    /// rows at flush and carries no delta allocations.
    pub fn record_variable_change(&mut self, key: &str, value: f64) {
        if matches!(self.sink, RowSink::Stream(_)) {
            self.current.variable_changes.push((key.to_string(), value));
        }
    }

    /// Flush the trailing row and return the collected rows (empty when
    /// streaming).
    pub fn finish(mut self) -> Result<Vec<Row>, ParsingError> {
        self.flush()?;
        Ok(match self.sink {
            RowSink::Collect(rows) => rows,
            RowSink::Stream(_) => Vec::new(),
        })
    }
}

/// A single typed column. Values are optional: `None` is a null cell.
#[derive(Debug, Clone, PartialEq)]
pub enum Column {
    Float(Vec<Option<f64>>),
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

pub fn is_string_column(name: &str) -> bool {
    MODAL_G_GROUPS.contains(&name)
        || NON_MODAL_G_GROUPS.contains(&name)
        || matches!(name, "T" | "non_returning_function_call" | "comment")
}

/// Whether a column is forward-filled in the sanitized table: value columns
/// (anything typed numeric by name, except the spline block addresses) and
/// the modal G-group columns. Shared by the batch table and the streaming
/// iterator so both fill identically.
pub fn is_forward_filled_column(name: &str) -> bool {
    let is_value = name != "M" && !is_string_column(name) && !BLOCK_ADDRESSES.contains(&name);
    is_value || MODAL_G_GROUPS.contains(&name)
}

impl Table {
    /// Build a sanitized table from interpreter rows: typed columns in the
    /// canonical order (N, G-group columns, axes, other value columns, T, M,
    /// function calls, comment), with axis and modal G-group columns
    /// forward-filled unless disabled.
    pub fn from_rows(rows: &[Row], disable_forward_fill: bool) -> Table {
        // Skip rows that carry no output values (blocks that only affected
        // internal state, e.g. definitions - their variable_changes are a
        // streaming-only concern).
        let rows: Vec<&HashMap<String, Value>> = rows.iter().map(|r| &r.cells).filter(|r| !r.is_empty()).collect();

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
                    && !BLOCK_ADDRESSES.contains(name)
                    && !matches!(*name, "T" | "M" | "non_returning_function_call" | "comment")
            })
            .collect();
        extra.sort_unstable();
        for name in extra {
            ordered.push(name.to_string());
        }
        // Block addresses (spline PW/SD/PL) come after the axes.
        for &name in BLOCK_ADDRESSES {
            push_if_present(name, &mut ordered);
        }
        for name in ["T", "M", "non_returning_function_call", "comment"] {
            push_if_present(name, &mut ordered);
        }

        let mut columns: Vec<(String, Column)> = Vec::with_capacity(ordered.len());
        for name in ordered {
            let mut column = build_column(&name, &rows);
            // Block addresses (spline PW/SD/PL) are never forward-filled: a
            // point weight applies only to the point it is programmed with.
            if !disable_forward_fill && is_forward_filled_column(&name) {
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
        // Block numbers are stored as their original integer lexeme; expose
        // them as integers (float fallback for legacy float-formatted values).
        let data = rows
            .iter()
            .map(|r| match r.get(name) {
                Some(Value::Str(s)) => s
                    .parse::<i64>()
                    .ok()
                    .or_else(|| s.parse::<f64>().ok().map(|v| v as i64)),
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
            Some(Value::Str(s)) => s.parse::<f64>().ok(),
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
