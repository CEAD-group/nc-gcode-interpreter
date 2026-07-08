# Resumable interpretation — checkpoint/resume design (#47, targeting 0.3.0)

Status: **design draft**. This document scopes the resumable-`NcSession` feature
requested in issue #47 and the interpreter change it requires. No code yet.

## 1. Goal and consumer requirements

Expose a resumable interpretation handle:

- `checkpoint() -> token` — capture the complete execution position roughly
  every *N* output rows.
- `resume(token, patched_text)` — restart interpretation from a checkpoint,
  against an edited program.

Consumer: ribweaver `/sim` phase-E *incremental re-interpret* for multi-million-row
loop expansions. When the user edits source line *L*, it resumes from the last
checkpoint whose block cursor precedes *L* instead of re-running the whole program.
Downstream is evidence-gated: debounced-full re-interpret is fine below ~10⁵ rows,
so the resume path only has to pay off for the *giant* case — which is exactly the
hard one, because a million-row expansion is typically **one** top-level `WHILE`/`LOOP`.
Top-level-only checkpoints would yield a single checkpoint for the whole expansion
and be useless; checkpoints must be capturable **mid-loop**.

Cancellation already exists (drop the iterator → `StreamClosed`); resume composes
with it as the latest-wins primitive.

## 2. Why this is hard — the crux

The issue's premise ("no call stack exists, so block-granularity checkpoints are
tractable") is half right. There is no *subprogram* call stack (#48), but the
**control structures form an implicit stack**, and the instruction pointer lives
on the **Rust call stack**, not in any serializable structure.

Precise findings (`src/interpret_rules.rs`):

- The instruction pointer is `index: usize`, a **stack-local** in `run_blocks`
  (`:2071-2103`) — one per active scope. There is no IP counter in `State`.
- Every control construct drives its body by **recursively calling**
  `interpret_blocks`, holding borrowed `Pair<Rule>` iterators and a `loop_count`
  as stack locals: `WHILE` (`:1386`), `LOOP` (`:1413`), `FOR` (`:1434`),
  `REPEAT/UNTIL` (`:1493`), `IF/ELSE` (`:1305`), `CASE` (`:1623`).
- `Pair<Rule>` is `pest::iterators::Pair<'i, Rule>` — a **borrowed cursor into the
  parse tree**, tied to the lifetime of the input `&str`. Execution positions are
  expressed as *paths within the pest tree*: inherently non-serializable.
- `loop_count` (WHILE/LOOP/REPEAT) is **purely stack-local** — not reflected in
  `State`. `FOR`'s counter *is* in `symbol_table`, but its `end_value` bound and
  `variable_name` are stack-local. Loop *conditions* re-evaluate from `State` each
  iteration, so they are re-derivable — but the position within the tree is not.
- `IF` branch choice is implicit in *which recursive call is currently live*.
- `BlockFlow::Jump` bubbles up through scopes via `resolve_jump` (`:184`); an
  unresolved jump propagates to the enclosing `run_blocks` (`:2098`) — that is how
  a jump leaves a loop or IF body. `EndProgram` (M2/M17/M30) terminates.

**Paused three loops deep inside an IF**, the resume position that is *not* in
`State` is: each nested `index`, each `jumps_taken`, each `loop_count`, each loop's
cached `end_value`/condition/blocks pest pairs, the live IF branch, and every
`block_pairs` slice cursor — all borrowed views into the pest tree.

So mid-loop resume requires **reifying the control stack** into an explicit,
serializable structure: replacing recursion with an execution cursor.

## 3. Convergence — this is already on the roadmap

`docs/sinumerik-execution-model.md` §"Scaling limits and the road to streaming",
point 4, already prescribes this exact change for gigabyte-scale programs:

> *Execution cursor* instead of recursion, yielding `(line_no, row)` — … makes
> seek-by-checkpoint (snapshot of `pc` + `State`) cheap.

So #47 is **not a throwaway rewrite**. The execution cursor it needs is the same
one already planned for streaming/scaling. Reifying the control stack:

