# Development


Use Cargo to build and run the CLi tool:

```
cargo run -- --help
```


To compile the python module:

```bash
maturin develop
```

## Setup python environment

```bash
uv venv -p 3.12
uv synv --all-extras
```



## Release

```bash
cargo build --release
maturin develop --release --uv
```

### Versioning

`Cargo.toml` holds the placeholder version `0.0.0-dev`; the release workflow
rewrites it from the git tag before building the wheels, so the released
version always comes from the tag and never from a committed number.

Local builds get a version too. `build_backend/nc_build_backend.py` is an
in-tree PEP 517 backend that wraps maturin: when it sees the placeholder and
finds a git checkout with tags, it stamps
`git describe --tags --long --dirty` onto the `Cargo.toml` version as
`0.2.6-dev.3+g<sha>` (maturin normalises that to the PEP 440
`0.2.6.dev3+<sha>`), builds, then restores `Cargo.toml` and `Cargo.lock`. So an
editable `uv sync` reports the commit it was built from rather than
`0.0.0.dev0` - which is what lets a downstream consumer notice it is running a
local edit of the interpreter rather than a release.

Any non-placeholder version in `Cargo.toml` is left alone, so the CI release
path is unaffected. Without git (an unpacked sdist, say) the placeholder
stands.

A plain `cargo build` doesn't go through that shim, so `build.rs` derives the
same version independently and exposes it as `NC_GCODE_INTERPRETER_VERSION`,
which is what the CLI's `--version` reports. It applies the same rules: an
already-stamped `Cargo.toml` version wins, and without git the placeholder
stands. The CLI therefore shows cargo's semver spelling
(`0.2.6-dev.1+ge5f2306`), not maturin's PEP 440 normalisation of it.

### Release profile

`[profile.release]` currently sets no `strip`/`lto` overrides. `strip = true` was
tried and dropped (`e1ff63a`): cargo runs the *host* `strip` binary even on a
cross-compiled artifact, so stripping the cross-built macOS x86_64 dylib with
the arm64 host `strip` produced a wheel PyPI rejected as "not a zipfile" -
stripping was reverted to unblock the release. It was a secondary size win
(mostly orthogonal to the polars removal that motivated it) and can be
revisited via maturin's cross-aware `--strip` flag instead of a Cargo
profile setting.

`lto` remains an untried, plausible speed knob (doesn't invoke an external
tool, so it isn't blocked by the cross-compile issue above) - just not yet
evaluated for its extra build time across the wheel matrix.


## Super simple test

There are a bunch of csv files in the examples directory. To test the tool on all of them (use git to check changes)

```bash
rm **/*.csv && cargo build --release && find examples -name "*.mpf" -type f -print0 | xargs -0 -I {} sh -c './target/release/nc-gcode-interpreter --axis-index-map E:4 --initial_state=examples/defaults.mpf "$1" || echo "Failed to process $1" >&2' sh {}
```

## python test
    
```bash
maturin develop --release --uv && pytest
```

## Benchmark harness and parsing experiments

The `bench-experiments` cargo feature compiles research-only strategies
(`src/line_driver/experiments.rs`), a benchmark executable (`benches/`) and
differential tests (`tests/performance_contract.rs`). Normal library and Python
builds do not include any of it. The strategies are a laboratory for #71, #72,
#73 and #74, not a session or incremental API.

```bash
cargo test --features bench-experiments
python3 scripts/benchmark.py --lines 10000 100000 400000 --samples 5 --output tmp/baseline.json
python3 scripts/compare_benchmarks.py tmp/baseline.json tmp/candidate.json
```

`scripts/benchmark.py` (Python 3.11 or newer, macOS or Linux) runs each case in
its own release-build process and records cold and warm samples, time to the
first batch, per-process peak RSS, build settings, the source and binary
SHA-256 and the git revision. Sample zero is cold. `prepare_ms + run_ms` is the
interpreter turnaround. The timings exclude Python, Arrow and viewer work, so
they say nothing about edit-to-visible latency (#72). Do not run other builds
concurrently when comparing. Results are not committed; keep them under `tmp/`.

| Mode | What it measures | Boundary |
| --- | --- | --- |
| `fresh` | The public batch API, 1 or 4 Rayon threads; `--reference` adds the whole-file grammar and the VM | Full interpretation every time |
| `grammar-cache` | Keeping a pest tree for unchanged reruns | Every edit reparses; O(file) AST |
| `decoded-cache` | Immutable decoded blocks, patched by line range | Trivial label-free lines only; output is re-executed |
| `bounded` | Validate eligibility, then decode and execute one line at a time | Two decode passes; trivial label-free lines only |
| `fused` | Decode and execute immediately with a reusable word arena | Same language; a later unsupported line fails after output started |

Edit cases (`--edits coordinate parameter insert delete translation`) prepare
two revised sources before timing; `decoded-cache` undoes the previous patch
and applies the next one inside `prepare_ms`. `--input <file>` benchmarks a
local program with `--edits none`; `--verify-digest` additionally checks that
every strategy produces the same rows and final state (hashing adds time, so
do not compare those timings with plain runs). `--cancel` drops the consumer
after the first batch. The manually dispatched Benchmark workflow runs the
correctness gate and uploads the JSON; it asserts no wall-clock numbers.

Findings so far (Apple M1 Pro, release builds, median of five warm samples,
single thread, 400k-line synthetic straight-line program):

| Strategy | Time | Peak RSS |
| --- | ---: | ---: |
| Public batch API | 672 ms | 560 MiB |
| Cached decoded blocks (64 lines) | 260 ms | 272 MiB |
| Fused decode and execute | 357 ms | 87 MiB |

Parsing reuse removes about a quarter of the warm-edit time; execution and
output dominate the rest, so output chunks or checkpoints (#47, #74) are
needed to go further. Keeping the whole pest tree was slower than the fast
path on ordinary CAM input and cost 3.7 s to rebuild after a TRANS edit. Four
Rayon threads did not consistently beat one. Thin LTO changed the flat cases by
1 to 9 %. Fused decoding handled synthetic 7M and 22M-line programs in 6.3 s
and 21 s at 1.0 and 3.1 GiB; the input copy and the State line index still
scale with the source. The sparse grammar buffer that came out of this work is
now the production fast path (#69).
