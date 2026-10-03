//! MNE-C source estimates (`.stc`): a time series on a set of cortical vertices.
//!
//! One file holds one hemisphere (`sample-lh.stc`, `sample-rh.stc`). It is
//! big-endian:
//!
//! | bytes | content |
//! |-------|---------|
//! | 4 | `tmin`, `float32`, in **milliseconds** |
//! | 4 | `tstep`, `float32`, in **milliseconds** |
//! | 4 | number of vertices, `uint32` |
//! | 4 × vertices | vertex numbers, `uint32` (surface node indices) |
//! | 4 | number of time points, `uint32` |
//! | 4 × vertices × times | `float32` values, **time-major**: every vertex at the first time, then every vertex at the second, and so on |
//!
//! Times are kept in seconds here, as MNE-Python does.
//!
//! AFNI has no reader for this format, so there is nothing to check against
//! but the layout above; the tests build the bytes by hand from it.

use std::path::Path;

use crate::array::TypedArray;
use crate::dset::NimlDataset;
use crate::error::{self, Error, Result};
use crate::niml::NumericMatrix;

/// A source estimate.
#[derive(Debug, Clone, PartialEq)]
pub struct Stc {
    /// Time of the first sample, in seconds.
    pub tmin: f64,
    /// Time between samples, in seconds.
    pub tstep: f64,
    /// Surface node index of each source.
    pub vertices: Vec<u32>,
    /// One vector of values per time point, each as long as `vertices`.
    pub time_points: Vec<Vec<f32>>,
}

impl Stc {
    /// Read a `.stc` file.
    pub fn read(path: impl AsRef<Path>) -> Result<Self> {
        Self::parse(&error::read_file(path.as_ref())?)
    }

    /// Parse `.stc` bytes. The file must be exactly as long as its header says.
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        let mut at = 0;
        let tmin_ms = f32::from_be_bytes(word(bytes, &mut at, "start time")?);
        let tstep_ms = f32::from_be_bytes(word(bytes, &mut at, "time step")?);
        if !tmin_ms.is_finite() {
            return Err(Error::invalid("STC start time is not finite"));
        }
        if !(tstep_ms.is_finite() && tstep_ms > 0.0) {
            return Err(Error::invalid("STC time step must be positive"));
        }
        let n_vertices = u32::from_be_bytes(word(bytes, &mut at, "vertex count")?) as usize;
        if n_vertices == 0 {
            return Err(Error::invalid("STC file has no vertices"));
        }
        // Check the room before allocating anything the header asks for.
        let vertex_end = at
            .checked_add(n_vertices.checked_mul(4).ok_or_else(overflow)?)
            .ok_or_else(overflow)?;
        let raw = bytes
            .get(at..vertex_end)
            .ok_or_else(|| Error::parse("STC file ends inside its vertex list"))?;
        let vertices = raw
            .chunks_exact(4)
            .map(|c| u32::from_be_bytes([c[0], c[1], c[2], c[3]]))
            .collect();
        at = vertex_end;

        let n_times = u32::from_be_bytes(word(bytes, &mut at, "time-point count")?) as usize;
        if n_times == 0 {
            return Err(Error::invalid("STC file has no time points"));
        }
        let n_values = n_vertices.checked_mul(n_times).ok_or_else(overflow)?;
        let n_bytes = n_values.checked_mul(4).ok_or_else(overflow)?;
        let rest = &bytes[at..];
        if rest.len() != n_bytes {
            return Err(Error::parse(format!(
                "STC data is {} bytes, but {n_vertices} vertices x {n_times} times need {n_bytes}",
                rest.len()
            )));
        }
        let time_points = rest
            .chunks_exact(4 * n_vertices)
            .map(|chunk| {
                chunk
                    .chunks_exact(4)
                    .map(|c| f32::from_be_bytes([c[0], c[1], c[2], c[3]]))
                    .collect()
            })
            .collect();

        Ok(Self {
            tmin: f64::from(tmin_ms) / 1000.0,
            tstep: f64::from(tstep_ms) / 1000.0,
            vertices,
            time_points,
        })
    }

    /// The time of sample `index`, in seconds.
    pub fn time(&self, index: usize) -> f64 {
        self.tmin + index as f64 * self.tstep
    }

    /// The `.stc` bytes. Every time point must have one value per vertex.
    pub fn to_bytes(&self) -> Result<Vec<u8>> {
        if self.vertices.is_empty() || self.time_points.is_empty() {
            return Err(Error::invalid("an STC needs vertices and time points"));
        }
        if !(self.tstep.is_finite() && self.tstep > 0.0 && self.tmin.is_finite()) {
            return Err(Error::invalid(
                "STC times must be finite, with a positive step",
            ));
        }
        if let Some(bad) = self
            .time_points
            .iter()
            .position(|t| t.len() != self.vertices.len())
        {
            return Err(Error::invalid(format!(
                "time point {bad} has {} values for {} vertices",
                self.time_points[bad].len(),
                self.vertices.len()
            )));
        }
        let mut out =
            Vec::with_capacity(16 + 4 * self.vertices.len() * (1 + self.time_points.len()));
        out.extend_from_slice(&((self.tmin * 1000.0) as f32).to_be_bytes());
        out.extend_from_slice(&((self.tstep * 1000.0) as f32).to_be_bytes());
        out.extend_from_slice(&(self.vertices.len() as u32).to_be_bytes());
        for v in &self.vertices {
            out.extend_from_slice(&v.to_be_bytes());
        }
        out.extend_from_slice(&(self.time_points.len() as u32).to_be_bytes());
        for t in &self.time_points {
            for x in t {
                out.extend_from_slice(&x.to_be_bytes());
            }
        }
        Ok(out)
    }

    /// Write the `.stc` file.
    pub fn write(&self, path: impl AsRef<Path>) -> Result<()> {
        error::write_file(path.as_ref(), &self.to_bytes()?)
    }

    /// The estimate as a sparse surface dataset: one row per vertex (its node
    /// index in `INDEX_LIST`), one `float` column per time point labelled
    /// `t=<seconds>`, and `ni_timestep` set to the time step.
    pub fn to_dataset(&self) -> Result<NimlDataset> {
        let columns = self
            .time_points
            .iter()
            .map(|t| TypedArray::Float32(t.clone()))
            .collect();
        let matrix = NumericMatrix::from_columns(columns)?;
        let mut dset = NimlDataset::new("Node_Bucket", matrix, Some(self.vertices.clone()));
        let labels: Vec<String> = (0..self.time_points.len())
            .map(|i| format!("t={:.6}", self.time(i)))
            .collect();
        dset.set_column_labels(&labels)?;
        dset.time_step = Some(self.tstep);
        Ok(dset)
    }
}

fn word(bytes: &[u8], at: &mut usize, what: &str) -> Result<[u8; 4]> {
    let raw = bytes
        .get(*at..*at + 4)
        .ok_or_else(|| Error::parse(format!("STC file ends inside its {what}")))?;
    *at += 4;
    Ok([raw[0], raw[1], raw[2], raw[3]])
}

fn overflow() -> Error {
    Error::invalid("STC dimensions overflow")
}
