//! Programmable frame rotation (ROT/AROT/RPL) for the geometry axes X, Y, Z.
//!
//! The frame is a rotation matrix `R` plus the per-axis translation kept in
//! `State`: machine XYZ = R * local XYZ + t. Only the geometry axes rotate;
//! every other axis (A, B, C, E, ...) gets a translation at most, so programmed
//! orientation axes stay what the program wrote. The conventions here follow
//! the Sinumerik frame chapters cited at each function.

/// Row-major 3x3 rotation matrix over the geometry axes (0 = X, 1 = Y, 2 = Z).
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

pub fn is_identity(r: &Matrix3) -> bool {
    (0..3).all(|i| (0..3).all(|j| (r[i][j] - IDENTITY[i][j]).abs() <= EPS))
}

/// True when the rotation maps geometry axis `axis` onto itself, so a plane
/// with that normal stays parallel to the machine plane with the same sense.
pub fn keeps_axis(r: &Matrix3, axis: usize) -> bool {
    (0..3).all(|i| (r[i][axis] - IDENTITY[i][axis]).abs() <= EPS)
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
    fn keeps_axis_detects_plane_normal() {
        assert!(keeps_axis(&axis_rotation(2, 33.0), 2));
        assert!(!keeps_axis(&axis_rotation(0, 33.0), 2));
        // A half turn about X flips Z: the plane is parallel but reversed.
        assert!(!keeps_axis(&axis_rotation(0, 180.0), 2));
        assert!(is_identity(&rpy_rotation(0.0, 0.0, 0.0)));
        assert!(is_identity(&axis_rotation(1, 360.0)));
    }
}
