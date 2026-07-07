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
use crate::state::{ColKind, State};
use crate::types::{NCParser, Pair, Rule, Value};
use pest::Parser;
use rayon::prelude::*;
use std::collections::HashMap;
use std::ops::Range;

/// Below this many bytes the parallel decode's thread hand-off and per-chunk
/// stitching cost more than the sequential scan saves, so small programs (all
/// of the test suite, most hand-written files) stay single-threaded.
const PARALLEL_MIN_BYTES: usize = 4 << 20; // 4 MiB

type Output = crate::output::OutputRows;

/// Minimum non-blank lines to observe before the low-trivial-ratio guard may
/// decline the fast path. Small programs pay negligible per-line overhead
/// either way, so the guard stays out of their path; the warm-up also stops a
/// handful of grammar-bound header lines from declining an otherwise trivial
/// program.
const RATIO_SAMPLE_MIN: usize = 2048;

/// One word of a decoded trivial line, in source order.
///
/// Every string field borrows a `&'a str` span of the program buffer, so pass
/// 1 does no per-word allocation. Keys are stored exactly as written; the
/// axis/block-address case normalization (`normalize_reserved_case`) is applied
/// at execution time (`normalize_key`), which keeps the pass-1 decode free of
/// the `to_uppercase` allocations `State::is_axis`/`is_block_address` perform.
///
/// Every field is `Copy` (`&str`, `f64`, `bool`), so `Word` is `Copy` and the
/// parallel decode can concatenate per-chunk arenas with a plain memcpy.
#[derive(Clone, Copy)]
enum Word<'a> {
    /// `X12.5` or `AXIS=12.5`: routed to axis / block address / variable
    /// exactly like `interpret_assignment` would.
    Assign(&'a str, f64),
    /// `KEY=IC(IDENT*NUMBER)` / `KEY=IC(NUMBER*IDENT)` / `KEY=IC(NUMBER)`
    /// (incremental), or the same product forms in plain parentheses,
    /// `KEY=(IDENT*NUMBER)` etc. (absolute): a deferred word. The value is
    /// `value(ident) * factor` (or just `factor` when there is no ident),
    /// evaluated at execution time so a mid-program reassignment of `ident`
    /// is honored; incremental words then apply IC() semantics exactly like
    /// `interpret_axis_increment`. CAM extrusion output is full of
    /// `E=IC(E_MULTIPLIER*0.02166)` and `F=(F_MULTI*1314)`; claiming these
    /// keeps assignment-heavy programs on the fast path.
    AssignDynamic {
        key: &'a str,
        ident: Option<&'a str>,
        factor: f64,
        incremental: bool,
    },
    /// A bare identifier that is neither a vocabulary command, a frame or
    /// reserved keyword, nor a word operator: a parameterless subprogram
    /// call, stored exactly like `interpret_non_returning_function_call`.
    /// `value` preserves the trailing whitespace that pest's span includes;
    /// `typo_warn` mirrors `looks_like_axis_word_typo` and is emitted at
    /// execution time (like the pest path, once per execution, never for
    /// lines that are decoded but not run).
    Call { value: &'a str, typo_warn: bool },
    /// A vocabulary-known keyword or `G<digits>` command: (group, as written).
    GCommand(&'static str, &'a str),
    /// An M code, as written.
    MCode(&'a str),
}

/// A decoded trivial line. `words` is a half-open range into this line's chunk
/// arena (the chunk is recorded on the `LineExec`), not an owned `Vec`, so a
/// line adds no heap allocation of its own.
struct DecodedLine<'a> {
    line_no: usize,
    n: Option<&'a str>,
    comment: Option<&'a str>,
    words: Range<usize>,
    /// The line defines a jump label (recorded in the target index only).
    has_content: bool,
}

enum LineExec<'a, 'i> {
    /// A decoded trivial line plus the index of the chunk arena its `words`
    /// range indexes into.
    Decoded(DecodedLine<'a>, usize),
    /// A pest-parsed block. The line was parsed with `line_entry` padded
    /// with leading newlines so that all positions (and therefore error
    /// messages) are correct in whole-file coordinates.
    Parsed(Pair<'i, Rule>),
    Blank,
}

/// One type per name (mirrors `interpret_assignment`): assigning a number to
/// an existing STRING variable is a hard error, also on the fast path.
fn reject_string_variable(key: &str, state: &State, line_no: usize) -> Result<(), ParsingError> {
    if state.string_table.contains_key(key) {
        let preview = state.get_line(line_no).unwrap_or("").to_string();
        return Err(ParsingError::with_context(
            line_no,
            preview,
            "assignment".to_string(),
            format!("'{key}' is a STRING variable; it cannot be assigned a number"),
        ));
    }
    Ok(())
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
    // Pass 1: decode every line. Words borrow `&str` spans of `input` and are
    // appended to per-chunk arenas; each decoded line only records its
    // half-open range into its chunk's arena, so pass 1 does no per-line/
    // per-word heap allocation. Decode is a pure function of the line text (no
    // shared state), so large programs decode their line chunks in parallel;
    // the chunks are never merged (see `decode_all_lines`).
    let stats = std::env::var("NC_STAGE1_STATS").is_ok();
    let t_pass1 = std::time::Instant::now();
    let lines: Vec<&str> = input.lines().collect();
    let (chunk_arenas, decode_results) = decode_all_lines(&lines, input.len());

    // Low-trivial-ratio guard. Every grammar-bound line costs a per-line pest
    // parse (plus its byte-decode probe and newline padding), which is strictly
    // more work than the same line in one whole-file parse; the fast path only
    // wins when the trivial lines it decodes cheaply dominate. Empirically the
    // wall-time crossover on assignment-heavy CAM output sits well under 50%
    // grammar-bound lines, so declining above 50% only sheds clear losers while
    // leaving trivial-heavy programs (>99% trivial in practice) untouched.
    // Sample a warm-up prefix before judging so a few early oddball lines can't
    // trip it.
    //
    // Applied here over the ordered results (a cheap counting scan) rather than
    // inside the decode loop: the parallel decode has no single ordered loop to
    // early-return from, so the decision is deterministic (a pure function of
    // the input, never of thread timing) and byte-for-byte identical to the old
    // in-loop guard - it trips at the very same line. The only difference is
    // that a declining program is now fully decoded before it declines; those
    // are rare (grammar-heavy) and fall back to the whole-file parse anyway.
    let mut non_blank: usize = 0;
    let mut needs_grammar: usize = 0;
    let mut padding_bytes: usize = 0;
    for (index, result) in decode_results.iter().flatten().enumerate() {
        match result {
            DecodeResult::NeedsGrammar => {
                padding_bytes += index;
                needs_grammar += 1;
                non_blank += 1;
            }
            DecodeResult::Trivial(..) => non_blank += 1,
            DecodeResult::Blank => {}
        }
        if non_blank >= RATIO_SAMPLE_MIN && needs_grammar * 2 > non_blank {
            return Ok(None);
        }
    }

    // Debug hook: NC_STAGE1_STATS=1 reports how much of the program the
    // byte decoder claims (used to diagnose fast-path pessimizations on
    // real CAM files).
    if stats {
        let words: usize = chunk_arenas.iter().map(|a| a.len()).sum();
        eprintln!(
            "STAGE1_STATS non_blank={} needs_grammar={} total_lines={} pass1_decode={:.3}s words={} nchunks={}",
            non_blank, needs_grammar, lines.len(), t_pass1.elapsed().as_secs_f64(), words, chunk_arenas.len()
        );
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
    for (index, result) in decode_results.iter().flatten().enumerate() {
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
    let mut execs: Vec<LineExec> = Vec::with_capacity(lines.len());
    // Iterate chunk by chunk so each decoded line carries the index of the
    // chunk arena its `words` range points into; `index` stays the global line
    // number (chunks are contiguous and in order).
    let mut index = 0usize;
    for (chunk_idx, chunk) in decode_results.into_iter().enumerate() {
        for result in chunk {
            match result {
                DecodeResult::Trivial(decoded, label) => {
                    if let Some(n) = decoded.n {
                        targets
                            .entry(format!("N:{}", canonical_block_number(n)))
                            .or_default()
                            .push(index);
                    }
                    if let Some(label) = label {
                        targets.entry(format!("LABEL:{}", label)).or_default().push(index);
                    }
                    execs.push(LineExec::Decoded(decoded, chunk_idx));
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
            index += 1;
        }
    }

    state.seen_jump_targets.extend(targets.keys().cloned());
    state.jump_scopes.push(targets.keys().cloned().collect());
    if stats {
        eprintln!("STAGE1_STATS pass2_setup_done={:.3}s (pass1+pass2, before execution)", t_pass1.elapsed().as_secs_f64());
    }
    let result = run_lines(&execs, &chunk_arenas, &targets, output, state);
    state.jump_scopes.pop();
    result.map(Some)
}

/// Decode every line into per-chunk word arenas and per-chunk result lists.
/// Dispatches to the parallel path for large programs. Each chunk keeps its own
/// arena (no global merge - that copy is pure memory-bandwidth cost and would
/// eat the parallel decode's gains); a decoded line's word range is local to
/// its chunk, and `LineExec::Decoded` records which chunk to slice at run time.
/// Chunks keep source order (rayon's indexed `par_chunks`/`collect`), so the
/// flattened result stream is byte-for-byte what the sequential decode
/// produces. The sequential path is just the single-chunk case.
fn decode_all_lines<'a>(
    lines: &[&'a str],
    input_bytes: usize,
) -> (Vec<Vec<Word<'a>>>, Vec<Vec<DecodeResult<'a>>>) {
    if input_bytes < PARALLEL_MIN_BYTES || rayon::current_num_threads() <= 1 {
        let mut arena: Vec<Word> = Vec::new();
        let mut results: Vec<DecodeResult> = Vec::with_capacity(lines.len());
        for (index, line) in lines.iter().enumerate() {
            results.push(decode_line(line, index + 1, &mut arena));
        }
        (vec![arena], vec![results])
    } else {
        let threads = rayon::current_num_threads().max(1);
        let chunk_size = (lines.len() / (threads * 8)).max(4096);
        lines
            .par_chunks(chunk_size)
            .enumerate()
            .map(|(ordinal, chunk)| {
                // `enumerate` gives the chunk ordinal, so the first global line
                // index in this chunk is `ordinal * chunk_size` (only the last
                // chunk is short). Reserve the chunk arena up front (CAM output
                // averages ~4 words per line): one allocation instead of ~20
                // doublings keeps the workers off the global allocator lock,
                // which otherwise serializes the parallel decode.
                let base_line = ordinal * chunk_size;
                let mut arena: Vec<Word> = Vec::with_capacity(chunk.len() * 4);
                let mut results: Vec<DecodeResult> = Vec::with_capacity(chunk.len());
                for (j, line) in chunk.iter().enumerate() {
                    results.push(decode_line(line, base_line + j + 1, &mut arena));
                }
                (arena, results)
            })
            .unzip()
    }
}

fn run_lines(
    execs: &[LineExec],
    chunk_arenas: &[Vec<Word>],
    targets: &HashMap<String, Vec<usize>>,
    output: &mut Output,
    state: &mut State,
) -> Result<BlockFlow, ParsingError> {
    let mut index = 0;
    let mut jumps_taken = 0;
    while index < execs.len() {
        let flow = match &execs[index] {
            LineExec::Blank => BlockFlow::Continue,
            LineExec::Decoded(line, chunk_idx) => {
                execute_decoded(line, &chunk_arenas[*chunk_idx], output, state)?
            }
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
fn execute_decoded(line: &DecodedLine, arena: &[Word], output: &mut Output, state: &mut State) -> Result<BlockFlow, ParsingError> {
    if !line.has_content {
        return Ok(BlockFlow::Continue);
    }
    output.start_row(line.line_no)?;
    let mut flow = BlockFlow::Continue;
    // Split borrows: row insertion vs axis-state updates.
    for word in &arena[line.words.clone()] {
        match word {
            Word::Assign(key, value) => {
                match state.resolve_output_key(key) {
                    Some((ColKind::Axis, skey)) => {
                        let machine_value = state.update_axis(skey, *value)?;
                        let last = output.last_mut().expect("row was just pushed");
                        last.insert(skey, Value::Float(machine_value));
                    }
                    Some((ColKind::Block, skey)) => {
                        let last = output.last_mut().expect("row was just pushed");
                        last.insert(skey, Value::Float(*value));
                    }
                    None => {
                        state.warn_unsupported_address(key, line.line_no);
                        // Same one-type-per-name rule as interpret_assignment:
                        // a STRING variable must not silently become numeric.
                        reject_string_variable(key, state, line.line_no)?;
                        output.record_variable_change(key, *value);
                        state.symbol_table.insert(key.to_string(), *value);
                    }
                }
            }
            Word::AssignDynamic { key, ident, factor, incremental } => {
                // Evaluate `value(ident) * factor` with the exact lookup and
                // undefined-variable behavior of interpret_primary.
                let value = match *ident {
                    Some(name) => {
                        let ident_value = match state.symbol_table.get(name).copied() {
                            Some(v) => v,
                            None if state.allow_undefined_variables => {
                                crate::state::emit_warning(format_args!("Warning: Variable '{}' is undefined, initializing to 0.0", name));
                                state.symbol_table.insert(name.to_string(), 0.0);
                                0.0
                            }
                            None => {
                                let preview = state
                                    .get_line(line.line_no)
                                    .unwrap_or("(could not retrieve line)")
                                    .to_string();
                                return Err(ParsingError::UndefinedVariable {
                                    line_no: line.line_no,
                                    preview,
                                    name: name.to_string(),
                                });
                            }
                        };
                        ident_value * *factor
                    }
                    None => *factor,
                };
                // IC(): new LOCAL coordinate = current local + increment (or the
                // bare increment, with a warning, if never set) - exactly
                // interpret_axis_increment. The lookup name is the resolved key
                // (uppercased interned name for axes/blocks, as-written for
                // variables), matching the pre-interning normalize_key behavior.
                let increment_local = |state: &State, name: &str, value: f64| -> f64 {
                    if !*incremental {
                        return value;
                    }
                    match state.get_axis_local(name) {
                        Some(local) => local + value,
                        None => {
                            crate::state::emit_warning(format_args!(
                                "Warning: The axis '{}' is incremented before a fixed value is set, the G-code behavior may be indeterminate.",
                                name
                            ));
                            value
                        }
                    }
                };
                match state.resolve_output_key(key) {
                    Some((ColKind::Axis, skey)) => {
                        let local_value = increment_local(state, skey, value);
                        let machine_value = state.update_axis(skey, local_value)?;
                        let last = output.last_mut().expect("row was just pushed");
                        last.insert(skey, Value::Float(machine_value));
                    }
                    Some((ColKind::Block, skey)) => {
                        let local_value = increment_local(state, skey, value);
                        let last = output.last_mut().expect("row was just pushed");
                        last.insert(skey, Value::Float(local_value));
                    }
                    None => {
                        state.warn_unsupported_address(key, line.line_no);
                        reject_string_variable(key, state, line.line_no)?;
                        let local_value = increment_local(state, key, value);
                        output.record_variable_change(key, local_value);
                        state.symbol_table.insert(key.to_string(), local_value);
                    }
                }
            }
            Word::Call { value, typo_warn } => {
                if *typo_warn {
                    crate::state::emit_warning(format_args!(
                        "Warning: '{}' (line {}) is interpreted as a subprogram call; did you mean one or more axis words (e.g. Y20)?",
                        value.trim(),
                        line.line_no
                    ));
                }
                let last = output.last_mut().expect("row was just pushed");
                last.insert("non_returning_function_call", Value::Str(value.to_string()));
            }
            Word::GCommand(group, as_written) => {
                let last = output.last_mut().expect("row was just pushed");
                // Uppercase like the grammar path: g2 IS G2.
                last.insert(group, Value::Str(as_written.to_uppercase()));
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
    if let Some(n) = line.n {
        last.insert("N", Value::Str(n.to_string()));
    }
    if let Some(comment) = line.comment {
        last.insert("comment", Value::Str(comment.to_string()));
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

enum DecodeResult<'a> {
    Trivial(DecodedLine<'a>, Option<String>),
    Blank,
    NeedsGrammar,
}

/// Parse a single numeric literal (`value` in the grammar: optional sign,
/// digits, optional fraction) at `*i`, advancing past it. Rejects exponents,
/// a second dot, or letter/underscore tails - those need the real grammar.
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

/// Control-flow keywords the grammar's `reserved` rule forbids as an
/// `identifier`. A word that collides with one of these is not a variable or
/// a call in the full grammar (the line takes a control-flow rule or fails
/// to parse), so the fast path must reject it to the grammar.
const RESERVED_WORDS: &[&str] = &[
    "IF", "ELSE", "ENDIF", "GOTOB", "GOTOF", "GOTOC", "GOTOS", "GOTO", "CASE", "OF", "DEFAULT",
    "LOOP", "FOR", "WHILE", "REPEAT", "LABEL", "TO", "UNTIL", "ENDWHILE", "ENDFOR", "ENDLOOP",
];

fn is_reserved_word(word: &str) -> bool {
    RESERVED_WORDS.iter().any(|kw| kw.eq_ignore_ascii_case(word))
}

/// First byte of the grammar's `identifier` rule (ASCII_ALPHA or `_`,
/// matching the leading-underscore support in grammar.pest).
fn is_ident_start(b: u8) -> bool {
    b.is_ascii_alphabetic() || b == b'_'
}

/// Decode the exact `IDENT*NUMBER`, `NUMBER*IDENT` or `NUMBER` product
/// shapes followed by `)`. `start` points just past the opening `(`. On a
/// match returns `(ident, factor, end)` where `end` is just past the `)`;
/// the value evaluates to `value(ident) * factor` (or `factor` alone when
/// `ident` is `None`). Anything else returns `None` so the caller falls back
/// to the full grammar - conservative by construction (never `IDENT*IDENT`,
/// nested parens, extra terms, or reserved-word idents).
fn decode_paren_product(line: &str, start: usize) -> Option<(Option<&str>, f64, usize)> {
    let bytes = line.as_bytes();
    let n = bytes.len();
    let mut i = start;
    let skip_ws = |i: &mut usize| {
        while *i < n && (bytes[*i] == b' ' || bytes[*i] == b'\t') {
            *i += 1;
        }
    };
    // Read an identifier: identifier-start ~ (ASCII_ALPHANUMERIC | "_")*.
    let read_ident = |i: &mut usize| -> Option<&str> {
        if *i >= n || !is_ident_start(bytes[*i]) {
            return None;
        }
        let ident_start = *i;
        *i += 1;
        while *i < n && (bytes[*i].is_ascii_alphanumeric() || bytes[*i] == b'_') {
            *i += 1;
        }
        let ident = &line[ident_start..*i];
        if is_reserved_word(ident) {
            return None;
        }
        Some(ident)
    };

    skip_ws(&mut i);
    let (ident, factor) = if i < n && is_ident_start(bytes[i]) {
        // IDENT * NUMBER
        let ident = read_ident(&mut i)?;
        skip_ws(&mut i);
        if i >= n || bytes[i] != b'*' {
            return None;
        }
        i += 1;
        skip_ws(&mut i);
        let factor = parse_number(line, &mut i)?;
        (Some(ident), factor)
    } else {
        // NUMBER, optionally NUMBER * IDENT
        let number = parse_number(line, &mut i)?;
        skip_ws(&mut i);
        if i < n && bytes[i] == b'*' {
            i += 1;
            skip_ws(&mut i);
            let ident = read_ident(&mut i)?;
            (Some(ident), number)
        } else {
            (None, number)
        }
    };
    skip_ws(&mut i);
    if i >= n || bytes[i] != b')' {
        return None;
    }
    i += 1;
    Some((ident, factor, i))
}

/// Decode `IC(<product>)` - the incremental variant of the product shapes.
/// `start` points just past the `=`.
fn decode_ic_word(line: &str, start: usize) -> Option<(Option<&str>, f64, usize)> {
    let bytes = line.as_bytes();
    // "IC" is case-insensitive in the grammar (^"IC"); need at least "IC(".
    if start + 3 > bytes.len()
        || !bytes[start].eq_ignore_ascii_case(&b'I')
        || !bytes[start + 1].eq_ignore_ascii_case(&b'C')
        || bytes[start + 2] != b'('
    {
        return None;
    }
    decode_paren_product(line, start + 3)
}

/// Constant-fold a numeric expression (`NUMBER (op NUMBER)*` with `+ - * /`
/// and unary minus) at `*i`, advancing past it. Folding is safe at decode
/// time because every operand is a literal; the evaluation order replicates
/// `evaluate_additive` / `evaluate_multiplicative` / `evaluate_unary`
/// bit-for-bit (left-associative, `* /` binding tighter than `+ -`). The
/// word operators DIV and MOD, parentheses, variables and anything else stop
/// the fold; if the fold stops mid-operator the caller's next-token scan
/// rejects the line to the grammar.
fn fold_const_expr(line: &str, i: &mut usize) -> Option<f64> {
    let bytes = line.as_bytes();
    let n = bytes.len();
    let skip_ws = |i: &mut usize| {
        while *i < n && (bytes[*i] == b' ' || bytes[*i] == b'\t') {
            *i += 1;
        }
    };
    // unary: prefix neg* ~ number. The grammar's prefix rule consumes the
    // '-' signs (whitespace-separated); parse_number also accepts a single
    // leading '-', which yields the identical f64 (decimal parsing is
    // sign-magnitude).
    fn unary(line: &str, i: &mut usize, skip_ws: &impl Fn(&mut usize)) -> Option<f64> {
        let bytes = line.as_bytes();
        let mut sign = 1.0f64;
        loop {
            skip_ws(i);
            // A '-' directly starting a number is consumed by parse_number;
            // one followed by whitespace or another '-' is a prefix neg.
            if *i < bytes.len()
                && bytes[*i] == b'-'
                && !(*i + 1 < bytes.len() && (bytes[*i + 1].is_ascii_digit() || bytes[*i + 1] == b'.'))
            {
                sign = -sign;
                *i += 1;
            } else {
                break;
            }
        }
        parse_number(line, i).map(|v| sign * v)
    }
    let multiplicative = |i: &mut usize| -> Option<f64> {
        let mut lhs = unary(line, i, &skip_ws)?;
        loop {
            let mark = *i;
            skip_ws(i);
            match bytes.get(*i) {
                Some(b'*') => {
                    *i += 1;
                    lhs *= unary(line, i, &skip_ws)?;
                }
                Some(b'/') => {
                    *i += 1;
                    lhs /= unary(line, i, &skip_ws)?;
                }
                _ => {
                    *i = mark;
                    return Some(lhs);
                }
            }
        }
    };
    let mut lhs = multiplicative(i)?;
    loop {
        let mark = *i;
        skip_ws(i);
        match bytes.get(*i) {
            Some(b'+') => {
                *i += 1;
                lhs += multiplicative(i)?;
            }
            Some(b'-') => {
                *i += 1;
                lhs -= multiplicative(i)?;
            }
            _ => {
                *i = mark;
                return Some(lhs);
            }
        }
    }
}

/// Byte-level scanner for trivially decodable lines. Conservative: any
/// construct beyond `N<d>`, `LABEL:`, `LETTER<num>`, `ident=<const expr>`,
/// `ident=IC(<product>)`, `ident=(<product>)`, `M<d>`, a vocabulary-known
/// bare keyword, a bare parameterless subprogram call and a trailing comment
/// rejects the line to the full grammar.
fn decode_line<'a>(
    line: &'a str,
    line_no: usize,
    arena: &mut Vec<Word<'a>>,
) -> DecodeResult<'a> {
    // Words are appended to the shared arena as they decode. A line that turns
    // out to need the grammar (or is blank) leaves no trace: roll the arena
    // back to where this line started so only claimed trivial lines contribute.
    let words_start = arena.len();
    let result = decode_line_inner(line, line_no, words_start, arena);
    if !matches!(result, DecodeResult::Trivial(..)) {
        arena.truncate(words_start);
    }
    result
}

fn decode_line_inner<'a>(
    line: &'a str,
    line_no: usize,
    words_start: usize,
    arena: &mut Vec<Word<'a>>,
) -> DecodeResult<'a> {
    let bytes = line.as_bytes();
    let n_len = bytes.len();
    let mut i = 0;
    let mut n: Option<&'a str> = None;
    let mut label: Option<String> = None;
    let mut comment: Option<&'a str> = None;

    let skip_ws = |i: &mut usize| {
        while *i < n_len && (bytes[*i] == b' ' || bytes[*i] == b'\t') {
            *i += 1;
        }
    };

    skip_ws(&mut i);

    // optional block number ("N" is case-sensitive in the grammar)
    if i + 1 < n_len && bytes[i] == b'N' && bytes[i + 1].is_ascii_digit() {
        let start = i + 1;
        i += 1;
        while i < n_len && bytes[i].is_ascii_digit() {
            i += 1;
        }
        n = Some(&line[start..i]);
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
            comment = Some(&line[i..]);
            break;
        }
        // Identifiers (and axis/keyword words) start with a letter or, for
        // user variables, an underscore - mirroring the grammar's `identifier`
        // rule. A leading-underscore word is always a multi-char identifier
        // (never an axis letter or M/G address), so it flows to the else-branch.
        if !(bytes[i].is_ascii_alphabetic() || bytes[i] == b'_') {
            return DecodeResult::NeedsGrammar;
        }
        let word_start = i;
        i += 1;
        let bare_number_follows = i < n_len && (bytes[i].is_ascii_digit() || bytes[i] == b'-' || bytes[i] == b'.');
        // The grammar's bare-word forms are all case-insensitive: M/G
        // addresses and variable_single_char (axis letters) alike.
        if bare_number_follows && bytes[word_start].is_ascii_alphabetic() {
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
                    arena.push(Word::MCode(code));
                } else {
                    match classify_g_command(&code.to_uppercase()) {
                        Some((group, _modal)) => arena.push(Word::GCommand(group, code)),
                        // Unknown G code: let the full path produce its error.
                        None => return DecodeResult::NeedsGrammar,
                    }
                }
            } else {
                // Axis-address letters: exactly the grammar's
                // variable_single_char set (normalized to the uppercase
                // column key). Anything else needs the grammar.
                let key = match letter {
                    b'X' => "X", b'Y' => "Y", b'Z' => "Z",
                    b'A' => "A", b'B' => "B", b'C' => "C",
                    b'U' => "U", b'V' => "V", b'W' => "W",
                    b'I' => "I", b'J' => "J", b'K' => "K",
                    b'T' => "T", b'S' => "S", b'F' => "F",
                    b'D' => "D", b'H' => "H", b'E' => "E",
                    _ => return DecodeResult::NeedsGrammar,
                };
                let Some(value) = parse_number(line, &mut i) else {
                    return DecodeResult::NeedsGrammar;
                };
                // The grammar's axis_word rejects a following '[' (arrays).
                if i < n_len && bytes[i] == b'[' {
                    return DecodeResult::NeedsGrammar;
                }
                arena.push(Word::Assign(key, value));
            }
        } else {
            // Multi-character word: keyword command, ident=<expr> or a call.
            while i < n_len && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                i += 1;
            }
            let word = &line[word_start..i];
            // The grammar's implicit WHITESPACE allows spaces around '='
            // (`E_MULTIPLIER  = 1`); peek past them without committing.
            let mut eq = i;
            skip_ws(&mut eq);
            if eq < n_len && bytes[eq] == b'=' {
                // A reserved word is not an `identifier`, so `IF=5` is a
                // parse error in the full grammar, not an assignment.
                if is_reserved_word(word) {
                    return DecodeResult::NeedsGrammar;
                }
                i = eq + 1;
                skip_ws(&mut i);
                // Store the key exactly as written; the axis/block-address
                // uppercasing (`normalize_reserved_case`) is deferred to
                // `normalize_key` at execution time so pass 1 never calls the
                // allocating `is_axis`/`is_block_address` here.
                let key = word;
                if let Some((ident, factor, end)) = decode_ic_word(line, i) {
                    // KEY=IC(...): deferred incremental word.
                    i = end;
                    arena.push(Word::AssignDynamic { key, ident, factor, incremental: true });
                } else if i < n_len && bytes[i] == b'(' {
                    // KEY=(IDENT*NUMBER) etc.: deferred absolute word.
                    let Some((ident, factor, end)) = decode_paren_product(line, i + 1) else {
                        return DecodeResult::NeedsGrammar;
                    };
                    i = end;
                    arena.push(Word::AssignDynamic { key, ident, factor, incremental: false });
                } else {
                    // KEY=<constant expression>: fold literals at decode time
                    // (identical evaluation order, see fold_const_expr).
                    let Some(value) = fold_const_expr(line, &mut i) else {
                        return DecodeResult::NeedsGrammar;
                    };
                    arena.push(Word::Assign(key, value));
                }
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
                        arena.push(Word::GCommand(group, word));
                    }
                    Some(_) => return DecodeResult::NeedsGrammar,
                    None => {
                        // A bare unknown word is a parameterless subprogram
                        // call in the full grammar - unless it is reserved
                        // (control flow), a frame keyword (frame_op at block
                        // start, a loud error mid-block), DEF (opens a
                        // definition), or the word operators DIV/MOD (which
                        // would continue a preceding expression, e.g.
                        // `X=5 DIV R1`); those take the full grammar.
                        if is_reserved_word(word)
                            || crate::interpret_rules::FRAME_KEYWORDS
                                .iter()
                                .any(|kw| kw.eq_ignore_ascii_case(word))
                            || word.eq_ignore_ascii_case("DEF")
                            || word.eq_ignore_ascii_case("DIV")
                            || word.eq_ignore_ascii_case("MOD")
                        {
                            return DecodeResult::NeedsGrammar;
                        }
                        // pest's non_returning_function_call span includes
                        // the whitespace skipped while attempting the
                        // optional argument list; replicate it in the value.
                        let mut ws_end = i;
                        skip_ws(&mut ws_end);
                        let value = &line[word_start..ws_end];
                        i = ws_end;
                        let typo_warn = crate::interpret_rules::looks_like_axis_word_typo(word);
                        arena.push(Word::Call { value, typo_warn });
                    }
                }
            }
        }
        skip_ws(&mut i);
    }

    // A line whose only content is a jump label produces no output row
    // (matching the whole-file path, where the empty row is pruned), but it
    // still exists as a jump target.
    let has_words = arena.len() > words_start;
    let row_content = n.is_some() || comment.is_some() || has_words;
    if !row_content && label.is_none() {
        return DecodeResult::Blank;
    }
    let decoded = DecodedLine {
        line_no,
        n,
        comment,
        words: words_start..arena.len(),
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

/// Differential tests: every program must produce identical output with the
/// fast path enabled and disabled - the Rust twin of
/// python/tests/test_stage1.py, focused on the deferred-word (IC/paren
/// product), call, constant-expression and ratio-decline forms.
#[cfg(test)]
mod tests {
    use crate::errors::ParsingError;
    use crate::interpreter::nc_to_table;
    use crate::output::{Column, Table};
    use std::collections::HashMap;
    use std::sync::Mutex;

    /// NC_STAGE1 is process-global; serialize every test that flips it.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn run(
        program: &str,
        stage1: bool,
        allow_undefined: bool,
    ) -> Result<(Table, crate::state::State), ParsingError> {
        std::env::set_var("NC_STAGE1", if stage1 { "1" } else { "0" });
        let result = nc_to_table(
            program,
            None,
            None,
            Some(vec!["ELX".to_string()]),
            10_000,
            false,
            Some(HashMap::from([("E".to_string(), 4)])),
            allow_undefined,
            None,
        );
        std::env::remove_var("NC_STAGE1");
        result
    }

    fn sorted_columns(table: &Table) -> Vec<(String, Column)> {
        let mut cols = table.columns.clone();
        cols.sort_by(|a, b| a.0.cmp(&b.0));
        cols
    }

    fn env_lock() -> std::sync::MutexGuard<'static, ()> {
        // A panic in another test only poisons the mutex; the env var is
        // always restored, so the lock is still safe to reuse.
        ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Run with the fast path off (reference) and on; assert identical
    /// tables (column order ignored, like the python harness) and state.
    fn assert_paths_agree(program: &str, allow_undefined: bool) {
        let _guard = env_lock();
        let full = run(program, false, allow_undefined);
        let fast = run(program, true, allow_undefined);
        match (full, fast) {
            (Ok((full_table, full_state)), Ok((fast_table, fast_state))) => {
                assert_eq!(
                    sorted_columns(&full_table),
                    sorted_columns(&fast_table),
                    "table mismatch for program:\n{program}"
                );
                assert_eq!(full_state.symbol_table, fast_state.symbol_table, "symbols for:\n{program}");
                assert_eq!(full_state.axes, fast_state.axes, "axes for:\n{program}");
                assert_eq!(full_state.translation, fast_state.translation, "translation for:\n{program}");
            }
            (Err(full_err), Err(fast_err)) => {
                // Both error. Interpretation errors (undefined variable,
                // loop limit, ...) are produced by shared code and match
                // exactly; pure parse errors are worded per entry rule
                // (whole-file vs line_entry), so only require the same line.
                let full_msg = full_err.to_string();
                let fast_msg = fast_err.to_string();
                if full_msg.contains("Parse error") || fast_msg.contains("Parse error") {
                    let line_of = |msg: &str| {
                        msg.lines()
                            .find(|l| l.starts_with("Line:"))
                            .map(str::to_string)
                    };
                    assert_eq!(line_of(&full_msg), line_of(&fast_msg), "parse-error line for:\n{program}");
                } else {
                    assert_eq!(full_msg, fast_msg, "error mismatch for program:\n{program}");
                }
            }
            (Ok(_), Err(e)) => panic!("fast path errored, full path succeeded on:\n{program}\n{e}"),
            (Err(e), Ok(_)) => panic!("full path errored, fast path succeeded on:\n{program}\n{e}"),
        }
    }

    #[test]
    fn ic_product_words_match_full_parse() {
        for program in [
            // the CAM extrusion shape, multiplier reassigned mid-program
            "E_MULTIPLIER = 1\nE=0\nG1 X1 E=IC(E_MULTIPLIER*0.02988) ELX=10335.146\nE_MULTIPLIER = 2\nX2 E=IC(E_MULTIPLIER*0.5)",
            // NUMBER*IDENT and bare NUMBER inside IC
            "M_A = 3\nE=0\nE=IC(0.25*M_A)\nE=IC(2)\nE=IC(-1.5)",
            // IC before the axis has a value (indeterminate-warning path)
            "M_A = 2\nE=IC(M_A*0.5)",
            // IC on a plain variable (stored to the symbol table, like pest)
            "M_A = 2\nSOMEVAR=IC(M_A*3)",
            // IC with whitespace and lowercase ic
            "M_A = 1\nE=0\nE=ic( M_A * 0.5 )",
            // IC under an active TRANS: output must be machine coordinates
            "TRANS E10\nM_A = 1\nE=0\nE=IC(M_A*0.5)",
            // not claimed (IDENT*IDENT, nested, sums): must still agree
            "A_1 = 1\nB_1 = 2\nE=0\nE=IC(A_1*B_1)\nE=IC((A_1)*2)\nE=IC(A_1+1)",
        ] {
            assert_paths_agree(program, false);
        }
    }

    #[test]
    fn ic_undefined_multiplier_matches_full_parse() {
        // Undefined IDENT: identical error without allow_undefined_variables,
        // identical init-to-zero behavior with it.
        assert_paths_agree("E=0\nE=IC(NOT_DEFINED*0.5)", false);
        assert_paths_agree("E=0\nE=IC(NOT_DEFINED*0.5)", true);
    }

    #[test]
    fn paren_product_words_match_full_parse() {
        for program in [
            "F_MULTI = 1\nG1 X1 F=(F_MULTI*1314)\nF_MULTI = 2\nX2 F=(F_MULTI*30000)",
            "F_MULTI = 1.5\nF=(2*F_MULTI)\nF=(2400)",
            "F=(NOT_DEFINED*10)",
        ] {
            assert_paths_agree(program, true);
        }
        assert_paths_agree("F=(NOT_DEFINED*10)", false);
    }

    #[test]
    fn constant_expressions_match_full_parse() {
        for program in [
            // the CAM progress/material shapes
            "MATERIAL_STILL_NEEDED = 149582.2969-17.7451\nPROGRAM_PROGRESS = 100 * 1/2222\nMATERIAL_PROGRESS = 100*38.0828/149582.2969",
            // precedence and unary minus
            "P_1 = 1+2*3\nP_2 = -2*3\nP_3 = 2--3\nP_4 = 2- -3\nP_5 = 10/4/5\nX=1+1",
            // whitespace around '='
            "E_MULTIPLIER  = 1\nCTOL = 1\nX = 5",
            // not claimed (DIV/MOD word operators, variables, parens)
            "P_1 = 7 DIV 4.1\nP_2 = 7 MOD 4\nP_3 = (1+2)*3\nP_4 = P_3*2",
        ] {
            assert_paths_agree(program, false);
        }
        // word operator after a folded expression must not split the line
        // into assignment + call ("X=5 DIV R1" is X = (5 DIV R1) in the
        // grammar); also with an undefined operand (identical errors).
        assert_paths_agree("R1=10\nX=5 DIV R1\nX=5 MOD R1", false);
        assert_paths_agree("X=5 DIV UNSET\nX1", false);
    }

    #[test]
    fn reserved_word_assignment_matches_full_parse() {
        // reserved words are not identifiers: both paths must error alike
        assert_paths_agree("OF=5", false);
        assert_paths_agree("UNTIL=1", false);
    }

    #[test]
    fn bare_calls_match_full_parse() {
        for program in [
            // parameterless subprogram calls, incl. pest's trailing-space span
            "N53 MATERIAL_UPDATE\nN37 EXTRUDER_ON ;(heaters)\nX1 LAYER_CHANGE M3\nRESET_LAYERS\t;tab",
            // lowercase word is a call, uppercase single letter too
            "x100\nX100",
            // axis-word-typo warning path (stderr only; rows must agree)
            "Y2O\nX10Y20",
            // calls that collide with special words are not claimed
            "DEF REAL DEPTH=2.5\nX=DEPTH\nTRANS X10\nX1\nTRANS\nX2",
        ] {
            assert_paths_agree(program, false);
        }
        // frame keyword mid-block: identical UnsupportedStatement error
        assert_paths_agree("G1 CROTS X0", false);
    }

    #[test]
    fn havoc_shaped_program_matches_full_parse() {
        // A miniature of the real Havoc file exercising every new form.
        let program = "\
DEF REAL F_MULTI
N5 X_COMP=0
N10 TOTAL_LAYER_NR = 2222
N33 G507
N35 E_MULTIPLIER  = 1
N36 F_MULTI = 1
N37 EXTRUDER_ON ;(heaters and transport)
N41 STOPRE
N49 MATERIAL_STILL_NEEDED = 149582.2969-17.7451 ;cm3
N51 MATERIAL_PROGRESS = 100*17.7451/149582.2969 ;cm3
N53 MATERIAL_UPDATE
E=0
N55 G1 X347.964 Y-45.000 Z47.965 A0 B0 C0 ELX=10381.483 F=(F_MULTI*30000)
N57 G1 X347.964 Y-45.000 Z-2.035 A0 B0 C0 E=IC(E_MULTIPLIER*0.00000) ELX=10412.522 F=(F_MULTI*1314)
N58 G1 X353.367 Y-45.000 Z-2.035 A0 B0 C0 E=IC(E_MULTIPLIER*0.04602) ELX=10416.787
N80 LAYER_CHANGE
N81 PROGRAM_PROGRESS = 100 * 1/2222
N83 TIMELAPSE_PICTURE
M30
";
        assert_paths_agree(program, true);
    }

    #[test]
    fn low_trivial_ratio_declines_the_fast_path() {
        // >50% grammar-bound lines after the warm-up prefix: the line driver
        // must decline (transparently falling back to the whole-file parse)
        // instead of paying a per-line pest parse for most of the program.
        let lines = super::RATIO_SAMPLE_MIN + 100;
        let program: String =
            (0..lines).map(|_| "X=SIN(30)\n").collect();
        let _guard = env_lock();
        let (table, _state) = run(&program, true, false).expect("interpret");
        let x = table
            .columns
            .iter()
            .find(|(name, _)| name == "X")
            .expect("X column");
        assert_eq!(x.1.len(), lines);
        // Confirm the decline: interpret_lines itself must return None.
        let mut state = crate::state::State::new(vec!["X".to_string()], 10_000, None, false);
        state.set_input(&program);
        let mut padded = Vec::new();
        let mut output = crate::output::OutputRows::collect();
        let declined = super::interpret_lines(&program, &mut padded, &mut output, &mut state)
            .expect("no error");
        assert!(declined.is_none(), "expected the ratio guard to decline");
    }

    #[test]
    fn mostly_trivial_program_stays_on_the_fast_path() {
        // Below the 50% threshold the fast path must claim the program.
        let mut program = String::new();
        for i in 0..(super::RATIO_SAMPLE_MIN * 2) {
            if i % 3 == 0 {
                program.push_str("X=SIN(30)\n"); // grammar-bound (33%)
            } else {
                program.push_str("X1.5 Y2.5\n");
            }
        }
        let mut state = crate::state::State::new(
            vec!["X".to_string(), "Y".to_string()],
            10_000,
            None,
            false,
        );
        state.set_input(&program);
        let mut padded = Vec::new();
        let mut output = crate::output::OutputRows::collect();
        let claimed = super::interpret_lines(&program, &mut padded, &mut output, &mut state)
            .expect("no error");
        assert!(claimed.is_some(), "expected the fast path to claim the program");
    }
}
