# How a Sinumerik executes NC code — and how this interpreter differs

This interpreter is **not** a Sinumerik emulator. It answers one question:
*what toolpath would result if this program ran on a Sinumerik?* That goal
allows — and sometimes requires — a different architecture than the real
control. This document records how the real control works, where this
interpreter deliberately deviates, and the measurements behind those
decisions. Manual references are to the SINUMERIK ONE NC programming manual
(sections 4.1.5.x, 4.1.7) and the Basic Functions manual (3.5.x).

## The real control: a cursor with a bounded pipeline

A Sinumerik never parses a program file as a whole. It runs a pipeline over
a *text cursor*, decoding one block (line) at a time:

```
text cursor ──interpret/decode──► block preparation ──► IPO buffer ──► interpolator
                                  MD28070, ~10 KB/block   MD28060        (servo clock)
```

- **MD28070 `$MC_MM_NUM_BLOCKS_IN_PREP`** sets how many blocks can be in
  preparation at once (roughly 10 KB of dynamic memory is reserved per
  block). **MD28060 `$MC_MM_IPO_BUFFER_SIZE`** sets the FIFO of fully
  prepared blocks the interpolator drains (SD42990
  `$SC_MAX_BLOCKS_IN_IPOBUFFER` can throttle it). Together they form the
  "lookahead window" of a few hundred to a few thousand blocks that runs
  ahead of the actual tool position.
- **Program start is O(1) in file size.** Starting a program just starts
  filling the pipeline; a 1 GB program starts as fast as a 1 KB one.
- **A syntax error is not a parse error but a runtime alarm.** The
  interpreter stage raises it when the malformed block *enters the
  preparation window* — typically well before the tool reaches it, but
  never at program load. A malformed line near the end of a long program is
  discovered near the end of the run.
- **`GOTO` is literally a search command.** The destination is found by
  scanning blocks in the programmed direction (alarm 14080 fires when the
  *search* fails). Structure keywords work the same way: "when a loop end
  is detected, a search is made for the loop beginning" (manual 4.1.7).
  Consequences that only make sense for search-based resolution:
  - duplicate labels are legal; the *nearest occurrence in the search
    direction* wins — there is no symbol table;
  - a `GOTO` to a missing label alarms only when it executes; an
    unexecuted bad jump is never noticed;
  - a jump invalidates everything already prepared, so jump commands imply
    a preprocessing stop — `GOTOS` "internally initiates a STOPRE"
    (manual 4.1.5.1).
- **The window is a real limitation.** With "Execution from external
  source" (program streamed through a bounded reload memory), jump
  destinations that have left the window are simply *gone*: the program
  aborts with alarm 14000. Siemens' own streaming-input mode is documented
  as incompatible with jumps; their recommended fix is EES — execution from
  storage that supports random access.

The control pays these costs to get bounded memory, instant start and hard
real-time guarantees. An offline interpreter has none of those constraints.

## What this interpreter does instead

| Aspect | Sinumerik | this interpreter |
| --- | --- | --- |
| program loading | streamed through a bounded window | whole file in memory |
| parsing | per block, on demand, at execution time | whole file up front (pest PEG) |
| syntax errors | runtime alarm when the block enters the prep window | error before any output row (parse is eager) |
| structure (`IF`/`ENDIF`, loops) | resolved by runtime keyword search | resolved by the grammar at parse time |
| jump destinations | directional text search at execution time | directional search over the parsed block list at execution time (same observable semantics, incl. duplicate labels and lazily-detected missing targets) |
| `M2`/`M17`/`M30` | end of program | end of interpretation (blocks after an executed end marker do not run) |
| `GOTOS` | restart if the PLC requests it via `enableGoToStart` | continue with the next block (the documented no-request behavior); a restart would produce an unbounded trace |
| PLC, timing, feed, servo | real | not modeled — the output is the geometric/modal trace, not motion timing |
| memory | O(window) | O(file + parse tree + trace) |

