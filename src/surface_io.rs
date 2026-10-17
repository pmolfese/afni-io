//! Unified readers for triangulated surface mesh formats used by SUMA.
//!
//! The format-specific modules remain available, while this module provides a
//! single dispatch point for applications such as Sumaru and DriveSuma.

use std::collections::HashMap;
use std::path::Path;

use crate::error;
use crate::surface::Surface;
use crate::{Error, Result};

/// A surface format understood by [`read_surface_as`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum SurfaceFormat {
    /// GIfTI XML, optionally gzip-compressed.
    Gifti,
    /// FreeSurfer/SUMA ASCII surface.
    FreeSurferAscii,
    /// FreeSurfer binary triangle surface.
    FreeSurferBinary,
    /// Separate AFNI `.1D`/Vec coordinate and triangle tables.
    OneD,
    /// Wavefront OBJ.
    WavefrontObj,
    /// Polygon File Format, ASCII or binary.
    Ply,
    /// Stereolithography, ASCII or binary.
    Stl,
    /// Movie.BYU polygon format.
    Byu,
    /// OpenDX field mesh (ASCII positions and connections).
    OpenDx,
    /// Open Inventor ASCII `Coordinate3`/`IndexedFaceSet` mesh.
    OpenInventor,
    /// MNI polygon-object ASCII format.
    MniObj,
    /// BrainVoyager SRF binary format.
    BrainVoyager,
    /// Paired Caret/SureFit `.coord` and `.topo` files.
    SureFit,
}

impl SurfaceFormat {
    /// Infer a format from the conventional filename extension.
    pub fn from_path(path: impl AsRef<Path>) -> Option<Self> {
        let lower = path.as_ref().to_string_lossy().to_ascii_lowercase();
        if lower.ends_with(".gii") || lower.ends_with(".gii.gz") {
            Some(Self::Gifti)
        } else if lower.ends_with(".asc") {
            Some(Self::FreeSurferAscii)
        } else if lower.ends_with(".obj") {
            Some(Self::WavefrontObj)
        } else if lower.ends_with(".ply") {
            Some(Self::Ply)
        } else if lower.ends_with(".stl") {
            Some(Self::Stl)
        } else if lower.ends_with(".1d") || lower.ends_with(".vec") {
            Some(Self::OneD)
        } else if lower.ends_with(".g") || lower.ends_with(".byu") {
            Some(Self::Byu)
        } else if lower.ends_with(".dx") {
            Some(Self::OpenDx)
        } else if lower.ends_with(".iv") || lower.ends_with(".inventor") {
            Some(Self::OpenInventor)
        } else if lower.ends_with(".srf") {
            Some(Self::BrainVoyager)
        } else {
            None
        }
    }
}

/// Read a single-file surface, inferring its format from the filename.
///
/// Extensionless files use the FreeSurfer binary reader, matching the common
/// `lh.white`/`rh.pial` convention. Paired 1D surfaces must use
/// [`read_surface_pair`].
pub fn read_surface(path: impl AsRef<Path>) -> Result<Surface> {
    let path = path.as_ref();
    let format = SurfaceFormat::from_path(path).unwrap_or(SurfaceFormat::FreeSurferBinary);
    if format == SurfaceFormat::WavefrontObj {
        // Wavefront and MNI both conventionally use `.obj`. Their first
        // geometry token is unambiguous (`v` versus MNI's polygon `P`).
        let text = read_text(path)?;
        if text
            .lines()
            .map(str::trim)
            .find(|line| !line.is_empty() && !line.starts_with('#'))
            .and_then(|line| line.split_whitespace().next())
            == Some("P")
        {
            parse_mni_obj(&text)
        } else {
            parse_obj(&text)
        }
    } else {
        read_surface_as(path, format)
    }
}

/// Read a single-file surface using an explicit format.
pub fn read_surface_as(path: impl AsRef<Path>, format: SurfaceFormat) -> Result<Surface> {
    let path = path.as_ref();
    match format {
        SurfaceFormat::Gifti => crate::gifti::Gifti::read(path)?.to_surface(),
        SurfaceFormat::FreeSurferAscii => Surface::read(path),
        SurfaceFormat::FreeSurferBinary => {
            Ok(crate::freesurfer::FreeSurferSurface::read(path)?.to_surface())
        }
        SurfaceFormat::OneD => Err(Error::invalid(
            "1D surfaces require coordinate and topology paths; use read_surface_pair",
        )),
        SurfaceFormat::WavefrontObj => parse_obj(&read_text(path)?),
        SurfaceFormat::Ply => parse_ply(&error::read_file(path)?),
        SurfaceFormat::Stl => parse_stl(&error::read_file(path)?),
        SurfaceFormat::Byu => parse_byu(&read_text(path)?),
        SurfaceFormat::OpenDx => parse_open_dx(&read_text(path)?),
        SurfaceFormat::OpenInventor => parse_open_inventor(&read_text(path)?),
        SurfaceFormat::MniObj => parse_mni_obj(&read_text(path)?),
        SurfaceFormat::BrainVoyager => parse_brainvoyager(&error::read_file(path)?),
        SurfaceFormat::SureFit => Err(Error::invalid(
            "SureFit surfaces require coordinate and topology paths; use read_surface_pair",
        )),
    }
}

/// Read a paired coordinate/topology surface.
pub fn read_surface_pair(
    coordinates: impl AsRef<Path>,
    topology: impl AsRef<Path>,
    format: SurfaceFormat,
) -> Result<Surface> {
    match format {
        SurfaceFormat::OneD => read_1d_pair(coordinates.as_ref(), topology.as_ref()),
        SurfaceFormat::SureFit => read_surefit_pair(coordinates.as_ref(), topology.as_ref()),
        _ => Err(Error::invalid(format!(
            "{format:?} is not a paired surface format"
        ))),
    }
}

fn read_text(path: &Path) -> Result<String> {
    let bytes = error::read_file(path)?;
    error::from_utf8(&bytes, "surface mesh")
}

