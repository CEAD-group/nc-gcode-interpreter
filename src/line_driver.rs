//! Stage-1 line triage: the per-line fast path for structure-free programs.
//!
//! Real CAM output is >99.9% lines of the shape
//! `N38 X-45.414 Y-8.835 Z49. A=0.0 ELX=3083.63 ; comment` — no
//! expressions, no definitions, no control flow. Parsing such a line with
//! the full grammar costs microseconds and ~40 parse-tree pairs; a byte
//! scanner decodes it in nanoseconds. This module claims exactly those
//! trivial lines and hands every other line to pest individually, so the
//! full grammar still owns everything structurally interesting.
//!
//! Scope: only programs without multi-line control structures
//! (IF/WHILE/FOR/LOOP/REPEAT blocks) take this path — the structure scan
//! decides. Labels, block numbers and the whole GOTO family remain
//! supported: jump statements are line-scoped, parse via pest, and resolve
//! against the line index built here.
//!
//! Correctness policy: the decoder is conservative (any unexpected byte
//! rejects the line to pest) and must replicate the interpreter's effects
//! exactly for the lines it claims; the differential test mode in
//! python/tests/test_stage1.py re-runs programs with the fast path
//! disabled and asserts identical output.

use crate::errors::ParsingError;
use crate::interpret_rules::{
    canonical_block_number, insert_m_key, interpret_block,
    is_end_of_program_m_code, resolve_jump, scan_jump_targets, BlockFlow,
};
use crate::modal_groups::classify_g_command;
use crate::state::State;
use crate::types::{NCParser, Pair, Rule, Value};
use pest::Parser;
use std::collections::HashMap;

type Output = crate::output::OutputRows;

