//! Experimental execution-cursor VM for the structured interpreter path (#47).
//!
//! This is a **behaviour-preserving reification** of the recursive block walker
//! in the parent module: `run_blocks` plus the recursive
//! `interpret_statement_while/for/loop/repeat/if` calls are replaced by an
//! explicit `Vec<Frame>` work-stack driven by a single loop. Each `Frame`
//! mirrors exactly what one recursive `interpret_blocks` invocation held on the
//! Rust stack (its block list, jump-target map, cursor `index`, backward-jump
//! counter, and — for a loop — its `loop_count` and cached bound/condition).
//!
//! Every *leaf* operation (statements, assignments, expressions, conditions,
//! frame ops, the GOTO family, M-code end detection) is reused verbatim from
//! the parent module via `super::` — the VM only owns *control flow*.
//!
//! Why: an explicit stack is serializable (a `pc` + owned frames), which is the
//! "execution cursor" that makes seek-by-checkpoint cheap
//! (docs/resumable-interpretation-design.md). Gated behind `NC_VM=1` so it runs
//! beside the recursive path and can be diffed against it on the golden corpus.

use super::{
    evaluate_condition, evaluate_expression, get_error_context, interpret_assignment, interpret_block_number,
    interpret_case, interpret_definition, interpret_frame_op, interpret_goto, interpret_if_goto, interpret_statement,
    resolve_jump, scan_jump_targets, BlockFlow,
};
use crate::errors::ParsingError;
use crate::output::OutputRows as Output;
use crate::state::State;
use crate::types::{Rule, Value};
use pest::iterators::Pair;
use std::collections::HashMap;

/// True when `NC_VM=1` selects the experimental cursor VM over the recursive
/// walker for the structured path.
pub(crate) fn vm_enabled() -> bool {
    std::env::var("NC_VM").map(|v| v == "1").unwrap_or(false)
}

/// One active scope on the reified control stack — the heap equivalent of a
/// live `interpret_blocks`/`run_blocks` frame. `Clone` is what makes a
/// checkpoint cheap: the whole stack snapshots by value (the `Pair`s are index
/// ranges into the shared, still-alive parse tree).
#[derive(Clone)]
struct Frame<'i> {
    /// The scope's block list; `index` is an offset into this (== `run_blocks`'
    /// `block_pairs`). Cloning a `Pair` is cheap (an index range into the tree).
    blocks: Vec<Pair<'i, Rule>>,
    /// Label / block-number → ascending block indices, for jump resolution.
    targets: HashMap<String, Vec<usize>>,
    /// The instruction pointer within this scope.
    index: usize,
    /// Backward-jump cycle counter (bounded by `iteration_limit`), per scope —
    /// mirrors `run_blocks`' local `jumps_taken`.
    jumps_taken: usize,
    kind: FrameKind<'i>,
}

/// What kind of scope a frame is, and the loop state its recursive twin held.
#[derive(Clone)]
enum FrameKind<'i> {
    /// The top-level program scope (or an IF branch): no loop-back.
    Straight,
    While {
        cond: Pair<'i, Rule>,
        count: usize,
    },
    Loop {
        count: usize,
    },
    Repeat {
        cond: Pair<'i, Rule>,
        count: usize,
    },
    For {
        var: String,
        end: f64,
    },
}

/// What executing one block asked the driver to do next.
enum Step<'i> {
    /// Advance to the next block in the current scope.
    Continue,
    /// M2/M17/M30 executed — end interpretation.
    EndProgram,
    /// A pending GOTO to resolve against the scope stack.
    Jump(super::JumpRequest),
    /// A structured control opened a body scope; push this frame.
    Enter(Box<Frame<'i>>),
}

/// The reified interpreter: an explicit control stack that can be stepped,
/// paused at a row boundary, and cloned. Cloning a `Vm` (plus the `State` and a
/// `Collect` output snapshot) is a checkpoint; resuming is dropping the clone
/// back into `step_until`. The `'i` lifetime ties it to the parse tree, which
/// an in-memory session keeps alive (Phase 1 — see the design doc).
#[derive(Clone)]
pub(crate) struct Vm<'i> {
    stack: Vec<Frame<'i>>,
}

/// Why `step_until` returned: the program ended (carrying its terminal
/// `BlockFlow`), or it paused having reached the requested row budget.
pub(crate) enum Outcome {
    Done(BlockFlow),
    Paused,
}

