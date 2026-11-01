//! Resumable batch runs for structure-free programs (#47, option A).
//!
//! A program the structure scan classifies as structure-free and whose
//! preflight finds no jump runs on the streaming line driver, where every
//! line is a top-level block and the interpreter's whole position is the
//! line cursor plus [`State`]. At a line boundary the run can therefore be
//! snapshotted exactly: a [`Checkpoint`] holds a `State` clone, the cursor
//! (line number and byte offset), the output row and variable-event counts,
//! and the output side's carried state (forward-fill carry and column set,
//! interned variable names, the flattener's positions and modes, warn
//! latches). A checkpoint is only taken where nothing is pending: the
//! in-flight row is flushed, no spline is buffered and the partial batch has
//! been emitted.
//!
//! Resuming from a checkpoint with a new program text whose bytes before the
//! checkpoint are unchanged runs only the suffix, and produces exactly the
//! rows, variable events and final state a full run of the new text produces
//! from that row on. Programs with control structures or jumps are not
//! resumable (option C, a reified control stack, would be needed).

use crate::errors::ParsingError;
use crate::flatten::Flattener;
use crate::interpreter::{build_state, install_flattener, interpret_file, interpret_source, SourceText};
use crate::output::{BatchBuilder, OutputRows, Table, VariableEvents};
use crate::state::State;
use std::collections::HashMap;
use std::sync::mpsc::{Sender, SyncSender};
use std::sync::{Arc, Mutex};

/// The checkpoints of one run, in program order, shared between the
/// interpreter thread that appends them and the session that hands them out.
pub type CheckpointList = Arc<Mutex<Vec<Arc<Checkpoint>>>>;

/// Whether a run can take checkpoints: [`UNKNOWN`] until the driver is
/// chosen, which happens before the first row.
pub type Resumability = Arc<std::sync::atomic::AtomicU8>;
pub const UNKNOWN: u8 = 0;
pub const RESUMABLE: u8 = 1;
pub const NOT_RESUMABLE: u8 = 2;

/// Configuration of a resumable batch run; fixed for a session because a
/// checkpoint is only valid under the configuration that produced it.
#[derive(Clone, Debug)]
pub struct SessionConfig {
    pub initial_state: Option<String>,
    pub axis_identifiers: Option<Vec<String>>,
    pub extra_axes: Option<Vec<String>>,
    pub iteration_limit: usize,
    pub disable_forward_fill: bool,
    pub axis_index_map: Option<HashMap<String, usize>>,
    pub allow_undefined_variables: bool,
    pub flatten_tolerance: Option<f64>,
    pub batch_size: usize,
    pub emit_line_no: bool,
    pub include_variables: bool,
    /// Take a checkpoint once this many output rows passed since the last
    /// one (0 = no row interval).
    pub every_rows: usize,
    /// Take a checkpoint once this many source lines passed since the last
    /// one (0 = no line interval).
    pub every_lines: usize,
}

/// Output-side state carried across a checkpoint.
#[derive(Clone)]
pub(crate) struct OutputSnapshot {
    pub(crate) builder: BatchBuilder,
    pub(crate) output_row_count: u32,
    pub(crate) frozen_events: Vec<Arc<VariableEvents>>,
    pub(crate) names: Vec<String>,
    pub(crate) name_ids: HashMap<String, u32>,
    pub(crate) flattener: Option<Flattener>,
    pub(crate) warned_g91: bool,
}

/// A resumable position in a run: everything before `line_no` has executed.
#[derive(Clone)]
pub struct Checkpoint {
    /// The session whose configuration produced this checkpoint.
    pub session_id: u64,
    /// 1-based number of the first line a resume executes.
    pub line_no: usize,
    /// Byte offset of that line in the program text.
    pub byte_offset: usize,
    /// Output rows emitted before the checkpoint: the global index of the
    /// first row a resume emits.
    pub row_idx: usize,
    /// Variable events recorded before the checkpoint.
    pub event_idx: usize,
    state: State,
    output: OutputSnapshot,
}

