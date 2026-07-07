//! Line-level pre-scan of control structures.
//!
//! A PEG parser cannot produce a useful error for an unclosed IF/WHILE/...:
//! it fails at the farthest position it reached — usually the end of the
//! file — with no reference to the opener. Sinumerik NC code is
//! line-oriented and structure keywords can only appear at the start of a
//! block, so a trivial scan can match openers and closers exactly and
//! report the *cause*: "IF on line 2 has no matching ENDIF".
//!
//! The scan is purely a pre-validator run before the real parse; it never
//! accepts anything pest would reject, it only turns the worst class of
//! parse errors into precise ones. It also stays deliberately conservative:
//! keywords are only recognized as the first word of a line (after the
//! optional block number and jump label), which is the only place the
//! grammar allows them.

use crate::errors::ParsingError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Structure {
    If,
    While,
    For,
    Loop,
    Repeat,
}

impl Structure {
    fn opener(self) -> &'static str {
        match self {
            Structure::If => "IF",
            Structure::While => "WHILE",
            Structure::For => "FOR",
            Structure::Loop => "LOOP",
            Structure::Repeat => "REPEAT",
        }
    }
    fn closer(self) -> &'static str {
        match self {
            Structure::If => "ENDIF",
            Structure::While => "ENDWHILE",
            Structure::For => "ENDFOR",
            Structure::Loop => "ENDLOOP",
            Structure::Repeat => "UNTIL",
        }
    }
}

/// Validate that every IF/WHILE/FOR/LOOP/REPEAT has its matching closer and
/// vice versa. Returns the first structural error with the line that caused
/// it (the opener for a missing closer, the closer for a missing opener).
/// On success, reports whether the program contains any multi-line control
/// structure at all: structure-free programs (all CAM output) qualify for
/// the per-line fast path.
pub fn check_structures(input: &str) -> Result<ProgramShape, ParsingError> {
    let mut stack: Vec<(Structure, usize, String)> = Vec::new();
    let mut has_block_structures = false;

    for (index, raw_line) in input.lines().enumerate() {
        let line_no = index + 1;
        let line = strip_comment(raw_line);
        let Some(word) = first_structure_word(line) else {
            continue;
        };
        let error = |message: String| {
            Err(ParsingError::UnmatchedStructure {
                line_no,
                preview: raw_line.to_string(),
                message,
            })
        };
        match word.as_str() {
            // IF opens a block unless it is the single-line conditional
            // jump form (IF <condition> GOTO... <target>).
            "IF" => {
                if !contains_goto_word(line) {
                    has_block_structures = true;
                    stack.push((Structure::If, line_no, raw_line.to_string()));
                }
            }
            "WHILE" => {
                has_block_structures = true;
                stack.push((Structure::While, line_no, raw_line.to_string()));
            }
            "FOR" => {
                has_block_structures = true;
                stack.push((Structure::For, line_no, raw_line.to_string()));
            }
            "LOOP" => {
                has_block_structures = true;
                stack.push((Structure::Loop, line_no, raw_line.to_string()));
            }
            "REPEAT" => {
                has_block_structures = true;
                stack.push((Structure::Repeat, line_no, raw_line.to_string()));
            }
            "ELSE" => match stack.last() {
                Some((Structure::If, _, _)) => {}
                Some((open, open_line, _)) => {
                    return error(format!(
                        "ELSE does not belong to an IF: the innermost open structure is {} from line {}",
                        open.opener(),
                        open_line
                    ))
                }
                None => return error("ELSE without a preceding IF".to_string()),
            },
            "ENDIF" | "ENDWHILE" | "ENDFOR" | "ENDLOOP" | "UNTIL" => {
                let expected = match word.as_str() {
                    "ENDIF" => Structure::If,
                    "ENDWHILE" => Structure::While,
                    "ENDFOR" => Structure::For,
                    "ENDLOOP" => Structure::Loop,
                    _ => Structure::Repeat,
                };
                match stack.pop() {
                    Some((open, _, _)) if open == expected => {}
                    Some((open, open_line, _)) => {
                        return error(format!(
                            "{} closes {}, but the innermost open structure is {} from line {} (expected {})",
                            word,
                            expected.opener(),
                            open.opener(),
                            open_line,
                            open.closer()
                        ))
                    }
                    None => return error(format!("{} without a preceding {}", word, expected.opener())),
                }
            }
            _ => {}
        }
    }

    if let Some((open, open_line, open_preview)) = stack.pop() {
        // Report the innermost unclosed structure (the last one opened):
        // with nesting like IF ... WHILE ... <EOF>, the ENDWHILE must come
        // before the ENDIF, so that is the actionable fix.
        return Err(ParsingError::UnmatchedStructure {
            line_no: open_line,
            preview: open_preview,
            message: format!(
                "{} is never closed: no matching {} before the end of the program",
                open.opener(),
                open.closer()
            ),
        });
    }
    Ok(ProgramShape { has_block_structures })
}

