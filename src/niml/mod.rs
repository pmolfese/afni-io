//! The NIML element tree: AFNI's XML-like container format.
//!
//! NIML ("NeuroImaging Markup Language") is the substrate for SUMA surface
//! datasets, drawn ROIs, and many AFNI inter-process messages. A NIML stream
//! is a sequence of *elements*. Each element has a tag name, a set of quoted
//! attributes, and a body that is either:
//!
//! * another list of elements (a *group*, marked `ni_form="ni_group"`),
//! * a numeric matrix (`ni_type` lists the per-column types),
//! * a string, or
//! * raw binary (`ni_form="binary*"`).
//!
//! This module parses both the ASCII and binary encodings into [`NimlElement`]
//! trees and serialises trees back to ASCII. Higher-level modules
//! ([`crate::dset`], [`crate::roi`]) interpret specific tag layouts.
//!
//! References: `afni/src/niml/`, `afni/src/matlab/afni_niml_*.m`.

use std::collections::BTreeMap;
use std::path::Path;

use crate::error::{self, Error, Result};

mod parse;
mod write;

pub use parse::parse_bytes;
pub use write::serialize;

/// A single NIML element: a tag, its attributes, and its body.
#[derive(Debug, Clone, PartialEq)]
pub struct NimlElement {
    /// The tag name, e.g. `AFNI_dataset` or `SPARSE_DATA`.
    pub name: String,
    /// Attributes in the element header, keyed by name. Ordered for stable
    /// output; AFNI does not depend on attribute order.
    pub attrs: BTreeMap<String, String>,
    /// The element body.
    pub data: NimlData,
}

/// The body of a [`NimlElement`].
#[derive(Debug, Clone, PartialEq)]
pub enum NimlData {
    /// No body (a self-closing or empty element).
    None,
    /// Free text (a single `String`/`CString` column, or untyped text).
    Text(String),
    /// A homogeneous numeric matrix (`rows` x columns of `f64`).
    Numeric(NumericMatrix),
    /// A heterogeneous table mixing text and numeric columns.
    Mixed(MixedTable),
    /// Child elements (`ni_form="ni_group"`).
    Group(Vec<NimlElement>),
}

/// A row-major numeric matrix decoded from a NIML element body.
///
/// All values are widened to `f64` regardless of their declared
/// [`NimlValueType`]; the original per-column types are kept in
/// [`column_types`](NumericMatrix::column_types) so writers can round-trip them.
#[derive(Debug, Clone, PartialEq)]
pub struct NumericMatrix {
    /// The declared type of each column.
    pub column_types: Vec<NimlValueType>,
    /// Number of rows.
    pub rows: usize,
    /// `rows * column_types.len()` values, stored row-major.
    pub values: Vec<f64>,
}

/// A table with a mix of numeric and string columns.
#[derive(Debug, Clone, PartialEq)]
pub struct MixedTable {
    /// The declared type of each column.
    pub column_types: Vec<NimlValueType>,
    /// Number of rows.
    pub rows: usize,
    /// `rows * column_types.len()` values, stored row-major.
    pub values: Vec<NimlValue>,
}

/// A single cell of a [`MixedTable`].
#[derive(Debug, Clone, PartialEq)]
pub enum NimlValue {
    /// An integer-typed cell.
    Integer(i64),
    /// A floating-point cell.
    Float(f64),
    /// A text cell.
    Text(String),
}

/// The element column types AFNI understands (`ni_type`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NimlValueType {
    /// 8-bit unsigned (`byte`).
    UInt8,
    /// 16-bit signed (`short`).
    Int16,
    /// 32-bit signed (`int`).
    Int32,
    /// 32-bit IEEE float (`float`).
    Float32,
    /// 64-bit IEEE float (`double`).
    Float64,
    /// A whitespace- or quote-delimited string (`String`).
    String,
    /// A NUL-delimited C string (`CString`).
    CString,
    /// A variable-length drawn-ROI datum record (`SUMA_NIML_ROI_DATUM`).
    SumaRoiDatum,
    /// Any other type name, preserved verbatim.
    Other(String),
}

impl NimlValueType {
    /// Map a NIML type name (case-insensitive) onto a variant.
    pub fn from_name(name: &str) -> Self {
        match name.trim().to_ascii_lowercase().as_str() {
            "byte" | "uint8" => Self::UInt8,
            "short" | "int16" => Self::Int16,
            "int" | "int32" => Self::Int32,
            "float" | "float32" => Self::Float32,
            "double" | "float64" => Self::Float64,
            "string" => Self::String,
            "cstring" => Self::CString,
            "suma_niml_roi_datum" => Self::SumaRoiDatum,
            _ => Self::Other(name.trim().to_string()),
        }
    }

