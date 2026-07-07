//! Flatten curved toolpath motions (G2/G3 arcs, ASPLINE/CSPLINE/BSPLINE
//! splines) into runs of G1 rows, keeping the output row/table format
//! unchanged.
//!
//! The flattener sits between the interpreter and the row sink: every
//! finished [`Row`] passes through [`Flattener::push`], which either passes
//! it along untouched or replaces it with a run of linear rows sampled from
//! the programmed curve. The emitted rows carry the source row's `line_no`,
//! so a visualizer still maps every sample back to the block that programmed
//! it. The interpolation parameters (`I`/`J`/`K`/`CR`) and spline addresses
//! (`PW`/`SD`/`PL`) are consumed and never appear in flattened output.
//!
//! A single knob controls the point density: `tolerance`, the maximum
//! chordal deviation (in path units, i.e. mm) between the emitted polyline
//! and the true curve.
//!
//! * Arcs are sampled uniformly in angle with the step chosen from the exact
//!   sagitta bound `s = r·(1 − cos(θ/2))`, so the deviation is strictly
//!   below the tolerance. A radius difference between start and end point
//!   (CAM rounding) is absorbed by interpolating the radius linearly, and
//!   any other axes programmed on the arc block (helix axis, extrusion, ...)
//!   are interpolated linearly over the sweep.
//! * Splines are flattened by recursive bisection: a parameter span is split
//!   until the curve, probed at 1/4, 1/2 and 3/4 of the span, stays within
//!   `tolerance` of the chord. Deviation is measured in the geometric
//!   X/Y/Z subspace when present (other channels ride along and are sampled
//!   at the same parameters).
//!
//! Spline semantics follow SINUMERIK (NC programming manual, 4.7.2):
//! * `ASPLINE` — Akima spline through the programmed points, chord-length
//!   parameterized.
//! * `CSPLINE` — cubic spline through the programmed points (natural end
//!   conditions), chord-length parameterized.
//! * `BSPLINE` — the programmed positions are control points of a clamped
//!   uniform B-spline; `PW` weights (rational), `SD` degree (default 3).
//!   A constant `PL` only rescales the parameter and never changes the
//!   curve shape, so its value is ignored.
//! * All splines start at the current position (the first point / control
//!   point) and are modal until deselected by another motion command.
//!
//! Helical `TURN=` (additional full turns, manual 3.9.7) is supported.
//!
//! Out of scope (rows pass through unchanged, with a once-per-word
//! warning): `CIP`/`CT` (the intermediate-point addresses `I1=`/`J1=`/`K1=`
//! are not captured by the parser), `POLY`, thread cutting (G33/G34/G35)
//! and involutes (INVCW/INVCCW).
//!
//! Known approximations, checked against the NC programming manual:
//! * Spline start/end conditions (`BAUTO`/`BNAT`/`BTAN`,
//!   `EAUTO`/`ENAT`/`ETAN`, manual 4.7.2) are not evaluated: CSPLINE uses
//!   natural end conditions (= BNAT/ENAT; the control default is
//!   BAUTO/EAUTO) and ASPLINE uses the standard Akima boundary
//!   extrapolation. Only the first/last spline segments differ.
//! * A/C splines are chord-length parameterized; a per-block `PL` parameter
//!   interval is not applied.
//! * The spline channels are all axes programmed in the spline blocks
//!   (`SPLINEPATH` declarations are not parsed; the control default is the
//!   first three channel axes).
//! * `G91` incremental mode is not resolved by the interpreter, so arcs and
//!   splines under G91 are wrong before flattening ever sees them.
//! * The arc-centre offsets I/J/K are always interpreted as increments
//!   relative to the start point (the control default); the `I=AC(...)`
//!   absolute form is not supported by the parser (it fails to parse rather
//!   than being misread).

use crate::errors::ParsingError;
use crate::output::{intern_column, CellMap, Row, FLATTENED_COLUMN};
use crate::state::emit_warning;
use crate::types::Value;
use std::collections::HashMap;

/// Recursion cap for the adaptive spline subdivision; 2^20 samples per spline
/// segment is far beyond any sane tolerance and bounds runaway recursion on
/// degenerate input.
const MAX_SUBDIV_DEPTH: u32 = 20;

/// Axis-identifier names that are output value columns but not path
/// coordinates: never interpolated, never part of a curve.
const NON_GEOMETRIC_AXES: &[&str] = &["N", "F", "S", "D", "T"];

/// The metric subspace for the deviation measure, when present among the
/// programmed channels.
const METRIC_AXES: &[&str] = &["X", "Y", "Z"];

