# Statement AST experiment (branch `explore/statement-ast`)

Experiment of 2026-10-14: one owned per-line AST produced from pest and from
the byte decoder, one executor, and a streaming driver that takes block
nesting from the structure scan, so no whole-file parse tree is built. It
runs beside the three existing paths and is selected with `NC_EXEC=ast`.

## Modules

| File | Role |
| --- | --- |
| `src/ast.rs` | The owned AST of one block (one line); the supported subset |
| `src/lower.rs` | pest `block` / `line_block` pairs to `ast::Block`, precedence climbing |
| `src/decode_ast.rs` | The byte decoder's words to `ast::Block`; equal to `lower` on the same text |
| `src/structure_scan.rs` | `scan_structure_table`: per-line opener / ELSE / closer table |
| `src/exec.rs`, `src/exec/expr.rs` | The executor: every semantic of interpret_rules.rs, matched exhaustively |
| `src/drive.rs` | The streaming driver: pre-pass, control stack, jumps by line index |
| `tests/ast_parity.rs` | Default path against AST path on examples, named cases and seeded random programs |

The grammar gained a `line` entry whose control alternative includes the
line-level structure forms (`if_head`, `else_kw`, `endif_kw`, `while_head`,
...); the existing rules are unchanged and the sparse parse costs the same.

## Results

Release builds, `nc_to_batches`, best of 3, peak RSS of the process. The
first AST column is 561be8a (phase 2), the second the head after phase 3;
each row was measured back to back in one session, and other sessions on the
machine moved the default's own time by up to 10% between rows.

| Program | Default | AST path, 561be8a | AST path, phase 3 |
| --- | --- | --- | --- |
| 100 WHILE loops, 100k lines | 558 ms, 366 MiB | 239 ms, 330 MiB | 180 ms, 336 MiB |
| 50k grammar-bound lines | 186 ms, 168 MiB | 266 ms, 142 MiB | 250 ms, 143 MiB |
| 400k straight lines | 376 ms, 647 MiB | 776 ms, 496 MiB | 504 ms, 491 MiB |
| 112 MB field program, 1.28M lines | 2.25 s, 2.3 GiB | 3.45 s, 1.7 GiB | 2.48 s, 1.9 GiB |
| 5M-line field sample, 252 MB | 5.2 s, 2.9 GiB | 8.4 s, 2.1 GiB | 5.5 s, 2.2 GiB |

Both suites pass under `NC_EXEC=ast` (209 lib, 46 parity and 3 CLI tests in
Rust; 392 passed, 4 skipped in Python) and the CLI output is byte-identical.

Phase 3 made two changes to the straight-line path. A trivial line is
decoded into one reused block that keeps its statement list, strings and
product boxes, so a line whose shapes the buffer has seen allocates nothing
(a counting-allocator test checks it; per output row both paths now make the
same 8 allocations, all for the row's cells). A program longer than one
chunk of 4096 lines decodes the four chunks after the running one on Rayon,
so the run loop no longer decodes on the interpreting thread; lines behind
the running chunk are decoded again as before.

Where the straight-line time goes now (400k lines, samply, per run): the AST
path spends 0.40 s interpreting where the default spends 0.24 s, and output
batching is 0.20 s on both. Of the 0.40 s, executing takes 0.20 s (the
default's executor 0.17 s, the difference spread over the general executor's
dispatch, the largest item being G code classification at run time), the
pre-pass 0.12 s (its decode 0.085 s), loading a prefetched line into the
reused block 0.04 s, the GOTO scan 0.02 s and the structure scan 0.02 s. The
pre-pass is the one sequential decode left: in a scratch copy that reads the
pre-pass's lines from the same prefetched chunks (same rule, same double
decode), the AST path took 427 ms against 386 ms on 400k lines and 2.15 s
against 2.31 s on the field program.

## Verified semantics and decisions (phase 1 notes)

Branch explore/statement-ast. Items marked "verified" were run against the
current default path (CLI on a release build) on 2026-10-14.

## Canonical form (ast.rs)

Both producers (`lower`, `decode_ast`) build this form, so equal text gives
equal blocks.

- Names of variables, array elements and assignment targets are uppercased.
  The tree walker uppercases every identifier before use (`interpret_variable`,
  `interpret_identifier`, `variable_single_char` handling), so nothing reads
  the original case of these names.
