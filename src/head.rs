//! AFNI volume headers: the `.HEAD` attribute file.
//!
//! The `.HEAD` is the attribute half of an AFNI volume dataset; its voxel data
//! lives in the companion `.BRIK`/`.BRIK.gz` (see [`crate::brik`]). This module
//! parses the attributes on their own — useful for inspecting a dataset's
//! geometry or metadata without loading the voxels.
//!
//! A `.HEAD` file is a flat list of attributes. Each attribute is introduced by
//! three header lines and followed by its values:
//!
//! ```text
//! type = integer-attribute
//! name = ORIENT_SPECIFIC
//! count = 3
//!  3 5 1
//! ```
//!
//! `type` is one of `integer-attribute`, `float-attribute`, or
//! `string-attribute`. Integer and float values are whitespace separated.
//! String values appear after a single opening `'` on the following line; NUL
//! bytes are stored on disk as `~` and restored on read.
//!
//! This module reads and writes the attribute list and provides typed accessors
//! for the mandatory geometry attributes needed to interpret a `.BRIK`.
//!
//! Reference: `afni/src/matlab/README.attributes`, `BrikInfo.m`,
//! `WriteBrikHEAD.m`.

use std::path::Path;

use crate::error::{self, from_utf8, Error, Result};

/// The value array of a single header [`Attribute`].
#[derive(Debug, Clone, PartialEq)]
pub enum AttributeValue {
    /// `integer-attribute`.
    Int(Vec<i64>),
    /// `float-attribute`.
    Float(Vec<f64>),
    /// `string-attribute`.
    String(String),
}

impl AttributeValue {
    /// The AFNI `type =` keyword for this value.
    pub fn type_keyword(&self) -> &'static str {
        match self {
            AttributeValue::Int(_) => "integer-attribute",
            AttributeValue::Float(_) => "float-attribute",
            AttributeValue::String(_) => "string-attribute",
        }
    }

    /// Number of entries (`count =`). For strings, the character count.
    pub fn count(&self) -> usize {
        match self {
            AttributeValue::Int(v) => v.len(),
            AttributeValue::Float(v) => v.len(),
            AttributeValue::String(s) => s.len(),
        }
    }
}

/// A named header attribute.
#[derive(Debug, Clone, PartialEq)]
pub struct Attribute {
    /// The attribute name (conventionally upper-case, no blanks).
    pub name: String,
    /// The attribute's value array.
    pub value: AttributeValue,
}

/// A parsed AFNI `.HEAD` file: an ordered list of attributes.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Header {
    /// Attributes in file order.
    pub attributes: Vec<Attribute>,
}

impl Header {
    /// Read and parse a `.HEAD` file.
    pub fn read(path: impl AsRef<Path>) -> Result<Self> {
        let bytes = error::read_file(path.as_ref())?;
        Self::parse(&from_utf8(&bytes, "HEAD file")?)
    }

    /// Parse a `.HEAD` file from text.
    pub fn parse(text: &str) -> Result<Self> {
        let mut attributes = Vec::new();
        let mut lines = text.lines().peekable();

        while let Some(line) = lines.next() {
            let line = line.trim();
            if line.is_empty() || !line.starts_with("type") {
                continue;
            }
            let type_kw = value_after_equals(line)
                .ok_or_else(|| Error::parse("HEAD: malformed 'type =' line"))?;

            let name_line = lines
                .next()
                .ok_or_else(|| Error::parse("HEAD: missing 'name =' line"))?;
            let name = value_after_equals(name_line.trim())
                .ok_or_else(|| Error::parse("HEAD: malformed 'name =' line"))?
                .to_string();

            let count_line = lines
                .next()
                .ok_or_else(|| Error::parse("HEAD: missing 'count =' line"))?;
            let count: usize = value_after_equals(count_line.trim())
                .ok_or_else(|| Error::parse("HEAD: malformed 'count =' line"))?
                .parse()
                .map_err(|_| Error::parse("HEAD: invalid count value"))?;

            let value = match type_kw {
                "string-attribute" => {
                    let mut s = String::new();
                    // The string body starts after a single opening quote and
                    // may span multiple physical lines until `count` chars are
                    // collected.
                    let mut first = true;
                    while s.len() < count {
                        let Some(raw) = lines.next() else { break };
                        let segment = if first {
                            first = false;
                            raw.strip_prefix('\'').unwrap_or(raw)
                        } else {
                            s.push('\n');
                            raw
                        };
                        s.push_str(segment);
                    }
                    s.truncate(count);
                    AttributeValue::String(decode_string(&s))
                }
                "integer-attribute" => {
                    AttributeValue::Int(read_numbers(&mut lines, count, parse_i64)?)
                }
                "float-attribute" => {
                    AttributeValue::Float(read_numbers(&mut lines, count, parse_f64)?)
                }
                other => {
                    return Err(Error::unsupported(format!("HEAD attribute type {other:?}")));
                }
            };

            attributes.push(Attribute { name, value });
        }

        Ok(Self { attributes })
    }

