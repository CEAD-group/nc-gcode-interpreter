//! Streaming driver of the statement-AST path (NC_EXEC=ast): interprets any
//! program line by line through `ast::Block`s, without a whole-file tree.
//!
//! Per line: `line_driver::decode_line` then `decode_ast::decode_ast` for a
//! trivial line, the block the pre-pass lowered for a grammar-bound one,
//! nothing for a blank one; then `exec::exec_block` and the `exec::Flow` it
//! returns. Nesting comes only from `structure_scan::scan_structure_table`,
//! which also raises the structure errors of check_structures. Jumps and loop
//! iterations go to line indices and re-decode the trivial lines they revisit.
//! A program longer than one chunk decodes the chunks after the running one
//! on Rayon (`Prefetch`), a bounded window ahead.
//!
//! Before running, one pass parses and lowers every grammar-bound line with
//! `lower::parse_line` (so a parse error anywhere wins over a runtime error,
//! as with the whole-file parse) and keeps those blocks, which is small on CAM
//! output; trivial lines keep nothing. It also checks that each structure line
//! parses as the form the scan paired, and records the byte offsets of the
//! lines control can reach out of sequence. Only when the program mentions
//! GOTO does it also build the jump index.
//!
//! The driver owns the control stack and keeps the tree walker's scoping: each
//! IF branch and each loop body is a jump scope. A head line's label belongs to
//! the enclosing scope, an ELSE or closer line's label to the body before it.
//! A jump resolves in the innermost scope first (the `resolve_jump` directions,
//! relative to the current line), then in each enclosing scope relative to the
//! structure's head line, so it can leave a body but never enter one. Entering
//! a scope (every iteration) extends `state.seen_jump_targets` and pushes its
//! labels on `state.jump_scopes`; backward jumps count per scope entry against
//! `iteration_limit`. Loop limits are interpret_rules' quirks included: WHILE
//! fails once its count reaches the limit even if the condition then fails;
//! LOOP and REPEAT count after each pass, REPEAT before its UNTIL test; FOR
//! has none. A jump unresolved at top level is `into_not_found_error`.

use crate::ast::{Block, Body, Control};
use crate::decode_ast::BlockBuffer;
use crate::errors::ParsingError;
use crate::exec::{self, Flow};
use crate::interpret_rules::{canonical_block_number, resolve_jump, JumpRequest};
use crate::line_driver::{decode_line, DecodeResult, Word};
use crate::lower;
use crate::output::OutputRows;
use crate::state::State;
use crate::structure_scan::{scan_structure_table, LineRole, Structure, StructureTable};
use std::collections::{HashMap, HashSet, VecDeque};
use std::rc::Rc;
use std::sync::mpsc::{sync_channel, Receiver};

/// Interpret `input` into `output` (the NC_EXEC=ast counterpart of
/// `interpret_file`): sets the input on `state`, scans the structure, and runs
/// every line.
pub(crate) fn interpret_ast(input: &str, state: &mut State, output: &mut OutputRows) -> Result<(), ParsingError> {
    interpret_ast_with(input, state, output, Some(Pipeline::DEFAULT))
}

/// How far the run decodes ahead of the line it executes.
#[derive(Debug, Clone, Copy)]
struct Pipeline {
    /// Lines per chunk decoded on Rayon.
    chunk_lines: usize,
    /// Chunks decoded ahead of the running one; memory stays O(window * chunk_lines).
    window: usize,
}

impl Pipeline {
    const DEFAULT: Pipeline = Pipeline {
        chunk_lines: 4096,
        window: 4,
    };
}

/// `interpret_ast` with an explicit decode pipeline; `None` decodes every line on this thread.
fn interpret_ast_with(
    input: &str,
    state: &mut State,
    output: &mut OutputRows,
    pipeline: Option<Pipeline>,
) -> Result<(), ParsingError> {
    state.set_input(input);
    let (shape, table) = scan_structure_table(input)?;
    let chunk_lines = pipeline.unwrap_or(Pipeline::DEFAULT).chunk_lines;
    let mut program = Program::index_chunked(input, table, chunk_lines, state).map_err(|error| {
        if shape.has_block_structures {
            initial_parse_context(error)
        } else {
            error
        }
    })?;
    let starts = std::mem::take(&mut program.chunk_starts);
    match pipeline {
        // On a Rayon worker, waiting for a chunk queued behind this very job could deadlock.
        Some(pipeline) if starts.len() > 1 && rayon::current_thread_index().is_none() => {
            rayon::in_place_scope(|scope| {
                let prefetch = Prefetch::new(scope, input, starts, pipeline);
                Driver::new(program, Some(prefetch)).run(state, output)
            })
        }
        _ => Driver::new(program, None).run(state, output),
    }
}

/// A jump scope: the top level, the body of the structure opened on a line
/// (for an IF, its true branch), or the ELSE branch of the IF on a line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Scope {
    Top,
    Body(usize),
    Else(usize),
}

/// A line position: 1-based line number and the byte offset it starts at.
#[derive(Debug, Clone, Copy)]
struct Cursor {
    line: usize,
    offset: usize,
}

