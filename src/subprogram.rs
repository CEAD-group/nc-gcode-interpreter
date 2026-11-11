//! Subprogram execution and externally supplied variables (#48).
//!
//! The interpreter knows no machine. Two caller-supplied extensions let it run
//! programs that depend on one:
//!
//! * subprogram texts by name: a call whose name resolves is executed with its
//!   own local scope, any other call stays an opaque output cell;
//! * an [`ExternalVariables`] provider that answers reads and receives writes
//!   of the variable names it serves (I/O, PLC data, actual positions, ...).

use crate::interpreter::SourceText;
use crate::output::BatchStreamSink;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

/// Deepest call nesting before the run fails (a recursive cycle never ends).
pub const MAX_CALL_DEPTH: usize = 16;

/// One index of an external variable access: `$A_IN[5]` or `$AA_IW[X]`.
#[derive(Debug, Clone, PartialEq)]
pub enum ExternalIndex {
    Int(i64),
    /// An axis identifier used as index, uppercased.
    Name(String),
}

/// Why a provider call did not return a value.
#[derive(Debug, Clone, PartialEq)]
pub enum ExternalError {
    /// The consumer stopped listening; the run ends quietly.
    Closed,
    /// The provider rejected the access; reported with line context.
    Failed(String),
}

/// Answers reads and receives writes of the variables the interpreter does
/// not own. Called in program order, after the rows that precede the access
/// were handed to the consumer.
pub trait ExternalVariables: Send {
    fn read(&mut self, name: &str, indices: &[ExternalIndex]) -> Result<f64, ExternalError>;
    fn write(&mut self, name: &str, indices: &[ExternalIndex], value: f64) -> Result<(), ExternalError>;
    /// The text of a served STRING variable, asked for where the program
    /// compares it with a string.
    fn read_text(&mut self, name: &str, _indices: &[ExternalIndex]) -> Result<String, ExternalError> {
        Err(ExternalError::Failed(format!("{name} is not served as a string")))
    }
    /// A frame value assigned to a served variable: `CTRANS(X, 10) : CROT(Z, 90)`.
    fn write_frame(
        &mut self,
        name: &str,
        _indices: &[ExternalIndex],
        _frame: &[FrameTerm],
    ) -> Result<(), ExternalError> {
        Err(ExternalError::Failed(format!("{name} does not accept a frame")))
    }
}

/// One function of a frame chain, with its arguments as written.
#[derive(Debug, Clone, PartialEq)]
pub struct FrameTerm {
    /// CTRANS, CROT, CROTS, CSCALE, CMIRROR or CFINE.
    pub function: String,
    pub arguments: Vec<FrameArgument>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum FrameArgument {
    /// An axis identifier, uppercased.
    Axis(String),
    Number(f64),
}

/// Caller-supplied extensions of one run.
#[derive(Default)]
pub struct Extensions {
    /// Subprogram name (case-insensitive, no extension) to its source text.
    pub subprograms: HashMap<String, String>,
    /// Variable base names the provider serves, and the provider.
    pub external: Option<(Vec<String>, Box<dyn ExternalVariables>)>,
    /// M function number to the subprogram that replaces it: the subprogram
    /// runs at the end of every block that programs the M function, as the
    /// control does for a tool-change cycle on M6.
    pub m_functions: HashMap<u32, String>,
    /// Names that must be among `subprograms` when the program calls them; a
    /// call of any other unknown name stays an opaque output cell.
    pub required_subprograms: Vec<String>,
    /// Fail the run at a SETAL call, reporting the alarm and the call path.
    pub stop_on_setal: bool,
}

/// A subprogram text with its line-start table, shared across calls.
#[derive(Clone)]
pub(crate) struct Subprogram {
    pub source: SourceText,
    pub line_offsets: Arc<[usize]>,
}

/// The variables of one active program level.
#[derive(Default, Clone, Debug)]
pub(crate) struct Frame {
    pub name: String,
    /// Base names declared at this level (DEF and parameters).
    pub locals: HashSet<String>,
    /// Same-named outer variables a declaration displaced; restored on return.
    pub shadowed_numbers: Vec<(String, f64)>,
    pub shadowed_strings: Vec<(String, String)>,
    /// This level's locals while a callee runs; restored when it returns.
    pub hidden_numbers: Vec<(String, f64)>,
    pub hidden_strings: Vec<(String, String)>,
}

/// The base name of a symbol key: `POS[2,1]` is an element of `POS`.
pub(crate) fn base_name(key: &str) -> &str {
    key.split('[').next().unwrap_or(key)
}

type SharedProvider = Arc<Mutex<Box<dyn ExternalVariables>>>;

/// Extension state carried by [`crate::state::State`].
#[derive(Clone, Default)]
pub(crate) struct ExtState {
    pub subprograms: Arc<HashMap<String, Subprogram>>,
    pub m_functions: Arc<HashMap<u32, String>>,
    pub required: Arc<HashSet<String>>,
    pub stop_on_setal: bool,
    external_names: Arc<HashSet<String>>,
    provider: Option<SharedProvider>,
    /// The batch sink to flush before a provider call, when rows are streamed.
    pub flush: Option<Arc<Mutex<BatchStreamSink>>>,
    /// Program levels, main program first. Empty until the main program starts,
    /// so initial-state definitions stay global.
    pub frames: Vec<Frame>,
    /// Active modal G function per G group, as far as the program set them;
    /// a PROC with the SAVE attribute restores these on return.
    pub modal_g: HashMap<&'static str, String>,
}

impl std::fmt::Debug for ExtState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExtState")
            .field("subprograms", &self.subprograms.keys().collect::<Vec<_>>())
            .field("external_names", &self.external_names)
            .field("frames", &self.frames)
            .finish()
    }
}

