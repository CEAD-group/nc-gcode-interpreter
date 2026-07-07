//! End-to-end test of the compiled CLI binary: the `--flatten-tolerance`
//! flag must actually reach the interpreter and the emitted CSV must contain
//! only G1 motions plus the `flattened` marker column. Everything deeper
//! (arc/spline math, tolerances) is covered by the library unit tests; this
//! pins the flag wiring and the CSV writer.

use std::process::Command;

const PROGRAM: &str = "G17 G1 X0 Y0 Z0 F1000\n\
                       G2 X100 Y0 I50 J0\n\
                       G1 X100 Y50\n\
                       BSPLINE X110 Y60 PW=2\n\
                       X120 Y40\n\
                       X130 Y60\n\
                       X140 Y50\n\
                       G1 X150 Y50 M30\n";

fn run_cli(dir: &std::path::Path, extra_args: &[&str]) -> Vec<String> {
    let input = dir.join("program.mpf");
    std::fs::write(&input, PROGRAM).expect("write input");
    let status = Command::new(env!("CARGO_BIN_EXE_nc-gcode-interpreter"))
        .arg(&input)
        .args(extra_args)
        .status()
        .expect("binary should run");
    assert!(status.success(), "CLI exited with {status}");
    // The CLI writes the CSV next to the input file.
    let csv = std::fs::read_to_string(dir.join("program.csv")).expect("CSV output should exist");
    csv.lines().map(str::to_string).collect()
}

fn motion_column(lines: &[String]) -> Vec<String> {
    let header: Vec<&str> = lines[0].split(',').collect();
    let motion = header
        .iter()
        .position(|&name| name == "gg01_motion")
        .expect("gg01_motion column");
    lines[1..]
        .iter()
        .map(|line| line.split(',').nth(motion).unwrap().to_string())
        .collect()
}

#[test]
fn flatten_tolerance_flag_yields_g1_only_csv() {
    let dir = std::env::temp_dir().join("nc-cli-test-flatten");
    std::fs::create_dir_all(&dir).expect("create temp dir");
    let lines = run_cli(&dir, &["--flatten-tolerance", "0.1"]);

    let motions = motion_column(&lines);
    assert!(motions.iter().all(|m| m == "G1"), "non-G1 motion in flattened CSV: {motions:?}");
    // The arc (~25 samples at 0.1 mm) and the spline expand well beyond the
    // 8 programmed blocks.
    assert!(motions.len() > 20, "arc/spline not expanded: {} rows", motions.len());
    assert!(
        lines[0].split(',').any(|name| name == "flattened"),
        "flattened marker column missing: {}",
        lines[0]
    );
    // The consumed interpolation parameters must not leak into the output.
    for gone in ["I", "J", "PW"] {
        assert!(
            !lines[0].split(',').any(|name| name == gone),
            "consumed column {gone} leaked into flattened CSV: {}",
            lines[0]
        );
    }
}

#[test]
fn without_flag_curves_pass_through() {
    let dir = std::env::temp_dir().join("nc-cli-test-raw");
    std::fs::create_dir_all(&dir).expect("create temp dir");
    let lines = run_cli(&dir, &[]);

    let motions = motion_column(&lines);
    assert_eq!(motions.len(), 8);
    assert!(motions.contains(&"G2".to_string()));
    assert!(motions.contains(&"BSPLINE".to_string()));
}