/// What the pre-pass knows about the program; O(grammar-bound, structure and
/// target lines).
struct Program<'a> {
    input: &'a str,
    table: StructureTable,
    /// The lowered block of every grammar-bound line, parsed once.
    grammar: HashMap<usize, Rc<Block>, rustc_hash::FxBuildHasher>,
    /// Byte offset of every structure line and jump-target line, the only
    /// lines control reaches out of sequence.
    offsets: HashMap<usize, usize>,
    /// Per scope, each jump target key and the ascending lines defining it.
    targets: HashMap<Scope, HashMap<String, Vec<usize>>>,
    /// The program mentions GOTO; without it no jump can happen, so the jump
    /// index and the scope bookkeeping are skipped, as on the fast path.
    has_jumps: bool,
    /// Byte offset of the first line of every decode chunk.
    chunk_starts: Vec<usize>,
}

/// The line starting at byte `offset` as `str::lines` yields it, and the
/// offset of the line after it.
fn line_at(input: &str, offset: usize) -> (&str, usize) {
    let rest = &input[offset..];
    let end = rest.find('\n').map_or(rest.len(), |newline| newline + 1);
    (rest[..end].lines().next().unwrap_or(""), offset + end)
}

/// Conservative: a GOTO in a comment or string only builds an unused index.
fn mentions_goto(input: &str) -> bool {
    input
        .as_bytes()
        .windows(4)
        .any(|word| word.eq_ignore_ascii_case(b"GOTO"))
}

fn block_number_key(digits: &str) -> String {
    format!("N:{}", canonical_block_number(digits.trim()))
}

fn label_key(label: &str) -> String {
    format!("LABEL:{label}")
}

/// The pre-pass is the up-front parse of the whole file; a structured program
/// names it as the whole-file parse does, a structure-free one as the fast path.
fn initial_parse_context(error: ParsingError) -> ParsingError {
    match error {
        ParsingError::ParsingContext {
            line_no,
            column,
            preview,
            context,
            message,
        } if context == "line parsing" => ParsingError::ParsingContext {
            line_no,
            column,
            preview,
            context: "initial file parsing".to_string(),
            message,
        },
        other => other,
    }
}

fn preview(state: &State, line_no: usize) -> String {
    state.get_line(line_no).unwrap_or("").to_string()
}

fn loop_limit(state: &State) -> ParsingError {
    ParsingError::LoopLimit {
        limit: state.iteration_limit.to_string(),
    }
}

/// The structure scan and the line parse disagree about a line, or the
/// control stack is not where the structure table says it is.
fn structure_mismatch(line_no: usize, state: &State, message: String) -> ParsingError {
    ParsingError::UnmatchedStructure {
        line_no,
        preview: preview(state, line_no),
        message,
    }
}

/// The structure line form a block parses as: a head, an ELSE, or a closer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Form {
    Head(Structure),
    Else,
    Close(Structure),
}

fn structure_form(block: &Block) -> Option<Form> {
    let Body::Control(control) = &block.body else {
        return None;
    };
    match control {
        Control::GotoS | Control::Goto(_) | Control::IfGoto(_) | Control::Case { .. } => None,
        Control::IfHead(_) => Some(Form::Head(Structure::If)),
        Control::Else => Some(Form::Else),
        Control::EndIf => Some(Form::Close(Structure::If)),
        Control::WhileHead(_) => Some(Form::Head(Structure::While)),
        Control::EndWhile => Some(Form::Close(Structure::While)),
        Control::ForHead { .. } => Some(Form::Head(Structure::For)),
        Control::EndFor => Some(Form::Close(Structure::For)),
        Control::RepeatHead => Some(Form::Head(Structure::Repeat)),
        Control::UntilTail(_) => Some(Form::Close(Structure::Repeat)),
        Control::LoopHead => Some(Form::Head(Structure::Loop)),
        Control::EndLoop => Some(Form::Close(Structure::Loop)),
    }
}

/// Loud failure when a line's parse and its structure-table role disagree
/// (the scan reads only first words; `IF MATCH(S,"GOTO")>0` is one such line).
fn check_role(line_no: usize, form: Option<Form>, role: Option<LineRole>, state: &State) -> Result<(), ParsingError> {
    let consistent = match (role, form) {
        (None, None) => true,
        (Some(LineRole::Opener { kind, .. }), Some(Form::Head(head))) => kind == head,
        (Some(LineRole::Else { .. }), Some(Form::Else)) => true,
        (Some(LineRole::Closer { kind, .. }), Some(Form::Close(closer))) => kind == closer,
        _ => false,
    };
    if consistent {
        return Ok(());
    }
    Err(structure_mismatch(
        line_no,
        state,
        format!(
            "this line parses as {} but the structure scan reads it as {}",
            describe_form(form),
            describe_role(role)
        ),
    ))
}

fn describe_form(form: Option<Form>) -> String {
    match form {
        None => "a plain block".to_string(),
        Some(Form::Head(kind)) => format!("the head of a {kind:?} block"),
        Some(Form::Else) => "an ELSE".to_string(),
        Some(Form::Close(kind)) => format!("the end of a {kind:?} block"),
    }
}

