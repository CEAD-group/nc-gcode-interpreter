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