The one deliberate philosophical difference: **validation is a feature.**
Knowing up front that a file parses — and getting the error immediately
when it does not — is worth more to an offline tool than emulating the
control's discover-errors-at-runtime behavior. Note the asymmetry that
remains: *syntax* errors are eager here, but *semantic* runtime errors
(undefined variable, missing jump target, iteration limit) still surface
only if and when the offending block executes, exactly like the control.

## Is eager whole-file parsing fast enough? (benchmark)

Harness: `src/interpreter.rs`, test `parse_speed_1m_lines` — generates a
deterministic 1M-line large-format-additive-style flood file (~29 MB:
mostly `X.. Y.. Z..` moves, some with `E`, `A/B/C`, an external axis,
modal `G1 F..`, block numbers, comments, spline sections) and times the
pest parse and the full `nc_to_table` pipeline separately.

```
cargo test --release --lib parse_speed -- --ignored --nocapture
# BENCH_LINES=... and BENCH_MODE=xyz|g1|g54|elx|comment|bspline isolate shapes
```

Results on an Apple-silicon laptop (release build, best of 3, ±10% run
noise):

| stage | 1M lines / 29 MB |
| --- | --- |
| pest parse | **3.2 s** (~310 klines/s, ~9 MB/s) |
| full `nc_to_table` (parse + interpret + table) | **5.6 s** |
| parse tree size | ~14.3 pairs/line (14.3M pairs) |
| peak RSS, whole pipeline | ~1.7 GB (≈ 57× input) |

So the "<10 s for 1M lines" goal holds with ~3× headroom, and parse time
is roughly linear in line count.

### What the profile showed

Sampling profile (samply) of the parse:

- ~30% of time in `pest::ParserState::match_insensitive` — the `^"KEYWORD"`
  matcher. The volume came from ordered-choice walls: every line attempted
  the control-statement keywords before falling through to statements, and
  every bare axis word attempted `identifier`, whose `reserved`
  negative-lookahead tries ~20 case-insensitive keywords. G-commands like
  `G54` walk ~115 literals through the gg-group walls (gg08 alone holds
  ~100 `G5xx` literals sorted longest-first).
- Per-line cost is remarkably flat (~3.5 µs) regardless of content: the
  floor is pest's combinator machinery itself (boxed parser state, token
  queue, implicit-whitespace calls), not any single rule.

### Optimizations applied (measured on the mixed flood)

1. `axis_word` fast path hoisted to the front of `statement`, so flood
   coordinates never touch `identifier`/`reserved`.
2. `statement+` tried before `control` in `block` — safe because every
   control keyword is in `reserved` and can never parse as a statement
   (`definition` and `frame_op` must stay first: `DEF`/`TRANS`/... are not
   reserved, deliberately, so identifiers like `TRANS_X` keep working).
3. `comment` made atomic — pest otherwise runs the implicit-whitespace rule
   between *every character* of every comment.
4. `value` made atomic — the interpreter only reads the lexeme, so the
   inner `float`/`integer` pairs were two dead tokens per coordinate.

Net effect: 3.74 s → 3.2 s parse (~15%), 17.5M → 14.3M pairs.

5. Moving the G vocabulary out of the grammar entirely (the ~600-literal
   generated gg walls became a Rust lookup table, the grammar recognizes
   only the lexical shapes `G<digits>` and `GFRAME[<n>]`) more than
   halved what remained: **1.43 s** for the 1M-line mixed flood,
   0.99 s for a pure-`G54` flood (was 3.58 s), 1.47 s for a real-world
   21 MB (319k-line) program (was 2.37 s). The win applies to *all*
   line shapes because the compiled parser shrank by hundreds of literal
   matchers. Beyond this, a substantially faster front end means the
   stage-1 line triage below — not more grammar tuning.

A trap worth recording: **PEG ordered choice commits.** In
`(^"GOTO" | ^"GOTOF" ...) ~ boundary`, an input `GOTOF` matches `^"GOTO"`,
the boundary check fails on `F`, and the whole expression fails *without
trying* `^"GOTOF"`. Keyword alternatives sharing a prefix must be sorted
longest-first (`reserved`, `goto_kw`, and the generated gg-groups all do
this now). Reordering rules for speed can expose latent shadowing bugs —
the golden-file suite is the safety net.

