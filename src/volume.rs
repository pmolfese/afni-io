//! One entry point for volumes in either format: AFNI `.HEAD`/`.BRIK` or
//! NIfTI.
//!
//! [`read_any`] picks the reader from the name (anything
//! [`AfniPaths::is_afni_name`] accepts is AFNI; everything else is NIfTI) and
//! returns a [`Volume`] with format-independent accessors: grid size, number
//! of volumes, the voxel-to-world matrix, one volume at a time as scaled
//! `f32`, and the AFNI attributes (the `.HEAD` itself, or the NIfTI's AFNI
//! extension), which carry sub-brick labels and statistics.
//!
//! Voxel order is the same in both formats: `i + nx * (j + ny * k)`, then
//! volume after volume.

use std::path::Path;

use crate::brik::{AfniPaths, Brik};
use crate::error::{Error, Result};
use crate::geometry::{dicom_to_ras, Mat44};
use crate::head::Header;
use crate::nifti::Nifti;
use crate::stat::StatSpec;

/// A volume read by [`read_any`].
#[derive(Debug, Clone)]
pub enum Volume {
    /// An AFNI `.HEAD`/`.BRIK` dataset.
    Afni(Brik),
    /// A NIfTI-1 or NIfTI-2 volume (boxed: its header is much larger than
    /// a [`Brik`]).
    Nifti(Box<Nifti>),
}

/// Read an AFNI dataset or a NIfTI volume, choosing by name.
pub fn read_any(path: impl AsRef<Path>) -> Result<Volume> {
    read_volumes(path.as_ref(), None)
}

/// Like [`read_any`], but for an AFNI dataset only the sub-bricks in
/// `volumes` are read ([`Brik::read_sub_bricks`]); the rest give `None` from
/// [`Volume::frame_f32`]. A NIfTI file is read whole.
pub fn read_any_volumes(path: impl AsRef<Path>, volumes: &[usize]) -> Result<Volume> {
    read_volumes(path.as_ref(), Some(volumes))
}

fn read_volumes(path: &Path, volumes: Option<&[usize]>) -> Result<Volume> {
    if AfniPaths::is_afni_name(path) {
        Ok(Volume::Afni(match volumes {
            Some(v) => Brik::read_sub_bricks(path, v)?,
            None => Brik::read(path)?,
        }))
    } else {
        Ok(Volume::Nifti(Box::new(Nifti::read(path)?)))
    }
}

impl Volume {
    /// Grid size `[nx, ny, nz]`. A NIfTI image with fewer than three
    /// dimensions is padded with 1s.
    pub fn dimensions(&self) -> [usize; 3] {
        match self {
            Volume::Afni(b) => b.dimensions,
            Volume::Nifti(n) => {
                let shape = n.shape();
                let dim = |i: usize| shape.get(i).copied().unwrap_or(1).max(1);
                [dim(0), dim(1), dim(2)]
            }
        }
    }

    /// Voxels per volume.
    pub fn voxels(&self) -> usize {
        self.dimensions().iter().product()
    }

    /// Number of volumes: sub-bricks for AFNI; for NIfTI the product of
    /// dimensions 4 and up (time, then any further dimensions).
    pub fn nvols(&self) -> usize {
        match self {
            Volume::Afni(b) => b.nvals(),
            Volume::Nifti(n) => n.shape().iter().skip(3).product::<usize>().max(1),
        }
    }

    /// The `ijk -> RAS` matrix: AFNI's [`Header::ijk_to_ras`], or the NIfTI
    /// sform/qform affine (sform first, AFNI's default as well).
    pub fn ijk_to_ras(&self) -> Result<Mat44> {
        match self {
            Volume::Afni(b) => b.header.ijk_to_ras(),
            Volume::Nifti(n) => Ok(n.header.affine()),
        }
    }

    /// The `ijk -> DICOM (RAI)` matrix, AFNI's native convention.
    pub fn ijk_to_dicom(&self) -> Result<Mat44> {
        Ok(dicom_to_ras(&self.ijk_to_ras()?))
    }

    /// The AFNI attributes: the `.HEAD` of an AFNI dataset, or the AFNI
    /// extension of a NIfTI file (`None` if it has none).
    pub fn afni_header(&self) -> Result<Option<Header>> {
        match self {
            Volume::Afni(b) => Ok(Some(b.header.clone())),
            Volume::Nifti(n) => n.afni_header(),
        }
    }

