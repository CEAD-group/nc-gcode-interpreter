//! Owned AST of one NC block (one source line), produced from pest by `lower`
//! and from the byte decoder by `decode_ast`, and executed by `exec`. The
//! multi-line structures appear as their head, ELSE and closer lines
//! (`Control::IfHead` .. `Control::EndLoop`); the driver takes their nesting
//! from the structure scan.
//!
//! Canonical form, so that both producers build equal blocks for equal text:
//! names of variables, array elements and assignment targets are uppercased
//! (the tree walker uppercases them before any use); an axis word `X10` and
//! `X=10` are the same `Assignment::Var`; a parenless call whose name is in
//! the G vocabulary and is not a frame keyword is a `Statement::G`; unary
//! minus and `+ - * /` over number literals are folded into `Expr::Number`
//! (see `Expr::binary`). Text the tree walker quotes in messages or rows is
//! kept as written (call text, function names, index text, string arguments).

/// One block: `[N<digits>] [LABEL:] <body> [;comment]`.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Block {
    /// 1-based source line.
    pub line_no: usize,
    /// The block number digits as written (leading zeros and sign kept).
    pub block_number: Option<String>,
    /// The jump label name, uppercased.
    pub label: Option<String>,
    pub body: Body,
    /// The comment including its leading `;`.
    pub comment: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Body {
    Empty,
    Definition(Definition),
    FrameOp(FrameOp),
    /// One or more statements in source order.
    Statements(Vec<Statement>),
    Control(Control),
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Statement {
    Assign(Assignment),
    AssignMulti(AssignMulti),
    /// Uppercased G command: `G<digits>`, `GFRAME[<n>]` or a vocabulary keyword
    /// (`BSPLINE`); classified against the G table by the executor.
    G(String),
    /// Uppercased M code as written (`M30`, `M02`).
    M(String),
    /// `T="<name>"`: the tool name.
    Tool(String),
    Call(Call),
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Assignment {
    /// `X10`, `X=<rhs>`, `NAME=IC(<expr>)`, `NAME="text"`.
    Var { name: String, rhs: Rhs },
    /// `NAME[<i>, ...]=<rhs>`; the grammar has no IC() form for elements.
    Element {
        name: String,
        indices: Vec<Index>,
        rhs: ElementRhs,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Rhs {
    /// A plain expression; it may still be string-valued at run time (a
    /// STRING variable read or SPRINT/SUBSTR), as in the tree walker.
    Expr(Expr),
    /// `IC(<expr>)`.
    Increment(Expr),
    Str(StringExpr),
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum ElementRhs {
    Expr(Expr),
    Str(StringExpr),
}

/// `NAME[<i>, ...] = SET(...)` or `REP(...)`.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct AssignMulti {
    pub name: String,
    pub indices: Vec<Index>,
    pub values: MultiValues,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum MultiValues {
    /// `SET(a, , c)`; `None` is a gap that leaves its element untouched.
    Set(Vec<Option<Expr>>),
    /// `REP(<value>[, <count>])`.
    Rep { value: Expr, count: Option<Expr> },
}

/// One array index; `text` is the trimmed source, which the tree walker
/// matches against the axis names and quotes in errors.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Index {
    pub expr: Expr,
    pub text: String,
}

/// A subprogram-like call statement.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Call {
    /// The name as written.
    pub name: String,
    /// `None` without parentheses, `Some(vec![])` for `NAME()`.
    pub args: Option<Vec<Arg>>,
    /// The source span the row stores (a parenless call keeps the whitespace
    /// pest skipped after it).
    pub text: String,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Arg {
    Expr(Expr),
    /// A quoted string or `<<` chain; `text` is its source span, which the
    /// type error of a numeric function quotes.
    Str {
        value: StringExpr,
        text: String,
    },
    /// An empty position, as in `CYCLE(1,,3)`.
    Omitted,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Expr {
    Number(f64),
    /// A variable read; a `$` name is a system variable.
    Var(String),
    /// An array element or indexed system variable read (`A[1,2]`, `$AA_IW[X]`).
    Element {
        name: String,
        indices: Vec<Index>,
    },
    Unary(UnaryOp, Box<Expr>),
    Binary(BinaryOp, Box<Expr>, Box<Expr>),
    /// A function call; the name as written (arity errors quote it).
    Call {
        name: String,
        args: Vec<Arg>,
    },
    /// `(<expr>)`, kept because an index `(X)` is not the axis name `X`.
    Paren(Box<Expr>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum UnaryOp {
    Neg,
    Not,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BinaryOp {
    Add,
    Sub,
    Mul,
    Div,
    /// `DIV`
    IntDiv,
    /// `MOD`
    Mod,
    BitAnd,
    BitXor,
    BitOr,
    And,
    Xor,
    Or,
    Lt,
    Gt,
    Le,
    Ge,
    Eq,
    Ne,
}

impl BinaryOp {
    /// The operator as the language spells it (uppercase for word operators).
    pub(crate) fn symbol(self) -> &'static str {
        match self {
            BinaryOp::Add => "+",
            BinaryOp::Sub => "-",
            BinaryOp::Mul => "*",
            BinaryOp::Div => "/",
            BinaryOp::IntDiv => "DIV",
            BinaryOp::Mod => "MOD",
            BinaryOp::BitAnd => "B_AND",
            BinaryOp::BitXor => "B_XOR",
            BinaryOp::BitOr => "B_OR",
            BinaryOp::And => "AND",
            BinaryOp::Xor => "XOR",
            BinaryOp::Or => "OR",
            BinaryOp::Lt => "<",
            BinaryOp::Gt => ">",
            BinaryOp::Le => "<=",
            BinaryOp::Ge => ">=",
            BinaryOp::Eq => "==",
            BinaryOp::Ne => "<>",
        }
    }
}

impl Expr {
    /// Build `lhs op rhs`, folding `+ - * /` over two literals: the evaluator
    /// performs the same f64 operation, and literals have no side effects.
    pub(crate) fn binary(op: BinaryOp, lhs: Expr, rhs: Expr) -> Expr {
        if let (Expr::Number(a), Expr::Number(b)) = (&lhs, &rhs) {
            let folded = match op {
                BinaryOp::Add => Some(a + b),
                BinaryOp::Sub => Some(a - b),
                BinaryOp::Mul => Some(a * b),
                BinaryOp::Div => Some(a / b),
                _ => None,
            };
            if let Some(value) = folded {
                return Expr::Number(value);
            }
        }
        Expr::Binary(op, Box::new(lhs), Box::new(rhs))
    }

    /// Build a prefix operation, folding unary minus over a literal.
    pub(crate) fn unary(op: UnaryOp, operand: Expr) -> Expr {
        match (op, operand) {
            (UnaryOp::Neg, Expr::Number(value)) => Expr::Number(-value),
            (op, operand) => Expr::Unary(op, Box::new(operand)),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum StringExpr {
    /// A lone quoted literal.
    Literal(String),
    /// `a << b << ...`, or the prefix form `<< a` (conversion to STRING) when
    /// the rest is empty.
    Concat(Operand, Vec<Operand>),
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Operand {
    Literal(String),
    /// Evaluated as text: a STRING variable or string function, or a number
    /// converted by the `<<` rules.
    Expr(Expr),
}

/// `DEF <type> <item>, <item>, ...`
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Definition {
    pub data_type: DataType,
    pub items: Vec<DefItem>,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum DataType {
    Int,
    Real,
    Bool,
    /// `STRING[<n>]`, the length digits as written (the tree walker ignores them).
    String(String),
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum DefItem {
    /// `NAME`
    Var(String),
    /// `NAME[<i>, ...]`
    Array { name: String, indices: Vec<Index> },
    /// `NAME=<value>` (any assignment form)
    Init(Assignment),
    /// `NAME[<i>]=SET(...)` / `REP(...)`
    Multi(AssignMulti),
}

/// A frame instruction alone in its block, with its axis assignments.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct FrameOp {
    pub kind: FrameKind,
    pub assignments: Vec<Assignment>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FrameKind {
    Trans,
    ATrans,
    Scale,
    AScale,
    Rot,
    ARot,
    Rots,
    ARots,
    CRots,
    Mirror,
    AMirror,
}

impl FrameKind {
    pub(crate) fn from_keyword(keyword: &str) -> Option<FrameKind> {
        Some(match keyword.to_ascii_uppercase().as_str() {
            "TRANS" => FrameKind::Trans,
            "ATRANS" => FrameKind::ATrans,
            "SCALE" => FrameKind::Scale,
            "ASCALE" => FrameKind::AScale,
            "ROT" => FrameKind::Rot,
            "AROT" => FrameKind::ARot,
            "ROTS" => FrameKind::Rots,
            "AROTS" => FrameKind::ARots,
            "CROTS" => FrameKind::CRots,
            "MIRROR" => FrameKind::Mirror,
            "AMIRROR" => FrameKind::AMirror,
            _ => return None,
        })
    }

    /// The uppercase keyword, as the tree walker's frame messages quote it.
    pub(crate) fn keyword(self) -> &'static str {
        match self {
            FrameKind::Trans => "TRANS",
            FrameKind::ATrans => "ATRANS",
            FrameKind::Scale => "SCALE",
            FrameKind::AScale => "ASCALE",
            FrameKind::Rot => "ROT",
            FrameKind::ARot => "AROT",
            FrameKind::Rots => "ROTS",
            FrameKind::ARots => "AROTS",
            FrameKind::CRots => "CROTS",
            FrameKind::Mirror => "MIRROR",
            FrameKind::AMirror => "AMIRROR",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Control {
    /// `GOTOS`
    GotoS,
    Goto(Goto),
    /// One or more `IF <cond> GOTO.. <target>` in one block; the first true one jumps.
    IfGoto(Vec<(Expr, Goto)>),
    /// `CASE(<selector>) OF <const> GOTO.. ... [DEFAULT GOTO..]`
    Case {
        selector: Expr,
        arms: Vec<(f64, Goto)>,
        default: Option<Goto>,
    },
    /// `IF <cond>` opening an IF block.
    IfHead(Expr),
    Else,
    EndIf,
    WhileHead(Expr),
    EndWhile,
    /// `FOR <assignment> TO <expr>`
    ForHead {
        assignment: Assignment,
        to: Expr,
    },
    EndFor,
    RepeatHead,
    /// `UNTIL <cond>`
    UntilTail(Expr),
    LoopHead,
    EndLoop,
}

/// `GOTO.. <target>`; the target as written (trimmed), canonicalized by the
/// executor with `canonical_jump_target`.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Goto {
    pub kind: GotoKind,
    pub target: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GotoKind {
    Goto,
    GotoF,
    GotoB,
    GotoC,
}
