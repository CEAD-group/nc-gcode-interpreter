//! Plain-Rust columnar output.
//!
//! The interpreter used to assemble a polars DataFrame in Rust, which coupled
//! the crate to the whole polars query engine. Instead, the core now produces
//! this simple `Table`; the `python` feature converts it to an Arrow record
//! batch (arrow-rs) handed to Python zero-copy via `pyo3-arrow`, where the
//! wrapper turns it into a polars DataFrame, and the CLI writes CSV directly
//! with the `csv` crate.

use crate::errors::ParsingError;
use crate::modal_groups::{MODAL_G_GROUPS, NON_MODAL_G_GROUPS};
use crate::state::BLOCK_ADDRESSES;
use crate::types::Value;
use std::collections::{HashMap, HashSet};
use std::sync::{Mutex, OnceLock};

/// FxHash map for the hot table-build lookups (`index_of`, per cell per row;
/// `fill`, per column per batch) — trusted `&'static str` keys, no need for
/// SipHash's DoS resistance. See [`crate::state::FxMap`].
type FxMap<K, V> = HashMap<K, V, rustc_hash::FxBuildHasher>;
type FxSet<T> = HashSet<T, rustc_hash::FxBuildHasher>;

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

/// Column-name equality for the per-row scans. Keys are mostly the same
/// interned instance (pointer hit) or differ in length or first byte, so the
/// `memcmp` call is rarely reached.
#[inline]
pub(crate) fn key_eq(a: &str, b: &str) -> bool {
    a.len() == b.len()
        && (std::ptr::eq(a.as_ptr(), b.as_ptr()) || (a.as_bytes().first() == b.as_bytes().first() && a == b))
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
            if key_eq(k, key) {
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

    /// Empty the cells, retaining the backing allocation so a recycled row can
    /// be refilled without a fresh heap allocation (see [`Row::reset_for_reuse`]).
    #[inline]
    pub fn clear(&mut self) {
        self.entries.clear();
    }

    #[inline]
    pub fn get_mut(&mut self, key: &str) -> Option<&mut Value> {
        self.entries.iter_mut().find(|(k, _)| key_eq(k, key)).map(|(_, v)| v)
    }

    #[inline]
    pub fn get(&self, key: &str) -> Option<&Value> {
        self.entries.iter().find(|(k, _)| key_eq(k, key)).map(|(_, v)| v)
    }

    /// Remove `key`, returning its value if present.
    #[inline]
    pub fn remove(&mut self, key: &str) -> Option<Value> {
        let position = self.entries.iter().position(|(k, _)| key_eq(k, key))?;
        Some(self.entries.remove(position).1)
    }

    /// Iterate `(&key, &value)` pairs, mirroring `HashMap::iter` so call sites
    /// that destructure `(&key, value)` keep working unchanged.
    #[inline]
    pub fn iter(&self) -> impl Iterator<Item = (&&'static str, &Value)> {
        self.entries.iter().map(|(k, v)| (k, v))
    }

    /// Move every cell out, leaving the map empty with its allocation kept.
    #[inline]
    pub(crate) fn drain(&mut self) -> impl Iterator<Item = (&'static str, Value)> + '_ {
        self.entries.drain(..)
    }

    /// Iterate the column keys, mirroring `HashMap::keys` (yields `&&'static str`
    /// so `.copied()` recovers the `&'static str`).
    #[inline]
    #[allow(dead_code)] // used via the rlib API
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

impl Row {
    /// Empty this row for reuse as the next `current`, retaining both the cells
    /// and variable-change allocations. On the batch path a finished row's
    /// buffers are consumed into the columnar batch and then recycled here,
    /// which collapses the per-row heap alloc/free (~22M on the 1.1 GB file,
    /// the dominant allocator cost) into a small pool cycled batch-to-batch.
    #[inline]
    pub fn reset_for_reuse(&mut self) {
        self.line_no = 0;
        self.cells.clear();
        self.variable_changes.clear();
    }
}

/// Sparse side-table of variable-change events for the batch path, mirroring
/// what the streaming `nc_to_rows` yields per row as `variable_changes`. Stored
/// columnar (struct-of-arrays) so it converts to an Arrow table cheaply:
///
/// * `row_idx[i]` - the index (0-based, global across batches) of the output
///   row this change is seen at / precedes. A change on an output row gets that
///   row's own index; a change on a variable-only block (no output row) is
///   attributed to the *next* output row, so replaying every event with
///   `row_idx <= k` reconstructs the symbol table as of output row `k`.
/// * `name_id[i]` - index into `names` (interned per run, stable, global).
/// * `value[i]` - the new numeric value assigned.
///
/// Emitting is opt-in (`include_variables`); off, the batch path records
/// nothing (no allocation), exactly as before.
#[derive(Debug, Default, Clone)]
pub struct VariableEvents {
    pub row_idx: Vec<u32>,
    pub name_id: Vec<u32>,
    pub value: Vec<f64>,
    pub names: Vec<String>,
}

impl VariableEvents {
    /// Render the events as an output [`Table`]: `row_idx`/`name_id` as Int64
    /// columns, `value` as Float64. The names list is exposed separately (as a
    /// Python list) so `name_id` indexes into it.
    #[allow(dead_code)] // used by the python-feature bindings, not the bin
    pub fn to_table(&self) -> Table {
        Table {
            columns: vec![
                (
                    "row_idx".to_string(),
                    Column::Int(self.row_idx.iter().map(|&v| Some(v as i64)).collect()),
                ),
                (
                    "name_id".to_string(),
                    Column::Int(self.name_id.iter().map(|&v| Some(v as i64)).collect()),
                ),
                (
                    "value".to_string(),
                    Column::Float(self.value.iter().map(|&v| Some(v)).collect()),
                ),
            ],
        }
    }
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
    /// A batch sink the external-variable provider can flush between rows (#48).
    #[allow(dead_code)]
    SharedBatch(std::sync::Arc<std::sync::Mutex<BatchStreamSink>>),
}

/// Where a batch sink sends its finished tables; `Err` when the consumer is gone.
pub type BatchSend = Box<dyn FnMut(Table) -> Result<(), ()> + Send>;

/// Accumulates finished rows into whole columnar batches on the worker thread.
/// Only cell-bearing rows count toward a batch (variable-only rows are dropped,
/// exactly as the batch table does); every `batch_size` such rows a [`Table`]
/// is built and sent. The [`BatchBuilder`] carries forward-fill state and the
/// growing canonical column set across batches, so - since rows arrive in
/// program order - concatenating the emitted batches reconstructs the whole-file
/// table byte-for-byte.
pub struct BatchStreamSink {
    /// `None` once finished, which releases the channel.
    send: Option<BatchSend>,
    builder: BatchBuilder,
    buffer: Vec<Row>,
    /// Emptied rows kept for reuse: after a batch is built the buffered rows'
    /// allocations are cleared (not freed) into this pool, and `accept` hands
    /// them back to the interpreter to refill instead of allocating fresh.
    recycle: Vec<Row>,
    batch_size: usize,
    /// Whether to accumulate variable-change events (opt-in `include_variables`).
    record_variables: bool,
    /// Unbounded channel the accumulated [`VariableEvents`] are sent on once, at
    /// finish. Unbounded so the send never blocks the worker (the events are
    /// small and only read after the batch stream is drained).
    events_sender: std::sync::mpsc::Sender<VariableEvents>,
    /// Growing sparse event log, sent whole at finish.
    events: VariableEvents,
    /// Interned variable-name -> `name_id`, stable across the run.
    name_ids: HashMap<String, u32>,
    /// Count of cell-bearing (output) rows emitted so far - the `row_idx` a
    /// change is attributed to (see [`VariableEvents`]).
    output_row_count: u32,
}

impl BatchStreamSink {
    /// Buffer a finished row and flush a batch once `batch_size` cell-bearing
    /// rows have accumulated. Variable-only rows carry no output cells and are
    /// dropped from the batch table, but their variable changes are still
    /// recorded into the sparse event log when `record_variables` is on.
    fn accept(&mut self, mut row: Row) -> Result<Option<Row>, ParsingError> {
        if self.record_variables {
            for (name, value) in &row.variable_changes {
                let id = match self.name_ids.get(name) {
                    Some(&id) => id,
                    None => {
                        let id = self.events.names.len() as u32;
                        self.events.names.push(name.clone());
                        self.name_ids.insert(name.clone(), id);
                        id
                    }
                };
                // Recorded at the current output-row count: a change on this row
                // (before the increment below) gets its own index; a change on a
                // variable-only row gets the next output row's index.
                self.events.row_idx.push(self.output_row_count);
                self.events.name_id.push(id);
                self.events.value.push(*value);
            }
        }
        // A variable-only row carries no output cell: it is not buffered, so hand
        // its (cleared) allocation straight back for reuse.
        if row.cells.is_empty() {
            row.reset_for_reuse();
            return Ok(Some(row));
        }
        self.output_row_count += 1;
        self.buffer.push(row);
        if self.buffer.len() >= self.batch_size {
            self.emit()?;
        }
        // Hand back a pooled row (refilled after the last emit) so the caller
        // refills a retained allocation instead of allocating a fresh one.
        Ok(self.recycle.pop())
    }

    /// Send the accumulated variable-change events once, at finish. A no-op when
    /// not recording. Best-effort: a hung-up consumer just misses the events.
    fn send_events(&mut self) {
        if self.record_variables {
            let _ = self.events_sender.send(std::mem::take(&mut self.events));
        }
    }

    /// Build and send the buffered rows as one [`Table`], threading the
    /// forward-fill carry into the next batch. A no-op when the buffer is empty
    /// (no trailing empty batch). Returns `StreamClosed` if the consumer hung up.
    pub(crate) fn emit(&mut self) -> Result<(), ParsingError> {
        if self.buffer.is_empty() {
            return Ok(());
        }
        // Take the buffered rows out to build the batch, then recycle their
        // allocations: `build_batch_owned` moves each cell value into the
        // columnar builders, so once it returns the row buffers are free to be
        // cleared and pooled rather than dropped.
        let mut rows = std::mem::take(&mut self.buffer);
        let table = self.builder.build_batch_owned(&mut rows);
        self.recycle.reserve(rows.len());
        for mut row in rows.drain(..) {
            row.reset_for_reuse();
            self.recycle.push(row);
        }
        // `rows` is now empty but keeps its `Vec<Row>` capacity - reinstate it as
        // the buffer so the outer vector is not reallocated each batch either.
        self.buffer = rows;
        match self.send.as_mut() {
            Some(send) => send(table).map_err(|_| ParsingError::StreamClosed),
            None => Err(ParsingError::StreamClosed),
        }
    }

    /// Send the trailing batch and the variable events, then release the channel.
    fn close(&mut self) -> Result<(), ParsingError> {
        self.emit()?;
        self.send_events();
        self.send = None;
        Ok(())
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
    /// Reused output buffer for the flattener (one allocation per run, not per row).
    flattened: Vec<Row>,
    /// Whether the once-per-run G91 warning has fired (see `flush`).
    warned_g91: bool,
    /// Set while a subprogram runs: the main-program line of the outermost
    /// call and the call path (`OUTER/INNER`), stamped on every row.
    call_context: Option<(usize, String)>,
    /// The call path and subprogram line to stamp on the row being filled.
    current_subprogram: Option<(String, usize)>,
}

impl OutputRows {
    pub fn collect() -> Self {
        OutputRows {
            current: Row::default(),
            sink: RowSink::Collect(Vec::new()),
            record_variables: false,
            flattener: None,
            flattened: Vec::new(),
            warned_g91: false,
            call_context: None,
            current_subprogram: None,
        }
    }

    #[allow(dead_code)] // used by the python-feature bindings, not the bin
    pub fn stream(sender: std::sync::mpsc::SyncSender<Row>) -> Self {
        OutputRows {
            current: Row::default(),
            sink: RowSink::Stream(sender),
            record_variables: true,
            flattener: None,
            flattened: Vec::new(),
            warned_g91: false,
            call_context: None,
            current_subprogram: None,
        }
    }

    /// A batch-streaming sink: finished rows are accumulated into a
    /// `BatchBuilder` on the worker thread and emitted as whole [`Table`]s every
    /// `batch_size` output rows. Variable deltas are not recorded (the batch
    /// table drops variable-only rows), so this behaves like the collect path
    /// for parallelization purposes.
    #[allow(dead_code)] // used by the python-feature bindings, not the bin
    #[allow(clippy::too_many_arguments)]
    pub fn batch_stream(
        sender: std::sync::mpsc::SyncSender<Table>,
        batch_size: usize,
        disable_forward_fill: bool,
        record_variables: bool,
        events_sender: std::sync::mpsc::Sender<VariableEvents>,
    ) -> Self {
        // Back-compatible entry point: never emits line_no.
        Self::batch_stream_with_line_numbers(
            sender,
            batch_size,
            disable_forward_fill,
            record_variables,
            events_sender,
            false,
        )
    }

    /// As [`batch_stream`](Self::batch_stream), but with the opt-in `line_no`
    /// column. Separate constructor so `batch_stream`'s signature stays stable.
    #[allow(clippy::too_many_arguments)]
    pub fn batch_stream_with_line_numbers(
        sender: std::sync::mpsc::SyncSender<Table>,
        batch_size: usize,
        disable_forward_fill: bool,
        record_variables: bool,
        events_sender: std::sync::mpsc::Sender<VariableEvents>,
        emit_line_no: bool,
    ) -> Self {
        Self::batch_stream_to(
            Box::new(move |table| sender.send(table).map_err(|_| ())),
            batch_size,
            disable_forward_fill,
            record_variables,
            events_sender,
            emit_line_no,
        )
    }

    /// As [`batch_stream_with_line_numbers`](Self::batch_stream_with_line_numbers),
    /// sending each table through `send`.
    pub fn batch_stream_to(
        send: BatchSend,
        batch_size: usize,
        disable_forward_fill: bool,
        record_variables: bool,
        events_sender: std::sync::mpsc::Sender<VariableEvents>,
        emit_line_no: bool,
    ) -> Self {
        OutputRows {
            current: Row::default(),
            sink: RowSink::Batch(BatchStreamSink {
                send: Some(send),
                builder: BatchBuilder::new(disable_forward_fill).with_line_numbers(emit_line_no),
                buffer: Vec::new(),
                recycle: Vec::new(),
                batch_size,
                record_variables,
                events_sender,
                events: VariableEvents::default(),
                name_ids: HashMap::new(),
                output_row_count: 0,
            }),
            // Recording variable deltas on rows is what feeds the batch event
            // log: on only when the caller opted in via `include_variables`.
            record_variables,
            flattener: None,
            flattened: Vec::new(),
            warned_g91: false,
            call_context: None,
            current_subprogram: None,
        }
    }

    /// Move a batch sink behind a shared handle and return the handle, so the
    /// external-variable provider can flush finished rows before it is asked.
    /// `None` for sinks that hold no batches.
    pub(crate) fn share_batch_sink(&mut self) -> Option<std::sync::Arc<std::sync::Mutex<BatchStreamSink>>> {
        let sink = std::mem::replace(&mut self.sink, RowSink::Collect(Vec::new()));
        match sink {
            RowSink::Batch(sink) => {
                let shared = std::sync::Arc::new(std::sync::Mutex::new(sink));
                self.sink = RowSink::SharedBatch(shared.clone());
                Some(shared)
            }
            RowSink::SharedBatch(shared) => {
                self.sink = RowSink::SharedBatch(shared.clone());
                Some(shared)
            }
            other => {
                self.sink = other;
                None
            }
        }
    }

    /// Set or clear the subprogram stamp for the rows started from here on,
    /// returning the previous one.
    pub(crate) fn set_call_context(&mut self, context: Option<(usize, String)>) -> Option<(usize, String)> {
        std::mem::replace(&mut self.call_context, context)
    }

    pub(crate) fn call_context(&self) -> Option<&(usize, String)> {
        self.call_context.as_ref()
    }

    /// Install a curve flattener: every subsequent row passes through it on
    /// its way to the sink (arcs and splines come out as sampled G1 runs).
    pub fn set_flattener(&mut self, flattener: crate::flatten::Flattener) {
        self.flattener = Some(flattener);
    }

    /// Number of committed rows in a `Collect` sink (0 for streaming sinks).
    /// Lets the experimental resumable VM (#47) pause at a row boundary.
    #[allow(dead_code)]
    pub(crate) fn collected_len(&self) -> usize {
        match &self.sink {
            RowSink::Collect(rows) => rows.len(),
            _ => 0,
        }
    }

    /// Deep-clone a checkpointable `Collect`-sink output: the committed rows,
    /// the in-flight row, and the warn latches. `None` when the sink is a
    /// streaming channel or a flattener is installed (their state isn't
    /// snapshottable this simply). For the experimental resumable VM (#47);
    /// the whole-file forward-fill happens later in `Table::from_rows`, so a
    /// Collect snapshot needs no columnar carry.
    #[allow(dead_code)]
    pub(crate) fn snapshot_collect(&self) -> Option<OutputRows> {
        if self.flattener.is_some() {
            return None;
        }
        match &self.sink {
            RowSink::Collect(rows) => Some(OutputRows {
                current: self.current.clone(),
                sink: RowSink::Collect(rows.clone()),
                record_variables: self.record_variables,
                flattener: None,
                flattened: Vec::new(),
                warned_g91: self.warned_g91,
                call_context: self.call_context.clone(),
                current_subprogram: self.current_subprogram.clone(),
            }),
            _ => None,
        }
    }

    /// Return a recycled row to the sink's pool. Used when a single `deliver`
    /// yields more recycled rows than the one the caller can reuse as `current`
    /// (the flattener fans one block out to many rows): the surplus goes back to
    /// the pool instead of being freed, so recycling stays effective on
    /// flattened paths too. A no-op for the non-recycling sinks.
    fn reclaim(&mut self, row: Row) {
        match &mut self.sink {
            RowSink::Batch(sink) => sink.recycle.push(row),
            RowSink::SharedBatch(sink) => sink.lock().expect("batch sink mutex poisoned").recycle.push(row),
            _ => {}
        }
    }

    /// Route a row to the sink, passing it through the flattener first when one
    /// is installed, and returning an emptied row the sink no longer needs
    /// (batch path) so the caller can reuse its allocation as the next
    /// `current`. `None` when nothing is available to recycle.
    fn deliver(&mut self, row: Row) -> Result<Option<Row>, ParsingError> {
        if let Some(mut flattener) = self.flattener.take() {
            let mut flattened = std::mem::take(&mut self.flattened);
            flattener.push(row, &mut flattened);
            self.flattener = Some(flattener);
            // Keep one recycled row for the caller to reuse; hand any others the
            // sink returns this call straight back to the pool so they are not
            // freed (a flattened block delivers many rows in one `deliver`).
            let mut recycled = None;
            let mut result = Ok(());
            for row in flattened.drain(..) {
                if result.is_err() {
                    continue;
                }
                match self.deliver_to_sink(row) {
                    Ok(Some(r)) => {
                        if let Some(prev) = recycled.replace(r) {
                            self.reclaim(prev);
                        }
                    }
                    Ok(None) => {}
                    Err(error) => result = Err(error),
                }
            }
            self.flattened = flattened;
            return result.map(|()| recycled);
        }
        self.deliver_to_sink(row)
    }

    /// Route a finished row to the sink: collected, streamed row-at-a-time, or
    /// fed to the worker-side batch producer.
    fn deliver_to_sink(&mut self, row: Row) -> Result<Option<Row>, ParsingError> {
        match &mut self.sink {
            RowSink::Collect(rows) => {
                rows.push(row);
                Ok(None)
            }
            // The receiver hung up: the consumer stopped iterating. Abort
            // interpretation instead of running the rest of the program.
            RowSink::Stream(sender) => sender.send(row).map(|_| None).map_err(|_| ParsingError::StreamClosed),
            RowSink::Batch(sink) => sink.accept(row),
            RowSink::SharedBatch(sink) => sink.lock().expect("batch sink mutex poisoned").accept(row),
        }
    }

    fn flush(&mut self) -> Result<(), ParsingError> {
        if self.current.cells.is_empty() && self.current.variable_changes.is_empty() {
            return Ok(());
        }
        let mut row = std::mem::take(&mut self.current);
        // Stamped here, not at start: a block that yields no cell stays empty.
        if let Some((path, line)) = self.current_subprogram.take() {
            if !row.cells.is_empty() {
                row.cells.insert(SUBPROGRAM_COLUMN, Value::Str(path));
                row.cells.insert(SUBPROGRAM_LINE_COLUMN, Value::Float(line as f64));
            }
        }
        rekey_g4_dwell(&mut row);
        // G91 is parsed but incremental dimensioning is NOT applied: every
        // axis value is emitted as if absolute, so positions are wrong from
        // this block on. Loud once-per-run warning - never butcher silently.
        if !self.warned_g91 {
            if matches!(row.cells.get("gg14_wp_measure_mode"), Some(Value::Str(code)) if code == "G91") {
                crate::state::emit_warning(format_args!(
                    "Warning [line {}]: G91 incremental dimensioning is not interpreted - axis values are emitted as absolute positions and will be wrong from this block on",
                    row.line_no
                ));
                self.warned_g91 = true;
            }
        }
        // `mem::take` above left `current` as an empty (unallocated) default; if
        // the sink handed back a pooled row, adopt its retained allocation for
        // the next block instead of allocating fresh on the first cell insert.
        if let Some(recycled) = self.deliver(row)? {
            self.current = recycled;
        }
        Ok(())
    }

    /// Begin the row for the block at `line_no`, flushing the previous row.
    pub fn start_row(&mut self, line_no: usize) -> Result<(), ParsingError> {
        self.flush()?;
        match &self.call_context {
            Some((call_line, name)) => {
                self.current.line_no = *call_line;
                self.current_subprogram = Some((name.clone(), line_no));
            }
            None => {
                self.current.line_no = line_no;
                self.current_subprogram = None;
            }
        }
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
                sink.close()?;
                Ok(Vec::new())
            }
            RowSink::SharedBatch(sink) => {
                sink.lock().expect("batch sink mutex poisoned").close()?;
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
    /// The G-group columns: a few distinct values, repeated over long runs.
    Dict(DictColumn),
    StrList(Vec<Option<Vec<String>>>),
}

/// A string column stored as one code per row into the batch's distinct
/// values, so filling a modal G-group down a batch writes integers instead of
/// allocating a `String` per row.
#[derive(Debug, Clone, Default)]
pub struct DictColumn {
    /// Distinct values, in order of first appearance in this batch.
    pub values: Vec<String>,
    /// Per row: an index into `values`, or [`DictColumn::NULL`].
    pub codes: Vec<u32>,
}

/// Equal when the cells are equal, whatever order the values were first seen in.
impl PartialEq for DictColumn {
    fn eq(&self, other: &Self) -> bool {
        self.len() == other.len() && self.iter().eq(other.iter())
    }
}

impl DictColumn {
    /// The code of a null cell.
    pub const NULL: u32 = u32::MAX;

    pub fn len(&self) -> usize {
        self.codes.len()
    }

    #[allow(dead_code)] // used via the rlib API
    pub fn is_empty(&self) -> bool {
        self.codes.is_empty()
    }

    /// The value at `row`, `None` for a null cell.
    pub fn get(&self, row: usize) -> Option<&str> {
        self.values.get(self.codes[row] as usize).map(String::as_str)
    }

    /// The column's cells in row order.
    pub fn iter(&self) -> impl Iterator<Item = Option<&str>> + '_ {
        self.codes
            .iter()
            .map(|&code| self.values.get(code as usize).map(String::as_str))
    }

    /// The code for `value`, adding it to `values` when new.
    fn code_for(&mut self, value: &str) -> u32 {
        match self.values.iter().position(|v| v == value) {
            Some(code) => code as u32,
            None => {
                self.values.push(value.to_string());
                (self.values.len() - 1) as u32
            }
        }
    }
}

impl Column {
    pub fn len(&self) -> usize {
        match self {
            Column::Float(v) => v.len(),
            Column::Int(v) => v.len(),
            Column::Str(v) => v.len(),
            Column::Dict(v) => v.len(),
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
            Column::Str(_) | Column::Dict(_) => "str",
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
            Column::Dict(column) => {
                let mut last = match seed {
                    Some(seed) => column.code_for(&seed.into_str()),
                    None => DictColumn::NULL,
                };
                for code in column.codes.iter_mut() {
                    if *code == DictColumn::NULL {
                        *code = last;
                    } else {
                        last = *code;
                    }
                }
                column.values.get(last as usize).cloned().map(Carry::Str)
            }
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

/// Text of a numeric tool selection (`T2`, `T=3`) for the string `T` column.
pub(crate) fn tool_number_text(value: f64) -> String {
    if value.fract() == 0.0 && value.abs() < 1e15 {
        format!("{}", value as i64)
    } else {
        format!("{value}")
    }
}

/// The call path of the subprogram that produced a row (`OUTER/INNER`); null
/// for main-program rows.
pub const SUBPROGRAM_COLUMN: &str = "subprogram";
/// The line of that row's block inside the innermost subprogram's text.
pub const SUBPROGRAM_LINE_COLUMN: &str = "subprogram_line";

pub fn is_string_column(name: &str) -> bool {
    MODAL_G_GROUPS.contains(&name)
        || NON_MODAL_G_GROUPS.contains(&name)
        || matches!(
            name,
            "T" | "non_returning_function_call" | "comment" | SUBPROGRAM_COLUMN
        )
}

/// Whether a column is forward-filled in the sanitized table: value columns
/// (anything typed numeric by name, except the spline block addresses) and
/// the modal G-group columns. Shared by the batch table and the streaming
/// iterator so both fill identically.
pub fn is_forward_filled_column(name: &str) -> bool {
    let is_value = name != "M"
        && name != FLATTENED_COLUMN
        && name != DWELL_COLUMN
        && name != SUBPROGRAM_LINE_COLUMN
        && !is_string_column(name)
        && !BLOCK_ADDRESSES.contains(&name);
    is_value || MODAL_G_GROUPS.contains(&name)
}

/// Per-block dwell-time column: on a `G4` block the `F` word is the dwell
/// time in seconds (`S` the dwell in spindle revolutions) - a block-local
/// parameter, NOT a feed/speed change. Like the block addresses it is never
/// forward-filled.
pub const DWELL_COLUMN: &str = "dwell";

/// On a G4 block, move the F (or, failing that, S) value out of the modal
/// feed/speed columns into the per-block [`DWELL_COLUMN`]. Without this the
/// dwell time forward-fills as the feed rate (`G4 F0.01` leaves F = 0.01
/// mm/min for every following block until the next real F word), corrupting
/// any downstream time computation.
fn rekey_g4_dwell(row: &mut Row) {
    let is_g4 = matches!(row.cells.get("gg02_wait"), Some(Value::Str(code)) if code == "G4");
    if !is_g4 {
        return;
    }
    // Consume BOTH F and S: whichever is not the dwell value must still not
    // forward-fill into the modal feed/spindle columns.
    let f = row.cells.remove("F");
    let s = row.cells.remove("S");
    if let Some(value) = f.or(s) {
        row.cells.insert(intern_column(DWELL_COLUMN), value);
    }
}

/// Marker column emitted by the curve flattener: `1.0` on rows it generated
/// (intermediate polyline samples), absent on programmed positions - so
/// filtering on null recovers the original toolpath points. Per-row like the
/// block addresses: never forward-filled.
pub const FLATTENED_COLUMN: &str = "flattened";

/// Leading column carrying each output row's 1-based source line number (the
/// `Row::line_no` the streaming path yields). One value per row, never
/// forward-filled and never null; loops and jumps produce repeated /
/// non-monotonic values, exactly matching `nc_to_rows`. Not a cell key, so it
/// sits outside `canonical_order` and is prepended to every batch. Opt-in via
/// the batch/dataframe `include_line_numbers` flag (default off).
pub const LINE_NO_COLUMN: &str = "line_no";

/// Canonical output-column order over the set of columns present so far:
/// N, modal then non-modal G-group columns, the fixed axis columns, any
/// remaining value columns (e.g. user extra axes) in alphabetical order, the
/// spline/arc block addresses, then T, M, function calls and comment. Column
/// names are `&'static str` (constant vocabulary or interned row keys), so the
/// order is comparison-by-content and independent of which `&'static str`
/// instance carries a given name.
fn canonical_order(present: &FxSet<&'static str>) -> Vec<&'static str> {
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
                && *name != DWELL_COLUMN
                && !matches!(
                    *name,
                    "T" | "M" | "non_returning_function_call" | "comment" | SUBPROGRAM_COLUMN | SUBPROGRAM_LINE_COLUMN
                )
        })
        .collect();
    extra.sort_unstable();
    ordered.extend(extra);
    // Block addresses (spline PW/SD/PL) come after the axes.
    for &name in BLOCK_ADDRESSES {
        push_if_present(name, &mut ordered);
    }
    push_if_present(intern_column(DWELL_COLUMN), &mut ordered);
    push_if_present(intern_column(FLATTENED_COLUMN), &mut ordered);
    for name in [
        "T",
        "M",
        "non_returning_function_call",
        SUBPROGRAM_COLUMN,
        SUBPROGRAM_LINE_COLUMN,
        "comment",
    ] {
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
    /// Prepend the `line_no` source-line column to every batch (opt-in).
    emit_line_no: bool,
    /// Columns seen in any batch so far, in canonical order. Grows monotonically
    /// so a column, once emitted, stays in every later batch (forward-filled or
    /// null), which is what lets the batches concatenate back to the whole
    /// table.
    columns: Vec<&'static str>,
    /// Last carried non-null value per forward-filled column.
    fill: FxMap<&'static str, Carry>,
    /// Position in `columns` of a cell key, by the key's address and length:
    /// rows reuse a few interned key instances, so this skips hashing the
    /// name for every cell. Cleared whenever `columns` changes.
    slots: FxMap<(usize, usize), u32>,
}

impl BatchBuilder {
    pub fn new(disable_forward_fill: bool) -> Self {
        BatchBuilder {
            disable_forward_fill,
            emit_line_no: false,
            columns: Vec::new(),
            fill: FxMap::default(),
            slots: FxMap::default(),
        }
    }

    /// Opt into the leading `line_no` column (default off). Builder-style so
    /// `BatchBuilder::new(..)` stays a stable single-argument constructor for
    /// existing Rust callers.
    pub fn with_line_numbers(mut self, emit_line_no: bool) -> Self {
        self.emit_line_no = emit_line_no;
        self
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
        // streaming-only concern). Keep the source line number alongside the
        // cells for when `emit_line_no` is set.
        self.build(
            rows.iter()
                .filter(|r| !r.cells.is_empty())
                .map(|r| (r.line_no, BatchCells::Shared(&r.cells)))
                .collect(),
        )
    }

    /// As [`build_batch`](Self::build_batch), but moves the cell values out of
    /// `rows` (left empty) instead of cloning their strings.
    pub(crate) fn build_batch_owned(&mut self, rows: &mut [Row]) -> Table {
        self.build(
            rows.iter_mut()
                .filter(|r| !r.cells.is_empty())
                .map(|r| (r.line_no, BatchCells::Owned(&mut r.cells)))
                .collect(),
        )
    }

    fn build(&mut self, mut kept: Vec<(usize, BatchCells)>) -> Table {
        let height = kept.len();
        // One typed builder per known column, each pre-filled with `height`
        // nulls. A column first seen in this batch gets a builder appended and
        // is moved to its canonical place after the pass.
        let mut names: Vec<&'static str> = self.columns.clone();
        let mut builders: Vec<ColumnBuilder> = names.iter().map(|&name| ColumnBuilder::new(name, height)).collect();

        // Single row-major pass: dispatch each present cell to its column
        // builder. Interning does not guarantee one instance per name, so a
        // key instance not seen before is matched by content once.
        let slots = &mut self.slots;
        for (row_index, (_, cells)) in kept.iter_mut().enumerate() {
            cells.for_each(|key, value| {
                let instance = (key.as_ptr() as usize, key.len());
                let position = match slots.get(&instance) {
                    Some(&position) => position as usize,
                    None => {
                        let position = names.iter().position(|&name| name == key).unwrap_or_else(|| {
                            names.push(key);
                            builders.push(ColumnBuilder::new(key, height));
                            names.len() - 1
                        });
                        slots.insert(instance, position as u32);
                        position
                    }
                };
                builders[position].set(row_index, value);
            });
        }

        if names.len() != self.columns.len() {
            // New columns: grow the canonical set and reorder the builders.
            let present: FxSet<&'static str> = names.iter().copied().collect();
            self.columns = canonical_order(&present);
            let mut by_name: FxMap<&'static str, ColumnBuilder> = names.into_iter().zip(builders).collect();
            builders = self
                .columns
                .iter()
                .map(|name| by_name.remove(name).expect("every present column has a builder"))
                .collect();
            self.slots.clear();
        }

        // Opt-in (default off): the source line number leads every batch, one
        // value per kept row, never null and never forward-filled. It is not a
        // cell key, so it stays out of `self.columns` / `canonical_order` and is
        // simply prepended. Default off keeps the output schema unchanged.
        let extra = usize::from(self.emit_line_no);
        let mut columns: Vec<(String, Column)> = Vec::with_capacity(self.columns.len() + extra);
        if self.emit_line_no {
            let line_no_column = Column::Int(kept.iter().map(|&(line_no, _)| Some(line_no as i64)).collect());
            columns.push((LINE_NO_COLUMN.to_string(), line_no_column));
        }
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

/// One kept row's cells for [`BatchBuilder::build`]: borrowed (values are
/// cloned) or owned by a row about to be recycled (values are moved out).
enum BatchCells<'a> {
    Shared(&'a CellMap),
    Owned(&'a mut CellMap),
}

impl BatchCells<'_> {
    fn for_each(&mut self, mut f: impl FnMut(&'static str, Value)) {
        match self {
            BatchCells::Shared(cells) => cells.iter().for_each(|(&key, value)| f(key, value.clone())),
            BatchCells::Owned(cells) => cells.drain().for_each(|(key, value)| f(key, value)),
        }
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
    Dict(DictColumn),
    StrList(Vec<Option<Vec<String>>>),
}

impl ColumnBuilder {
    fn new(name: &str, height: usize) -> Self {
        if name == "N" || name == SUBPROGRAM_LINE_COLUMN {
            ColumnBuilder::Int(vec![None; height])
        } else if name == "M" {
            ColumnBuilder::StrList(vec![None; height])
        } else if MODAL_G_GROUPS.contains(&name) || NON_MODAL_G_GROUPS.contains(&name) {
            ColumnBuilder::Dict(DictColumn {
                values: Vec::new(),
                codes: vec![DictColumn::NULL; height],
            })
        } else if is_string_column(name) {
            ColumnBuilder::Str(vec![None; height])
        } else {
            ColumnBuilder::Float(vec![None; height])
        }
    }

    fn set(&mut self, row_index: usize, value: Value) {
        match self {
            ColumnBuilder::Int(column) => {
                // Block numbers: original integer lexeme, float fallback.
                column[row_index] = match value {
                    Value::Str(s) => s
                        .parse::<i64>()
                        .ok()
                        .or_else(|| s.parse::<f64>().ok().map(|v| v as i64)),
                    Value::Float(f) => Some(f as i64),
                    _ => None,
                };
            }
            ColumnBuilder::StrList(column) => {
                column[row_index] = match value {
                    Value::StrList(l) => Some(l),
                    Value::Str(s) => Some(vec![s]),
                    _ => None,
                };
            }
            ColumnBuilder::Str(column) => {
                column[row_index] = match value {
                    Value::Str(s) => Some(s),
                    Value::Float(f) => Some(f.to_string()),
                    _ => None,
                };
            }
            ColumnBuilder::Dict(column) => {
                column.codes[row_index] = match value {
                    Value::Str(s) => column.code_for(&s),
                    Value::Float(f) => column.code_for(&f.to_string()),
                    _ => DictColumn::NULL,
                };
            }
            ColumnBuilder::Float(column) => {
                column[row_index] = match value {
                    Value::Float(f) => Some(f),
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
            ColumnBuilder::Dict(v) => Column::Dict(v),
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
        // The whole-file / CLI path never emits line_no (opt-in, default off).
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
                    Column::Dict(v) => v.get(row).unwrap_or_default().to_string(),
                    Column::StrList(v) => v[row].as_ref().and_then(|l| l.get(copy)).cloned().unwrap_or_default(),
                });
            }
            w.write_record(&record)?;
        }
    }
    w.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Moving cell values out of recycled rows builds the same batches as
    /// cloning them, forward-fill carry included.
    #[test]
    fn owned_batches_match_borrowed_batches() {
        let mut rows: Vec<Row> = (0..6)
            .map(|i| {
                let mut row = Row {
                    line_no: i + 1,
                    ..Row::default()
                };
                if i != 2 {
                    row.cells.insert(intern_column("X"), Value::Float(i as f64));
                }
                if i % 2 == 0 {
                    row.cells
                        .insert(intern_column("gg01_motion"), Value::Str(format!("G{}", i % 4)));
                }
                if i == 3 {
                    row.cells.insert("M", Value::StrList(vec!["M3".into(), "M8".into()]));
                    row.cells.insert("N", Value::Str("30".into()));
                }
                row
            })
            .collect();
        let mut shared = BatchBuilder::new(false).with_line_numbers(true);
        let mut owned = BatchBuilder::new(false).with_line_numbers(true);
        for batch in rows.chunks_mut(4) {
            let expected = shared.build_batch(batch);
            let got = owned.build_batch_owned(batch);
            assert_eq!(expected.columns, got.columns);
            assert!(batch.iter().all(|row| row.cells.is_empty()));
        }
    }

    /// Text of every cell of a column, `None` for a null.
    fn cell_text(column: &Column) -> Vec<Option<String>> {
        match column {
            Column::Float(v) => v.iter().map(|c| c.map(|f| f.to_string())).collect(),
            Column::Int(v) => v.iter().map(|c| c.map(|i| i.to_string())).collect(),
            Column::Str(v) => v.clone(),
            Column::Dict(v) => v.iter().map(|c| c.map(str::to_string)).collect(),
            Column::StrList(v) => v.iter().map(|c| c.as_ref().map(|l| l.join(" "))).collect(),
        }
    }

    /// Rows where the G-group values change, go missing, and a column (`Y`,
    /// then `gg06_plane_select`) first appears after the first batch.
    fn mixed_rows(count: usize) -> Vec<Row> {
        (0..count)
            .map(|i| {
                let mut row = Row {
                    line_no: i + 1,
                    ..Row::default()
                };
                row.cells.insert(intern_column("X"), Value::Float(i as f64));
                if i % 5 == 1 {
                    row.cells
                        .insert(intern_column("gg01_motion"), Value::Str(format!("G{}", i % 3)));
                }
                if i == 7 {
                    row.cells.insert(intern_column("gg02_wait"), Value::Str("G4".into()));
                }
                if i >= 9 {
                    row.cells.insert(intern_column("Y"), Value::Float(-(i as f64)));
                }
                if i == 13 {
                    row.cells
                        .insert(intern_column("gg06_plane_select"), Value::Str("G18".into()));
                }
                row
            })
            .collect()
    }

    /// Batches of any size concatenate to the whole-table build: G-group
    /// columns carry their value across batch boundaries, and a column that
    /// first appears in a later batch lands in its canonical position.
    #[test]
    fn batches_concatenate_to_the_whole_table() {
        let rows = mixed_rows(20);
        for disable_forward_fill in [false, true] {
            let whole = Table::from_rows(&rows, disable_forward_fill);
            let names: Vec<&str> = whole.columns.iter().map(|(name, _)| name.as_str()).collect();
            assert_eq!(names, ["gg01_motion", "gg06_plane_select", "gg02_wait", "X", "Y"]);
            for batch_size in [1, 3, 4, 7, 20] {
                let mut builder = BatchBuilder::new(disable_forward_fill);
                let mut offset = 0;
                for batch in rows.chunks(batch_size) {
                    let table = builder.build_batch(batch);
                    for (name, column) in &table.columns {
                        let (_, expected) = whole.columns.iter().find(|(n, _)| n == name).unwrap();
                        assert_eq!(
                            cell_text(column),
                            cell_text(expected)[offset..offset + batch.len()],
                            "column {name}, batch size {batch_size}, rows from {offset}"
                        );
                    }
                    // A column absent from this batch is all null in the whole table so far.
                    for (name, expected) in &whole.columns {
                        if !table.columns.iter().any(|(n, _)| n == name) {
                            assert!(cell_text(expected)[..offset + batch.len()].iter().all(Option::is_none));
                        }
                    }
                    offset += batch.len();
                }
            }
        }
    }

    /// G-group columns are dictionary columns: the modal one fills down, the
    /// non-modal one stays on its own row.
    #[test]
    fn g_group_columns_are_dictionary_encoded() {
        let table = Table::from_rows(&mixed_rows(10), false);
        let column = |name: &str| &table.columns.iter().find(|(n, _)| n == name).unwrap().1;
        let Column::Dict(motion) = column("gg01_motion") else {
            panic!("gg01_motion is not a dictionary column");
        };
        assert_eq!(motion.values, ["G1", "G0"]);
        assert_eq!(motion.get(0), None);
        assert_eq!(motion.get(5), Some("G1"));
        assert_eq!(motion.get(6), Some("G0"));
        let Column::Dict(wait) = column("gg02_wait") else {
            panic!("gg02_wait is not a dictionary column");
        };
        assert_eq!(wait.iter().filter(|cell| cell.is_some()).count(), 1);
        assert_eq!(wait.get(7), Some("G4"));
        assert_eq!(column("gg01_motion").dtype_name(), "str");
    }
}
