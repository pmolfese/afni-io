//! FreeSurfer / SUMA ASCII surface meshes (`.asc`).
//!
//! This is the plain-text triangular-mesh format SUMA reads and writes for
//! surfaces. The layout is:
//!
//! ```text
//! #!ascii version of <something>      (one comment line, starts with '#')
//! <n_vertices> <n_faces>
//! x y z 0                             (n_vertices lines)
//! ...
//! v0 v1 v2 0                          (n_faces lines)
//! ...
//! ```
//!
//! Each vertex and face line carries a trailing flag column (usually `0`) that
//! is preserved on read so files round-trip.
//!
//! Reference: FreeSurfer `write_surface`/`read_surface` ASCII format; SUMA's
//! `SUMA_FreeSurfer_Read`.

use std::path::Path;

use crate::error::{self, from_utf8, Error, Result};

/// A triangular surface mesh.
#[derive(Debug, Clone, PartialEq)]
pub struct Surface {
    /// The leading comment line, without the leading `#` (if any was present).
    pub comment: Option<String>,
    /// Vertex coordinates, one `[x, y, z]` per node.
    pub vertices: Vec<[f32; 3]>,
    /// Triangles as triples of zero-based vertex indices.
    pub faces: Vec<[u32; 3]>,
    /// Per-vertex trailing flag values (parallel to `vertices`).
    pub vertex_flags: Vec<i32>,
    /// Per-face trailing flag values (parallel to `faces`).
    pub face_flags: Vec<i32>,
}

impl Surface {
    /// Read a `.asc` surface from disk.
    pub fn read(path: impl AsRef<Path>) -> Result<Self> {
        let bytes = error::read_file(path.as_ref())?;
        Self::parse(&from_utf8(&bytes, "ASCII surface")?)
    }

    /// Parse a `.asc` surface from text.
    pub fn parse(text: &str) -> Result<Self> {
        let mut lines = text.lines();

        // Optional leading comment line.
        let mut comment = None;
        let mut counts_line = None;
        for line in lines.by_ref() {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            if let Some(rest) = trimmed.strip_prefix('#') {
                comment = Some(rest.trim_start_matches('!').trim().to_string());
                continue;
            }
            counts_line = Some(trimmed);
            break;
        }

        let counts_line =
            counts_line.ok_or_else(|| Error::parse("surface: missing counts line"))?;
        let counts: Vec<usize> = counts_line
            .split_whitespace()
            .map(|t| t.parse::<usize>())
            .collect::<std::result::Result<_, _>>()
            .map_err(|_| Error::parse("surface: invalid vertex/face counts"))?;
        if counts.len() < 2 {
            return Err(Error::parse("surface: counts line needs two numbers"));
        }
        let (n_vertices, n_faces) = (counts[0], counts[1]);

        let mut vertices = Vec::with_capacity(n_vertices);
        let mut vertex_flags = Vec::with_capacity(n_vertices);
        for _ in 0..n_vertices {
            let line = next_data_line(&mut lines)
                .ok_or_else(|| Error::parse("surface: too few vertex lines"))?;
            let mut it = line.split_whitespace();
            let x = parse_f32(it.next())?;
            let y = parse_f32(it.next())?;
            let z = parse_f32(it.next())?;
            vertices.push([x, y, z]);
            vertex_flags.push(it.next().and_then(|t| t.parse::<i32>().ok()).unwrap_or(0));
        }

        let mut faces = Vec::with_capacity(n_faces);
        let mut face_flags = Vec::with_capacity(n_faces);
        for _ in 0..n_faces {
            let line = next_data_line(&mut lines)
                .ok_or_else(|| Error::parse("surface: too few face lines"))?;
            let mut it = line.split_whitespace();
            let a = parse_u32(it.next())?;
            let b = parse_u32(it.next())?;
            let c = parse_u32(it.next())?;
            faces.push([a, b, c]);
            face_flags.push(it.next().and_then(|t| t.parse::<i32>().ok()).unwrap_or(0));
        }

        Ok(Self {
            comment,
            vertices,
            faces,
            vertex_flags,
            face_flags,
        })
    }

