# Changelog

Notable changes to **nc-gcode-interpreter**. The format loosely follows
[Keep a Changelog](https://keepachangelog.com/); versions are git tags,
released to PyPI.

## [v0.2.0] - unreleased

### Added

- `dwell` output column: F/S on a `G4` block is the dwell time (seconds /
  spindle revolutions), a per-block parameter - it now lands in its own
  never-forward-filled `dwell` column instead of polluting the modal F/S
  columns (previously `G4 F0.01` set the feed to 0.01 mm/min for every
  following block until the next real F word)
- Curve flattening: `flatten_tolerance` on `nc_to_dataframe` / `nc_to_rows` /
  `nc_to_batches` (CLI: `--flatten-tolerance`) converts G2/G3 arcs (I/J/K and
  CR= forms, all planes, helical, full circles) and ASPLINE/BSPLINE/CSPLINE
  splines (PW weights, SD degree) into runs of G1 rows within a single
  max-deviation tolerance of the true curve; interpolation addresses are
  consumed, source line numbers and auxiliary cells preserved; generated
  samples carry a `flattened = 1` marker column so the original programmed
  points remain distinguishable
- Optional `viz` extra: `nc_gcode_interpreter.viz.view_toolpath(df)` shows a
  toolpath in threejs-viewer as an animated bead tube (feed-rate time base,
  programmed vs flattened point coloring), and the `nc-view` console command
  interprets + flattens + animates an .mpf in one step, with a nozzle
  marker riding the path tip and camera follow/look-at tracking

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
- `docs/sinumerik-execution-model.md`: how a real control executes NC code
  versus this interpreter, and why (#30)

### Changed

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