fn read_1d_pair(coordinates: &Path, topology: &Path) -> Result<Surface> {
    let xyz = crate::onedee::OneD::read(coordinates)?;
    let ijk = crate::onedee::OneD::read(topology)?;
    if xyz.cols < 3 || ijk.cols < 3 {
        return Err(Error::invalid(
            "1D surface coordinates and topology need at least three columns",
        ));
    }
    let vertices = (0..xyz.rows)
        .map(|row| {
            Ok([
                finite_f32(xyz.get(row, 0).unwrap_or(0.0), "1D coordinate")?,
                finite_f32(xyz.get(row, 1).unwrap_or(0.0), "1D coordinate")?,
                finite_f32(xyz.get(row, 2).unwrap_or(0.0), "1D coordinate")?,
            ])
        })
        .collect::<Result<Vec<_>>>()?;
    let faces = (0..ijk.rows)
        .map(|row| {
            Ok([
                index_value(ijk.get(row, 0).unwrap_or(-1.0), vertices.len())?,
                index_value(ijk.get(row, 1).unwrap_or(-1.0), vertices.len())?,
                index_value(ijk.get(row, 2).unwrap_or(-1.0), vertices.len())?,
            ])
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(Surface::from_geometry(vertices, faces))
}

fn read_surefit_pair(coordinates: &Path, topology: &Path) -> Result<Surface> {
    let coordinate_text = read_text(coordinates)?;
    let topology_text = read_text(topology)?;
    parse_surefit_pair(&coordinate_text, &topology_text)
}

fn parse_surefit_pair(coordinate_text: &str, topology_text: &str) -> Result<Surface> {
    let coordinate_data = after_end_header(coordinate_text, "SureFit coordinate")?;
    let mut coordinate_tokens = coordinate_data.split_whitespace();
    let vertex_count = next_usize(&mut coordinate_tokens, "SureFit vertex count")?;
    let mut vertices = Vec::with_capacity(vertex_count);
    for _ in 0..vertex_count {
        let _id = next_i64(&mut coordinate_tokens, "SureFit vertex id")?;
        vertices.push([
            next_f32(&mut coordinate_tokens, "SureFit vertex")?,
            next_f32(&mut coordinate_tokens, "SureFit vertex")?,
            next_f32(&mut coordinate_tokens, "SureFit vertex")?,
        ]);
    }

    let topology_data = after_end_header(topology_text, "SureFit topology")?;
    let tokens = topology_data.split_whitespace().collect::<Vec<_>>();
    let mut at = 0;
    let face_count;
    if tokens.first().map(|value| value.to_ascii_lowercase()) == Some("tag-version".into()) {
        at += 1;
        let _version = parse_token_f64(tokens.get(at), "SureFit tag version")?;
        at += 1;
        face_count = parse_token_usize(tokens.get(at), "SureFit face count")?;
        at += 1;
    } else {
        let node_specs = parse_token_usize(tokens.get(at), "SureFit node-spec count")?;
        at += 1;
        for _ in 0..node_specs {
            let specs = tokens
                .get(at..at + 6)
                .ok_or_else(|| Error::parse("SureFit: truncated node specification"))?;
            let neighbor_count = specs[1]
                .parse::<usize>()
                .map_err(|_| Error::parse("SureFit: invalid neighbor count"))?;
            at += 6;
            at = at
                .checked_add(neighbor_count.saturating_mul(2))
                .ok_or_else(|| Error::invalid("SureFit neighbor list is too large"))?;
            if at > tokens.len() {
                return Err(Error::parse("SureFit: truncated neighbor list"));
            }
        }
        face_count = parse_token_usize(tokens.get(at), "SureFit face count")?;
        at += 1;
    }
    let mut faces = Vec::with_capacity(face_count);
    for _ in 0..face_count {
        let values = tokens
            .get(at..at + 3)
            .ok_or_else(|| Error::parse("SureFit: truncated face list"))?;
        faces.push([
            parse_u32(values[0], "SureFit face")?,
            parse_u32(values[1], "SureFit face")?,
            parse_u32(values[2], "SureFit face")?,
        ]);
        at += 3;
    }
    finish_surface(vertices, faces, "SureFit")
}

fn after_end_header<'a>(text: &'a str, context: &str) -> Result<&'a str> {
    let at = text
        .find("EndHeader")
        .ok_or_else(|| Error::missing(format!("{context} EndHeader")))?;
    Ok(&text[at + "EndHeader".len()..])
}

fn next_usize<'a>(tokens: &mut impl Iterator<Item = &'a str>, context: &str) -> Result<usize> {
    tokens
        .next()
        .ok_or_else(|| Error::missing(context.to_string()))?
        .parse()
        .map_err(|_| Error::parse(format!("{context}: invalid integer")))
}

fn next_i64<'a>(tokens: &mut impl Iterator<Item = &'a str>, context: &str) -> Result<i64> {
    tokens
        .next()
        .ok_or_else(|| Error::missing(context.to_string()))?
        .parse()
        .map_err(|_| Error::parse(format!("{context}: invalid integer")))
}

fn next_f32<'a>(tokens: &mut impl Iterator<Item = &'a str>, context: &str) -> Result<f32> {
    let token = tokens
        .next()
        .ok_or_else(|| Error::missing(context.to_string()))?;
    parse_f32(token, context)
}

fn parse_obj(text: &str) -> Result<Surface> {
    let mut vertices = Vec::new();
    let mut faces = Vec::new();
    for (line_number, raw) in text.lines().enumerate() {
        let line = raw.split('#').next().unwrap_or("").trim();
        let mut fields = line.split_whitespace();
        match fields.next() {
            Some("v") => {
                let values = fields
                    .take(3)
                    .map(|value| parse_f32(value, "OBJ vertex"))
                    .collect::<Result<Vec<_>>>()?;
                if values.len() != 3 {
                    return Err(Error::parse(format!(
                        "OBJ line {}: vertex needs three coordinates",
                        line_number + 1
                    )));
                }
                vertices.push([values[0], values[1], values[2]]);
            }
            Some("f") => {
                let polygon = fields
                    .map(|field| parse_obj_index(field, vertices.len()))
                    .collect::<Result<Vec<_>>>()?;
                triangulate(&polygon, &mut faces, "OBJ face")?;
            }
            _ => {}
        }
    }
    finish_surface(vertices, faces, "OBJ")
}