    /// Build a surface from raw geometry (flags default to `0`).
    pub fn from_geometry(vertices: Vec<[f32; 3]>, faces: Vec<[u32; 3]>) -> Self {
        let vertex_flags = vec![0; vertices.len()];
        let face_flags = vec![0; faces.len()];
        Self {
            comment: None,
            vertices,
            faces,
            vertex_flags,
            face_flags,
        }
    }

    /// Number of vertices.
    pub fn n_vertices(&self) -> usize {
        self.vertices.len()
    }

    /// Number of faces.
    pub fn n_faces(&self) -> usize {
        self.faces.len()
    }

    /// Serialise the mesh to `.asc` text.
    pub fn to_asc_string(&self) -> String {
        let mut out = String::new();
        let comment = self.comment.as_deref().unwrap_or("ascii version");
        out.push_str("#!");
        out.push_str(comment);
        out.push('\n');
        out.push_str(&format!("{} {}\n", self.n_vertices(), self.n_faces()));
        for (i, v) in self.vertices.iter().enumerate() {
            let flag = self.vertex_flags.get(i).copied().unwrap_or(0);
            out.push_str(&format!(
                "{} {} {} {}\n",
                fmt(v[0]),
                fmt(v[1]),
                fmt(v[2]),
                flag
            ));
        }
        for (i, f) in self.faces.iter().enumerate() {
            let flag = self.face_flags.get(i).copied().unwrap_or(0);
            out.push_str(&format!("{} {} {} {}\n", f[0], f[1], f[2], flag));
        }
        out
    }

    /// Write the mesh to a `.asc` file.
    pub fn write(&self, path: impl AsRef<Path>) -> Result<()> {
        error::write_file(path.as_ref(), self.to_asc_string().as_bytes())
    }
}

fn next_data_line<'a>(lines: &mut std::str::Lines<'a>) -> Option<&'a str> {
    for line in lines.by_ref() {
        let trimmed = line.trim();
        if !trimmed.is_empty() && !trimmed.starts_with('#') {
            return Some(trimmed);
        }
    }
    None
}

fn parse_f32(token: Option<&str>) -> Result<f32> {
    token
        .ok_or_else(|| Error::parse("surface: missing coordinate"))?
        .parse::<f32>()
        .map_err(|_| Error::parse("surface: invalid coordinate"))
}

fn parse_u32(token: Option<&str>) -> Result<u32> {
    token
        .ok_or_else(|| Error::parse("surface: missing vertex index"))?
        .parse::<u32>()
        .map_err(|_| Error::parse("surface: invalid vertex index"))
}

fn fmt(value: f32) -> String {
    crate::niml::format_float(value as f64)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "#!ascii version of lh.pial\n4 2\n0.0 0.0 0.0 0\n1.0 0.0 0.0 0\n0.0 1.0 0.0 0\n0.0 0.0 1.0 0\n0 1 2 0\n0 1 3 0\n";

    #[test]
    fn reads_mesh() {
        let surf = Surface::parse(SAMPLE).unwrap();
        assert_eq!(surf.n_vertices(), 4);
        assert_eq!(surf.n_faces(), 2);
        assert_eq!(surf.vertices[1], [1.0, 0.0, 0.0]);
        assert_eq!(surf.faces[1], [0, 1, 3]);
        assert_eq!(surf.comment.as_deref(), Some("ascii version of lh.pial"));
    }

    #[test]
    fn round_trips() {
        let surf = Surface::parse(SAMPLE).unwrap();
        let reparsed = Surface::parse(&surf.to_asc_string()).unwrap();
        assert_eq!(surf.vertices, reparsed.vertices);
        assert_eq!(surf.faces, reparsed.faces);
    }
}
