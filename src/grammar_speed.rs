//! Parse-speed guard for grammar additions (run explicitly in release mode).

#[cfg(test)]
mod tests {
    use crate::types::{NCParser, Rule};
    use pest::Parser;
    use std::time::Instant;

    fn best_of(samples: usize, mut run: impl FnMut()) -> f64 {
        (0..samples)
            .map(|_| {
                let start = Instant::now();
                run();
                start.elapsed().as_secs_f64()
            })
            .fold(f64::MAX, f64::min)
    }

    /// cargo test --release --lib grammar_speed -- --ignored --nocapture
    #[test]
    #[ignore = "timing benchmark, run explicitly in release mode"]
    fn sparse_parse_50k_sin_lines() {
        let program: String = (0..50_000).map(|_| "X=SIN(30)\n").collect();
        let parse = best_of(21, || {
            let pairs = NCParser::parse(Rule::sparse_lines, &program).expect("parses");
            std::hint::black_box(pairs);
        });
        let interpret = best_of(7, || {
            let result = crate::interpreter::nc_to_table(&program, None, None, None, 100_000, false, None, false, None);
            std::hint::black_box(result.expect("interprets"));
        });
        let sparse_line_blocks = best_of(21, || {
            let pairs = NCParser::parse(Rule::sparse_line_blocks, &program).expect("parses");
            std::hint::black_box(pairs);
        });
        let per_line = best_of(7, || {
            for text in program.lines() {
                std::hint::black_box(NCParser::parse(Rule::line, text).expect("parses"));
            }
        });
        let state = crate::state::State::new(vec!["X".to_string()], 1000, None, false);
        let per_line_lowered = best_of(7, || {
            for (index, text) in program.lines().enumerate() {
                std::hint::black_box(crate::lower::parse_line(text, index + 1, &state).expect("lowers"));
            }
        });
        println!("sparse_lines parse, 50k X=SIN(30): best {:.2} ms", parse * 1e3);
        println!("nc_to_table, 50k X=SIN(30): best {:.2} ms", interpret * 1e3);
        println!("sparse_line_blocks parse: best {:.2} ms", sparse_line_blocks * 1e3);
        println!("line parse, one call per line: best {:.2} ms", per_line * 1e3);
        println!(
            "line parse and lower, one call per line: best {:.2} ms",
            per_line_lowered * 1e3
        );
    }
}
