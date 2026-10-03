//! FreeSurfer binary triangle surfaces (`lh.white`, `rh.pial`, `lh.inflated`, …).
//!
//! The file is big-endian throughout:
//!
//! | bytes | content |
//! |-------|---------|
//! | 3 | magic `0xFFFFFE` (triangle surface) |
//! | … | creation line, ending in `\n` |
//! | … | a second line (usually empty), ending in `\n` |
//! | 4 | vertex count (`int32`) |
//! | 4 | triangle count (`int32`) |
//! | 12 × vertices | `x y z` as `float32` |
//! | 12 × triangles | three `int32` vertex indices |
//! | … | optional footer: tagged volume geometry (kept as raw bytes) |
//!
//! This is what AFNI's `SUMA_FreeSurfer_ReadBin_eng` reads (`ConvertSurface
//! -i_fs`), including its limits: triangle files only, 5000 bytes at most for
//! the comment, and at most 2,000,000 vertices or triangles. The old quad
//! formats (`0xFFFFFF`, `0xFFFFFD`) are refused, as AFNI refuses them.
//!
//! The footer that recent FreeSurfer versions append (a tag followed by
//! `key = value` lines naming the volume the surface was made from) is not
//! interpreted; it is kept as bytes so a read followed by a write loses
//! nothing. [`FreeSurferSurface::footer_values`] picks `key = a b c` lines out
//! of it.
//!
//! ```
//! use afni_io::freesurfer::FreeSurferSurface;
//!
//! let surface = FreeSurferSurface::new(
//!     vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]],
//!     vec![[0, 1, 2]],
//! );
//! let bytes = surface.to_bytes()?;
//! assert_eq!(FreeSurferSurface::parse(&bytes)?, surface);
//! # Ok::<(), afni_io::Error>(())
//! ```

use std::path::Path;

use crate::error::{self, Error, Result};
use crate::surface::Surface;

const TRIANGLE_MAGIC: u32 = 0x00FF_FFFE;
const QUAD_MAGIC: u32 = 0x00FF_FFFF;
const NEW_QUAD_MAGIC: u32 = 0x00FF_FFFD;
/// AFNI refuses a comment line longer than this.
const MAX_COMMENT: usize = 5000;
/// AFNI refuses more vertices or triangles than this.
const MAX_COUNT: usize = 2_000_000;

/// The line FreeSurfer's own tools leave in the creation stamp is
/// `created by <user> on <host> at <date>`; this is what we write.
pub const DEFAULT_CREATED_BY: &str = "created by afni-io";

/// A FreeSurfer triangle surface.
#[derive(Debug, Clone, PartialEq)]
pub struct FreeSurferSurface {
    /// The creation line, without its newline (`created by …`).
    pub created_by: String,
    /// The second header line, without its newline. Usually empty.
    pub comment: String,
    /// Vertex coordinates (FreeSurfer "surface RAS", in mm).
    pub vertices: Vec<[f32; 3]>,
    /// Triangles as zero-based vertex indices.
    pub faces: Vec<[u32; 3]>,
    /// Whatever follows the face array (volume-geometry tag and text), as read.
    pub footer: Vec<u8>,
}

impl FreeSurferSurface {
    /// A surface from geometry, with the default creation line and no footer.
    pub fn new(vertices: Vec<[f32; 3]>, faces: Vec<[u32; 3]>) -> Self {
        Self {
            created_by: DEFAULT_CREATED_BY.to_string(),
            comment: String::new(),
            vertices,
            faces,
            footer: Vec::new(),
        }
    }

    /// Read a binary surface from disk.
    pub fn read(path: impl AsRef<Path>) -> Result<Self> {
        Self::parse(&error::read_file(path.as_ref())?)
    }

    /// Whether `bytes` start with the triangle-surface magic number.
    pub fn is_triangle_surface(bytes: &[u8]) -> bool {
        bytes.len() >= 3 && magic(bytes) == TRIANGLE_MAGIC
    }

    /// Parse a binary surface.
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        if bytes.len() < 3 {
            return Err(Error::parse("FreeSurfer surface is shorter than its magic"));
        }
        match magic(bytes) {
            TRIANGLE_MAGIC => {}
            QUAD_MAGIC | NEW_QUAD_MAGIC => {
                return Err(Error::unsupported(
                    "legacy FreeSurfer quad surfaces; convert to the triangle format",
                ))
            }
            other => {
                return Err(Error::parse(format!(
                    "not a FreeSurfer binary surface (magic {other:#08x})"
                )))
            }
        }
        let mut at = 3;
        let created_by = take_line(bytes, &mut at, "creation line")?;
        let comment = take_line(bytes, &mut at, "second header line")?;
        let n_vertices = take_count(bytes, &mut at, "vertex count")?;
        let n_faces = take_count(bytes, &mut at, "triangle count")?;

        let vertex_bytes = take(bytes, &mut at, n_vertices * 12, "vertices")?;
        let vertices: Vec<[f32; 3]> = vertex_bytes
            .chunks_exact(12)
            .map(|c| {
                [
                    f32::from_be_bytes([c[0], c[1], c[2], c[3]]),
                    f32::from_be_bytes([c[4], c[5], c[6], c[7]]),
                    f32::from_be_bytes([c[8], c[9], c[10], c[11]]),
                ]
            })
            .collect();
        if let Some(bad) = vertices
            .iter()
            .position(|v| v.iter().any(|x| !x.is_finite()))
        {
            return Err(Error::invalid(format!(
                "FreeSurfer vertex {bad} has a non-finite coordinate"
            )));
        }

