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
use std::sync::{Mutex, OnceLock};

/// Intern a column name to a process-stable `&'static str`.
///
/// The set of distinct output-column names is a small closed vocabulary
/// (axes, block addresses, G-group short names, and the fixed
/// `N`/`M`/`T`/`comment`/`non_returning_function_call` columns), so the pool
/// stays tiny and is consulted only when a *new* name is first seen - never
/// per row. Output rows then carry `&'static str` keys, which are `Copy`: a
/// row cell costs no per-cell heap allocation for its key. The one-time leak
/// of each distinct name is bounded and shared across every run in the
/// process (repeated identical names reuse the same `&'static str`).
pub fn intern_column(name: &str) -> &'static str {
    static POOL: OnceLock<Mutex<HashSet<&'static str>>> = OnceLock::new();
    let pool = POOL.get_or_init(|| Mutex::new(HashSet::new()));
    let mut guard = pool.lock().expect("column intern pool poisoned");
    if let Some(&existing) = guard.get(name) {
        return existing;
    }
    let leaked: &'static str = Box::leak(name.to_owned().into_boxed_str());
    guard.insert(leaked);
    leaked
}

/// A row's output cells: interned column name -> value, in first-insert order.
///
/// A block produces only a handful of cells (~5-9: a few axes, one or two
/// G-group columns, `N`, maybe `comment`/`M`), so a linear-scan `Vec` beats a
/// `HashMap` here: it skips the per-row control-buffer allocation *and* the
/// SipHash of every key, both of which the old `HashMap<&'static str, Value>`
/// paid ~537k times on the Havoc file and ~22M times on the 1.1 GB file.
/// Ordering is irrelevant to the output (columns are placed by
/// `canonical_order`, and cells dispatch to builders by key), so the only
/// invariant that matters is last-write-wins on a repeated key - which
/// `insert` preserves, exactly as the map did.
#[derive(Debug, Default, Clone)]
pub struct CellMap {
    entries: Vec<(&'static str, Value)>,
}

impl CellMap {
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Insert `value` for `key`, replacing the current value if `key` is already
    /// present (last-write-wins, matching `HashMap::insert`). Linear scan over
    /// the handful of cells a block carries.
    #[inline]
    pub fn insert(&mut self, key: &'static str, value: Value) -> Option<Value> {
        for (k, v) in self.entries.iter_mut() {
            if *k == key {
                return Some(std::mem::replace(v, value));
            }
        }
        // A block's cells fit in a single small allocation; reserve once up
        // front so a row never reallocates while filling.
        if self.entries.is_empty() {
            self.entries.reserve(8);
        }
        self.entries.push((key, value));
        None
    }

    #[inline]
    pub fn get_mut(&mut self, key: &str) -> Option<&mut Value> {
        self.entries.iter_mut().find(|(k, _)| *k == key).map(|(_, v)| v)
    }

    #[inline]
    pub fn get(&self, key: &str) -> Option<&Value> {
        self.entries.iter().find(|(k, _)| *k == key).map(|(_, v)| v)
    }

    /// Remove `key`, returning its value if present.
    #[inline]
    pub fn remove(&mut self, key: &str) -> Option<Value> {
        let position = self.entries.iter().position(|(k, _)| *k == key)?;
        Some(self.entries.remove(position).1)
    }

    /// Iterate `(&key, &value)` pairs, mirroring `HashMap::iter` so call sites
    /// that destructure `(&key, value)` keep working unchanged.
    #[inline]
    pub fn iter(&self) -> impl Iterator<Item = (&&'static str, &Value)> {
        self.entries.iter().map(|(k, v)| (k, v))
    }

    /// Iterate the column keys, mirroring `HashMap::keys` (yields `&&'static str`
    /// so `.copied()` recovers the `&'static str`).
    #[inline]
    pub fn keys(&self) -> impl Iterator<Item = &&'static str> {
        self.entries.iter().map(|(k, _)| k)
    }
}

/// One interpreter output row: the source line it came from, the values the
/// block produced, and the variables the block assigned. Loops and jumps emit
/// repeated / non-monotonic line numbers - exactly what a visualizer needs to
/// map trace rows to source.
#[derive(Debug, Default, Clone)]
pub struct Row {
    pub line_no: usize,
    /// Column keys are interned `&'static str` (see `intern_column`): the hot
    /// per-block output path allocates no heap String per cell key.
    pub cells: CellMap,
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
    /// Worker-side batch producer: finished rows are accumulated into a
    /// `BatchBuilder` and, every `batch_size` output rows, a completed columnar
    /// [`Table`] is sent across a bounded channel. This moves columnar
    /// materialization off the consumer thread and collapses the per-row channel
    /// traffic into a handful of whole-batch messages.
    #[allow(dead_code)] // constructed by the python-feature bindings, not the bin
    Batch(BatchStreamSink),
}

