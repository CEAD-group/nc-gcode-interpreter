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
use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Structure {
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
    // A closer after other content (`X1 ENDIF`) is invisible to the
    // first-word scan, which would blame the opener; name the closer instead.
    scan(input, None).map_err(|error| misplaced_keyword_error(input).unwrap_or(error))
}

/// `check_structures` plus the structure table the statement-AST driver
/// navigates by. A second ELSE in one IF is an error here; the grammar
/// rejects it as well, which is all check_structures relies on.
#[allow(dead_code)] // the NC_EXEC=ast driver's entry, wired in its next phase
pub(crate) fn scan_structure_table(input: &str) -> Result<(ProgramShape, StructureTable), ParsingError> {
    let mut table = StructureTable::default();
    let shape = scan(input, Some(&mut table)).map_err(|error| misplaced_keyword_error(input).unwrap_or(error))?;
    Ok((shape, table))
}

/// The line structure of a program, keyed by 1-based line number; only
/// structure lines have an entry.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct StructureTable {
    roles: HashMap<usize, LineRole>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LineRole {
    /// An IF/WHILE/FOR/LOOP/REPEAT head line.
    Opener {
        kind: Structure,
        else_line: Option<usize>,
        closer_line: usize,
    },
    /// The ELSE of the IF on `opener_line`.
    Else { opener_line: usize },
    /// The ENDIF/ENDWHILE/ENDFOR/ENDLOOP/UNTIL of the structure on `opener_line`.
    Closer { kind: Structure, opener_line: usize },
}

#[allow(dead_code)] // read by the NC_EXEC=ast driver
impl StructureTable {
    pub(crate) fn role(&self, line_no: usize) -> Option<LineRole> {
        self.roles.get(&line_no).copied()
    }

    pub(crate) fn len(&self) -> usize {
        self.roles.len()
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.roles.is_empty()
    }
}

/// An open structure on the scan stack.
struct Open {
    kind: Structure,
    line_no: usize,
    preview: String,
    else_line: Option<usize>,
}