    /// Look up an attribute by name.
    pub fn get(&self, name: &str) -> Option<&AttributeValue> {
        self.attributes
            .iter()
            .find(|a| a.name == name)
            .map(|a| &a.value)
    }

    /// Integer values for `name`, if it exists and is an integer attribute.
    pub fn ints(&self, name: &str) -> Option<&[i64]> {
        match self.get(name)? {
            AttributeValue::Int(v) => Some(v),
            _ => None,
        }
    }

    /// Float values for `name`. Integer attributes are widened to float so that
    /// numeric geometry fields can be read uniformly.
    pub fn floats(&self, name: &str) -> Option<Vec<f64>> {
        match self.get(name)? {
            AttributeValue::Float(v) => Some(v.clone()),
            AttributeValue::Int(v) => Some(v.iter().map(|&x| x as f64).collect()),
            _ => None,
        }
    }

    /// The string value for `name`, if it is a string attribute.
    ///
    /// AFNI stores string attributes as NUL-terminated C strings (the `count`
    /// includes the terminator); the trailing NUL is trimmed here. The raw
    /// value, including any NUL, remains available via [`Header::get`].
    pub fn string(&self, name: &str) -> Option<&str> {
        match self.get(name)? {
            AttributeValue::String(s) => Some(s.trim_end_matches('\0')),
            _ => None,
        }
    }

    /// Insert or replace an attribute, preserving position on replace.
    pub fn set(&mut self, name: impl Into<String>, value: AttributeValue) {
        let name = name.into();
        if let Some(existing) = self.attributes.iter_mut().find(|a| a.name == name) {
            existing.value = value;
        } else {
            self.attributes.push(Attribute { name, value });
        }
    }

    // --- Typed geometry accessors (mandatory attributes) -------------------

    /// `DATASET_RANK`: `[spatial_dims, nvals]`. The number of sub-bricks is the
    /// second value.
    pub fn rank(&self) -> Option<[i64; 2]> {
        let v = self.ints("DATASET_RANK")?;
        Some([*v.first()?, *v.get(1)?])
    }

    /// Number of sub-bricks (`DATASET_RANK[1]`), defaulting to 1.
    pub fn nvals(&self) -> usize {
        self.rank()
            .map(|r| r[1].max(0) as usize)
            .unwrap_or(1)
            .max(1)
    }

    /// `DATASET_DIMENSIONS`: voxel counts `[nx, ny, nz]`.
    pub fn dimensions(&self) -> Option<[usize; 3]> {
        let v = self.ints("DATASET_DIMENSIONS")?;
        Some([
            (*v.first()?).max(0) as usize,
            (*v.get(1)?).max(0) as usize,
            (*v.get(2)?).max(0) as usize,
        ])
    }

    /// `DELTA`: voxel sizes in mm `[dx, dy, dz]`.
    pub fn delta(&self) -> Option<[f64; 3]> {
        let v = self.floats("DELTA")?;
        Some([*v.first()?, *v.get(1)?, *v.get(2)?])
    }

    /// `ORIGIN`: coordinate of voxel (0,0,0) `[x, y, z]`.
    pub fn origin(&self) -> Option<[f64; 3]> {
        let v = self.floats("ORIGIN")?;
        Some([*v.first()?, *v.get(1)?, *v.get(2)?])
    }

    /// `ORIENT_SPECIFIC` orientation codes for each axis.
    pub fn orientation(&self) -> Option<[i64; 3]> {
        let v = self.ints("ORIENT_SPECIFIC")?;
        Some([*v.first()?, *v.get(1)?, *v.get(2)?])
    }

    /// The dataset ID code (`IDCODE_STRING`).
    pub fn idcode(&self) -> Option<&str> {
        self.string("IDCODE_STRING")
    }

    /// `TYPESTRING`, e.g. `3DIM_HEAD_ANAT`.
    pub fn typestring(&self) -> Option<&str> {
        self.string("TYPESTRING")
    }

    /// Per-sub-brick storage type codes (`BRICK_TYPES`), defaulting to all
    /// shorts when the attribute is absent (the AFNI 1.0 default).
    pub fn brick_types(&self) -> Vec<i64> {
        self.ints("BRICK_TYPES")
            .map(|v| v.to_vec())
            .unwrap_or_else(|| vec![1; self.nvals()])
    }

    /// Per-sub-brick scale factors (`BRICK_FLOAT_FACS`); `0.0` means unscaled.
    pub fn brick_float_facs(&self) -> Vec<f64> {
        self.floats("BRICK_FLOAT_FACS")
            .unwrap_or_else(|| vec![0.0; self.nvals()])
    }

