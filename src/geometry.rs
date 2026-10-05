//! Volume geometry and timing from AFNI `.HEAD` attributes.
//!
//! # Coordinate conventions
//!
//! AFNI's native world coordinates are DICOM order, which AFNI calls **RAI**:
//! +x is Left, +y is Posterior, +z is Superior (the same axes as LPS). NIfTI's
//! sform/qform, GIfTI, and FreeSurfer use **RAS**: +x Right, +y Anterior,
//! +z Superior. Converting between them negates x and y; [`dicom_to_ras`] does
//! that for a matrix and [`flip_xy`] for a point. AFNI's own naming is
//! confusing here: its code and docs call RAS "LPI", because AFNI names an
//! axis by the end its coordinate *starts* from.
//!
//! This crate returns every coordinate in the convention of the file it came
//! from and never converts implicitly:
//!
//! * `.HEAD` matrices ([`crate::head::Header::ijk_to_dicom`]) are RAI, with
//!   [`crate::head::Header::ijk_to_ras`] for the RAS equivalent.
//! * NIfTI affines ([`crate::nifti::NiftiHeader::affine`]) are RAS.
//! * GIfTI and FreeSurfer surface coordinates are RAS. SUMA flips GIfTI
//!   coordinates to RAI on read and back on write (`flip_float_triples` in
//!   `suma_gifti.c`, unless `AFNI_GIFTI_IN_RAI` is set). It flips FreeSurfer
//!   surfaces to RAI only when aligning them to a surface volume
//!   (`SUMA_Align_to_VolPar`, `SUMA_VolData.c`), which it skips for spheres
//!   and flat patches.
//! * SUMA `.asc` and `.1D.coord` files written by SUMA itself hold SUMA's
//!   internal (RAI) coordinates.
//!
//! # Voxel-to-world matrices
//!
//! AFNI keeps two `ijk -> DICOM` matrices for a dataset:
//!
//! * the **cardinal** one, built from `ORIENT_SPECIFIC`, `ORIGIN` and `DELTA`
//!   (`THD_daxes_to_mat44`, `thd_matdaxes.c`). Each axis maps to exactly one
//!   DICOM axis through a permutation with no sign flips
//!   (`THD_set_daxes_to_dicomm`, `thd_editdaxes.c`); the signs live in
//!   `DELTA` and `ORIGIN`, which AFNI stores already signed for DICOM.
//! * the **real** one, `IJK_TO_DICOM_REAL` (12 floats, a row-major 3x4),
//!   which can carry obliquity. AFNI displays on the cardinal grid but uses
//!   the real matrix for `3dinfo -aform_real`, for obliquity, and when it
//!   writes NIfTI.
//!
//! References: `afni/src/thd_matdaxes.c`, `thd_editdaxes.c`,
//! `thd_dsetdblk.c`, `thd_coords.c` (`THD_compute_oblique_angle`), `3ddata.h`.

/// A 4x4 matrix, row-major: `m[row][col]`, with `[0, 0, 0, 1]` as the last row.
pub type Mat44 = [[f64; 4]; 4];

/// AFNI's tolerance, in degrees, below which a dataset counts as plumb
/// (`OBLIQ_ANGLE_THRESH` in `3ddata.h`).
pub const OBLIQUE_ANGLE_THRESHOLD: f64 = 0.01;

/// The direction of one dataset axis (an `ORIENT_SPECIFIC` code).
///
/// Each variant names where the axis starts and where it goes: `R2L` runs
/// from Right to Left.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Orientation {
    /// Code 0: right to left.
    R2L,
    /// Code 1: left to right.
    L2R,
    /// Code 2: posterior to anterior.
    P2A,
    /// Code 3: anterior to posterior.
    A2P,
    /// Code 4: inferior to superior.
    I2S,
    /// Code 5: superior to inferior.
    S2I,
}

impl Orientation {
    /// Map an `ORIENT_SPECIFIC` code (0–5) onto a variant.
    pub fn from_code(code: i64) -> Option<Self> {
        Some(match code {
            0 => Self::R2L,
            1 => Self::L2R,
            2 => Self::P2A,
            3 => Self::A2P,
            4 => Self::I2S,
            5 => Self::S2I,
            _ => return None,
        })
    }