fn describe_role(role: Option<LineRole>) -> String {
    match role {
        None => "no structure line".to_string(),
        Some(LineRole::Opener { kind, .. }) => format!("the head of a {kind:?} block"),
        Some(LineRole::Else { opener_line }) => format!("the ELSE of the IF on line {opener_line}"),
        Some(LineRole::Closer { kind, opener_line }) => {
            format!("the end of the {kind:?} block on line {opener_line}")
        }
    }
}

impl<'a> Program<'a> {
    /// The pre-pass: validate every grammar-bound line and every structure
    /// line, record the offsets, and build the per-scope jump index.
    /// The pre-pass with the default decode chunks.
    #[cfg(test)]
    fn index(input: &'a str, table: StructureTable, state: &State) -> Result<Self, ParsingError> {
        Self::index_chunked(input, table, Pipeline::DEFAULT.chunk_lines, state)
    }

    /// The pre-pass, also recording where every chunk of `chunk_lines` lines starts.
    fn index_chunked(
        input: &'a str,
        table: StructureTable,
        chunk_lines: usize,
        state: &State,
    ) -> Result<Self, ParsingError> {
        let has_jumps = mentions_goto(input);
        let mut chunk_starts = Vec::new();
        let mut offsets = HashMap::new();
        let mut grammar = HashMap::default();
        let mut targets: HashMap<Scope, HashMap<String, Vec<usize>>> = HashMap::new();
        let mut open: Vec<Scope> = Vec::new();
        let mut arena: Vec<Word> = Vec::new();
        let mut at = Cursor { line: 1, offset: 0 };
        while at.offset < input.len() {
            if (at.line - 1).is_multiple_of(chunk_lines) {
                chunk_starts.push(at.offset);
            }
            let (text, next_offset) = line_at(input, at.offset);
            let role = table.role(at.line);
            arena.clear();
            let mut keys: [Option<String>; 2] = [None, None];
            match decode_line(text, at.line, &mut arena) {
                DecodeResult::Blank => check_role(at.line, None, role, state)?,
                DecodeResult::Trivial(decoded, label) => {
                    check_role(at.line, None, role, state)?;
                    if has_jumps {
                        keys = [decoded.n.map(block_number_key), label.as_deref().map(label_key)];
                    }
                }
                DecodeResult::NeedsGrammar => {
                    let block = lower::parse_line(text, at.line, state)?;
                    check_role(at.line, structure_form(&block), role, state)?;
                    if has_jumps {
                        keys = [
                            block.block_number.as_deref().map(block_number_key),
                            block.label.as_deref().map(label_key),
                        ];
                    }
                    grammar.insert(at.line, Rc::new(block));
                }
            }
            // The line's block number and label belong to the scope open before it.
            let scope = open.last().copied().unwrap_or(Scope::Top);
            match role {
                Some(LineRole::Opener { .. }) => open.push(Scope::Body(at.line)),
                Some(LineRole::Else { opener_line }) => {
                    if let Some(top) = open.last_mut() {
                        *top = Scope::Else(opener_line);
                    }
                }
                Some(LineRole::Closer { .. }) => {
                    open.pop();
                }
                None => {}
            }
            if role.is_some() {
                offsets.insert(at.line, at.offset);
            }
            for key in keys.into_iter().flatten() {
                targets.entry(scope).or_default().entry(key).or_default().push(at.line);
                offsets.insert(at.line, at.offset);
            }
            at = Cursor {
                line: at.line + 1,
                offset: next_offset,
            };
        }
        Ok(Program {
            input,
            table,
            grammar,
            offsets,
            targets,
            has_jumps,
            chunk_starts,
        })
    }

    fn offset(&self, line_no: usize, state: &State) -> Result<usize, ParsingError> {
        self.offsets
            .get(&line_no)
            .copied()
            .ok_or_else(|| structure_mismatch(line_no, state, "the driver has no position for this line".to_string()))
    }

    /// The tree walker's `interpret_blocks` entry: the scope's targets become
    /// visible to GOTOC and to the not-found hint.
    fn enter_scope(&self, scope: Scope, state: &mut State) {
        if !self.has_jumps {
            return;
        }
        let keys: HashSet<String> = self
            .targets
            .get(&scope)
            .map(|targets| targets.keys().cloned().collect())
            .unwrap_or_default();
        state.seen_jump_targets.extend(keys.iter().cloned());
        state.jump_scopes.push(keys);
    }

    fn exit_scope(&self, state: &mut State) {
        if self.has_jumps {
            state.jump_scopes.pop();
        }
    }
}

/// One line of a decoded chunk.
struct ChunkLine<'a> {
    offset: usize,
    text: &'a str,
    next_offset: usize,
    result: DecodeResult<'a>,
}

/// The decoded lines `index * chunk_lines + 1 ..`; their word ranges index `arena`.
struct Chunk<'a> {
    index: usize,
    lines: Vec<ChunkLine<'a>>,
    arena: Vec<Word<'a>>,
}

