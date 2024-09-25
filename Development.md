# Development


Use Cargo to build and run the CLi tool:

```
cargo run -- --help
```


To compile the python module:

```bash
maturin develop
```

## Seup python environment

```bash
uv venv
uv pip install pip polars pytest
```



## Release

```bash
cargo build --release
maturing develop --release
```


## Super simple test

There are a bunc of csv files in the examples directory.  To test the tool on all of them (use git to check changes)

```bash
rm **/*.csv && cargo build --release && find examples -name "*.mpf" -type f -print0 | xargs -0 -I {} sh -c './target/release/nc-gcode-interpreter --initial_state=examples/defaults.mpf "$1" || echo "Failed to process $1" >&2' sh {}
```

## python test
    
```bash
maturin develop --release && pytest
```