/// Accumulates finished rows into whole columnar batches on the worker thread.
/// Only cell-bearing rows count toward a batch (variable-only rows are dropped,
/// exactly as the batch table does); every `batch_size` such rows a [`Table`]
/// is built and sent. The [`BatchBuilder`] carries forward-fill state and the
/// growing canonical column set across batches, so - since rows arrive in
/// program order - concatenating the emitted batches reconstructs the whole-file
/// table byte-for-byte.
pub struct BatchStreamSink {
    sender: std::sync::mpsc::SyncSender<Table>,
    builder: BatchBuilder,
    buffer: Vec<Row>,
    batch_size: usize,
}

impl BatchStreamSink {
    /// Buffer a finished row and flush a batch once `batch_size` cell-bearing
    /// rows have accumulated. Variable-only rows carry no output cells and are
    /// dropped (they never reach the batch table).
    fn accept(&mut self, row: Row) -> Result<(), ParsingError> {
        if row.cells.is_empty() {
            return Ok(());
        }
        self.buffer.push(row);
        if self.buffer.len() >= self.batch_size {
            self.emit()?;
        }
        Ok(())
    }

    /// Build and send the buffered rows as one [`Table`], threading the
    /// forward-fill carry into the next batch. A no-op when the buffer is empty
    /// (no trailing empty batch). Returns `StreamClosed` if the consumer hung up.
    fn emit(&mut self) -> Result<(), ParsingError> {
        if self.buffer.is_empty() {
            return Ok(());
        }
        let rows = std::mem::take(&mut self.buffer);
        let table = self.builder.build_batch(&rows);
        self.sender.send(table).map_err(|_| ParsingError::StreamClosed)
    }
}

/// The interpreter's output handle. A block starts a row with `start_row`;
/// statements fill it via `last_mut`; starting the next row (or finishing)
/// flushes the previous one to the sink. Empty rows - blocks that only
/// affected internal state - are dropped at flush time, mirroring the old
/// whole-table pruning.
pub struct OutputRows {
    current: Row,
    sink: RowSink,
    /// Whether per-block variable deltas are recorded on rows. On for the
    /// streaming sink (the row iterator consumes them); off for the batch/table
    /// collect path, which prunes variable-only rows.
    record_variables: bool,
    /// Optional curve flattener sitting between the interpreter and the
    /// sink: arc and spline rows are replaced by sampled runs of G1 rows
    /// before they reach the sink (see [`crate::flatten`]).
    flattener: Option<crate::flatten::Flattener>,
}

impl OutputRows {
    pub fn collect() -> Self {
        OutputRows {
            current: Row::default(),
            sink: RowSink::Collect(Vec::new()),
            record_variables: false,
            flattener: None,
        }
    }

    #[allow(dead_code)] // used by the python-feature bindings, not the bin
    pub fn stream(sender: std::sync::mpsc::SyncSender<Row>) -> Self {
        OutputRows {
            current: Row::default(),
            sink: RowSink::Stream(sender),
            record_variables: true,
            flattener: None,
        }
    }

    /// A batch-streaming sink: finished rows are accumulated into a
    /// `BatchBuilder` on the worker thread and emitted as whole [`Table`]s every
    /// `batch_size` output rows. Variable deltas are not recorded (the batch
    /// table drops variable-only rows), so this behaves like the collect path
    /// for parallelization purposes.
    #[allow(dead_code)] // used by the python-feature bindings, not the bin
    pub fn batch_stream(
        sender: std::sync::mpsc::SyncSender<Table>,
        batch_size: usize,
        disable_forward_fill: bool,
    ) -> Self {
        OutputRows {
            current: Row::default(),
            sink: RowSink::Batch(BatchStreamSink {
                sender,
                builder: BatchBuilder::new(disable_forward_fill),
                buffer: Vec::new(),
                batch_size,
            }),
            record_variables: false,
            flattener: None,
        }
    }

    /// Install a curve flattener: every subsequent row passes through it on
    /// its way to the sink (arcs and splines come out as sampled G1 runs).
    pub fn set_flattener(&mut self, flattener: crate::flatten::Flattener) {
        self.flattener = Some(flattener);
    }