fn decode_chunk(input: &str, index: usize, offset: usize, chunk_lines: usize) -> Chunk<'_> {
    let mut chunk = Chunk {
        index,
        lines: Vec::with_capacity(chunk_lines),
        // CAM output averages about four words per line; one reservation keeps workers off the allocator.
        arena: Vec::with_capacity(chunk_lines * 4),
    };
    let mut offset = offset;
    for line_no in index * chunk_lines + 1..(index + 1) * chunk_lines + 1 {
        if offset >= input.len() {
            break;
        }
        let (text, next_offset) = line_at(input, offset);
        let result = decode_line(text, line_no, &mut chunk.arena);
        chunk.lines.push(ChunkLine {
            offset,
            text,
            next_offset,
            result,
        });
        offset = next_offset;
    }
    chunk
}

/// Decodes the chunks after the running one on Rayon, at most `window` ahead.
/// Lines behind the running chunk (a loop pass, a backward jump) are not
/// kept; the driver decodes them again itself.
struct Prefetch<'s, 'a> {
    scope: &'s rayon::Scope<'a>,
    input: &'a str,
    /// Byte offset of the first line of every chunk.
    starts: Vec<usize>,
    pipeline: Pipeline,
    /// Spawned chunks in order, each delivered through its own channel.
    ahead: VecDeque<(usize, Receiver<Chunk<'a>>)>,
    /// The next chunk to spawn.
    next: usize,
    running: Option<Chunk<'a>>,
}

impl<'s, 'a> Prefetch<'s, 'a> {
    fn new(scope: &'s rayon::Scope<'a>, input: &'a str, starts: Vec<usize>, pipeline: Pipeline) -> Self {
        Prefetch {
            scope,
            input,
            starts,
            pipeline,
            ahead: VecDeque::new(),
            next: 0,
            running: None,
        }
    }

    /// Line `line_no` decoded, with its chunk's word arena; `None` when it is
    /// behind the running chunk.
    fn line(&mut self, line_no: usize) -> Option<(&ChunkLine<'a>, &[Word<'a>])> {
        let index = (line_no - 1) / self.pipeline.chunk_lines;
        let running = self.running.as_ref().map(|chunk| chunk.index);
        if running.is_some_and(|running| running > index) {
            return None;
        }
        if running != Some(index) {
            self.advance(index);
        }
        let chunk = self.running.as_ref()?;
        let line = chunk.lines.get(line_no - 1 - index * self.pipeline.chunk_lines)?;
        Some((line, &chunk.arena))
    }

    /// Make chunk `index` the running one and keep the window after it decoding.
    fn advance(&mut self, index: usize) {
        self.running = None;
        // Chunks a forward jump passed over.
        while self.ahead.front().is_some_and(|(next, _)| *next < index) {
            self.ahead.pop_front();
        }
        if self.ahead.front().is_none_or(|(next, _)| *next != index) {
            // A jump beyond the window: decode from where it lands.
            self.ahead.clear();
            self.next = index;
        }
        self.spawn_ahead(1);
        if let Some((_, chunk)) = self.ahead.pop_front() {
            // A decode that panicked sends nothing: its lines decode here, the panic surfaces at scope end.
            self.running = chunk.recv().ok();
        }
        self.spawn_ahead(self.pipeline.window);
    }

    fn spawn_ahead(&mut self, count: usize) {
        while self.ahead.len() < count && self.next < self.starts.len() {
            let (index, offset) = (self.next, self.starts[self.next]);
            let (input, chunk_lines) = (self.input, self.pipeline.chunk_lines);
            let (sender, receiver) = sync_channel(1);
            self.scope.spawn(move |_| {
                // The run may have ended or jumped past this chunk.
                let _ = sender.send(decode_chunk(input, index, offset, chunk_lines));
            });
            self.ahead.push_back((index, receiver));
            self.next += 1;
        }
    }
}

/// A loaded line: blank, decoded into the driver's buffer, or kept from the pre-pass.
enum Loaded {
    Blank,
    Decoded,
    Kept(Rc<Block>),
}

/// One entered structure on the control stack.
struct Frame {
    kind: Structure,
    opener: usize,
    closer: usize,
    /// The scope of the running body (an IF frame's branch).
    scope: Scope,
    /// The head line's block, for the WHILE re-test and each pass's first row.
    head: Rc<Block>,
    /// The line after the head, where every pass starts.
    body: Cursor,
    /// Completed or started passes, as the tree walker counts them.
    passes: usize,
    /// Backward jumps taken in the current pass (the walker's per-scope count).
    jumps: usize,
    /// FOR counter name and end value.
    counter: Option<(String, f64)>,
}

struct Driver<'s, 'a> {
    program: Program<'a>,
    /// Decodes the lines ahead in parallel; `None` decodes every line here.
    prefetch: Option<Prefetch<'s, 'a>>,
    arena: Vec<Word<'a>>,
    /// The block of the decoded trivial line being run.
    buffer: BlockBuffer,
    stack: Vec<Frame>,
    /// Backward jumps taken in the top-level scope.
    top_jumps: usize,
}

impl<'s, 'a> Driver<'s, 'a> {
    fn new(program: Program<'a>, prefetch: Option<Prefetch<'s, 'a>>) -> Self {
        Driver {
            program,
            prefetch,
            arena: Vec::new(),
            buffer: BlockBuffer::new(),
            stack: Vec::new(),
            top_jumps: 0,
        }
    }

