//! Documented Siemens spellings must have the same meaning in the whole-file,
//! streaming and indexed parsers. Subprocesses isolate the parser environment.
use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};

struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "nc-syntax-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }

    fn run(&self, source: &str, mode: &str, flatten: bool) -> Output {
        let input = self.0.join("program.mpf");
        std::fs::write(&input, source).unwrap();
        let mut command = Command::new(env!("CARGO_BIN_EXE_nc-gcode-interpreter"));
        command
            .arg(input)
            .env("NC_STAGE1", if mode == "whole-file" { "0" } else { "1" })
            .env(
                "NC_STREAM_MIN_BYTES",
                if mode == "indexed" { "18446744073709551615" } else { "0" },
            );
        if flatten {
            command.args(["--flatten-tolerance", "0.01"]);
        }
        command.output().unwrap()
    }

    fn csv(&self, source: &str, mode: &str, flatten: bool) -> String {
        let output = self.run(source, mode, flatten);
        assert!(
            output.status.success(),
            "{mode}: {source}\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
        std::fs::read_to_string(self.0.join("program.csv")).unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

const MODES: [&str; 3] = ["whole-file", "streaming", "indexed"];

#[test]
fn signed_and_leading_dot_literals_match_existing_spelling() {
    let source = "G0 X.5 Y-.25 Z+0.5\nF+100\nG1 X+.75 Y1. Z-.125\n\
                  R1=+.25*+2-.125\nX=R1\nX=IC(+.375)\n\
                  TRANS X+.5 Y-.25\nG1 X.5 Y+.25\nX=IC(.25)\n\
                  MULT=+2\nE=0\nE=IC(MULT*+.125)\nX=(+.5*MULT)\n\
                  DEF REAL DEPTH=+.5\nZ=DEPTH\nY=ABS(-.125)+.5\n";
    let canonical = "G0 X0.5 Y-0.25 Z0.5\nF100\nG1 X0.75 Y1.0 Z-0.125\n\
                     R1=0.25*2-0.125\nX=R1\nX=IC(0.375)\n\
                     TRANS X0.5 Y-0.25\nG1 X0.5 Y0.25\nX=IC(0.25)\n\
                     MULT=2\nE=0\nE=IC(MULT*0.125)\nX=(0.5*MULT)\n\
                     DEF REAL DEPTH=0.5\nZ=DEPTH\nY=ABS(-0.125)+0.5\n";
    let fixture = Fixture::new();
    let expected = fixture.csv(canonical, "whole-file", false);
    for mode in MODES {
        assert_eq!(fixture.csv(source, mode, false), expected, "{mode}");
    }
}

#[test]
fn padded_g_codes_match_canonical_output_and_flattening() {
    let source = "G000 X0 Y0 Z0\nG017 G01 F100 X1 Y0\n\
                  g003 X0 Y1 I-1 J0\nG002 X1 Y0 I0 J-1\nG0001 X2\n";
    let canonical = "G0 X0 Y0 Z0\nG17 G1 F100 X1 Y0\n\
                     G3 X0 Y1 I-1 J0\nG2 X1 Y0 I0 J-1\nG1 X2\n";
    let fixture = Fixture::new();
    for flatten in [false, true] {
        let expected = fixture.csv(canonical, "whole-file", flatten);
        if flatten {
            assert!(expected.lines().count() > 6, "arcs must actually be flattened");
        }
        for mode in MODES {
            assert_eq!(
                fixture.csv(source, mode, flatten),
                expected,
                "{mode}, flatten={flatten}"
            );
        }
    }
}

#[test]
fn padded_m_codes_match_canonical_output_and_end_the_program() {
    let source = "G0 X1 M03 m008\nX2 M005 M0\nM030\nX3\n";
    let canonical = "G0 X1 M3 M8\nX2 M5 M0\nM30\nX3\n";
    let fixture = Fixture::new();
    let expected = fixture.csv(canonical, "whole-file", false);
    assert!(expected.ends_with(",M30\n"), "M30 must end the program:\n{expected}");
    for mode in MODES {
        assert_eq!(fixture.csv(source, mode, false), expected, "{mode}");
    }
}

#[test]
fn padded_plane_codes_control_frame_rotation() {
    let source = "G018\nROT RPL=90\nG000 X1 Y2 Z3\nG019\nROT RPL=90\nG000 X1 Y2 Z3\n";
    let canonical = "G18\nROT RPL=90\nG0 X1 Y2 Z3\nG19\nROT RPL=90\nG0 X1 Y2 Z3\n";
    let fixture = Fixture::new();
    let expected = fixture.csv(canonical, "whole-file", false);
    for mode in MODES {
        assert_eq!(fixture.csv(source, mode, false), expected, "{mode}");
    }
}

#[test]
fn malformed_literals_and_unknown_codes_still_fail() {
    let fixture = Fixture::new();
    for source in [
        "G0 X.",
        "G0 X+.",
        "G0 X-.",
        "G0 X+",
        "G0 X+-1",
        "G0 X++1",
        "G0 X.5.6",
        "G0 X1.2.3",
        "G0 X=.",
        "G0 X=+.",
        "G0 X=1.2E3",
        "G00099999",
        "G01.5 X1",
    ] {
        for mode in MODES {
            let output = fixture.run(source, mode, false);
            assert!(!output.status.success(), "accepted {source:?} in {mode}");
        }
    }
}

#[test]
fn padded_looking_identifiers_remain_variables() {
    let source = "DEF REAL G01_OFFSET=+.5\nG0 X=G01_OFFSET\n";
    let fixture = Fixture::new();
    let expected = fixture.csv("DEF REAL G01_OFFSET=0.5\nG0 X=G01_OFFSET\n", "whole-file", false);
    for mode in MODES {
        assert_eq!(fixture.csv(source, mode, false), expected, "{mode}");
    }
}

#[test]
fn ex_exponent_literals_match_the_plain_spelling() {
    // Manual 3 "REAL constant" (X=-.1EX-3) and 4.1 ($R[6]=1.874EX8, 8.2EX-3).
    let source = "G1 X=-.1EX-3 Y2EX1 Z1.5ex+2 F100\nR1=1.874EX8\nR2=8.2EX-3*2\nX=R1/1EX6 Y=R2\n\
                  X=IC(2.5EX-1)\nTRANS X1EX1\nG1 X1 Y2EX0\nDEF REAL DEPTH=-5EX-1\nZ=DEPTH\nSCALE X2EX0\nAROT X3EX1\nG1 Y1\n";
    let canonical = "G1 X=-0.0001 Y20 Z150 F100\nR1=187400000\nR2=0.0082*2\nX=R1/1000000 Y=R2\n\
                     X=IC(0.25)\nTRANS X10\nG1 X1 Y2\nDEF REAL DEPTH=-0.5\nZ=DEPTH\nSCALE X2\nAROT X30\nG1 Y1\n";
    let fixture = Fixture::new();
    let expected = fixture.csv(canonical, "whole-file", false);
    for mode in MODES {
        assert_eq!(fixture.csv(source, mode, false), expected, "{mode}");
    }
}

#[test]
fn malformed_exponents_are_rejected() {
    let fixture = Fixture::new();
    for source in ["G1 X1EX-\n", "G1 X1EX2.5\n", "R1=1EXA\n", "R1=1EX\n", "R1=1E5\n"] {
        for mode in MODES {
            assert!(!fixture.run(source, mode, false).status.success(), "{mode}: {source}");
        }
    }
}
