//! Parity of the statement-AST path (NC_EXEC=ast) with the default path
//! (the line fast path for structure-free programs, the tree walker
//! otherwise): equal tables, streamed rows, final state, and errors. Parse
//! errors are worded per entry rule, so for those only the kind and line are
//! compared.

use nc_gcode_interpreter::interpreter::{nc_to_row_stream, nc_to_table};
use nc_gcode_interpreter::output::{Column, Row};
use std::collections::HashMap;
use std::path::Path;
use std::sync::{Mutex, MutexGuard};

/// NC_EXEC is process-global; every test that flips it holds this lock.
static ENV_LOCK: Mutex<()> = Mutex::new(());

fn env_lock() -> MutexGuard<'static, ()> {
    // A failed test only poisons the lock; the variable is always restored.
    ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

#[derive(Clone)]
struct Options {
    initial_state: Option<String>,
    axes: Option<Vec<String>>,
    extra_axes: Option<Vec<String>>,
    axis_index_map: Option<HashMap<String, usize>>,
    iteration_limit: usize,
    allow_undefined: bool,
    flatten_tolerance: Option<f64>,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            initial_state: None,
            axes: None,
            extra_axes: None,
            axis_index_map: None,
            iteration_limit: 10_000,
            allow_undefined: false,
            flatten_tolerance: None,
        }
    }
}

fn limit(iteration_limit: usize) -> Options {
    Options {
        iteration_limit,
        ..Options::default()
    }
}

/// One output row as comparable data: cells sorted by key.
#[derive(Debug, PartialEq)]
struct RowSummary {
    line_no: usize,
    cells: Vec<(String, String)>,
    variable_changes: Vec<(String, f64)>,
}

fn summarize(row: Row) -> RowSummary {
    let mut cells: Vec<(String, String)> = row
        .cells
        .iter()
        .map(|(key, value)| (key.to_string(), format!("{value:?}")))
        .collect();
    cells.sort();
    RowSummary {
        line_no: row.line_no,
        cells,
        variable_changes: row.variable_changes,
    }
}

#[derive(Debug, PartialEq)]
struct StateSummary {
    symbols: Vec<(String, f64)>,
    strings: Vec<(String, String)>,
    axes: Vec<(String, f64)>,
    translation: Vec<(String, f64)>,
    rotation: [[f64; 3]; 3],
    rotated: bool,
    plane: String,
    open_jump_scopes: usize,
}

fn sorted<V: Clone>(map: impl IntoIterator<Item = (String, V)>) -> Vec<(String, V)> {
    let mut entries: Vec<(String, V)> = map.into_iter().collect();
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    entries
}

#[derive(Debug, PartialEq)]
struct Failure {
    kind: String,
    line: Option<usize>,
    column: Option<usize>,
    context: Option<String>,
    /// The Display text; `None` for parse errors, which differ per entry rule.
    message: Option<String>,
}

#[derive(Debug, PartialEq)]
enum Outcome {
    Done {
        columns: Vec<(String, Column)>,
        rows: Vec<RowSummary>,
        state: StateSummary,
    },
    Failed(Failure),
}

impl Outcome {
    fn failure(&self) -> &Failure {
        match self {
            Outcome::Failed(failure) => failure,
            Outcome::Done { .. } => panic!("expected an error, the program ran"),
        }
    }

    fn rows(&self) -> &[RowSummary] {
        match self {
            Outcome::Done { rows, .. } => rows,
            Outcome::Failed(failure) => panic!("expected rows, got {failure:?}"),
        }
    }
}

/// The did-you-mean hint breaks distance ties by HashSet order (random per
/// process), so only its presence is compared.
fn mask_suggestion(message: &str) -> String {
    const HINT: &str = "Did you mean '";
    match message.find(HINT) {
        Some(start) => {
            let name_start = start + HINT.len();
            let name_end = message[name_start..]
                .find('\'')
                .map_or(message.len(), |end| name_start + end);
            format!("{}...{}", &message[..name_start], &message[name_end..])
        }
        None => message.to_string(),
    }
}

/// `debug` and `display` are the error's Debug and Display text, the rest
/// its `location()` (the error type is not exported, so callers unpack it).
fn failure_of(
    debug: &str,
    display: &str,
    line: Option<usize>,
    column: Option<usize>,
    context: Option<String>,
) -> Failure {
    let kind: String = debug.chars().take_while(|c| c.is_ascii_alphanumeric()).collect();
    let parse = kind == "ParsingContext" && context.as_deref().is_some_and(|context| context.ends_with("parsing"));
    Failure {
        kind: if parse { "parse error".to_string() } else { kind },
        line,
        column,
        context,
        message: (!parse).then(|| mask_suggestion(display)),
    }
}

fn run_path(program: &str, options: &Options, ast: bool) -> Outcome {
    if ast {
        std::env::set_var("NC_EXEC", "ast");
    } else {
        std::env::remove_var("NC_EXEC");
    }
    let outcome = run_both_apis(program, options);
    std::env::remove_var("NC_EXEC");
    outcome
}