    /// The canonical name AFNI writes for this type.
    pub fn canonical_name(&self) -> &str {
        match self {
            Self::UInt8 => "byte",
            Self::Int16 => "short",
            Self::Int32 => "int",
            Self::Float32 => "float",
            Self::Float64 => "double",
            Self::String => "String",
            Self::CString => "CString",
            Self::SumaRoiDatum => "SUMA_NIML_ROI_DATUM",
            Self::Other(value) => value,
        }
    }

    /// Whether this type holds a fixed-width number.
    pub fn is_numeric(&self) -> bool {
        matches!(
            self,
            Self::UInt8 | Self::Int16 | Self::Int32 | Self::Float32 | Self::Float64
        )
    }

    /// Whether this type holds an integer.
    pub fn is_integer(&self) -> bool {
        matches!(self, Self::UInt8 | Self::Int16 | Self::Int32)
    }

    /// The fixed byte width of a numeric type, if it has one.
    pub fn byte_width(&self) -> Result<usize> {
        match self {
            Self::UInt8 => Ok(1),
            Self::Int16 => Ok(2),
            Self::Int32 | Self::Float32 => Ok(4),
            Self::Float64 => Ok(8),
            other => Err(Error::invalid(format!(
                "NIML type {} has no fixed byte width",
                other.canonical_name()
            ))),
        }
    }
}

impl NumericMatrix {
    /// Construct a matrix, validating that `values.len() == rows * columns`.
    pub fn new(column_types: Vec<NimlValueType>, rows: usize, values: Vec<f64>) -> Result<Self> {
        if column_types.is_empty() {
            return Err(Error::invalid("NIML numeric matrix has no columns"));
        }
        let expected = rows * column_types.len();
        if values.len() != expected {
            return Err(Error::invalid(format!(
                "NIML numeric matrix has {} values but expected {expected}",
                values.len()
            )));
        }
        Ok(Self {
            column_types,
            rows,
            values,
        })
    }

    /// Number of columns.
    pub fn columns(&self) -> usize {
        self.column_types.len()
    }

    /// Fetch the value at `(row, column)`, if in bounds.
    pub fn get(&self, row: usize, column: usize) -> Option<f64> {
        if row >= self.rows || column >= self.columns() {
            return None;
        }
        self.values.get(row * self.columns() + column).copied()
    }
}

impl MixedTable {
    /// Construct a table, validating that `values.len() == rows * columns`.
    pub fn new(
        column_types: Vec<NimlValueType>,
        rows: usize,
        values: Vec<NimlValue>,
    ) -> Result<Self> {
        if column_types.is_empty() {
            return Err(Error::invalid("NIML mixed table has no columns"));
        }
        let expected = rows * column_types.len();
        if values.len() != expected {
            return Err(Error::invalid(format!(
                "NIML mixed table has {} values but expected {expected}",
                values.len()
            )));
        }
        Ok(Self {
            column_types,
            rows,
            values,
        })
    }

    /// Number of columns.
    pub fn columns(&self) -> usize {
        self.column_types.len()
    }

    /// Fetch a cell, if in bounds.
    pub fn get(&self, row: usize, column: usize) -> Option<&NimlValue> {
        if row >= self.rows || column >= self.columns() {
            return None;
        }
        self.values.get(row * self.columns() + column)
    }
}

impl NimlElement {
    /// Build a group element wrapping `children`.
    pub fn group(
        name: impl Into<String>,
        attrs: BTreeMap<String, String>,
        children: Vec<NimlElement>,
    ) -> Self {
        Self {
            name: name.into(),
            attrs,
            data: NimlData::Group(children),
        }
    }

    /// Build a text element.
    pub fn text(
        name: impl Into<String>,
        attrs: BTreeMap<String, String>,
        text: impl Into<String>,
    ) -> Self {
        Self {
            name: name.into(),
            attrs,
            data: NimlData::Text(text.into()),
        }
    }

    /// Build a numeric-matrix element.
    pub fn numeric(
        name: impl Into<String>,
        attrs: BTreeMap<String, String>,
        matrix: NumericMatrix,
    ) -> Self {
        Self {
            name: name.into(),
            attrs,
            data: NimlData::Numeric(matrix),
        }
    }

    /// The first direct child with the given tag name, if any.
    pub fn child(&self, name: &str) -> Option<&NimlElement> {
        match &self.data {
            NimlData::Group(children) => children.iter().find(|c| c.name == name),
            _ => None,
        }
    }

    /// All direct children, or an empty slice for non-group elements.
    pub fn children(&self) -> &[NimlElement] {
        match &self.data {
            NimlData::Group(children) => children,
            _ => &[],
        }
    }
}