impl<'i> Vm<'i> {
    /// Start a VM at the top of `blocks` (registers the root scope's jump
    /// targets with `state`, like `interpret_blocks`).
    pub(crate) fn new(blocks: Pair<'i, Rule>, state: &mut State) -> Self {
        let root = new_frame(collect_blocks(blocks), FrameKind::Straight, state);
        Vm { stack: vec![root] }
    }

    /// Step the VM until the program ends or (when `stop_at_rows` is `Some(k)`)
    /// the `Collect` sink has committed at least `k` rows — pausing at a block
    /// boundary. Resumes cleanly on the next call.
    pub(crate) fn step_until(
        &mut self,
        output: &mut Output,
        state: &mut State,
        stop_at_rows: Option<usize>,
    ) -> Result<Outcome, ParsingError> {
        while let Some(top) = self.stack.last() {
            if top.index >= top.blocks.len() {
                // End of this scope: loop back, or pop to the enclosing scope.
                // (Program end is `Step::EndProgram` from `exec_block`.)
                scope_end(&mut self.stack, output, state)?;
                continue;
            }

            let top = self.stack.last().unwrap();
            let block = top.blocks[top.index].clone();
            match exec_block(block, output, state)? {
                Step::Continue => self.stack.last_mut().unwrap().index += 1,
                Step::EndProgram => {
                    pop_all(&mut self.stack, state);
                    return Ok(Outcome::Done(BlockFlow::EndProgram));
                }
                Step::Enter(frame) => push_frame(&mut self.stack, *frame, state),
                Step::Jump(request) => {
                    resolve_across_stack(&mut self.stack, request, state)?;
                }
            }

            if let Some(k) = stop_at_rows {
                if output.collected_len() >= k {
                    return Ok(Outcome::Paused);
                }
            }
        }
        Ok(Outcome::Done(BlockFlow::Continue))
    }
}

/// Drop-in replacement for `interpret_blocks` at the top level: interpret the
/// program to completion with the explicit-stack VM. An unresolvable jump
/// becomes a `JumpTargetNotFound` error (as in the recursive path).
pub(crate) fn run(blocks: Pair<Rule>, output: &mut Output, state: &mut State) -> Result<BlockFlow, ParsingError> {
    let mut vm = Vm::new(blocks, state);
    match vm.step_until(output, state, None)? {
        Outcome::Done(flow) => Ok(flow),
        Outcome::Paused => unreachable!("no row budget was set"),
    }
}

/// Collect a `blocks` pair's child `block`s into an owned vector of (cheap)
/// pair handles.
fn collect_blocks(blocks: Pair<Rule>) -> Vec<Pair<Rule>> {
    blocks.into_inner().collect()
}

/// Build a frame and register its jump targets with `State` (mirrors
/// `interpret_blocks`' `seen_jump_targets.extend` + `jump_scopes.push`).
fn new_frame<'i>(blocks: Vec<Pair<'i, Rule>>, kind: FrameKind<'i>, state: &mut State) -> Frame<'i> {
    let targets = scan_jump_targets(&blocks);
    state.seen_jump_targets.extend(targets.keys().cloned());
    state.jump_scopes.push(targets.keys().cloned().collect());
    Frame {
        blocks,
        targets,
        index: 0,
        jumps_taken: 0,
        kind,
    }
}

fn push_frame<'i>(stack: &mut Vec<Frame<'i>>, frame: Frame<'i>, _state: &mut State) {
    // jump_scopes was already pushed by new_frame(); just move the frame in.
    stack.push(frame);
}

/// Pop a frame and unwind its `jump_scopes` entry. Used on jump propagation
/// (the enclosing scope will resolve at *its* current index, so no advance).
fn pop_frame(stack: &mut Vec<Frame>, state: &mut State) {
    stack.pop();
    state.jump_scopes.pop();
}

/// Pop a scope that completed normally, then advance the enclosing scope past
/// the control block that spawned it — the recursive walker does this when
/// `interpret_block(control_block)` returns `Continue` and `run_blocks` does
/// `index += 1`. Without the advance the parent re-executes the control forever.
fn pop_and_advance(stack: &mut Vec<Frame>, state: &mut State) {
    pop_frame(stack, state);
    if let Some(parent) = stack.last_mut() {
        parent.index += 1;
    }
}

fn pop_all(stack: &mut Vec<Frame>, state: &mut State) {
    while !stack.is_empty() {
        pop_frame(stack, state);
    }
}

