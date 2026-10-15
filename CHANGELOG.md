# Changelog

Notable changes to **nc-gcode-interpreter**. The format loosely follows
[Keep a Changelog](https://keepachangelog.com/); versions are git tags,
released to PyPI.

## [v0.2.8] - 2026-10-15

### Added

- `MATCH` and `MINDEX` string search functions (manual 4.1.4.6), next to
  `INDEX`/`RINDEX`: `MATCH` returns the 0-based position of a substring,
  `MINDEX` that of the first character found in a character list; both return
  `-1` when nothing matches (#64).

- The `<<` operator is accepted in function-argument position, including the
  prefix form with no left operand, which is the implicit conversion to STRING
  (manual 4.1.4.1). Real programs write `MATCH(<machine data>, <<NAME)` in an
  IF condition; that line used to be a parse error (#64).

- Programmable frame rotations `ROT` and `AROT`, with `X.. Y.. Z..` angles or
  the planar form `RPL=`, on top of the existing `TRANS`/`ATRANS` offsets
  (#68). The geometry axes X, Y, Z are output as machine coordinates under a
  rotation matrix plus translation (machine = R * local + t); every other axis
  keeps its plain offset, so programmed A/B/C orientations are never rotated.
  The semantics follow the Sinumerik frame chapters (NC programming manual
  3.12.2.1 and 3.12.2.4, Basic Functions manual 4.5.2.3 and 4.5.2.4): `ROT`
  and `TRANS` are both absolute and each deletes the whole programmable frame,
  so a `TRANS` after `AROT` drops the rotation and a `ROT` after `TRANS` drops
  the offset; `AROT` chains onto the current frame; several angles in one
  block are RPY angles executed Z, then Y', then X'' (MD10600
  FRAME_ANGLE_INPUT_MODE = 1, the default; the Euler ZX'Z'' mode is out of
  scope); `RPL` turns about the normal of the active G17/G18/G19 plane;
  `ATRANS` after a rotation offsets in the rotated system. `IC()` increments
  stay in local coordinates. G2/G3 arcs (I/J/K and CR forms, TURN) are
  supported while the rotation keeps the arc plane equal to the machine plane
  (RPL, or a rotation about the plane normal): the centre offsets are rotated
  with the frame and the flattener works unchanged. An arc under a rotation
  that tilts the plane, `RPL=` mixed with axis angles, angles on non-geometry
  axes, and the still unmodeled `ROTS`/`AROTS`/`CROTS`, `SCALE`/`ASCALE` and
  `MIRROR`/`AMIRROR` all raise with line context. The final state dict gains a
  `rotation` entry (row-major 3x3 list); `translation` keeps its meaning. The
  fast path and the streaming/batch APIs mirror the whole-file interpreter.

### Changed

- Jump-free programs of 64 MiB or more on the fast path no longer keep every
  decoded line in memory. A parallel preflight scan validates the whole
  program first (a parse error anywhere, also after `M30`, is still reported
  before any row is emitted), then execution decodes and parses one line at a
  time, so parse storage is bounded by the longest line instead of the program
  length. Peak RSS of `nc_to_batches` on a 1.28M-line (112 MB) field program
  drops from about 1.3 GiB to 0.4 GiB, and on a 22M-line (1.1 GB) one from
  5.4 GiB to 2.4 GiB; the second decode costs up to about 20 percent of
  interpretation time. Below 64 MiB the indexed path stays: its retained state
  is at most about 0.7 GiB there, and the second decode would cost the most
  relative time. `NC_STREAM_MIN_BYTES` sets the threshold in bytes (0 streams
  every jump-free program). Programs with a jump keep the indexed path at any
  size (#73).

- The fast path builds its jump-target index (one `N:` key per block number,
  one `LABEL:` key per label) only when a grammar-bound line mentions GOTO.
  Trivial lines can never carry a jump, so a program without one skips the
  allocation entirely; a GOTO inside a comment merely builds an unused index.
  About 40 % less time on a 400k-line straight-line program (#73).

- A frame instruction no longer moves the interpreted position. `TRANS`,
  `ATRANS`, `ROT` and `AROT` hold the machine position of every positioned
  axis and re-derive its local (workpiece) coordinate under the new frame, as
  a control does when a frame becomes active (Basic Functions manual 4.5.11:
  `$AA_IM` is the machine setpoint, `$AA_IW` the workpiece one). So a block
  after the frame change that programs only some axes moves the others only
  through the rotation, `$AA_IM` agrees with the output rows, `$AA_IW` reports
  the re-derived value, and an `IC()` increment after a `TRANS` moves from the
  current position (NC programming manual 3.1.1.5) instead of jumping by the
  offset; `examples/trans2.csv` changes accordingly. A geometry axis that a
  rotation couples to a positioned axis before any block positioned it is
  taken at machine 0 (with a warning) from then on, so it can be read with
  `$AA_IM` and appears in the rows.

- `scripts/compare_benchmarks.py` refuses to compare reports recorded on
  different inputs or, when both were recorded with `--verify-digest`, with
  differing row counts or output/state digests in any matched case (#76).

- A control-structure keyword must stand alone in its block (#84). The
  grammar accepted statements on the same line and ran them as part of the
  body: `WHILE R1<3 X1` (likewise after `LOOP`, `REPEAT`, `FOR ... TO ...` and
  `ELSE`) ran `X1` as the first body block of every pass, and a mid-line
  `X1 ELSE X2` ended the true branch. The NC programming manual (4.1.7) shows
  every structure keyword in a block of its own and does not say a control
  accepts more, so these forms are now parse errors that name the text to
  move. A comment after the keyword and a block number before it are still
  allowed. A closer after other content (`X1 ENDIF`) is reported at the closer
  instead of as "IF is never closed". A jump label on a structure line
  (`LB: WHILE R1<3`, `LB: ENDIF`) is rejected too, as a real control does with
  alarm 12630; a conditional jump (`LB: IF R1<3 GOTOB LB`) is no structure and
  keeps its label.

### Fixed

- Flattened `BSPLINE` samples now carry the line number of the control-point
  block they belong to, instead of all carrying the last control point's line.
  `ASPLINE`/`CSPLINE` and G2/G3 already attributed their samples per segment,
  so a simulator that highlights the executing line from `line_no` stuck on
  one line for a B-spline and walked through the control-point lines for
  every other curve. The curve is now split at the Greville abscissae of the
  clamped knot vector: a sample belongs to the first control point whose
  Greville abscissa is at or beyond the sample's parameter, the last control
  point taking the remainder. Between its Greville abscissae a control point
  is the one with the most influence on the curve, and for degree 1 the rule
  reduces to the polyline segment ending at each programmed point. The
  sampled geometry is unchanged (same coordinates, order and count), only the
  `line_no` distribution changes: the auxiliary cells of a control-point
  block (feed, M codes, comments) now ride on its first sample, and a block
  that owns samples no longer emits an aux-only placeholder row. Only the
  curve end stays a programmed position; every other sample keeps
  `flattened = 1`. The checked-in `examples/flattening/flatten_demo_flattened.csv`
  is regenerated accordingly (#62).

- Two statements glued onto one line with no separator (`... = SET(...)DEF INT
  ...`, a copy-paste defect seen in a real program) are still rejected, but
  the error now names the trailing text and points at the column where the
  second statement starts, instead of "expected the end of the program". The
  INDEX-based single-character write from the same report has worked since
  v0.2.6 and is now pinned by a test (#58).

- Reading an array element or indexed system variable that was never assigned
  reported "Invalid array index ... used as an array index"; the message now
  says the element is undefined and how to supply it. It still names the
  variable and the line, so an unknown string-typed machine data in a string
  argument is a loud error rather than a silent empty string (#64).

- A quoted string passed to a numeric function (`SIN("abc")`) was silently
  dropped from the argument list and surfaced as an arity error; it is now a
  type error naming the string.

- Nested function calls parse in linear time: each function argument, and
  each assignment right-hand side, is parsed once instead of once per grammar
  alternative, so a deeply nested `SIN(SIN(...))` is instant (#78).

- The line fast path no longer declines whole programs because a few hundred
  grammar-bound lines (procedure calls such as `SUPPORT_PIN_TRIGGER(2.5)`,
  `MATERIAL_UPDATE`, `R1=R1+1`, `STOPRE`) are spread through a large file
  (#69). Grammar-bound lines used to be parsed one at a time, each padded with
  one newline per preceding line so that pest reported whole-file positions;
  the padding cost O(line index) bytes per line, and a budget guard sent any
  program over it to the whole-file parse. The grammar-bound lines are now
  parsed in one pest run over a sparse copy of the program (only those lines
  keep their text, every other line is empty), which gives the same whole-file
  positions at O(input) bytes. The budget guard, the padded buffers and the
  grammar-line ratio guard are gone: a program that is entirely grammar-bound
  now costs the same as the whole-file parse (measured equal at 100k and 400k
  lines), so every structure-free program takes the line path and the
  whole-file parse is only used for programs with IF/WHILE/FOR/LOOP/REPEAT
  blocks. On a 112 MB, 1.28M-line CAM
  program with about 12,000 such lines the Python batch API went from 24.3 s
  and 3.0 GiB peak RSS (whole-file parse) to 2.2 s and 1.3 GiB.

- The line fast path no longer gives three line shapes a different meaning
  than the grammar does (#83). `N10X5` (a block number glued to the next word)
  was read as block 10 plus `X5`, where the grammar reads one call named
  `N10X5` and warns about the glued word; `TRANS=5` (a frame keyword before
  `=`) was stored as a variable, where the grammar rejects the line; and
  `X=5 AND Y1` (likewise `OR`, `XOR`, `B_AND`, `B_OR`, `B_XOR` after a
  constant) became `X=5`, a call `AND` and `Y1`, where the grammar evaluates
  `5 AND Y1`. The decoder now hands these lines to the grammar, so the result
  no longer depends on which path a program takes. A seeded random-line test
  compares every line the decoder claims against the grammar.

## [v0.2.7] - 2026-09-08

### Changed

- Local and editable builds now derive their version from `git describe`
  instead of reporting the `0.0.0.dev0` placeholder: an editable checkout three
  commits past `v0.2.6` reports `0.2.6.dev3+<sha>`. A small in-tree PEP 517
  backend (`build_backend/nc_build_backend.py`) stamps the version onto the
  `Cargo.toml` placeholder and hands off to maturin; the release CI, which
  already writes the tag version into `Cargo.toml`, is untouched. Downstream
  consumers installing this package editable now get a real version to pin
  against and a signal that they are running a local edit (#65).
- `nc-gcode-interpreter --version` now reports the actual build version. It was
  a hard-coded `1.0` string literal that had never matched any release; a
  `build.rs` now derives the same `git describe` version for plain `cargo`
  builds (which don't go through the PEP 517 shim) and the CLI reads it, so a
  dev build prints e.g. `0.2.6-dev.1+ge5f2306` and a release build prints the
  tag.
- Ruff's implicit default rule set widened in 0.16 and turned the lint job red
  on 64 findings in untouched code. Pin the rule selection in `pyproject.toml`
  and the ruff version in CI, so a linter upgrade adds rules only when asked,
  and fix the findings. Mostly mechanical (`X | None`, `list`/`dict` builtins,
  import sorting, explicit `zip(strict=)`). The rule set carries no `ignore`
  list: the one rule that would have needed an exception, `FLY002`, is instead
  satisfied by an `nc_lines(*blocks)` test helper, which keeps the NC test
  programs one block per line with their explanatory comments intact.

## [v0.2.6] - 2026-07-09

### Added

- String operations (manual 4.1.4), so real CAM programs that build a
  timestamped protocol-file name now parse and run end to end:
  - Single-character writes into a STRING variable, `STRING[<index>] = "<char>"`
    (manual 4.1.4.8), 0-based; only user-defined variables, never system
    variables. The right-hand side must be exactly one character and the index
    in range, both enforced loudly.
  - The `<<` concatenation operator, joining quoted strings, STRING variables,
    string functions and numbers (INT in plain form, REAL with up to 10 decimals
    and trailing zeros trimmed, per manual 4.1.4.1).
  - String functions: `SPRINT` (printf-style: `%d %f %s %x %b %c` with field
    width and precision; unsupported conversions error loudly), `SUBSTR`,
    `INDEX`, `RINDEX`, `NUMBER`, `STRLEN`, `ISNUMBER`. All string indices are
    0-based; the search family returns `-1` when not found; `NUMBER` on a
    non-numeric string is a hard error.

### Fixed

- A quoted string that is only whitespace (e.g. `" "`) no longer loses its
  content: the implicit `WHITESPACE` rule used to eat it, so `INDEX(x, " ")`
  found nothing — exactly the space that date-formatting code searches for.
  Quoted-string bodies are now taken verbatim (leading/trailing spaces too).
- Declaring a variable whose name collides with a reserved axis letter *with*
  an initializer (e.g. `DEF STRING[13] S = "..."`) now reports the name
  collision ("conflicts with an axis name") instead of a confusing downstream
  "cannot assign a string" error.

## [v0.2.5] - 2026-07-08

### Changed

- Interpreter throughput on the DataFrame/batch path: the per-output-row `Row`
  allocations are now recycled instead of freed. Profiling the 1.1 GB → DataFrame
  conversion showed ~50% of CPU in the system allocator, dominated by allocating
  and freeing each of the 22M rows' cell buffers. After a batch is built its rows
  are cleared (capacity retained) into a pool and handed back to the interpreter
  to refill, bounding live row allocations to ~2× the batch size instead of the
  whole-file row count. Output is byte-identical; the streaming (`nc_to_rows`)
  and in-memory collect paths are unchanged. ~10% off end-to-end on the large
  real-world program (#61).

## [v0.2.4] - 2026-07-08

### Changed

- Interpreter throughput: the hot, closed-vocabulary lookup maps (`axes`,
  `translation`, `output_keys`, and the table builder's per-cell/forward-fill
  maps) now use the non-cryptographic FxHash hasher, and `State::update_axis`
  overwrites in place instead of re-allocating the key on repeat writes. ~17%
  off the collect (dataframe) path on the large real-world benchmark, with no
  behavior change. `symbol_table` deliberately stays on the default SipHash
  hasher: its keys are user-controlled variable names, so it keeps its
  hash-flooding resistance (#60).

### Added

- Experimental execution-cursor interpreter (opt-in via `NC_VM=1`): the
  recursive control-flow walk is reified into an explicit frame stack, enabling
  in-memory checkpoint/resume. Off by default; the standard path is unchanged.
  Groundwork for resumable/streaming interpretation (#47, #59).

## [v0.2.3] - 2026-07-07

### Added

- `NcError.kind`: a stable, machine-readable string discriminating the error
  class (e.g. `"unexpected_axis"`, `"undefined_variable"`, `"unknown_g_command"`,
  `"parse_context"`), so a consumer can branch on the kind of error without
  string-matching the formatted message. Present on every `NcError` alongside
  the existing `line` / `column` / `context` / `line_text` location attributes
  (#56).

### Fixed

- Validation/semantic errors now carry their source location. `Unexpected axis`,
  and the "axis/reserved name used as a variable" definition errors, previously
  raised an `NcError` with `line` / `column` / `context` / `line_text` all
  `None`; they now anchor to the offending line (and expose its text), so an
  editor can mark the exact spot - matching what syntactic parse errors already
  did (#56).

## [v0.2.2] - 2026-07-07

### Added

- Variable-change events on the batch path: `nc_to_batches(...,
  include_variables=True)` exposes, once exhausted, a sparse `variable_events`
  DataFrame (`row_idx` / `name_id` / `value`) plus a `variable_names` list -
  the batch-path twin of the per-row `variables` dict `nc_to_rows` already
  yields. `row_idx` is the output-row index a change is seen at (a change on a
  variable-only block is attributed to the next output row), so replaying the
  events reconstructs the symbol table at any row. Off by default (no cost).
- `string_table` in the interpreter state dict: `DEF STRING` variables now
  round-trip into `.state` / the `nc_to_dataframe` state tuple as
  `state["string_table"]` (`dict[str, str]`), alongside the existing `axes` /
  `symbol_table` / `translation` numeric tables (previously omitted).

## [v0.2.1] - 2026-07-07

### Added

- Optional `line_no` output column on the batch/dataframe path, enabled with
  `include_line_numbers=True` on `nc_to_dataframe` / `nc_to_batches` (default
  `False`, so the output schema is unchanged unless you ask for it). When
  enabled it prepends a leading `Int64` column giving the 1-based source line
  each output row came from - previously only the streaming `nc_to_rows`
  exposed it. Loops repeat the value and jumps make it non-monotonic, matching
  `nc_to_rows` row-for-row; flatten-generated samples keep the originating
  block's line number; `dataframe_to_nc` ignores it (source provenance, not an
  emittable word). Concatenating batches reconstructs the same per-row
  `line_no` as the whole-file dataframe (#45)
- Structured error locations: parse/interpret failures now raise
  `nc_gcode_interpreter.NcError` (a `ValueError` subclass, so existing
  `except ValueError` keeps working) carrying the position as data - `.line`,
  `.column` (syntax errors), `.context`, and `.line_text` attributes (each an
  int / str or `None`) - so a caller (e.g. an editor) can locate the offending
  token without regex-parsing the message. `str(err)` is unchanged

### Fixed

- Arithmetic-function arity: added a regression test pinning that every
  `SIN`/`COS`/`ATAN2`/`BOUND`/... call already validates its argument count
  (`check_args`) before indexing `args[..]`, returning
  `ParsingError::InvalidFunctionArity` instead of panicking on a wrong-arity
  call (#16).

### Documented

- Pinned iterator-drop -> `ParsingError::StreamClosed` as the supported
  cancel contract for `nc_to_rows`/`nc_to_batches` (README, docstrings) and
  noted the release-profile `strip`/`lto` tradeoff in `Development.md` (#49).

## [v0.2.0] - 2026-07-07

### Added

- `dwell` output column: F/S on a `G4` block is the dwell time (seconds /
  spindle revolutions), a per-block parameter - it now lands in its own
  never-forward-filled `dwell` column instead of polluting the modal F/S
  columns (previously `G4 F0.01` set the feed to 0.01 mm/min for every
  following block until the next real F word)
- Loud warnings for known-but-uninterpreted constructs (never silently
  butcher a statement): assignments to `AR`/`AP`/`RP` (opening-angle and
  polar arc forms) warn once per run that the motion will be wrong; `G91`
  warns once that incremental dimensioning is not applied; the flattener
  warns per word for CIP/CT/POLY/thread/involute pass-through
- `TURN` output column (block address, never forward-filled): additional
  full helix turns on G2/G3 blocks; previously swallowed as a user variable
- Curve flattening: `flatten_tolerance` on `nc_to_dataframe` / `nc_to_rows` /
  `nc_to_batches` (CLI: `--flatten-tolerance`) converts G2/G3 arcs (I/J/K and
  CR= forms, all planes, helical, full circles) and ASPLINE/BSPLINE/CSPLINE
  splines (PW weights, SD degree) into runs of G1 rows within a single
  max-deviation tolerance of the true curve; interpolation addresses are
  consumed, source line numbers and auxiliary cells preserved; generated
  samples carry a `flattened = 1` marker column so the original programmed
  points remain distinguishable
- Optional `viz` extra (threejs-viewer >= 0.0.41):
  `nc_gcode_interpreter.viz.view_toolpath(df)` shows a toolpath in
  threejs-viewer as an animated bead tube (feed-rate float64 time base,
  programmed vs flattened point coloring), and the `nc-view` console command
  interprets + flattens + animates an .mpf in one step, with a nozzle
  marker riding the path tip, camera follow/look-at tracking, and travel
  moves drawn natively as a thin line in lockstep with the bead

- Program jumps and branches: `GOTOF`/`GOTOB`/`GOTO`/`GOTOC`/`GOTOS` and
  `CASE ... OF ... DEFAULT`, with per-scope label/block-number resolution,
  jumps out of IF bodies and loops, and alarm-14080 semantics for an
  unresolved target (`GOTOC` warns and continues) (#29)
- Streaming API `nc_to_rows(program)`: yields `(line_no, row)` lazily while
  the interpreter runs on a background thread — batch-identical typing and
  forward-fill, constant memory, early abort by dropping the iterator,
  errors raised at the offending row, final state on the exhausted
  iterator (#35)
- `nc_to_rows(..., include_variables=True)` yields `(line_no, row,
  variables)` with per-block variable-assignment deltas, exposing
  variable-only blocks that the batch DataFrame prunes (#35)
- Spline programming: `PW`/`SD`/`PL` block addresses become output columns
  (not forward-filled, no `TRANS` offset) instead of being silently
  swallowed (#18)
- G2/G3 arc interpolation parameters `I`/`J`/`K`/`CR` become per-block output
  columns (not forward-filled, no `TRANS` offset) instead of being silently
  dropped — arcs previously came out as straight-line endpoints (#37)
- `nc_to_batches(program, batch_size=...)`: interpret a program into a
  stream of columnar polars DataFrames built on a worker thread and handed
  over via the Arrow C data interface — bounded memory for programs too
  large to fit in one DataFrame (#37)
- Parsing: leading-underscore identifiers (`_WITH_M0`), assignment to
  `$AC_*` system variables (`$AC_TIMER[1] = 0`), and the `NOT` logical
  operator (#37)
- `DEF STRING[n]` string-variable declarations and quoted-string
  assignments; strings stay out of the numeric pipeline (a string in an
  expression, or a type mismatch, is a hard error, never a silent 0.0).
  String *processing* (`SPRINT`/`INDEX`/`<<`) remains out of scope and
  fails loudly (#40)
- Logic, comparison and bit operators in expressions (`AND`/`OR`/`XOR`,
  `B_AND`/`B_OR`/`B_XOR`, `==`/`<>`/`<`/`>`/`<=`/`>=`) at the manual's
  priorities, so conditions like `IF (A == 1 AND B == 1)` work and
  comparison results are assignable (`R11 = R10 >= 100`) (#41)
- `$AA_IW[<axis>]` / `$AA_IM[<axis>]` read the interpreted actual work /
  machine position of an axis, so layer loops
  (`REPEAT ... UNTIL $AA_IW[Z] > H`) terminate; reading before the axis is
  positioned errors loudly and the variables are read-only (#42)
- `nc-view` prints a corrected retry command on the classic new-machine
  failures (missing `--axis-index-map`, undefined machine-parameter
  variables) instead of a bare traceback
- `docs/sinumerik-execution-model.md`: how a real control executes NC code
  versus this interpreter, and why (#30)

### Changed

- Rust-side polars is gone. The Table -> Python DataFrame handoff no longer
  builds a polars DataFrame in Rust (via `pyo3-polars`); it builds an Arrow
  record batch with the minimal `arrow-array`/`arrow-schema`/`arrow-data`
  crates and hands it to Python zero-copy through the Arrow PyCapsule
  interface (`__arrow_c_array__`), where `pl.DataFrame(...)` wraps it. The
  Python API is unchanged (still returns `polars.DataFrame`), needs no
  `pyarrow`, and performance is unchanged. This drops ~60 crates from the
  `python`-feature build (127 -> 64), cutting a clean release build ~4x
  (83s -> 21s), and bumps PyO3 0.28 -> 0.29 (resolving the RUSTSEC pyo3
  advisories).
- **Breaking:** `I`/`J`/`K`/`CR` are now treated as arc interpolation-parameter
  block addresses (output columns), so they can no longer be used as user
  variable names: `I=5` followed by `X=I+1` was a variable read before and is
  now an undefined-variable error. Matches Sinumerik address semantics (#37)
- `nc_to_dataframe` is now the concatenation of the internal batch stream
  rather than collecting every row up front: same output, but bounded
  intermediate memory and interpretation overlapped with DataFrame assembly
  (a 1.1 GB program went from ~209 s / 6 GB to ~33 s / 3.3 GB) (#37)
- An executed `M2`/`M17`/`M30` now ends the program immediately, even
  mid-file, instead of being ignored (#29)
- Unknown G codes now error like Sinumerik alarm 12470 instead of silently
  parsing as a subprogram call (#31)
- Unsupported or unmodeled statements (parameterized `ROT`/`SCALE`/`MIRROR`
  frames, and previously jumps/`LOOP` before #29 implemented them) raise a
  clear `UnsupportedStatement` error instead of corrupting output; a frame
  instruction following another statement in the same block errors (#20,
  #32)
- Frame semantics per the manual: absolute frame instructions (`TRANS`,
  bare `ROT`/`SCALE`/`MIRROR`) substitute — deleting previously programmed
  offsets — rather than accumulate (#20, #26)
- Values are f64 end-to-end to match the control's 64-bit `REAL`:
  coordinates are bit-exact for decimal literals, `DIV` truncates the real
  result (`7 DIV 4.1 = 1`), and `==`/`<>`/`<=`/`>=` use the manual's 1e-12
  relative tolerance (#23)
- The ~488-command G vocabulary and its 60 G-groups moved out of the PEG
  grammar into a Rust table generated from `ggroups.json`; the grammar
  recognizes lexical shape only (#32)
- Rust-side polars removed: the core returns a plain typed table and the
  Python wrapper builds the DataFrame, so any Python polars version works;
  CSV and DataFrame output are unchanged (#27)
- Dependency refresh: pest 2.8.6, pyo3 0.28, clap 4.6, polars 1.42.1 (#28)

### Fixed

- The NC language is case-insensitive (manual 3.3.2): lowercase axis
  words (`g1 x0 y0`) are axis moves rather than silently-dropped subprogram
  calls, G/M values are normalized to uppercase, and user-variable
  identifiers fold case (a program that declares `lAYER_HEIGHT` but assigns
  `LAYER_HEIGHT` is one variable, not two) (#43)
- The IC-before-position warning now states what actually happens ("axis
  incremented with `IC()` before any absolute position was set; assuming it
  starts at 0") instead of the vague "behavior may be indeterminate"
- Expression evaluation: correct operator precedence (`2+3*4` is now 14,
  not 20), degree-based trig per the manual, corrected `ATAN2` argument
  order, and `DIV` by zero errors instead of panicking (#19)
- `TRANS`/`ATRANS` translation is applied per row at output time under the
  frame active at that block; fixes double-applied offsets on `IC()` moves
  and axis movement as a parsing side effect (#26)
- Unclosed or crossed `IF`/`WHILE`/`FOR`/`LOOP`/`REPEAT` structures are
  reported at the (innermost) opener's own line instead of as a parse
  failure at end-of-file (#33)
- Parse errors are phrased for humans (no grammar-internal rule names),
  mistyped axis words like `Y2O` warn, and unresolved jump targets get
  did-you-mean suggestions — direction-aware when the label exists but only
  in the other search direction (#31)

### Performance

- Stage-1 line triage: structure-free CAM programs run through a byte
  decoder for the >99.9% trivial lines, per-line parses for the rest —
  a 319k-line program drops from ~33 s to ~9 s in `nc_to_dataframe`
  (`NC_STAGE1=0` disables) (#34)
- Grammar restructuring: ~15% faster parsing, plus an ignored 1M-line
  benchmark harness (#30); an earlier redundant-lookahead removal cut parse
  time ~27% (#25)

## [v0.1.12] - 2025-06-26

- Added `allow_undefined_variables`: undefined variables initialize to 0.0
  with a warning instead of erroring (#17)
- Dependency updates

## [v0.1.11] - 2025-06-25

- Added arithmetic functions and `REPEAT ... UNTIL` (#15)
- Added `axis_index_map` to map axis identifiers to array indices (e.g.
  `FL[E]=10`)
- Improved error messages and grammar edge cases (comments in `IF`
  statements, relational-operator ordering)

## [v0.1.10] - 2025-02-21

- Updated supported Python versions and release versioning; no interpreter
  changes

## [v0.1.9] - 2024-10-25

- Added `dataframe_to_nc`: write a polars DataFrame back out as NC G-code
  (round-trip), with the `sanitize_dataframe` helper exposed (#13)

## [v0.1.8] - 2024-10-22

- Exposed G-code groups (ggroups) in the API (#12); added type hints (#11)

## [v0.1.7] - 2024-10-17

- Git-tag-based versioning; CI cleanup (minimum Python 3.11)

## [v0.1.6] - 2024-10-16

- Implemented tool selection (`T="..."`/`T5`); M and T matching is
  case-insensitive; quoted strings are unquoted in output (#8)

## [v0.1.5] - 2024-10-09

- Reordered G-group parsing, roughly doubling parse performance (#7)

## [v0.1.1] – [v0.1.4] - 2024-10-03

- Packaging and CI: PyPI publishing via Trusted Publisher, contributing
  guidelines, macOS builds, WASM and binary bindings disabled to fix the
  Python package (#1–#3)

## [v0.1] - 2024-09-26

- Initial release: Rust (pest) interpreter for Sinumerik-flavored NC
  G-code with a CLI (MPF → CSV) and Python bindings (MPF → polars
  DataFrame + final state)
- G-code groups and modal commands, `TRANS`/`ATRANS`, `WHILE`/`FOR`/
  `IF`/`ELSE`, local variables and arrays, arithmetic, incremental moves
  (`IC()`), custom/extra axes, initial-state file, iteration limit,
  forward-fill toggle
