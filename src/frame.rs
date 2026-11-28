//! Programmable frame rotation, scaling and mirroring (ROT/AROT/RPL,
//! SCALE/ASCALE, MIRROR/AMIRROR) for the geometry axes X, Y, Z.
//!
//! The frame is a linear part `L` plus the per-axis translation kept in
//! `State`: machine XYZ = L * local XYZ + t, with L = R * S * M (Basic
//! Functions manual 4.5.2.11). Only the geometry axes are transformed;
//! every other axis (A, B, C, E, ...) gets a translation at most, so programmed
//! orientation axes stay what the program wrote. The conventions here follow
//! the Sinumerik frame chapters cited at each function.

/// Row-major 3x3 matrix over the geometry axes (0 = X, 1 = Y, 2 = Z).
pub type Matrix3 = [[f64; 3]; 3];

pub const IDENTITY: Matrix3 = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];

/// Tolerance for "this matrix entry is zero / one"; angles come from sin/cos
/// of user degrees, so exact comparisons would miss identity rotations.
pub const EPS: f64 = 1e-9;

/// Active working plane (G17/G18/G19). It fixes the RPL rotation axis and the
/// plane an arc must keep under a rotated frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Plane {
    G17,
    G18,
    G19,
}

impl Plane {
    pub fn from_code(code: &str) -> Option<Plane> {
        match code {
            "G17" => Some(Plane::G17),
            "G18" => Some(Plane::G18),
            "G19" => Some(Plane::G19),
            _ => None,
        }
    }

    /// Geometry axis perpendicular to the plane: G17 Z, G18 Y, G19 X
    /// (NC programming manual 3.12.2.4, "RPL=... Rotation around the geometry
    /// axis perpendicular to the active machining plane").
    pub fn normal_axis(self) -> usize {
        match self {
            Plane::G17 => 2,
            Plane::G18 => 1,
            Plane::G19 => 0,
        }
    }

    pub fn code(self) -> &'static str {
        match self {
            Plane::G17 => "G17",
            Plane::G18 => "G18",
            Plane::G19 => "G19",
        }
    }
}

pub const AXIS_NAMES: [&str; 3] = ["X", "Y", "Z"];

/// Index of a geometry axis by its (uppercase) name.
pub fn geometry_index(key: &str) -> Option<usize> {
    match key {
        "X" => Some(0),
        "Y" => Some(1),
        "Z" => Some(2),
        _ => None,
    }
}

pub fn multiply(a: &Matrix3, b: &Matrix3) -> Matrix3 {
    let mut out = [[0.0; 3]; 3];
    for (i, row) in out.iter_mut().enumerate() {
        for (j, cell) in row.iter_mut().enumerate() {
            *cell = (0..3).map(|k| a[i][k] * b[k][j]).sum();
        }
    }
    out
}

pub fn apply(r: &Matrix3, v: [f64; 3]) -> [f64; 3] {
    let mut out = [0.0; 3];
    for (i, cell) in out.iter_mut().enumerate() {
        *cell = r[i][0] * v[0] + r[i][1] * v[1] + r[i][2] * v[2];
    }
    out
}

pub fn determinant(m: &Matrix3) -> f64 {
    m[0][0] * (m[1][1] * m[2][2] - m[1][2] * m[2][1]) - m[0][1] * (m[1][0] * m[2][2] - m[1][2] * m[2][0])
        + m[0][2] * (m[1][0] * m[2][1] - m[1][1] * m[2][0])
}

/// The inverse by cofactors, or `None` for a singular matrix (a zero scale).
pub fn inverse(m: &Matrix3) -> Option<Matrix3> {
    let det = determinant(m);
    if !det.is_finite() || det.abs() <= EPS {
        return None;
    }
    let mut out = [[0.0; 3]; 3];
    for (i, row) in out.iter_mut().enumerate() {
        for (j, cell) in row.iter_mut().enumerate() {
            let (r0, r1) = ((j + 1) % 3, (j + 2) % 3);
            let (c0, c1) = ((i + 1) % 3, (i + 2) % 3);
            *cell = (m[r0][c0] * m[r1][c1] - m[r0][c1] * m[r1][c0]) / det;
        }
    }
    Some(out)
}