- An axis word `X10` and the assignment `X=10` are the same
  `Assignment::Var { name: "X", rhs: Rhs::Expr(Number(10)) }`. The tree walker
  treats `(variable_single_char, value)` and `(variable, expression)` alike, and
  the decoder already emits `Word::Assign` for both.
- A parenless call whose uppercased name is in the G vocabulary and is not a
  frame keyword is `Statement::G`, decided at lowering (`classify_g_command` is
  a pure table). The decoder emits `Word::GCommand` for these. Frame keywords
  stay `Statement::Call`, so the executor raises the "frame instruction after
  another statement" error.
- `Expr::binary` / `Expr::unary` fold `+ - * /` over two literals and unary
  minus over a literal into `Expr::Number`. The decoder folds constant
  expressions at decode time with the same operation order, so the results are
  bit-identical. DIV, MOD, NOT, comparisons and logic are not folded (DIV can
  fail, the decoder never folds the others). `Paren` is never folded away.
  `0/0` folds to NaN, which `PartialEq` cannot compare; keep it out of
  equality tests.

## Text kept as written, and why

- `Index.text` (trimmed index source): `interpret_indices` tests
  `state.is_axis(text)` (case-insensitive), then looks the text up in the axis
  index map case-sensitively (`get_axis_index`), and `InvalidAxisIndex` quotes
  it. `read_actual_position_sysvar` uses `text.to_uppercase()`. So `A[x]` and
  `A[X]` can differ, and `A[(X)]` is not an axis index.
- `Arg::Str.text`: `eval_number_arg` says "expected a number but got the
  string {text}" with the raw span; a `<<` chain span can end in whitespace.
- `Call.text`: the row value. A parenless call keeps the whitespace pest skipped
  while trying the optional argument list (`"LAYER_CHANGE "`). `Call.name` as
  written feeds the typo warning and the frame keyword check.
- `Expr::Call.name` as written: `InvalidFunctionArity` quotes it; the unknown
  function message uppercases it.
- Block number digits as written (the row keeps leading zeros). The grammar's
  `integer` allows a sign, so `N-5` is the block number "-5".
- M code: `as_str().trim_end()` uppercased. `m_command` is non-atomic, so
  `M 30` parses and stays "M 30", which is not an end of program.
- `$ NAME`: `nc_variable` is non-atomic, so the name keeps the whitespace,
  exactly as the tree walker keys it.
- GOTO target trimmed as written; the executor applies `canonical_jump_target`.
- `DataType::String(n)`: the length digits are kept although the walker
  ignores them.

## Semantics the executor must reproduce (interpret_rules.rs)

- Omitted arguments: `function_arguments` yields no pair for an empty
  position, so the walker evaluates `F(1,,2)` as `F(1,2)`. Verified:
  `X=ATAN2(1,,1)` gives 45. The executor must skip `Arg::Omitted`.
- SET/REP outside DEF: `interpret_statement` has no arm for
  `assignment_multi`, so `A[2]=SET(1,2,3)` is `UnexpectedRule { rule:
  assignment_multi, context: "interpret_statement", message: "Unexpected rule
  in interpret_statement: assignment_multi" }` (verified). The port needs
  `Rule::assignment_multi` to build that error.
- REP is a SET of its one or two expressions (`interpret_value_array`):
  `DEF REAL A[2]=REP(7,2)` sets A[0]=7, A[1]=2 and leaves A[2] undefined
  (verified). Elements a SET/REP leaves out are not zero-initialized.
- `interpret_variable_array` builds every element name up to the index (a read
  of `A[100,100]` builds 10,201 names) and uses the last. DEF arrays and SET
  need the whole list; a read can compute the last name directly but must keep
  the "Invalid number of indices" error for more than three indices.
- Bit operators: the walker's error quotes the operator as written
  (`operator.as_str()`); the AST keeps only `BinaryOp`, and
  `BinaryOp::symbol()` is uppercase. A lowercase `b_and` with a fractional
  operand would say "B_AND requires ..." instead of "b_and requires ...". No
  test uses lowercase bit operators; accepted deviation.
- The parenless keyword path of `interpret_statement` does not call
  `set_plane`, the `g_command` path does. Only G17/G18/G19 are in gg06, so the
  canonical `Statement::G` cannot change the plane differently.