/// Handle reaching the end of the top scope's block list: for a loop, re-test
/// and either restart the body (reset `index`) or pop; for a straight scope,
/// pop. Mirrors the `while`/`loop` tails of the recursive statement fns.
fn scope_end(stack: &mut Vec<Frame>, output: &mut Output, state: &mut State) -> Result<(), ParsingError> {
    // We need the loop kind/state; take it out to avoid borrow conflicts.
    let top = stack.last_mut().expect("scope_end called with a frame");
    match &mut top.kind {
        FrameKind::Straight => {
            pop_and_advance(stack, state);
        }
        FrameKind::While { cond, count } => {
            let cond = cond.clone();
            // `while eval() && count < limit { count += 1; body }` then
            // `if count >= limit { Err }`. Re-test to decide restart vs exit.
            if evaluate_condition(cond, state)? && *count < state.iteration_limit {
                let top = stack.last_mut().unwrap();
                if let FrameKind::While { count, .. } = &mut top.kind {
                    *count += 1;
                }
                top.index = 0;
            } else {
                let hit_limit = *count >= state.iteration_limit;
                pop_and_advance(stack, state);
                if hit_limit {
                    return Err(ParsingError::LoopLimit {
                        limit: state.iteration_limit.to_string(),
                    });
                }
            }
        }
        FrameKind::Loop { count } => {
            // `loop { body; count += 1; if count >= limit { Err } }`.
            *count += 1;
            if *count >= state.iteration_limit {
                let limit = state.iteration_limit.to_string();
                pop_and_advance(stack, state);
                return Err(ParsingError::LoopLimit { limit });
            }
            top.index = 0;
        }
        FrameKind::Repeat { cond, count } => {
            // `loop { body; count += 1; if count >= limit { Err }; if eval() { break } }`.
            *count += 1;
            if *count >= state.iteration_limit {
                let limit = state.iteration_limit.to_string();
                pop_and_advance(stack, state);
                return Err(ParsingError::LoopLimit { limit });
            }
            let cond = cond.clone();
            if evaluate_condition(cond, state)? {
                pop_and_advance(stack, state);
            } else {
                stack.last_mut().unwrap().index = 0;
            }
        }
        FrameKind::For { var, end } => {
            // `while symbol[var] <= end { body; symbol[var] += 1; record }`.
            let var = var.clone();
            let end = *end;
            let new_value = {
                let v = state.symbol_table.get_mut(&var).expect("FOR counter exists");
                *v += 1.0;
                *v
            };
            output.record_variable_change(&var, new_value);
            if new_value <= end {
                stack.last_mut().unwrap().index = 0;
            } else {
                pop_and_advance(stack, state);
            }
        }
    }
    Ok(())
}

/// Execute one block, mirroring `interpret_block`: start its output row, then
/// process each item. A structured control (`WHILE/FOR/LOOP/REPEAT/IF`) yields
/// `Step::Enter` instead of recursing; leaf jumps (`GOTO*`, `IF..GOTO`, `CASE`)
/// go through the reused leaf fns and yield their `BlockFlow`.
fn exec_block<'i>(block: Pair<'i, Rule>, output: &mut Output, state: &mut State) -> Result<Step<'i>, ParsingError> {
    if block.as_rule() != Rule::block {
        return Err(ParsingError::UnexpectedRule {
            rule: block.as_rule(),
            context: "vm::exec_block".to_string(),
            line_no: block.line_col().0,
            preview: state.get_line(block.line_col().0).unwrap_or("").to_string(),
            message: format!("Expected a block, found {:?}", block.as_rule()),
        });
    }
    output.start_row(block.line_col().0)?;

    // Mirror `interpret_block`: process EVERY item, letting the control-flow
    // signal accumulate (last write wins, like `flow = interpret_control(...)`),
    // and return it only at the end — so a trailing `; comment` on a GOTO line
    // still lands on the row. A *structured* control opens a body scope and must
    // suspend block processing, so it returns `Enter` immediately.
    let mut flow = BlockFlow::Continue;
    for item in block.into_inner() {
        match item.as_rule() {
            Rule::statement => {
                if let BlockFlow::EndProgram = interpret_statement(item, output, state)? {
                    flow = BlockFlow::EndProgram;
                }
            }
            Rule::block_number => interpret_block_number(item, output),
            Rule::label_def => {}
            Rule::definition => interpret_definition(item, output, state)?,
            Rule::frame_op => interpret_frame_op(item, state)?,
            Rule::comment => {
                let last = output.last_mut().expect("row started");
                last.insert("comment", Value::Str(item.as_str().to_string()));
            }
            Rule::control => match exec_control(item, output, state)? {
                ControlOutcome::Enter(frame) => return Ok(Step::Enter(frame)),
                ControlOutcome::Flow(f) => flow = f,
            },
            other => {
                return Err(ParsingError::UnexpectedRule {
                    rule: other,
                    context: "vm::exec_block".to_string(),
                    line_no: 0,
                    preview: String::new(),
                    message: format!("Unexpected rule in block: {other:?}"),
                })
            }
        }
    }
    Ok(match flow {
        BlockFlow::Continue => Step::Continue,
        BlockFlow::EndProgram => Step::EndProgram,
        BlockFlow::Jump(r) => Step::Jump(r),
    })
}

