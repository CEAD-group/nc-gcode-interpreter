use crate::errors::ParsingError;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

/// A `HashMap` using the non-cryptographic FxHash instead of the default
/// SipHash. The hot interpreter maps (`axes`, `translation`, `symbol_table`,
/// `output_keys`) have tiny, trusted keys and are hit millions of times a run;
/// profiling the 1.1 GB file showed SipHash of these keys (esp. in
/// `update_axis`) as a top cost. FxHash needs no DoS resistance here.
pub(crate) type FxMap<K, V> = HashMap<K, V, rustc_hash::FxBuildHasher>;

/// Emit an interpreter warning to stderr. Callers pass `format_args!(...)` so
/// the message is only formatted at the point of emission.
pub fn emit_warning(args: std::fmt::Arguments) {
    eprintln!("{}", args);
}

/// Block addresses: per-block values that are emitted to the output like axes,
/// but are not axes (no translation applies) and not user variables. They are
/// non-modal: each value belongs to the block that programs it and is never
/// forward-filled onto later blocks.
///
/// Three families share these semantics:
/// * the circular/helical interpolation parameters `I`, `J`, `K` (arc-centre
///   offsets relative to the start point), `CR` (the arc-radius form) and
///   `TURN` (additional full helix turns), programmed on G2/G3 (and CIP/CT)
///   blocks;
/// * the spline programming addresses `PW` (point weight), `SD` (spline
///   degree) and `PL` (parameter interval length).
///
/// Before these were listed here the arc-centre offsets were silently dropped
/// from the output (they fell through to the user-variable branch), so arcs
/// came out as bare straight-line endpoints.
pub const BLOCK_ADDRESSES: &[&str] = &["I", "J", "K", "CR", "TURN", "PW", "SD", "PL"];

/// NC addresses the interpreter recognizes but does not implement: an
/// assignment to one of these parses as a plain user variable, so the
/// construct it belongs to is NOT interpreted and the resulting motion is
/// wrong. Each gets a loud once-per-run warning - an out-of-scope construct
/// must never be butchered silently.
const UNSUPPORTED_ADDRESSES: &[(&str, &str)] = &[
    ("AR", "arc opening angle (G2/G3 ... AR=)"),
    ("AP", "polar angle (G0..G3 AP= RP=)"),
    ("RP", "polar radius (G0..G3 AP= RP=)"),
    ("I1", "CIP intermediate point (I1= J1= K1=)"),
    ("J1", "CIP intermediate point (I1= J1= K1=)"),
    ("K1", "CIP intermediate point (I1= J1= K1=)"),
];

/// Which kind of output column an assignment key resolves to. Variables (which
/// never appear as output cells) are represented by the absence of a resolution
/// (see [`State::resolve_output_key`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColKind {
    Axis,
    Block,
}