fn parse_mni_obj(text: &str) -> Result<Surface> {
    let start = text
        .find('P')
        .ok_or_else(|| Error::missing("MNI OBJ polygon marker P"))?;
    let tokens = text[start + 1..].split_whitespace().collect::<Vec<_>>();
    let mut at = 0;
    // Five material/rendering values precede the vertex count.
    if tokens.len() < 6 {
        return Err(Error::parse("MNI OBJ: truncated polygon header"));
    }
    at += 5;
    let vertex_count = parse_token_usize(tokens.get(at), "MNI OBJ vertex count")?;
    at += 1;
    let mut vertices = Vec::with_capacity(vertex_count);
    for _ in 0..vertex_count {
        let xyz = tokens
            .get(at..at + 3)
            .ok_or_else(|| Error::parse("MNI OBJ: truncated vertex list"))?;
        vertices.push([
            parse_f32(xyz[0], "MNI OBJ vertex")?,
            parse_f32(xyz[1], "MNI OBJ vertex")?,
            parse_f32(xyz[2], "MNI OBJ vertex")?,
        ]);
        at += 3;
    }
    // The format stores one normal per vertex. Geometry consumers can
    // recompute them, so validate and skip the array.
    at = at
        .checked_add(vertex_count.saturating_mul(3))
        .ok_or_else(|| Error::invalid("MNI OBJ normal list is too large"))?;
    let face_count = parse_token_usize(tokens.get(at), "MNI OBJ face count")?;
    at += 1;
    // Five colour/material values and one cumulative end-index per polygon.
    at = at
        .checked_add(5 + face_count)
        .ok_or_else(|| Error::invalid("MNI OBJ face metadata is too large"))?;
    let mut faces = Vec::with_capacity(face_count);
    for _ in 0..face_count {
        let values = tokens
            .get(at..at + 3)
            .ok_or_else(|| Error::parse("MNI OBJ: truncated triangle list"))?;
        faces.push([
            parse_u32(values[0], "MNI OBJ triangle")?,
            parse_u32(values[1], "MNI OBJ triangle")?,
            parse_u32(values[2], "MNI OBJ triangle")?,
        ]);
        at += 3;
    }
    finish_surface(vertices, faces, "MNI OBJ")
}

fn parse_brainvoyager(bytes: &[u8]) -> Result<Surface> {
    if bytes.len() < 28 {
        return Err(Error::parse("BrainVoyager SRF: truncated header"));
    }
    let little_version = f32::from_le_bytes(bytes[0..4].try_into().unwrap());
    let little_vertices = i32::from_le_bytes(bytes[8..12].try_into().unwrap());
    let little_faces = i32::from_le_bytes(bytes[12..16].try_into().unwrap());
    let little = little_version.is_finite()
        && (0.0..=500_000.0).contains(&little_version)
        && little_vertices >= 0
        && little_faces >= 0;
    let big_version = f32::from_be_bytes(bytes[0..4].try_into().unwrap());
    let big_vertices = i32::from_be_bytes(bytes[8..12].try_into().unwrap());
    let big_faces = i32::from_be_bytes(bytes[12..16].try_into().unwrap());
    let (version, vertex_count, face_count, little) = if little {
        (little_version, little_vertices, little_faces, true)
    } else {
        (big_version, big_vertices, big_faces, false)
    };
    if !version.is_finite()
        || !(0.0..=500_000.0).contains(&version)
        || vertex_count < 0
        || face_count < 0
    {
        return Err(Error::parse(
            "BrainVoyager SRF: invalid header or byte order",
        ));
    }
    let vertex_count = vertex_count as usize;
    let face_count = face_count as usize;
    let mut cursor = BinaryCursor::new(bytes, 16, little);
    // Center is metadata, not an offset SUMA applies to coordinates.
    for _ in 0..3 {
        cursor.f32()?;
    }
    let mut vertices = vec![[0.0_f32; 3]; vertex_count];
    for axis in 0..3 {
        for vertex in &mut vertices {
            vertex[axis] = cursor.f32()?;
        }
    }
    // Stored normals and curvature colours are not needed for geometry.
    cursor.skip(vertex_count.saturating_mul(3).saturating_mul(4))?;
    cursor.skip(8 * 4)?;
    let mut visible = Vec::with_capacity(vertex_count);
    for _ in 0..vertex_count {
        visible.push(cursor.i32()? >= 0);
    }
    for _ in 0..vertex_count {
        let neighbors = cursor.i32()?;
        if neighbors < 0 {
            return Err(Error::parse("BrainVoyager SRF: negative neighbor count"));
        }
        cursor.skip((neighbors as usize).saturating_mul(4))?;
    }
    let mut faces = Vec::with_capacity(face_count);
    for _ in 0..face_count {
        let face = [cursor.u32()?, cursor.u32()?, cursor.u32()?];
        if face
            .iter()
            .all(|index| visible.get(*index as usize).copied().unwrap_or(false))
        {
            faces.push(face);
        }
    }
    finish_surface(vertices, faces, "BrainVoyager SRF")
}

struct BinaryCursor<'a> {
    bytes: &'a [u8],
    offset: usize,
    little: bool,
}

impl<'a> BinaryCursor<'a> {
    fn new(bytes: &'a [u8], offset: usize, little: bool) -> Self {
        Self {
            bytes,
            offset,
            little,
        }
    }

    fn take4(&mut self) -> Result<[u8; 4]> {
        let value = self
            .bytes
            .get(self.offset..self.offset + 4)
            .ok_or_else(|| Error::parse("BrainVoyager SRF: truncated data"))?;
        self.offset += 4;
        Ok(value.try_into().unwrap())
    }

    fn u32(&mut self) -> Result<u32> {
        let value = self.take4()?;
        Ok(if self.little {
            u32::from_le_bytes(value)
        } else {
            u32::from_be_bytes(value)
        })
    }

    fn i32(&mut self) -> Result<i32> {
        Ok(self.u32()? as i32)
    }

