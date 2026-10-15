//! Research-only strategies. No stable session API or output reuse is implied.
//! Sources must outlive caches; edits never mutate borrowed source buffers.
use super::*;
use crate::output::{Row, Table};

pub fn fresh_state(input: &str) -> State {
    let mut state = crate::interpreter::build_state(
        None,
        Some(vec!["ELX".into()]),
        10_000,
        axis_index_map(),
        allow_undefined(),
    );
    state.set_input(input);
    state
}

fn finish(flow: BlockFlow, state: &State) -> Result<(), ParsingError> {
    match flow {
        BlockFlow::Jump(request) => Err(request.into_not_found_error(state)),
        _ => Ok(()),
    }
}

/// Whole-file pest cache: measures the benefit and memory cost of keeping an AST.
/// Any source edit rebuilds it; only unchanged reruns reuse parsing.
pub struct GrammarCache<'a> {
    blocks: Pair<'a, Rule>,
}
impl<'a> GrammarCache<'a> {
    pub fn new(source: &'a str) -> Result<Self, String> {
        crate::structure_scan::check_structures(source).map_err(|e| e.to_string())?;
        let file = NCParser::parse(Rule::file, source)
            .map_err(|e| e.to_string())?
            .next()
            .unwrap();
        Ok(Self {
            blocks: file.into_inner().next().unwrap().into(),
        })
    }
    pub fn execute(&self, output: &mut Output, state: &mut State) -> Result<(), ParsingError> {
        let flow = crate::interpret_rules::interpret_blocks(self.blocks.clone(), output, state)?;
        finish(flow, state)
    }
}

struct Block<'a> {
    source: Vec<&'a str>,
    arena: Vec<Word<'a>>,
    lines: Vec<Option<DecodedLine<'a>>>,
}
impl<'a> Block<'a> {
    fn new(source: Vec<&'a str>) -> Result<Self, String> {
        let mut arena = Vec::new();
        let mut lines = Vec::with_capacity(source.len());
        for (i, text) in source.iter().enumerate() {
            match decode_line(text, i + 1, &mut arena) {
                DecodeResult::Blank => lines.push(None),
                DecodeResult::Trivial(line, None) => lines.push(Some(line)),
                _ => {
                    return Err(format!(
                        "decoded-block experiment requires trivial, label-free lines: {text}"
                    ))
                }
            }
        }
        Ok(Self { source, arena, lines })
    }
}

/// Immutable borrowed blocks, patched by line range. Re-decodes boundary blocks
/// and inserted text only; all output is still executed and materialized afresh.
/// Deliberately rejects grammar lines/labels instead of claiming general sessions.
pub struct DecodedCache<'a> {
    blocks: Vec<Block<'a>>,
    block_size: usize,
    pub decoded_lines: usize,
}
impl<'a> DecodedCache<'a> {
    pub fn new(source: &'a str, block_size: usize) -> Result<Self, String> {
        if block_size == 0 {
            return Err("block size must be positive".into());
        }
        let lines: Vec<_> = source.lines().collect();
        Ok(Self {
            blocks: lines
                .chunks(block_size)
                .map(|c| Block::new(c.to_vec()))
                .collect::<Result<_, _>>()?,
            block_size,
            decoded_lines: lines.len(),
        })
    }
    /// Zero-based, half-open source line range. Failed decoding leaves cache intact.
    pub fn replace(&mut self, range: Range<usize>, replacement: &'a str) -> Result<(), String> {
        let total: usize = self.blocks.iter().map(|b| b.source.len()).sum();
        if range.start > range.end || range.end > total {
            return Err("invalid edit range".into());
        }
        let mut base = 0;
        let mut first = self.blocks.len();
        let mut last = first;
        let mut boundary = Vec::new();
        for (i, block) in self.blocks.iter().enumerate() {
            let end = base + block.source.len();
            if end > range.start && base <= range.end {
                if first == self.blocks.len() {
                    first = i;
                    boundary.extend_from_slice(&block.source[..range.start.saturating_sub(base)]);
                    boundary.extend(replacement.lines());
                }
                last = i + 1;
                if range.end < end {
                    boundary.extend_from_slice(&block.source[range.end.saturating_sub(base)..]);
                }
            }
            base = end;
        }
        if first == self.blocks.len() {
            boundary.extend(replacement.lines());
        }
        let blocks = boundary
            .chunks(self.block_size)
            .map(|c| Block::new(c.to_vec()))
            .collect::<Result<Vec<_>, _>>()?;
        self.decoded_lines = boundary.len();
        self.blocks.splice(first..last, blocks);
        Ok(())
    }
    pub fn execute(&self, output: &mut Output, state: &mut State) -> Result<(), ParsingError> {
        let mut base = 0;
        for block in &self.blocks {
            for line in block.lines.iter().flatten() {
                let mut line = line.clone();
                line.line_no += base;
                match execute_decoded(&line, &block.arena, output, state)? {
                    BlockFlow::Continue => (),
                    flow => return finish(flow, state),
                }
            }
            base += block.source.len();
        }
        Ok(())
    }
}