    /// Route a finished row to the sink, passing it through the flattener
    /// first when one is installed. Shared by `flush`.
    fn deliver(&mut self, row: Row) -> Result<(), ParsingError> {
        if let Some(mut flattener) = self.flattener.take() {
            let mut flattened = Vec::new();
            flattener.push(row, &mut flattened);
            self.flattener = Some(flattener);
            for row in flattened {
                self.deliver_to_sink(row)?;
            }
            return Ok(());
        }
        self.deliver_to_sink(row)
    }

    /// Route a finished row to the sink: collected, streamed row-at-a-time, or
    /// fed to the worker-side batch producer.
    fn deliver_to_sink(&mut self, row: Row) -> Result<(), ParsingError> {
        match &mut self.sink {
            RowSink::Collect(rows) => {
                rows.push(row);
                Ok(())
            }
            // The receiver hung up: the consumer stopped iterating. Abort
            // interpretation instead of running the rest of the program.
            RowSink::Stream(sender) => sender.send(row).map_err(|_| ParsingError::StreamClosed),
            RowSink::Batch(sink) => sink.accept(row),
        }
    }

    fn flush(&mut self) -> Result<(), ParsingError> {
        if self.current.cells.is_empty() && self.current.variable_changes.is_empty() {
            return Ok(());
        }
        let row = std::mem::take(&mut self.current);
        self.deliver(row)
    }

    /// Begin the row for the block at `line_no`, flushing the previous row.
    pub fn start_row(&mut self, line_no: usize) -> Result<(), ParsingError> {
        self.flush()?;
        self.current.line_no = line_no;
        Ok(())
    }

    /// The row currently being filled. Named after `Vec::last_mut`, which
    /// this type replaced; always `Some`.
    pub fn last_mut(&mut self) -> Option<&mut CellMap> {
        Some(&mut self.current.cells)
    }

    /// Record a variable assignment on the row currently being filled.
    /// Only the streaming iterator consumes variable deltas, so this is a
    /// no-op in collect mode: the batch path keeps pruning variable-only
    /// rows at flush and carries no delta allocations.
    pub fn record_variable_change(&mut self, key: &str, value: f64) {
        if self.record_variables {
            self.current.variable_changes.push((key.to_string(), value));
        }
    }