/// The result of a `control` node: either a structured body to enter, or a
/// leaf control-flow signal (from a GOTO family / CASE / GOTOS) to accumulate.
enum ControlOutcome<'i> {
    Enter(Box<Frame<'i>>),
    Flow(BlockFlow),
}

/// Handle a `control` node, mirroring `interpret_control`: a structured control
/// yields `Enter`; leaf jumps run through the reused leaf fns with the first
/// non-`Continue` winning.
fn exec_control<'i>(
    control: Pair<'i, Rule>,
    output: &mut Output,
    state: &mut State,
) -> Result<ControlOutcome<'i>, ParsingError> {
    for stmt in control.into_inner() {
        let leaf = match stmt.as_rule() {
            Rule::while_statement => return enter_while(stmt, output, state),
            Rule::loop_statement => return enter_loop(stmt, state),
            Rule::for_statement => return enter_for(stmt, output, state),
            Rule::repeat_until_statement => return enter_repeat(stmt, output, state),
            Rule::if_statement => return enter_if(stmt, output, state),
            // Leaf jumps: reuse the recursive path's leaf fns.
            Rule::goto_statement => interpret_goto(stmt, state)?,
            Rule::if_goto_statement => interpret_if_goto(stmt, state)?,
            Rule::case_statement => interpret_case(stmt, state)?,
            Rule::gotos_statement => {
                let (line_no, _) = get_error_context(&stmt, state);
                crate::state::emit_warning(format_args!(
                    "Warning: GOTOS ignored (line {}): the program restart depends on the PLC signal enableGoToStart; continuing with the next block",
                    line_no
                ));
                BlockFlow::Continue
            }
            other => {
                return Err(ParsingError::UnexpectedRule {
                    rule: other,
                    context: "vm::exec_control".to_string(),
                    line_no: 0,
                    preview: String::new(),
                    message: format!("Unexpected control rule: {other:?}"),
                })
            }
        };
        // First non-Continue leaf jump wins (matches interpret_control).
        if !matches!(leaf, BlockFlow::Continue) {
            return Ok(ControlOutcome::Flow(leaf));
        }
    }
    Ok(ControlOutcome::Flow(BlockFlow::Continue))
}

/// `WHILE cond ... ENDWHILE`: test once; enter the body scope iff it passes.
fn enter_while<'i>(
    stmt: Pair<'i, Rule>,
    _output: &mut Output,
    state: &mut State,
) -> Result<ControlOutcome<'i>, ParsingError> {
    let mut pairs = stmt.into_inner();
    let cond = pairs.next().expect("while: condition");
    let body = pairs.next().expect("while: body blocks");
    if evaluate_condition(cond.clone(), state)? && 0 < state.iteration_limit {
        let frame = new_frame(collect_blocks(body), FrameKind::While { cond, count: 1 }, state);
        Ok(ControlOutcome::Enter(Box::new(frame)))
    } else {
        Ok(ControlOutcome::Flow(BlockFlow::Continue))
    }
}

/// `LOOP ... ENDLOOP`: unconditional; always enter the body (guarded by the
/// iteration limit at each back-edge).
fn enter_loop<'i>(stmt: Pair<'i, Rule>, state: &mut State) -> Result<ControlOutcome<'i>, ParsingError> {
    let body = stmt.into_inner().next().expect("loop: body blocks");
    let frame = new_frame(collect_blocks(body), FrameKind::Loop { count: 0 }, state);
    Ok(ControlOutcome::Enter(Box::new(frame)))
}