## Scaling limits and the road to streaming

Extrapolating: ~10M lines (≈300 MB) parses in ~35 s and needs several GB —
workable. A true 1 GB program (~35M lines) would need ~2 minutes and tens
of GB peak for tree + trace: **whole-file eager parsing stops being the
right answer somewhere around 10⁷ lines.** The control's architecture
shows the escape hatch, adapted for an offline tool that can afford one
cheap whole-file pass:

1. *Line index*: one newline scan (we already compute `line_offsets`).
2. *Skeleton scan*: same pass records labels, block numbers and structure
   keywords — an O(1) jump table instead of the control's O(distance)
   search, same observable semantics.
3. *Lazy per-line parsing*: pest stays, entry rule `block`, parsed when the
   cursor first reaches a line; hot lines (loop bodies) memoized.
4. *Execution cursor* instead of recursion, yielding `(line_no, row)` —
   which is also exactly the streaming/generator API a robot-cell
   simulator or visualizer wants (`next()` → source line + next trace
   row), and makes seek-by-checkpoint (snapshot of `pc` + `State`) cheap.

Until programs of that size actually appear, eager parsing keeps the
stronger up-front guarantees at trivial cost.

## Two-pass triage: measured on real CAM output

Real post-processor output (mill-sim `test-case-1`, eight programs,
393,724 lines total) is even more skewed than the synthetic flood:

- **99.96–100% of lines** match a conservative "trivial line" shape:
  `N<d>` + words that are `LETTER<num>`, `ident=<num>`, `G<d>`/`M<d>` or a
  bare known keyword, plus an optional comment. The entire remainder
  across all eight files is ~30 lines (`DEF`, `TRANS ...=expr`,
  `SETAL(...)`, `TRAORI`, ...) — all *single-line* constructs.
- **Zero control structures.** CAM output is straight-line; IF/WHILE/GOTO
  appear in hand-written parametric programs, which are small.

A prototype byte-level scanner for exactly that shape (in the
`parse_speed` bench, `BENCH_FILE=...`), decoding key/value pairs as it
goes:

| | 319,591-line real program (20.6 MB) |
| --- | --- |
| whole-file pest parse | 2.37 s (8.7 MB/s), 13.8M pairs (~43/line) |
| stage-1 scanner | **52 ms (395 MB/s)**, 3 lines left for pest |

So the two-pass design — triage lines with a fast scanner, give only the
non-trivial remainder to pest, merge by line order — costs ~45× less than
parsing everything with the full grammar, extrapolates a 1 GB file to
~3 s instead of ~2 minutes, and eliminates the parse-tree memory for
flood lines entirely. pest is demoted to what it is good at: the rare,
structurally interesting lines.

This design is now implemented in `src/line_driver.rs`, gated by the
structure scan: programs without multi-line control structures (all CAM
output; labels and the whole GOTO family remain supported since jumps are
line-scoped) are interpreted line by line — trivial lines through a
conservative byte decoder, the rare rest through per-line pest parses
padded to their file position so error messages keep whole-file
coordinates. Bare words are claimed only via the vocabulary table, so
`GOTOF LABEL` and reserved words always take the grammar. `NC_STAGE1=0`
disables the fast path; the differential tests
(`python/tests/test_stage1.py`) run synthetic edge cases and the whole
mill-sim corpus through both paths and assert identical output.

Measured on the real-world 21 MB (319k-line) program, end to end: the Rust CLI
(decode + execute + table + CSV) went 5.0 s → **2.9 s**, and
`nc_to_dataframe` from Python 33 s → **9 s**. Parsing has left the
profile entirely; what remains is row assembly (`HashMap` rows,
forward-fill, the pyo3 → polars conversion).

## Streaming rows (nc_to_rows)