impl Extensions {
    /// Names are case-insensitive: two `subprograms` keys for one name would
    /// shadow each other.
    pub fn duplicate_subprogram(&self) -> Option<String> {
        let mut seen = HashSet::new();
        self.subprograms
            .keys()
            .map(|name| name.to_uppercase())
            .find(|name| !seen.insert(name.clone()))
    }
}

impl ExtState {
    pub fn new(extensions: Extensions) -> Self {
        let subprograms = extensions
            .subprograms
            .into_iter()
            .map(|(name, text)| {
                let line_offsets: Arc<[usize]> = text.match_indices('\n').map(|(i, _)| i).collect::<Vec<_>>().into();
                let source = SourceText::from(text);
                (name.to_uppercase(), Subprogram { source, line_offsets })
            })
            .collect();
        let (names, provider) = match extensions.external {
            Some((names, provider)) => (
                names.iter().map(|n| n.to_uppercase()).collect(),
                Some(Arc::new(Mutex::new(provider))),
            ),
            None => (HashSet::new(), None),
        };
        let m_functions = extensions
            .m_functions
            .into_iter()
            .map(|(number, name)| (number, name.to_uppercase()))
            .collect();
        ExtState {
            required: Arc::new(
                extensions
                    .required_subprograms
                    .iter()
                    .map(|n| n.to_uppercase())
                    .collect(),
            ),
            stop_on_setal: extensions.stop_on_setal,
            m_functions: Arc::new(m_functions),
            subprograms: Arc::new(subprograms),
            external_names: Arc::new(names),
            provider,
            flush: None,
            frames: Vec::new(),
            modal_g: HashMap::new(),
        }
    }

    /// Whether any extension is in use; the line fast path is bypassed then.
    pub fn active(&self) -> bool {
        !self.subprograms.is_empty()
            || self.provider.is_some()
            || !self.m_functions.is_empty()
            || !self.required.is_empty()
            || self.stop_on_setal
    }

    pub fn has_external(&self) -> bool {
        !self.external_names.is_empty()
    }

    pub fn is_external(&self, base: &str) -> bool {
        self.external_names.contains(base)
    }

    /// Hand the consumer every finished row before it is asked a question.
    fn flush_rows(&self) -> Result<(), ExternalError> {
        if let Some(sink) = &self.flush {
            sink.lock()
                .expect("batch sink mutex poisoned")
                .emit()
                .map_err(|_| ExternalError::Closed)?;
        }
        Ok(())
    }

    pub fn read(&self, name: &str, indices: &[ExternalIndex]) -> Result<f64, ExternalError> {
        let provider = self.provider.as_ref().expect("external name without provider");
        self.flush_rows()?;
        provider.lock().expect("provider mutex poisoned").read(name, indices)
    }

    pub fn write(&self, name: &str, indices: &[ExternalIndex], value: f64) -> Result<(), ExternalError> {
        let provider = self.provider.as_ref().expect("external name without provider");
        self.flush_rows()?;
        provider
            .lock()
            .expect("provider mutex poisoned")
            .write(name, indices, value)
    }

    pub fn read_text(&self, name: &str, indices: &[ExternalIndex]) -> Result<String, ExternalError> {
        let provider = self.provider.as_ref().expect("external name without provider");
        self.flush_rows()?;
        provider
            .lock()
            .expect("provider mutex poisoned")
            .read_text(name, indices)
    }

