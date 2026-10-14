//! Dependency-free benchmark executable; scripts/benchmark.py drives the matrix.
mod fixtures;
use nc_gcode_interpreter::{
    experiments, interpreter,
    output::{OutputRows, Table},
};
use std::{
    hash::{Hash, Hasher},
    hint::black_box,
    sync::mpsc,
    time::Instant,
};

fn signature(value: &str) -> u64 {
    let mut hash = std::collections::hash_map::DefaultHasher::new();
    value.hash(&mut hash);
    hash.finish()
}

fn main() {
    let args: Vec<_> = std::env::args().collect();
    assert!(
        args.len() == 9,
        "mode shape lines samples block_size edit batch_size cancel"
    );
    let mode = args[1].as_str();
    let shape = args[2].as_str();
    let lines: usize = args[3].parse().unwrap();
    let samples: usize = args[4].parse().unwrap();
    let block_size: usize = args[5].parse().unwrap();
    let edit = args[6].as_str();
    let batch_size: usize = args[7].parse().unwrap();
    let cancel = args[8] == "1";
    let verify = std::env::var("NC_BENCH_VERIFY_DIGEST").is_ok_and(|v| v == "1");
    assert!(samples > 0 && batch_size > 0);
    let source = if let Some(path) = shape.strip_prefix("file:") {
        std::fs::read_to_string(path).expect("read input")
    } else {
        fixtures::program(shape, lines)
    };
    // Keep immutable source revisions alive for borrowed-cache experiments.
    let revisions: Vec<_> = (0..if edit == "none" { 0 } else { 2 })
        .map(|i| fixtures::edit(&source, edit, i % 2 == 0))
        .collect();
    let restorations: Vec<_> = revisions
        .iter()
        .map(|(_, range, _)| {
            let offset: usize = source.split_inclusive('\n').take(range.start).map(str::len).sum();
            let len: usize = source
                .split_inclusive('\n')
                .skip(range.start)
                .take(range.len())
                .map(str::len)
                .sum();
            &source[offset..offset + len]
        })
        .collect();
    let start = Instant::now();
    let mut grammar = (mode == "grammar-cache").then(|| experiments::GrammarCache::new(&source).unwrap());
    let mut decoded = (mode == "decoded-cache").then(|| experiments::DecodedCache::new(&source, block_size).unwrap());
    let cold_prepare_ms = start.elapsed().as_secs_f64() * 1000.;
    println!("sample,bytes,prepare_ms,run_ms,first_batch_ms,rows,decoded_lines,output_digest,state_digest");
    // Sample zero is cold load; later samples are warm reruns / actual edits.
    for sample in 0..=samples {
        let start = Instant::now();
        let current = if sample == 0 || edit == "none" {
            &source
        } else {
            &revisions[(sample - 1) % 2].0
        };
        let mut decoded_lines = if sample == 0 {
            decoded.as_ref().map_or(0, |c| c.decoded_lines)
        } else {
            0
        };
        if sample > 0 && edit != "none" {
            if mode == "grammar-cache" {
                grammar = Some(experiments::GrammarCache::new(current).unwrap());
            }
            if mode == "decoded-cache" {
                // Undo the previous edit before applying the next, also counted.
                if sample > 1 {
                    let (_, old_range, old_text) = &revisions[(sample - 2) % 2];
                    let restored = restorations[(sample - 2) % 2];
                    decoded
                        .as_mut()
                        .unwrap()
                        .replace(old_range.start..old_range.start + old_text.lines().count(), restored)
                        .unwrap();
                    decoded_lines += decoded.as_ref().unwrap().decoded_lines;
                }
                let (_, range, replacement) = &revisions[(sample - 1) % 2];
                decoded.as_mut().unwrap().replace(range.clone(), replacement).unwrap();
                decoded_lines += decoded.as_ref().unwrap().decoded_lines;
            }
        }
        let prepare_ms = if sample == 0 {
            cold_prepare_ms
        } else {
            start.elapsed().as_secs_f64() * 1000.
        };
        let (tx, rx) = mpsc::sync_channel::<Table>(2);
        let (events_tx, _events_rx) = mpsc::channel();
        let started = Instant::now();
        let mut state_digest = 0;
        let (rows, first, digest) = std::thread::scope(|scope| {
            let consumer = scope.spawn(move || {
                let mut first = None;
                let mut rows = 0;
                let mut hash = std::collections::hash_map::DefaultHasher::new();
                for mut batch in &rx {
                    first.get_or_insert_with(|| started.elapsed().as_secs_f64() * 1000.);
                    rows += batch.height();
                    if verify {
                        batch.columns.sort_by(|a, b| a.0.cmp(&b.0));
                        format!("{:?}", batch.columns).hash(&mut hash);
                    }
                    black_box(batch);
                    if cancel {
                        break;
                    }
                }
                (rows, first, if verify { hash.finish() } else { 0 })
            });
            let result = (|| -> Result<(), String> {
                if mode == "fresh" {
                    interpreter::nc_to_batch_stream_with_line_numbers(
                        current,
                        None,
                        None,
                        Some(vec!["ELX".into()]),
                        10_000,
                        false,
                        experiments::axis_index_map(),
                        experiments::allow_undefined(),
                        None,
                        batch_size,
                        true,
                        tx,
                        false,
                        events_tx,
                    )
                    .map(|state| {
                        if verify {
                            state_digest = signature(&experiments::observation(&[], &state));
                        }
                    })
                    .map_err(|e| e.to_string())
                } else {
                    let mut state = experiments::fresh_state(current);
                    let mut output =
                        OutputRows::batch_stream_with_line_numbers(tx, batch_size, false, false, events_tx, true);
                    match mode {
                        "grammar-cache" => grammar
                            .as_ref()
                            .unwrap()
                            .execute(&mut output, &mut state)
                            .map_err(|e| e.to_string())?,
                        "decoded-cache" => decoded
                            .as_ref()
                            .unwrap()
                            .execute(&mut output, &mut state)
                            .map_err(|e| e.to_string())?,
                        "bounded" => experiments::bounded_decode(current, &mut output, &mut state)?,
                        "fused" => experiments::fused_decode(current, &mut output, &mut state)?,
                        _ => panic!("unknown mode"),
                    }
                    output
                        .finish()
                        .map(|_| {
                            if verify {
                                state_digest = signature(&experiments::observation(&[], &state));
                            }
                        })
                        .map_err(|e| e.to_string())
                }
            })();
            if cancel {
                if let Err(error) = result {
                    assert_eq!(error, "row stream closed by the consumer");
                }
            } else {
                result.unwrap();
            }
            consumer.join().unwrap()
        });
        println!(
            "{sample},{},{prepare_ms:.6},{:.6},{:.6},{rows},{decoded_lines},{digest:016x},{state_digest:016x}",
            current.len(),
            started.elapsed().as_secs_f64() * 1000.,
            first.unwrap_or(0.)
        );
    }
}