impl Checkpoint {
    /// The program text the checkpoint was taken in.
    fn source(&self) -> &str {
        self.state.source().as_str()
    }
}

/// Decides when a checkpoint is due and records it. Lives on the output
/// (see [`OutputRows::line_boundary`]).
pub struct Checkpointer {
    session_id: u64,
    every_rows: usize,
    every_lines: usize,
    last_line: usize,
    last_rows: usize,
    list: CheckpointList,
    resumable: Resumability,
}

impl Checkpointer {
    /// `line_index` (0-based) and `rows` are where counting starts: the
    /// program start for a full run, the checkpoint for a resume.
    pub fn new(
        session_id: u64,
        config: &SessionConfig,
        line_index: usize,
        rows: usize,
        list: CheckpointList,
        resumable: Resumability,
    ) -> Self {
        Checkpointer {
            session_id,
            every_rows: config.every_rows,
            every_lines: config.every_lines,
            last_line: line_index,
            last_rows: rows,
            list,
            resumable,
        }
    }

    /// Record which driver the run took: only the streaming one checkpoints.
    pub(crate) fn set_resumable(&self, resumable: bool) {
        let value = if resumable { RESUMABLE } else { NOT_RESUMABLE };
        self.resumable.store(value, std::sync::atomic::Ordering::Release);
    }

    #[inline]
    pub(crate) fn due(&self, line_index: usize, rows: usize) -> bool {
        (self.every_rows > 0 && rows - self.last_rows >= self.every_rows)
            || (self.every_lines > 0 && line_index - self.last_line >= self.every_lines)
    }

    pub(crate) fn record(
        &mut self,
        line_index: usize,
        byte_offset: usize,
        row_idx: usize,
        event_idx: usize,
        state: &State,
        output: OutputSnapshot,
    ) {
        self.last_line = line_index;
        self.last_rows = row_idx;
        let checkpoint = Checkpoint {
            session_id: self.session_id,
            line_no: line_index + 1,
            byte_offset,
            row_idx,
            event_idx,
            state: state.clone(),
            output,
        };
        self.list
            .lock()
            .expect("checkpoint list mutex poisoned")
            .push(Arc::new(checkpoint));
    }
}

/// Why a resume was refused. The caller falls back to a full run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotResumable {
    /// The new text differs from the old before the checkpoint.
    PrefixChanged,
    /// The new text has control structures (IF/WHILE/FOR/LOOP/REPEAT/CASE).
    Structured,
    /// The new text has a jump after the checkpoint.
    Jumps,
    /// The checkpoint belongs to another session (another configuration).
    ForeignCheckpoint,
    /// The streaming line driver is disabled (`NC_STAGE1=0`) or bypassed
    /// (`NC_STREAM_MIN_BYTES`).
    Disabled,
}

impl NotResumable {
    pub fn reason(self) -> &'static str {
        match self {
            NotResumable::PrefixChanged => "prefix_changed",
            NotResumable::Structured => "structured",
            NotResumable::Jumps => "jumps",
            NotResumable::ForeignCheckpoint => "foreign_checkpoint",
            NotResumable::Disabled => "disabled",
        }
    }

    pub fn message(self, checkpoint: &Checkpoint) -> String {
        match self {
            NotResumable::PrefixChanged => format!(
                "the program changed before line {}, where the checkpoint was taken; resume from an earlier checkpoint or run in full",
                checkpoint.line_no
            ),
            NotResumable::Structured => {
                "the program has control structures (IF/WHILE/FOR/LOOP/REPEAT/CASE); only structure-free programs can resume".to_string()
            }
            NotResumable::Jumps => format!(
                "the program has a jump (GOTO/GOTOF/GOTOB/GOTOC/GOTOS) after line {}; only jump-free programs can resume",
                checkpoint.line_no
            ),
            NotResumable::ForeignCheckpoint => "the checkpoint belongs to another session".to_string(),
            NotResumable::Disabled => {
                "the streaming line driver is disabled (NC_STAGE1 or NC_STREAM_MIN_BYTES), so the program cannot resume"
                    .to_string()
            }
        }
    }
}