    fn f32(&mut self) -> Result<f32> {
        let value = f32::from_bits(self.u32()?);
        if value.is_finite() {
            Ok(value)
        } else {
            Err(Error::invalid(
                "BrainVoyager SRF contains non-finite coordinates",
            ))
        }
    }

    fn skip(&mut self, count: usize) -> Result<()> {
        self.offset = self
            .offset
            .checked_add(count)
            .filter(|offset| *offset <= self.bytes.len())
            .ok_or_else(|| Error::parse("BrainVoyager SRF: truncated data"))?;
        Ok(())
    }
}

fn parse_obj_index(value: &str, vertex_count: usize) -> Result<u32> {
    let raw = value.split('/').next().unwrap_or("");
    let index = raw
        .parse::<i64>()
        .map_err(|_| Error::parse(format!("OBJ: invalid face index {value:?}")))?;
    let zero = if index > 0 {
        index - 1
    } else if index < 0 {
        vertex_count as i64 + index
    } else {
        -1
    };
    if zero < 0 || zero as usize >= vertex_count {
        return Err(Error::invalid(format!(
            "OBJ face index {index} is outside {vertex_count} vertices"
        )));
    }
    Ok(zero as u32)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PlyEncoding {
    Ascii,
    Little,
    Big,
}

#[derive(Debug, Clone, Copy)]
enum PlyScalar {
    I8,
    U8,
    I16,
    U16,
    I32,
    U32,
    F32,
    F64,
}

impl PlyScalar {
    fn parse(value: &str) -> Result<Self> {
        match value {
            "char" | "int8" => Ok(Self::I8),
            "uchar" | "uint8" => Ok(Self::U8),
            "short" | "int16" => Ok(Self::I16),
            "ushort" | "uint16" => Ok(Self::U16),
            "int" | "int32" => Ok(Self::I32),
            "uint" | "uint32" => Ok(Self::U32),
            "float" | "float32" => Ok(Self::F32),
            "double" | "float64" => Ok(Self::F64),
            _ => Err(Error::unsupported(format!("PLY scalar type {value:?}"))),
        }
    }

    fn size(self) -> usize {
        match self {
            Self::I8 | Self::U8 => 1,
            Self::I16 | Self::U16 => 2,
            Self::I32 | Self::U32 | Self::F32 => 4,
            Self::F64 => 8,
        }
    }
}

#[derive(Debug, Clone)]
enum PlyProperty {
    Scalar(PlyScalar, String),
    List(PlyScalar, PlyScalar, String),
}

#[derive(Debug, Clone)]
struct PlyElement {
    name: String,
    count: usize,
    properties: Vec<PlyProperty>,
}

fn parse_ply(bytes: &[u8]) -> Result<Surface> {
    let header_end = bytes
        .windows(10)
        .position(|window| window == b"end_header")
        .ok_or_else(|| Error::parse("PLY: missing end_header"))?;
    let after_word = header_end + 10;
    let data_start = if bytes.get(after_word..after_word + 2) == Some(b"\r\n") {
        after_word + 2
    } else if bytes.get(after_word) == Some(&b'\n') {
        after_word + 1
    } else {
        return Err(Error::parse("PLY: end_header is not line terminated"));
    };
    let header = std::str::from_utf8(&bytes[..header_end]).map_err(|source| Error::Utf8 {
        context: "PLY header".into(),
        source,
    })?;
    let mut encoding = None;
    let mut elements: Vec<PlyElement> = Vec::new();
    for line in header.lines().map(str::trim) {
        let fields = line.split_whitespace().collect::<Vec<_>>();
        match fields.as_slice() {
            ["format", "ascii", _] => encoding = Some(PlyEncoding::Ascii),
            ["format", "binary_little_endian", _] => encoding = Some(PlyEncoding::Little),
            ["format", "binary_big_endian", _] => encoding = Some(PlyEncoding::Big),
            ["element", name, count] => elements.push(PlyElement {
                name: (*name).into(),
                count: count
                    .parse()
                    .map_err(|_| Error::parse("PLY: invalid element count"))?,
                properties: Vec::new(),
            }),
            ["property", kind, name] => elements
                .last_mut()
                .ok_or_else(|| Error::parse("PLY: property before element"))?
                .properties
                .push(PlyProperty::Scalar(PlyScalar::parse(kind)?, (*name).into())),
            ["property", "list", count_kind, item_kind, name] => elements
                .last_mut()
                .ok_or_else(|| Error::parse("PLY: property before element"))?
                .properties
                .push(PlyProperty::List(
                    PlyScalar::parse(count_kind)?,
                    PlyScalar::parse(item_kind)?,
                    (*name).into(),
                )),
            _ => {}
        }
    }
    let encoding = encoding.ok_or_else(|| Error::missing("PLY format declaration"))?;
    if encoding == PlyEncoding::Ascii {
        parse_ply_ascii(&bytes[data_start..], &elements)
    } else {
        parse_ply_binary(&bytes[data_start..], &elements, encoding)
    }
}

fn parse_ply_ascii(bytes: &[u8], elements: &[PlyElement]) -> Result<Surface> {
    let text = std::str::from_utf8(bytes).map_err(|source| Error::Utf8 {
        context: "ASCII PLY data".into(),
        source,
    })?;
    let mut lines = text.lines().filter(|line| !line.trim().is_empty());
    let mut vertices = Vec::new();
    let mut faces = Vec::new();
    for element in elements {
        for _ in 0..element.count {
            let line = lines
                .next()
                .ok_or_else(|| Error::parse("PLY: too few element records"))?;
            let fields = line.split_whitespace().collect::<Vec<_>>();
            if element.name == "vertex" {
                let mut cursor = 0;
                let mut xyz = [None, None, None];
                for property in &element.properties {
                    match property {
                        PlyProperty::Scalar(_, name) => {
                            let value = fields
                                .get(cursor)
                                .ok_or_else(|| Error::parse("PLY: short vertex property record"))?;
                            if let Some(slot) = xyz_slot(name) {
                                xyz[slot] = Some(parse_f32(value, "PLY vertex")?);
                            }
                            cursor += 1;
                        }
                        PlyProperty::List(_, _, _) => {
                            let count = parse_usize(fields.get(cursor), "PLY list count")?;
                            cursor += 1 + count;
                        }
                    }
                }
                vertices.push(required_xyz(xyz, "PLY")?);
            } else if element.name == "face" {
                let polygon = parse_ascii_face(&fields, &element.properties)?;
                triangulate(&polygon, &mut faces, "PLY face")?;
            }
        }
    }
    finish_surface(vertices, faces, "PLY")
}

fn parse_ascii_face(fields: &[&str], properties: &[PlyProperty]) -> Result<Vec<u32>> {
    let mut cursor = 0;
    let mut polygon = None;
    for property in properties {
        match property {
            PlyProperty::Scalar(_, _) => cursor += 1,
            PlyProperty::List(_, _, name) => {
                let count = parse_usize(fields.get(cursor), "PLY list count")?;
                cursor += 1;
                let values = fields
                    .get(cursor..cursor + count)
                    .ok_or_else(|| Error::parse("PLY: short face list"))?;
                if name == "vertex_indices" || name == "vertex_index" {
                    polygon = Some(
                        values
                            .iter()
                            .map(|value| {
                                value
                                    .parse::<u32>()
                                    .map_err(|_| Error::parse("PLY: invalid vertex index"))
                            })
                            .collect::<Result<Vec<_>>>()?,
                    );
                }
                cursor += count;
            }
        }
    }
    polygon.ok_or_else(|| Error::missing("PLY face vertex_indices property"))
}

fn parse_ply_binary(
    bytes: &[u8],
    elements: &[PlyElement],
    encoding: PlyEncoding,
) -> Result<Surface> {
    let mut cursor = ByteCursor::new(bytes, encoding);
    let mut vertices = Vec::new();
    let mut faces = Vec::new();
    for element in elements {
        for _ in 0..element.count {
            let mut xyz = [None, None, None];
            let mut polygon = None;
            for property in &element.properties {
                match property {
                    PlyProperty::Scalar(kind, name) => {
                        let value = cursor.scalar(*kind)?;
                        if element.name == "vertex" {
                            if let Some(slot) = xyz_slot(name) {
                                xyz[slot] = Some(finite_f32(value, "PLY vertex")?);
                            }
                        }
                    }
                    PlyProperty::List(count_kind, item_kind, name) => {
                        let count = cursor.scalar(*count_kind)?;
                        if count < 0.0 || count.fract() != 0.0 {
                            return Err(Error::parse("PLY: invalid binary list count"));
                        }
                        let mut values = Vec::with_capacity(count as usize);
                        for _ in 0..count as usize {
                            let value = cursor.scalar(*item_kind)?;
                            if element.name == "face"
                                && (name == "vertex_indices" || name == "vertex_index")
                            {
                                if value < 0.0 || value.fract() != 0.0 {
                                    return Err(Error::parse("PLY: invalid binary face index"));
                                }
                                values.push(value as u32);
                            }
                        }
                        if !values.is_empty() {
                            polygon = Some(values);
                        }
                    }
                }
            }
            if element.name == "vertex" {
                vertices.push(required_xyz(xyz, "PLY")?);
            } else if let Some(polygon) = polygon {
                triangulate(&polygon, &mut faces, "PLY face")?;
            }
        }
    }
    finish_surface(vertices, faces, "PLY")
}

struct ByteCursor<'a> {
    bytes: &'a [u8],
    offset: usize,
    encoding: PlyEncoding,
}