/// Scaling (and, with a negative factor, mirroring) along the geometry axes.
pub fn scaling(factors: [f64; 3]) -> Matrix3 {
    [[factors[0], 0.0, 0.0], [0.0, factors[1], 0.0], [0.0, 0.0, factors[2]]]
}

/// True when the columns are mutually orthogonal, so the matrix is a rotation
/// times per-axis scale and mirror factors: the form R * S * M a Sinumerik
/// frame holds. A rotation chained after unequal scale factors shears the
/// axes and fails this.
pub fn columns_orthogonal(m: &Matrix3) -> bool {
    (0..3).all(|a| {
        (a + 1..3).all(|b| {
            let dot: f64 = (0..3).map(|k| m[k][a] * m[k][b]).sum();
            let scale: f64 = (0..3).map(|k| m[k][a].abs().max(m[k][b].abs())).fold(1.0, f64::max);
            dot.abs() <= EPS * scale * scale
        })
    })
}

/// True when the frame mixes geometry axis `axis` with another one, in either direction.
pub fn couples_axis(m: &Matrix3, axis: usize) -> bool {
    (0..3).any(|other| other != axis && (m[axis][other].abs() > EPS || m[other][axis].abs() > EPS))
}

/// The scale factor of the plane with normal `normal` when the frame maps that
/// plane onto itself by a rotation and one positive factor, so a circle stays
/// a circle with the same sense of rotation. `None` for a tilted plane,
/// unequal in-plane factors or an in-plane mirror.
pub fn plane_similarity(m: &Matrix3, normal: usize) -> Option<f64> {
    if couples_axis(m, normal) {
        return None;
    }
    let (a, b) = ((normal + 1) % 3, (normal + 2) % 3);
    let (p, q, r, s) = (m[a][a], m[a][b], m[b][a], m[b][b]);
    let scale = (p * p + r * r).sqrt();
    let tolerance = EPS * scale.max(1.0);
    let similar = (p - s).abs() <= tolerance && (q + r).abs() <= tolerance;
    (similar && scale > EPS).then_some(scale)
}

pub fn is_identity(r: &Matrix3) -> bool {
    (0..3).all(|i| (0..3).all(|j| (r[i][j] - IDENTITY[i][j]).abs() <= EPS))
}

/// Rotation about one geometry axis by `degrees`, right-hand rule: positive
/// is clockwise when looking along the positive axis direction (Basic
/// Functions manual 4.5.2.3), so +90 about Z takes +X onto +Y.
pub fn axis_rotation(axis: usize, degrees: f64) -> Matrix3 {
    let (s, c) = degrees.to_radians().sin_cos();
    match axis {
        0 => [[1.0, 0.0, 0.0], [0.0, c, -s], [0.0, s, c]],
        1 => [[c, 0.0, s], [0.0, 1.0, 0.0], [-s, 0.0, c]],
        _ => [[c, -s, 0.0], [s, c, 0.0], [0.0, 0.0, 1.0]],
    }
}