    pub fn write_frame(&self, name: &str, indices: &[ExternalIndex], frame: &[FrameTerm]) -> Result<(), ExternalError> {
        let provider = self.provider.as_ref().expect("external name without provider");
        self.flush_rows()?;
        provider
            .lock()
            .expect("provider mutex poisoned")
            .write_frame(name, indices, frame)
    }

    /// Start tracking locals: everything defined from here on belongs to the
    /// main program and is invisible to the subprograms it calls.
    pub fn begin_main(&mut self) {
        if self.active() && self.frames.is_empty() {
            self.frames.push(Frame::default());
        }
    }
}

/// Remove every entry whose base name is in `bases`, returning the entries.
fn take_matching<V>(table: &mut HashMap<String, V>, bases: &HashSet<String>) -> Vec<(String, V)> {
    let keys: Vec<String> = table
        .keys()
        .filter(|key| bases.contains(base_name(key)))
        .cloned()
        .collect();
    keys.into_iter()
        .filter_map(|key| table.remove(&key).map(|value| (key, value)))
        .collect()
}

impl crate::state::State {
    /// Declare `base` a local of the current program level. A same-named outer
    /// variable is set aside until the level returns. Called before the
    /// declaration stores its value.
    pub(crate) fn declare_local(&mut self, base: &str) {
        let Some(frame) = self.ext.frames.last_mut() else {
            return;
        };
        if !frame.locals.insert(base.to_string()) {
            return;
        }
        let mut one = HashSet::new();
        one.insert(base.to_string());
        frame
            .shadowed_numbers
            .extend(take_matching(&mut self.symbol_table, &one));
        frame
            .shadowed_strings
            .extend(take_matching(&mut self.string_table, &one));
    }

    /// Open a program level for a call: the caller's locals become invisible.
    pub(crate) fn enter_call(&mut self, name: &str) {
        if let Some(caller) = self.ext.frames.last_mut() {
            caller.hidden_numbers = take_matching(&mut self.symbol_table, &caller.locals);
            caller.hidden_strings = take_matching(&mut self.string_table, &caller.locals);
        }
        self.ext.frames.push(Frame {
            name: name.to_string(),
            ..Frame::default()
        });
    }

