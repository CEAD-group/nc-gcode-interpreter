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
    let internal =
        |message: String| ParsingError::with_context(decoded.line_no, line, "decoded line", message.as_str());
    let words = arena
        .get(decoded.words.clone())
        .ok_or_else(|| internal("word range outside the arena".to_string()))?;
    let mut statements = Vec::with_capacity(words.len());
    for word in words {
        statements.push(match *word {
            Word::Assign(key, value) => Statement::Assign(Assignment::Var {
                name: key.to_uppercase(),
                rhs: Rhs::Expr(Expr::Number(value)),
            }),
            Word::AssignDynamic {
                key,
                ident,
                factor,
                incremental,
            } => {
                let value = match ident {
                    None => Expr::Number(factor),
                    Some(name) => {
                        let var = Expr::Var(name.to_uppercase());
                        let number = Expr::Number(factor);
                        // The decoder keeps no operand order, the tree does: IDENT*NUMBER or NUMBER*IDENT.
                        if ident_precedes_star(line, name)
                            .ok_or_else(|| internal("identifier outside its line".to_string()))?
                        {
                            Expr::Binary(BinaryOp::Mul, Box::new(var), Box::new(number))
                        } else {
                            Expr::Binary(BinaryOp::Mul, Box::new(number), Box::new(var))
                        }
                    }
                };
                let rhs = if incremental {
                    Rhs::Increment(value)
                } else {
                    Rhs::Expr(Expr::Paren(Box::new(value)))
                };
                Statement::Assign(Assignment::Var {
                    name: key.to_uppercase(),
                    rhs,
                })
            }
            Word::Call { value, .. } => {
                Statement::Call(call_from_text(value).ok_or_else(|| internal(format!("undecodable call '{value}'")))?)
            }
            Word::GCommand(_, code) => Statement::G(code.to_uppercase()),
            Word::MCode(code) => Statement::M(code.to_uppercase()),
        });
    }
    Ok(Block {
        line_no: decoded.line_no,
        block_number: decoded.n.map(str::to_string),
        label: label.map(str::to_string),
        body: if statements.is_empty() {
            Body::Empty
        } else {
            Body::Statements(statements)
        },
        comment: decoded.comment.map(str::to_string),
    })
}

/// True when the `*` of a decoded product follows `ident` (IDENT*NUMBER);
/// `None` when `ident` is not a span of `line`.
fn ident_precedes_star(line: &str, ident: &str) -> Option<bool> {
    let offset = (ident.as_ptr() as usize).checked_sub(line.as_ptr() as usize)?;
    let after = line.get(offset + ident.len()..)?;
    Some(after.trim_start_matches([' ', '\t']).starts_with('*'))
}

/// A decoded call: `NAME` or `NAME(<number>, ...)`, the only shapes the
/// decoder claims.
fn call_from_text(value: &str) -> Option<Call> {
    let name_end = value
        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .unwrap_or(value.len());
    let name = &value[..name_end];
    let rest = value[name_end..].trim_start_matches([' ', '\t']);
    let args = match rest.strip_prefix('(') {
        None if rest.is_empty() => None,
        None => return None,
        Some(inner) => {
            let inner = inner.strip_suffix(')')?.trim_matches([' ', '\t']);
            if inner.is_empty() {
                Some(Vec::new())
            } else {
                let mut args = Vec::new();
                for item in inner.split(',') {
                    let number = item.trim_matches([' ', '\t']).parse::<f64>().ok()?;
                    args.push(Arg::Expr(Expr::Number(number)));
                }
                Some(args)
            }
        }
    };
    Some(Call {
        name: name.to_string(),
        args,
        text: value.to_string(),
    })
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

    /// Lines the decoder claims with a meaning the grammar does not give them;
    /// a pre-existing fast-path divergence (NC_STAGE1=0 interprets them differently).
    #[test]
    fn known_decoder_divergences() {
        let state = state();
        for line in [
            // The decoder ends the block number at the digits; the grammar's
            // integer needs a non-letter after them, so this is a call N10X5.
            "N10X5",
            "N10LBL: X1",
            // A frame keyword followed by '=' is a parse error in the grammar.
            "TRANS=5",
            "ROT=1",
            // Word operators the decoder takes for calls continue the expression.
            "X=5 AND Y1",
            "X=5 OR Y1",
            "X=5 XOR Y1",
            "X=5 B_AND Y1",
            "X=5 B_OR Y1",
            "X=5 B_XOR Y1",
        ] {
            let from_decoder = decoded(line).unwrap_or_else(|| panic!("{line:?} is no longer claimed"));
            if let Ok(from_pest) = crate::lower::parse_line(line, 7, &state) {
                assert_ne!(from_decoder, from_pest, "{line:?} no longer diverges");
            }
        }
    }
}