/// Two passes: validate eligibility before emitting anything, then decode/execute
/// one line at a time. O(max line words) parse storage; input/state/output remain.
/// This intentionally pays for decoding twice and does not support grammar lines.
pub fn bounded_decode(input: &str, output: &mut Output, state: &mut State) -> Result<(), String> {
    let mut arena = Vec::new();
    for (i, line) in input.lines().enumerate() {
        arena.clear();
        match decode_line(line, i + 1, &mut arena) {
            DecodeResult::Blank | DecodeResult::Trivial(_, None) => (),
            _ => return Err("bounded experiment requires trivial, label-free lines".into()),
        }
    }
    fused_decode(input, output, state)
}

/// Decode and execute immediately. Unlike the preflight variant, an unsupported
/// later line can fail after earlier batches were delivered. No fallback/reuse.
pub fn fused_decode(input: &str, output: &mut Output, state: &mut State) -> Result<(), String> {
    let mut arena = Vec::new();
    for (i, line) in input.lines().enumerate() {
        arena.clear();
        match decode_line(line, i + 1, &mut arena) {
            DecodeResult::Blank => (),
            DecodeResult::Trivial(decoded, None) => {
                match execute_decoded(&decoded, &arena, output, state).map_err(|e| e.to_string())? {
                    BlockFlow::Continue => (),
                    flow => return finish(flow, state).map_err(|e| e.to_string()),
                }
            }
            _ => return Err(format!("unsupported line {} in fused experiment", i + 1)),
        }
    }
    Ok(())
}

/// Include provenance and variable events in differential observations, not just
/// the sanitized table. Sorted cell keys avoid randomized map order.
pub fn observation(rows: &[Row], state: &State) -> String {
    let mut result = String::new();
    for row in rows {
        let mut cells: Vec<_> = row.cells.iter().collect();
        cells.sort_by_key(|(key, _)| **key);
        result.push_str(&format!("{}:{cells:?}:{:?}\n", row.line_no, row.variable_changes));
    }
    for map in [&state.axes, &state.translation] {
        let mut items: Vec<_> = map.iter().collect();
        items.sort_by_key(|(key, _)| *key);
        result.push_str(&format!("{items:?}\n"));
    }
    let mut symbols: Vec<_> = state.symbol_table.iter().collect();
    symbols.sort_by_key(|(key, _)| *key);
    let mut strings: Vec<_> = state.string_table.iter().collect();
    strings.sort_by_key(|(key, _)| *key);
    result.push_str(&format!("{symbols:?}\n{strings:?}"));
    let mut table = Table::from_rows(rows, false);
    table.columns.sort_by(|a, b| a.0.cmp(&b.0));
    result.push_str(&format!("{:?}", table.columns));
    result
}

/// Test helper using the real row-stream sink so variable-only rows and events
/// remain observable. The consumer drains concurrently with a bounded channel.
pub fn observe(
    input: &str,
    tolerance: Option<f64>,
    run: impl FnOnce(&mut Output, &mut State) -> Result<(), String>,
) -> Result<String, String> {
    let mut state = fresh_state(input);
    let (tx, rx) = std::sync::mpsc::sync_channel(16);
    std::thread::scope(|scope| {
        let reader = scope.spawn(move || rx.into_iter().collect::<Vec<_>>());
        let mut output = Output::stream(tx);
        if let Some(tolerance) = tolerance {
            output.set_flattener(
                crate::flatten::Flattener::new(tolerance, &state.axis_identifiers).map_err(|e| e.to_string())?,
            );
        }
        run(&mut output, &mut state)?;
        output.finish().map_err(|e| e.to_string())?;
        Ok(observation(&reader.join().unwrap(), &state))
    })
}

pub fn axis_index_map() -> Option<HashMap<String, usize>> {
    let map: HashMap<_, _> = [("E", "NC_BENCH_AXIS_E_INDEX"), ("ELX", "NC_BENCH_AXIS_ELX_INDEX")]
        .into_iter()
        .filter_map(|(axis, variable)| {
            std::env::var(variable)
                .ok()
                .map(|value| (axis.into(), value.parse().expect("axis index")))
        })
        .collect();
    (!map.is_empty()).then_some(map)
}
pub fn allow_undefined() -> bool {
    std::env::var("NC_BENCH_ALLOW_UNDEFINED").is_ok_and(|v| v == "1")
}
