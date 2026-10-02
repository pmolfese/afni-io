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

use crate::array::{DataType, TypedArray};
use crate::error::{self, Error, Result};

mod parse;
mod write;

pub use parse::{parse_bytes, parse_stream};
pub use write::{serialize, serialize_binary};

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
    /// A numeric matrix, one typed column per `ni_type` entry.
    Numeric(NumericMatrix),
    /// A heterogeneous table mixing text and numeric columns.
    Mixed(MixedTable),
    /// Variable-length records of a NIML rowtype such as
    /// `SUMA_NIML_ROI_DATUM` or `TAYLOR_TRACT_DATUM`.
    Records(RecordTable),
    /// Child elements (`ni_form="ni_group"`).
    Group(Vec<NimlElement>),
}

/// A numeric matrix decoded from a NIML element body, stored column by
/// column in each column's declared type (`byte`, `short`, `int`, `float`,
/// `double`). A `float` dataset therefore takes half the memory it would as
/// `f64`, and a column can be handed on without copying.
#[derive(Debug, Clone, PartialEq)]
pub struct NumericMatrix {
    /// Number of rows.
    pub rows: usize,
    /// One array of `rows` values per column.
    pub columns: Vec<TypedArray>,
}

/// One field of a NIML rowtype: a number, or a variable-length array of
/// numbers whose length is stored in an earlier field of the same record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RecordField {
    /// The numeric type of the field's values.
    pub ty: DataType,
    /// `None` for a single value. `Some(k)` for an array whose length is the
    /// value of field `k` (0-based) of the same record: AFNI's `type[#k+1]`.
    pub length_from: Option<usize>,
}

/// Records of a NIML rowtype with variable-length fields, such as drawn-ROI
/// strokes (`SUMA_NIML_ROI_DATUM` = `int,int,int,int[#3]`) or tracts
/// (`TAYLOR_TRACT_DATUM` = `int,int,float[#2]`).
#[derive(Debug, Clone, PartialEq)]
pub struct RecordTable {
    /// The rowtype, as named in `ni_type`.
    pub record_type: NimlValueType,
    /// `rows[r][f]` holds field `f` of record `r`: one value for a scalar
    /// field, the whole array for a variable-length one. Values are `f64`,
    /// which holds every `int` and `float` exactly.
    ///
    /// When written, a length field is set from its array, so it never needs
    /// to be kept in step by hand.
    pub rows: Vec<Vec<Vec<f64>>>,
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
    /// A variable-length tract record (`TAYLOR_TRACT_DATUM`, AFNI/FATCAT).
    TaylorTractDatum,
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
            "taylor_tract_datum" => Self::TaylorTractDatum,
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
            Self::TaylorTractDatum => "TAYLOR_TRACT_DATUM",
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

    /// The array type that stores this NIML type's values, for the numeric
    /// types.
    pub fn data_type(&self) -> Option<DataType> {
        Some(match self {
            Self::UInt8 => DataType::UInt8,
            Self::Int16 => DataType::Int16,
            Self::Int32 => DataType::Int32,
            Self::Float32 => DataType::Float32,
            Self::Float64 => DataType::Float64,
            _ => return None,
        })
    }

    /// The NIML type for an array type, if NIML has one (`byte`, `short`,
    /// `int`, `float`, `double`).
    pub fn from_data_type(dtype: DataType) -> Option<Self> {
        Some(match dtype {
            DataType::UInt8 => Self::UInt8,
            DataType::Int16 => Self::Int16,
            DataType::Int32 => Self::Int32,
            DataType::Float32 => Self::Float32,
            DataType::Float64 => Self::Float64,
            _ => return None,
        })
    }

