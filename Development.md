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


## Super simple test

There are a bunch of csv files in the examples directory. To test the tool on all of them (use git to check changes)

```bash
rm **/*.csv && cargo build --release && find examples -name "*.mpf" -type f -print0 | xargs -0 -I {} sh -c './target/release/nc-gcode-interpreter --axis-index-map E:4 --initial_state=examples/defaults.mpf "$1" || echo "Failed to process $1" >&2' sh {}
```

## python test
    
```bash
maturin develop --release --uv && pytest
```