impl<'a> ByteCursor<'a> {
    fn new(bytes: &'a [u8], encoding: PlyEncoding) -> Self {
        Self {
            bytes,
            offset: 0,
            encoding,
        }
    }

    fn scalar(&mut self, kind: PlyScalar) -> Result<f64> {
        let size = kind.size();
        let bytes = self
            .bytes
            .get(self.offset..self.offset + size)
            .ok_or_else(|| Error::parse("PLY: truncated binary data"))?;
        self.offset += size;
        let little = self.encoding == PlyEncoding::Little;
        Ok(match kind {
            PlyScalar::I8 => i8::from_ne_bytes([bytes[0]]) as f64,
            PlyScalar::U8 => bytes[0] as f64,
            PlyScalar::I16 => endian_i16(bytes, little) as f64,
            PlyScalar::U16 => endian_u16(bytes, little) as f64,
            PlyScalar::I32 => endian_i32(bytes, little) as f64,
            PlyScalar::U32 => endian_u32(bytes, little) as f64,
            PlyScalar::F32 => f32::from_bits(endian_u32(bytes, little)) as f64,
            PlyScalar::F64 => f64::from_bits(endian_u64(bytes, little)),
        })
    }
}

fn parse_stl(bytes: &[u8]) -> Result<Surface> {
    let binary_count = bytes
        .get(80..84)
        .map(|value| u32::from_le_bytes(value.try_into().unwrap()) as usize);
    if binary_count.and_then(|count| 84usize.checked_add(count.checked_mul(50)?))
        == Some(bytes.len())
    {
        parse_binary_stl(bytes, binary_count.unwrap())
    } else {
        let text = std::str::from_utf8(bytes).map_err(|source| Error::Utf8 {
            context: "ASCII STL".into(),
            source,
        })?;
        parse_ascii_stl(text)
    }
}

fn parse_binary_stl(bytes: &[u8], count: usize) -> Result<Surface> {
    let mut vertices = Vec::new();
    let mut faces = Vec::with_capacity(count);
    let mut known = HashMap::new();
    for triangle in 0..count {
        let base = 84 + triangle * 50 + 12;
        let mut face = [0_u32; 3];
        for (corner, slot) in face.iter_mut().enumerate() {
            let at = base + corner * 12;
            let vertex = [
                read_le_f32(bytes, at)?,
                read_le_f32(bytes, at + 4)?,
                read_le_f32(bytes, at + 8)?,
            ];
            *slot = intern_vertex(vertex, &mut vertices, &mut known);
        }
        faces.push(face);
    }
    finish_surface(vertices, faces, "STL")
}

