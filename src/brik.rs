//! AFNI volume datasets: the `.HEAD`/`.BRIK` pair.
//!
//! An AFNI volume is a single dataset split across two files. The `.HEAD` holds
//! the ASCII attributes parsed by [`crate::head`] (geometry, datum type, byte
//! order, scale factors); the `.BRIK` (or gzipped `.BRIK.gz`) holds the raw
//! binary voxel data those attributes describe. The two always travel together
//! and share a base name, so [`Brik::read`] takes any member of the pair and
//! loads the whole dataset.
//!
//! Sub-bricks are stored back-to-back: for sub-brick *p* there are `nx*ny*nz`
//! voxels of the type named by `BRICK_TYPES[p]`, in `(i + j*nx + k*nx*ny)`
//! order. On read, each sub-brick is widened to `f32` and any positive
//! `BRICK_FLOAT_FACS[p]` scale factor is applied, giving "true" values.
//!
//! Reference: `afni/src/matlab/BrikLoad.m`, `README.attributes`.

use std::path::{Path, PathBuf};

use crate::error::{self, Error, Result};
use crate::head::Header;

/// AFNI sub-brick storage types (the `BRICK_TYPES` codes).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BrikType {
    /// Code 0: unsigned 8-bit.
    Byte,
    /// Code 1: signed 16-bit.
    Short,
    /// Code 3: 32-bit IEEE float.
    Float,
    /// Code 5: complex (two 32-bit floats: real, imaginary).
    Complex,
}

impl BrikType {
    /// Map an AFNI `BRICK_TYPES` code onto a variant.
    pub fn from_code(code: i64) -> Result<Self> {
        match code {
            0 => Ok(Self::Byte),
            1 => Ok(Self::Short),
            3 => Ok(Self::Float),
            5 => Ok(Self::Complex),
            2 | 4 | 6 => Err(Error::unsupported(format!(
                "BRICK_TYPES code {code} (int/double/rgb)"
            ))),
            other => Err(Error::invalid(format!("unknown BRICK_TYPES code {other}"))),
        }
    }

    /// Bytes occupied by one voxel of this type.
    pub fn byte_width(self) -> usize {
        match self {
            Self::Byte => 1,
            Self::Short => 2,
            Self::Float => 4,
            Self::Complex => 8,
        }
    }
}

/// A loaded AFNI volume: its header plus one `f32` value array per sub-brick.
#[derive(Debug, Clone)]
pub struct Brik {
    /// The parsed `.HEAD` header.
    pub header: Header,
    /// Voxel dimensions `[nx, ny, nz]`.
    pub dimensions: [usize; 3],
    /// One value array per sub-brick, each `nx*ny*nz` long, scaled to true
    /// values. Complex sub-bricks store interleaved real/imaginary pairs and
    /// therefore have `2*nx*ny*nz` entries.
    pub sub_bricks: Vec<Vec<f32>>,
}

impl Brik {
    /// Load an AFNI dataset, given the path to any member of the pair: the
    /// `.HEAD`, the `.BRIK`, or a gzip-compressed `.BRIK.gz`. The other member
    /// is located automatically, and a `.BRIK.gz` is decompressed in memory.
    pub fn read(path: impl AsRef<Path>) -> Result<Self> {
        let (head_path, brik_path) = resolve_pair(path.as_ref())?;
        let header = Header::read(&head_path)?;
        let mut bytes = error::read_file(&brik_path)?;
        if crate::compress::is_gzip(&bytes) {
            bytes = crate::compress::gunzip(&bytes)?;
        }
        Self::from_parts(header, &bytes)
    }

    /// Combine an already-parsed header with raw `.BRIK` bytes.
    pub fn from_parts(header: Header, brik_bytes: &[u8]) -> Result<Self> {
        let dimensions = header
            .dimensions()
            .ok_or_else(|| Error::missing("DATASET_DIMENSIONS"))?;
        let voxels = dimensions[0] * dimensions[1] * dimensions[2];
        let nvals = header.nvals();
        let little = header.brik_is_little_endian();
        let codes = header.brick_types();
        let facs = header.brick_float_facs();

        let mut sub_bricks = Vec::with_capacity(nvals);
        let mut offset = 0;
        for p in 0..nvals {
            let code = *codes.get(p).ok_or_else(|| {
                Error::invalid(format!("BRICK_TYPES has no entry for sub-brick {p}"))
            })?;
            let ty = BrikType::from_code(code)?;
            let scalars = if ty == BrikType::Complex {
                voxels * 2
            } else {
                voxels
            };
            let needed = scalars
                * if ty == BrikType::Complex {
                    4
                } else {
                    ty.byte_width()
                };
            if offset + needed > brik_bytes.len() {
                return Err(Error::parse(format!(
                    "BRIK ended early reading sub-brick {p}"
                )));
            }
            let chunk = &brik_bytes[offset..offset + needed];
            offset += needed;

            let fac = facs.get(p).copied().unwrap_or(0.0);
            sub_bricks.push(decode_sub_brick(ty, chunk, scalars, little, fac));
        }

        Ok(Self {
            header,
            dimensions,
            sub_bricks,
        })
    }

    /// Number of sub-bricks.
    pub fn nvals(&self) -> usize {
        self.sub_bricks.len()
    }

    /// Number of voxels per sub-brick (`nx*ny*nz`).
    pub fn voxels(&self) -> usize {
        self.dimensions[0] * self.dimensions[1] * self.dimensions[2]
    }