    fn run(mut self, state: &mut State, output: &mut OutputRows) -> Result<(), ParsingError> {
        self.program.enter_scope(Scope::Top, state);
        let mut at = Cursor { line: 1, offset: 0 };
        while at.offset < self.program.input.len() {
            let (loaded, next) = self.load(at, state)?;
            let flow = match &loaded {
                Loaded::Blank => {
                    at = next;
                    continue;
                }
                Loaded::Decoded => exec::exec_block(self.buffer.block(), state, output)?,
                Loaded::Kept(block) => exec::exec_block(block, state, output)?,
            };
            at = match flow {
                Flow::Continue => next,
                Flow::EndProgram => return self.end_program(state, output),
                Flow::Jump(request) => self.jump(request, at.line, state, output)?,
                Flow::ScopeEnd => {
                    let block = self.keep(loaded);
                    self.scope_end(block, at, next, state, output)?
                }
                flow @ (Flow::Branch(_) | Flow::ForInit { .. } | Flow::LoopStart | Flow::Repeat) => {
                    let block = self.keep(loaded);
                    self.open(block, at, next, flow, state, output)?
                }
            };
        }
        if let Some(frame) = self.stack.last() {
            return Err(structure_mismatch(
                frame.opener,
                state,
                "the program ended inside this structure".to_string(),
            ));
        }
        self.program.exit_scope(state);
        Ok(())
    }

    /// Decode the line at `at` into the buffer (or take it from the prefetched
    /// chunk), or take its kept block.
    fn load(&mut self, at: Cursor, state: &State) -> Result<(Loaded, Cursor), ParsingError> {
        let input: &'a str = self.program.input;
        let sequential;
        let (text, next_offset, result, arena) = match self.prefetch.as_mut().and_then(|p| p.line(at.line)) {
            Some((line, arena)) if line.offset == at.offset => (line.text, line.next_offset, &line.result, arena),
            Some(_) => {
                return Err(structure_mismatch(
                    at.line,
                    state,
                    "the decoded chunk disagrees with the line position".to_string(),
                ))
            }
            None => {
                let (text, next_offset) = line_at(input, at.offset);
                self.arena.clear();
                sequential = decode_line(text, at.line, &mut self.arena);
                (text, next_offset, &sequential, self.arena.as_slice())
            }
        };
        let next = Cursor {
            line: at.line + 1,
            offset: next_offset,
        };
        let loaded = match result {
            DecodeResult::Blank => Loaded::Blank,
            DecodeResult::Trivial(decoded, label) => {
                self.buffer.decode(text, decoded, arena, label.as_deref())?;
                Loaded::Decoded
            }
            DecodeResult::NeedsGrammar => match self.program.grammar.get(&at.line) {
                Some(block) => Loaded::Kept(Rc::clone(block)),
                None => {
                    return Err(structure_mismatch(
                        at.line,
                        state,
                        "the pre-pass kept no block for this grammar-bound line".to_string(),
                    ))
                }
            },
        };
        Ok((loaded, next))
    }

    /// A loaded line's block as a value a frame can hold; structure lines are
    /// grammar-bound, so the copy of a decoded block is never taken in practice.
    fn keep(&self, loaded: Loaded) -> Rc<Block> {
        match loaded {
            Loaded::Kept(block) => block,
            Loaded::Blank | Loaded::Decoded => Rc::new(self.buffer.block().clone()),
        }
    }

    /// Load a structure line out of sequence (an ELSE or a closer).
    fn load_line(&mut self, line_no: usize, state: &State) -> Result<(Rc<Block>, Cursor), ParsingError> {
        let at = Cursor {
            line: line_no,
            offset: self.program.offset(line_no, state)?,
        };
        match self.load(at, state)? {
            (Loaded::Blank, _) => Err(structure_mismatch(
                line_no,
                state,
                "a structure line is blank".to_string(),
            )),
            (loaded, next) => Ok((self.keep(loaded), next)),
        }
    }

    /// A head line ran: enter the structure, or skip it to its closer.
    fn open(
        &mut self,
        head: Rc<Block>,
        at: Cursor,
        next: Cursor,
        flow: Flow,
        state: &mut State,
        output: &mut OutputRows,
    ) -> Result<Cursor, ParsingError> {
        let Some(LineRole::Opener {
            kind,
            else_line,
            closer_line,
        }) = self.program.table.role(at.line)
        else {
            return Err(structure_mismatch(
                at.line,
                state,
                format!("{flow:?} from a line that opens no structure"),
            ));
        };
        let mut frame = Frame {
            kind,
            opener: at.line,
            closer: closer_line,
            scope: Scope::Body(at.line),
            head,
            body: next,
            passes: 0,
            jumps: 0,
            counter: None,
        };
        match (kind, flow) {
            (Structure::If, Flow::Branch(true)) => {
                self.program.enter_scope(frame.scope, state);
                self.stack.push(frame);
                Ok(next)
            }
            (Structure::If, Flow::Branch(false)) => match else_line {
                Some(else_line) => {
                    let (else_block, after_else) = self.load_line(else_line, state)?;
                    frame.scope = Scope::Else(at.line);
                    self.program.enter_scope(frame.scope, state);
                    self.stack.push(frame);
                    exec::begin_body(&else_block, output)?;
                    Ok(after_else)
                }
                None => self.leave_closer(closer_line, state, output),
            },
            (Structure::While, Flow::Branch(taken)) => {
                self.stack.push(frame);
                self.while_test(taken, None, state, output)
            }
            (Structure::For, Flow::ForInit { var, to }) => {
                if exec::for_continues(&var, to, state) {
                    frame.counter = Some((var, to));
                    self.stack.push(frame);
                    self.begin_pass(state, output)
                } else {
                    self.leave_closer(closer_line, state, output)
                }
            }
            (Structure::Loop, Flow::LoopStart) | (Structure::Repeat, Flow::Repeat) => {
                self.stack.push(frame);
                self.begin_pass(state, output)
            }
            (kind, flow) => Err(structure_mismatch(
                at.line,
                state,
                format!("{flow:?} from the head of a {kind:?} block"),
            )),
        }
    }