The interpreter's output is a sink (`OutputRows`): collected into the
batch table, or pushed row by row into a bounded channel drained by a
Python iterator while interpretation runs on a worker thread.
`nc_to_rows(program, ...)` yields `(line_no, row_dict)` — the 1-based
source line each block came from (loops and jumps repeat and reorder
line numbers, which is exactly what a visualizer needs for
trace-to-source highlighting), and values typed and forward-filled like
the batch DataFrame. Dropping the iterator hangs up the channel and
aborts interpretation — breaking out of a `for` loop over an anonymous
iterator drops it, but a stored iterator keeps the worker alive (parked
on the bounded channel) until it is deleted or garbage-collected.
Errors raise from `next()` at the offending row; the final state is
available on the iterator once exhausted.

On the real-world 21 MB (319k-line) program: the first row arrives after ~1.6 s
(the price of eager whole-file validation — the decode/parse passes run
before execution starts), a full drain takes ~8.6 s versus ~10 s for the
batch DataFrame, and memory stays constant. The differential tests
(`python/tests/test_streaming.py`) assert the streamed rows reconstruct
the batch DataFrame exactly.

With `include_variables=True` the iterator yields `(line_no, row_dict,
variables_dict)` instead: every variable assignment a block performs
(`R1=R1+1`, `DEF REAL Q=5`, FOR counters — the counter increment
surfaces on the ENDFOR line, where the control performs it) arrives as
a per-row delta, and blocks that only assign variables — pruned from
the batch DataFrame — are streamed too, with an empty row dict. A live
query of the interpreter's symbol table would be wrong by construction
(the worker runs ahead of the consumer behind the channel buffer);
deltas riding on the rows are race-free, and accumulating them with
`dict.update` reconstructs the full variable state at any row.

Not yet built: checkpoints (`position` + `State` snapshot) for
seek/scrub in a visualizer — straightforward on top of the line driver
when the need arrives.

## Requirement: good errors when a file does not parse

Up-front validation is only as valuable as its error messages. The
current state, probed with representative mistakes:

| mistake | today | quality |
| --- | --- | --- |
| `G1 X10 Y=` (incomplete expression) | right line, caret, "expected expression or axis_increment" | good — pest at its best |
| missing `ENDIF` | error at EOF ("expected comment, control, …"), nothing points at the unclosed `IF` | bad — PEG reports the farthest position, not the cause |
| `G1 X10 Y2O` (letter-O typo) | **no error** — `Y2O` silently becomes a subprogram-call column, the Y move is dropped | worst case |
| `G999` (unknown G code) | **no error** — silently a subprogram call | bad (real control: alarm 12470) |
| `GOTOF LAB1`, label typo'd elsewhere | precise runtime error with search direction and alarm reference | good, could add did-you-mean |

Four distinct problem classes, four distinct fixes:

1. **Wrong-position structural errors** (unclosed IF/WHILE/…): a PEG
   cannot say "the IF on line 2 is never closed" — it fails at EOF. The
   *line-level structure scan* from the triage design matches opener and
   closer keywords per line and reports exactly that, before pest runs.
   The same pass that indexes jump labels fixes the worst error class.
2. **Silent misparse through the catch-all**: arbitrary bare words are
   legal (subprogram calls), so the grammar cannot reject them — but the
   interpreter can diagnose: `G<digits>` not in the vocabulary table →
   hard error ("unknown G code", matching alarm 12470); a bare word
   shaped like `LETTER<digits><junk>` → "did you mean an axis word?"
   warning. Needs the vocabulary table in Rust — the gg-group extraction
   again.
3. **Noisy expected-sets and internal rule names** leaking into messages
   (`gg08_work_offset`, `non_returning_function_call`): pest's
   `Error::renamed_rules` maps rule names to human phrasing ("a G code",
   "an axis word", …); cheap, immediate.
4. **One error per run**: whole-file parsing stops at the first failure.
   Per-line parsing (the triage architecture) isolates errors — every
   line validates independently, so a validation pass can report *all*
   bad lines with exact positions in one round trip.

The notable conclusion: the error-quality requirement *strengthens* the
two-pass line-oriented design rather than arguing for more grammar. A
whole-file PEG gives one error, sometimes at the wrong place; per-line
triage plus a structure scan plus a vocabulary table give multi-error
reports, matched-pair diagnostics and did-you-mean — while pest stays
exactly where its errors are good: inside complex single lines.