fn run_both_apis(program: &str, options: &Options) -> Outcome {
    let table = nc_to_table(
        program,
        options.initial_state.as_deref(),
        options.axes.clone(),
        options.extra_axes.clone(),
        options.iteration_limit,
        false,
        options.axis_index_map.clone(),
        options.allow_undefined,
        options.flatten_tolerance,
    );
    let (sender, receiver) = std::sync::mpsc::sync_channel(1024);
    let streamed = std::thread::scope(|scope| {
        let worker = scope.spawn(move || {
            nc_to_row_stream(
                program,
                options.initial_state.as_deref(),
                options.axes.clone(),
                options.extra_axes.clone(),
                options.iteration_limit,
                options.axis_index_map.clone(),
                options.allow_undefined,
                options.flatten_tolerance,
                sender,
            )
        });
        let rows: Vec<RowSummary> = receiver.iter().map(summarize).collect();
        (worker.join().expect("the streaming worker panicked"), rows)
    });
    match (table, streamed) {
        (Ok((table, state)), (Ok(_), rows)) => {
            let mut columns = table.columns;
            columns.sort_by(|a, b| a.0.cmp(&b.0));
            Outcome::Done {
                columns,
                rows,
                state: StateSummary {
                    symbols: sorted(state.symbol_table.clone()),
                    strings: sorted(state.string_table.clone()),
                    axes: sorted(state.axes.clone()),
                    translation: sorted(state.translation.clone()),
                    rotation: state.rotation,
                    rotated: state.rotated,
                    plane: format!("{:?}", state.plane),
                    open_jump_scopes: state.jump_scopes.len(),
                },
            }
        }
        (Err(error), (Err(streamed), _)) => {
            let location = error.location();
            let failure = failure_of(
                &format!("{error:?}"),
                &error.to_string(),
                location.as_ref().map(|l| l.line),
                location.as_ref().and_then(|l| l.column),
                location.and_then(|l| l.context),
            );
            assert_eq!(
                mask_suggestion(&streamed.to_string()),
                mask_suggestion(&error.to_string()),
                "the table and stream APIs disagree"
            );
            Outcome::Failed(failure)
        }
        (table, (streamed, _)) => panic!(
            "the table and stream APIs disagree: {:?} versus {:?}",
            table.map(|_| ()),
            streamed.map(|_| ())
        ),
    }
}

/// Run `program` on both paths and assert they agree; returns the outcome.
fn parity_with(program: &str, options: &Options) -> Outcome {
    let _guard = env_lock();
    let default = run_path(program, options, false);
    let ast = run_path(program, options, true);
    if default != ast {
        panic!("NC_EXEC=ast diverges for:\n{program}\n--- default:\n{default:#?}\n--- ast:\n{ast:#?}");
    }
    default
}

fn parity(program: &str) -> Outcome {
    parity_with(program, &Options::default())
}

fn assert_fails_at(outcome: &Outcome, kind: &str, line: usize) {
    let failure = outcome.failure();
    assert_eq!(failure.kind, kind, "{failure:?}");
    assert_eq!(failure.line, Some(line), "{failure:?}");
}

/// The loop limit error carries no line.
fn assert_loop_limit(outcome: &Outcome) {
    let failure = outcome.failure();
    assert_eq!(failure.kind, "LoopLimit", "{failure:?}");
}

fn row_lines(outcome: &Outcome) -> Vec<usize> {
    outcome.rows().iter().map(|row| row.line_no).collect()
}

// ---------------------------------------------------------------- examples

fn examples_dir() -> &'static Path {
    Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/examples"))
}

/// The options of python/tests/test_expected_output.py.
fn example_options() -> Options {
    let defaults = std::fs::read_to_string(examples_dir().join("defaults.mpf")).expect("defaults.mpf");
    Options {
        initial_state: Some(defaults),
        extra_axes: Some(vec!["ELX".to_string()]),
        axis_index_map: Some(HashMap::from([("E".to_string(), 4)])),
        ..Options::default()
    }
}

#[test]
fn every_example_program() {
    let mut paths: Vec<_> = std::fs::read_dir(examples_dir())
        .expect("examples")
        .map(|entry| entry.expect("entry").path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "mpf"))
        .collect();
    paths.sort();
    assert!(paths.len() > 20, "found only {} examples", paths.len());
    for path in paths {
        let program = std::fs::read_to_string(&path).expect("example");
        let with_defaults = parity_with(&program, &example_options());
        assert!(
            matches!(with_defaults, Outcome::Done { .. }),
            "{}: {with_defaults:?}",
            path.display()
        );
        parity(&program);
    }
}

#[test]
fn flattening_example() {
    let program = std::fs::read_to_string(examples_dir().join("flattening/flatten_demo.mpf")).expect("demo");
    for tolerance in [0.1, 0.01] {
        parity_with(
            &program,
            &Options {
                flatten_tolerance: Some(tolerance),
                ..Options::default()
            },
        );
    }
    let looped = format!("R1=0\nWHILE R1<2\n{program}R1=R1+1\nENDWHILE\n").replace("M30", "");
    parity_with(
        &looped,
        &Options {
            flatten_tolerance: Some(0.1),
            ..Options::default()
        },
    );
}