- FOR on an axis counter: `interpret_assignment` stores the axis, the loop
  test reads `symbol_table`, finds nothing, and the body never runs (verified:
  `FOR X=1 TO 3` runs nothing). The init goes through `interpret_assignment`
  only, so an axis counter gets no row cell.
- Loop limits: WHILE is `while cond && count < limit`, then `count >= limit`
  is an error, so a WHILE that runs exactly `limit` times fails (verified with
  limit 3). REPEAT counts after the body and checks the limit before the UNTIL
  condition, so a REPEAT that would stop on pass `limit` fails (verified). LOOP
  counts after each pass. FOR has no limit.
- Backward jumps: `run_blocks` counts jumps with `destination <= index` per
  scope invocation. Each loop pass re-enters the body scope, so the count
  restarts every pass.
- GOTOC: suppressed with a warning (Continue) when the target is on no scope of
  `state.jump_scopes`, checked when the jump runs.
- GOTOS: warning, Continue.
- `state.seen_jump_targets` grows with each scope's targets on every scope
  entry; the not-found hint reads it.

## Structure lines: which part of a line belongs where (verified)

The whole-file grammar splits a structure line into parts that run at
different times. The AST keeps the line as one `Block`; the executor contract
(exec.rs doc) assigns the parts.

- `[N] [L:] IF c [;cm]`: N and label are the IF block (the label is in the
  enclosing scope). The comment goes into the IF row before the condition is
  evaluated.
- `[N] [L:] WHILE c [;cm]`, and FOR, LOOP, REPEAT: N and label are the
  structure block, run once on entry. The rest of the line is the first body
  block, so every pass starts a row on the head line holding the comment (an
  empty row without one). Verified: `WHILE R1<2 ;head` gives two comment rows.
  A WHILE re-test after ENDWHILE evaluates only the condition.
- `[N] [L:] ELSE [;cm]`: N and label are the last block of the true branch (a
  row with N when the condition was true; verified). The comment is the first
  block of the false branch (verified).
- `[N] [L:] ENDWHILE [;cm]`, and ENDIF, ENDFOR, ENDLOOP, `UNTIL c`: N and label
  are the last block of the body, a row on the closer line on every pass;
  ENDFOR's increment is recorded on that row. With an ELSE, the ENDIF prefix is
  the last block of the false branch. The comment belongs to the block that
  contains the whole structure: it is written into the current row once the
  structure finishes, whatever the reason (normal exit, a jump out, verified;
  end of program), never on error. `IF 0 ;a` ... `ENDIF ;b` leaves one row with
  comment ";b" (the IF row, overwritten; verified).
- Blank and label-only lines start empty rows in the walker, which flush
  drops; the driver can skip them. The closer line's empty row is observable
  (FOR increment, the line a closer comment lands on, line numbers).

## Not represented: rejected loudly by the `line` entry

The whole-file grammar and the structure scan accept these, the `line` entry
does not (a parse error with the line):

- A body on the head line: `WHILE c X1`, `LOOP X1`, `REPEAT X1`,
  `FOR I=1 TO 3 X=I`, `WHILE c GOTOF L`, `ELSE X2`. The old grammar runs X1 as
  the first body block of every pass (verified for WHILE).
- A mid-line ELSE: `X1 ELSE`, `X1 ELSE X2`. The scan only reads first words
  and ELSE does not change its stack, so it passes; the grammar ends the true
  branch at ELSE (verified).
- Statements before or after a closer (`X1 ENDWHILE`, `ENDWHILE X1`): the scan
  already rejects the first, the grammar the second, on both paths.

