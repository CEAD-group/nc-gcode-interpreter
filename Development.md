build release then test:
    
    rm **/*.csv && cargo build --release && find examples -name "*.mpf" -type f -print0 | xargs -0 -I {} sh -c './target/release/nc-gcode-interpreter --defaults=examples/defaults.mpf "$1" || echo "Failed to process $1" >&2' sh {}