    /// Start a pass of the top frame's body: enter its scope and run the rest
    /// of the head line (its comment) as the body's first block.
    fn begin_pass(&mut self, state: &mut State, output: &mut OutputRows) -> Result<Cursor, ParsingError> {
        let frame = self.stack.last_mut().expect("begin_pass runs on an entered structure");
        frame.jumps = 0;
        self.program.enter_scope(frame.scope, state);
        exec::begin_body(&frame.head, output)?;
        Ok(frame.body)
    }

    /// `while cond && passes < limit` then `passes >= limit` is an error, in
    /// the tree walker's order: the condition is evaluated first.
    fn while_test(
        &mut self,
        taken: bool,
        closer: Option<(Rc<Block>, Cursor)>,
        state: &mut State,
        output: &mut OutputRows,
    ) -> Result<Cursor, ParsingError> {
        let limit = state.iteration_limit;
        let frame = self.stack.last_mut().expect("while_test runs on an entered WHILE");
        if taken && frame.passes < limit {
            frame.passes += 1;
            return self.begin_pass(state, output);
        }
        if frame.passes >= limit {
            return Err(loop_limit(state));
        }
        self.close(closer, state, output)
    }

    /// An ELSE or closer line ran: the body before it is done.
    fn scope_end(
        &mut self,
        block: Rc<Block>,
        at: Cursor,
        next: Cursor,
        state: &mut State,
        output: &mut OutputRows,
    ) -> Result<Cursor, ParsingError> {
        let role = self.program.table.role(at.line);
        let (kind, opener, scope) = match self.stack.last() {
            Some(frame) => (frame.kind, frame.opener, frame.scope),
            None => {
                return Err(structure_mismatch(
                    at.line,
                    state,
                    "a block end outside any structure".to_string(),
                ))
            }
        };
        let is_else = match role {
            Some(LineRole::Else { opener_line }) if opener_line == opener && scope == Scope::Body(opener) => true,
            Some(LineRole::Closer { opener_line, .. }) if opener_line == opener => false,
            _ => {
                return Err(structure_mismatch(
                    at.line,
                    state,
                    format!("this line does not end the structure on line {opener}"),
                ))
            }
        };
        self.program.exit_scope(state);
        if is_else {
            // The true branch is done; the ELSE branch is skipped to ENDIF.
            return self.close(None, state, output);
        }
        let closer = Some((block, next));
        match kind {
            Structure::If => self.close(closer, state, output),
            Structure::While => {
                let head = &self.stack.last().expect("frame checked above").head;
                let taken = exec::eval_condition(head, state)?;
                self.while_test(taken, closer, state, output)
            }
            Structure::For => {
                let (var, to) = self
                    .stack
                    .last()
                    .and_then(|frame| frame.counter.as_ref())
                    .expect("an entered FOR has its counter");
                exec::for_step(var, state, output)?;
                if exec::for_continues(var, *to, state) {
                    self.begin_pass(state, output)
                } else {
                    self.close(closer, state, output)
                }
            }
            Structure::Loop => {
                self.count_pass(state)?;
                self.begin_pass(state, output)
            }
            Structure::Repeat => {
                self.count_pass(state)?;
                let until = closer.as_ref().map(|(block, _)| block).expect("set above");
                if exec::eval_condition(until, state)? {
                    self.close(closer, state, output)
                } else {
                    self.begin_pass(state, output)
                }
            }
        }
    }

    /// LOOP and REPEAT: count the pass just completed against the limit.
    fn count_pass(&mut self, state: &State) -> Result<(), ParsingError> {
        let frame = self.stack.last_mut().expect("count_pass runs on an entered loop");
        frame.passes += 1;
        if frame.passes >= state.iteration_limit {
            return Err(loop_limit(state));
        }
        Ok(())
    }

    /// Pop the top frame and write its closer comment; `closer` is the closer
    /// line when it just ran, otherwise it is loaded.
    fn close(
        &mut self,
        closer: Option<(Rc<Block>, Cursor)>,
        state: &mut State,
        output: &mut OutputRows,
    ) -> Result<Cursor, ParsingError> {
        let frame = self.stack.pop().expect("close runs on an entered structure");
        match closer {
            Some((block, next)) => {
                exec::leave_structure(&block, output);
                Ok(next)
            }
            None => self.leave_closer(frame.closer, state, output),
        }
    }