    /// Flush the trailing row and return the collected rows (empty when
    /// streaming).
    pub fn finish(mut self) -> Result<Vec<Row>, ParsingError> {
        self.flush()?;
        // A program ending inside a spline still owes its buffered curve.
        if let Some(mut flattener) = self.flattener.take() {
            let mut flattened = Vec::new();
            flattener.finish(&mut flattened);
            for row in flattened {
                self.deliver_to_sink(row)?;
            }
        }
        match self.sink {
            RowSink::Collect(rows) => Ok(rows),
            RowSink::Stream(_) => Ok(Vec::new()),
            // Send the trailing partial batch (the buffered rows since the last
            // full batch), then close the channel by dropping the sender.
            RowSink::Batch(mut sink) => {
                sink.emit()?;
                Ok(Vec::new())
            }
        }
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

    /// Forward-fill null cells with the last non-null value, seeded by the
    /// value carried from the previous batch (`None` for the first / only
    /// batch). Returns the last non-null value after filling - the seed for the
    /// next batch (or the incoming seed unchanged when the batch was all-null).
    /// Passing `None` reproduces the whole-table forward-fill exactly.
    fn forward_fill_seeded(&mut self, seed: Option<Carry>) -> Option<Carry> {
        fn fill<T: Clone>(v: &mut [Option<T>], seed: Option<T>) -> Option<T> {
            let mut last: Option<T> = seed;
            for cell in v.iter_mut() {
                match cell {
                    Some(value) => last = Some(value.clone()),
                    None => *cell = last.clone(),
                }
            }
            last
        }
        match self {
            Column::Float(v) => fill(v, seed.map(Carry::into_float)).map(Carry::Float),
            Column::Int(v) => fill(v, seed.map(Carry::into_int)).map(Carry::Int),
            Column::Str(v) => fill(v, seed.map(Carry::into_str)).map(Carry::Str),
            Column::StrList(v) => fill(v, seed.map(Carry::into_str_list)).map(Carry::StrList),
        }
    }
}

/// A single forward-fill carry value, matching the column type it belongs to.
/// The column a name maps to is stable across batches (`X` is always a float
/// column, `N` always an int column, ...), so the carried value is always read
/// back into the same variant.
#[derive(Debug, Clone)]
enum Carry {
    Float(f64),
    Int(i64),
    Str(String),
    StrList(Vec<String>),
}

impl Carry {
    fn into_float(self) -> f64 {
        match self {
            Carry::Float(v) => v,
            _ => unreachable!("carry type mismatch for a float column"),
        }
    }
    fn into_int(self) -> i64 {
        match self {
            Carry::Int(v) => v,
            _ => unreachable!("carry type mismatch for an int column"),
        }
    }
    fn into_str(self) -> String {
        match self {
            Carry::Str(v) => v,
            _ => unreachable!("carry type mismatch for a str column"),
        }
    }
    fn into_str_list(self) -> Vec<String> {
        match self {
            Carry::StrList(v) => v,
            _ => unreachable!("carry type mismatch for a list column"),
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
    let is_value = name != "M"
        && name != FLATTENED_COLUMN
        && !is_string_column(name)
        && !BLOCK_ADDRESSES.contains(&name);
    is_value || MODAL_G_GROUPS.contains(&name)
}

/// Marker column emitted by the curve flattener: `1.0` on rows it generated
/// (intermediate polyline samples), absent on programmed positions - so
/// filtering on null recovers the original toolpath points. Per-row like the
/// block addresses: never forward-filled.
pub const FLATTENED_COLUMN: &str = "flattened";

/// Canonical output-column order over the set of columns present so far:
/// N, modal then non-modal G-group columns, the fixed axis columns, any
/// remaining value columns (e.g. user extra axes) in alphabetical order, the
/// spline/arc block addresses, then T, M, function calls and comment. Column
/// names are `&'static str` (constant vocabulary or interned row keys), so the
/// order is comparison-by-content and independent of which `&'static str`
/// instance carries a given name.
fn canonical_order(present: &HashSet<&'static str>) -> Vec<&'static str> {
    let mut ordered: Vec<&'static str> = Vec::new();
    let push_if_present = |name: &'static str, ordered: &mut Vec<&'static str>| {
        if present.contains(name) && !ordered.contains(&name) {
            ordered.push(name);
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
    let mut extra: Vec<&'static str> = present
        .iter()
        .copied()
        .filter(|name| {
            !ordered.contains(name)
                && !BLOCK_ADDRESSES.contains(name)
                && *name != FLATTENED_COLUMN
                && !matches!(*name, "T" | "M" | "non_returning_function_call" | "comment")
        })
        .collect();
    extra.sort_unstable();
    ordered.extend(extra);
    // Block addresses (spline PW/SD/PL) come after the axes.
    for &name in BLOCK_ADDRESSES {
        push_if_present(name, &mut ordered);
    }
    push_if_present(intern_column(FLATTENED_COLUMN), &mut ordered);
    for name in ["T", "M", "non_returning_function_call", "comment"] {
        push_if_present(name, &mut ordered);
    }
    ordered
}

/// Incremental, column-wise output builder. Produces one [`Table`] per batch of
/// rows while carrying forward-fill state and the growing canonical column set
/// across batches, so concatenating the batches reconstructs the whole-file
/// table. A fresh builder handed all rows in a single call reproduces the old
/// whole-table `Table::from_rows` byte-for-byte - which is exactly how
/// `from_rows` is now implemented.
pub struct BatchBuilder {
    disable_forward_fill: bool,
    /// Columns seen in any batch so far, in canonical order. Grows monotonically
    /// so a column, once emitted, stays in every later batch (forward-filled or
    /// null), which is what lets the batches concatenate back to the whole
    /// table.
    columns: Vec<&'static str>,
    /// Last carried non-null value per forward-filled column.
    fill: HashMap<&'static str, Carry>,
}

impl BatchBuilder {
    pub fn new(disable_forward_fill: bool) -> Self {
        BatchBuilder {
            disable_forward_fill,
            columns: Vec::new(),
            fill: HashMap::new(),
        }
    }

    /// Build the [`Table`] for one batch of rows, updating the carried
    /// forward-fill state and canonical column set.
    ///
    /// This is a single row-major pass: each row's cells are dispatched to
    /// their column builder (rows carry only a handful of cells each), instead
    /// of re-scanning every row once per column. The result is byte-identical
    /// to a per-column scan - each column builder pre-fills nulls and only the
    /// cells actually present overwrite them.
    pub fn build_batch(&mut self, rows: &[Row]) -> Table {
        // Skip rows that carry no output values (blocks that only affected
        // internal state, e.g. definitions - their variable_changes are a
        // streaming-only concern).
        let cells: Vec<&CellMap> =
            rows.iter().map(|r| &r.cells).filter(|r| !r.is_empty()).collect();

        // Union of every column seen so far with those present in this batch.
        let mut present: HashSet<&'static str> = self.columns.iter().copied().collect();
        present.extend(cells.iter().flat_map(|r| r.keys().copied()));
        self.columns = canonical_order(&present);

        let height = cells.len();
        // One typed builder per column, in canonical order, each pre-filled
        // with `height` nulls. A name->position index lets each cell find its
        // builder in O(1).
        let mut index_of: HashMap<&'static str, usize> = HashMap::with_capacity(self.columns.len());
        let mut builders: Vec<ColumnBuilder> = Vec::with_capacity(self.columns.len());
        for (position, &name) in self.columns.iter().enumerate() {
            index_of.insert(name, position);
            builders.push(ColumnBuilder::new(name, height));
        }

        // Single pass: dispatch each present cell to its column builder. Every
        // cell key is in `present` (hence in `index_of`), so the lookup always
        // hits; the `if let` is defensive only.
        for (row_index, cell) in cells.iter().enumerate() {
            for (&key, value) in cell.iter() {
                if let Some(&position) = index_of.get(key) {
                    builders[position].set(row_index, value);
                }
            }
        }

        let mut columns: Vec<(String, Column)> = Vec::with_capacity(self.columns.len());
        for (&name, builder) in self.columns.iter().zip(builders) {
            let mut column = builder.into_column();
            // Block addresses (spline PW/SD/PL) are never forward-filled: a
            // point weight applies only to the point it is programmed with.
            if !self.disable_forward_fill && is_forward_filled_column(name) {
                let seed = self.fill.get(name).cloned();
                if let Some(last) = column.forward_fill_seeded(seed) {
                    self.fill.insert(name, last);
                }
            }
            columns.push((name.to_string(), column));
        }
        Table { columns }
    }
}

/// A typed, null-prefilled column under construction. Mirrors [`Column`] but
/// accepts cells one at a time by row index (`set`), converting each [`Value`]
/// with the same name-based typing rules as the old per-column scan. Empty
/// cells stay the pre-filled `None`.
enum ColumnBuilder {
    Float(Vec<Option<f64>>),
    Int(Vec<Option<i64>>),
    Str(Vec<Option<String>>),
    StrList(Vec<Option<Vec<String>>>),
}

impl ColumnBuilder {
    fn new(name: &str, height: usize) -> Self {
        if name == "N" {
            ColumnBuilder::Int(vec![None; height])
        } else if name == "M" {
            ColumnBuilder::StrList(vec![None; height])
        } else if is_string_column(name) {
            ColumnBuilder::Str(vec![None; height])
        } else {
            ColumnBuilder::Float(vec![None; height])
        }
    }

    fn set(&mut self, row_index: usize, value: &Value) {
        match self {
            ColumnBuilder::Int(column) => {
                // Block numbers: original integer lexeme, float fallback.
                column[row_index] = match value {
                    Value::Str(s) => s
                        .parse::<i64>()
                        .ok()
                        .or_else(|| s.parse::<f64>().ok().map(|v| v as i64)),
                    Value::Float(f) => Some(*f as i64),
                    _ => None,
                };
            }
            ColumnBuilder::StrList(column) => {
                column[row_index] = match value {
                    Value::StrList(l) => Some(l.clone()),
                    Value::Str(s) => Some(vec![s.clone()]),
                    _ => None,
                };
            }
            ColumnBuilder::Str(column) => {
                column[row_index] = match value {
                    Value::Str(s) => Some(s.clone()),
                    Value::Float(f) => Some(f.to_string()),
                    _ => None,
                };
            }
            ColumnBuilder::Float(column) => {
                column[row_index] = match value {
                    Value::Float(f) => Some(*f),
                    Value::Str(s) => s.parse::<f64>().ok(),
                    _ => None,
                };
            }
        }
    }

    fn into_column(self) -> Column {
        match self {
            ColumnBuilder::Float(v) => Column::Float(v),
            ColumnBuilder::Int(v) => Column::Int(v),
            ColumnBuilder::Str(v) => Column::Str(v),
            ColumnBuilder::StrList(v) => Column::StrList(v),
        }
    }
}

impl Table {
    /// Build a sanitized table from interpreter rows: typed columns in the
    /// canonical order (N, G-group columns, axes, other value columns, T, M,
    /// function calls, comment), with axis and modal G-group columns
    /// forward-filled unless disabled. Equivalent to a single [`BatchBuilder`]
    /// batch over all rows.
    pub fn from_rows(rows: &[Row], disable_forward_fill: bool) -> Table {
        BatchBuilder::new(disable_forward_fill).build_batch(rows)
    }

    pub fn height(&self) -> usize {
        self.columns.first().map_or(0, |(_, c)| c.len())
    }
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