    /// The fields of a variable-length rowtype, for the ones AFNI defines
    /// with `NI_rowtype_define`: `SUMA_NIML_ROI_DATUM` is
    /// `int,int,int,int[#3]` (`SUMA_niml.c`) and `TAYLOR_TRACT_DATUM` is
    /// `int,int,float[#2]` (`ptaylor/TrackIO.h`).
    pub fn record_fields(&self) -> Option<Vec<RecordField>> {
        let scalar = |ty| RecordField {
            ty,
            length_from: None,
        };
        Some(match self {
            Self::SumaRoiDatum => vec![
                scalar(DataType::Int32),
                scalar(DataType::Int32),
                scalar(DataType::Int32),
                RecordField {
                    ty: DataType::Int32,
                    length_from: Some(2),
                },
            ],
            Self::TaylorTractDatum => vec![
                scalar(DataType::Int32),
                scalar(DataType::Int32),
                RecordField {
                    ty: DataType::Float32,
                    length_from: Some(1),
                },
            ],
            _ => return None,
        })
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
    /// Build a matrix from row-major `f64` values, storing each column in its
    /// declared type (values are cast, as AFNI casts on input).
    pub fn new(column_types: Vec<NimlValueType>, rows: usize, values: Vec<f64>) -> Result<Self> {
        if column_types.is_empty() {
            return Err(Error::invalid("NIML numeric matrix has no columns"));
        }
        let ncols = column_types.len();
        if values.len() != rows * ncols {
            return Err(Error::invalid(format!(
                "NIML numeric matrix has {} values but expected {}",
                values.len(),
                rows * ncols
            )));
        }
        let columns = column_types
            .iter()
            .enumerate()
            .map(|(c, ty)| {
                let dtype = ty.data_type().ok_or_else(|| {
                    Error::invalid(format!("NIML type {} is not numeric", ty.canonical_name()))
                })?;
                Ok(typed_from_f64(
                    dtype,
                    (0..rows).map(|r| values[r * ncols + c]),
                ))
            })
            .collect::<Result<_>>()?;
        Ok(Self { rows, columns })
    }

    /// Build a matrix from columns of equal length, each of a type NIML can
    /// store (`byte`, `short`, `int`, `float`, `double`).
    pub fn from_columns(columns: Vec<TypedArray>) -> Result<Self> {
        let rows = columns
            .first()
            .ok_or_else(|| Error::invalid("NIML numeric matrix has no columns"))?
            .len();
        for (c, column) in columns.iter().enumerate() {
            if column.len() != rows {
                return Err(Error::invalid(format!(
                    "NIML column {c} has {} values but column 0 has {rows}",
                    column.len()
                )));
            }
            if NimlValueType::from_data_type(column.dtype()).is_none() {
                return Err(Error::invalid(format!(
                    "NIML has no type for {:?} (column {c})",
                    column.dtype()
                )));
            }
        }
        Ok(Self { rows, columns })
    }

    /// Number of columns.
    pub fn column_count(&self) -> usize {
        self.columns.len()
    }

    /// The NIML type of each column.
    pub fn column_types(&self) -> Vec<NimlValueType> {
        self.columns
            .iter()
            .map(|c| NimlValueType::from_data_type(c.dtype()).expect("checked on construction"))
            .collect()
    }

    /// Column `column`, if it exists.
    pub fn column(&self, column: usize) -> Option<&TypedArray> {
        self.columns.get(column)
    }

    /// The value at `(row, column)` as `f64`, if in bounds.
    pub fn get(&self, row: usize, column: usize) -> Option<f64> {
        self.columns.get(column)?.get_f64(row)
    }
}

/// Collect `f64` values into an array of `dtype`, casting each one.
pub(crate) fn typed_from_f64(dtype: DataType, values: impl Iterator<Item = f64>) -> TypedArray {
    match dtype {
        DataType::UInt8 => TypedArray::UInt8(values.map(|v| v as u8).collect()),
        DataType::Int8 => TypedArray::Int8(values.map(|v| v as i8).collect()),
        DataType::UInt16 => TypedArray::UInt16(values.map(|v| v as u16).collect()),
        DataType::Int16 => TypedArray::Int16(values.map(|v| v as i16).collect()),
        DataType::UInt32 => TypedArray::UInt32(values.map(|v| v as u32).collect()),
        DataType::Int32 => TypedArray::Int32(values.map(|v| v as i32).collect()),
        DataType::UInt64 => TypedArray::UInt64(values.map(|v| v as u64).collect()),
        DataType::Int64 => TypedArray::Int64(values.map(|v| v as i64).collect()),
        DataType::Float32 => TypedArray::Float32(values.map(|v| v as f32).collect()),
        DataType::Float64 => TypedArray::Float64(values.collect()),
    }
}

impl RecordTable {
    /// The rowtype's field layout.
    pub fn fields(&self) -> Vec<RecordField> {
        self.record_type.record_fields().unwrap_or_default()
    }