    /// Close the innermost program level: drop its locals, put back what they
    /// displaced, and make the caller's locals visible again.
    pub(crate) fn leave_call(&mut self) {
        let Some(frame) = self.ext.frames.pop() else {
            return;
        };
        take_matching(&mut self.symbol_table, &frame.locals);
        take_matching(&mut self.string_table, &frame.locals);
        self.symbol_table.extend(frame.shadowed_numbers);
        self.string_table.extend(frame.shadowed_strings);
        if let Some(caller) = self.ext.frames.last_mut() {
            self.symbol_table.extend(std::mem::take(&mut caller.hidden_numbers));
            self.string_table.extend(std::mem::take(&mut caller.hidden_strings));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::interpreter::nc_to_table_with;
    use crate::output::{Column, Table};

    fn run(program: &str, subprograms: &[(&str, &str)]) -> Result<(Table, crate::state::State), String> {
        run_with(program, subprograms, None)
    }

    fn run_with(
        program: &str,
        subprograms: &[(&str, &str)],
        external: Option<(Vec<String>, Box<dyn ExternalVariables>)>,
    ) -> Result<(Table, crate::state::State), String> {
        let extensions = Extensions {
            subprograms: subprograms
                .iter()
                .map(|(n, t)| (n.to_string(), t.to_string()))
                .collect(),
            external,
            // M6 runs the tool-change cycle in the tests that supply one.
            m_functions: subprograms
                .iter()
                .filter(|(name, _)| *name == "TOOLCHANGE")
                .map(|(name, _)| (6, name.to_string()))
                .collect(),
            ..Default::default()
        };
        nc_to_table_with(program, None, None, None, 1000, true, None, false, None, extensions)
            .map_err(|e| e.to_string())
    }

    fn floats(table: &Table, name: &str) -> Vec<Option<f64>> {
        match &table.columns.iter().find(|(n, _)| n == name).expect(name).1 {
            Column::Float(v) => v.clone(),
            other => panic!("{name} is not a float column: {other:?}"),
        }
    }

    fn texts(table: &Table, name: &str) -> Vec<Option<String>> {
        match &table.columns.iter().find(|(n, _)| n == name).expect(name).1 {
            Column::Str(v) => v.clone(),
            Column::Dict(d) => d.iter().map(|c| c.map(str::to_string)).collect(),
            other => panic!("{name} is not a string column: {other:?}"),
        }
    }

    #[test]
    fn call_runs_the_body_with_bound_parameters() {
        let sub = "PROC GOTO_X(REAL TARGET, REAL SCALE)\nG1 X=TARGET*SCALE\nM17\nX999\n";
        let (table, _) = run("G1 X0\nGOTO_X(5, 2)\nX1\n", &[("goto_x", sub)]).unwrap();
        // Rows: X0, the call marker, the body's move, its M17, X1.
        assert_eq!(floats(&table, "X"), [Some(0.0), None, Some(10.0), None, Some(1.0)]);
        let inside = Some("GOTO_X".to_string());
        assert_eq!(texts(&table, "subprogram"), [None, None, inside.clone(), inside, None]);
        assert_eq!(
            texts(&table, "non_returning_function_call")[1].as_deref(),
            Some("GOTO_X(5, 2)")
        );
    }

    #[test]
    fn unresolved_call_stays_an_opaque_cell() {
        let (table, _) = run("G1 X0\nOTHER(1)\n", &[("KNOWN", "G1 X5\n")]).unwrap();
        assert_eq!(
            texts(&table, "non_returning_function_call")[1].as_deref(),
            Some("OTHER(1)")
        );
        assert_eq!(floats(&table, "X"), [Some(0.0), None]);
    }

    #[test]
    fn locals_are_scoped_and_var_parameters_copy_back() {
        let sub =
            "PROC ADD(REAL INC, VAR REAL SUM, VAR REAL LIST[3])\nDEF REAL TMP=100\nSUM=SUM+INC\nLIST[1]=INC\nRET\n";
        let program =
            "DEF REAL TMP=1, TOTAL=10\nDEF REAL VALUES[3]\nADD(5, TOTAL, VALUES)\nG1 X=TMP Y=TOTAL Z=VALUES[1]\n";
        let (table, state) = run(program, &[("ADD", sub)]).unwrap();
        assert_eq!(floats(&table, "X").last(), Some(&Some(1.0)));
        assert_eq!(floats(&table, "Y").last(), Some(&Some(15.0)));
        assert_eq!(floats(&table, "Z").last(), Some(&Some(5.0)));
        assert!(
            !state.symbol_table.contains_key("INC"),
            "parameter leaked into the caller"
        );
    }

    #[test]
    fn caller_locals_are_invisible_in_the_callee() {
        let error = run("DEF REAL SECRET=1\nPEEK\n", &[("PEEK", "G1 X=SECRET\n")]).unwrap_err();
        assert!(error.contains("SECRET"), "{error}");
        assert!(error.contains("PEEK"), "{error}");
    }

    #[test]
    fn nested_calls_and_recursion_limit() {
        let outer = "PROC OUTER(REAL VAL)\nINNER(VAL+1)\nG1 Y=VAL\n";
        let inner = "PROC INNER(REAL VAL)\nG1 X=VAL\n";
        let (table, _) = run("OUTER(1)\n", &[("OUTER", outer), ("INNER", inner)]).unwrap();
        assert_eq!(
            texts(&table, "subprogram"),
            [
                None,
                Some("OUTER".to_string()),
                Some("OUTER/INNER".to_string()),
                Some("OUTER".to_string())
            ]
        );
        let lines = match &table.columns.iter().find(|(n, _)| n == "subprogram_line").unwrap().1 {
            Column::Int(v) => v.clone(),
            other => panic!("subprogram_line is not an int column: {other:?}"),
        };
        assert_eq!(lines, [None, Some(2), Some(2), Some(3)]);
        let error = run("LOOPING\n", &[("LOOPING", "LOOPING\n")]).unwrap_err();
        assert!(error.contains("nested more than"), "{error}");
    }

    #[test]
    fn repetition_count_runs_the_body_and_is_recorded() {
        let sub = "PROC STEP(VAR REAL AT)\nAT = AT + 1\nG1 X=AT\n";
        let (table, _) = run("DEF REAL POS = 0\nSTEP(POS) P=3\nL6 P2\n", &[("STEP", sub)]).unwrap();
        let moves: Vec<_> = floats(&table, "X").into_iter().flatten().collect();
        assert_eq!(moves, [1.0, 2.0, 3.0]);
        let calls: Vec<_> = texts(&table, "non_returning_function_call")
            .into_iter()
            .flatten()
            .collect();
        assert_eq!(calls, ["STEP(POS)", "L6"]);
        let counts = match &table.columns.iter().find(|(n, _)| n == "call_repetitions").unwrap().1 {
            Column::Int(v) => v.iter().flatten().copied().collect::<Vec<_>>(),
            other => panic!("call_repetitions is not an int column: {other:?}"),
        };
        assert_eq!(counts, [3, 2]);
    }

    #[test]
    fn second_call_in_a_block_is_an_error() {
        let error = run("FOO BAR\n", &[("FOO", "G1 X1\n")]).unwrap_err();
        assert!(error.contains("BAR follows the call FOO in the same block"), "{error}");
    }

    #[test]
    fn header_and_return_are_not_calls() {
        let (table, _) = run("PROC MAIN SAVE\nEXTERN FOO(INT)\nG1 X1\nRET\nX2\n", &[]).unwrap();
        assert_eq!(floats(&table, "X"), [Some(1.0)]);
        assert!(table.columns.iter().all(|(n, _)| n != "non_returning_function_call"));
        let error = run("PROC MAIN(INT A)\nG1 X1\n", &[]).unwrap_err();
        assert!(error.contains("only run as a subprogram"), "{error}");
    }

    #[test]
    fn m_function_runs_its_subprogram_after_the_block() {
        // The cycle reads the selected tool, and its own M6 does not recurse.
        let cycle = "PROC TOOLCHANGE\nG1 Z100\nM6\nG1 Z=T\n";
        let nested = "PROC ATTACH(STRING[10] KIND)\nT0 M6\nMSG(KIND)\n";
        let subprograms = [("TOOLCHANGE", cycle), ("ATTACH", nested)];
        let (table, _) = run("G1 X0 Z0\nT2 M6\nATTACH(\"SPINDLE\")\n", &subprograms).unwrap();
        assert_eq!(
            floats(&table, "Z"),
            [
                Some(0.0),
                None,
                Some(100.0),
                None,
                Some(2.0),
                None,
                None,
                Some(100.0),
                None,
                Some(0.0),
                None
            ]
        );
        let paths = texts(&table, "subprogram");
        assert_eq!(paths[2].as_deref(), Some("TOOLCHANGE"));
        assert_eq!(paths[7].as_deref(), Some("ATTACH/TOOLCHANGE"));
        assert_eq!(
            texts(&table, "non_returning_function_call").last().unwrap().as_deref(),
            Some("MSG(KIND)")
        );
        let error = run("G1 X0\nM6\n", &[("OTHER", "G1 X1\n")]);
        assert!(error.is_ok(), "an unmapped M6 stays a plain M code");
    }

    struct Recorder {
        log: Arc<Mutex<Vec<String>>>,
        input: f64,
    }

    impl ExternalVariables for Recorder {
        fn read(&mut self, name: &str, indices: &[ExternalIndex]) -> Result<f64, ExternalError> {
            self.log.lock().unwrap().push(format!("read {name}{indices:?}"));
            if name == "$FAIL" {
                return Err(ExternalError::Failed("no such signal".to_string()));
            }
            self.input += 1.0;
            Ok(self.input)
        }

        fn write(&mut self, name: &str, indices: &[ExternalIndex], value: f64) -> Result<(), ExternalError> {
            self.log
                .lock()
                .unwrap()
                .push(format!("write {name}{indices:?}={value}"));
            Ok(())
        }
    }

    #[test]
    fn external_variables_go_to_the_provider() {
        let log = Arc::new(Mutex::new(Vec::new()));
        let provider = Recorder {
            log: log.clone(),
            input: 0.0,
        };
        let names = vec![
            "$A_IN".to_string(),
            "$A_OUT".to_string(),
            "$AA_IW".to_string(),
            "$FAIL".to_string(),
        ];
        let program = "$A_OUT[3]=1\nWHILE $A_IN[5]<3\nG4 F0.1\nENDWHILE\nG1 X=$AA_IW[X]\n";
        let (table, state) = run_with(program, &[], Some((names.clone(), Box::new(provider)))).unwrap();
        assert_eq!(
            *log.lock().unwrap(),
            [
                "write $A_OUT[Int(3)]=1",
                "read $A_IN[Int(5)]",
                "read $A_IN[Int(5)]",
                "read $A_IN[Int(5)]",
                "read $AA_IW[Name(\"X\")]",
            ]
        );
        assert_eq!(floats(&table, "X").last(), Some(&Some(4.0)));
        assert!(
            !state.symbol_table.contains_key("$A_OUT[3]"),
            "an external write was stored"
        );

        let provider = Recorder { log, input: 0.0 };
        let error = run_with("G1 X=$FAIL\n", &[], Some((names, Box::new(provider)))).unwrap_err();
        assert!(error.contains("no such signal"), "{error}");
    }
}
