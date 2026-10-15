//! The byte decoder's trivial lines (`line_driver::decode_line`) as
//! `ast::Block`s, in the canonical form `lower` builds for the same text.

use crate::ast::{Arg, Assignment, BinaryOp, Block, Body, Call, Expr, Rhs, Statement};
use crate::errors::ParsingError;
use crate::line_driver::{DecodedLine, Word};

/// Build the block of a decoded trivial line. `line` is the text the line was
/// decoded from (its words borrow from it), `arena` the word arena its range
/// indexes, `label` the uppercased label `decode_line` returned with it.
pub(crate) fn decode_ast(
    line: &str,
    decoded: &DecodedLine,
    arena: &[Word],
    label: Option<&str>,
) -> Result<Block, ParsingError> {
    let mut buffer = BlockBuffer::new();
    buffer.decode(line, decoded, arena, label)?;
    Ok(buffer.block)
}

/// One block reused for every decoded line, so a trivial line allocates
/// nothing once the buffer has seen its shape.
pub(crate) struct BlockBuffer {
    block: Block,
    /// The statement list, parked while the block's body is not `Statements`.
    statements: Vec<Statement>,
    /// Statements a shorter line left over, kept with their allocations.
    spare: Vec<Statement>,
    /// The block number, label and comment strings while the line has none.
    spare_text: [String; 3],
}

impl BlockBuffer {
    pub(crate) fn new() -> Self {
        BlockBuffer {
            block: Block {
                line_no: 0,
                block_number: None,
                label: None,
                body: Body::Empty,
                comment: None,
            },
            statements: Vec::new(),
            spare: Vec::new(),
            spare_text: Default::default(),
        }
    }

    /// The block of the last `decode`.
    pub(crate) fn block(&self) -> &Block {
        &self.block
    }

    /// Refill the block with a decoded trivial line (arguments as for `decode_ast`).
    pub(crate) fn decode(
        &mut self,
        line: &str,
        decoded: &DecodedLine,
        arena: &[Word],
        label: Option<&str>,
    ) -> Result<&Block, ParsingError> {
        let internal =
            |message: String| ParsingError::with_context(decoded.line_no, line, "decoded line", message.as_str());
        let words = arena
            .get(decoded.words.clone())
            .ok_or_else(|| internal("word range outside the arena".to_string()))?;
        let mut statements = match std::mem::replace(&mut self.block.body, Body::Empty) {
            Body::Statements(statements) => statements,
            _ => std::mem::take(&mut self.statements),
        };
        for (index, word) in words.iter().enumerate() {
            if index == statements.len() {
                // An empty G code holds no allocation; `refill` replaces it.
                statements.push(self.spare.pop().unwrap_or(Statement::G(String::new())));
            }
            refill(&mut statements[index], word, line).map_err(internal)?;
        }
        if !words.is_empty() {
            // Reversed, so the next longer line pops them back in source order.
            self.spare.extend(statements.drain(words.len()..).rev());
        }
        let block = &mut self.block;
        block.line_no = decoded.line_no;
        let [number, label_text, comment] = &mut self.spare_text;
        set_optional(&mut block.block_number, number, decoded.n);
        set_optional(&mut block.label, label_text, label);
        set_optional(&mut block.comment, comment, decoded.comment);
        if words.is_empty() {
            // Parked with its statements, which the next line with words refills.
            self.statements = statements;
        } else {
            block.body = Body::Statements(statements);
        }
        Ok(&self.block)
    }
}