/// Expand a `ni_type` string into one [`NimlValueType`] per column.
///
/// Handles the comma-separated list and the `N*type` repeat syntax, e.g.
/// `"int,3*float,String"` becomes `[Int32, Float32, Float32, Float32, String]`.
pub fn expand_ni_type(ni_type: &str) -> Result<Vec<NimlValueType>> {
    let mut types = Vec::new();
    for piece in ni_type.split(',').map(str::trim).filter(|p| !p.is_empty()) {
        let (count, type_name) = match piece.split_once('*') {
            Some((count, type_name)) if count.trim().chars().all(|c| c.is_ascii_digit()) => {
                let count: usize = count
                    .trim()
                    .parse()
                    .map_err(|_| Error::parse(format!("invalid NIML repeat count in {piece:?}")))?;
                if count == 0 {
                    return Err(Error::invalid("NIML type repeat count must be positive"));
                }
                (count, type_name.trim())
            }
            _ => (1, piece),
        };
        let value_type = NimlValueType::from_name(type_name);
        types.extend(std::iter::repeat(value_type).take(count));
    }
    if types.is_empty() {
        return Err(Error::invalid("NIML ni_type is empty"));
    }
    Ok(types)
}

/// Render a slice of column types as a canonical `ni_type` string.
pub fn ni_type_string(types: &[NimlValueType]) -> String {
    types
        .iter()
        .map(NimlValueType::canonical_name)
        .collect::<Vec<_>>()
        .join(",")
}

/// Parse every top-level element from raw NIML bytes (ASCII or binary).
///
/// Files whose elements are commented out with a leading `#` on each line
/// (as SUMA writes `.niml.roi` files) are de-commented first.
pub fn parse(bytes: &[u8]) -> Result<Vec<NimlElement>> {
    if let Ok(text) = std::str::from_utf8(bytes) {
        if text
            .lines()
            .any(|line| line.trim_start().starts_with("# <"))
        {
            let cleaned = strip_comment_prefixes(text);
            return parse_bytes(cleaned.as_bytes());
        }
    }
    parse_bytes(bytes)
}

/// Parse NIML from a UTF-8 string.
pub fn parse_str(text: &str) -> Result<Vec<NimlElement>> {
    parse(text.as_bytes())
}

/// Read and parse a NIML file from disk.
pub fn read(path: impl AsRef<Path>) -> Result<Vec<NimlElement>> {
    let path = path.as_ref();
    parse(&error::read_file(path)?)
}

/// Serialise elements to ASCII and write them to disk.
pub fn write(path: impl AsRef<Path>, elements: &[NimlElement]) -> Result<()> {
    error::write_file(path.as_ref(), serialize(elements).as_bytes())
}

/// Remove a leading `# ` (or `#`) from each line.
///
/// SUMA writes `.niml.roi` files with the whole NIML element commented out;
/// stripping the prefixes recovers a normal NIML stream.
pub(crate) fn strip_comment_prefixes(text: &str) -> String {
    let mut cleaned = String::with_capacity(text.len());
    for line in text.lines() {
        let stripped = line.trim_start();
        if let Some(rest) = stripped.strip_prefix('#') {
            cleaned.push_str(rest.strip_prefix(' ').unwrap_or(rest));
        } else {
            cleaned.push_str(line);
        }
        cleaned.push('\n');
    }
    cleaned
}

/// Escape the five XML entities AFNI recognises.
pub(crate) fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('\'', "&apos;")
        .replace('"', "&quot;")
        .replace('>', "&gt;")
        .replace('<', "&lt;")
}