fn scan(input: &str, mut table: Option<&mut StructureTable>) -> Result<ProgramShape, ParsingError> {
    let mut stack: Vec<Open> = Vec::new();
    let mut has_block_structures = false;

    for (index, raw_line) in input.lines().enumerate() {
        let line_no = index + 1;
        let line = strip_comment(raw_line);
        let Some(word) = first_structure_word(line) else {
            continue;
        };
        if let Some((label, keyword, column)) = labelled_structure_keyword(raw_line) {
            return Err(ParsingError::ParsingContext {
                line_no,
                column: Some(column),
                preview: raw_line.to_string(),
                context: "control structure check".to_string(),
                message: labelled_structure_message(&label, &keyword),
            });
        }
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
                    stack.push(Open {
                        kind: Structure::If,
                        line_no,
                        preview: raw_line.to_string(),
                        else_line: None,
                    });
                }
            }
            "WHILE" => {
                has_block_structures = true;
                stack.push(Open {
                    kind: Structure::While,
                    line_no,
                    preview: raw_line.to_string(),
                    else_line: None,
                });
            }
            "FOR" => {
                has_block_structures = true;
                stack.push(Open {
                    kind: Structure::For,
                    line_no,
                    preview: raw_line.to_string(),
                    else_line: None,
                });
            }
            "LOOP" => {
                has_block_structures = true;
                stack.push(Open {
                    kind: Structure::Loop,
                    line_no,
                    preview: raw_line.to_string(),
                    else_line: None,
                });
            }
            "REPEAT" => {
                has_block_structures = true;
                stack.push(Open {
                    kind: Structure::Repeat,
                    line_no,
                    preview: raw_line.to_string(),
                    else_line: None,
                });
            }
            "ELSE" => match stack.last_mut() {
                Some(open) if open.kind == Structure::If => {
                    if let Some(table) = table.as_deref_mut() {
                        if let Some(first) = open.else_line {
                            return error(format!(
                                "IF on line {} already has an ELSE on line {}",
                                open.line_no, first
                            ));
                        }
                        open.else_line = Some(line_no);
                        table.roles.insert(
                            line_no,
                            LineRole::Else {
                                opener_line: open.line_no,
                            },
                        );
                    }
                }
                Some(open) => {
                    return error(format!(
                        "ELSE does not belong to an IF: the innermost open structure is {} from line {}",
                        open.kind.opener(),
                        open.line_no
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
                    Some(open) if open.kind == expected => {
                        if let Some(table) = table.as_deref_mut() {
                            table.roles.insert(
                                open.line_no,
                                LineRole::Opener {
                                    kind: open.kind,
                                    else_line: open.else_line,
                                    closer_line: line_no,
                                },
                            );
                            table.roles.insert(
                                line_no,
                                LineRole::Closer {
                                    kind: open.kind,
                                    opener_line: open.line_no,
                                },
                            );
                        }
                    }
                    Some(open) => {
                        return error(format!(
                            "{} closes {}, but the innermost open structure is {} from line {} (expected {})",
                            word,
                            expected.opener(),
                            open.kind.opener(),
                            open.line_no,
                            open.kind.closer()
                        ))
                    }
                    None => return error(format!("{} without a preceding {}", word, expected.opener())),
                }
            }
            _ => {}
        }
    }

    if let Some(open) = stack.pop() {
        // Report the innermost unclosed structure (the last one opened):
        // with nesting like IF ... WHILE ... <EOF>, the ENDWHILE must come
        // before the ENDIF, so that is the actionable fix.
        return Err(ParsingError::UnmatchedStructure {
            line_no: open.line_no,
            preview: open.preview,
            message: format!(
                "{} is never closed: no matching {} before the end of the program",
                open.kind.opener(),
                open.kind.closer()
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

/// The line after the optional block number (N123) and the optional jump
/// label (NAME:), without leading whitespace.
fn skip_block_prefix(line: &str) -> &str {
    split_block_prefix(line).1
}

/// The byte range of the jump label in `line`, if any, and the rest of the
/// line after the block number and label (see skip_block_prefix).
fn split_block_prefix(line: &str) -> (Option<(usize, usize)>, &str) {
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

    // optional jump label: a word directly followed by a colon
    let mut label = None;
    let word_end = word_len(rest);
    if word_end > 0 && rest.as_bytes().get(word_end) == Some(&b':') {
        let start = line.len() - rest.len();
        label = Some((start, start + word_end));
        rest = rest[word_end + 1..].trim_start();
    }
    (label, rest)
}

fn word_len(text: &str) -> usize {
    text.bytes()
        .take_while(|b| b.is_ascii_alphanumeric() || *b == b'_')
        .count()
}

/// The first word of the block, skipping the optional block number (N123)
/// and the optional jump label (NAME:), uppercased. Returns None for lines
/// that cannot start a control structure.
pub(crate) fn first_structure_word(line: &str) -> Option<String> {
    let rest = skip_block_prefix(line);
    let word = &rest[..word_len(rest)];
    // All structure keywords start with I/E/W/F/L/R/U; skip the uppercase
    // allocation for the coordinate-flood lines that dominate real files.
    if !matches!(
        word.as_bytes().first(),
        Some(b'I' | b'i' | b'E' | b'e' | b'W' | b'w' | b'F' | b'f' | b'L' | b'l' | b'R' | b'r' | b'U' | b'u')
    ) {
        return None;
    }
    Some(word.to_uppercase())
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

/// Keywords that end a structure body. Each must start its own block: the
/// grammar rejects one that follows other content on the line (#84).
const BODY_END_KEYWORDS: &[&str] = &["ELSE", "ENDIF", "ENDWHILE", "ENDFOR", "ENDLOOP", "UNTIL"];

/// The first ELSE/ENDIF/.../UNTIL word that follows other content in the
/// block, outside strings and the comment, uppercased with its 1-based
/// column. The block number and label may precede it. Error paths only.
pub(crate) fn misplaced_structure_keyword(line: &str) -> Option<(String, usize)> {
    let bytes = line.as_bytes();
    let mut i = line.len() - skip_block_prefix(line).len();
    let mut in_string = false;
    let mut content_seen = false;
    while i < bytes.len() {
        let b = bytes[i];
        if in_string {
            in_string = b != b'"';
            i += 1;
        } else if b == b';' {
            return None;
        } else if b.is_ascii_alphanumeric() || b == b'_' {
            let start = i;
            i += word_len(&line[i..]);
            let word = &line[start..i];
            if content_seen && BODY_END_KEYWORDS.iter().any(|kw| kw.eq_ignore_ascii_case(word)) {
                return Some((word.to_uppercase(), start + 1));
            }
            content_seen = true;
        } else {
            in_string = b == b'"';
            content_seen |= !b.is_ascii_whitespace();
            i += 1;
        }
    }
    None
}

/// The label of a block that holds a control-structure keyword (`LB: WHILE
/// R1<3`, `LB: ENDIF`), with the keyword and the label's 1-based column. A
/// conditional jump (`LB: IF R1>0 GOTOB LB`) is no structure and may keep it.
pub(crate) fn labelled_structure_keyword(line: &str) -> Option<(String, String, usize)> {
    let keyword = first_structure_word(line)?;
    let is_structure = match keyword.as_str() {
        "IF" => !contains_goto_word(strip_comment(line)),
        "ELSE" | "ENDIF" | "WHILE" | "ENDWHILE" | "FOR" | "ENDFOR" | "LOOP" | "ENDLOOP" | "REPEAT" | "UNTIL" => true,
        _ => false,
    };
    if !is_structure {
        return None;
    }
    let (start, end) = split_block_prefix(line).0?;
    Some((line[start..end].to_string(), keyword, start + 1))
}

pub(crate) fn labelled_structure_message(label: &str, keyword: &str) -> String {
    format!(
        "the jump label {label}: is not allowed in a block with {keyword} (a real control raises \
         alarm 12630), so write the label alone in the block before it"
    )
}

pub(crate) fn misplaced_keyword_message(keyword: &str) -> String {
    format!(
        "{keyword} must start its own block: a control-structure keyword cannot follow other \
         content on the same line, so put {keyword} on its own line"
    )
}

/// The first line of the program with a misplaced structure keyword, as a
/// parse error at the keyword's column.
fn misplaced_keyword_error(input: &str) -> Option<ParsingError> {
    input.lines().enumerate().find_map(|(index, line)| {
        misplaced_structure_keyword(line).map(|(keyword, column)| ParsingError::ParsingContext {
            line_no: index + 1,
            column: Some(column),
            preview: line.to_string(),
            context: "control structure check".to_string(),
            message: misplaced_keyword_message(&keyword),
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keyword_after_content_is_misplaced() {
        for (line, keyword, column) in [
            ("X1 ELSE X2", "ELSE", 4),
            ("X1 else", "ELSE", 4),
            ("N10 R1=R1+1 ENDWHILE", "ENDWHILE", 13),
            ("GOTOF LB ENDIF", "ENDIF", 10),
            ("LOOP ENDLOOP", "ENDLOOP", 6),
            ("ELSE ENDIF", "ENDIF", 6),
            ("MSG(\"a\") UNTIL R1>0", "UNTIL", 10),
        ] {
            assert_eq!(
                misplaced_structure_keyword(line),
                Some((keyword.to_string(), column)),
                "{line:?}"
            );
        }
    }

    #[test]
    fn keyword_at_block_start_or_in_text_is_not_misplaced() {
        for line in [
            "ENDIF",
            "  ELSE ; X1 ENDIF",
            "N20 ENDWHILE",
            "N 20 ENDWHILE",
            "LB: ENDIF",
            "N10 LB: ELSE",
            "MSG(\"X1 ELSE\")",
            "X1 ; ELSE",
            "ELSEWHERE=1 ENDIFX=2",
            "WHILE R1<3",
        ] {
            assert_eq!(misplaced_structure_keyword(line), None, "{line:?}");
        }
    }

    #[test]
    fn label_on_structure_line_is_found() {
        for (line, label, keyword, column) in [
            ("LB: WHILE R1<3", "LB", "WHILE", 1),
            ("N10 LB: ENDIF", "LB", "ENDIF", 5),
            ("  Back_1: else ; c", "Back_1", "ELSE", 3),
            ("LB: IF R1>0 ; GOTOF in a comment", "LB", "IF", 1),
        ] {
            assert_eq!(
                labelled_structure_keyword(line),
                Some((label.to_string(), keyword.to_string(), column)),
                "{line:?}"
            );
        }
        for line in [
            "LB: IF R1>0 GOTOB LB",
            "LB: X1",
            "LB:",
            "WHILE R1<3",
            "LB: INDEX(1)",
            "LB: GOTOF ENDE",
        ] {
            assert_eq!(labelled_structure_keyword(line), None, "{line:?}");
        }
    }

    #[test]
    fn first_word_skips_block_number_and_label() {
        assert_eq!(first_structure_word("N10 WHILE R1<3").as_deref(), Some("WHILE"));
        assert_eq!(first_structure_word("n 10 lb: endif").as_deref(), Some("ENDIF"));
        assert_eq!(first_structure_word("UNTIL_X: X1").as_deref(), None);
        assert_eq!(first_structure_word("LB:").as_deref(), None);
        assert_eq!(first_structure_word("X1 ELSE").as_deref(), None);
    }

    fn table(program: &str) -> StructureTable {
        let (_, table) = scan_structure_table(program).unwrap_or_else(|e| panic!("{program}\n{e}"));
        table
    }

    fn opener(kind: Structure, else_line: Option<usize>, closer_line: usize) -> Option<LineRole> {
        Some(LineRole::Opener {
            kind,
            else_line,
            closer_line,
        })
    }

    fn closer(kind: Structure, opener_line: usize) -> Option<LineRole> {
        Some(LineRole::Closer { kind, opener_line })
    }

    #[test]
    fn nested_if_in_while() {
        let table = table("R1=0\nWHILE R1<3\n  IF R1==1\n    X1\n  ENDIF\n  R1=R1+1\nENDWHILE\nM30\n");
        assert_eq!(table.role(2), opener(Structure::While, None, 7));
        assert_eq!(table.role(3), opener(Structure::If, None, 5));
        assert_eq!(table.role(5), closer(Structure::If, 3));
        assert_eq!(table.role(7), closer(Structure::While, 2));
        for line in [1, 4, 6, 8] {
            assert_eq!(table.role(line), None, "line {line}");
        }
        assert_eq!(table.len(), 4);
    }

    #[test]
    fn if_with_else() {
        let table = table("N10 IF R1>0 ; test\nX1\nN20 else ; other\nX2\nendif\n");
        assert_eq!(table.role(1), opener(Structure::If, Some(3), 5));
        assert_eq!(table.role(3), Some(LineRole::Else { opener_line: 1 }));
        assert_eq!(table.role(5), closer(Structure::If, 1));
    }

    #[test]
    fn else_belongs_to_the_innermost_if() {
        let table = table("IF A\nIF B\nX1\nELSE\nX2\nENDIF\nELSE\nX3\nENDIF\n");
        assert_eq!(table.role(1), opener(Structure::If, Some(7), 9));
        assert_eq!(table.role(2), opener(Structure::If, Some(4), 6));
        assert_eq!(table.role(4), Some(LineRole::Else { opener_line: 2 }));
        assert_eq!(table.role(7), Some(LineRole::Else { opener_line: 1 }));
    }

    #[test]
    fn repeat_until() {
        let table = table("REPEAT\nX=X+1\nUNTIL X>3\n");
        assert_eq!(table.role(1), opener(Structure::Repeat, None, 3));
        assert_eq!(table.role(3), closer(Structure::Repeat, 1));
    }

    #[test]
    fn for_loop() {
        let table = table("FOR I=1 TO 3\nX=I\nENDFOR\n");
        assert_eq!(table.role(1), opener(Structure::For, None, 3));
        assert_eq!(table.role(3), closer(Structure::For, 1));
    }

    #[test]
    fn loop_with_jump_out() {
        let table = table("LOOP\nIF R1>3 GOTOF OUT\nR1=R1+1\nENDLOOP\nOUT: X1\n");
        assert_eq!(table.role(1), opener(Structure::Loop, None, 4));
        assert_eq!(table.role(2), None);
        assert_eq!(table.role(4), closer(Structure::Loop, 1));
    }

    #[test]
    fn single_line_if_goto_is_not_a_block() {
        let (shape, table) = scan_structure_table("N10 IF R1>0 GOTOF L1\nX1\nL1: X2\n").expect("valid");
        assert!(!shape.has_block_structures);
        assert!(table.is_empty());
    }

    #[test]
    fn a_second_else_is_an_error_for_the_table_only() {
        let program = "IF A\nX1\nELSE\nX2\nELSE\nX3\nENDIF\n";
        assert!(
            check_structures(program).is_ok(),
            "check_structures leaves this to the parser"
        );
        let error = scan_structure_table(program).err().expect("two ELSEs");
        assert_eq!(error.location().map(|l| l.line), Some(5));
        assert!(error.to_string().contains("already has an ELSE on line 3"), "{error}");
    }

    #[test]
    fn errors_match_check_structures() {
        for program in [
            "IF A\nX1\n",
            "WHILE A\nIF B\nENDWHILE\n",
            "ENDIF\n",
            "X1\nELSE\n",
            "LOOP\nELSE\nENDLOOP\n",
            "REPEAT\nENDFOR\n",
        ] {
            let expected = check_structures(program).err().expect("invalid").to_string();
            let actual = scan_structure_table(program).err().expect("invalid").to_string();
            assert_eq!(expected, actual, "{program}");
        }
    }
}