    /// The `ORIENT_SPECIFIC` code.
    pub fn code(self) -> i64 {
        self as i64
    }

    /// The letter AFNI uses in orientation strings such as `RAI`: the end the
    /// axis starts from.
    pub fn letter(self) -> char {
        match self {
            Self::R2L => 'R',
            Self::L2R => 'L',
            Self::P2A => 'P',
            Self::A2P => 'A',
            Self::I2S => 'I',
            Self::S2I => 'S',
        }
    }

    /// The DICOM axis this orientation lies along: 0 = x (R/L), 1 = y (A/P),
    /// 2 = z (I/S).
    pub fn dicom_axis(self) -> usize {
        match self {
            Self::R2L | Self::L2R => 0,
            Self::P2A | Self::A2P => 1,
            Self::I2S | Self::S2I => 2,
        }
    }
}

/// The AFNI view a dataset belongs to (`SCENE_DATA[0]`), which is also the
/// `+view` part of its file name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum View {
    /// Code 0: original acquisition space, `+orig`.
    Orig,
    /// Code 1: AC-PC aligned, `+acpc`.
    Acpc,
    /// Code 2: Talairach or another standard space, `+tlrc`.
    Tlrc,
}

impl View {
    /// Map a `SCENE_DATA[0]` code onto a variant.
    pub fn from_code(code: i64) -> Option<Self> {
        Some(match code {
            0 => Self::Orig,
            1 => Self::Acpc,
            2 => Self::Tlrc,
            _ => return None,
        })
    }

    /// The file-name suffix, e.g. `+orig`.
    pub fn suffix(self) -> &'static str {
        match self {
            Self::Orig => "+orig",
            Self::Acpc => "+acpc",
            Self::Tlrc => "+tlrc",
        }
    }
}

/// Units of a dataset's time axis (`TAXIS_NUMS[2]`, `UNITS_*_TYPE` in
/// `3ddata.h`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimeUnits {
    /// Code 77001.
    Milliseconds,
    /// Code 77002, and also what AFNI assumes for any other code (current
    /// AFNI writes 0).
    Seconds,
    /// Code 77003: the "time" axis is a frequency axis.
    Hertz,
}

impl TimeUnits {
    /// The AFNI `UNITS_*_TYPE` code written in `TAXIS_NUMS[2]`.
    pub fn code(self) -> i64 {
        match self {
            Self::Milliseconds => 77001,
            Self::Seconds => 77002,
            Self::Hertz => 77003,
        }
    }

    /// Map a `TAXIS_NUMS[2]` code onto a variant.
    pub fn from_code(code: i64) -> Self {
        match code {
            77001 => Self::Milliseconds,
            77003 => Self::Hertz,
            _ => Self::Seconds,
        }
    }
}

/// A dataset's time axis, from `TAXIS_NUMS`, `TAXIS_FLOATS` and
/// `TAXIS_OFFSETS`.
///
/// As AFNI does on load (`DSET_UNMSEC`, `thd_dsetdblk.c`), times stored in
/// milliseconds are converted to seconds; [`TimeAxis::stored_units`] says
/// what the file used.
#[derive(Debug, Clone, PartialEq)]
pub struct TimeAxis {
    /// Number of time points (`TAXIS_NUMS[0]`).
    pub nt: usize,
    /// Time of the first point (`TAXIS_FLOATS[0]`).
    pub origin: f64,
    /// Time step, the TR for FMRI (`TAXIS_FLOATS[1]`).
    pub step: f64,
    /// Acquisition duration (`TAXIS_FLOATS[2]`); usually 0.
    pub duration: f64,
    /// Units as stored. Times above are in seconds unless this is
    /// [`TimeUnits::Hertz`].
    pub stored_units: TimeUnits,
    /// Per-slice acquisition offsets (`TAXIS_OFFSETS`), one per z slice, or
    /// empty when the dataset has no slice timing.
    pub slice_offsets: Vec<f64>,
    /// z coordinate of the first slice the offsets refer to
    /// (`TAXIS_FLOATS[3]`).
    pub slice_z_origin: f64,
    /// Slice spacing along z (`TAXIS_FLOATS[4]`).
    pub slice_dz: f64,
}

impl TimeAxis {
    /// The TR in seconds, or `None` for a frequency axis.
    pub fn tr_seconds(&self) -> Option<f64> {
        (self.stored_units != TimeUnits::Hertz).then_some(self.step)
    }
}