fn parse_ascii_stl(text: &str) -> Result<Surface> {
    let mut vertices = Vec::new();
    let mut faces = Vec::new();
    let mut known = HashMap::new();
    let mut triangle = Vec::with_capacity(3);
    for line in text.lines().map(str::trim) {
        let fields = line.split_whitespace().collect::<Vec<_>>();
        if fields.first() == Some(&"vertex") {
            if fields.len() < 4 {
                return Err(Error::parse("STL: short vertex record"));
            }
            let vertex = [
                parse_f32(fields[1], "STL vertex")?,
                parse_f32(fields[2], "STL vertex")?,
                parse_f32(fields[3], "STL vertex")?,
            ];
            triangle.push(intern_vertex(vertex, &mut vertices, &mut known));
            if triangle.len() == 3 {
                faces.push([triangle[0], triangle[1], triangle[2]]);
                triangle.clear();
            }
        }
    }
    if !triangle.is_empty() {
        return Err(Error::parse("STL: incomplete triangle"));
    }
    finish_surface(vertices, faces, "STL")
}

fn parse_byu(text: &str) -> Result<Surface> {
    let tokens = text.split_whitespace().collect::<Vec<_>>();
    if tokens.len() < 4 {
        return Err(Error::parse("BYU: missing header"));
    }
    let parts = parse_usize(tokens.first(), "BYU part count")?;
    let vertex_count = parse_usize(tokens.get(1), "BYU vertex count")?;
    let polygon_count = parse_usize(tokens.get(2), "BYU polygon count")?;
    let mut at = 4 + parts * 2;
    let mut vertices = Vec::with_capacity(vertex_count);
    for _ in 0..vertex_count {
        let xyz = tokens
            .get(at..at + 3)
            .ok_or_else(|| Error::parse("BYU: truncated vertices"))?;
        vertices.push([
            parse_f32(xyz[0], "BYU vertex")?,
            parse_f32(xyz[1], "BYU vertex")?,
            parse_f32(xyz[2], "BYU vertex")?,
        ]);
        at += 3;
    }
    let mut faces = Vec::new();
    for _ in 0..polygon_count {
        let mut polygon = Vec::new();
        loop {
            let token = tokens
                .get(at)
                .ok_or_else(|| Error::parse("BYU: truncated polygon list"))?;
            at += 1;
            let value = token
                .parse::<i64>()
                .map_err(|_| Error::parse("BYU: invalid polygon index"))?;
            let absolute = value.unsigned_abs();
            if absolute == 0 || absolute as usize > vertex_count {
                return Err(Error::invalid("BYU polygon index is out of range"));
            }
            polygon.push((absolute - 1) as u32);
            if value < 0 {
                break;
            }
        }
        triangulate(&polygon, &mut faces, "BYU polygon")?;
    }
    finish_surface(vertices, faces, "BYU")
}

fn parse_open_dx(text: &str) -> Result<Surface> {
    let lines = text.lines().map(str::trim).collect::<Vec<_>>();
    let mut vertices = Vec::new();
    let mut faces = Vec::new();
    let mut index = 0;
    while index < lines.len() {
        let lower = lines[index].to_ascii_lowercase();
        if lower.contains("class array") && lower.contains("rank 1") && lower.contains("shape 3") {
            let count = header_items(&lower, "OpenDX")?;
            let is_connections = lower.contains("type int");
            index += 1;
            for _ in 0..count {
                let fields = lines
                    .get(index)
                    .ok_or_else(|| Error::parse("OpenDX: truncated array"))?
                    .split_whitespace()
                    .collect::<Vec<_>>();
                if fields.len() < 3 {
                    return Err(Error::parse("OpenDX: short rank-1 record"));
                }
                if is_connections {
                    faces.push([
                        parse_u32(fields[0], "OpenDX connection")?,
                        parse_u32(fields[1], "OpenDX connection")?,
                        parse_u32(fields[2], "OpenDX connection")?,
                    ]);
                } else {
                    vertices.push([
                        parse_f32(fields[0], "OpenDX position")?,
                        parse_f32(fields[1], "OpenDX position")?,
                        parse_f32(fields[2], "OpenDX position")?,
                    ]);
                }
                index += 1;
            }
            continue;
        }
        index += 1;
    }
    finish_surface(vertices, faces, "OpenDX")
}

fn parse_open_inventor(text: &str) -> Result<Surface> {
    let points = bracket_body_after(text, "point")?;
    let indices = bracket_body_after(text, "coordIndex")?;
    let point_values = numeric_tokens(points)
        .map(|value| parse_f32(value, "Inventor point"))
        .collect::<Result<Vec<_>>>()?;
    if point_values.len() % 3 != 0 {
        return Err(Error::parse(
            "Inventor: point array is not divisible by three",
        ));
    }
    let vertices = point_values
        .chunks_exact(3)
        .map(|value| [value[0], value[1], value[2]])
        .collect::<Vec<_>>();
    let mut faces = Vec::new();
    let mut polygon = Vec::new();
    for token in numeric_tokens(indices) {
        let value = token
            .parse::<i64>()
            .map_err(|_| Error::parse("Inventor: invalid coordIndex"))?;
        if value == -1 {
            triangulate(&polygon, &mut faces, "Inventor face")?;
            polygon.clear();
        } else if value < 0 {
            return Err(Error::invalid("Inventor coordIndex below -1"));
        } else {
            polygon.push(value as u32);
        }
    }
    if !polygon.is_empty() {
        triangulate(&polygon, &mut faces, "Inventor face")?;
    }
    finish_surface(vertices, faces, "Open Inventor")
}

fn bracket_body_after<'a>(text: &'a str, marker: &str) -> Result<&'a str> {
    let marker_at = text
        .find(marker)
        .ok_or_else(|| Error::missing(format!("Inventor {marker} array")))?;
    let rest = &text[marker_at + marker.len()..];
    let open = rest
        .find('[')
        .ok_or_else(|| Error::parse(format!("Inventor {marker}: missing [")))?;
    let close = rest[open + 1..]
        .find(']')
        .ok_or_else(|| Error::parse(format!("Inventor {marker}: missing ]")))?;
    Ok(&rest[open + 1..open + 1 + close])
}