// ---------------------------------------------------------- straight lines

/// Every form the byte decoder claims, plus grammar-bound neighbours.
const WORD_FORMS: &str = "\
N10 G1 X1.5 Y-2 Z.5 F1000
X=10 Y = 20
E_MULT = 0.5
F_MULTI = 2
E=0
E=IC(E_MULT*0.25) X11
E=IC(0.5*E_MULT)
E=IC(2)
X=IC(-1.5)
F=(F_MULTI*1314)
F=(2*F_MULTI)
F=(2400)
M3 M8
G90 G17
G0 X0
MATERIAL_UPDATE
SUPPORT_PIN_TRIGGER(2.5)
support_pin_trigger ( -2.5, 0, 3. )
CUSTOM()
N20 LBL_A: X5 ; comment
; only a comment
   X6
P_1 = 1+2*3
P_2 = -2*3
P_3 = 2--3
P_4 = 10/4/5
P_5 = 7 DIV 4.1
P_6 = 7 MOD 4
X=SIN(30) Y=COS(60)
Y2O
LBL_B:
X1 LAYER_CHANGE M5
N0030 X7
";

#[test]
fn straight_lines_with_every_decoder_word_form() {
    parity(&format!("{WORD_FORMS}M30\n"));
}

#[test]
fn decoder_word_forms_inside_structures() {
    // Structured programs take the tree walker by default: this compares
    // decode_ast with the whole-file grammar on every claimed line.
    parity(&format!("IF 1\n{WORD_FORMS}ENDIF\nM30\n"));
    parity(&format!("R9=0\nWHILE R9<2\n{WORD_FORMS}R9=R9+1\nENDWHILE\n"));
}

#[test]
fn empty_blank_and_comment_only_programs() {
    for program in ["", "\n", "\n\n\n", "; only a comment", "  \t\n; x\n\n"] {
        parity(program);
    }
}

#[test]
fn crlf_line_endings() {
    parity("X1\r\nX=SIN(30)\r\nR1=0\r\nWHILE R1<2 ;w\r\nX=R1\r\nR1=R1+1\r\nENDWHILE ;e\r\nX2\r\n");
    parity("N10 X1\r\nGOTOF N30\r\nN20 X2\r\nN30 X3\r\n");
}

#[test]
fn undefined_variables_with_and_without_allow() {
    assert_fails_at(&parity("X1\nX=UNDEF_VAR\n"), "UndefinedVariable", 2);
    parity_with(
        "X1\nX=UNDEF_VAR\nWHILE UNDEF_TWO<1\nUNDEF_TWO=UNDEF_TWO+1\nENDWHILE\n",
        &Options {
            allow_undefined: true,
            ..Options::default()
        },
    );
}

// ------------------------------------------------------------------- jumps

#[test]
fn labels_and_every_goto_form_forward_and_backward() {
    let outcome = parity(
        "\
N10 R1=0 R2=0
N20 GOTOF FWD
N30 X999
FWD: X1
N40 R1=R1+1
IF R1<3 GOTOB N40
gotof N60
N50 X888
N60 X2
BOTH: X3
R2=R2+1
IF R2<2 GOTO BOTH
GOTO AHEAD
X777
AHEAD: GOTOC NOWHERE
N70 IF R2<3 GOTOB 0060 ; a block number with leading zeros
",
    );
    assert!(!row_lines(&outcome).contains(&3));
}

#[test]
fn examples_goto_program_shape_with_structures() {
    parity(
        "\
N10 R1=30 R2=10 R4=3
N20 GOTOF MIDDLE
N30 X=999
N40 REPEAT_HERE: X=R1
N50 R1=R1+R2 R4=R4-1
N60 IF R4>0 GOTOB REPEAT_HERE
N70 GOTOF 90
N80 X=999
N90 GOTOC NOWHERE
N100 GOTO FINISH
N110 X=999
N120 FINISH: M30
MIDDLE: Y=1
IF 1
  X5
ENDIF
GOTOB REPEAT_HERE
",
    );
}

#[test]
fn gotoc_suppression_on_and_off_the_scope_chain() {
    parity("GOTOC MISSING\nX2\n");
    // INNER is defined only in a branch that never ran: not on the chain.
    parity("IF 1\n  GOTOC INNER\n  X1\nENDIF\nIF 0\n  INNER: X2\nENDIF\nX3\n");
    // A visible target jumps, out of the IF.
    parity("IF 1\n  GOTOC AFTER\n  X1\nENDIF ;c\nX2\nAFTER: X3\n");
}

#[test]
fn gotos_warns_and_continues() {
    parity("X1\nGOTOS\nX2\nR1=0\nWHILE R1<1\nGOTOS\nR1=1\nENDWHILE\n");
}