#[derive(Debug, Clone, Copy, PartialEq)]
enum SplineKind {
    Akima,
    BSpline,
    Cubic,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Motion {
    Linear,
    Arc { cw: bool },
    Spline(SplineKind),
    Other,
}

/// Working planes: the two in-plane axes (a right-handed pair whose normal
/// is the third geometry axis, matching DIN 66025 arc orientation) and their
/// centre-offset addresses.
#[derive(Debug, Clone, Copy)]
struct Plane {
    axes: [&'static str; 2],
    offsets: [&'static str; 2],
}

const G17: Plane = Plane { axes: ["X", "Y"], offsets: ["I", "J"] };
const G18: Plane = Plane { axes: ["Z", "X"], offsets: ["K", "I"] };
const G19: Plane = Plane { axes: ["Y", "Z"], offsets: ["J", "K"] };

/// One buffered block of an active spline: either a point-bearing row (its
/// geometric axis cells define the next spline point / control point) or any
/// other row that arrived mid-spline and is re-emitted in place.
enum SplineItem {
    Point(Row),
    Other(Row),
}

pub struct Flattener {
    tolerance: f64,
    /// Geometric axis columns (configured axis identifiers minus the
    /// non-geometric value columns), interned.
    geometric_axes: Vec<&'static str>,
    /// Last known machine coordinate per geometric axis column.
    positions: HashMap<&'static str, f64>,
    motion: Motion,
    plane: Plane,
    /// Buffered blocks of the active spline, in program order. Non-empty
    /// only while `motion` is a spline mode.
    spline_buffer: Vec<SplineItem>,
    /// Snapshot of `positions` taken when the spline was selected: the
    /// spline's start point.
    spline_start: HashMap<&'static str, f64>,
    /// Current B-spline degree (`SD=`, modal within the spline).
    spline_degree: usize,
    /// Curve motion words already warned about (CIP, CT, POLY, ...): one
    /// warning per word per run, not one per block.
    warned_motions: Vec<String>,
}

impl Flattener {
    pub fn new(tolerance: f64, axis_identifiers: &[String]) -> Result<Self, ParsingError> {
        if !(tolerance > 0.0 && tolerance.is_finite()) {
            return Err(ParsingError::ParseError {
                message: format!("flatten tolerance must be a positive number, got {}", tolerance),
            });
        }
        let geometric_axes = axis_identifiers
            .iter()
            .map(|a| a.to_uppercase())
            .filter(|a| !NON_GEOMETRIC_AXES.contains(&a.as_str()))
            .map(|a| intern_column(&a))
            .collect();
        Ok(Flattener {
            tolerance,
            geometric_axes,
            positions: HashMap::new(),
            motion: Motion::Linear,
            plane: G17,
            spline_buffer: Vec::new(),
            spline_start: HashMap::new(),
            spline_degree: 3,
            warned_motions: Vec::new(),
        })
    }

    /// Process one interpreter row, appending the resulting output row(s) to
    /// `out`. Most rows come straight back; arc rows expand into a sampled
    /// run, spline rows are buffered until the spline is deselected.
    pub fn push(&mut self, row: Row, out: &mut Vec<Row>) {
        // A motion command on this block may deselect an active spline: the
        // buffered curve is complete and must be emitted first.
        if let Some(new_motion) = row_motion(&row) {
            if self.buffering_spline() && new_motion != self.motion {
                self.flush_spline(out);
            }
            self.motion = new_motion;
        }
        if let Some(Value::Str(plane)) = row.cells.get(intern_column("gg06_plane_select")) {
            self.plane = match plane.as_str() {
                "G18" => G18,
                "G19" => G19,
                _ => G17,
            };
        }

        match self.motion {
            Motion::Spline(_) => {
                if !self.buffering_spline() {
                    self.spline_start = self.positions.clone();
                    self.spline_degree = 3;
                }
                if let Some(Value::Float(sd)) = row.cells.get(intern_column("SD")) {
                    self.spline_degree = (*sd as usize).max(1);
                }
                let has_point = self
                    .geometric_axes
                    .iter()
                    .any(|&axis| matches!(row.cells.get(axis), Some(Value::Float(_))));
                self.track_positions(&row);
                if has_point {
                    self.spline_buffer.push(SplineItem::Point(row));
                } else {
                    self.spline_buffer.push(SplineItem::Other(row));
                }
            }
            Motion::Arc { cw } => {
                self.flatten_arc(row, cw, out);
            }
            Motion::Other => {
                // A curve interpolation the flattener does not implement
                // (CIP, CT, POLY, G33/G34/G35 threads, INVCW/INVCCW, ...):
                // the block passes through unchanged. Warn once per word.
                if let Some(Value::Str(word)) = row.cells.get("gg01_motion") {
                    if !self.warned_motions.iter().any(|w| w == word) {
                        emit_warning(format_args!(
                            "Warning [line {}]: {} interpolation is not flattened; its blocks pass through unchanged",
                            row.line_no, word
                        ));
                        self.warned_motions.push(word.clone());
                    }
                }
                self.track_positions(&row);
                out.push(row);
            }
            Motion::Linear => {
                self.track_positions(&row);
                out.push(row);
            }
        }
    }

    /// Flush any pending spline at end of program.
    pub fn finish(&mut self, out: &mut Vec<Row>) {
        if self.buffering_spline() {
            self.flush_spline(out);
        }
    }

    fn buffering_spline(&self) -> bool {
        !self.spline_buffer.is_empty()
    }

    fn track_positions(&mut self, row: &Row) {
        for &axis in &self.geometric_axes {
            if let Some(Value::Float(v)) = row.cells.get(axis) {
                self.positions.insert(axis, *v);
            }
        }
    }

    /// Pass a curve row through unchanged after a warning: the block
    /// programmed a curve the flattener cannot resolve, so the output keeps
    /// the original (un-flattened) representation.
    fn pass_through_with_warning(&mut self, row: Row, reason: &str, out: &mut Vec<Row>) {
        emit_warning(format_args!(
            "Warning [line {}]: cannot flatten block ({}); emitting it unchanged",
            row.line_no, reason
        ));
        self.track_positions(&row);
        out.push(row);
    }

    // ------------------------------------------------------------------
    // Arcs
    // ------------------------------------------------------------------

    fn flatten_arc(&mut self, row: Row, cw: bool, out: &mut Vec<Row>) {
        let plane = self.plane;
        let u_axis = intern_column(plane.axes[0]);
        let v_axis = intern_column(plane.axes[1]);

        let programs_geometry = self
            .geometric_axes
            .iter()
            .any(|&axis| matches!(row.cells.get(axis), Some(Value::Float(_))))
            || cell_float(&row, plane.offsets[0]).is_some()
            || cell_float(&row, plane.offsets[1]).is_some()
            || cell_float(&row, "CR").is_some();
        if !programs_geometry {
            // A block in modal arc mode that moves nothing (M codes, feed
            // change, comment): not an arc, pass through.
            out.push(row);
            return;
        }

        let (Some(su), Some(sv)) = (
            self.positions.get(u_axis).copied(),
            self.positions.get(v_axis).copied(),
        ) else {
            self.pass_through_with_warning(row, "arc start position is unknown", out);
            return;
        };

        let eu = cell_float(&row, u_axis).unwrap_or(su);
        let ev = cell_float(&row, v_axis).unwrap_or(sv);
        let off_u = cell_float(&row, plane.offsets[0]);
        let off_v = cell_float(&row, plane.offsets[1]);

        // Centre from I/J/K offsets (relative to the start point, the
        // SINUMERIK default) or from the CR radius form.
        let (cu, cv) = if off_u.is_some() || off_v.is_some() {
            (su + off_u.unwrap_or(0.0), sv + off_v.unwrap_or(0.0))
        } else if let Some(cr) = cell_float(&row, "CR") {
            let (mu, mv) = ((su + eu) / 2.0, (sv + ev) / 2.0);
            let (du, dv) = (eu - su, ev - sv);
            let chord = (du * du + dv * dv).sqrt();
            if chord < 1e-12 {
                self.pass_through_with_warning(row, "CR arc with coincident start and end point", out);
                return;
            }
            let r = cr.abs();
            let half = chord / 2.0;
            if r < half - 1e-9 {
                self.pass_through_with_warning(row, "CR radius is smaller than half the chord", out);
                return;
            }
            let h = (r * r - half * half).max(0.0).sqrt();
            // Unit normal to the chord, counter-clockwise (left of S->E).
            let (nu, nv) = (-dv / chord, du / chord);
            // Minor arc (CR > 0): the centre lies left of the chord for G2,
            // right for G3, so the swept side is short; CR < 0 flips it.
            let minor = cr >= 0.0;
            let side = if cw == minor { -1.0 } else { 1.0 };
            (mu + side * h * nu, mv + side * h * nv)
        } else {
            self.pass_through_with_warning(row, "arc block without I/J/K centre offsets or CR radius", out);
            return;
        };

        let r_start = ((su - cu).powi(2) + (sv - cv).powi(2)).sqrt();
        let r_end = ((eu - cu).powi(2) + (ev - cv).powi(2)).sqrt();
        if r_start < 1e-9 || r_end < 1e-9 {
            self.pass_through_with_warning(row, "arc with zero radius", out);
            return;
        }

        let a_start = (sv - cv).atan2(su - cu);
        let a_end = (ev - cv).atan2(eu - cu);
        let tau = std::f64::consts::TAU;
        let mut sweep = (a_end - a_start).rem_euclid(tau);
        // Coincident start/end programs a full circle; otherwise a (near-)
        // zero sweep is a genuine zero-length arc.
        let full_circle = (eu - su).abs() < 1e-9 && (ev - sv).abs() < 1e-9;
        if sweep < 1e-12 && full_circle {
            sweep = tau;
        }
        if cw {
            sweep -= tau; // (0, 2pi] -> (-2pi, 0], the clockwise sweep
            if full_circle && sweep > -1e-12 {
                sweep = -tau;
            }
        }
        // Helical interpolation with TURN= (NC programming manual 3.9.7):
        // the programmed number of ADDITIONAL full circles on top of the
        // start-to-end sweep.
        if let Some(turns) = cell_float(&row, "TURN") {
            let extra = turns.max(0.0).round();
            sweep += if cw { -tau * extra } else { tau * extra };
        }

        // Exact sagitta bound: deviation of a chord spanning angle theta is
        // r(1 - cos(theta/2)) <= tolerance.
        let r_max = r_start.max(r_end);
        let cos_half = (1.0 - self.tolerance / r_max).clamp(-1.0, 1.0);
        let theta_step = 2.0 * cos_half.acos();
        let segments = if theta_step > 1e-12 {
            (sweep.abs() / theta_step).ceil() as usize
        } else {
            1
        }
        .clamp(1, 10_000_000);

        // Channels interpolated linearly over the sweep: every other
        // geometric axis programmed on this block.
        let mut linear: Vec<(&'static str, f64, f64)> = Vec::new();
        for &axis in &self.geometric_axes {
            if axis == u_axis || axis == v_axis {
                continue;
            }
            if let Some(end) = cell_float(&row, axis) {
                let start = self.positions.get(axis).copied().unwrap_or(end);
                linear.push((axis, start, end));
            }
        }

        let mut emitted: Vec<Row> = Vec::with_capacity(segments);
        for k in 1..=segments {
            let f = k as f64 / segments as f64;
            let mut cells = CellMap::default();
            if k == segments {
                cells.insert(u_axis, Value::Float(eu));
                cells.insert(v_axis, Value::Float(ev));
            } else {
                let angle = a_start + sweep * f;
                let r = r_start + (r_end - r_start) * f;
                cells.insert(u_axis, Value::Float(cu + r * angle.cos()));
                cells.insert(v_axis, Value::Float(cv + r * angle.sin()));
                // Generated sample, not a programmed position.
                cells.insert(intern_column(FLATTENED_COLUMN), Value::Float(1.0));
            }
            for &(axis, start, end) in &linear {
                let value = if k == segments { end } else { start + (end - start) * f };
                cells.insert(axis, Value::Float(value));
            }
            emitted.push(Row { line_no: row.line_no, cells, variable_changes: Vec::new() });
        }

        let mut geometry: Vec<&'static str> = vec![u_axis, v_axis];
        geometry.extend(linear.iter().map(|&(axis, _, _)| axis));
        merge_aux_cells(&row, &mut emitted[0], &geometry);
        emitted[0].variable_changes = row.variable_changes.clone();

        self.track_positions(&row);
        // The final position of unprogrammed plane axes is the arc endpoint
        // (== start for a full circle), already in `positions`.
        out.append(&mut emitted);
    }

    // ------------------------------------------------------------------
    // Splines
    // ------------------------------------------------------------------

    fn flush_spline(&mut self, out: &mut Vec<Row>) {
        let Motion::Spline(kind) = self.motion else {
            self.spline_buffer.clear();
            return;
        };
        let items = std::mem::take(&mut self.spline_buffer);
        let start = std::mem::take(&mut self.spline_start);

        // The spline channels: every geometric axis programmed on any
        // point-bearing block of the spline.
        let mut channels: Vec<&'static str> = Vec::new();
        for item in &items {
            if let SplineItem::Point(row) = item {
                for &axis in &self.geometric_axes {
                    if matches!(row.cells.get(axis), Some(Value::Float(_))) && !channels.contains(&axis) {
                        channels.push(axis);
                    }
                }
            }
        }

        // Build the point list: the start position, then one point per
        // point-bearing block (unprogrammed channels hold their previous
        // value). `point_rows[i]` is the buffer index of the block that
        // programmed point i (start point: None).
        let mut points: Vec<Vec<f64>> = Vec::with_capacity(items.len() + 1);
        let mut weights: Vec<f64> = Vec::with_capacity(items.len() + 1);
        let mut point_rows: Vec<Option<usize>> = Vec::with_capacity(items.len() + 1);
        let mut current: Vec<f64> = channels
            .iter()
            .map(|&c| {
                start.get(c).copied().unwrap_or_else(|| {
                    // Channel never positioned before the spline: start it at
                    // its first programmed value (a zero-length lead-in).
                    items
                        .iter()
                        .find_map(|item| match item {
                            SplineItem::Point(row) => cell_float(row, c),
                            _ => None,
                        })
                        .unwrap_or(0.0)
                })
            })
            .collect();
        points.push(current.clone());
        weights.push(1.0);
        point_rows.push(None);
        for (index, item) in items.iter().enumerate() {
            if let SplineItem::Point(row) = item {
                for (ci, &c) in channels.iter().enumerate() {
                    if let Some(v) = cell_float(row, c) {
                        current[ci] = v;
                    }
                }
                points.push(current.clone());
                weights.push(cell_float(row, "PW").unwrap_or(1.0).max(1e-4));
                point_rows.push(Some(index));
            }
        }

        // Indices (into `channels`) of the deviation-metric subspace.
        let metric: Vec<usize> = {
            let xyz: Vec<usize> = channels
                .iter()
                .enumerate()
                .filter(|(_, c)| METRIC_AXES.contains(&**c))
                .map(|(i, _)| i)
                .collect();
            if xyz.is_empty() { (0..channels.len()).collect() } else { xyz }
        };

        // Sample the curve: per source point, the run of samples that ends
        // at (or is attributed to) that point.
        let runs: Option<Vec<Vec<Vec<f64>>>> = if points.len() < 2 {
            None
        } else {
            match kind {
                SplineKind::Akima | SplineKind::Cubic => {
                    interpolating_spline_runs(&points, &metric, self.tolerance, kind)
                }
                SplineKind::BSpline => {
                    bspline_runs(&points, &weights, self.spline_degree, &metric, self.tolerance)
                }
            }
        };

        // Emit: walk the buffered blocks in order; each point-bearing block
        // is replaced by its sampled run, everything else passes through in
        // place. `runs[i]` belongs to point i+1 == point_rows[i+1].
        let mut run_of_item: HashMap<usize, Vec<Vec<f64>>> = HashMap::new();
        if let Some(runs) = runs {
            for (pi, run) in runs.into_iter().enumerate() {
                if let Some(Some(item_index)) = point_rows.get(pi + 1) {
                    run_of_item.entry(*item_index).or_default().extend(run);
                }
            }
        }

        // The buffer index of the last point-bearing block: for a B-spline,
        // only its final sample (the curve end == last control point) is a
        // programmed position.
        let last_point_index = point_rows.iter().rev().find_map(|source| *source);

        let mut first_motion_pending = true;
        for (index, item) in items.into_iter().enumerate() {
            match item {
                SplineItem::Other(row) => out.push(row),
                SplineItem::Point(row) => {
                    let samples = run_of_item.remove(&index).unwrap_or_default();
                    let mut emitted: Vec<Row> = Vec::with_capacity(samples.len().max(1));
                    if samples.is_empty() {
                        // No curve stretch attributed to this block. For an
                        // interpolating spline that means a degenerate
                        // (duplicate / single-point) spline: keep the block as
                        // a plain move to its programmed point. A B-spline
                        // control point is not on the curve, so its block
                        // keeps only its auxiliary cells (coordinates come
                        // from the samples attributed to later blocks).
                        let mut cells = CellMap::default();
                        if kind != SplineKind::BSpline {
                            for (ci, &c) in channels.iter().enumerate() {
                                if let Some(Value::Float(_)) = row.cells.get(c) {
                                    cells.insert(c, Value::Float(points_value(&points, &point_rows, index, ci)));
                                }
                            }
                        }
                        emitted.push(Row { line_no: row.line_no, cells, variable_changes: Vec::new() });
                    } else {
                        for (si, sample) in samples.iter().enumerate() {
                            let mut cells = CellMap::default();
                            for (ci, &c) in channels.iter().enumerate() {
                                cells.insert(c, Value::Float(sample[ci]));
                            }
                            // A run through an interpolating spline ends
                            // exactly on its programmed point; a B-spline only
                            // reaches a programmed position at the curve end.
                            let is_run_end = si + 1 == samples.len();
                            let programmed = match kind {
                                SplineKind::BSpline => is_run_end && last_point_index == Some(index),
                                _ => is_run_end,
                            };
                            if !programmed {
                                cells.insert(intern_column(FLATTENED_COLUMN), Value::Float(1.0));
                            }
                            emitted.push(Row {
                                line_no: row.line_no,
                                cells,
                                variable_changes: Vec::new(),
                            });
                        }
                    }
                    merge_aux_cells(&row, &mut emitted[0], &channels);
                    emitted[0].variable_changes = row.variable_changes.clone();
                    if first_motion_pending {
                        emitted[0].cells.insert(intern_column("gg01_motion"), Value::Str("G1".to_string()));
                        first_motion_pending = false;
                    } else {
                        // A restated spline command mid-run must not leak into
                        // the flattened output.
                        emitted[0].cells.remove(intern_column("gg01_motion"));
                    }
                    out.append(&mut emitted);
                }
            }
        }
    }
}

/// The exact coordinates of the point programmed by buffer item `item_index`.
fn points_value(points: &[Vec<f64>], point_rows: &[Option<usize>], item_index: usize, channel: usize) -> f64 {
    for (pi, source) in point_rows.iter().enumerate() {
        if *source == Some(item_index) {
            return points[pi][channel];
        }
    }
    points.last().map_or(0.0, |p| p[channel])
}

fn row_motion(row: &Row) -> Option<Motion> {
    match row.cells.get(intern_column("gg01_motion")) {
        Some(Value::Str(m)) => Some(match m.as_str() {
            "G0" | "G1" => Motion::Linear,
            "G2" => Motion::Arc { cw: true },
            "G3" => Motion::Arc { cw: false },
            "ASPLINE" => Motion::Spline(SplineKind::Akima),
            "BSPLINE" => Motion::Spline(SplineKind::BSpline),
            "CSPLINE" => Motion::Spline(SplineKind::Cubic),
            _ => Motion::Other,
        }),
        _ => None,
    }
}

fn cell_float(row: &Row, key: &str) -> Option<f64> {
    match row.cells.get(key) {
        Some(Value::Float(v)) => Some(*v),
        _ => None,
    }
}

/// Copy every cell of `source` that is neither a flattened geometry channel
/// nor a consumed interpolation address onto `target` (the first row of the
/// flattened run), rewriting the motion command to G1. Feed, spindle, block
/// number, M codes, comments and all other modal G groups ride along, so the
/// flattened stream forward-fills exactly like the original.
fn merge_aux_cells(source: &Row, target: &mut Row, geometry: &[&'static str]) {
    let motion_key = intern_column("gg01_motion");
    for (&key, value) in source.cells.iter() {
        if geometry.contains(&key) || crate::state::BLOCK_ADDRESSES.contains(&key) {
            continue;
        }
        if key == motion_key {
            target.cells.insert(motion_key, Value::Str("G1".to_string()));
        } else {
            target.cells.insert(key, value.clone());
        }
    }
    if !target.cells.iter().any(|(&k, _)| k == motion_key) {
        target.cells.insert(motion_key, Value::Str("G1".to_string()));
    }
}

// ----------------------------------------------------------------------
// Curve evaluation and adaptive flattening
// ----------------------------------------------------------------------

/// Distance from `p` to the segment `[a, b]`, measured in the `metric`
/// channel subspace.
fn deviation(p: &[f64], a: &[f64], b: &[f64], metric: &[usize]) -> f64 {
    let mut dot = 0.0;
    let mut len2 = 0.0;
    for &i in metric {
        let d = b[i] - a[i];
        dot += (p[i] - a[i]) * d;
        len2 += d * d;
    }
    let t = if len2 > 0.0 { (dot / len2).clamp(0.0, 1.0) } else { 0.0 };
    let mut dist2 = 0.0;
    for &i in metric {
        let closest = a[i] + t * (b[i] - a[i]);
        dist2 += (p[i] - closest) * (p[i] - closest);
    }
    dist2.sqrt()
}

/// Recursively subdivide `[t0, t1]` until the curve stays within `tol` of
/// each chord, probing at 1/4, 1/2 and 3/4 of every candidate span. Appends
/// the accepted samples (excluding the span start) to `out`.
#[allow(clippy::too_many_arguments)]
fn adaptive_flatten(
    eval: &dyn Fn(f64) -> Vec<f64>,
    t0: f64,
    p0: &[f64],
    t1: f64,
    p1: &[f64],
    tol: f64,
    metric: &[usize],
    depth: u32,
    out: &mut Vec<Vec<f64>>,
) {
    let tm = 0.5 * (t0 + t1);
    let pm = eval(tm);
    let within = |p: &[f64]| deviation(p, p0, p1, metric) <= tol;
    let ok = depth >= MAX_SUBDIV_DEPTH
        || (within(&pm)
            && within(&eval(t0 + 0.25 * (t1 - t0)))
            && within(&eval(t0 + 0.75 * (t1 - t0))));
    if ok {
        out.push(p1.to_vec());
    } else {
        adaptive_flatten(eval, t0, p0, tm, &pm, tol, metric, depth + 1, out);
        adaptive_flatten(eval, tm, &pm, t1, p1, tol, metric, depth + 1, out);
    }
}

/// Chord-length parameter values for a point sequence, measured in the
/// metric subspace. Duplicate points get a zero-length span.
fn chord_parameters(points: &[Vec<f64>], metric: &[usize]) -> Vec<f64> {
    let mut ts = Vec::with_capacity(points.len());
    let mut t = 0.0;
    ts.push(0.0);
    for pair in points.windows(2) {
        let mut d2 = 0.0;
        for &i in metric {
            let d = pair[1][i] - pair[0][i];
            d2 += d * d;
        }
        t += d2.sqrt();
        ts.push(t);
    }
    ts
}

/// Flatten an interpolating spline (Akima or natural cubic) through
/// `points`. Returns one run of samples per segment `points[i] ->
/// points[i+1]` (each run ends exactly at `points[i+1]`), or `None` when the
/// path is degenerate (all points coincide).
fn interpolating_spline_runs(
    points: &[Vec<f64>],
    metric: &[usize],
    tol: f64,
    kind: SplineKind,
) -> Option<Vec<Vec<Vec<f64>>>> {
    let ts = chord_parameters(points, metric);
    if *ts.last().unwrap() <= 0.0 {
        return None;
    }
    // Collapse duplicate parameters (zero chords) for fitting; remember which
    // original segments they belong to.
    let mut kept: Vec<usize> = vec![0];
    for i in 1..points.len() {
        if ts[i] - ts[kept[kept.len() - 1]] > 1e-12 {
            kept.push(i);
        }
    }
    if kept.len() < 2 {
        return None;
    }
    let fit_ts: Vec<f64> = kept.iter().map(|&i| ts[i]).collect();
    let channels = points[0].len();
    let fit_points: Vec<Vec<f64>> = kept.iter().map(|&i| points[i].clone()).collect();

    // Per-channel Hermite slopes at the kept points.
    let slopes: Vec<Vec<f64>> = (0..channels)
        .map(|c| {
            let ys: Vec<f64> = fit_points.iter().map(|p| p[c]).collect();
            match kind {
                SplineKind::Akima => akima_slopes(&fit_ts, &ys),
                _ => natural_cubic_slopes(&fit_ts, &ys),
            }
        })
        .collect();

    let eval = |t: f64| -> Vec<f64> {
        // Locate the fit segment containing t.
        let mut seg = fit_ts.len() - 2;
        for w in 0..fit_ts.len() - 1 {
            if t <= fit_ts[w + 1] {
                seg = w;
                break;
            }
        }
        let h = fit_ts[seg + 1] - fit_ts[seg];
        let s = ((t - fit_ts[seg]) / h).clamp(0.0, 1.0);
        let (s2, s3) = (s * s, s * s * s);
        let (h00, h10, h01, h11) =
            (2.0 * s3 - 3.0 * s2 + 1.0, s3 - 2.0 * s2 + s, -2.0 * s3 + 3.0 * s2, s3 - s2);
        (0..channels)
            .map(|c| {
                h00 * fit_points[seg][c]
                    + h10 * h * slopes[c][seg]
                    + h01 * fit_points[seg + 1][c]
                    + h11 * h * slopes[c][seg + 1]
            })
            .collect()
    };

    // One run per original segment; zero-length segments produce a single
    // repeated sample so every source block still lands on its point.
    let mut runs: Vec<Vec<Vec<f64>>> = Vec::with_capacity(points.len() - 1);
    for i in 0..points.len() - 1 {
        if ts[i + 1] - ts[i] <= 1e-12 {
            runs.push(vec![points[i + 1].clone()]);
        } else {
            let mut run = Vec::new();
            adaptive_flatten(&eval, ts[i], &points[i], ts[i + 1], &points[i + 1], tol, metric, 0, &mut run);
            runs.push(run);
        }
    }
    Some(runs)
}

/// Akima slope estimates on a (strictly increasing) parameterization, with
/// the standard two-point extrapolation at the boundaries.
fn akima_slopes(ts: &[f64], ys: &[f64]) -> Vec<f64> {
    let n = ts.len();
    if n == 2 {
        let m = (ys[1] - ys[0]) / (ts[1] - ts[0]);
        return vec![m, m];
    }
    // Segment slopes with two virtual segments extrapolated on each side.
    let mut m = Vec::with_capacity(n + 3);
    m.push(0.0); // placeholder m[-2]
    m.push(0.0); // placeholder m[-1]
    for i in 0..n - 1 {
        m.push((ys[i + 1] - ys[i]) / (ts[i + 1] - ts[i]));
    }
    m[1] = 2.0 * m[2] - m[3]; // m[-1]
    m[0] = 2.0 * m[1] - m[2]; // m[-2]
    let m_n = 2.0 * m[n] - m[n - 1];
    m.push(m_n);
    m.push(2.0 * m_n - m[n]);

    (0..n)
        .map(|i| {
            // Slopes m[i-2..i+2] live at m[i..i+4] after the offset.
            let (m1, m2, m3, m4) = (m[i], m[i + 1], m[i + 2], m[i + 3]);
            let w1 = (m4 - m3).abs();
            let w2 = (m2 - m1).abs();
            if w1 + w2 > 1e-12 {
                (w1 * m2 + w2 * m3) / (w1 + w2)
            } else {
                0.5 * (m2 + m3)
            }
        })
        .collect()
}

/// Endpoint slopes of the natural cubic spline through `(ts, ys)`: solve the
/// tridiagonal system for the second derivatives (natural end conditions),
/// then convert to first derivatives at the knots.
fn natural_cubic_slopes(ts: &[f64], ys: &[f64]) -> Vec<f64> {
    let n = ts.len();
    if n == 2 {
        let m = (ys[1] - ys[0]) / (ts[1] - ts[0]);
        return vec![m, m];
    }
    // Thomas algorithm on the interior equations; sigma[0] = sigma[n-1] = 0.
    let mut sub = vec![0.0; n];
    let mut diag = vec![1.0; n];
    let mut sup = vec![0.0; n];
    let mut rhs = vec![0.0; n];
    for i in 1..n - 1 {
        let h0 = ts[i] - ts[i - 1];
        let h1 = ts[i + 1] - ts[i];
        sub[i] = h0;
        diag[i] = 2.0 * (h0 + h1);
        sup[i] = h1;
        rhs[i] = 6.0 * ((ys[i + 1] - ys[i]) / h1 - (ys[i] - ys[i - 1]) / h0);
    }
    for i in 1..n {
        let w = sub[i] / diag[i - 1];
        diag[i] -= w * sup[i - 1];
        rhs[i] -= w * rhs[i - 1];
    }
    let mut sigma = vec![0.0; n];
    for i in (1..n - 1).rev() {
        sigma[i] = (rhs[i] - sup[i] * sigma[i + 1]) / diag[i];
    }
    (0..n)
        .map(|i| {
            if i < n - 1 {
                let h = ts[i + 1] - ts[i];
                (ys[i + 1] - ys[i]) / h - h * (2.0 * sigma[i] + sigma[i + 1]) / 6.0
            } else {
                let h = ts[i] - ts[i - 1];
                (ys[i] - ys[i - 1]) / h + h * (sigma[i - 1] + 2.0 * sigma[i]) / 6.0
            }
        })
        .collect()
}

/// Flatten a clamped uniform (rational) B-spline whose control points are
/// `points` with weights `weights`. Returns one run of samples per control
/// point after the first (span j is attributed to control point
/// `min(j + degree, n - 1)`, the last point whose block influences it), so
/// every source block owns a contiguous stretch of the curve.
fn bspline_runs(
    points: &[Vec<f64>],
    weights: &[f64],
    degree: usize,
    metric: &[usize],
    tol: f64,
) -> Option<Vec<Vec<Vec<f64>>>> {
    let n = points.len();
    let p = degree.min(n - 1).max(1);
    let channels = points[0].len();

    // Clamped uniform knot vector on [0, n - p].
    let spans = n - p;
    let mut knots = Vec::with_capacity(n + p + 1);
    knots.extend(std::iter::repeat_n(0.0, p + 1));
    for i in 1..spans {
        knots.push(i as f64);
    }
    knots.extend(std::iter::repeat_n(spans as f64, p + 1));

    // Homogeneous control points (channel * w, ..., w).
    let ctrl: Vec<Vec<f64>> = points
        .iter()
        .zip(weights)
        .map(|(pt, &w)| {
            let mut h: Vec<f64> = pt.iter().map(|v| v * w).collect();
            h.push(w);
            h
        })
        .collect();

    let eval = move |t: f64| -> Vec<f64> {
        let t = t.clamp(0.0, spans as f64);
        // Knot span index k with knots[k] <= t < knots[k+1].
        let k = if t >= spans as f64 { n - 1 } else { p + t as usize };
        // de Boor on the homogeneous coordinates.
        let mut d: Vec<Vec<f64>> = (0..=p).map(|j| ctrl[j + k - p].clone()).collect();
        for r in 1..=p {
            for j in (r..=p).rev() {
                let i = j + k - p;
                let denom = knots[i + p + 1 - r] - knots[i];
                let alpha = if denom.abs() < 1e-30 { 0.0 } else { (t - knots[i]) / denom };
                // Indexing two rows of `d` at once: a plain indexed loop is
                // clearer than a split_at_mut dance.
                #[allow(clippy::needless_range_loop)]
                for c in 0..=channels {
                    let previous = d[j - 1][c];
                    d[j][c] = (1.0 - alpha) * previous + alpha * d[j][c];
                }
            }
        }
        let w = d[p][channels];
        d[p][..channels].iter().map(|v| v / w).collect()
    };

    // Flatten span by span; a span's samples belong to the last control
    // point influencing it.
    let mut runs: Vec<Vec<Vec<f64>>> = vec![Vec::new(); n - 1];
    let mut prev_t = 0.0;
    let mut prev_p = eval(0.0);
    for j in 0..spans {
        let t1 = (j + 1) as f64;
        let p1 = eval(t1);
        let owner = (j + p).min(n - 1) - 1;
        adaptive_flatten(&eval, prev_t, &prev_p, t1, &p1, tol, metric, 0, &mut runs[owner]);
        prev_t = t1;
        prev_p = p1;
    }
    // Guarantee the curve endpoint is owned by the last block.
    if runs[n - 2].is_empty() {
        runs[n - 2].push(points[n - 1].clone());
    }
    Some(runs)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn axis_ids() -> Vec<String> {
        ["X", "Y", "Z", "A", "E", "F", "S", "N"].iter().map(|s| s.to_string()).collect()
    }

    fn flattener(tol: f64) -> Flattener {
        Flattener::new(tol, &axis_ids()).unwrap()
    }

    fn row(line_no: usize, cells: &[(&str, Value)]) -> Row {
        let mut map = CellMap::default();
        for (key, value) in cells {
            map.insert(intern_column(key), value.clone());
        }
        Row { line_no, cells: map, variable_changes: Vec::new() }
    }

    fn f(v: f64) -> Value {
        Value::Float(v)
    }

    fn s(v: &str) -> Value {
        Value::Str(v.to_string())
    }

    fn xy(row: &Row) -> (f64, f64) {
        (cell_float(row, "X").unwrap(), cell_float(row, "Y").unwrap())
    }

    fn run(fl: &mut Flattener, rows: Vec<Row>) -> Vec<Row> {
        let mut out = Vec::new();
        for r in rows {
            fl.push(r, &mut out);
        }
        fl.finish(&mut out);
        out
    }

    #[test]
    fn linear_rows_pass_through() {
        let mut fl = flattener(0.01);
        let rows = vec![
            row(1, &[("gg01_motion", s("G1")), ("X", f(0.0)), ("Y", f(0.0))]),
            row(2, &[("X", f(10.0)), ("Y", f(5.0))]),
        ];
        let out = run(&mut fl, rows);
        assert_eq!(out.len(), 2);
        assert_eq!(xy(&out[1]), (10.0, 5.0));
    }

    #[test]
    fn semicircle_stays_within_tolerance() {
        let tol = 0.01;
        let mut fl = flattener(tol);
        let rows = vec![
            row(1, &[("gg01_motion", s("G1")), ("X", f(0.0)), ("Y", f(0.0))]),
            row(2, &[("gg01_motion", s("G2")), ("X", f(100.0)), ("Y", f(0.0)), ("I", f(50.0)), ("J", f(0.0))]),
        ];
        let out = run(&mut fl, rows);
        assert!(out.len() > 10, "expected a sampled run, got {} rows", out.len());
        // Endpoint exact.
        let last = out.last().unwrap();
        assert_eq!(xy(last), (100.0, 0.0));
        // All samples on the circle of radius 50 around (50, 0); midpoint
        // sagitta of every chord within tolerance.
        let mut prev = (0.0, 0.0);
        for r in &out[1..] {
            let (x, y) = xy(r);
            let radius = ((x - 50.0).powi(2) + y.powi(2)).sqrt();
            assert!((radius - 50.0).abs() < 1e-9, "sample off circle: r={radius}");
            // G2 from (0,0) with centre (50,0): clockwise starts at the
            // 9-o'clock position and sweeps over the top -> y >= 0.
            assert!(y >= -1e-9, "G2 arc should sweep through positive Y, got y={y}");
            let (mx, my) = ((x + prev.0) / 2.0, (y + prev.1) / 2.0);
            let sagitta = 50.0 - ((mx - 50.0).powi(2) + my.powi(2)).sqrt();
            assert!(sagitta <= tol + 1e-9, "chord deviation {sagitta} > {tol}");
            prev = (x, y);
        }
        // Motion rewritten to G1 on the first emitted row, I/J consumed.
        assert!(matches!(out[1].cells.get("gg01_motion"), Some(Value::Str(m)) if m == "G1"));
        assert!(out.iter().all(|r| r.cells.get("I").is_none() && r.cells.get("J").is_none()));
    }

    #[test]
    fn ccw_arc_sweeps_negative_y() {
        let mut fl = flattener(0.01);
        let rows = vec![
            row(1, &[("gg01_motion", s("G1")), ("X", f(0.0)), ("Y", f(0.0))]),
            row(2, &[("gg01_motion", s("G3")), ("X", f(100.0)), ("Y", f(0.0)), ("I", f(50.0)), ("J", f(0.0))]),
        ];
        let out = run(&mut fl, rows);
        assert!(out[2..].iter().all(|r| cell_float(r, "Y").unwrap() <= 1e-9));
    }

    #[test]
    fn full_circle_from_coincident_endpoints() {
        let mut fl = flattener(0.1);
        let rows = vec![
            row(1, &[("gg01_motion", s("G1")), ("X", f(0.0)), ("Y", f(0.0))]),
            row(2, &[("gg01_motion", s("G2")), ("X", f(0.0)), ("Y", f(0.0)), ("I", f(25.0)), ("J", f(0.0))]),
        ];
        let out = run(&mut fl, rows);
        // Sweep of 2*pi at r=25 with tol=0.1: at least 20-some segments.
        assert!(out.len() > 20);
        assert_eq!(xy(out.last().unwrap()), (0.0, 0.0));
        // Passes through the far side of the circle.
        assert!(out.iter().skip(2).any(|r| cell_float(r, "X").unwrap() > 49.0));
    }

    #[test]
    fn cr_radius_form_minor_and_major() {
        let mut fl = flattener(0.01);
        let rows = vec![
            row(1, &[("gg01_motion", s("G1")), ("X", f(0.0)), ("Y", f(0.0))]),
            row(2, &[("gg01_motion", s("G2")), ("X", f(40.0)), ("Y", f(0.0)), ("CR", f(20.0))]),
        ];
        let out = run(&mut fl, rows);
        // CR=20 with chord 40: semicircle, clockwise -> over the top, apex +20.
        let max_y = out[1..].iter().map(|r| cell_float(r, "Y").unwrap()).fold(f64::MIN, f64::max);
        assert!((max_y - 20.0).abs() < 0.05, "expected semicircle apex at +20, got {max_y}");
        assert_eq!(xy(out.last().unwrap()), (40.0, 0.0));

        // Negative CR selects the major arc: sweep > half turn.
        let mut fl = flattener(0.01);
        let rows = vec![
            row(1, &[("gg01_motion", s("G1")), ("X", f(0.0)), ("Y", f(0.0))]),
            row(2, &[("gg01_motion", s("G2")), ("X", f(20.0)), ("Y", f(0.0)), ("CR", f(-20.0))]),
        ];
        let out = run(&mut fl, rows);
        let max_y = out[1..].iter().map(|r| cell_float(r, "Y").unwrap()).fold(f64::MIN, f64::max);
        assert!(max_y > 30.0, "major arc should swing well above the chord, got {max_y}");
    }

    #[test]
    fn helical_arc_interpolates_z_linearly() {
        let mut fl = flattener(0.01);
        let rows = vec![
            row(1, &[("gg01_motion", s("G1")), ("X", f(0.0)), ("Y", f(0.0)), ("Z", f(0.0))]),
            row(2, &[("gg01_motion", s("G2")), ("X", f(0.0)), ("Y", f(0.0)), ("Z", f(10.0)), ("I", f(25.0)), ("J", f(0.0))]),
        ];
        let out = run(&mut fl, rows);
        let zs: Vec<f64> = out[1..].iter().map(|r| cell_float(r, "Z").unwrap()).collect();
        assert!((zs.last().unwrap() - 10.0).abs() < 1e-12);
        assert!(zs.windows(2).all(|w| w[1] > w[0]), "Z must rise monotonically");
        // Z is proportional to swept angle: sample k of n has z = 10*k/n.
        let n = zs.len() as f64;
        for (k, z) in zs.iter().enumerate() {
            assert!((z - 10.0 * (k + 1) as f64 / n).abs() < 1e-9);
        }
    }

    #[test]
    fn g18_arc_uses_zx_plane() {
        let mut fl = flattener(0.01);
        let rows = vec![
            row(1, &[("gg01_motion", s("G1")), ("gg06_plane_select", s("G18")), ("X", f(0.0)), ("Y", f(0.0)), ("Z", f(0.0))]),
            row(2, &[("gg01_motion", s("G2")), ("Z", f(0.0)), ("X", f(40.0)), ("K", f(0.0)), ("I", f(20.0))]),
        ];
        let out = run(&mut fl, rows);
        assert!(out.len() > 5);
        // Circle in ZX around (z, x) = (0, 20).
        for r in &out[1..] {
            let z = cell_float(r, "Z").unwrap();
            let x = cell_float(r, "X").unwrap();
            let radius = (z * z + (x - 20.0).powi(2)).sqrt();
            assert!((radius - 20.0).abs() < 1e-9);
        }
    }

    #[test]
    fn g19_arc_uses_yz_plane() {
        let mut fl = flattener(0.01);
        let rows = vec![
            row(1, &[("gg01_motion", s("G1")), ("gg06_plane_select", s("G19")), ("X", f(0.0)), ("Y", f(0.0)), ("Z", f(0.0))]),
            row(2, &[("gg01_motion", s("G3")), ("Y", f(40.0)), ("Z", f(0.0)), ("J", f(20.0)), ("K", f(0.0))]),
        ];
        let out = run(&mut fl, rows);
        assert!(out.len() > 5);
        // Circle in YZ around (y, z) = (20, 0); X untouched.
        for r in &out[1..] {
            let y = cell_float(r, "Y").unwrap();
            let z = cell_float(r, "Z").unwrap();
            assert!(((y - 20.0).powi(2) + z * z).sqrt() - 20.0 < 1e-9);
            assert!(r.cells.get("X").is_none());
        }
        assert_eq!(
            (cell_float(out.last().unwrap(), "Y").unwrap(), cell_float(out.last().unwrap(), "Z").unwrap()),
            (40.0, 0.0)
        );
    }

    #[test]
    fn cr_with_g3_mirrors_g2() {
        // G3 CR=20 over a 40 chord: semicircle counter-clockwise -> dips to -20.
        let mut fl = flattener(0.01);
        let rows = vec![
            row(1, &[("gg01_motion", s("G1")), ("X", f(0.0)), ("Y", f(0.0))]),
            row(2, &[("gg01_motion", s("G3")), ("X", f(40.0)), ("Y", f(0.0)), ("CR", f(20.0))]),
        ];
        let out = run(&mut fl, rows);
        let min_y = out[1..].iter().map(|r| cell_float(r, "Y").unwrap()).fold(f64::MAX, f64::min);
        assert!((min_y + 20.0).abs() < 0.05, "G3 semicircle should dip to -20, got {min_y}");
        assert_eq!(xy(out.last().unwrap()), (40.0, 0.0));
    }

    #[test]
    fn full_circle_in_g18() {
        let mut fl = flattener(0.1);
        let rows = vec![
            row(1, &[("gg01_motion", s("G1")), ("gg06_plane_select", s("G18")), ("X", f(0.0)), ("Y", f(0.0)), ("Z", f(0.0))]),
            row(2, &[("gg01_motion", s("G2")), ("Z", f(0.0)), ("X", f(0.0)), ("K", f(25.0)), ("I", f(0.0))]),
        ];
        let out = run(&mut fl, rows);
        assert!(out.len() > 20, "full circle in G18 under-sampled: {}", out.len());
        // Passes through the far side (z = 50).
        assert!(out[1..].iter().any(|r| cell_float(r, "Z").unwrap() > 49.0));
    }

    /// TURN= adds full helix turns (NC programming manual 3.9.7).
    #[test]
    fn turn_adds_full_helix_turns() {
        let helix = |turn: Option<f64>| {
            let mut fl = flattener(0.1);
            let mut cells = vec![
                ("gg01_motion", s("G2")),
                ("X", f(0.0)),
                ("Y", f(0.0)),
                ("Z", f(30.0)),
                ("I", f(25.0)),
                ("J", f(0.0)),
            ];
            if let Some(v) = turn {
                cells.push(("TURN", f(v)));
            }
            run(
                &mut fl,
                vec![
                    row(1, &[("gg01_motion", s("G1")), ("X", f(0.0)), ("Y", f(0.0)), ("Z", f(0.0))]),
                    row(2, &cells),
                ],
            )
        };
        let single = helix(None).len();
        let out = helix(Some(2.0));
        // Sweep of 3 full circles instead of 1: ~3x the samples, and X crosses
        // the far side of the circle 3 times.
        assert!(out.len() > 2 * single, "TURN=2 should triple the sweep: {} vs {single}", out.len());
        let far_crossings = out[1..]
            .windows(2)
            .filter(|w| {
                let a = cell_float(&w[0], "X").unwrap();
                let b = cell_float(&w[1], "X").unwrap();
                (a < 49.0) != (b < 49.0)
            })
            .count();
        assert!(far_crossings >= 5, "expected 3 far-side passes, got {far_crossings} crossings");
        // Z still lands exactly, monotonically.
        let zs: Vec<f64> = out[1..].iter().map(|r| cell_float(r, "Z").unwrap()).collect();
        assert!((zs.last().unwrap() - 30.0).abs() < 1e-9);
        assert!(zs.windows(2).all(|w| w[1] > w[0]));
        // TURN is consumed like the other interpolation addresses.
        assert!(out.iter().all(|r| r.cells.get("TURN").is_none()));
    }

    /// CIP and other unimplemented curve interpolations pass through
    /// unchanged (with a once-per-word warning).
    #[test]
    fn cip_passes_through_unchanged() {
        let mut fl = flattener(0.01);
        let rows = vec![
            row(1, &[("gg01_motion", s("G1")), ("X", f(0.0)), ("Y", f(0.0))]),
            row(2, &[("gg01_motion", s("CIP")), ("X", f(20.0)), ("Y", f(0.0)), ("I", f(10.0)), ("J", f(5.0))]),
            row(3, &[("gg01_motion", s("G1")), ("X", f(30.0)), ("Y", f(0.0))]),
        ];
        let out = run(&mut fl, rows);
        assert_eq!(out.len(), 3);
        assert!(matches!(out[1].cells.get("gg01_motion"), Some(Value::Str(m)) if m == "CIP"));
        assert!(out[1].cells.get("I").is_some(), "CIP block must keep its parameters");
    }

    #[test]
    fn tighter_tolerance_gives_more_arc_points() {
        let arc = |tol: f64| {
            let mut fl = flattener(tol);
            run(
                &mut fl,
                vec![
                    row(1, &[("gg01_motion", s("G1")), ("X", f(0.0)), ("Y", f(0.0))]),
                    row(2, &[("gg01_motion", s("G3")), ("X", f(100.0)), ("Y", f(0.0)), ("I", f(50.0)), ("J", f(0.0))]),
                ],
            )
            .len()
        };
        assert!(arc(0.001) > 2 * arc(0.1));
    }

    #[test]
    fn arc_without_parameters_passes_through_with_warning() {
        let mut fl = flattener(0.01);
        let rows = vec![
            row(1, &[("gg01_motion", s("G1")), ("X", f(0.0)), ("Y", f(0.0))]),
            row(2, &[("gg01_motion", s("G2")), ("X", f(10.0)), ("Y", f(0.0))]),
        ];
        let out = run(&mut fl, rows);
        assert_eq!(out.len(), 2);
        assert!(matches!(out[1].cells.get("gg01_motion"), Some(Value::Str(m)) if m == "G2"));
    }

    #[test]
    fn aux_cells_ride_on_first_arc_row() {
        let mut fl = flattener(0.01);
        let rows = vec![
            row(1, &[("gg01_motion", s("G1")), ("X", f(0.0)), ("Y", f(0.0))]),
            row(
                2,
                &[
                    ("gg01_motion", s("G2")),
                    ("N", f(20.0)),
                    ("F", f(1234.0)),
                    ("X", f(100.0)),
                    ("Y", f(0.0)),
                    ("I", f(50.0)),
                    ("J", f(0.0)),
                    ("M", Value::StrList(vec!["8".to_string()])),
                ],
            ),
        ];
        let out = run(&mut fl, rows);
        assert!(out.len() > 3);
        assert!(cell_float(&out[1], "F").is_some());
        assert!(out[1].cells.get("M").is_some());
        assert!(out[2..].iter().all(|r| r.cells.get("F").is_none() && r.cells.get("M").is_none()));
        assert!(out.iter().all(|r| r.line_no == 1 || r.line_no == 2));
    }

    // ------------------------------------------------------------------
    // Splines
    // ------------------------------------------------------------------

    /// A quarter-circle-ish CSPLINE through points on the unit circle must
    /// stay close to it and hit every programmed point exactly.
    #[test]
    fn cspline_passes_through_points() {
        let mut fl = flattener(0.001);
        let mut rows = vec![row(1, &[("gg01_motion", s("G1")), ("X", f(1.0)), ("Y", f(0.0))])];
        let points: Vec<(f64, f64)> = (1..=6)
            .map(|i| {
                let a = std::f64::consts::FRAC_PI_2 * i as f64 / 6.0;
                (a.cos(), a.sin())
            })
            .collect();
        rows.push(row(2, &[("gg01_motion", s("CSPLINE")), ("X", f(points[0].0)), ("Y", f(points[0].1))]));
        for (i, (x, y)) in points.iter().enumerate().skip(1) {
            rows.push(row(3 + i, &[("X", f(*x)), ("Y", f(*y))]));
        }
        rows.push(row(20, &[("gg01_motion", s("G1")), ("X", f(0.0)), ("Y", f(2.0))]));
        let out = run(&mut fl, rows);

        // Every programmed point appears exactly in the output.
        for (x, y) in &points {
            assert!(
                out.iter().any(|r| {
                    cell_float(r, "X").is_some_and(|rx| (rx - x).abs() < 1e-9)
                        && cell_float(r, "Y").is_some_and(|ry| (ry - y).abs() < 1e-9)
                }),
                "programmed point ({x}, {y}) missing from flattened output"
            );
        }
        // Samples stay near the unit circle (the cubic through circle points
        // deviates a little, but far less than 2%).
        for r in &out[1..out.len() - 1] {
            let (x, y) = xy(r);
            let radius = (x * x + y * y).sqrt();
            assert!((radius - 1.0).abs() < 0.02, "sample far off circle: r={radius}");
        }
        // Spline command must not survive flattening.
        assert!(out.iter().all(
            |r| !matches!(r.cells.get("gg01_motion"), Some(Value::Str(m)) if m.contains("SPLINE"))
        ));
        // Deselecting G1 row passes through as-is.
        assert_eq!(xy(out.last().unwrap()), (0.0, 2.0));
    }

    #[test]
    fn aspline_passes_through_points() {
        let mut fl = flattener(0.01);
        let pts = [(10.0, 20.0), (20.0, 40.0), (30.0, 30.0), (40.0, 45.0), (50.0, 0.0)];
        let mut rows = vec![
            row(1, &[("gg01_motion", s("G1")), ("X", f(0.0)), ("Y", f(0.0))]),
            row(2, &[("gg01_motion", s("ASPLINE")), ("X", f(pts[0].0)), ("Y", f(pts[0].1))]),
        ];
        for (i, (x, y)) in pts.iter().enumerate().skip(1) {
            rows.push(row(3 + i, &[("X", f(*x)), ("Y", f(*y))]));
        }
        let out = run(&mut fl, rows);
        for (x, y) in &pts {
            assert!(
                out.iter().any(|r| {
                    cell_float(r, "X").is_some_and(|rx| (rx - x).abs() < 1e-9)
                        && cell_float(r, "Y").is_some_and(|ry| (ry - y).abs() < 1e-9)
                }),
                "programmed point ({x}, {y}) missing"
            );
        }
        assert_eq!(xy(out.last().unwrap()), (50.0, 0.0));
    }

    /// Collinear control points make the B-spline a straight line: the
    /// flattener must not densify it.
    #[test]
    fn straight_bspline_stays_sparse() {
        let mut fl = flattener(0.01);
        let rows = vec![
            row(1, &[("gg01_motion", s("G1")), ("X", f(0.0)), ("Y", f(0.0))]),
            row(2, &[("gg01_motion", s("BSPLINE")), ("X", f(10.0)), ("Y", f(0.0))]),
            row(3, &[("X", f(20.0)), ("Y", f(0.0))]),
            row(4, &[("X", f(30.0)), ("Y", f(0.0))]),
        ];
        let out = run(&mut fl, rows);
        assert!(out.len() <= 5, "straight B-spline over-sampled: {} rows", out.len());
        assert_eq!(xy(out.last().unwrap()), (30.0, 0.0));
    }

    /// A B-spline starts at the current position and ends at the last
    /// control point; in between it follows the control polygon smoothly and
    /// respects the tolerance against its own exact evaluation.
    #[test]
    fn bspline_endpoints_and_convex_hull() {
        let mut fl = flattener(0.001);
        let rows = vec![
            row(1, &[("gg01_motion", s("G1")), ("X", f(0.0)), ("Y", f(0.0))]),
            row(2, &[("gg01_motion", s("BSPLINE")), ("X", f(10.0)), ("Y", f(20.0))]),
            row(3, &[("X", f(20.0)), ("Y", f(-20.0))]),
            row(4, &[("X", f(30.0)), ("Y", f(20.0))]),
            row(5, &[("X", f(40.0)), ("Y", f(0.0))]),
        ];
        let out = run(&mut fl, rows);
        assert!(out.len() > 6, "curved B-spline should densify, got {}", out.len());
        assert_eq!(xy(out.last().unwrap()), (40.0, 0.0));
        // Convex hull property in each coordinate (skip aux-only rows).
        for r in out[1..].iter().filter(|r| r.cells.get("X").is_some()) {
            let (x, y) = xy(r);
            assert!((-1e-9..=40.0 + 1e-9).contains(&x));
            assert!((-20.0 - 1e-9..=20.0 + 1e-9).contains(&y));
        }
        // The curve must not pass through the interior control points
        // (B-splines approximate): no sample at exactly (20, -20).
        assert!(!out.iter().any(|r| {
            cell_float(r, "X").is_some_and(|x| (x - 20.0).abs() < 1e-6)
                && cell_float(r, "Y").is_some_and(|y| (y + 20.0).abs() < 1e-6)
        }));
    }

    /// A heavier PW weight pulls the curve toward its control point.
    #[test]
    fn bspline_weight_pulls_curve() {
        let sample_min_dist = |pw: Option<f64>| {
            let mut fl = flattener(0.0005);
            let mut mid = vec![("X", f(20.0)), ("Y", f(30.0))];
            if let Some(w) = pw {
                mid.push(("PW", f(w)));
            }
            let rows = vec![
                row(1, &[("gg01_motion", s("G1")), ("X", f(0.0)), ("Y", f(0.0))]),
                row(2, &[("gg01_motion", s("BSPLINE")), ("X", f(10.0)), ("Y", f(0.0))]),
                row(3, &mid),
                row(4, &[("X", f(30.0)), ("Y", f(0.0))]),
                row(5, &[("X", f(40.0)), ("Y", f(0.0))]),
            ];
            let out = run(&mut fl, rows);
            out.iter()
                .skip(1)
                .filter(|r| r.cells.get("X").is_some())
                .map(|r| {
                    let (x, y) = xy(r);
                    ((x - 20.0).powi(2) + (y - 30.0).powi(2)).sqrt()
                })
                .fold(f64::MAX, f64::min)
        };
        let unweighted = sample_min_dist(None);
        let weighted = sample_min_dist(Some(3.0));
        assert!(
            weighted < unweighted - 1.0,
            "PW=3 should pull the curve closer to the control point: {weighted} vs {unweighted}"
        );
    }

    #[test]
    fn quadratic_bspline_via_sd() {
        let mut fl = flattener(0.001);
        let rows = vec![
            row(1, &[("gg01_motion", s("G1")), ("X", f(0.0)), ("Y", f(0.0))]),
            row(2, &[("gg01_motion", s("BSPLINE")), ("X", f(10.0)), ("Y", f(10.0)), ("SD", f(2.0))]),
            row(3, &[("X", f(20.0)), ("Y", f(0.0))]),
        ];
        let out = run(&mut fl, rows);
        // Quadratic Bezier from (0,0) via (10,10) to (20,0): apex at (10,5).
        assert_eq!(xy(out.last().unwrap()), (20.0, 0.0));
        let apex = out
            .iter()
            .skip(1)
            .filter_map(|r| cell_float(r, "Y"))
            .fold(f64::MIN, f64::max);
        assert!((apex - 5.0).abs() < 0.01, "quadratic apex should be ~5, got {apex}");
    }

    /// Extra channels (here E) ride along with the spline samples.
    #[test]
    fn spline_extra_channel_is_sampled() {
        let mut fl = flattener(0.01);
        let rows = vec![
            row(1, &[("gg01_motion", s("G1")), ("X", f(0.0)), ("Y", f(0.0)), ("E", f(0.0))]),
            row(2, &[("gg01_motion", s("ASPLINE")), ("X", f(10.0)), ("Y", f(10.0)), ("E", f(1.0))]),
            row(3, &[("X", f(20.0)), ("Y", f(0.0)), ("E", f(2.0))]),
            row(4, &[("X", f(30.0)), ("Y", f(10.0)), ("E", f(3.0))]),
        ];
        let out = run(&mut fl, rows);
        assert!(out.len() > 4);
        for r in &out[1..] {
            assert!(r.cells.get("E").is_some(), "E channel missing on a sample row");
        }
        assert!((cell_float(out.last().unwrap(), "E").unwrap() - 3.0).abs() < 1e-9);
        // E rises monotonically along this path.
        let es: Vec<f64> = out[1..].iter().map(|r| cell_float(r, "E").unwrap()).collect();
        assert!(es.windows(2).all(|w| w[1] >= w[0] - 1e-6));
    }

    /// Aux rows (M codes, comments) inside a spline stay in program order.
    #[test]
    fn non_point_rows_inside_spline_kept_in_order() {
        let mut fl = flattener(0.01);
        let rows = vec![
            row(1, &[("gg01_motion", s("G1")), ("X", f(0.0)), ("Y", f(0.0))]),
            row(2, &[("gg01_motion", s("ASPLINE")), ("X", f(10.0)), ("Y", f(10.0))]),
            row(3, &[("M", Value::StrList(vec!["8".to_string()]))]),
            row(4, &[("X", f(20.0)), ("Y", f(0.0))]),
        ];
        let out = run(&mut fl, rows);
        let m_index = out.iter().position(|r| r.cells.get("M").is_some()).unwrap();
        // The M row sits after the samples of the first span and before the
        // samples of the second.
        assert!(out[..m_index].iter().any(|r| r.line_no == 2));
        assert!(out[m_index + 1..].iter().any(|r| r.line_no == 4));
    }

    /// Generated samples carry the `flattened` marker; programmed positions
    /// (arc endpoints, spline through-points, the B-spline curve end) do not.
    #[test]
    fn flattened_marker_distinguishes_generated_points() {
        let marked = |r: &Row| r.cells.get(FLATTENED_COLUMN).is_some();

        // Arc: every sample but the programmed endpoint is marked.
        let mut fl = flattener(0.01);
        let out = run(
            &mut fl,
            vec![
                row(1, &[("gg01_motion", s("G1")), ("X", f(0.0)), ("Y", f(0.0))]),
                row(2, &[("gg01_motion", s("G2")), ("X", f(100.0)), ("Y", f(0.0)), ("I", f(50.0)), ("J", f(0.0))]),
            ],
        );
        assert!(!marked(&out[0]), "passthrough row must not be marked");
        assert!(out[1..out.len() - 1].iter().all(marked));
        assert!(!marked(out.last().unwrap()), "programmed arc endpoint must not be marked");

        // Interpolating spline: the programmed points are the unmarked rows.
        let pts = [(10.0, 20.0), (20.0, 40.0), (30.0, 30.0)];
        let mut fl = flattener(0.01);
        let mut rows = vec![
            row(1, &[("gg01_motion", s("G1")), ("X", f(0.0)), ("Y", f(0.0))]),
            row(2, &[("gg01_motion", s("ASPLINE")), ("X", f(pts[0].0)), ("Y", f(pts[0].1))]),
        ];
        for (i, (x, y)) in pts.iter().enumerate().skip(1) {
            rows.push(row(3 + i, &[("X", f(*x)), ("Y", f(*y))]));
        }
        let out = run(&mut fl, rows);
        let unmarked: Vec<(f64, f64)> = out[1..]
            .iter()
            .filter(|r| !marked(r))
            .map(|r| xy(r))
            .collect();
        assert_eq!(unmarked, pts.to_vec(), "unmarked rows must be exactly the programmed points");

        // B-spline: only the curve end (== last control point) is unmarked.
        let mut fl = flattener(0.01);
        let out = run(
            &mut fl,
            vec![
                row(1, &[("gg01_motion", s("G1")), ("X", f(0.0)), ("Y", f(0.0))]),
                row(2, &[("gg01_motion", s("BSPLINE")), ("X", f(10.0)), ("Y", f(20.0))]),
                row(3, &[("X", f(20.0)), ("Y", f(-20.0))]),
                row(4, &[("X", f(30.0)), ("Y", f(0.0))]),
            ],
        );
        let unmarked_coord_rows: Vec<(f64, f64)> = out[1..]
            .iter()
            .filter(|r| !marked(r) && r.cells.get("X").is_some())
            .map(|r| xy(r))
            .collect();
        assert_eq!(unmarked_coord_rows, vec![(30.0, 0.0)]);
    }

    #[test]
    fn tolerance_must_be_positive() {
        assert!(Flattener::new(0.0, &axis_ids()).is_err());
        assert!(Flattener::new(-1.0, &axis_ids()).is_err());
        assert!(Flattener::new(f64::NAN, &axis_ids()).is_err());
    }

    /// The flattened output of a spline respects the tolerance against a
    /// dense reference sampling of the same spline.
    #[test]
    fn cspline_flattening_respects_tolerance() {
        let tol = 0.01;
        let pts: Vec<(f64, f64)> = (0..=8)
            .map(|i| {
                let x = i as f64 * 10.0;
                (x, (x / 20.0 * std::f64::consts::PI).sin() * 15.0)
            })
            .collect();
        let build = |tol: f64| {
            let mut fl = flattener(tol);
            let mut rows = vec![row(1, &[("gg01_motion", s("G1")), ("X", f(pts[0].0)), ("Y", f(pts[0].1))])];
            rows.push(row(2, &[("gg01_motion", s("CSPLINE")), ("X", f(pts[1].0)), ("Y", f(pts[1].1))]));
            for (i, (x, y)) in pts.iter().enumerate().skip(2) {
                rows.push(row(i + 2, &[("X", f(*x)), ("Y", f(*y))]));
            }
            run(&mut fl, rows)
        };
        // Reference: very fine flattening approximates the true curve. Both
        // polylines include the start row.
        let reference: Vec<(f64, f64)> = build(1e-6).iter().map(xy).collect();
        let coarse: Vec<(f64, f64)> = build(tol).iter().map(xy).collect();
        // Every reference point must lie within ~tol of the coarse polyline.
        for &(rx, ry) in &reference {
            let mut best = f64::MAX;
            for w in coarse.windows(2) {
                let d = deviation(&[rx, ry], &[w[0].0, w[0].1], &[w[1].0, w[1].1], &[0, 1]);
                best = best.min(d);
            }
            assert!(best <= tol * 1.5 + 1e-9, "reference point ({rx}, {ry}) deviates {best}");
        }
    }
}