/// Refill `slot` with the statement of `word`, in the slot's own strings when
/// it already has that shape.
fn refill(slot: &mut Statement, word: &Word, line: &str) -> Result<(), String> {
    match *word {
        Word::Assign(key, value) => match slot {
            Statement::Assign(Assignment::Var { name, rhs }) => {
                set_upper(name, key);
                match rhs {
                    Rhs::Expr(Expr::Number(number)) => *number = value,
                    rhs => *rhs = Rhs::Expr(Expr::Number(value)),
                }
            }
            slot => {
                *slot = Statement::Assign(Assignment::Var {
                    name: key.to_uppercase(),
                    rhs: Rhs::Expr(Expr::Number(value)),
                })
            }
        },
        Word::AssignDynamic {
            key,
            ident,
            factor,
            incremental,
        } => {
            // The decoder keeps no operand order, the tree does: IDENT*NUMBER or NUMBER*IDENT.
            let ident = match ident {
                None => None,
                Some(name) => Some((
                    name,
                    ident_precedes_star(line, name).ok_or("identifier outside its line")?,
                )),
            };
            match slot {
                Statement::Assign(Assignment::Var { name, rhs }) => {
                    set_upper(name, key);
                    match (rhs, incremental) {
                        (Rhs::Increment(product), true) => refill_product(product, ident, factor),
                        (Rhs::Expr(Expr::Paren(product)), false) => refill_product(product, ident, factor),
                        (rhs, _) => *rhs = dynamic_rhs(ident, factor, incremental),
                    }
                }
                slot => {
                    *slot = Statement::Assign(Assignment::Var {
                        name: key.to_uppercase(),
                        rhs: dynamic_rhs(ident, factor, incremental),
                    })
                }
            }
        }
        Word::Call { value, .. } => match slot {
            Statement::Call(call) => fill_call(call, value).ok_or_else(|| format!("undecodable call '{value}'"))?,
            slot => {
                let mut call = Call {
                    name: String::new(),
                    args: None,
                    text: String::new(),
                };
                fill_call(&mut call, value).ok_or_else(|| format!("undecodable call '{value}'"))?;
                *slot = Statement::Call(call);
            }
        },
        Word::GCommand(_, code) => match slot {
            Statement::G(text) => set_upper(text, code),
            slot => *slot = Statement::G(code.to_uppercase()),
        },
        Word::MCode(code) => match slot {
            Statement::M(text) => set_upper(text, code),
            slot => *slot = Statement::M(code.to_uppercase()),
        },
    }
    Ok(())
}

/// The value of a dynamic word: `IC(<product>)` or `(<product>)`.
fn dynamic_rhs(ident: Option<(&str, bool)>, factor: f64, incremental: bool) -> Rhs {
    let product = product(ident, factor);
    if incremental {
        Rhs::Increment(product)
    } else {
        Rhs::Expr(Expr::Paren(Box::new(product)))
    }
}

/// `factor`, or `ident * factor` in source order (`ident` carries whether it comes first).
fn product(ident: Option<(&str, bool)>, factor: f64) -> Expr {
    match ident {
        None => Expr::Number(factor),
        Some((name, ident_first)) => {
            let var = Box::new(Expr::Var(name.to_uppercase()));
            let number = Box::new(Expr::Number(factor));
            if ident_first {
                Expr::Binary(BinaryOp::Mul, var, number)
            } else {
                Expr::Binary(BinaryOp::Mul, number, var)
            }
        }
    }
}

/// `*expr = product(ident, factor)` in the boxes and name `expr` already holds
/// when it is a product of the same kind.
fn refill_product(expr: &mut Expr, ident: Option<(&str, bool)>, factor: f64) {
    match ident {
        None => {
            if let Expr::Number(value) = expr {
                *value = factor;
                return;
            }
        }
        Some((name, ident_first)) => {
            if let Expr::Binary(BinaryOp::Mul, lhs, rhs) = expr {
                let (var, number) = if ident_first { (lhs, rhs) } else { (rhs, lhs) };
                if matches!(**var, Expr::Number(_)) && matches!(**number, Expr::Var(_)) {
                    // The previous line had the other operand order.
                    std::mem::swap(var, number);
                }
                if let (Expr::Var(slot), Expr::Number(value)) = (&mut **var, &mut **number) {
                    set_upper(slot, name);
                    *value = factor;
                    return;
                }
            }
        }
    }
    *expr = product(ident, factor);
}

/// `*slot = text.to_uppercase()` without a new allocation once the slot is large enough.
fn set_upper(slot: &mut String, text: &str) {
    slot.clear();
    if text.is_ascii() {
        slot.push_str(text);
        slot.make_ascii_uppercase();
    } else {
        slot.push_str(&text.to_uppercase());
    }
}

