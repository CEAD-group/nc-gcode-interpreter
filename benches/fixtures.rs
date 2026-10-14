use std::fmt::Write;

pub fn program(shape: &str, lines: usize) -> String {
    assert!(lines >= 8);
    let mut text = String::with_capacity(lines * 90);
    text.push_str(if shape == "frames" {
        "TRANS X10\n"
    } else {
        "MULTIPLIER=1\n"
    });
    text.push_str("G1 X0 Y0 Z0 E0 F1200\n");
    for i in 2..lines - 1 {
        match shape {
            "calls" if i % 1000 == 0 => text.push_str("SUPPORT_PIN_TRIGGER(2.5)\n"),
            "grammar" => text.push_str("X=SIN(30) Y=COS(60)\n"),
            "structured" if i == 2 => text.push_str("IF 1==1\n"),
            "structured" if i == lines - 2 => text.push_str("ENDIF\n"),
            _ => writeln!(
                text,
                "N{i} G1 X{}.125 Y{}.25 Z1 E=IC({}) ELX=100 F1200",
                i % 1000,
                i % 300,
                if shape == "frames" { "0.025" } else { "MULTIPLIER*0.025" }
            )
            .unwrap(),
        }
    }
    text.push_str("M30\n");
    text
}

/// All edits are against the original revision, allowing alternating edit/undo.
pub fn edit(source: &str, kind: &str, alternate: bool) -> (String, std::ops::Range<usize>, String) {
    let mut lines: Vec<_> = source.lines().collect();
    let mid = lines.len() / 2;
    let (range, replacement) = match kind {
        "none" => (0..0, String::new()),
        "coordinate" => (
            mid..mid + 1,
            format!("G1 X{} Y2 E=IC(0.025)", if alternate { 7 } else { 8 }),
        ),
        "parameter" => (0..1, format!("MULTIPLIER={}", if alternate { 2 } else { 3 })),
        "translation" => (0..1, format!("TRANS X{}", if alternate { 20 } else { 30 })),
        "insert" => (mid..mid, "; inserted comment\nG1 X3".into()),
        "delete" => (mid..mid + 1, String::new()),
        _ => panic!("unknown edit {kind}"),
    };
    lines.splice(range.clone(), replacement.lines());
    (format!("{}\n", lines.join("\n")), range, replacement)
}