/// One word of a decoded trivial line, in source order.
enum Word {
    /// `X12.5` or `AXIS=12.5`: routed to axis / block address / variable
    /// exactly like `interpret_assignment` would.
    Assign(String, f64),
    /// A vocabulary-known keyword or `G<digits>` command: (group, as written).
    GCommand(&'static str, String),
    /// An M code, as written.
    MCode(String),
}

struct DecodedLine {
    line_no: usize,
    n: Option<String>,
    comment: Option<String>,
    words: Vec<Word>,
    /// The line defines a jump label (recorded in the target index only).
    has_content: bool,
}

enum LineExec<'i> {
    Decoded(DecodedLine),
    /// A pest-parsed block. The line was parsed with `line_entry` padded
    /// with leading newlines so that all positions (and therefore error
    /// messages) are correct in whole-file coordinates.
    Parsed(Pair<'i, Rule>),
    Blank,
}

/// Interpret a structure-free program line by line. Mirrors
/// `interpret_blocks` for a flat block list: jumps resolve against the
/// line index; an unresolved jump propagates to the caller.
///
/// Returns `Ok(None)` - before touching `output` or `state` - when the
/// program does not suit the fast path after all (see the padding budget
/// below); the caller then runs the whole-file parse instead.
pub fn interpret_lines(
    input: &str,
    padded_lines: &mut Vec<String>,
    output: &mut Output,
    state: &mut State,
) -> Result<Option<BlockFlow>, ParsingError> {
    // Pass 1: decode every line (owned results, no borrows).
    let lines: Vec<&str> = input.lines().collect();
    let mut decode_results: Vec<DecodeResult> = Vec::with_capacity(lines.len());
    let mut padding_bytes: usize = 0;
    for (index, line) in lines.iter().enumerate() {
        let result = decode_line(line, index + 1, state);
        if matches!(result, DecodeResult::NeedsGrammar) {
            padding_bytes += index;
        }
        decode_results.push(result);
    }

    // The newline padding gives pest whole-file positions, but costs
    // O(line_no) bytes per grammar-parsed line. A program where many late
    // lines need the grammar would degenerate to O(n²) memory; decline and
    // let the whole-file parse handle it (it parses everything once anyway).
    // Real CAM output is >99.9% trivial lines, so it never comes close.
    if padding_bytes > input.len().max(1 << 22) {
        return Ok(None);
    }

    // Prepare the newline-padded copies for the lines that need the grammar.
    for (index, result) in decode_results.iter().enumerate() {
        padded_lines.push(match result {
            DecodeResult::NeedsGrammar => {
                // Pad with newlines so pest reports whole-file positions.
                let mut padded = "\n".repeat(index);
                padded.push_str(lines[index]);
                padded
            }
            _ => String::new(),
        });
    }

    // Pass 2: parse the non-trivial lines, build the jump-target index and
    // the executable list. padded_lines is only read from here on, so the
    // parsed pairs may borrow from it.
    let mut targets: HashMap<String, Vec<usize>> = HashMap::new();
    let mut execs: Vec<LineExec> = Vec::with_capacity(decode_results.len());
    for (index, result) in decode_results.into_iter().enumerate() {
        match result {
            DecodeResult::Trivial(decoded, label) => {
                if let Some(n) = &decoded.n {
                    targets
                        .entry(format!("N:{}", canonical_block_number(n)))
                        .or_default()
                        .push(index);
                }
                if let Some(label) = label {
                    targets.entry(format!("LABEL:{}", label)).or_default().push(index);
                }
                execs.push(LineExec::Decoded(decoded));
            }
            DecodeResult::Blank => execs.push(LineExec::Blank),
            DecodeResult::NeedsGrammar => {
                let block = parse_single_line(&padded_lines[index], index + 1, state)?;
                for key in scan_jump_targets(std::slice::from_ref(&block)).into_keys() {
                    targets.entry(key).or_default().push(index);
                }
                execs.push(LineExec::Parsed(block));
            }
        }
    }

    state.seen_jump_targets.extend(targets.keys().cloned());
    state.jump_scopes.push(targets.keys().cloned().collect());
    let result = run_lines(&execs, &targets, output, state);
    state.jump_scopes.pop();
    result.map(Some)
}

fn run_lines(
    execs: &[LineExec],
    targets: &HashMap<String, Vec<usize>>,
    output: &mut Output,
    state: &mut State,
) -> Result<BlockFlow, ParsingError> {
    let mut index = 0;
    let mut jumps_taken = 0;
    while index < execs.len() {
        let flow = match &execs[index] {
            LineExec::Blank => BlockFlow::Continue,
            LineExec::Decoded(line) => execute_decoded(line, output, state)?,
            LineExec::Parsed(block) => interpret_block(block.clone(), output, state)?,
        };
        match flow {
            BlockFlow::Continue => index += 1,
            BlockFlow::EndProgram => return Ok(BlockFlow::EndProgram),
            BlockFlow::Jump(request) => match resolve_jump(targets, index, &request) {
                Some(destination) => {
                    // Only backward jumps can form cycles; bound them like
                    // the loop statements (same >= threshold). Forward jumps
                    // strictly advance and are fine in any number.
                    if destination <= index {
                        jumps_taken += 1;
                        if jumps_taken >= state.iteration_limit {
                            return Err(ParsingError::LoopLimit {
                                limit: state.iteration_limit.to_string(),
                            });
                        }
                    }
                    index = destination;
                }
                None => return Ok(BlockFlow::Jump(request)),
            },
        }
    }
    Ok(BlockFlow::Continue)
}

/// Execute a decoded trivial line: the exact effects of `interpret_block`
/// on the same line, without the parse tree.
fn execute_decoded(line: &DecodedLine, output: &mut Output, state: &mut State) -> Result<BlockFlow, ParsingError> {
    if !line.has_content {
        return Ok(BlockFlow::Continue);
    }
    output.start_row(line.line_no)?;
    let mut flow = BlockFlow::Continue;
    // Split borrows: row insertion vs axis-state updates.
    for word in &line.words {
        match word {
            Word::Assign(key, value) => {
                if state.is_axis(key) {
                    let machine_value = state.update_axis(key, *value)?;
                    let last = output.last_mut().expect("row was just pushed");
                    last.insert(key.clone(), Value::Float(machine_value));
                } else if state.is_block_address(key) {
                    let last = output.last_mut().expect("row was just pushed");
                    last.insert(key.clone(), Value::Float(*value));
                } else {
                    state.symbol_table.insert(key.clone(), *value);
                    output.record_variable_change(key, *value);
                }
            }
            Word::GCommand(group, as_written) => {
                let last = output.last_mut().expect("row was just pushed");
                last.insert(group.to_string(), Value::Str(as_written.clone()));
            }
            Word::MCode(code) => {
                let last = output.last_mut().expect("row was just pushed");
                let preview = state.get_line(line.line_no).unwrap_or("").to_string();
                insert_m_key(last, code, line.line_no, preview)?;
                if is_end_of_program_m_code(code) {
                    flow = BlockFlow::EndProgram;
                }
            }
        }
    }
    let last = output.last_mut().expect("row was just pushed");
    if let Some(n) = &line.n {
        last.insert("N".to_string(), Value::Str(n.clone()));
    }
    if let Some(comment) = &line.comment {
        last.insert("comment".to_string(), Value::Str(comment.clone()));
    }
    Ok(flow)
}

fn parse_single_line<'i>(
    padded: &'i str,
    line_no: usize,
    state: &State,
) -> Result<Pair<'i, Rule>, ParsingError> {
    let parsed = NCParser::parse(Rule::line_entry, padded).map_err(|e| {
        let preview = state.get_line(line_no).unwrap_or("(could not retrieve line)").to_string();
        ParsingError::with_context(
            line_no,
            preview,
            "line parsing".to_string(),
            crate::interpreter::describe_parse_error(&e),
        )
    })?;
    parsed
        .flatten()
        .find(|p| p.as_rule() == Rule::block)
        .ok_or_else(|| ParsingError::ParseError {
            message: format!("Line {} produced no block", line_no),
        })
}