fn numeric_tokens(text: &str) -> impl Iterator<Item = &str> {
    text.split(|character: char| character.is_whitespace() || character == ',')
        .filter(|value| !value.is_empty())
}

fn header_items(line: &str, context: &str) -> Result<usize> {
    let fields = line.split_whitespace().collect::<Vec<_>>();
    let at = fields
        .iter()
        .position(|field| *field == "items")
        .ok_or_else(|| Error::missing(format!("{context} items count")))?;
    parse_usize(fields.get(at + 1), &format!("{context} items count"))
}

fn triangulate(polygon: &[u32], faces: &mut Vec<[u32; 3]>, context: &str) -> Result<()> {
    if polygon.len() < 3 {
        return Err(Error::invalid(format!(
            "{context} has fewer than three vertices"
        )));
    }
    for index in 1..polygon.len() - 1 {
        faces.push([polygon[0], polygon[index], polygon[index + 1]]);
    }
    Ok(())
}

fn finish_surface(vertices: Vec<[f32; 3]>, faces: Vec<[u32; 3]>, name: &str) -> Result<Surface> {
    if vertices.is_empty() {
        return Err(Error::missing(format!("{name} vertices")));
    }
    if faces.is_empty() {
        return Err(Error::missing(format!("{name} faces")));
    }
    for face in &faces {
        if face.iter().any(|index| *index as usize >= vertices.len()) {
            return Err(Error::invalid(format!(
                "{name} face references a vertex outside 0..{}",
                vertices.len()
            )));
        }
    }
    Ok(Surface::from_geometry(vertices, faces))
}

fn xyz_slot(name: &str) -> Option<usize> {
    match name {
        "x" => Some(0),
        "y" => Some(1),
        "z" => Some(2),
        _ => None,
    }
}

fn required_xyz(values: [Option<f32>; 3], context: &str) -> Result<[f32; 3]> {
    Ok([
        values[0].ok_or_else(|| Error::missing(format!("{context} vertex x property")))?,
        values[1].ok_or_else(|| Error::missing(format!("{context} vertex y property")))?,
        values[2].ok_or_else(|| Error::missing(format!("{context} vertex z property")))?,
    ])
}

fn intern_vertex(
    vertex: [f32; 3],
    vertices: &mut Vec<[f32; 3]>,
    known: &mut HashMap<[u32; 3], u32>,
) -> u32 {
    let key = vertex.map(f32::to_bits);
    if let Some(index) = known.get(&key) {
        *index
    } else {
        let index = vertices.len() as u32;
        vertices.push(vertex);
        known.insert(key, index);
        index
    }
}

fn read_le_f32(bytes: &[u8], at: usize) -> Result<f32> {
    let raw = bytes
        .get(at..at + 4)
        .ok_or_else(|| Error::parse("STL: truncated binary triangle"))?;
    Ok(f32::from_le_bytes(raw.try_into().unwrap()))
}

fn finite_f32(value: f64, context: &str) -> Result<f32> {
    let converted = value as f32;
    if value.is_finite() && converted.is_finite() {
        Ok(converted)
    } else {
        Err(Error::invalid(format!("{context} is not finite f32")))
    }
}

fn index_value(value: f64, count: usize) -> Result<u32> {
    if value < 0.0 || value.fract() != 0.0 || value as usize >= count {
        Err(Error::invalid(format!(
            "surface index {value} is outside 0..{count}"
        )))
    } else {
        Ok(value as u32)
    }
}

fn parse_f32(value: &str, context: &str) -> Result<f32> {
    let value = value
        .parse::<f64>()
        .map_err(|_| Error::parse(format!("{context}: invalid number {value:?}")))?;
    finite_f32(value, context)
}

fn parse_u32(value: &str, context: &str) -> Result<u32> {
    value
        .parse()
        .map_err(|_| Error::parse(format!("{context}: invalid index {value:?}")))
}

fn parse_usize(value: Option<&&str>, context: &str) -> Result<usize> {
    value
        .ok_or_else(|| Error::missing(context.to_string()))?
        .parse()
        .map_err(|_| Error::parse(format!("{context}: invalid integer")))
}

fn parse_token_usize(value: Option<&&str>, context: &str) -> Result<usize> {
    parse_usize(value, context)
}

fn parse_token_f64(value: Option<&&str>, context: &str) -> Result<f64> {
    let value = value.ok_or_else(|| Error::missing(context.to_string()))?;
    let parsed = value
        .parse::<f64>()
        .map_err(|_| Error::parse(format!("{context}: invalid number")))?;
    if parsed.is_finite() {
        Ok(parsed)
    } else {
        Err(Error::invalid(format!("{context} is not finite")))
    }
}

fn endian_u16(bytes: &[u8], little: bool) -> u16 {
    let raw: [u8; 2] = bytes.try_into().unwrap();
    if little {
        u16::from_le_bytes(raw)
    } else {
        u16::from_be_bytes(raw)
    }
}

fn endian_i16(bytes: &[u8], little: bool) -> i16 {
    endian_u16(bytes, little) as i16
}

fn endian_u32(bytes: &[u8], little: bool) -> u32 {
    let raw: [u8; 4] = bytes.try_into().unwrap();
    if little {
        u32::from_le_bytes(raw)
    } else {
        u32::from_be_bytes(raw)
    }
}

fn endian_i32(bytes: &[u8], little: bool) -> i32 {
    endian_u32(bytes, little) as i32
}