1. unblocks checkpoint/resume (#47),
2. unblocks O(1)-start lazy streaming for >10⁷-line programs, and
3. removes the deep-recursion stack-depth risk on pathological nesting.

One change, three payoffs — that is the argument for spending a 0.3.0 on it.

## 4. What a checkpoint must capture (full inventory)

Assembled from a line-anchored audit of the three state-bearing subsystems.

### 4.1 `State` (`src/state.rs:52-86`) — already `#[derive(Clone)]`, cheap
`axes`, `symbol_table`, `string_table`, `translation`, `jump_scopes` (label sets,
mirrors scope depth but not the index within each), `seen_jump_targets`,
`warned_addresses`; config (`axis_identifiers`, `iteration_limit`,
`axis_index_map`, `allow_undefined_variables`, `output_keys`); `input`/`line_offsets`
shared via `Arc` so clone is O(live symbols). ✅ Already snapshot-ready in memory.

### 4.2 Control-flow position (`src/interpret_rules.rs`) — THE reified stack
Not in `State` today; must become an explicit `Vec<Frame>`:
- per scope: `index` (IP), `jumps_taken`;
- per loop frame: kind (WHILE/LOOP/FOR/REPEAT), `loop_count`, and cached bounds
  (`FOR` `end_value` + `variable_name`);
- the scope/branch identity (which blocks list, which IF branch) as a **stable
  address**, not a borrowed pest `Pair`.

### 4.3 Flattener (`src/flatten.rs:130-150`) — NOT currently `Clone`
- Reconstructable from `new(tolerance, axis_identifiers)`: `tolerance`,
  `geometric_axes`.
- Must snapshot: `positions`, `motion` (modal), `plane` (modal), `spline_buffer`
  (raw buffered `Row`s of an in-progress multi-block spline — no compressed form,
  capture verbatim), `spline_start`, `spline_degree`, `warned_motions` (warn-once).
- Blockers: `SplineItem` needs a `Clone` derive (mechanical; wraps `Row: Clone`);
  interned `&'static str` keys re-intern via `intern_column` on *disk* restore only.

### 4.4 Output pipeline (`src/output.rs`)
- `OutputRows::current` — the **in-flight row** for the block being interpreted
  (not yet flushed to the sink); `warned_g91` latch. A checkpoint between blocks
  must capture `current` together with the sink, and reproduce the
  flush-on-next-`start_row` ordering so no row is dropped or double-emitted.
- Batch path forward-fill carry (`BatchBuilder`): `columns` (growing canonical
  set, monotonic — resuming with an empty set changes later-batch schema),
  `fill` (last-seen value per forward-filled column — the core carry).
- `BatchStreamSink`: `buffer` (rows since last emitted batch), `events`,
  `name_ids`, `output_row_count` (drives `row_idx` attribution and batch cadence).
- Plumbing that a checkpoint *cannot* recreate: the channel handles (`Stream`
  sender, batch `sender`, `events_sender`). Resume attaches fresh channels.
- Process-global (outside any checkpoint): the `intern_column` `OnceLock` pool —
  serialize key *content*, rehydrate through `intern_column`.

## 5. Design: the reified execution VM

### 5.1 Addressing a position without a borrowed pest `Pair`
The single change that makes everything else fall out: express an execution
position as a **stable, owned address** instead of a `pest::Pair` cursor.

Two candidate representations:

- **(a) Path into the retained tree.** Keep the parse tree alive (already `Arc`-able
  via the input) and address a block by a `Vec<u32>` path of child indices from the
  root. Serializable (just integers); resolved to a `Pair` by walking `into_inner`.
  Smallest change; still tied to pest at runtime.
- **(b) Pre-flattened owned IR (the execution cursor).** One whole-file pass lowers
  the block/control tree into an owned, indexable program: a `Vec` of block
  instructions with explicit scope boundaries and pre-resolved jump tables (a
  whole-program `scan_jump_targets`). Execution is a `pc` over the IR with an
  explicit frame stack; pest is gone at runtime. This is the roadmap's "execution
  cursor" — larger, but unlocks lazy parse (Phase 3) and full serialization.

**Recommendation: target (b).** It is the roadmap direction; doing (a) then (b) is
double work. (a) is a viable *interim* only if we need #47 shipped before the IR
lands.

### 5.2 The frame stack and the token
```
Frame = { scope: ScopeId, ip: u32, jumps_taken: u32, loop: Option<LoopState> }
LoopState = While { count } | Loop { count } | Repeat { count }
          | For { count, var: SymbolId, end: f64 }
Checkpoint = {
    program_fingerprint,          // structural hash of the program prefix ≤ cursor
    stack: Vec<Frame>,            // the reified control stack (innermost last)
    state: State,                 // cloned
    flattener: FlattenerSnapshot, // §4.3 evolving fields
    output_carry: OutputCarry,    // §4.4 forward-fill + counts + in-flight row
    cursor_line_no: u32,          // for the consumer's "checkpoint precedes L" gate
}
```
Loop conditions are **not** stored — they re-evaluate from `state` on resume,
exactly as the interpreter does each iteration today. That is what keeps the token
small and correct.

### 5.3 `checkpoint()` / `resume()` semantics
- `checkpoint()` is valid only at a **between-rows boundary** (`output.start_row`,
  `interpret_rules.rs:2005`) where the in-flight `current` row is well-defined.
  Emit checkpoints every *N* output rows.
- `resume(token, patched_text)`:
  1. Re-lower `patched_text` to the IR.
  2. Verify `program_fingerprint` against the patched program's prefix up to
     `cursor_line_no`. If the edit changed block structure *before* the cursor, the
     stored `ip`/`ScopeId`s are stale → **fail loudly** (`ParsingError`, a new
     `kind = "checkpoint_invalidated"`), never resume into a shifted tree. This
     matches the consumer's own gate ("last checkpoint whose block cursor precedes
     L") and the project's loud-failure-over-silent-wrongness rule.
  3. Restore `state` + frame stack + flattener + output carry; attach fresh output
     channels; continue the VM loop from `stack`.

### 5.4 Resume + edited text — the validity contract
A checkpoint is resumable against a patched program **iff the patch lies at or
after the checkpoint's block cursor**. Enforced by the prefix fingerprint. The
consumer already honours this; the API makes it a checked invariant rather than a
convention.

## 6. Phasing

- **Phase 1 — reify the stack (delivers #47).** Replace the recursive tree-walk in
  `run_blocks`/`interpret_*` with the explicit VM + frame stack over IR (b), keeping
  eager whole-file parse. Add `NcSession` with in-memory, **same-process**
  `checkpoint()`/`resume()` (token holds owned `State`/frames/snapshots; no disk
  serialization yet). This is everything the consumer needs.
- **Phase 2 — serializable token.** `Serialize`/`Deserialize` for the snapshots;
  re-intern `&'static str` keys on load; address symbols/scopes by stable ids.
  Only if cross-process/persisted checkpoints are wanted.
- **Phase 3 — lazy parse / O(1) start.** The streaming payoff: lazy per-line parse
  feeding the same VM, demoting pest to the rare structural lines (the two-pass
  design already prototyped in `src/line_driver.rs`). Independent follow-on.

Phases 2 and 3 are optional and can slip past 0.3.0; Phase 1 is the release.

## 7. Correctness strategy

The VM must be a **behaviour-preserving** refactor of the tree-walker before
`checkpoint` is even exposed. Guardrails:

- **Differential parity**: run the whole existing golden-file suite + the mill-sim
  corpus through both the old recursive path and the new VM, asserting byte-identical
  output — the same discipline `test_stage1.py` already uses for the line-driver.
- **Checkpoint/resume equivalence**: for a program run straight through vs.
  checkpointed-and-resumed at every row boundary, assert identical `(line_no, row)`
  streams and identical final `State` — including the hard cases: mid-`WHILE`,
  mid-`FOR` (counter in `State`, bound in the frame), mid-spline (buffered
  `spline_buffer`), a jump out of a loop body, and forward-fill continuity across a
  resumed batch boundary.
- **Invalidation**: edits before the cursor must fail loudly, never silently resume.

## 8. Rough scope

Phase 1 is real interpreter surgery — the IR lowering, the VM loop, the frame
stack, `FlattenerSnapshot`/`OutputCarry` capture-restore, and the parity harness —
but it is bounded and well-understood after this audit, and it is load-bearing for
the streaming roadmap regardless of #47. Estimate: the dominant 0.3.0 work item.
Phases 2–3 are additive and independently schedulable.

## 9. Open questions for review
- (b) IR now vs (a) tree-path interim — how urgently does the consumer need #47
  relative to the streaming work? If both are wanted this quarter, go straight to (b).
- Token type: opaque handle only (Phase 1) vs. a documented serializable schema —
  affects how much of §5.2 is public API from day one.
- Checkpoint cadence policy: fixed *N* rows vs. adaptive (e.g. denser near loop
  headers so an edit resumes closer to *L*).