#[test]
fn jump_target_not_found_messages() {
    assert_fails_at(&parity("N10 X1\nGOTO TYPO\n"), "JumpTargetNotFound", 2);
    // Did-you-mean from the targets seen so far.
    assert_fails_at(&parity("TARGET1: X1\nGOTOF TARGET\n"), "JumpTargetNotFound", 2);
    // Defined, but in the wrong direction.
    assert_fails_at(&parity("BACK: X1\nGOTOF BACK\n"), "JumpTargetNotFound", 2);
    assert_fails_at(&parity("GOTOB AHEAD\nAHEAD: X1\n"), "JumpTargetNotFound", 1);
}

#[test]
fn a_jump_cannot_enter_a_body() {
    let outcome = parity("R1=0\nWHILE R1<2\n  INSIDE: X1\n  R1=R1+1\nENDWHILE\nGOTO INSIDE\n");
    assert_fails_at(&outcome, "JumpTargetNotFound", 6);
    assert!(
        outcome
            .failure()
            .message
            .as_deref()
            .unwrap_or("")
            .contains("cannot enter"),
        "{outcome:?}"
    );
    assert_fails_at(
        &parity("GOTOF INSIDE\nIF 1\n  INSIDE: X1\nENDIF\n"),
        "JumpTargetNotFound",
        1,
    );
    // The ELSE branch cannot see a label of the true branch.
    assert_fails_at(
        &parity("IF 0\n  TRUE_SIDE: X1\nELSE\n  GOTOB TRUE_SIDE\nENDIF\n"),
        "JumpTargetNotFound",
        4,
    );
}

#[test]
fn jumps_out_of_nested_structures_write_every_closer_comment() {
    let outcome = parity(
        "\
R1=0
WHILE R1<5 ;w
  FOR R2=1 TO 3 ;f
    LOOP ;l
      IF R2==2 ;i
        GOTOF OUT
      ENDIF ;ie
      X=R2
      GOTOF NEXT_PASS
    ENDLOOP ;le
    NEXT_PASS: Y=R2
  ENDFOR ;fe
  R1=R1+1
ENDWHILE ;we
OUT: X0
",
    );
    assert!(row_lines(&outcome).contains(&15));
}

#[test]
fn jump_to_a_label_on_a_closer_or_else_line() {
    parity("R1=0\nWHILE R1<3\n  R1=R1+1\n  GOTOF LAST\n  X999\nLAST: ENDWHILE\nX1\n");
    parity("IF 1\n  X1\n  GOTOF N30\n  X999\nN30 ELSE ;e\n  X2\nN40 ENDIF\nX3\n");
    parity("IF 0\n  X1\nELSE\n  GOTOF N40\n  X999\nN40 ENDIF ;c\nX3\n");
}

#[test]
fn jump_back_to_a_structure_head_reenters_it() {
    parity(
        "\
R1=0 R2=0
TOP: WHILE R1<3 ;head
  R1=R1+1
  IF R1==2
    R2=R2+1
    IF R2<3 GOTOB TOP
  ENDIF
ENDWHILE
X=R2
",
    );
    parity("R1=0\nN5 IF R1<3 ;h\n  R1=R1+1\n  GOTOB N5\nENDIF ;e\nX=R1\n");
}

#[test]
fn backward_jump_limits_per_scope() {
    // Top level: the third backward jump trips a limit of 3.
    assert_loop_limit(&parity_with("R1=0\nLBL: R1=R1+1\nIF R1<10 GOTOB LBL\n", &limit(3)));
    // Each WHILE pass is a new scope entry, so its count restarts.
    parity_with(
        "R1=0\nWHILE R1<2\n  R2=0\n  AGAIN: R2=R2+1\n  IF R2<2 GOTOB AGAIN\n  R1=R1+1\nENDWHILE\n",
        &limit(3),
    );
    assert_loop_limit(&parity_with(
        "R1=0\nWHILE R1<2\n  R2=0\n  AGAIN: R2=R2+1\n  IF R2<9 GOTOB AGAIN\n  R1=R1+1\nENDWHILE\n",
        &limit(3),
    ));
}

// -------------------------------------------------------------------- CASE

#[test]
fn case_with_and_without_default() {
    parity(
        "\
DEF INT VAR1 = 4
N30 CASE(VAR1+2) OF 7 GOTOF LABEL_1 6 GOTOF LABEL_2 DEFAULT GOTOF LABEL_3
N40 LABEL_1: X1
N50 LABEL_2: X2
N60 LABEL_3: X3
CASE(VAR1) OF 1 GOTOF ONE 2 GOTOF TWO
X4
ONE: X5
TWO: X6
CASE(VAR1) OF 1 GOTOF ONE DEFAULT GOTOF FINAL
X7
FINAL: X8
",
    );
}

#[test]
fn case_inside_a_loop() {
    parity(
        "\
R1=0
WHILE R1<4
  R1=R1+1
  CASE(R1) OF 1 GOTOF CA 2 GOTOF CB DEFAULT GOTOF CC
  CA: X1
  GOTOF DONE
  CB: X2
  GOTOF DONE
  CC: X3
  DONE: Y=R1
ENDWHILE
",
    );
}