        let face_bytes = take(bytes, &mut at, n_faces * 12, "triangles")?;
        let mut faces = Vec::with_capacity(n_faces);
        for (face, c) in face_bytes.chunks_exact(12).enumerate() {
            let mut triangle = [0u32; 3];
            for (k, node) in triangle.iter_mut().enumerate() {
                let index =
                    i32::from_be_bytes([c[4 * k], c[4 * k + 1], c[4 * k + 2], c[4 * k + 3]]);
                if index < 0 || index as usize >= n_vertices {
                    return Err(Error::invalid(format!(
                        "FreeSurfer triangle {face} names vertex {index}, \
                         but there are {n_vertices} vertices"
                    )));
                }
                *node = index as u32;
            }
            faces.push(triangle);
        }

        Ok(Self {
            created_by,
            comment,
            vertices,
            faces,
            footer: bytes[at..].to_vec(),
        })
    }

    /// The file's bytes. Fails if a triangle names a missing vertex, if there
    /// are more than 2,000,000 vertices or triangles (AFNI would not read
    /// them), or if a header line holds a newline.
    pub fn to_bytes(&self) -> Result<Vec<u8>> {
        if self.vertices.len() > MAX_COUNT || self.faces.len() > MAX_COUNT {
            return Err(Error::invalid(format!(
                "AFNI reads at most {MAX_COUNT} vertices or triangles"
            )));
        }
        for line in [&self.created_by, &self.comment] {
            if line.contains('\n') {
                return Err(Error::invalid(
                    "FreeSurfer header lines cannot hold a newline",
                ));
            }
            if line.len() >= MAX_COMMENT {
                return Err(Error::invalid("FreeSurfer header line is too long"));
            }
        }
        if let Some(bad) = self
            .faces
            .iter()
            .flatten()
            .find(|i| **i as usize >= self.vertices.len())
        {
            return Err(Error::invalid(format!(
                "a triangle names vertex {bad}, but there are {} vertices",
                self.vertices.len()
            )));
        }

        let mut out = Vec::with_capacity(
            3 + self.created_by.len()
                + self.comment.len()
                + 10
                + 12 * (self.vertices.len() + self.faces.len()),
        );
        out.extend_from_slice(&TRIANGLE_MAGIC.to_be_bytes()[1..]);
        out.extend_from_slice(self.created_by.as_bytes());
        out.push(b'\n');
        out.extend_from_slice(self.comment.as_bytes());
        out.push(b'\n');
        out.extend_from_slice(&(self.vertices.len() as i32).to_be_bytes());
        out.extend_from_slice(&(self.faces.len() as i32).to_be_bytes());
        for v in &self.vertices {
            for x in v {
                out.extend_from_slice(&x.to_be_bytes());
            }
        }
        for f in &self.faces {
            for i in f {
                out.extend_from_slice(&(*i as i32).to_be_bytes());
            }
        }
        out.extend_from_slice(&self.footer);
        Ok(out)
    }

    /// Write the surface to `path`.
    pub fn write(&self, path: impl AsRef<Path>) -> Result<()> {
        error::write_file(path.as_ref(), &self.to_bytes()?)
    }

    /// The geometry as an ASCII-surface [`Surface`] (no flags).
    pub fn to_surface(&self) -> Surface {
        Surface::from_geometry(self.vertices.clone(), self.faces.clone())
    }

    /// A binary surface from an ASCII one (the comment and flags are dropped).
    pub fn from_surface(surface: &Surface) -> Self {
        Self::new(surface.vertices.clone(), surface.faces.clone())
    }

    /// The numbers on the footer's `key = a b c` line, if there is one. Looks
    /// for the first line, in any printable stretch of the footer, that starts
    /// with `key` and an `=` (for example `cras` or `voxelsize`).
    pub fn footer_values(&self, key: &str) -> Option<Vec<f64>> {
        let text: String = self
            .footer
            .iter()
            .map(|b| {
                if b.is_ascii_graphic() || *b == b' ' || *b == b'\n' {
                    *b as char
                } else {
                    '\n'
                }
            })
            .collect();
        text.lines().find_map(|line| {
            let (name, rest) = line.split_once('=')?;
            if name.trim() != key {
                return None;
            }
            let values: Vec<f64> = rest
                .split('#')
                .next()?
                .split_whitespace()
                .map(|v| v.parse().ok())
                .collect::<Option<_>>()?;
            Some(values)
        })
    }
}

fn magic(bytes: &[u8]) -> u32 {
    u32::from_be_bytes([0, bytes[0], bytes[1], bytes[2]])
}

fn take<'a>(bytes: &'a [u8], at: &mut usize, n: usize, what: &str) -> Result<&'a [u8]> {
    let end = at
        .checked_add(n)
        .filter(|end| *end <= bytes.len())
        .ok_or_else(|| Error::parse(format!("FreeSurfer surface ends inside its {what}")))?;
    let slice = &bytes[*at..end];
    *at = end;
    Ok(slice)
}

fn take_line(bytes: &[u8], at: &mut usize, what: &str) -> Result<String> {
    let rest = &bytes[*at..];
    let end = rest
        .iter()
        .take(MAX_COMMENT)
        .position(|b| *b == b'\n')
        .ok_or_else(|| {
            Error::parse(format!(
                "FreeSurfer {what} has no newline in {MAX_COMMENT} bytes"
            ))
        })?;
    *at += end + 1;
    Ok(String::from_utf8_lossy(&rest[..end]).into_owned())
}

fn take_count(bytes: &[u8], at: &mut usize, what: &str) -> Result<usize> {
    let raw = take(bytes, at, 4, what)?;
    let value = i32::from_be_bytes([raw[0], raw[1], raw[2], raw[3]]);
    if value < 0 || value as usize > MAX_COUNT {
        return Err(Error::invalid(format!(
            "FreeSurfer {what} {value} is outside 0..={MAX_COUNT}"
        )));
    }
    Ok(value as usize)
}
