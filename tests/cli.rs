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
    assert!(
        motions.iter().all(|m| m == "G1"),
        "non-G1 motion in flattened CSV: {motions:?}"
    );
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

/// Real CAM programs that build a timestamped protocol-file name with the full
/// string-operations family - SPRINT, INDEX, single-character writes, `<<`
/// concatenation, SUBSTR and NUMBER - must parse and run end to end. These are
/// the programs that motivated the string support; `$A_YEAR` and friends are
/// real-time-clock system variables the interpreter does not model, so the run
/// uses `--allow-undefined-variables` (they default to 0). The assertion is
/// simply that the whole file interprets without error.
#[test]
fn real_world_string_programs_run() {
    for name in ["control-flow_spindle-ped.mpf", "control-flow_move-grid-a45.mpf"] {
        let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("test-data/real-world")
            .join(name);
        let dir = std::env::temp_dir().join(format!("nc-cli-real-{name}"));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        let input = dir.join("program.mpf");
        std::fs::copy(&src, &input).expect("copy fixture");

        let status = Command::new(env!("CARGO_BIN_EXE_nc-gcode-interpreter"))
            .arg(&input)
            .arg("--allow-undefined-variables")
            .status()
            .expect("binary should run");
        assert!(status.success(), "{name} exited with {status}");

        // A CSV is produced, with at least a header and one interpreted row.
        let csv = std::fs::read_to_string(dir.join("program.csv")).expect("CSV output should exist");
        assert!(csv.lines().count() >= 2, "{name} produced no rows");
    }
}
