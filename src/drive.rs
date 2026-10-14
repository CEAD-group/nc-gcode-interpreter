//! Streaming driver of the statement-AST path (NC_EXEC=ast): interprets any
//! program line by line through `ast::Block`s, without a whole-file tree.
//!
//! Per line: `line_driver::decode_line` then `decode_ast::decode_ast` for a
//! trivial line, `lower::parse_line` for a grammar-bound one, nothing for a
//! blank one; then `exec::exec_block` and the `exec::Flow` it returns. Nesting
//! comes only from `structure_scan::scan_structure_table`, which also raises
//! the structure errors of check_structures. Jumps and loop iterations go to
//! line indices and re-decode the lines they revisit; nothing is kept per line.
//!
//! Before running, one pass parses every grammar-bound line (so a parse error
//! anywhere wins over a runtime error, as with the whole-file parse), checks
//! that each structure line parses as the form the scan paired, and records
//! the byte offsets of the lines control can reach out of sequence. Only when
//! the program mentions GOTO does it also build the jump index.
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
use crate::errors::ParsingError;
use crate::exec::{self, Flow};
use crate::interpret_rules::{canonical_block_number, resolve_jump, JumpRequest};
use crate::line_driver::{decode_line, DecodeResult, Word};
use crate::lower;
use crate::output::OutputRows;
use crate::state::State;
use crate::structure_scan::{scan_structure_table, LineRole, Structure, StructureTable};
use std::collections::{HashMap, HashSet};

/// Interpret `input` into `output` (the NC_EXEC=ast counterpart of
/// `interpret_file`): sets the input on `state`, scans the structure, and runs
/// every line.
pub(crate) fn interpret_ast(input: &str, state: &mut State, output: &mut OutputRows) -> Result<(), ParsingError> {
    state.set_input(input);
    let (_, table) = scan_structure_table(input)?;
    let program = Program::index(input, table, state)?;
    Driver {
        program,
        arena: Vec::new(),
        stack: Vec::new(),
        top_jumps: 0,
    }
    .run(state, output)
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

/// What the pre-pass knows about the program; O(structure and target lines).
struct Program<'a> {
    input: &'a str,
    table: StructureTable,
    /// Byte offset of every structure line and jump-target line, the only
    /// lines control reaches out of sequence.
    offsets: HashMap<usize, usize>,
    /// Per scope, each jump target key and the ascending lines defining it.
    targets: HashMap<Scope, HashMap<String, Vec<usize>>>,
    /// The program mentions GOTO; without it no jump can happen, so the jump
    /// index and the scope bookkeeping are skipped, as on the fast path.
    has_jumps: bool,
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
    fn index(input: &'a str, table: StructureTable, state: &State) -> Result<Self, ParsingError> {
        let has_jumps = mentions_goto(input);
        let mut offsets = HashMap::new();
        let mut targets: HashMap<Scope, HashMap<String, Vec<usize>>> = HashMap::new();
        let mut open: Vec<Scope> = Vec::new();
        let mut arena: Vec<Word> = Vec::new();
        let mut at = Cursor { line: 1, offset: 0 };
        while at.offset < input.len() {
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
            offsets,
            targets,
            has_jumps,
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

/// One entered structure on the control stack.
struct Frame {
    kind: Structure,
    opener: usize,
    closer: usize,
    /// The scope of the running body (an IF frame's branch).
    scope: Scope,
    /// The head line's block, for the WHILE re-test and each pass's first row.
    head: Block,
    /// The line after the head, where every pass starts.
    body: Cursor,
    /// Completed or started passes, as the tree walker counts them.
    passes: usize,
    /// Backward jumps taken in the current pass (the walker's per-scope count).
    jumps: usize,
    /// FOR counter name and end value.
    counter: Option<(String, f64)>,
}

struct Driver<'a> {
    program: Program<'a>,
    arena: Vec<Word<'a>>,
    stack: Vec<Frame>,
    /// Backward jumps taken in the top-level scope.
    top_jumps: usize,
}

impl<'a> Driver<'a> {
    fn run(mut self, state: &mut State, output: &mut OutputRows) -> Result<(), ParsingError> {
        self.program.enter_scope(Scope::Top, state);
        let mut at = Cursor { line: 1, offset: 0 };
        while at.offset < self.program.input.len() {
            let (block, next) = self.load(at, state)?;
            let Some(block) = block else {
                at = next;
                continue;
            };
            at = match exec::exec_block(&block, state, output)? {
                Flow::Continue => next,
                Flow::EndProgram => return self.end_program(state, output),
                Flow::Jump(request) => self.jump(request, at.line, state, output)?,
                Flow::ScopeEnd => self.scope_end(block, at, next, state, output)?,
                flow @ (Flow::Branch(_) | Flow::ForInit { .. } | Flow::LoopStart | Flow::Repeat) => {
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

    /// Decode or parse the line at `at`; `None` for a blank line.
    fn load(&mut self, at: Cursor, state: &State) -> Result<(Option<Block>, Cursor), ParsingError> {
        let input: &'a str = self.program.input;
        let (text, next_offset) = line_at(input, at.offset);
        let next = Cursor {
            line: at.line + 1,
            offset: next_offset,
        };
        self.arena.clear();
        let block = match decode_line(text, at.line, &mut self.arena) {
            DecodeResult::Blank => None,
            DecodeResult::Trivial(decoded, label) => Some(crate::decode_ast::decode_ast(
                text,
                &decoded,
                &self.arena,
                label.as_deref(),
            )?),
            DecodeResult::NeedsGrammar => Some(lower::parse_line(text, at.line, state)?),
        };
        Ok((block, next))
    }

    /// Load a structure line out of sequence (an ELSE or a closer).
    fn load_line(&mut self, line_no: usize, state: &State) -> Result<(Block, Cursor), ParsingError> {
        let at = Cursor {
            line: line_no,
            offset: self.program.offset(line_no, state)?,
        };
        match self.load(at, state)? {
            (Some(block), next) => Ok((block, next)),
            (None, _) => Err(structure_mismatch(
                line_no,
                state,
                "a structure line is blank".to_string(),
            )),
        }
    }

    /// A head line ran: enter the structure, or skip it to its closer.
    fn open(
        &mut self,
        head: Block,
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
        closer: Option<(Block, Cursor)>,
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
        block: Block,
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
        closer: Option<(Block, Cursor)>,
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