    /// Fetch the scaled value at voxel `(i, j, k)` in sub-brick `p`.
    ///
    /// For complex sub-bricks this returns the real part; use
    /// [`Brik::sub_bricks`] directly for the imaginary component.
    pub fn value(&self, i: usize, j: usize, k: usize, p: usize) -> Option<f32> {
        let [nx, ny, nz] = self.dimensions;
        if i >= nx || j >= ny || k >= nz {
            return None;
        }
        let brick = self.sub_bricks.get(p)?;
        let stride = if brick.len() == self.voxels() * 2 {
            2
        } else {
            1
        };
        brick.get((i + j * nx + k * nx * ny) * stride).copied()
    }
}

fn decode_sub_brick(
    ty: BrikType,
    bytes: &[u8],
    scalars: usize,
    little: bool,
    fac: f64,
) -> Vec<f32> {
    let scale = if fac > 0.0 { fac as f32 } else { 1.0 };
    let mut out = Vec::with_capacity(scalars);
    match ty {
        BrikType::Byte => {
            for &b in bytes.iter().take(scalars) {
                out.push(b as f32 * scale);
            }
        }
        BrikType::Short => {
            for chunk in bytes.chunks_exact(2).take(scalars) {
                let raw = [chunk[0], chunk[1]];
                let v = if little {
                    i16::from_le_bytes(raw)
                } else {
                    i16::from_be_bytes(raw)
                };
                out.push(v as f32 * scale);
            }
        }
        BrikType::Float | BrikType::Complex => {
            for chunk in bytes.chunks_exact(4).take(scalars) {
                let raw = [chunk[0], chunk[1], chunk[2], chunk[3]];
                let v = if little {
                    f32::from_le_bytes(raw)
                } else {
                    f32::from_be_bytes(raw)
                };
                out.push(v * scale);
            }
        }
    }
    out
}

/// Given any member of a HEAD/BRIK pair (`.HEAD`, `.BRIK`, or `.BRIK.gz`),
/// return `(head_path, brik_path)`. The `.BRIK.gz` is preferred only when no
/// plain `.BRIK` is present; the caller decompresses as needed.
fn resolve_pair(path: &Path) -> Result<(PathBuf, PathBuf)> {
    let name = path
        .to_str()
        .ok_or_else(|| Error::invalid("non-UTF-8 dataset path"))?;

    let stem = if let Some(rest) = name.strip_suffix(".HEAD") {
        rest
    } else if let Some(rest) = name.strip_suffix(".BRIK.gz") {
        rest
    } else if let Some(rest) = name.strip_suffix(".BRIK") {
        rest
    } else {
        return Err(Error::invalid(format!(
            "{name}: expected a .HEAD, .BRIK, or .BRIK.gz path"
        )));
    };

    let head = PathBuf::from(format!("{stem}.HEAD"));
    let plain = PathBuf::from(format!("{stem}.BRIK"));
    let gz = PathBuf::from(format!("{stem}.BRIK.gz"));
    let brik = if plain.exists() {
        plain
    } else if gz.exists() {
        gz
    } else {
        plain
    };
    Ok((head, brik))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::head::{AttributeValue, Header};

    fn header(nvals: i64, dims: [i64; 3], types: Vec<i64>, facs: Vec<f64>) -> Header {
        let mut h = Header::default();
        h.set("DATASET_RANK", AttributeValue::Int(vec![3, nvals]));
        h.set("DATASET_DIMENSIONS", AttributeValue::Int(dims.to_vec()));
        h.set("BRICK_TYPES", AttributeValue::Int(types));
        h.set("BRICK_FLOAT_FACS", AttributeValue::Float(facs));
        h.set(
            "BYTEORDER_STRING",
            AttributeValue::String("LSB_FIRST".into()),
        );
        h
    }

    #[test]
    fn reads_scaled_short_brick() {
        // 2x1x1 volume, one short sub-brick, scale factor 0.5.
        let h = header(1, [2, 1, 1], vec![1], vec![0.5]);
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&10i16.to_le_bytes());
        bytes.extend_from_slice(&20i16.to_le_bytes());

        let brik = Brik::from_parts(h, &bytes).unwrap();
        assert_eq!(brik.nvals(), 1);
        assert_eq!(brik.value(0, 0, 0, 0), Some(5.0));
        assert_eq!(brik.value(1, 0, 0, 0), Some(10.0));
    }

    #[test]
    fn reads_float_brick_unscaled() {
        let h = header(1, [1, 1, 1], vec![3], vec![0.0]);
        let bytes = 1.25f32.to_le_bytes();
        let brik = Brik::from_parts(h, &bytes).unwrap();
        assert_eq!(brik.value(0, 0, 0, 0), Some(1.25));
    }

    #[test]
    fn reads_head_and_gzipped_brik_pair() {
        // Write a real .HEAD + .BRIK.gz pair to a temp dir and read it back
        // from the .HEAD path, exercising sibling resolution + gunzip.
        let dir = std::env::temp_dir().join(format!("afni_brik_test_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let stem = dir.join("vol+orig");

        let h = header(1, [2, 1, 1], vec![1], vec![0.5]);
        h.write(stem.with_extension("HEAD")).unwrap();

        let mut raw = Vec::new();
        raw.extend_from_slice(&10i16.to_le_bytes());
        raw.extend_from_slice(&20i16.to_le_bytes());
        let gz = crate::compress::gzip(&raw);
        // `vol+orig` -> `vol+orig.BRIK.gz`
        std::fs::write(format!("{}.BRIK.gz", stem.display()), gz).unwrap();

        let brik = Brik::read(stem.with_extension("HEAD")).unwrap();
        assert_eq!(brik.value(0, 0, 0, 0), Some(5.0));
        assert_eq!(brik.value(1, 0, 0, 0), Some(10.0));

        std::fs::remove_dir_all(&dir).ok();
    }
}