/// `*slot = value.map(str::to_string)`, parking the string in `spare` while the slot is empty.
fn set_optional(slot: &mut Option<String>, spare: &mut String, value: Option<&str>) {
    match value {
        Some(value) => set_text(slot.get_or_insert_with(|| std::mem::take(spare)), value),
        None => {
            if let Some(text) = slot.take() {
                *spare = text;
            }
        }
    }
}

/// `*slot = text.to_string()` in the slot's own allocation.
fn set_text(slot: &mut String, text: &str) {
    slot.clear();
    slot.push_str(text);
}

/// True when the `*` of a decoded product follows `ident` (IDENT*NUMBER);
/// `None` when `ident` is not a span of `line`.
fn ident_precedes_star(line: &str, ident: &str) -> Option<bool> {
    let offset = (ident.as_ptr() as usize).checked_sub(line.as_ptr() as usize)?;
    let after = line.get(offset + ident.len()..)?;
    Some(after.trim_start_matches([' ', '\t']).starts_with('*'))
}

/// Fill `call` from a decoded call: `NAME` or `NAME(<number>, ...)`, the only
/// shapes the decoder claims.
fn fill_call(call: &mut Call, value: &str) -> Option<()> {
    let name_end = value
        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .unwrap_or(value.len());
    set_text(&mut call.name, &value[..name_end]);
    set_text(&mut call.text, value);
    let rest = value[name_end..].trim_start_matches([' ', '\t']);
    match rest.strip_prefix('(') {
        None if rest.is_empty() => call.args = None,
        None => return None,
        Some(inner) => {
            let inner = inner.strip_suffix(')')?.trim_matches([' ', '\t']);
            let args = call.args.get_or_insert_with(Vec::new);
            args.clear();
            if !inner.is_empty() {
                for item in inner.split(',') {
                    let number = item.trim_matches([' ', '\t']).parse::<f64>().ok()?;
                    args.push(Arg::Expr(Expr::Number(number)));
                }
            }
        }
    }
    Some(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::line_driver::{decode_line, DecodeResult};
    use crate::state::State;

    const PREFIXES: &[&str] = &["", "N10 ", "N0010 ", "LBL_A: ", "N20 _UNDER: ", "  "];
    const COMMENTS: &[&str] = &["", " ;comment", "\t;tab", " ; (heaters)"];
    const WORDS: &[&str] = &[
        // axis words and constant assignments
        "X1",
        "X-1.5",
        "Y2.500",
        "Z0",
        "z-3.",
        "x100",
        "E5.",
        "F2400",
        "S1000",
        "T5",
        "D1",
        "H2",
        "I1.5",
        "J-2",
        "K3",
        "U1",
        "V2",
        "W3",
        "B45",
        "C90",
        "A=0.0",
        "X=1+2*3",
        "ELX=3083.63",
        "E_MULTIPLIER = 1",
        "P_1 = 2--3",
        "P_2 = 2- -3",
        "P_3=10/4/5",
        "P_4 = -2*3",
        "PROGRESS = 100 * 1/2222",
        "x=10",
        "Mixed_Case=5",
        "X = 5",
        "R1=-0",
        "Y=- 4",
        // IC() and parenthesized products
        "E=IC(E_MULT*0.02166)",
        "E=IC(0.25*M_A)",
        "E=IC(2)",
        "E=IC(-1.5)",
        "e=ic( M_A * 0.5 )",
        "E=IC(M_A*-0.5)",
        "E=IC(-0.5*M_A)",
        "F=(F_MULTI*1314)",
        "F=(2*F_MULTI)",
        "F=(2400)",
        "F=( F_MULTI * 1.5 )",
        "q_var=IC(3)",
        // G and M codes, vocabulary keywords
        "G1",
        "G0",
        "g2",
        "G17",
        "G90",
        "G54",
        "G500",
        "G641",
        "BSPLINE",
        "soft",
        "CP",
        "M3",
        "M30",
        "m5",
        "M02",
        "M17",
        // subprogram calls
        "MATERIAL_UPDATE",
        "EXTRUDER_ON",
        "SUPPORT_PIN_TRIGGER(2.5)",
        "support_pin_trigger ( -2.5, 0, 3. )",
        "CUSTOM()",
        "CUSTOM ( )",
        "Y2O",
        "N_CALL",
    ];

    fn representative_lines() -> Vec<String> {
        let mut lines = Vec::new();
        for (p, prefix) in PREFIXES.iter().enumerate() {
            for (w, word) in WORDS.iter().enumerate() {
                lines.push(format!("{prefix}{word}{}", COMMENTS[(p + w) % COMMENTS.len()]));
            }
        }
        // Word pairs and triples, as CAM output strings them together.
        for (w, word) in WORDS.iter().enumerate() {
            let second = WORDS[(w * 7 + 3) % WORDS.len()];
            let third = WORDS[(w * 13 + 5) % WORDS.len()];
            lines.push(format!("{word} {second}"));
            lines.push(format!("N{w} {word} {second} {third}{}", COMMENTS[w % COMMENTS.len()]));
        }
        lines.extend(
            [
                "N38 X-45.414 Y-8.835 Z49. A=0.0 ELX=3083.63 ; comment",
                "N57 G1 X347.964 Y-45.000 Z-2.035 A0 B0 C0 E=IC(E_MULTIPLIER*0.00000) ELX=10412.522 F=(F_MULTI*1314)",
                "G1 X1 Y2 F=(F_MULTI*1314)",
                "N10",
                "LBL:",
                ";only a comment",
                "N10 ;c",
                "LBL: ;c",
                "",
                "   ",
                "CUSTOM(1) OTHER(2)",
                "X1 LAYER_CHANGE M3",
                "RESET_LAYERS\t;tab",
            ]
            .map(str::to_string),
        );
        lines
    }

    fn state() -> State {
        State::new(
            vec!["X".to_string(), "Y".to_string(), "Z".to_string()],
            1000,
            None,
            false,
        )
    }

    /// The decoder's block for `line`, or None when it does not claim the line.
    fn decoded(line: &str) -> Option<Block> {
        let mut arena = Vec::new();
        match decode_line(line, 7, &mut arena) {
            DecodeResult::Trivial(decoded, label) => {
                Some(decode_ast(line, &decoded, &arena, label.as_deref()).expect("decoded lines convert"))
            }
            DecodeResult::Blank | DecodeResult::NeedsGrammar => None,
        }
    }

    #[test]
    fn decoded_lines_lower_to_equal_blocks() {
        let state = state();
        let mut claimed = 0;
        for line in representative_lines() {
            let Some(from_decoder) = decoded(&line) else { continue };
            claimed += 1;
            let from_pest = crate::lower::parse_line(&line, 7, &state)
                .unwrap_or_else(|e| panic!("the decoder claims {line:?}, which does not parse: {e}"));
            assert_eq!(from_decoder, from_pest, "line {line:?}");
        }
        assert!(claimed >= 400, "only {claimed} lines were claimed by the decoder");
    }

    #[test]
    fn blank_and_grammar_lines_are_not_claimed() {
        for line in [
            "",
            "  ",
            "X=SIN(30)",
            "IF R1>0 GOTOF L",
            "WHILE R1<5",
            "DEF REAL A",
            "TRANS X1",
            "X10Y20",
        ] {
            assert!(decoded(line).is_none(), "{line:?}");
        }
    }

    /// Refill the buffer with `line`; None when the decoder does not claim it.
    fn refilled(buffer: &mut BlockBuffer, line: &str) -> Option<Block> {
        let mut arena = Vec::new();
        match decode_line(line, 7, &mut arena) {
            DecodeResult::Trivial(words, label) => Some(
                buffer
                    .decode(line, &words, &arena, label.as_deref())
                    .expect("decoded lines convert")
                    .clone(),
            ),
            DecodeResult::Blank | DecodeResult::NeedsGrammar => None,
        }
    }

    #[test]
    fn a_reused_buffer_keeps_nothing_from_earlier_lines() {
        let lines: Vec<String> = representative_lines()
            .into_iter()
            .filter(|line| decoded(line).is_some())
            .collect();
        let single: Vec<&str> = WORDS.iter().copied().filter(|word| decoded(word).is_some()).collect();
        let mut sequences: Vec<Vec<&str>> = vec![
            lines.iter().map(String::as_str).collect(),
            lines.iter().rev().map(String::as_str).collect(),
        ];
        // Every single-word shape after every other, then a longer and a shorter line.
        for first in &single {
            for second in &single {
                sequences.push(vec![first, second, "N1 G1 X1 Y2 E=IC(E_M*0.5) F=(2*F_M) ;c", second]);
            }
        }
        for sequence in sequences {
            let mut buffer = BlockBuffer::new();
            let mut previous = "";
            for line in sequence {
                assert_eq!(
                    refilled(&mut buffer, line),
                    decoded(line),
                    "{line:?} after {previous:?}"
                );
                previous = line;
            }
        }
    }

    /// Counts each thread's allocations, so a test can assert that a refill makes none.
    struct CountingAllocator;

    thread_local! {
        static ALLOCATIONS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    }

    unsafe impl std::alloc::GlobalAlloc for CountingAllocator {
        unsafe fn alloc(&self, layout: std::alloc::Layout) -> *mut u8 {
            let _ = ALLOCATIONS.try_with(|count| count.set(count.get() + 1));
            std::alloc::System.alloc(layout)
        }

        unsafe fn dealloc(&self, ptr: *mut u8, layout: std::alloc::Layout) {
            std::alloc::System.dealloc(ptr, layout)
        }

        unsafe fn realloc(&self, ptr: *mut u8, layout: std::alloc::Layout, new_size: usize) -> *mut u8 {
            let _ = ALLOCATIONS.try_with(|count| count.set(count.get() + 1));
            std::alloc::System.realloc(ptr, layout, new_size)
        }
    }

    #[global_allocator]
    static GLOBAL: CountingAllocator = CountingAllocator;

    #[test]
    fn refilling_seen_shapes_allocates_nothing() {
        let lines = [
            "N10 G1 X1 E=IC(E_M*0.5) F=(2*F_M) M3 CALL_A(1) ;first",
            // A comment-only line parks the statements and the block number.
            ";gap",
            "N11 G0 X2 E=IC(E_N*0.7) F=(3*F_N) M5 CALL_B(2) ;other",
            "N12 G0 X3 ;short",
            "N13 G1 X4 E=IC(0.5*E_M) F=(F_M*2) M3 CALL_A(3) ;again",
        ];
        let mut buffer = BlockBuffer::new();
        // The first pass sizes the buffer; the second must reuse every allocation.
        for pass in 0..2 {
            for line in lines {
                let mut arena = Vec::with_capacity(16);
                let DecodeResult::Trivial(words, label) = decode_line(line, 7, &mut arena) else {
                    panic!("{line:?} is not claimed");
                };
                let before = ALLOCATIONS.with(|count| count.get());
                let block = buffer
                    .decode(line, &words, &arena, label.as_deref())
                    .expect("decoded lines convert");
                let allocated = ALLOCATIONS.with(|count| count.get()) - before;
                if pass == 1 {
                    assert_eq!(allocated, 0, "{line:?}");
                }
                assert_eq!(Some(block.clone()), decoded(line), "{line:?}");
            }
        }
    }

    /// Lines the decoder used to claim with a meaning the grammar does not
    /// give them (#83); it now leaves them to the grammar, so both entries
    /// build the block from the same parse.
    #[test]
    fn former_decoder_divergences_take_the_grammar() {
        for line in [
            // The grammar's integer needs a non-letter after the digits, so
            // this is a call N10X5, not block number 10.
            "N10X5",
            "N10LBL: X1",
            // A frame keyword followed by '=' is a parse error in the grammar.
            "TRANS=5",
            "ROT=1",
            // Word operators continue the expression after a constant.
            "X=5 AND Y1",
            "X=5 OR Y1",
            "X=5 XOR Y1",
            "X=5 B_AND Y1",
            "X=5 B_OR Y1",
            "X=5 B_XOR Y1",
        ] {
            assert!(decoded(line).is_none(), "{line:?} must take the grammar");
        }
    }
}