    /// One label per volume, from the AFNI attributes when there are any,
    /// otherwise AFNI's defaults `#0`, `#1`, ...
    pub fn labels(&self) -> Result<Vec<String>> {
        Ok(match self.afni_header()? {
            Some(h) => {
                let mut labels = h.brick_labels();
                labels.resize_with(self.nvols(), String::new);
                labels
                    .into_iter()
                    .enumerate()
                    .map(|(p, l)| if l.is_empty() { format!("#{p}") } else { l })
                    .collect()
            }
            None => (0..self.nvols()).map(|p| format!("#{p}")).collect(),
        })
    }

    /// The statistic in each volume. AFNI attributes win when present.
    /// Otherwise a NIfTI statistical intent applies to every volume, as in
    /// AFNI's `thd_niftiread.c`, except when `dim[5] > 1` (per-voxel
    /// parameters, which AFNI does not support either).
    ///
    /// For a header intent, correlation parameters are classified by structure
    /// (see [`afni_core::stat::StatSpec::from_intent`]); a malformed set is an
    /// error, not a missing statistic. Use
    /// [`stats_with_origin`](Self::stats_with_origin) to state who wrote them.
    pub fn stats(&self) -> Result<Vec<Option<StatSpec>>> {
        self.stats_with_origin(afni_core::stat::IntentOrigin::Unknown)
    }

    /// Like [`stats`](Self::stats), with the writer of a NIfTI header intent
    /// stated. `origin` only affects header intents (never AFNI attributes) and
    /// only correlation.
    pub fn stats_with_origin(
        &self,
        origin: afni_core::stat::IntentOrigin,
    ) -> Result<Vec<Option<StatSpec>>> {
        if let Some(h) = self.afni_header()? {
            let mut stats = h.brick_stats();
            stats.resize(self.nvols(), None);
            return Ok(stats);
        }
        let Volume::Nifti(n) = self else {
            unreachable!("an AFNI volume always has a header");
        };
        let h = &n.header;
        let per_voxel = h.shape().get(4).copied().unwrap_or(1) > 1;
        let stat = if per_voxel {
            None
        } else {
            StatSpec::from_intent(
                i64::from(h.intent_code),
                [h.intent_p1, h.intent_p2, h.intent_p3],
                origin,
            )
            .map_err(|e| Error::invalid(format!("NIfTI statistic: {e}")))?
        };
        Ok(vec![stat; self.nvols()])
    }

    /// Volume `t` as scaled `f32` values in `i + nx * (j + ny * k)` order.
    /// `None` when `t` is out of range, the sub-brick was not read, or the
    /// data has no scalar value (AFNI RGB/RGBA). NaN and Inf are kept.
    pub fn frame_f32(&self, t: usize) -> Option<Vec<f32>> {
        match self {
            Volume::Afni(b) => b.sub_brick(t)?.to_f32(),
            Volume::Nifti(n) => {
                if t >= self.nvols() {
                    return None;
                }
                let voxels = self.voxels();
                let start = t * voxels;
                (start..start + voxels)
                    .map(|i| n.get_scaled(i).map(|v| v as f32))
                    .collect()
            }
        }
    }

    /// The scaled value at voxel `(i, j, k)` of volume `t`.
    pub fn value(&self, i: usize, j: usize, k: usize, t: usize) -> Option<f32> {
        match self {
            Volume::Afni(b) => b.value(i, j, k, t),
            Volume::Nifti(n) => {
                let [nx, ny, nz] = self.dimensions();
                if i >= nx || j >= ny || k >= nz || t >= self.nvols() {
                    return None;
                }
                n.get_scaled(t * self.voxels() + i + nx * (j + ny * k))
                    .map(|v| v as f32)
            }
        }
    }

    /// The underlying AFNI dataset, if this is one.
    pub fn as_afni(&self) -> Option<&Brik> {
        match self {
            Volume::Afni(b) => Some(b),
            Volume::Nifti(_) => None,
        }
    }

    /// The underlying NIfTI volume, if this is one.
    pub fn as_nifti(&self) -> Option<&Nifti> {
        match self {
            Volume::Nifti(n) => Some(n),
            Volume::Afni(_) => None,
        }
    }
}

impl TryFrom<Volume> for Brik {
    type Error = Error;

    fn try_from(volume: Volume) -> Result<Self> {
        match volume {
            Volume::Afni(b) => Ok(b),
            Volume::Nifti(_) => Err(Error::invalid("not an AFNI dataset")),
        }
    }
}