// ----------------------------------------------------------------- IF/ELSE

#[test]
fn if_else_nesting() {
    parity(
        "\
R1=1 R2=0
N5 IF R1==1 ;outer
  X1
  IF R2==1
    Y1
  N10 ELSE ;inner else
    Y2
    IF R1>0 ;deep
      Z1
    N15 ENDIF ;deep end
  N20 ENDIF
N25 ELSE
  X2
N30 ENDIF ;outer end
IF 0 ;a
  X9
ENDIF ;b
IF 1
ELSE
ENDIF
IF 0
ELSE ;only else
  X3
ENDIF
M30
",
    );
}

#[test]
fn if_condition_forms() {
    parity(
        "\
R1=2 R2=3
IF (R1 == 2 AND R2 == 3)
  X1
ENDIF
IF NOT R1 == 5 ;n
  X2
ENDIF
IF R1 > 1 OR R2 < 0
  X3
ELSE
  X4
ENDIF
IF R1 B_AND 2
  X5
ENDIF
",
    );
}

// ------------------------------------------------------------------- loops

#[test]
fn while_for_repeat_loop_nests() {
    parity(
        "\
R1=0
WHILE R1<3 ;w
  R2=0
  REPEAT ;r
    R2=R2+1
    FOR R3=1 TO 2 ;f
      X=R1*100+R2*10+R3
    N10 ENDFOR ;fe
  N20 UNTIL R2>=2 ;re
  R1=R1+1
N30 ENDWHILE ;we
R4=0
LOOP ;l
  R4=R4+1
  IF R4>3 GOTOF OUT
N40 ENDLOOP ;le
OUT: X0
M30
",
    );
}

#[test]
fn examples_style_nested_for_with_arrays() {
    parity(
        "\
DEF INT FELD[2,3,4] = REP(1,24)
FOR nn=0 TO 1
  FOR m=0 TO 2
    FOR o=0 TO 3
      FELD[nn,m,o] = nn*100+m*10+o
      X = FELD[nn,m,o]
    ENDFOR
  ENDFOR
ENDFOR
",
    );
}

#[test]
fn while_that_runs_exactly_the_limit_fails() {
    assert_loop_limit(&parity_with("R1=0\nWHILE R1<3\nR1=R1+1\nENDWHILE\nX1\n", &limit(3)));
    parity_with("R1=0\nWHILE R1<2\nR1=R1+1\nENDWHILE\nX1\n", &limit(3));
}

#[test]
fn repeat_that_stops_on_the_limit_pass_fails() {
    assert_loop_limit(&parity_with("R1=0\nREPEAT\nR1=R1+1\nUNTIL R1>=3\nX1\n", &limit(3)));
    parity_with("R1=0\nREPEAT\nR1=R1+1\nUNTIL R1>=2\nX1\n", &limit(3));
}

#[test]
fn loop_without_exit_and_for_without_limit() {
    assert_loop_limit(&parity_with("LOOP\nX1\nENDLOOP\n", &limit(5)));
    parity_with("FOR R1=1 TO 10\nX=R1\nENDFOR\n", &limit(3));
    parity_with("R1=0\nLOOP\nR1=R1+1\nIF R1>=4 GOTOF OUT\nENDLOOP\nOUT: X1\n", &limit(5));
}

#[test]
fn for_counter_quirks() {
    // An axis counter is stored as an axis; the loop test finds no variable.
    parity("FOR X=1 TO 3\nY=1\nENDFOR\nZ1\n");
    parity("FOR R1=5 TO 3\nX=R1\nENDFOR ;never\nX9\n");
    parity("R5=2\nFOR R1=R5 TO R5*2 ;h\nX=R1\nN99 ENDFOR ;e\n");
}

#[test]
fn errors_inside_loop_bodies_and_conditions() {
    assert_fails_at(
        &parity("R1=0\nWHILE R1<3\nR1=R1+1\nX=NOPE\nENDWHILE\n"),
        "UndefinedVariable",
        4,
    );
    assert_fails_at(
        &parity("R1=0\nWHILE NOPE<3\nR1=R1+1\nENDWHILE\n"),
        "UndefinedVariable",
        2,
    );
    assert_fails_at(&parity("R1=0\nREPEAT\nR1=R1+1\nUNTIL NOPE>2\n"), "UndefinedVariable", 4);
    parity("FOR R1=1 TO NOPE\nX1\nENDFOR\n");
}

#[test]
fn actual_position_reads_inside_repeat() {
    parity("G1 X0 Y0 Z0 F100\nREPEAT\nZ=IC(1)\nUNTIL $AA_IW[Z] > 4.5\nM30\n");
    parity("TRANS Z10\nG1 Z5 F100\nREPEAT ;r\nZ=IC(1)\nR1 = $AA_IM[Z]\nUNTIL $AA_IW[Z] >= 8 ;u\nM30\n");
    assert_eq!(parity("REPEAT\nR1 = $AA_IW[Z]\nUNTIL 1\n").failure().line, Some(2));
}