/// The identity matrix.
pub fn identity() -> Mat44 {
    let mut m = [[0.0; 4]; 4];
    for (i, row) in m.iter_mut().enumerate() {
        row[i] = 1.0;
    }
    m
}

/// Convert an AFNI `ijk -> DICOM (RAI)` matrix to `ijk -> RAS`, the NIfTI
/// sform convention, by negating its x and y rows. Applying it twice gives
/// back the original.
pub fn dicom_to_ras(m: &Mat44) -> Mat44 {
    let mut out = *m;
    for row in out.iter_mut().take(2) {
        for value in row.iter_mut() {
            *value = -*value;
        }
    }
    out
}

/// Negate x and y of a point: RAI <-> RAS (see the module docs).
pub fn flip_xy([x, y, z]: [f64; 3]) -> [f64; 3] {
    [-x, -y, z]
}

/// Apply `m` to a point.
pub fn transform_point(m: &Mat44, [x, y, z]: [f64; 3]) -> [f64; 3] {
    let row = |r: usize| m[r][0] * x + m[r][1] * y + m[r][2] * z + m[r][3];
    [row(0), row(1), row(2)]
}

/// How far, in degrees, the voxel axes of `m` are from the nearest cardinal
/// axes; 0 when the difference is within [`OBLIQUE_ANGLE_THRESHOLD`].
///
/// This is AFNI's `THD_compute_oblique_angle` (`thd_coords.c`): for each
/// column, the largest absolute component over the column's length gives the
/// cosine of that axis's tilt, and the worst axis decides.
pub fn oblique_angle(m: &Mat44) -> f64 {
    let cosine = |col: usize| {
        let parts = [m[0][col].abs(), m[1][col].abs(), m[2][col].abs()];
        let length = parts.iter().map(|v| v * v).sum::<f64>().sqrt();
        parts.iter().copied().fold(0.0, f64::max) / length
    };
    let worst = cosine(0).min(cosine(1)).min(cosine(2)).min(1.0);
    let angle = worst.acos().to_degrees();
    if angle > OBLIQUE_ANGLE_THRESHOLD {
        angle
    } else {
        0.0
    }
}

/// Build a [`Mat44`] from 12 row-major values (a 3x4 matrix such as
/// `IJK_TO_DICOM_REAL`).
pub fn mat44_from_3x4(values: &[f64]) -> Option<Mat44> {
    let v = values.get(..12)?;
    Some([
        [v[0], v[1], v[2], v[3]],
        [v[4], v[5], v[6], v[7]],
        [v[8], v[9], v[10], v[11]],
        [0.0, 0.0, 0.0, 1.0],
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn oblique_angle_matches_afni() {
        assert_eq!(oblique_angle(&identity()), 0.0);
        let (s, c) = 10f64.to_radians().sin_cos();
        let rotated = [
            [c, -s, 0.0, 1.0],
            [s, c, 0.0, 2.0],
            [0.0, 0.0, 1.0, 3.0],
            [0.0, 0.0, 0.0, 1.0],
        ];
        assert!((oblique_angle(&rotated) - 10.0).abs() < 1e-9);
        // Permuted, flipped and scaled axes are still plumb.
        let permuted = [
            [0.0, -2.0, 0.0, 0.0],
            [0.0, 0.0, 3.0, 0.0],
            [1.5, 0.0, 0.0, 0.0],
            [0.0, 0.0, 0.0, 1.0],
        ];
        assert_eq!(oblique_angle(&permuted), 0.0);
    }

    #[test]
    fn ras_conversion_negates_x_and_y() {
        let m = mat44_from_3x4(&[1., 2., 3., 4., 5., 6., 7., 8., 9., 10., 11., 12.]).unwrap();
        let ras = dicom_to_ras(&m);
        assert_eq!(ras[0], [-1., -2., -3., -4.]);
        assert_eq!(ras[1], [-5., -6., -7., -8.]);
        assert_eq!(ras[2], m[2]);
        assert_eq!(dicom_to_ras(&ras), m);
        assert_eq!(flip_xy([1., -2., 3.]), [-1., 2., 3.]);
        assert_eq!(transform_point(&m, [1., 0., 0.]), [5., 13., 21.]);
    }
}
