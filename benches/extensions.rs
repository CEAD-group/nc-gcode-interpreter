//! Cost of supplied subprograms and an external-variable provider (#48),
//! without the Python boundary: `cargo bench --bench extensions [samples]`.
use nc_gcode_interpreter::interpreter::{nc_to_batch_stream_with, nc_to_table_with};
use nc_gcode_interpreter::start::StartInputs;
use nc_gcode_interpreter::subprogram::{Extensions, ExternalError, ExternalIndex, ExternalVariables};
use std::{collections::HashMap, hint::black_box, time::Instant};

const BODY_LINES: usize = 20;
const CALLS: usize = 10_000;
const READS: usize = 100_000;

/// Answers every read with 1 and counts the accesses.
struct Constant(usize);

impl ExternalVariables for Constant {
    fn read(&mut self, _: &str, _: &[ExternalIndex]) -> Result<f64, ExternalError> {
        self.0 += 1;
        Ok(1.0)
    }
    fn write(&mut self, _: &str, _: &[ExternalIndex], _: f64) -> Result<(), ExternalError> {
        Ok(())
    }
}

fn body() -> String {
    let mut text = String::from("PROC BODY(REAL VAL)\n");
    for k in 0..BODY_LINES - 2 {
        text += &format!("G1 X=VAL+{k} Y={k}.5 F1200\n");
    }
    text + "M17\n"
}

fn calls() -> String {
    let mut text = String::from("G1 X0 Y0 F1200\n");
    for i in 0..CALLS {
        text += &format!("BODY({i})\n");
    }
    text + "M30\n"
}

/// The rows of `calls()` with the body inlined: the interpreter-only reference.
fn inlined() -> String {
    let mut text = String::from("G1 X0 Y0 F1200\n");
    for i in 0..CALLS {
        text += &format!("R10={i}\n");
        for k in 0..BODY_LINES - 2 {
            text += &format!("G1 X=R10+{k} Y={k}.5 F1200\n");
        }
    }
    text + "M30\n"
}

fn read_loop(source: &str) -> String {
    format!("R1=0\nR2=0\nWHILE R1<{READS}\nR2=R2+{source}\nR1=R1+1\nENDWHILE\nG1 F1200 X=R2\nM30\n")
}

fn read_rows(source: &str) -> String {
    (0..READS)
        .map(|i| format!("G1 F1200 X={source}+{}\n", i % 100))
        .collect::<String>()
        + "M30\n"
}

fn provider() -> Option<(Vec<String>, Box<dyn ExternalVariables>)> {
    Some((vec!["$A_IN".to_string()], Box::new(Constant(0))))
}

fn table(program: &str, initial: Option<&StartInputs>, extensions: Extensions) -> usize {
    let (table, _) = nc_to_table_with(
        program, initial, None, None, 10_000_000, false, None, false, None, extensions,
    )
    .expect("program runs");
    table.height()
}

/// As the Python bindings run it: batches over a channel, flushed before each provider call.
fn stream(program: &str, initial: Option<&StartInputs>, extensions: Extensions) -> usize {
    let (tx, rx) = std::sync::mpsc::sync_channel(3);
    let (events, _events) = std::sync::mpsc::channel();
    std::thread::scope(|scope| {
        let consumer = scope.spawn(move || rx.iter().map(|t: nc_gcode_interpreter::output::Table| t.height()).sum());
        nc_to_batch_stream_with(
            program,
            initial,
            None,
            None,
            10_000_000,
            false,
            None,
            false,
            None,
            500_000,
            false,
            Box::new(move |t| tx.send(t).map_err(|_| ())),
            false,
            events,
            extensions,
        )
        .expect("program runs");
        consumer.join().unwrap()
    })
}

fn main() {
    let samples: usize = std::env::args().nth(1).and_then(|s| s.parse().ok()).unwrap_or(5);
    let sub = body();
    let preset_inputs = StartInputs {
        definitions: vec!["DEF REAL AIN1=1\n".to_string()],
        ..StartInputs::default()
    };
    let preset = Some(&preset_inputs);
    let calls = calls();
    let inlined = inlined();
    let loop_preset = read_loop("AIN1");
    let loop_ext = read_loop("$A_IN[1]");
    let rows_preset = read_rows("AIN1");
    let rows_ext = read_rows("$A_IN[1]");
    let subs = || Extensions {
        subprograms: HashMap::from([("BODY".to_string(), sub.clone())]),
        ..Default::default()
    };
    let ext = || Extensions {
        external: provider(),
        ..Default::default()
    };
    let cases: Vec<(&str, Box<dyn Fn() -> usize>)> = vec![
        (
            "inline_10k_x20",
            Box::new(|| table(&inlined, None, Extensions::default())),
        ),
        ("calls_10k_x20", Box::new(|| table(&calls, None, subs()))),
        (
            "readloop_preset_100k",
            Box::new(|| table(&loop_preset, preset, Extensions::default())),
        ),
        ("readloop_provider_100k", Box::new(|| table(&loop_ext, None, ext()))),
        (
            "readrows_preset_100k",
            Box::new(|| stream(&rows_preset, preset, Extensions::default())),
        ),
        ("readrows_provider_100k", Box::new(|| stream(&rows_ext, None, ext()))),
    ];
    println!("case,median_ms,min_ms,max_ms,rows");
    for (name, case) in cases {
        let mut times: Vec<f64> = Vec::with_capacity(samples);
        let mut rows = 0;
        for _ in 0..samples {
            let start = Instant::now();
            rows = black_box(case());
            times.push(start.elapsed().as_secs_f64() * 1000.);
        }
        times.sort_by(f64::total_cmp);
        println!(
            "{name},{:.1},{:.1},{:.1},{rows}",
            times[times.len() / 2],
            times[0],
            times[times.len() - 1]
        );
    }
}