/// Several angles in one ROT/AROT block are RPY angles: the rotations are
/// executed about Z first, then the rotated Y', then the twice-rotated X''
/// (NC programming manual 3.12.2.4 "Rotation sequence", Basic Functions
/// manual 4.5.2.4). As intrinsic rotations that is R = Rz * Ry * Rx. This is
/// MD10600 FRAME_ANGLE_INPUT_MODE = 1, the factory default; mode 2 (Euler
/// ZX'Z'') is out of scope.
pub fn rpy_rotation(x_deg: f64, y_deg: f64, z_deg: f64) -> Matrix3 {
    let rz = axis_rotation(2, z_deg);
    let ry = axis_rotation(1, y_deg);
    let rx = axis_rotation(0, x_deg);
    multiply(&multiply(&rz, &ry), &rx)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: [f64; 3], b: [f64; 3]) -> bool {
        a.iter().zip(b).all(|(p, q)| (p - q).abs() < 1e-9)
    }

    #[test]
    fn positive_z_rotation_takes_x_onto_y() {
        let r = axis_rotation(2, 90.0);
        assert!(close(apply(&r, [1.0, 0.0, 0.0]), [0.0, 1.0, 0.0]));
    }

    #[test]
    fn positive_y_rotation_takes_z_onto_x() {
        let r = axis_rotation(1, 90.0);
        assert!(close(apply(&r, [0.0, 0.0, 1.0]), [1.0, 0.0, 0.0]));
    }

    #[test]
    fn positive_x_rotation_takes_y_onto_z() {
        let r = axis_rotation(0, 90.0);
        assert!(close(apply(&r, [0.0, 1.0, 0.0]), [0.0, 0.0, 1.0]));
    }

    #[test]
    fn rpy_order_is_z_then_y_then_x() {
        // Intrinsic Z, Y', X'' equals AROT Z, then AROT Y, then AROT X.
        let chained = multiply(
            &multiply(&axis_rotation(2, 30.0), &axis_rotation(1, 40.0)),
            &axis_rotation(0, 50.0),
        );
        let rpy = rpy_rotation(50.0, 40.0, 30.0);
        for i in 0..3 {
            for j in 0..3 {
                assert!((chained[i][j] - rpy[i][j]).abs() < 1e-12);
            }
        }
        // The reverse order (X first) is a different rotation.
        let reversed = multiply(
            &multiply(&axis_rotation(0, 50.0), &axis_rotation(1, 40.0)),
            &axis_rotation(2, 30.0),
        );
        assert!((0..3).any(|i| (0..3).any(|j| (reversed[i][j] - rpy[i][j]).abs() > 1e-6)));
    }

    #[test]
    fn plane_similarity_accepts_only_rotation_and_one_positive_factor() {
        assert_eq!(plane_similarity(&axis_rotation(2, 33.0), 2), Some(1.0));
        assert_eq!(plane_similarity(&axis_rotation(0, 33.0), 2), None);
        // A half turn about X reverses the sense of rotation in the X/Y plane.
        assert_eq!(plane_similarity(&axis_rotation(0, 180.0), 2), None);
        let scaled = multiply(&axis_rotation(2, 20.0), &scaling([2.0, 2.0, 5.0]));
        assert!((plane_similarity(&scaled, 2).unwrap() - 2.0).abs() < 1e-12);
        assert_eq!(plane_similarity(&scaling([2.0, 3.0, 1.0]), 2), None);
        assert_eq!(plane_similarity(&scaling([-1.0, 1.0, 1.0]), 2), None);
        // Mirroring the normal axis leaves the plane itself untouched.
        assert_eq!(plane_similarity(&scaling([1.0, 1.0, -1.0]), 2), Some(1.0));
        assert!(is_identity(&rpy_rotation(0.0, 0.0, 0.0)));
        assert!(is_identity(&axis_rotation(1, 360.0)));
    }

    #[test]
    fn inverse_undoes_rotation_scale_and_mirror() {
        let m = multiply(&rpy_rotation(10.0, 20.0, 30.0), &scaling([2.0, -0.5, 3.0]));
        assert!(is_identity(&multiply(&m, &inverse(&m).unwrap())));
        assert!(inverse(&scaling([1.0, 0.0, 1.0])).is_none());
    }

    #[test]
    fn a_rotation_after_unequal_scale_factors_shears() {
        let scale = scaling([2.0, 1.0, 1.0]);
        assert!(columns_orthogonal(&multiply(&axis_rotation(2, 30.0), &scale)));
        assert!(!columns_orthogonal(&multiply(&scale, &axis_rotation(2, 30.0))));
        // Equal factors in the rotated plane commute with the rotation.
        assert!(columns_orthogonal(&multiply(
            &scaling([2.0, 2.0, 5.0]),
            &axis_rotation(2, 30.0)
        )));
    }
}