enum DecodeResult {
    Trivial(DecodedLine, Option<String>),
    Blank,
    NeedsGrammar,
}

/// Byte-level scanner for trivially decodable lines. Conservative: any
/// construct beyond `N<d>`, `LABEL:`, `LETTER<num>`, `ident=<num>`,
/// `M<d>`, a vocabulary-known bare keyword and a trailing comment rejects
/// the line to the full grammar.
fn decode_line(line: &str, line_no: usize, state: &State) -> DecodeResult {
    let bytes = line.as_bytes();
    let n_len = bytes.len();
    let mut i = 0;
    let mut words: Vec<Word> = Vec::new();
    let mut n: Option<String> = None;
    let mut label: Option<String> = None;
    let mut comment: Option<String> = None;

    let skip_ws = |i: &mut usize| {
        while *i < n_len && (bytes[*i] == b' ' || bytes[*i] == b'\t') {
            *i += 1;
        }
    };

    fn parse_number(line: &str, i: &mut usize) -> Option<f64> {
        let bytes = line.as_bytes();
        let start = *i;
        if *i < bytes.len() && bytes[*i] == b'-' {
            *i += 1;
        }
        let digits_start = *i;
        while *i < bytes.len() && bytes[*i].is_ascii_digit() {
            *i += 1;
        }
        if *i == digits_start {
            return None;
        }
        if *i < bytes.len() && bytes[*i] == b'.' {
            *i += 1;
            while *i < bytes.len() && bytes[*i].is_ascii_digit() {
                *i += 1;
            }
        }
        // Exponents, a second dot, or letter tails need the real grammar.
        if *i < bytes.len() && (bytes[*i].is_ascii_alphabetic() || bytes[*i] == b'_' || bytes[*i] == b'.') {
            return None;
        }
        line[start..*i].parse::<f64>().ok()
    }

    skip_ws(&mut i);

    // optional block number ("N" is case-sensitive in the grammar)
    if i + 1 < n_len && bytes[i] == b'N' && bytes[i + 1].is_ascii_digit() {
        let start = i + 1;
        i += 1;
        while i < n_len && bytes[i].is_ascii_digit() {
            i += 1;
        }
        n = Some(line[start..i].to_string());
        skip_ws(&mut i);
    }

    // optional jump label NAME: (2..32 chars, first two letters/underscore)
    {
        let word_len = bytes[i..]
            .iter()
            .take_while(|b| b.is_ascii_alphanumeric() || **b == b'_')
            .count();
        if word_len >= 2
            && bytes.get(i + word_len) == Some(&b':')
            && bytes[i..i + 2].iter().all(|b| b.is_ascii_alphabetic() || *b == b'_')
            && word_len <= 32
        {
            label = Some(line[i..i + word_len].to_uppercase());
            i += word_len + 1;
            skip_ws(&mut i);
        }
    }

    while i < n_len {
        if bytes[i] == b';' {
            comment = Some(line[i..].to_string());
            break;
        }
        if !bytes[i].is_ascii_alphabetic() {
            return DecodeResult::NeedsGrammar;
        }
        let word_start = i;
        i += 1;
        let bare_number_follows = i < n_len && (bytes[i].is_ascii_digit() || bytes[i] == b'-' || bytes[i] == b'.');
        // The grammar's bare-word forms: M/G addresses are case-insensitive,
        // but variable_single_char (axis letters) is uppercase-only - a
        // lowercase x100 parses as a subprogram call in the full grammar.
        if bare_number_follows
            && (bytes[word_start].is_ascii_uppercase() || matches!(bytes[word_start], b'm' | b'g'))
        {
            let letter = bytes[word_start].to_ascii_uppercase();
            if letter == b'M' || letter == b'G' {
                // M/G addresses take integer codes and are not axis words.
                let digits_start = i;
                while i < n_len && bytes[i].is_ascii_digit() {
                    i += 1;
                }
                if i == digits_start
                    || (i < n_len && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_' || bytes[i] == b'.'))
                {
                    return DecodeResult::NeedsGrammar;
                }
                let code = &line[word_start..i];
                if letter == b'M' {
                    words.push(Word::MCode(code.to_string()));
                } else {
                    match classify_g_command(&code.to_uppercase()) {
                        Some((group, _modal)) => words.push(Word::GCommand(group, code.to_string())),
                        // Unknown G code: let the full path produce its error.
                        None => return DecodeResult::NeedsGrammar,
                    }
                }
            } else {
                // Axis-address letters: exactly the grammar's
                // variable_single_char set. Anything else needs the grammar.
                if !matches!(letter, b'X' | b'Y' | b'Z' | b'A' | b'B' | b'C' | b'U' | b'V' | b'W' | b'I' | b'J' | b'K' | b'T' | b'S' | b'F' | b'D' | b'H' | b'E')
                {
                    return DecodeResult::NeedsGrammar;
                }
                let Some(value) = parse_number(line, &mut i) else {
                    return DecodeResult::NeedsGrammar;
                };
                // The grammar's axis_word rejects a following '[' (arrays).
                if i < n_len && bytes[i] == b'[' {
                    return DecodeResult::NeedsGrammar;
                }
                let key = (bytes[word_start] as char).to_ascii_uppercase().to_string();
                words.push(Word::Assign(key, value));
            }
        } else {
            // Multi-character word: keyword command or ident=<number>.
            while i < n_len && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                i += 1;
            }
            let word = &line[word_start..i];
            if i < n_len && bytes[i] == b'=' {
                i += 1;
                let Some(value) = parse_number(line, &mut i) else {
                    return DecodeResult::NeedsGrammar;
                };
                // Case normalization for axes and block addresses mirrors
                // normalize_reserved_case; other identifiers keep their case.
                let key = if state.is_axis(word) || state.is_block_address(word) {
                    word.to_uppercase()
                } else {
                    word.to_string()
                };
                words.push(Word::Assign(key, value));
            } else if i < n_len && !(bytes[i] == b' ' || bytes[i] == b'\t' || bytes[i] == b';') {
                // '(', '[', quotes, operators...: full grammar.
                return DecodeResult::NeedsGrammar;
            } else {
                match classify_g_command(&word.to_uppercase()) {
                    // Frame keywords must go through frame_op / the frame
                    // guard; TRUE/FALSE etc. are not commands. Only claim
                    // plain keyword commands.
                    Some((group, _modal))
                        if !crate::interpret_rules::FRAME_KEYWORDS
                            .iter()
                            .any(|kw| kw.eq_ignore_ascii_case(word)) =>
                    {
                        words.push(Word::GCommand(group, word.to_string()));
                    }
                    // Unknown bare words are subprogram calls, reserved
                    // words are control flow - both take the full grammar.
                    _ => return DecodeResult::NeedsGrammar,
                }
            }
        }
        skip_ws(&mut i);
    }

    // A line whose only content is a jump label produces no output row
    // (matching the whole-file path, where the empty row is pruned), but it
    // still exists as a jump target.
    let row_content = n.is_some() || comment.is_some() || !words.is_empty();
    if !row_content && label.is_none() {
        return DecodeResult::Blank;
    }
    let decoded = DecodedLine {
        line_no,
        n,
        comment,
        words,
        has_content: row_content,
    };
    DecodeResult::Trivial(decoded, label)
}

/// Escape hatch for differential testing and debugging: NC_STAGE1=0
/// disables the fast path so everything runs through the whole-file parse.
pub fn stage1_enabled() -> bool {
    !std::env::var("NC_STAGE1")
        .is_ok_and(|v| v == "0" || v.eq_ignore_ascii_case("off"))
}