#[test]
fn m30_inside_a_loop_ends_the_program() {
    let outcome = parity("R1=0\nWHILE 1 ;w\n  R1=R1+1\n  IF R1>2 ;i\n    X=R1 M30\n  ENDIF ;ie\nENDWHILE ;we\nX999\n");
    assert!(!row_lines(&outcome).contains(&8));
    parity("FOR R1=1 TO 5\nX=R1\nIF R1==3\nM30\nENDIF\nENDFOR ;f\nX999\n");
    parity("LOOP\nM17\nENDLOOP ;l\n");
}

#[test]
fn comments_on_structure_lines() {
    let outcome = parity(
        "\
R1=0
WHILE R1<2 ;head
  R1=R1+1
ENDWHILE ;tail
FOR R2=1 TO 2 ;for head
  X=R2
ENDFOR ;for tail
LOOP ;loop head
  R1=R1+1
  IF R1>4 GOTOF OUT
ENDLOOP ;loop tail
OUT: REPEAT ;repeat head
  R1=R1-1
UNTIL R1<=0 ;until tail
IF R1==0 ;if head
  Y1
ELSE ;else
  Y2
ENDIF ;endif
IF R1==1 ;if head 2
  Y3
ELSE ;else 2
  Y4
ENDIF ;endif 2
",
    );
    assert_eq!(row_lines(&outcome).iter().filter(|line| **line == 2).count(), 2);
}

#[test]
fn block_numbers_and_labels_on_closer_lines() {
    parity(
        "\
R1=0
N10 TOP: WHILE R1<2
  R1=R1+1
N20 W_END: ENDWHILE
N30 FOR R2=1 TO 2
N40 ENDFOR
N50 LOOP
  R1=R1+1
  IF R1>3 GOTOF N90
N60 ENDLOOP
N90 REPEAT
  R1=R1-1
N70 UNTIL R1<1
N80 IF R1==0
  X1
N81 ELSE
  X2
N82 ENDIF
N83 IF R1==1
  X3
N84 ELSE
  X4
N85 ENDIF
",
    );
}

#[test]
fn blank_and_label_only_lines_inside_structures() {
    parity("R1=0\nWHILE R1<2\n\n  LBL_X:\n\n  R1=R1+1\n\nENDWHILE\n\nX1\n");
}

#[test]
fn deep_nesting() {
    let mut program = String::new();
    for depth in 1..=12 {
        program.push_str(&format!("R{depth}=0\nWHILE R{depth}<2 ;d{depth}\n"));
    }
    program.push_str("X=R1+R2\n");
    for depth in (1..=12).rev() {
        program.push_str(&format!("R{depth}=R{depth}+1\nENDWHILE ;e{depth}\n"));
    }
    parity(&program);
}

// ------------------------------------------------------------------ frames

#[test]
fn frames_trans_atrans_rot_arot() {
    parity(
        "\
G1 X10 Y0 Z0 F100
TRANS X5
X1
ATRANS Y2
X2 Y2
ROT Z90
X3
AROT Z45
X4
AROT RPL=30
X5 Y1
ROT
TRANS
X5
SCALE X2
X1
MIRROR X0
X1
M30
",
    );
}

#[test]
fn frames_inside_loops() {
    parity("G1 X0 Y0 Z0 F100\nFOR R1=1 TO 3\n  ATRANS X1\n  G1 X0 Y=R1\n  AROT Z10\nENDFOR\nTRANS\nX1\n");
}

#[test]
fn frame_errors() {
    assert_fails_at(&parity("G1 X0 Y0 Z0\nG1 CROTS X0\n"), "UnsupportedStatement", 2);
    for (block, line) in [("ROT RPL=30 X10", 2), ("AROT A30", 2), ("ROT RPL=\"A\"", 2)] {
        let outcome = parity(&format!("G1 X0 Y0 Z0\n{block}\nG1 X1\n"));
        assert_eq!(outcome.failure().line, Some(line), "{block}: {outcome:?}");
    }
    let outcome = parity("G1 X0 Y0 Z0 F100\nIF 1\nAROT X30\nG2 X10 Y0 I5 J0\nENDIF\n");
    assert_eq!(outcome.failure().line, Some(4), "{outcome:?}");
}

// -------------------------------------------------------- strings and DEF