None of these occur in examples/*.mpf (every example line lowers,
`lower::tests::every_example_line_lowers`); a rough grep of python/tests found
none either.

## Grammar decisions

- `line_control` has the four single-line control forms, then the line-level
  structure forms, and leaves out `if_statement` .. `repeat_until_statement`:
  in a sparse buffer they would match across lines and break one block per
  line.
- `if_head` has a `!goto_kw` guard: an IF with a malformed jump is not a block
  head.
- `for_head` keeps TO unbounded as in `for_statement`, so `FOR R1=0 TOR2`
  parses on both paths.
- The new keywords are word-bounded; the old structure rules are not
  (`WHILER1<5` parses as WHILE R1<5 there), but the scan requires word-bounded
  first words, so the accepted language is the same.
- `sparse_line_blocks` is the sparse variant over `line_block`, for the
  driver's option (b); tested in `lower::tests`.
- `describe_rule` has arms for the new rules (unknown rules read "a G code").
- AST-path parse errors have context "line parsing", like the fast path; the
  whole-file path says "initial file parsing". The expected-rule list of a
  `line` parse can differ from a whole-file parse at the same column, so
  parity tests should compare only the line for parse errors, as
  `line_driver::tests` does.
- Names like `S1`, `T5`, `F2` are axis words (axis_word is tried first), so
  `S1="txt"` is a parse error on every path.

## Structure table

- `scan_structure_table` shares the scan loop with `check_structures` (same
  errors, same order; `errors_match_check_structures`). It also rejects a
  second ELSE in one IF ("IF on line X already has an ELSE on line Y", at the
  second ELSE); the default path gets a pest parse error at that line instead.
- Inherited scan quirk: `contains_goto_word` also sees GOTO inside a string,
  so `IF MATCH(S,"GOTO")>0` is taken for a single-line jump.

## Pre-existing fast-path divergences (found by the equality test, verified)

The byte decoder claims these lines with a meaning the grammar does not give
them, so the default path and NC_STAGE1=0 disagree:

- `N10X5`: decoder N10 + X5; grammar: the call `N10X5` (its `integer` needs a
  non-letter after the digits). `N10LBL: X1` is a parse error in the grammar.
- `TRANS=5`, `ROT=1` (a frame keyword then `=`): decoder: variable = 5;
  grammar: parse error.
- `X=5 AND Y1` with AND, OR, XOR, B_AND, B_OR or B_XOR after a constant:
  decoder: X=5, call "AND ", Y=1; grammar: X = 5 AND Y1. The decoder rejects
  DIV and MOD for this reason but misses the logic and bit operators.

Pinned by `decode_ast::tests::known_decoder_divergences`; the equality test
covers 553 other decoder-claimed lines.

## Deviations from the brief

- `Flow` has no `GotoStart` (nothing produces it). UntilTail returns
  `ScopeEnd` and the driver calls `eval_condition` after its loop-limit check,
  because the walker checks the limit before the UNTIL condition.
- `Flow::Jump` carries `interpret_rules::JumpRequest` (fields made
  pub(crate)), so the driver can reuse `resolve_jump` and
  `into_not_found_error` for identical messages.
- exec.rs has five entry points besides `exec_block` (`begin_body`,
  `eval_condition`, `for_continues`, `for_step`, `leave_structure`), because
  the parts of a structure line run at different times.
- exec/expr.rs has four functions besides `eval_expr` and `eval_string`
  (`expr_is_string`, `eval_expr_string`, `eval_indices`, `element_keys`),
  which exec.rs also calls.
- No separate system-variable `Expr` variant: `$` names are `Var`/`Element`;
  recognizing `$AA_IW`/`$AA_IM` needs the axis list in `State`.
- `Statement::M` is a String; `Assignment` is an enum (Var / Element with
  `ElementRhs`) so that IC() on an element is not representable.
- `decode_ast` takes the line text: the decoder keeps no operand order for
  IDENT*NUMBER versus NUMBER*IDENT, so `decode_ast` reads the byte after the
  identifier span.
- Visibility only: interpret_rules.rs `JumpRequest` fields, `JumpDirection`,
  `normalize_reserved_case`, `format_nc_concat_number`, `parse_nc_number`,
  `reals_equal`; line_driver.rs `Word`, `DecodedLine` and its fields,
  `DecodeResult`, `decode_line`.
- `check_structures` now calls a shared scan core (behaviour and errors
  unchanged, `ProgramShape` untouched).

## Timings (release, best of 7 to 21 runs, 50k lines of `X=SIN(30)`)

| measurement | before | after |
|---|---|---|
| `sparse_lines` parse | 90.2 to 92.2 ms | 90.4 to 91.4 ms |
| `nc_to_table` | 163 to 169 ms | 162 to 170 ms |

New entry points on the same input: `sparse_line_blocks` parse 90.6 ms;
`line`, one parse call per line, 113 ms; `line` parse plus lowering 146 ms
(lowering about 0.66 us per line). Per-line parsing (option a) costs about 25%
more than one sparse parse before lowering. Reproduce with
`cargo test --release --lib grammar_speed -- --ignored --nocapture`.