#[derive(Debug, Clone)]
pub struct State {
    pub axes: FxMap<String, f64>,
    pub symbol_table: FxMap<String, f64>,
    /// String variables (DEF STRING[n]); kept apart from the numeric
    /// symbol_table - using one in a numeric expression is a loud error.
    pub string_table: HashMap<String, String>,
    pub translation: FxMap<String, f64>,
    pub axis_identifiers: Vec<String>,
    pub iteration_limit: usize,
    pub axis_index_map: Option<HashMap<String, usize>>,
    pub allow_undefined_variables: bool,
    /// Stack of jump-target sets (labels and block numbers), one per active
    /// `blocks` scope, innermost last. Used by GOTOC to decide whether its
    /// destination exists anywhere on the scope chain before jumping.
    pub jump_scopes: Vec<HashSet<String>>,
    /// Every jump target seen anywhere during the run (never popped), used
    /// for "did you mean" suggestions when a jump destination is not found.
    pub seen_jump_targets: HashSet<String>,
    /// Unsupported NC addresses already warned about (once per run).
    warned_addresses: HashSet<String>,
    /// Store line offsets for efficient error reporting. Shared (`Arc`) to
    /// avoid re-copying the offset table when the state is cloned.
    line_offsets: Arc<[usize]>,
    /// Store the input text for error messages. Shared (`Arc`) so cloning the
    /// state does not copy the (up to gigabyte) program buffer.
    input: Arc<str>,
    /// Registry resolving an assignment key (axis or block address) to its
    /// interned `&'static str` output-column name, so output rows carry `Copy`
    /// keys with no per-row allocation. Keyed by the uppercased name; the
    /// lookup falls back to uppercasing only when a direct (already-uppercase)
    /// hit misses. Built once at construction from the axis identifiers and the
    /// fixed block addresses.
    output_keys: FxMap<String, (ColKind, &'static str)>,
}

impl State {
    /// Creates a new State with the given axis identifiers and configuration.
    ///
    /// # Arguments
    ///
    /// * `axis_identifiers` - List of valid axis names (e.g., ["X", "Y", "Z", "E"])
    /// * `iteration_limit` - Maximum number of iterations for loops
    /// * `axis_index_map` - Optional mapping of axis names to array indices (e.g., {"E": 4})
    pub fn new(
        axis_identifiers: Vec<String>,
        iteration_limit: usize,
        axis_index_map: Option<HashMap<String, usize>>,
        allow_undefined_variables: bool,
    ) -> Self {
        let mut symbols = FxMap::default();
        symbols.insert("TRUE".to_string(), 1.0);
        symbols.insert("FALSE".to_string(), 0.0);

        let mut translation = FxMap::default();
        for axis in &axis_identifiers {
            translation.insert(axis.clone(), 0.0);
        }

        // Validate axis_index_map if provided
        if let Some(map) = &axis_index_map {
            for axis in map.keys() {
                if !axis_identifiers.contains(&axis.to_uppercase()) {
                    panic!("Axis '{}' in axis_index_map is not a valid axis", axis);
                }
            }
        }

        // Pre-resolve every output-column key to its interned &'static str
        // once, up front. Axis identifiers are case-insensitive on lookup, so
        // the registry is keyed by the uppercased name.
        let mut output_keys: FxMap<String, (ColKind, &'static str)> = FxMap::default();
        for axis in &axis_identifiers {
            let upper = axis.to_uppercase();
            let interned = crate::output::intern_column(&upper);
            output_keys.insert(upper, (ColKind::Axis, interned));
        }
        for &block in BLOCK_ADDRESSES {
            // Block addresses are constants; a direct static lookup is enough,
            // but interning keeps a single source of &'static str keys.
            output_keys
                .entry(block.to_string())
                .or_insert((ColKind::Block, crate::output::intern_column(block)));
        }

        State {
            axes: FxMap::default(),
            symbol_table: symbols,
            string_table: HashMap::new(),
            translation,
            axis_identifiers,
            iteration_limit,
            axis_index_map,
            allow_undefined_variables,
            jump_scopes: Vec::new(),
            seen_jump_targets: HashSet::new(),
            warned_addresses: HashSet::new(),
            line_offsets: Arc::from(Vec::new()),
            input: Arc::from(""),
            output_keys,
        }
    }

    /// Resolve an assignment key to its output column: `Some((kind, interned
    /// name))` for an axis or block address, or `None` for a user variable
    /// (which is never emitted as an output cell). The common case - a key
    /// already in canonical (uppercase) form - hits without allocating; only a
    /// mixed/lowercase key pays a single `to_uppercase`.
    pub fn resolve_output_key(&self, key: &str) -> Option<(ColKind, &'static str)> {
        if let Some(entry) = self.output_keys.get(key) {
            return Some(*entry);
        }
        self.output_keys.get(&key.to_uppercase()).copied()
    }

    /// Warn (once per run per address) when an assignment targets a known
    /// but unsupported NC address: the value lands in the symbol table as a
    /// user variable and the construct it programs is not interpreted.
    /// Cheap on the hot variable path: unsupported addresses are all two
    /// characters, so longer keys return before any lookup.
    pub fn warn_unsupported_address(&mut self, key: &str, line_no: usize) {
        if key.len() != 2 {
            return;
        }
        let upper = key.to_uppercase();
        if let Some((name, what)) = UNSUPPORTED_ADDRESSES.iter().find(|(n, _)| *n == upper) {
            if self.warned_addresses.insert(upper) {
                emit_warning(format_args!(
                    "Warning [line {}]: address '{}' - {} - is not interpreted and is treated as a user variable; the motion it programs will be wrong",
                    line_no, name, what
                ));
            }
        }
    }

    /// True if a jump target (canonical key) is defined in any scope on the
    /// currently active scope chain.
    pub fn jump_target_visible(&self, key: &str) -> bool {
        self.jump_scopes.iter().any(|scope| scope.contains(key))
    }

    /// Sets the input text and pre-calculates line offsets for efficient access.
    /// `Arc::from(&str)` copies the program once into the `Arc<str>` (making
    /// later `State` clones cheap). Taking `&str` lets the caller pass its
    /// existing borrow directly; the previous `set_input(input.to_string())`
    /// call first materialized a throwaway `String` - a second full copy of the
    /// whole program, a wasted gigabyte on the largest inputs.
    pub fn set_input(&mut self, input: &str) {
        self.line_offsets = input.match_indices('\n').map(|(i, _)| i).collect::<Vec<_>>().into();
        self.input = Arc::from(input);
    }

    /// Gets a line from the input by line number (1-based indexing)
    pub fn get_line(&self, line_no: usize) -> Option<&str> {
        if line_no == 0 {
            return None;
        }
        let start = if line_no == 1 {
            0
        } else {
            self.line_offsets.get(line_no - 2).map(|&i| i + 1)?
        };
        let end = self.line_offsets.get(line_no - 1).copied().unwrap_or(self.input.len());
        Some(&self.input[start..end])
    }

    /// Checks if a given key is a valid axis identifier
    pub fn is_axis(&self, key: &str) -> bool {
        self.axis_identifiers.contains(&key.to_uppercase())
    }

    /// Checks if a given key is a block address (e.g. spline PW/SD/PL)
    pub fn is_block_address(&self, key: &str) -> bool {
        BLOCK_ADDRESSES.contains(&key.to_uppercase().as_str())
    }

    /// Updates the translation value for an axis
    pub fn update_translation(
        &mut self,
        axis: &str,
        value: f64,
        line_no: usize,
        preview: &str,
    ) -> Result<(), ParsingError> {
        if self.is_axis(axis) {
            self.translation.insert(axis.to_string(), value);
            Ok(())
        } else {
            Err(ParsingError::UnexpectedAxis {
                axis: axis.to_string(),
                axes: self.axis_identifiers.join(", "),
                line_no,
                preview: preview.to_string(),
            })
        }
    }

    /// Gets the translation value for an axis
    pub fn get_translation(&self, axis: &str) -> f64 {
        *self.translation.get(axis).unwrap_or(&0.0)
    }

    /// Resets all translation values to zero (bare `TRANS` deletes the
    /// programmable frame)
    pub fn reset_translations(&mut self) {
        for value in self.translation.values_mut() {
            *value = 0.0;
        }
    }

    /// Updates an axis value in local coordinates (without translation).
    /// Returns the machine coordinate (local + translation) for output purposes.
    pub fn update_axis(&mut self, key: &str, local_value: f64) -> Result<f64, ParsingError> {
        // Store the local coordinate. Get-mut first: after the first block that
        // moves an axis, the key already exists, so the common path overwrites
        // in place and allocates no String (HashMap::insert would take the key
        // by value and allocate `key.to_string()` on every row).
        match self.axes.get_mut(key) {
            Some(slot) => *slot = local_value,
            None => {
                self.axes.insert(key.to_string(), local_value);
            }
        }
        // Return the machine coordinate for output
        let translation_value = self.get_translation(key);
        Ok(local_value + translation_value)
    }

    /// Gets the current local coordinate for an axis
    pub fn get_axis_local(&self, key: &str) -> Option<f64> {
        self.axes.get(key).copied()
    }

    /// Gets the current machine coordinate for an axis (local + translation)
    pub fn get_axis_machine(&self, key: &str) -> Option<f64> {
        self.axes.get(key).map(|local| local + self.get_translation(key))
    }

    /// Gets the array index for an axis, if a mapping exists
    pub fn get_axis_index(&self, axis: &str, line_no: usize, preview: &str) -> Result<usize, ParsingError> {
        if let Some(map) = &self.axis_index_map {
            map.get(axis).copied().ok_or_else(|| ParsingError::MissingAxisMapping {
                line_no,
                preview: preview.to_string(),
                axis: axis.to_string(),
            })
        } else {
            Err(ParsingError::MissingAxisMapping {
                line_no,
                preview: preview.to_string(),
                axis: axis.to_string(),
            })
        }
    }

    /// Snapshot the end-of-run state for the Python-facing `.state` dict. The
    /// numeric maps (`axes`, `symbol_table`, `translation`) and the string
    /// variables (`string_table`, from `DEF STRING`) are carried apart because
    /// they have different value types; the Python binding renders them as one
    /// dict `{axes, symbol_table, translation, string_table}`.
    #[allow(dead_code)]
    pub fn final_state(&self) -> FinalState {
        FinalState {
            axes: self.axes.clone(),
            symbol_table: self.symbol_table.clone(),
            translation: self.translation.clone(),
            string_table: self.string_table.clone(),
        }
    }
}

/// End-of-run interpreter state handed to Python as the `.state` dict. The
/// numeric sub-tables keep their `f64` values; `string_table` carries the
/// `DEF STRING` variables (a numeric sub-dict cannot hold them). The Python
/// binding turns this into `{axes, symbol_table, translation, string_table}`.
#[derive(Debug, Clone, Default)]
#[allow(dead_code)] // fields read only by the python-feature bindings, not the bin
pub struct FinalState {
    pub axes: FxMap<String, f64>,
    pub symbol_table: FxMap<String, f64>,
    pub translation: FxMap<String, f64>,
    pub string_table: HashMap<String, String>,
}