fn endian_u64(bytes: &[u8], little: bool) -> u64 {
    let raw: [u8; 8] = bytes.try_into().unwrap();
    if little {
        u64::from_le_bytes(raw)
    } else {
        u64::from_be_bytes(raw)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_obj_polygons_and_negative_indices() {
        let mesh = parse_obj("v 0 0 0\nv 1 0 0\nv 1 1 0\nv 0 1 0\nf -4 -3 -2 -1\n").unwrap();
        assert_eq!(mesh.vertices.len(), 4);
        assert_eq!(mesh.faces, vec![[0, 1, 2], [0, 2, 3]]);
    }

    #[test]
    fn parses_ascii_ply_with_extra_vertex_property() {
        let ply = b"ply\nformat ascii 1.0\nelement vertex 3\nproperty float x\nproperty float y\nproperty float z\nproperty uchar red\nelement face 1\nproperty list uchar int vertex_indices\nend_header\n0 0 0 255\n1 0 0 255\n0 1 0 255\n3 0 1 2\n";
        let mesh = parse_ply(ply).unwrap();
        assert_eq!(mesh.vertices.len(), 3);
        assert_eq!(mesh.faces, vec![[0, 1, 2]]);
    }

    #[test]
    fn parses_binary_little_endian_ply() {
        let mut ply = b"ply\nformat binary_little_endian 1.0\nelement vertex 3\nproperty float x\nproperty float y\nproperty float z\nelement face 1\nproperty list uchar int vertex_indices\nend_header\n".to_vec();
        for value in [0.0_f32, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0] {
            ply.extend(value.to_le_bytes());
        }
        ply.push(3);
        for value in [0_i32, 1, 2] {
            ply.extend(value.to_le_bytes());
        }
        let mesh = parse_ply(&ply).unwrap();
        assert_eq!(mesh.vertices.len(), 3);
        assert_eq!(mesh.faces, vec![[0, 1, 2]]);
    }

    #[test]
    fn parses_ascii_stl_and_deduplicates_vertices() {
        let stl = "solid x\nfacet normal 0 0 1\nouter loop\nvertex 0 0 0\nvertex 1 0 0\nvertex 0 1 0\nendloop\nendfacet\nendsolid\n";
        let mesh = parse_ascii_stl(stl).unwrap();
        assert_eq!(mesh.vertices.len(), 3);
        assert_eq!(mesh.faces, vec![[0, 1, 2]]);
    }

    #[test]
    fn parses_binary_stl() {
        let mut stl = vec![0_u8; 80];
        stl.extend(1_u32.to_le_bytes());
        for value in [
            0.0_f32, 0.0, 1.0, // normal
            0.0, 0.0, 0.0, // first vertex
            1.0, 0.0, 0.0, // second vertex
            0.0, 1.0, 0.0, // third vertex
        ] {
            stl.extend(value.to_le_bytes());
        }
        stl.extend(0_u16.to_le_bytes());
        let mesh = parse_stl(&stl).unwrap();
        assert_eq!(mesh.vertices.len(), 3);
        assert_eq!(mesh.faces, vec![[0, 1, 2]]);
    }

    #[test]
    fn parses_byu_polygons() {
        let mesh = parse_byu("1 4 1 4\n1 1\n0 0 0  1 0 0  1 1 0  0 1 0\n1 2 3 -4\n").unwrap();
        assert_eq!(mesh.faces, vec![[0, 1, 2], [0, 2, 3]]);
    }

    #[test]
    fn parses_open_dx_mesh() {
        let dx = "object 1 class array type float rank 1 shape 3 items 3 data follows\n0 0 0\n1 0 0\n0 1 0\nobject 2 class array type int rank 1 shape 3 items 1 data follows\n0 1 2\n";
        let mesh = parse_open_dx(dx).unwrap();
        assert_eq!(mesh.vertices.len(), 3);
        assert_eq!(mesh.faces, vec![[0, 1, 2]]);
    }

    #[test]
    fn parses_open_inventor_mesh() {
        let iv = "#Inventor V2.1 ascii\nCoordinate3 { point [ 0 0 0, 1 0 0, 0 1 0 ] }\nIndexedFaceSet { coordIndex [ 0, 1, 2, -1 ] }\n";
        let mesh = parse_open_inventor(iv).unwrap();
        assert_eq!(mesh.vertices.len(), 3);
        assert_eq!(mesh.faces, vec![[0, 1, 2]]);
    }

    #[test]
    fn parses_mni_obj_mesh() {
        let mni = "P 0.3 0.3 0.4 10 1 3\n0 0 0  1 0 0  0 1 0\n0 0 1  0 0 1  0 0 1\n1\n0 1 1 1 1\n0\n0 1 2\n";
        let mesh = parse_mni_obj(mni).unwrap();
        assert_eq!(mesh.vertices.len(), 3);
        assert_eq!(mesh.faces, vec![[0, 1, 2]]);
    }

    #[test]
    fn parses_brainvoyager_little_endian_mesh() {
        let mut bytes = Vec::new();
        bytes.extend(4.0_f32.to_le_bytes());
        bytes.extend(0_i32.to_le_bytes());
        bytes.extend(3_i32.to_le_bytes());
        bytes.extend(1_i32.to_le_bytes());
        for value in [0.0_f32, 0.0, 0.0] {
            bytes.extend(value.to_le_bytes());
        }
        for value in [0.0_f32, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0] {
            bytes.extend(value.to_le_bytes());
        }
        bytes.extend(vec![0_u8; 3 * 3 * 4]);
        bytes.extend(vec![0_u8; 8 * 4]);
        for _ in 0..3 {
            bytes.extend(0_i32.to_le_bytes()); // visible colour
        }
        for _ in 0..3 {
            bytes.extend(0_i32.to_le_bytes()); // no neighbours
        }
        for value in [0_u32, 1, 2] {
            bytes.extend(value.to_le_bytes());
        }
        let mesh = parse_brainvoyager(&bytes).unwrap();
        assert_eq!(
            mesh.vertices,
            vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]]
        );
        assert_eq!(mesh.faces, vec![[0, 1, 2]]);
    }

    #[test]
    fn parses_surefit_tag_version_pair() {
        let coordinates = "BeginHeader\nencoding ASCII\nEndHeader\n3\n0 0 0 0\n1 1 0 0\n2 0 1 0\n";
        let topology = "BeginHeader\nencoding ASCII\nEndHeader\ntag-version 1 1\n0 1 2\n";
        let mesh = parse_surefit_pair(coordinates, topology).unwrap();
        assert_eq!(mesh.vertices.len(), 3);
        assert_eq!(mesh.faces, vec![[0, 1, 2]]);
    }
}