/// `FOR var = a TO b ... ENDFOR`: init the counter, evaluate the bound, enter
/// the body iff `counter <= bound`.
fn enter_for<'i>(
    stmt: Pair<'i, Rule>,
    output: &mut Output,
    state: &mut State,
) -> Result<ControlOutcome<'i>, ParsingError> {
    let mut pairs = stmt.into_inner();
    let assignment = pairs.next().expect("for: assignment");
    let (assign_line_no, assign_preview) = get_error_context(&assignment, state);
    let (var, initial) = interpret_assignment(assignment, state)?;
    let Some(initial) = initial else {
        return Err(ParsingError::with_context(
            assign_line_no,
            assign_preview,
            "FOR statement".to_string(),
            format!("FOR counter '{var}' cannot be initialized with a string"),
        ));
    };
    output.record_variable_change(&var, initial);
    let to_expr = pairs.next().expect("for: TO expression");
    let end = evaluate_expression(to_expr, state)?;
    let body = pairs.next().expect("for: body blocks");

    let current = *state.symbol_table.get(&var).expect("FOR counter set");
    if current <= end {
        let frame = new_frame(collect_blocks(body), FrameKind::For { var, end }, state);
        Ok(ControlOutcome::Enter(Box::new(frame)))
    } else {
        Ok(ControlOutcome::Flow(BlockFlow::Continue))
    }
}

/// `REPEAT ... UNTIL cond`: body-first; always enter the body once.
fn enter_repeat<'i>(
    stmt: Pair<'i, Rule>,
    output: &mut Output,
    state: &mut State,
) -> Result<ControlOutcome<'i>, ParsingError> {
    let mut pairs = stmt.into_inner();
    let first = pairs.next().expect("repeat: first pair");
    let (body, cond) = if first.as_rule() == Rule::comment {
        let last = output.last_mut().expect("row started");
        last.insert("comment", Value::Str(first.as_str().to_string()));
        let body = pairs.next().expect("repeat: body blocks");
        let cond = pairs.next().expect("repeat: condition");
        (body, cond)
    } else {
        let cond = pairs.next().expect("repeat: condition");
        (first, cond)
    };
    let frame = new_frame(collect_blocks(body), FrameKind::Repeat { cond, count: 0 }, state);
    Ok(ControlOutcome::Enter(Box::new(frame)))
}

/// `IF cond ... [ELSE ...] ENDIF`: enter the taken branch as a straight scope.
fn enter_if<'i>(
    stmt: Pair<'i, Rule>,
    output: &mut Output,
    state: &mut State,
) -> Result<ControlOutcome<'i>, ParsingError> {
    let mut pairs = stmt.into_inner();
    let condition = pairs.next().expect("if: condition");
    // Optional comment between the condition and the true block.
    let mut next = pairs.next().expect("if: true block or comment");
    if next.as_rule() == Rule::comment {
        let last = output.last_mut().expect("row started");
        last.insert("comment", Value::Str(next.as_str().to_string()));
        next = pairs.next().expect("if: true block");
    }
    let true_block = next;
    let false_block = pairs.next();

    let taken = if evaluate_condition(condition, state)? {
        Some(true_block)
    } else {
        false_block
    };
    match taken {
        Some(blocks) => {
            let frame = new_frame(collect_blocks(blocks), FrameKind::Straight, state);
            Ok(ControlOutcome::Enter(Box::new(frame)))
        }
        None => Ok(ControlOutcome::Flow(BlockFlow::Continue)),
    }
}

/// Resolve a pending jump against the scope stack, innermost first: on a hit,
/// set that scope's cursor (bounding backward jumps by the iteration limit) and
/// discard the inner scopes it jumped out of; on a miss, pop the scope and retry
/// in the enclosing one. Always resolves or errors (never returns `false`).
fn resolve_across_stack(
    stack: &mut Vec<Frame>,
    request: super::JumpRequest,
    state: &mut State,
) -> Result<bool, ParsingError> {
    loop {
        let top = stack.last_mut().expect("stack non-empty during jump");
        if let Some(dest) = resolve_jump(&top.targets, top.index, &request) {
            if dest <= top.index {
                top.jumps_taken += 1;
                if top.jumps_taken >= state.iteration_limit {
                    return Err(ParsingError::LoopLimit {
                        limit: state.iteration_limit.to_string(),
                    });
                }
            }
            top.index = dest;
            return Ok(true);
        }
        // Not resolvable here: leave this scope and try the enclosing one.
        pop_frame(stack, state);
        if stack.is_empty() {
            return Err(request.into_not_found_error(state));
        }
    }
}

