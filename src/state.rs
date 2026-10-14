use crate::errors::ParsingError;
use crate::frame::{self, Matrix3, Plane};
use crate::output::CellMap;
use crate::types::Value;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

/// A `HashMap` using the non-cryptographic FxHash instead of the default
/// SipHash. Reserved for hot maps with a *closed, trusted* key vocabulary:
/// `axes`, `translation`, `output_keys` — axis/block names fixed at
/// construction, hit millions of times a run. Profiling the 1.1 GB file showed
/// SipHash of these keys (esp. in `update_axis`) as a top cost, and with a
/// closed key set there is nothing to hash-flood.
///
/// NOTE: deliberately *not* used for `symbol_table`, whose keys are
/// user-controlled, unbounded-length variable names parsed from the input.
/// That map stays on SipHash to preserve hash-flooding (DoS) resistance on
/// crafted programs; it is also cold on the CAM-flood workloads that motivated
/// this change, so the default hasher costs nothing there.
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
    /// Numeric variables. Keys are user-controlled variable names parsed from
    /// the input, so this map stays on the default SipHash hasher for
    /// hash-flooding resistance (see [`FxMap`]).
    pub symbol_table: HashMap<String, f64>,
    /// String variables (DEF STRING[n]); kept apart from the numeric
    /// symbol_table - using one in a numeric expression is a loud error.
    pub string_table: HashMap<String, String>,
    pub translation: FxMap<String, f64>,
    /// Programmable frame rotation over the geometry axes X, Y, Z (row-major):
    /// machine XYZ = rotation * local XYZ + translation. Every other axis only
    /// gets its translation, so programmed A/B/C orientations are never rotated.
    pub rotation: Matrix3,
    /// `rotation` is not the identity. The hot path checks this flag only, so
    /// programs without ROT/AROT pay nothing for the frame model.
    pub rotated: bool,
    /// Active working plane (G17 default): the RPL rotation axis and the plane
    /// an arc must keep under a rotated frame.
    pub plane: Plane,
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
    /// Interned output keys of the geometry axes X, Y, Z (`None` when the axis
    /// is not configured) and of the arc centre offsets I, J, K.
    geometry_keys: [Option<&'static str>; 3],
    arc_offset_keys: [&'static str; 3],
    /// Geometry axes already warned about for being moved by a rotation
    /// before any block positioned them (once per run per axis).
    warned_unpositioned: [bool; 3],
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
        let mut symbols: HashMap<String, f64> = HashMap::new();
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

        let geometry_keys = frame::AXIS_NAMES.map(|name| output_keys.get(name).map(|(_, key)| *key));
        let arc_offset_keys = ["I", "J", "K"].map(crate::output::intern_column);

        State {
            axes: FxMap::default(),
            symbol_table: symbols,
            string_table: HashMap::new(),
            translation,
            rotation: frame::IDENTITY,
            rotated: false,
            plane: Plane::G17,
            axis_identifiers,
            iteration_limit,
            axis_index_map,
            allow_undefined_variables,
            jump_scopes: Vec::new(),
            seen_jump_targets: HashSet::new(),
            warned_addresses: HashSet::new(),
            geometry_keys,
            arc_offset_keys,
            warned_unpositioned: [false; 3],
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

    /// Resets all translation values to zero.
    pub fn reset_translations(&mut self) {
        for value in self.translation.values_mut() {
            *value = 0.0;
        }
    }

    /// Delete the whole programmable frame, translation and rotation: what an
    /// absolute frame instruction does before applying its own values
    /// (manual 3.12.2.1, "deletes all previously programmed frames").
    pub fn reset_frame(&mut self) {
        self.reset_translations();
        self.rotation = frame::IDENTITY;
        self.rotated = false;
    }

    /// Track the working plane from a G17/G18/G19 command.
    pub fn set_plane(&mut self, code: &str) {
        if let Some(plane) = Plane::from_code(code) {
            self.plane = plane;
        }
    }

    /// Compose a rotation onto the programmable frame. `absolute` (ROT) first
    /// deletes the frame, translation included; additive (AROT) chains onto
    /// the current frame, so the angles refer to the already rotated axes
    /// (manual 3.12.2.4 example 3: AROT Y90 then AROT Z90 turns about the
    /// rotated Z).
    pub fn apply_rotation(
        &mut self,
        rotation: Matrix3,
        absolute: bool,
        line_no: usize,
        preview: &str,
    ) -> Result<(), ParsingError> {
        if absolute {
            self.reset_frame();
        }
        self.rotation = frame::multiply(&self.rotation, &rotation);
        self.rotated = !frame::is_identity(&self.rotation);
        // A geometry axis that is not configured cannot report the motion a
        // rotation would give it, so such a rotation must leave it alone.
        for (index, key) in self.geometry_keys.iter().enumerate() {
            if key.is_none() && !frame::keeps_axis(&self.rotation, index) {
                return Err(ParsingError::with_context(
                    line_no,
                    preview.to_string(),
                    "frame rotation".to_string(),
                    format!(
                        "the rotation moves geometry axis {}, which is not a configured axis ({})",
                        frame::AXIS_NAMES[index],
                        self.axis_identifiers.join(", ")
                    ),
                ));
            }
        }
        Ok(())
    }

    /// ATRANS: add an offset in the current (possibly rotated) workpiece
    /// system, so a geometry-axis delta is rotated into machine components
    /// (manual 3.12.2.1: the additive reference point is the last programmed
    /// frame; 3.12.2.4 example 2 programs ATRANS X5 after AROT Y30).
    pub fn add_translation(
        &mut self,
        axis: &str,
        delta: f64,
        line_no: usize,
        preview: &str,
    ) -> Result<(), ParsingError> {
        if !self.is_axis(axis) {
            return Err(ParsingError::UnexpectedAxis {
                axis: axis.to_string(),
                axes: self.axis_identifiers.join(", "),
                line_no,
                preview: preview.to_string(),
            });
        }
        match frame::geometry_index(axis).filter(|_| self.rotated) {
            Some(column) => {
                for row in 0..3 {
                    let Some(key) = self.geometry_keys[row] else { continue };
                    let component = self.rotation[row][column] * delta;
                    *self.translation.entry(key.to_string()).or_insert(0.0) += component;
                }
            }
            None => {
                let current = self.get_translation(axis);
                self.translation.insert(axis.to_string(), current + delta);
            }
        }
        Ok(())
    }

    /// Local X, Y, Z plus a flag per axis saying whether any block has
    /// positioned it; an unpositioned axis counts as local 0.
    fn local_geometry(&self) -> ([f64; 3], [bool; 3]) {
        let mut local = [0.0; 3];
        let mut positioned = [false; 3];
        for (index, name) in frame::AXIS_NAMES.iter().enumerate() {
            if let Some(value) = self.axes.get(*name) {
                local[index] = *value;
                positioned[index] = true;
            }
        }
        (local, positioned)
    }

    /// Machine X, Y, Z under the active frame: rotation * local + translation.
    pub fn machine_geometry(&self) -> [f64; 3] {
        let (local, _) = self.local_geometry();
        let mut machine = frame::apply(&self.rotation, local);
        for (index, name) in frame::AXIS_NAMES.iter().enumerate() {
            machine[index] += self.get_translation(name);
        }
        machine
    }

    /// Write the machine coordinate of a just-updated axis into the output row.
    /// Under a rotation a geometry axis cannot be emitted alone: the machine
    /// X, Y and Z all depend on the local X, Y and Z, so the whole triple is
    /// (re)written. Without a rotation this is the plain local + translation.
    #[inline]
    pub fn insert_machine_axis(&mut self, skey: &'static str, local_value: f64, last: &mut CellMap) {
        if self.rotated && frame::geometry_index(skey).is_some() {
            self.insert_rotated_geometry(last);
            return;
        }
        last.insert(skey, Value::Float(local_value + self.get_translation(skey)));
    }

    /// Emit every geometry axis whose machine value this block can change: the
    /// positioned ones, plus an unpositioned axis the rotation couples to a
    /// positioned one (assumed at local 0, with a once-per-run warning).
    fn insert_rotated_geometry(&mut self, last: &mut CellMap) {
        let (_, positioned) = self.local_geometry();
        let machine = self.machine_geometry();
        for row in 0..3 {
            let Some(key) = self.geometry_keys[row] else { continue };
            let coupled = (0..3)
                .any(|column| column != row && positioned[column] && self.rotation[row][column].abs() > frame::EPS);
            if !positioned[row] && !coupled {
                continue;
            }
            if !positioned[row] && !self.warned_unpositioned[row] {
                self.warned_unpositioned[row] = true;
                emit_warning(format_args!(
                    "Warning: axis {} is moved by the frame rotation before any block positioned it; assuming it starts at 0 (a real control would use the actual axis position).",
                    key
                ));
            }
            last.insert(key, Value::Float(machine[row]));
        }
    }

    /// End-of-block frame work for the arc parameters I/J/K and CR: under a
    /// rotation the centre offset vector is rotated with the frame so the
    /// row stays consistent with its machine X/Y/Z, which only models arcs
    /// whose plane stays the machine plane (rotation about the plane normal,
    /// as RPL gives). Any other rotation under an arc block is an error.
    pub fn finish_block_frame(&mut self, last: &mut CellMap, line_no: usize) -> Result<(), ParsingError> {
        if !self.rotated {
            return Ok(());
        }
        let mut offsets = [0.0; 3];
        let mut programmed = [false; 3];
        for (index, key) in self.arc_offset_keys.iter().enumerate() {
            if let Some(Value::Float(value)) = last.get(key) {
                offsets[index] = *value;
                programmed[index] = true;
            }
        }
        let has_radius = matches!(last.get("CR"), Some(Value::Float(_)));
        if !programmed.iter().any(|p| *p) && !has_radius {
            return Ok(());
        }
        let normal = self.plane.normal_axis();
        if !frame::keeps_axis(&self.rotation, normal) {
            return Err(ParsingError::UnsupportedStatement {
                line_no,
                preview: self.get_line(line_no).unwrap_or("").to_string(),
                statement: format!(
                    "A circular arc under a frame rotation that tilts the {} plane",
                    self.plane.code()
                ),
                hint: "Arcs are modeled only while the programmable frame rotates about the \
                       normal of the active plane (RPL, or ROT/AROT about that axis); a tilted \
                       arc plane is not flattened and would produce wrong coordinates."
                    .to_string(),
            });
        }
        let rotated = frame::apply(&self.rotation, offsets);
        for (index, key) in self.arc_offset_keys.iter().enumerate() {
            if programmed[index] || rotated[index].abs() > frame::EPS {
                last.insert(key, Value::Float(rotated[index]));
            }
        }
        Ok(())
    }

    /// Updates an axis value in local coordinates (without translation). The
    /// machine coordinate for the output row is derived separately by
    /// [`State::insert_machine_axis`].
    pub fn update_axis(&mut self, key: &str, local_value: f64) -> Result<(), ParsingError> {
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
        Ok(())
    }

    /// Gets the current local coordinate for an axis
    pub fn get_axis_local(&self, key: &str) -> Option<f64> {
        self.axes.get(key).copied()
    }

    /// Gets the current machine coordinate for an axis: rotation * local +
    /// translation for a geometry axis, local + translation for any other.
    pub fn get_axis_machine(&self, key: &str) -> Option<f64> {
        let local = *self.axes.get(key)?;
        if self.rotated {
            if let Some(index) = frame::geometry_index(key) {
                return Some(self.machine_geometry()[index]);
            }
        }
        Some(local + self.get_translation(key))
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
    /// dict `{axes, symbol_table, translation, rotation, string_table}`.
    #[allow(dead_code)]
    pub fn final_state(&self) -> FinalState {
        FinalState {
            axes: self.axes.clone(),
            symbol_table: self.symbol_table.clone(),
            translation: self.translation.clone(),
            rotation: self.rotation,
            string_table: self.string_table.clone(),
        }
    }
}

/// End-of-run interpreter state handed to Python as the `.state` dict. The
/// numeric sub-tables keep their `f64` values; `string_table` carries the
/// `DEF STRING` variables (a numeric sub-dict cannot hold them). The Python
/// binding turns this into `{axes, symbol_table, translation, rotation, string_table}`.
#[derive(Debug, Clone, Default)]
#[allow(dead_code)] // fields read only by the python-feature bindings, not the bin
pub struct FinalState {
    pub axes: FxMap<String, f64>,
    pub symbol_table: HashMap<String, f64>,
    pub translation: FxMap<String, f64>,
    /// Programmable frame rotation (row-major; machine = rotation * local +
    /// translation for X, Y, Z). The identity when no ROT/AROT is active.
    pub rotation: Matrix3,
    pub string_table: HashMap<String, String>,
}