#[test]
fn strings_and_def_arrays() {
    parity(
        "\
DEF STRING[20] S1=\"abc\", S2
DEF STRING[100] WF
DEF INT IDX = 2
DEF REAL VAL = 9.654
S2 = S1 << \"def\"
WF = \"i:\" << IDX << \"/v:\" << VAL
WF = \"pre_\" << (S1) << \"_post\"
S1[1] = \"X\"
R1 = STRLEN(S2)
R2 = INDEX(S2, \"d\")
R3 = RINDEX(S2, \"c\")
R4 = MATCH(S2, \"cd\")
WF = SPRINT(\"a=%f b=%.2f c=%s\", 3.5, 3.14159, \"XY\")
WF = SUBSTR(\"abcde\", 1, 2)
R5 = NUMBER(\"12.5\")
R6 = ISNUMBER(\"x\")
DEF REAL ARR[3]
ARR[0] = 1
ARR[1] = ARR[0] + 2
DEF INT B[2,2] = SET(1,2,3,4)
DEF REAL C[3] = REP(7)
DEF REAL D[3] = REP(7,2)
X = ARR[1] + B[1,1] + C[2]
",
    );
}

#[test]
fn strings_in_structures() {
    parity(
        "\
DEF STRING[40] LOG = \"\"
DEF INT N_I
FOR N_I=1 TO 3
  LOG = LOG << N_I << \",\"
ENDFOR
IF MATCH(LOG, \"2,\") >= 0
  X1
ENDIF
R1 = STRLEN(LOG)
",
    );
}

#[test]
fn string_and_array_errors() {
    for program in [
        "DEF STRING[5] S\nS = 5\n",
        "DEF REAL A[2]\nA[2]=SET(1,2,3)\n",
        "R1 = NUMBER(\"abc\")\n",
        "X=SIN(30, 45)\n",
        "DEF STRING[5] S=\"ab\"\nS[9] = \"x\"\n",
        "WHILE 1\nDEF REAL A[2]\nA[5] = 1\nENDWHILE\n",
    ] {
        let outcome = parity(program);
        assert!(matches!(outcome, Outcome::Failed(_)), "{program}: {outcome:?}");
    }
}

// ------------------------------------------------------------ parse errors

#[test]
fn parse_errors_at_known_lines() {
    assert_fails_at(&parity("X1\nX2\nX=\nX3\n"), "parse error", 3);
    assert_fails_at(&parity("X1\nX=SIN(30)\nX2 junk(\nX3\n"), "parse error", 3);
    assert_fails_at(
        &parity("R1=0\nWHILE R1<2\nX1\nX2 junk(\nR1=R1+1\nENDWHILE\n"),
        "parse error",
        4,
    );
    // A dead branch and the lines after M30 are still parsed.
    assert_fails_at(&parity("IF 0\nX=\nENDIF\n"), "parse error", 2);
    assert_fails_at(&parity("X1\nM30\nX=\n"), "parse error", 3);
    // A parse error wins over an earlier runtime error.
    assert_fails_at(&parity("X=UNDEFINED\nX=\n"), "parse error", 2);
    // The context names the up-front parse as the default path does for the program's shape.
    let glued = "DEF REAL POSE[6] = SET(0,-20,-110,0,0,0)DEF INT COUNT = 8";
    for (program, context) in [
        (format!("G1 X1\n{glued}\nG1 X2\n"), "line parsing"),
        (
            format!("G1 X1\n{glued}\nIF COUNT == 8\nG1 X2\nENDIF\n"),
            "initial file parsing",
        ),
    ] {
        let outcome = parity(&program);
        assert_fails_at(&outcome, "parse error", 2);
        assert_eq!(outcome.failure().column, Some(41), "{outcome:?}");
        assert_eq!(outcome.failure().context.as_deref(), Some(context), "{outcome:?}");
    }
    assert_fails_at(
        &parity("R1=0\nWHILE R1<2\nR1=R1+1\nENDWHILE\nIF 1\nX=(\nENDIF\n"),
        "parse error",
        6,
    );
}

#[test]
fn structure_errors_come_from_the_shared_scan() {
    assert_fails_at(&parity("IF 1\nX1\n"), "UnmatchedStructure", 1);
    assert_fails_at(&parity("X1\nENDWHILE\n"), "UnmatchedStructure", 2);
    assert_fails_at(&parity("WHILE 1\nIF 1\nENDWHILE\n"), "UnmatchedStructure", 3);
}

#[test]
fn reserved_words_as_names_fail_alike() {
    assert_fails_at(&parity("OF=5\n"), "parse error", 1);
    assert_fails_at(&parity("UNTIL=1\n"), "UnmatchedStructure", 1);
}

// ---------------------------------------------------- expected divergences

/// Divergences the phase 1 notes list; pinned so a change is noticed.
fn both_paths(program: &str, options: &Options) -> (Outcome, Outcome) {
    let _guard = env_lock();
    (run_path(program, options, false), run_path(program, options, true))
}

#[test]
fn expected_divergence_body_on_a_head_line() {
    let (default, ast) = both_paths("R1=0\nWHILE R1<2 X1\nR1=R1+1\nENDWHILE\n", &Options::default());
    assert!(matches!(default, Outcome::Done { .. }), "{default:?}");
    assert_fails_at(&ast, "parse error", 2);
}

#[test]
fn expected_divergence_second_else() {
    let (default, ast) = both_paths("IF 1\nX1\nELSE\nX2\nELSE\nX3\nENDIF\n", &Options::default());
    assert_fails_at(&default, "parse error", 5);
    assert_fails_at(&ast, "UnmatchedStructure", 5);
}

// ------------------------------------------------------- random programs

/// splitmix64: a fixed seed gives the same corpus on every run.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }

    fn chance(&mut self, percent: u64) -> bool {
        self.below(100) < percent
    }
}

/// Random structured programs: nests of every structure, labels and block
/// numbers on any line kind, jumps in every direction and scope, comments on
/// structure lines, early ends. Loops that never end hit the limit on both
/// paths alike.
struct Generator {
    rng: Rng,
    lines: Vec<String>,
    block_number: usize,
}

impl Generator {
    fn prefix(&mut self) -> String {
        let mut prefix = String::new();
        if self.rng.chance(25) {
            self.block_number += 10;
            prefix.push_str(&format!("N{} ", self.block_number));
        }
        if self.rng.chance(20) {
            prefix.push_str(&format!("LB{}: ", self.rng.below(8)));
        }
        prefix
    }

    fn comment(&mut self) -> String {
        if self.rng.chance(30) {
            format!(" ;c{}", self.rng.below(100))
        } else {
            String::new()
        }
    }

    fn var(&mut self) -> String {
        format!("R{}", 1 + self.rng.below(4))
    }

    fn simple_line(&mut self) -> String {
        let prefix = self.prefix();
        let var = self.var();
        let body = match self.rng.below(12) {
            0 | 1 => format!("{var}={var}+1"),
            2 => format!("G1 X{} Y={var}", self.rng.below(50)),
            3 => format!("X={var}*2"),
            4 => {
                let keyword = ["GOTOF", "GOTOB", "GOTO", "GOTOC"][self.rng.below(4) as usize];
                format!("IF {var}>{} {keyword} LB{}", self.rng.below(4), self.rng.below(8))
            }
            5 => {
                let keyword = ["GOTOF", "GOTOB", "GOTO", "GOTOC"][self.rng.below(4) as usize];
                if self.rng.chance(50) {
                    format!("{keyword} LB{}", self.rng.below(8))
                } else {
                    format!("{keyword} N{}", 10 * (1 + self.rng.below(20)))
                }
            }
            6 => format!(
                "CASE({var}) OF 1 GOTOF LB{} 2 GOTOB LB{}",
                self.rng.below(8),
                self.rng.below(8)
            ),
            7 if self.rng.chance(15) => "M30".to_string(),
            8 => String::new(),
            _ => format!("Z{}", self.rng.below(9)),
        };
        format!("{prefix}{body}{}", self.comment())
    }

    fn body(&mut self, depth: usize) {
        for _ in 0..1 + self.rng.below(4) {
            if depth < 3 && self.rng.chance(30) {
                self.structure(depth + 1);
            } else {
                let line = self.simple_line();
                self.lines.push(line);
            }
        }
    }

    fn structure(&mut self, depth: usize) {
        let var = self.var();
        let bound = 1 + self.rng.below(3);
        let (head, closer) = match self.rng.below(5) {
            0 => (format!("IF {var}<{bound}"), "ENDIF".to_string()),
            1 => (format!("WHILE {var}<{bound}"), "ENDWHILE".to_string()),
            // A counter per depth: a FOR has no limit, and an inner FOR resetting
            // the outer counter would never end on either path.
            2 => (format!("FOR R{}=0 TO {bound}", 4 + depth), "ENDFOR".to_string()),
            3 => ("REPEAT".to_string(), format!("UNTIL {var}>={bound}")),
            _ => ("LOOP".to_string(), "ENDLOOP".to_string()),
        };
        let prefix = self.prefix();
        let comment = self.comment();
        self.lines.push(format!("{prefix}{head}{comment}"));
        self.body(depth);
        if head.starts_with("IF") && self.rng.chance(50) {
            let prefix = self.prefix();
            let comment = self.comment();
            self.lines.push(format!("{prefix}ELSE{comment}"));
            self.body(depth);
        }
        if !head.starts_with("IF") && !head.starts_with("FOR") {
            self.lines.push(format!("{var}={var}+1"));
        }
        let prefix = self.prefix();
        let comment = self.comment();
        self.lines.push(format!("{prefix}{closer}{comment}"));
    }

    fn program(seed: u64) -> String {
        let mut generator = Generator {
            rng: Rng(seed),
            lines: vec!["R1=0 R2=0 R3=0 R4=0".to_string()],
            block_number: 0,
        };
        for _ in 0..2 + generator.rng.below(4) {
            if generator.rng.chance(60) {
                generator.structure(1);
            } else {
                let line = generator.simple_line();
                generator.lines.push(line);
            }
        }
        generator.lines.join("\n") + "\n"
    }
}

#[test]
fn random_structured_programs() {
    let mut outcomes: HashMap<String, usize> = HashMap::new();
    for seed in 0..400 {
        let program = Generator::program(seed);
        let outcome = parity_with(&program, &limit(12));
        let key = match &outcome {
            Outcome::Done { .. } => "done".to_string(),
            Outcome::Failed(failure) => failure.kind.clone(),
        };
        *outcomes.entry(key).or_default() += 1;
    }
    // The corpus must exercise both finished runs and every jump error kind.
    for kind in ["done", "LoopLimit", "JumpTargetNotFound"] {
        assert!(outcomes.get(kind).is_some_and(|n| *n >= 10), "{outcomes:?}");
    }
}