/// Length of the longest common byte prefix of `a` and `b`.
pub fn common_prefix_len(a: &str, b: &str) -> usize {
    const CHUNK: usize = 4096;
    let (a, b) = (a.as_bytes(), b.as_bytes());
    let n = a.len().min(b.len());
    let mut i = 0;
    // Whole-chunk slice compares are memcmp; only the differing chunk is
    // scanned byte by byte.
    while i + CHUNK <= n && a[i..i + CHUNK] == b[i..i + CHUNK] {
        i += CHUNK;
    }
    while i < n && a[i] == b[i] {
        i += 1;
    }
    i
}

/// A resume the checks admitted: the checkpoint's state moved onto the new
/// text, and `deferred`, a parse error in the suffix that the run reports
/// (like a full run reports it) instead of refusing.
pub struct ResumePlan {
    state: State,
    deferred: Option<ParsingError>,
}

impl ResumePlan {
    /// `checkpoint`, whose text equals the resumed text up to it, moved onto
    /// the resumed text, so a kept checkpoint does not keep the old text alive.
    pub fn rebase(&self, checkpoint: &Checkpoint) -> Arc<Checkpoint> {
        let mut rebased = checkpoint.clone();
        rebased.state.share_source(&self.state);
        Arc::new(rebased)
    }
}

/// Check that `checkpoint` can resume with `input`, cheapest checks first:
/// the unchanged prefix, the driver switches, the structure scan and the
/// preflight of the suffix (syntax and jumps). `known_common` is the length
/// of the common prefix of `input` and `known_text` when the caller already
/// compared them; when `known_text` is the checkpoint's own text the prefix
/// is not compared again.
pub fn plan_resume(
    checkpoint: &Checkpoint,
    input: &SourceText,
    known: Option<(&str, usize)>,
) -> Result<ResumePlan, NotResumable> {
    let text = input.as_str();
    let offset = checkpoint.byte_offset;
    let old = checkpoint.source();
    let unchanged = match known {
        Some((known_text, common)) if std::ptr::eq(known_text, old) => common >= offset,
        _ => text.len() >= offset && text.as_bytes()[..offset] == old.as_bytes()[..offset],
    };
    if !unchanged {
        return Err(NotResumable::PrefixChanged);
    }
    if !crate::line_driver::stage1_enabled() || !crate::line_driver::streams(text) {
        return Err(NotResumable::Disabled);
    }
    // The prefix ran on the streaming driver, so it has no structures: the
    // suffix decides.
    let suffix = &text[offset..];
    match crate::structure_scan::check_structures(suffix) {
        Ok(shape) if !shape.has_block_structures => {}
        _ => return Err(NotResumable::Structured),
    }
    let mut state = checkpoint.state.clone();
    state.set_source_with_prefix(input.clone(), offset);
    match crate::line_driver::preflight_suffix(text, offset, checkpoint.line_no - 1, &state) {
        Ok(true) => Ok(ResumePlan { state, deferred: None }),
        Ok(false) => Err(NotResumable::Jumps),
        Err(error) => Ok(ResumePlan {
            state,
            deferred: Some(error),
        }),
    }
}

/// A full batch run that takes checkpoints into `list` (see
/// [`crate::interpreter::nc_to_batch_stream_with_line_numbers`]).
pub fn run_checkpointed(
    input: SourceText,
    config: &SessionConfig,
    session_id: u64,
    list: CheckpointList,
    resumable: Resumability,
    sender: SyncSender<Table>,
    events_sender: Sender<VariableEvents>,
) -> Result<State, ParsingError> {
    let mut state = build_state(
        config.axis_identifiers.clone(),
        config.extra_axes.clone(),
        config.iteration_limit,
        config.axis_index_map.clone(),
        config.allow_undefined_variables,
    );
    if let Some(initial_state) = &config.initial_state {
        let mut discard = OutputRows::collect();
        interpret_file(initial_state, &mut state, &mut discard)?;
    }
    let mut output = OutputRows::batch_stream_with_line_numbers(
        sender,
        config.batch_size,
        config.disable_forward_fill,
        config.include_variables,
        events_sender,
        config.emit_line_no,
    );
    install_flattener(&mut output, &state, config.flatten_tolerance)?;
    output.set_checkpointer(Checkpointer::new(session_id, config, 0, 0, list, resumable));
    interpret_source(input, &mut state, &mut output)?;
    output.finish()?;
    Ok(state)
}