    fn leave_closer(
        &mut self,
        closer_line: usize,
        state: &mut State,
        output: &mut OutputRows,
    ) -> Result<Cursor, ParsingError> {
        let (block, next) = self.load_line(closer_line, state)?;
        exec::leave_structure(&block, output);
        Ok(next)
    }

    /// Resolve a jump from `from` through the scope chain, leaving every
    /// structure it jumps out of.
    fn jump(
        &mut self,
        request: JumpRequest,
        from: usize,
        state: &mut State,
        output: &mut OutputRows,
    ) -> Result<Cursor, ParsingError> {
        let mut current = from;
        loop {
            let scope = self.stack.last().map_or(Scope::Top, |frame| frame.scope);
            let destination = self
                .program
                .targets
                .get(&scope)
                .and_then(|targets| resolve_jump(targets, current, &request));
            if let Some(line) = destination {
                // Only backward jumps can cycle; same >= threshold as run_blocks.
                if line <= current {
                    let jumps = self
                        .stack
                        .last_mut()
                        .map_or(&mut self.top_jumps, |frame| &mut frame.jumps);
                    *jumps += 1;
                    if *jumps >= state.iteration_limit {
                        return Err(loop_limit(state));
                    }
                }
                return Ok(Cursor {
                    line,
                    offset: self.program.offset(line, state)?,
                });
            }
            let Some(frame) = self.stack.last() else {
                return Err(request.into_not_found_error(state));
            };
            current = frame.opener;
            self.program.exit_scope(state);
            self.close(None, state, output)?;
        }
    }

    /// M2/M17/M30 ran: every enclosing structure finishes, innermost first.
    fn end_program(mut self, state: &mut State, output: &mut OutputRows) -> Result<(), ParsingError> {
        while !self.stack.is_empty() {
            self.program.exit_scope(state);
            self.close(None, state, output)?;
        }
        self.program.exit_scope(state);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn index(program: &str) -> Program<'_> {
        let mut state = State::new(vec!["X".to_string(), "Y".to_string()], 100, None, false);
        state.set_input(program);
        let (_, table) = scan_structure_table(program).expect("valid structure");
        Program::index(program, table, &state).unwrap_or_else(|e| panic!("{program}\n{e}"))
    }

    fn lines_of(program: &Program, scope: Scope, key: &str) -> Vec<usize> {
        program
            .targets
            .get(&scope)
            .and_then(|targets| targets.get(key))
            .cloned()
            .unwrap_or_default()
    }

    #[test]
    fn line_at_follows_str_lines() {
        for input in ["a\nb", "a\r\nb\r\n", "\n\nx", "x\n", "a\rb\n", "last\r"] {
            let mut offset = 0;
            let mut ours = Vec::new();
            while offset < input.len() {
                let (text, next) = line_at(input, offset);
                ours.push(text);
                offset = next;
            }
            assert_eq!(ours, input.lines().collect::<Vec<_>>(), "{input:?}");
        }
    }

    #[test]
    fn targets_belong_to_the_scope_the_walker_gives_them() {
        let program = "\
N10 X1
TOP: WHILE R1<2 ; head
  N20 X2
  IF R1==1
    INNER: X3
  N30 ELSE
    N40 X4
  N50 ENDIF
  R1=R1+1
LAST: ENDWHILE
N60 GOTOB TOP
";
        let program = index(program);
        assert!(program.has_jumps);
        assert_eq!(lines_of(&program, Scope::Top, "N:10"), vec![1]);
        assert_eq!(lines_of(&program, Scope::Top, "LABEL:TOP"), vec![2]);
        assert_eq!(lines_of(&program, Scope::Top, "N:60"), vec![11]);
        assert_eq!(lines_of(&program, Scope::Body(2), "N:20"), vec![3]);
        assert_eq!(lines_of(&program, Scope::Body(2), "LABEL:LAST"), vec![10]);
        assert_eq!(lines_of(&program, Scope::Body(4), "LABEL:INNER"), vec![5]);
        assert_eq!(lines_of(&program, Scope::Body(4), "N:30"), vec![6]);
        assert_eq!(lines_of(&program, Scope::Else(4), "N:40"), vec![7]);
        assert_eq!(lines_of(&program, Scope::Else(4), "N:50"), vec![8]);
        for line in [2, 4, 6, 8, 10] {
            assert!(program.offsets.contains_key(&line), "structure line {line}");
        }
    }

    #[test]
    fn no_goto_word_builds_no_index() {
        let program = index("N10 X1\nWHILE R1<2\nN20 R1=R1+1\nENDWHILE\n");
        assert!(!program.has_jumps);
        assert!(program.targets.is_empty());
        assert_eq!(program.offsets.len(), 2);
    }

    #[test]
    fn parse_errors_surface_before_anything_runs() {
        let program = "X1\nM30\nX=\n";
        let mut state = State::new(vec!["X".to_string()], 100, None, false);
        state.set_input(program);
        let (_, table) = scan_structure_table(program).expect("valid structure");
        let error = Program::index(program, table, &state).err().expect("parse error");
        assert_eq!(error.location().map(|l| l.line), Some(3), "{error}");
    }