    /// Per-sub-brick labels (`BRICK_LABS`, `~`-separated on disk).
    pub fn brick_labels(&self) -> Vec<String> {
        self.string("BRICK_LABS")
            .map(|s| {
                s.split('~')
                    .map(str::to_string)
                    .filter(|p| !p.is_empty())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Whether the `.BRIK` data is little-endian, from `BYTEORDER_STRING`.
    /// Defaults to the host byte order when the attribute is absent.
    pub fn brik_is_little_endian(&self) -> bool {
        match self.string("BYTEORDER_STRING") {
            Some(s) if s.eq_ignore_ascii_case("LSB_FIRST") => true,
            Some(s) if s.eq_ignore_ascii_case("MSB_FIRST") => false,
            _ => cfg!(target_endian = "little"),
        }
    }

    /// Serialise to `.HEAD` text.
    pub fn to_head_string(&self) -> String {
        let mut out = String::new();
        for attr in &self.attributes {
            out.push_str("\ntype = ");
            out.push_str(attr.value.type_keyword());
            out.push_str("\nname = ");
            out.push_str(&attr.name);
            out.push_str("\ncount = ");
            out.push_str(&attr.value.count().to_string());
            out.push('\n');
            write_value(&attr.value, &mut out);
            out.push('\n');
        }
        out
    }

    /// Write the header to a `.HEAD` file.
    pub fn write(&self, path: impl AsRef<Path>) -> Result<()> {
        error::write_file(path.as_ref(), self.to_head_string().as_bytes())
    }
}

fn write_value(value: &AttributeValue, out: &mut String) {
    match value {
        AttributeValue::Int(v) => write_numbers(v.iter().map(|x| x.to_string()), out),
        AttributeValue::Float(v) => {
            write_numbers(v.iter().map(|x| crate::niml::format_float(*x)), out)
        }
        AttributeValue::String(s) => {
            out.push('\'');
            out.push_str(&encode_string(s));
            out.push('~');
        }
    }
}

/// Emit numbers five per line, matching AFNI's own output style.
fn write_numbers(items: impl Iterator<Item = String>, out: &mut String) {
    for (i, item) in items.enumerate() {
        if i > 0 {
            out.push(if i % 5 == 0 { '\n' } else { ' ' });
        }
        out.push_str(&item);
    }
}

fn read_numbers<T>(
    lines: &mut std::iter::Peekable<std::str::Lines<'_>>,
    count: usize,
    parse: fn(&str) -> Result<T>,
) -> Result<Vec<T>> {
    let mut values = Vec::with_capacity(count);
    // Values follow the `count =` line, possibly wrapped across several lines.
    while values.len() < count {
        let Some(line) = lines.next() else { break };
        for token in line.split_whitespace() {
            values.push(parse(token)?);
            if values.len() == count {
                break;
            }
        }
    }
    if values.len() != count {
        return Err(Error::parse(format!(
            "HEAD: expected {count} values but found {}",
            values.len()
        )));
    }
    Ok(values)
}

fn parse_i64(token: &str) -> Result<i64> {
    token
        .parse::<i64>()
        // Some integer attributes are written with a trailing `.0`.
        .or_else(|_| token.parse::<f64>().map(|f| f as i64))
        .map_err(|_| Error::parse(format!("HEAD: invalid integer {token:?}")))
}

fn parse_f64(token: &str) -> Result<f64> {
    token
        .parse::<f64>()
        .map_err(|_| Error::parse(format!("HEAD: invalid float {token:?}")))
}

fn value_after_equals(line: &str) -> Option<&str> {
    line.split_once('=').map(|(_, rest)| rest.trim())
}

/// On disk, NUL is stored as `~`; restore real NULs on read.
fn decode_string(s: &str) -> String {
    s.replace('~', "\0")
}

/// On write, real NULs become `~` and literal `~` becomes `*` (AFNI behaviour).
fn encode_string(s: &str) -> String {
    s.replace('~', "*").replace('\0', "~")
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "\ntype = integer-attribute\nname = DATASET_RANK\ncount = 2\n 3 6\n\ntype = integer-attribute\nname = DATASET_DIMENSIONS\ncount = 3\n 64 64 30\n\ntype = float-attribute\nname = DELTA\ncount = 3\n 3.0 3.0 4.0\n\ntype = float-attribute\nname = ORIGIN\ncount = 3\n -90.0 -126.0 -72.0\n\ntype = string-attribute\nname = TYPESTRING\ncount = 15\n'3DIM_HEAD_ANAT~\n\ntype = integer-attribute\nname = BRICK_TYPES\ncount = 6\n1 1 1 1 1 1\n";

    #[test]
    fn parses_geometry() {
        let head = Header::parse(SAMPLE).unwrap();
        assert_eq!(head.rank(), Some([3, 6]));
        assert_eq!(head.nvals(), 6);
        assert_eq!(head.dimensions(), Some([64, 64, 30]));
        assert_eq!(head.delta(), Some([3.0, 3.0, 4.0]));
        assert_eq!(head.origin(), Some([-90.0, -126.0, -72.0]));
        assert_eq!(head.typestring(), Some("3DIM_HEAD_ANAT"));
        assert_eq!(head.brick_types(), vec![1, 1, 1, 1, 1, 1]);
    }

    #[test]
    fn round_trips() {
        let head = Header::parse(SAMPLE).unwrap();
        let reparsed = Header::parse(&head.to_head_string()).unwrap();
        assert_eq!(head, reparsed);
    }
}