/// Run `input` from `checkpoint` on: the batches hold the rows from
/// `checkpoint.row_idx` on, the variable events cover the whole program, and
/// new checkpoints are appended to `list`. `plan` comes from [`plan_resume`]
/// for the same checkpoint and input.
pub fn resume_checkpointed(
    checkpoint: &Checkpoint,
    plan: ResumePlan,
    input: SourceText,
    config: &SessionConfig,
    list: CheckpointList,
    sender: SyncSender<Table>,
    events_sender: Sender<VariableEvents>,
) -> Result<State, ParsingError> {
    if let Some(error) = plan.deferred {
        return Err(error);
    }
    let mut state = plan.state;
    let mut output = OutputRows::resume_batch_stream(
        sender,
        config.batch_size,
        config.include_variables,
        events_sender,
        &checkpoint.output,
    );
    output.set_checkpointer(Checkpointer::new(
        checkpoint.session_id,
        config,
        checkpoint.line_no - 1,
        checkpoint.row_idx,
        list,
        Resumability::new(RESUMABLE.into()),
    ));
    crate::line_driver::resume_streaming(
        input.as_str(),
        checkpoint.byte_offset,
        checkpoint.line_no - 1,
        &mut output,
        &mut state,
    )?;
    output.finish()?;
    Ok(state)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    fn config(every_rows: usize, include_variables: bool) -> SessionConfig {
        SessionConfig {
            initial_state: None,
            axis_identifiers: None,
            extra_axes: None,
            iteration_limit: 10000,
            disable_forward_fill: false,
            axis_index_map: None,
            allow_undefined_variables: false,
            flatten_tolerance: None,
            batch_size: 4,
            emit_line_no: true,
            include_variables,
            every_rows,
            every_lines: 0,
        }
    }

    struct Outcome {
        tables: Vec<Table>,
        events: Option<VariableEvents>,
        state: State,
    }

    fn collect(run: impl FnOnce(SyncSender<Table>, Sender<VariableEvents>) -> Result<State, ParsingError>) -> Outcome {
        let (sender, receiver) = mpsc::sync_channel(1 << 16);
        let (events_sender, events_receiver) = mpsc::channel();
        let state = run(sender, events_sender).expect("run");
        Outcome {
            tables: receiver.try_iter().collect(),
            events: events_receiver.try_iter().next(),
            state,
        }
    }

    fn full(text: &str, config: &SessionConfig, list: &CheckpointList) -> Outcome {
        collect(|s, e| {
            run_checkpointed(
                SourceText::from(text),
                config,
                7,
                list.clone(),
                Resumability::default(),
                s,
                e,
            )
        })
    }

    fn program(edit: Option<(usize, &str)>) -> String {
        (1..=40)
            .map(|i| match edit {
                Some((line, text)) if line == i => text.to_string(),
                _ if i % 5 == 0 => format!("R{} = {}", i % 3, i),
                _ => format!("G1 X{} Y{}", i, i * 2),
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn snapshot_site_is_a_clean_line_boundary() {
        let config = config(8, true);
        let list = CheckpointList::default();
        let original = full(&program(None), &config, &list);
        let checkpoints = list.lock().unwrap().clone();
        assert!(checkpoints.len() >= 3, "{} checkpoints", checkpoints.len());
        let mut previous_rows = 0;
        for checkpoint in &checkpoints {
            // Taken at a line start, after the row count crossed the interval.
            let text = program(None);
            assert!(checkpoint.byte_offset == 0 || text.as_bytes()[checkpoint.byte_offset - 1] == b'\n');
            assert_eq!(
                text[..checkpoint.byte_offset].matches('\n').count() + 1,
                checkpoint.line_no
            );
            assert!(checkpoint.row_idx - previous_rows >= 8);
            previous_rows = checkpoint.row_idx;
        }
        // The batches split at every checkpoint: their heights add up to them.
        let mut boundaries = vec![0];
        for table in &original.tables {
            boundaries.push(boundaries.last().unwrap() + table.height());
        }
        for checkpoint in &checkpoints {
            assert!(boundaries.contains(&checkpoint.row_idx));
        }

        // Resume after an edit equals the full run of the edited text.
        let middle = checkpoints[1].clone();
        let edited = program(Some((middle.line_no + 3, "G91 X1 F200")));
        let fresh = CheckpointList::default();
        let expected = full(&edited, &config, &fresh);
        let input = SourceText::from(edited.as_str());
        let plan = plan_resume(&middle, &input, None).expect("resumable");
        let resumed_list = CheckpointList::default();
        let resumed =
            collect(|s, e| resume_checkpointed(&middle, plan, input.clone(), &config, resumed_list.clone(), s, e));
        let suffix: Vec<&Table> = expected
            .tables
            .iter()
            .scan(0, |rows, table| {
                let start = *rows;
                *rows += table.height();
                Some((start, table))
            })
            .filter(|(start, _)| *start >= middle.row_idx)
            .map(|(_, table)| table)
            .collect();
        assert_eq!(resumed.tables.len(), suffix.len());
        for (got, want) in resumed.tables.iter().zip(suffix) {
            assert_eq!(&got.columns, &want.columns);
        }
        let (got, want) = (resumed.events.unwrap(), expected.events.unwrap());
        assert_eq!(
            (got.row_idx, got.name_id, got.value, got.names),
            (want.row_idx, want.name_id, want.value, want.names)
        );
        assert_eq!(resumed.state.final_state().axes, expected.state.final_state().axes);
        assert_eq!(resumed.state.symbol_table, expected.state.symbol_table);
        // The resumed run takes the same later checkpoints as the full run.
        let later: Vec<_> = fresh
            .lock()
            .unwrap()
            .iter()
            .filter(|c| c.line_no > middle.line_no)
            .map(|c| (c.line_no, c.row_idx, c.event_idx))
            .collect();
        let again: Vec<_> = resumed_list
            .lock()
            .unwrap()
            .iter()
            .map(|c| (c.line_no, c.row_idx, c.event_idx))
            .collect();
        assert_eq!(later, again);
    }

    #[test]
    fn edits_before_the_checkpoint_and_jumps_are_refused() {
        let config = config(8, false);
        let list = CheckpointList::default();
        full(&program(None), &config, &list);
        let checkpoint = list.lock().unwrap()[1].clone();
        let before = SourceText::from(program(Some((2, "G1 X99"))));
        assert_eq!(
            plan_resume(&checkpoint, &before, None).err(),
            Some(NotResumable::PrefixChanged)
        );
        let jump = SourceText::from(program(Some((checkpoint.line_no + 1, "GOTOB START"))));
        assert_eq!(plan_resume(&checkpoint, &jump, None).err(), Some(NotResumable::Jumps));
        let structured = SourceText::from(program(Some((checkpoint.line_no + 1, "IF R1 > 0\nENDIF"))));
        assert_eq!(
            plan_resume(&checkpoint, &structured, None).err(),
            Some(NotResumable::Structured)
        );
    }

    #[test]
    fn no_checkpoint_inside_a_spline() {
        let mut config = config(1, false);
        config.flatten_tolerance = Some(0.01);
        let text = "G1 X0 Y0\nBSPLINE X1 Y1\nX2 Y0\nX3 Y1\nG1 X4 Y0\nX5\nX6";
        let list = CheckpointList::default();
        full(text, &config, &list);
        let lines: Vec<usize> = list.lock().unwrap().iter().map(|c| c.line_no).collect();
        assert!(lines.iter().all(|&line| !(3..=5).contains(&line)), "{lines:?}");
        assert!(lines.contains(&6), "{lines:?}");
    }
}