/// Reverse of [`escape`], in one pass, following `unescape_inplace` in
/// `niml/niml_util.c`: the five named entities plus the numeric forms
/// `&#ddd;` and `&#xhh;`. Anything else is copied unchanged.
pub(crate) fn unescape(text: &str) -> String {
    if !text.contains('&') {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        rest = &rest[amp..];
        let entity = rest.find(';').map(|end| (&rest[1..end], end));
        let decoded = entity.and_then(|(name, end)| {
            let ch = match name {
                "lt" => '<',
                "gt" => '>',
                "quot" => '"',
                "apos" => '\'',
                "amp" => '&',
                _ => {
                    let code = if let Some(hex) = name.strip_prefix("#x") {
                        u32::from_str_radix(hex, 16).ok()?
                    } else {
                        name.strip_prefix('#')?.parse().ok()?
                    };
                    char::from_u32(code)?
                }
            };
            Some((ch, end))
        });
        match decoded {
            Some((ch, end)) => {
                out.push(ch);
                rest = &rest[end + 1..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// Format a float as the shortest decimal that reads back to the same value,
/// keeping a `.0` on whole numbers.
///
/// AFNI floats are 32-bit, so a value that is exactly an `f32` is written
/// with `f32`'s shortest form (`0.3`, not `0.30000001192092896`); anything
/// else uses `f64`'s. (A fixed 10 decimals, used before, turned a scale
/// factor such as 3.0517578e-05 into 0.0000305176 and small p-values into 0.)
pub(crate) fn format_float(value: f64) -> String {
    let narrow = value as f32;
    let mut s = if f64::from(narrow) == value || !value.is_finite() {
        format!("{narrow}")
    } else {
        format!("{value}")
    };
    if value.is_finite() && !s.contains('.') {
        s.push_str(".0");
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ni_type_expands_repeat_syntax() {
        let types = expand_ni_type("int,3*float,String,SUMA_NIML_ROI_DATUM").unwrap();
        assert_eq!(
            types,
            vec![
                NimlValueType::Int32,
                NimlValueType::Float32,
                NimlValueType::Float32,
                NimlValueType::Float32,
                NimlValueType::String,
                NimlValueType::SumaRoiDatum,
            ]
        );
    }

    #[test]
    fn round_trips_a_numeric_group() {
        let text = r#"<AFNI_dataset ni_form="ni_group" dset_type="Node_Bucket" >
<SPARSE_DATA ni_type="float,float" ni_dimen="2" >
1.5 2.5
3.5 4.5
</SPARSE_DATA>
</AFNI_dataset>"#;
        let elements = parse_str(text).unwrap();
        assert_eq!(elements.len(), 1);
        let sparse = elements[0].child("SPARSE_DATA").unwrap();
        let NimlData::Numeric(matrix) = &sparse.data else {
            panic!("expected numeric");
        };
        assert_eq!(matrix.rows, 2);
        assert_eq!(matrix.get(1, 1), Some(4.5));

        let reparsed = parse_str(&serialize(&elements)).unwrap();
        assert_eq!(reparsed, elements);
    }

    #[test]
    fn valueless_attributes_round_trip_as_bare_names() {
        let text = "<AFNI_dataset ni_form=\"ni_group\" domain_parent_idcode label=\"x\" >\n</AFNI_dataset>";
        let elements = parse_str(text).unwrap();
        assert_eq!(elements[0].attrs["domain_parent_idcode"], "");
        let written = serialize(&elements);
        assert!(written.contains("\n  domain_parent_idcode\n"), "{written}");
        assert_eq!(parse_str(&written).unwrap(), elements);
    }

    #[test]
    fn escapes_round_trip() {
        let elements =
            parse_str(r#"<AFNI_atr atr_name="NOTE" >hello &lt;world&gt;</AFNI_atr>"#).unwrap();
        let NimlData::Text(text) = &elements[0].data else {
            panic!("expected text");
        };
        assert_eq!(text, "hello <world>");
    }

    #[test]
    fn floats_round_trip_exactly() {
        // f32 values come back exactly as f32 (how AFNI reads them) ...
        for v in [0.3f32, 3.0517578e-05, 1e-12, -2.5, 1e30] {
            let text = format_float(f64::from(v));
            assert_eq!(text.parse::<f32>().unwrap(), v, "{text}");
        }
        // ... and anything else exactly as f64, e.g. values parsed from text.
        for v in [0.1, 0.984808, 1e-300, 12.5] {
            assert_eq!(format_float(v).parse::<f64>().unwrap(), v);
        }
        assert_eq!(format_float(0.3f32 as f64), "0.3");
        assert_eq!(format_float(2.0), "2.0");
        assert_eq!(format_float(f64::NAN), "NaN");
    }

    #[test]
    fn unescape_handles_numeric_entities_in_one_pass() {
        assert_eq!(unescape("a&#x0a;b&#32;c"), "a\nb c");
        assert_eq!(unescape("&amp;lt; &lt; &bogus; & x"), "&lt; < &bogus; & x");
    }

    #[test]
    fn string_columns_are_split_before_unescaping_and_quoted_on_write() {
        // One quoted string holding an escaped quote, spaces and a newline.
        let text = "<AFNI_atr ni_type=\"String\" ni_dimen=\"1\" atr_name=\"HISTORY_NOTE\" >\n \"say &quot;hi&quot; to a&#x0a;b\"\n</AFNI_atr>";
        let elements = parse_str(text).unwrap();
        assert_eq!(
            elements[0].data,
            NimlData::Text("say \"hi\" to a\nb".into())
        );
        let written = serialize(&elements);
        assert!(
            written.contains("\"say &quot;hi&quot; to a\nb\""),
            "{written}"
        );
        assert_eq!(parse_str(&written).unwrap(), elements);

        // Several strings in one column (AFNI splits long attributes this way).
        let text =
            "<AFNI_atr ni_type=\"String\" ni_dimen=\"2\" >\"part one \" 'part two'</AFNI_atr>";
        let NimlData::Mixed(table) = &parse_str(text).unwrap()[0].data else {
            panic!("expected one row per string");
        };
        assert_eq!(
            table.values,
            [
                NimlValue::Text("part one ".into()),
                NimlValue::Text("part two".into())
            ]
        );
    }
}