    /// Record `r` with every length field set from its array, as written.
    pub(crate) fn normalized_row(&self, r: usize) -> Vec<Vec<f64>> {
        let mut row = self.rows[r].clone();
        for (f, field) in self.fields().iter().enumerate() {
            if let (Some(k), Some(len)) = (field.length_from, row.get(f).map(Vec::len)) {
                if let Some(count) = row.get_mut(k) {
                    *count = vec![len as f64];
                }
            }
        }
        row
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
    pub fn column_count(&self) -> usize {
        self.column_types.len()
    }

    /// Fetch a cell, if in bounds.
    pub fn get(&self, row: usize, column: usize) -> Option<&NimlValue> {
        if row >= self.rows || column >= self.column_count() {
            return None;
        }
        self.values.get(row * self.column_count() + column)
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

/// Render a slice of column types as an `ni_type` string, grouping runs of
/// the same type as `N*type` the way AFNI writes them (e.g. `int,3*float`).
pub fn ni_type_string(types: &[NimlValueType]) -> String {
    let mut parts = Vec::new();
    let mut i = 0;
    while i < types.len() {
        let run = types[i..].iter().take_while(|t| **t == types[i]).count();
        let name = types[i].canonical_name();
        parts.push(if run > 1 {
            format!("{run}*{name}")
        } else {
            name.to_string()
        });
        i += run;
    }
    parts.join(",")
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

/// Serialise elements with binary numeric and record bodies
/// ([`serialize_binary`]) and write them to disk.
pub fn write_binary(path: impl AsRef<Path>, elements: &[NimlElement]) -> Result<()> {
    error::write_file(path.as_ref(), &serialize_binary(elements))
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
<SPARSE_DATA ni_type="2*float" ni_dimen="2" >
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
    fn binary_byte_order_and_base64_follow_niml_elemio() {
        // A bare "binary" is big-endian, as in AFNI (default NI_MSB_FIRST).
        let mut bytes = b"<a ni_type=\"int\" ni_dimen=\"1\" ni_form=\"binary\" >".to_vec();
        bytes.extend_from_slice(&258i32.to_be_bytes());
        bytes.extend_from_slice(b"</a>");
        let elements = parse(&bytes).unwrap();
        let NimlData::Numeric(m) = &elements[0].data else {
            panic!()
        };
        assert_eq!(m.get(0, 0), Some(258.0));

        // base64.lsbfirst: two little-endian floats.
        let payload = [1.5f32.to_le_bytes(), (-2.0f32).to_le_bytes()].concat();
        let text = format!(
            "<a ni_type=\"float\" ni_dimen=\"2\" ni_form=\"base64.lsbfirst\" >\n{}\n</a>",
            crate::base64::encode(&payload)
        );
        let elements = parse_str(&text).unwrap();
        let NimlData::Numeric(m) = &elements[0].data else {
            panic!()
        };
        assert_eq!((m.get(0, 0), m.get(1, 0)), (Some(1.5), Some(-2.0)));
    }

    #[test]
    fn ni_type_strings_group_repeats_like_afni() {
        let types = expand_ni_type("int,3*float,String,2*float").unwrap();
        assert_eq!(ni_type_string(&types), "int,3*float,String,2*float");
        assert_eq!(ni_type_string(&[NimlValueType::Float32]), "float");
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