    fn text<'a>(prefetch: &mut Prefetch<'_, 'a>, line_no: usize) -> Option<&'a str> {
        prefetch.line(line_no).map(|(line, _)| line.text)
    }

    #[test]
    fn prefetch_serves_lines_ahead_and_none_behind() {
        let input: String = (1..=50).map(|i| format!("N{i} X{i}\n")).collect();
        let pipeline = Pipeline {
            chunk_lines: 4,
            window: 2,
        };
        let starts: Vec<usize> = (0..50)
            .step_by(4)
            .map(|line| input.lines().take(line).map(|text| text.len() + 1).sum())
            .collect();
        rayon::in_place_scope(|scope| {
            let mut prefetch = Prefetch::new(scope, &input, starts, pipeline);
            for line_no in 1..=10 {
                assert_eq!(
                    text(&mut prefetch, line_no),
                    Some(format!("N{line_no} X{line_no}").as_str())
                );
            }
            // Line 10 runs chunk 2; earlier chunks are gone, its own lines stay.
            assert_eq!(text(&mut prefetch, 3), None);
            assert_eq!(text(&mut prefetch, 9), Some("N9 X9"));
            // A jump beyond the window decodes from where it lands.
            assert_eq!(text(&mut prefetch, 45), Some("N45 X45"));
            assert_eq!(text(&mut prefetch, 20), None);
            assert_eq!(text(&mut prefetch, 50), Some("N50 X50"));
            assert_eq!(text(&mut prefetch, 51), None);
            assert!(prefetch.ahead.len() <= pipeline.window);
        });
    }

    /// Lines of every kind, with labels unique to `copy`.
    fn straight(copy: usize) -> String {
        (1..=40)
            .map(|i| match i % 7 {
                0 => "\n".to_string(),
                1 => format!("; comment {i}\n"),
                2 => format!("R{i}=SIN({i})\n"),
                3 => format!("LBL_{copy}_{i}: X{i}\n"),
                _ => format!("N{i} G1 X{i} Y{} E=IC(E_M*0.5) F=(F_M*2) ;c{i}\n", i * 2),
            })
            .collect()
    }

    /// Rows, final variables and axes of a run, or its error.
    fn run_with(program: &str, pipeline: Option<Pipeline>) -> String {
        let mut state = crate::interpreter::build_state(None, None, 100, None, true);
        let (sender, receiver) = std::sync::mpsc::sync_channel(1 << 16);
        let mut output = OutputRows::stream(sender);
        let result = interpret_ast_with(program, &mut state, &mut output, pipeline).and_then(|()| output.finish());
        let rows: Vec<String> = receiver.try_iter().map(|row| format!("{row:?}")).collect();
        let mut symbols: Vec<String> = state.symbol_table.iter().map(|(k, v)| format!("{k}={v}")).collect();
        symbols.sort();
        let mut axes: Vec<String> = state.axes.iter().map(|(k, v)| format!("{k}={v}")).collect();
        axes.sort();
        format!("{:?}\n{}\n{symbols:?}\n{axes:?}", result.map(|_| ()), rows.join("\n"))
    }

    #[test]
    fn the_decode_pipeline_runs_what_sequential_decoding_runs() {
        let head = "E_M=1\nF_M=2\nR90=0\nR91=0\n";
        let (a, b) = (straight(1), straight(2));
        let programs = [
            format!("{head}{a}M30\n"),
            // A forward jump past the window, then backward jumps across chunks.
            format!("{head}GOTOF FAR\n{a}FAR: R90=R90+1\n{b}IF R90<3 GOTOB FAR\nM30\n"),
            format!("{head}WHILE R91<3\n{a}R91=R91+1\nENDWHILE\nFOR R92=1 TO 2\n{b}ENDFOR\n"),
            format!("{head}{a}M30\n{b}"),
            // A runtime error in a later chunk.
            format!("{head}{a}GOTOF NOWHERE\n{b}"),
        ];
        for program in &programs {
            let sequential = run_with(program, None);
            assert!(sequential.lines().count() > 40, "{sequential}");
            for chunk_lines in [1, 2, 3, 5, 8] {
                for window in [1, 2, 4] {
                    let pipeline = Pipeline { chunk_lines, window };
                    assert_eq!(run_with(program, Some(pipeline)), sequential, "{pipeline:?}\n{program}");
                }
            }
        }
    }

    #[test]
    fn an_if_the_scan_takes_for_a_jump_fails_loudly() {
        // The scan sees GOTO inside the string and pairs nothing; the line parses as an IF head.
        let program = "DEF STRING[8] S=\"A\"\nIF MATCH(S,\"GOTO\")>0\nX1\n";
        let mut state = State::new(vec!["X".to_string()], 100, None, false);
        state.set_input(program);
        let (_, table) = scan_structure_table(program).expect("valid structure");
        let error = Program::index(program, table, &state).err().expect("mismatch");
        assert_eq!(error.location().map(|l| l.line), Some(2), "{error}");
        assert!(error.to_string().contains("structure scan"), "{error}");
    }
}