/// Result of a successful structure scan.
pub struct ProgramShape {
    /// True when the program contains IF/WHILE/FOR/LOOP/REPEAT blocks
    /// (multi-line structures); false for straight-line programs, which
    /// may still contain labels, jumps and single-line conditional jumps.
    pub has_block_structures: bool,
}

fn strip_comment(line: &str) -> &str {
    match line.find(';') {
        Some(position) => &line[..position],
        None => line,
    }
}

/// The first word of the block, skipping the optional block number (N123)
/// and the optional jump label (NAME:), uppercased. Returns None for lines
/// that cannot start a control structure.
fn first_structure_word(line: &str) -> Option<String> {
    let mut rest = line.trim_start();

    // optional block number; the grammar's block_number_set is non-atomic,
    // so whitespace may separate the N from its digits ("N 123 IF ...")
    let bytes = rest.as_bytes();
    if bytes.first().is_some_and(|b| *b == b'N' || *b == b'n') {
        let digits_start = 1 + bytes[1..].iter().take_while(|b| b.is_ascii_whitespace()).count();
        let digit_count = bytes[digits_start..].iter().take_while(|b| b.is_ascii_digit()).count();
        if digit_count > 0 {
            rest = rest[digits_start + digit_count..].trim_start();
        }
    }

    let word_end = rest
        .bytes()
        .take_while(|b| b.is_ascii_alphanumeric() || *b == b'_')
        .count();
    if word_end == 0 {
        return None;
    }
    // optional jump label: a word directly followed by a colon
    if rest.as_bytes().get(word_end) == Some(&b':') {
        rest = rest[word_end + 1..].trim_start();
        let label_next_end = rest
            .bytes()
            .take_while(|b| b.is_ascii_alphanumeric() || *b == b'_')
            .count();
        if label_next_end == 0 {
            return None;
        }
        rest = &rest[..label_next_end];
    } else {
        rest = &rest[..word_end];
    }
    // All structure keywords start with I/E/W/F/L/R/U; skip the uppercase
    // allocation for the coordinate-flood lines that dominate real files.
    if !matches!(
        rest.as_bytes().first(),
        Some(b'I' | b'i' | b'E' | b'e' | b'W' | b'w' | b'F' | b'f' | b'L' | b'l' | b'R' | b'r' | b'U' | b'u')
    ) {
        return None;
    }
    Some(rest.to_uppercase())
}

/// True if the line contains a GOTO-family word (GOTO/GOTOF/GOTOB/GOTOC/
/// GOTOS) as a standalone word: the single-line conditional jump form.
fn contains_goto_word(line: &str) -> bool {
    let upper = line.to_uppercase();
    let bytes = upper.as_bytes();
    let mut search_start = 0;
    while let Some(found) = upper[search_start..].find("GOTO") {
        let start = search_start + found;
        let preceded_ok = start == 0 || !(bytes[start - 1].is_ascii_alphanumeric() || bytes[start - 1] == b'_');
        let mut end = start + 4;
        // allow the F/B/C/S suffix
        if matches!(bytes.get(end), Some(b'F') | Some(b'B') | Some(b'C') | Some(b'S')) {
            end += 1;
        }
        let followed_ok = !bytes.get(end).is_some_and(|b| b.is_ascii_alphanumeric() || *b == b'_');
        if preceded_ok && followed_ok {
            return true;
        }
        search_start = start + 4;
    }
    false
}