#[cfg(test)]
mod resume_tests {
    //! Demonstrates that the reified stack makes checkpoint/resume work: run a
    //! loop, pause mid-way, snapshot `{Vm, State, Collect output}`, discard the
    //! originals, and resume purely from the snapshot — producing output
    //! byte-identical to a straight run. This is the Phase-1 payoff of #47.
    use super::*;
    use crate::output::Row;
    use crate::types::NCParser;
    use pest::Parser;

    fn axes() -> Vec<String> {
        ["N", "X", "Y", "Z", "A", "B", "C", "D", "E", "F", "S"]
            .iter()
            .map(|s| s.to_string())
            .collect()
    }

    fn parse_blocks(input: &str) -> Pair<Rule> {
        let file = NCParser::parse(Rule::file, input).expect("parse").next().expect("file");
        file.into_inner().next().expect("blocks")
    }

    fn fresh_state(input: &str) -> State {
        let mut s = State::new(axes(), 100_000, None, false);
        s.set_input(input);
        s
    }

    fn run_full(input: &str) -> Vec<Row> {
        let blocks = parse_blocks(input);
        let mut state = fresh_state(input);
        let mut out = Output::collect();
        let mut vm = Vm::new(blocks, &mut state);
        vm.step_until(&mut out, &mut state, None).expect("run");
        out.finish().expect("finish")
    }

    #[test]
    fn checkpoint_mid_loop_resumes_to_identical_output() {
        // 1000 iterations, one output row (`X=R1`) each.
        let program = "R1=0\nWHILE R1<1000\nX=R1\nR1=R1+1\nENDWHILE\nM30\n";

        // Reference: straight run through the VM.
        let reference = run_full(program);
        assert!(reference.len() >= 1000, "expected ~1000 rows, got {}", reference.len());

        // Run until ~500 rows committed, then pause mid-loop.
        let blocks = parse_blocks(program);
        let mut state = fresh_state(program);
        let mut out = Output::collect();
        let mut vm = Vm::new(blocks, &mut state);
        let outcome = vm.step_until(&mut out, &mut state, Some(500)).expect("step");
        assert!(matches!(outcome, Outcome::Paused), "VM should pause at the row budget");
        let at = out.collected_len();
        assert!(at >= 500 && at < reference.len(), "paused mid-loop at {at} rows");
        // We are genuinely mid-loop: the loop counter is partway through.
        let r1 = state.symbol_table["R1"];
        assert!(r1 > 0.0 && r1 < 1000.0, "mid-loop counter R1={r1}");

        // ---- checkpoint: clone the whole resumable state ----
        let ckpt_vm = vm.clone();
        let ckpt_state = state.clone();
        let ckpt_out = out.snapshot_collect().expect("Collect snapshot");

        // Prove resume depends ONLY on the checkpoint: drop the originals.
        drop(vm);
        drop(state);
        drop(out);

        // ---- resume purely from the checkpoint, run to completion ----
        let mut r_vm = ckpt_vm;
        let mut r_state = ckpt_state;
        let mut r_out = ckpt_out;
        r_vm.step_until(&mut r_out, &mut r_state, None).expect("resume");
        let resumed = r_out.finish().expect("finish");

        // Byte-for-byte identical to the straight run.
        assert_eq!(resumed.len(), reference.len(), "row count diverged after resume");
        assert_eq!(
            format!("{reference:?}"),
            format!("{resumed:?}"),
            "resumed output diverged from the straight run"
        );
    }

    #[test]
    fn checkpoint_resume_across_a_backward_jump_loop() {
        // Same shape but built from a GOTOB loop (exercises the jump stack, not
        // a structured WHILE): N10 counts up, GOTOB back to the label.
        let program = "R1=0\n\
             LOOP_TOP: X=R1\n\
             R1=R1+1\n\
             IF R1<800 GOTOB LOOP_TOP\n\
             M30\n";
        let reference = run_full(program);
        assert!(reference.len() >= 800);

        let blocks = parse_blocks(program);
        let mut state = fresh_state(program);
        let mut out = Output::collect();
        let mut vm = Vm::new(blocks, &mut state);
        vm.step_until(&mut out, &mut state, Some(400)).expect("step");

        let mut r_vm = vm.clone();
        let mut r_state = state.clone();
        let mut r_out = out.snapshot_collect().expect("snapshot");
        r_vm.step_until(&mut r_out, &mut r_state, None).expect("resume");
        let resumed = r_out.finish().expect("finish");

        assert_eq!(format!("{reference:?}"), format!("{resumed:?}"));
    }
}
