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
maturing develop --release --uv
```

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