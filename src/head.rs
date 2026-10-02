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

use std::collections::BTreeMap;
use std::path::Path;

use crate::array::TypedArray;
use crate::error::{self, from_utf8, Error, Result};
use crate::geometry::{Mat44, Orientation, TimeAxis, TimeUnits, View};
use crate::niml::{NimlData, NimlElement, NimlValue, NumericMatrix};
use crate::stat::{parse_statsym_list, StatKind, StatSpec, ThresholdCurve};

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

    /// Number of entries (`count =`). For strings, the byte count including
    /// the terminating NUL that AFNI always stores (added on write when
    /// missing).
    pub fn count(&self) -> usize {
        match self {
            AttributeValue::Int(v) => v.len(),
            AttributeValue::Float(v) => v.len(),
            AttributeValue::String(s) => s.len() + usize::from(!s.ends_with('\0')),
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

    /// The `BRICK_TYPES` code for sub-brick `p`, following AFNI's loader
    /// (`THD_init_datablock_brick` in `thd_initdblk.c`): a missing or empty
    /// attribute means short (code 1) for every sub-brick, and a list shorter
    /// than `nvals` repeats its last entry.
    pub fn brick_type_code(&self, p: usize) -> i64 {
        match self.ints("BRICK_TYPES") {
            Some(codes) if !codes.is_empty() => {
                codes.get(p).copied().unwrap_or(codes[codes.len() - 1])
            }
            _ => 1,
        }
    }

    /// The `BRICK_FLOAT_FACS` scale factor for sub-brick `p`, or `0.0`
    /// (unscaled) when the attribute is missing or too short, as in
    /// `thd_initdblk.c`. AFNI multiplies by any non-zero factor, including a
    /// negative one.
    pub fn brick_factor(&self, p: usize) -> f64 {
        self.floats("BRICK_FLOAT_FACS")
            .and_then(|facs| facs.get(p).copied())
            .unwrap_or(0.0)
    }

    /// Per-sub-brick labels from `BRICK_LABS`, one per sub-brick.
    ///
    /// On disk the labels are `~`-separated; once parsed, the `~` are NULs
    /// (see the module docs). As in `thd_initdblk.c`, label *p* is the *p*-th
    /// field, and a missing or empty field gets AFNI's default `#p`. (AFNI
    /// also truncates labels to 64 characters; they are kept whole here.)
    pub fn brick_labels(&self) -> Vec<String> {
        let mut fields = self
            .get("BRICK_LABS")
            .and_then(|value| match value {
                AttributeValue::String(s) => Some(s.split('\0')),
                _ => None,
            })
            .into_iter()
            .flatten();
        (0..self.nvals())
            .map(|p| match fields.next() {
                Some(label) if !label.is_empty() => label.to_string(),
                _ => format!("#{p}"),
            })
            .collect()
    }

    /// Whether the `.BRIK` data is little-endian, from `BYTEORDER_STRING`.
    ///
    /// As in `thd_initdblk.c`, only the leading `LSB_FIRST` / `MSB_FIRST` is
    /// compared, and a missing or unrecognised value falls back to the host
    /// byte order (AFNI's legacy behaviour, with a warning there).
    pub fn brik_is_little_endian(&self) -> bool {
        match self.string("BYTEORDER_STRING") {
            Some(s) if s.starts_with("LSB_FIRST") => true,
            Some(s) if s.starts_with("MSB_FIRST") => false,
            _ => cfg!(target_endian = "little"),
        }
    }

    // --- Geometry and timing (see `crate::geometry`) ------------------------

    /// `ORIENT_SPECIFIC` as typed orientations, or `None` if the attribute is
    /// missing, too short, or holds an unknown code.
    pub fn orientations(&self) -> Option<[Orientation; 3]> {
        let [a, b, c] = self.orientation()?;
        Some([
            Orientation::from_code(a)?,
            Orientation::from_code(b)?,
            Orientation::from_code(c)?,
        ])
    }

    /// The orientation string AFNI prints (`3dinfo -orient`), e.g. `RAI`.
    pub fn orientation_string(&self) -> Option<String> {
        Some(self.orientations()?.iter().map(|o| o.letter()).collect())
    }

    /// The dataset's view (`SCENE_DATA[0]`).
    pub fn view(&self) -> Option<View> {
        View::from_code(*self.ints("SCENE_DATA")?.first()?)
    }

    /// The cardinal `ijk -> DICOM (RAI)` matrix built from `ORIENT_SPECIFIC`,
    /// `ORIGIN` and `DELTA`, as `THD_daxes_to_mat44` builds it. This ignores
    /// any obliquity; see [`Header::ijk_to_dicom`].
    pub fn ijk_to_dicom_cardinal(&self) -> Result<Mat44> {
        let orient = self
            .orientations()
            .ok_or_else(|| Error::missing("valid ORIENT_SPECIFIC"))?;
        let origin = self.origin().ok_or_else(|| Error::missing("ORIGIN"))?;
        let delta = self.delta().ok_or_else(|| Error::missing("DELTA"))?;
        let mut m = crate::geometry::identity();
        m[0][0] = 0.0;
        m[1][1] = 0.0;
        m[2][2] = 0.0;
        let mut used = [false; 3];
        for axis in 0..3 {
            let row = orient[axis].dicom_axis();
            if std::mem::replace(&mut used[row], true) {
                return Err(Error::invalid(format!(
                    "ORIENT_SPECIFIC {:?} uses one direction twice",
                    orient.map(Orientation::code)
                )));
            }
            m[row][axis] = delta[axis];
            m[row][3] = origin[axis];
        }
        Ok(m)
    }

    /// `IJK_TO_DICOM_REAL` as a matrix, or `None` when the attribute is
    /// missing or has fewer than 12 values.
    pub fn ijk_to_dicom_real(&self) -> Option<Mat44> {
        crate::geometry::mat44_from_3x4(&self.floats("IJK_TO_DICOM_REAL")?)
    }

    /// The `ijk -> DICOM (RAI)` matrix AFNI reports as `aform_real`:
    /// `IJK_TO_DICOM_REAL` when present, otherwise the cardinal matrix.
    pub fn ijk_to_dicom(&self) -> Result<Mat44> {
        match self.ijk_to_dicom_real() {
            Some(m) => Ok(m),
            None => self.ijk_to_dicom_cardinal(),
        }
    }

    /// [`Header::ijk_to_dicom`] converted to RAS, matching the sform that
    /// `3dAFNItoNIFTI` writes.
    pub fn ijk_to_ras(&self) -> Result<Mat44> {
        Ok(crate::geometry::dicom_to_ras(&self.ijk_to_dicom()?))
    }

    /// Degrees from plumb (`3dinfo -obliquity`), from `IJK_TO_DICOM_REAL`;
    /// 0 when that attribute is absent.
    pub fn obliquity(&self) -> f64 {
        self.ijk_to_dicom_real()
            .map_or(0.0, |m| crate::geometry::oblique_angle(&m))
    }

    /// Whether the dataset is oblique (`3dinfo -is_oblique`).
    pub fn is_oblique(&self) -> bool {
        self.obliquity() > 0.0
    }

    /// The time axis, if the dataset has one (`TAXIS_NUMS` and
    /// `TAXIS_FLOATS` both present), following `thd_dsetdblk.c`: slice
    /// offsets are dropped when there are fewer than `TAXIS_NUMS[1]` of them,
    /// and millisecond times are converted to seconds.
    pub fn time_axis(&self) -> Option<TimeAxis> {
        let nums = self.ints("TAXIS_NUMS")?;
        let floats = self.floats("TAXIS_FLOATS")?;
        let float = |i: usize| floats.get(i).copied().unwrap_or(0.0);
        let stored_units = TimeUnits::from_code(nums.get(2).copied().unwrap_or(-1));
        let scale = if stored_units == TimeUnits::Milliseconds {
            0.001
        } else {
            1.0
        };

        let nsl = nums.get(1).copied().unwrap_or(0).max(0) as usize;
        let offsets = self.floats("TAXIS_OFFSETS").unwrap_or_default();
        let (slice_offsets, mut slice_z_origin, mut slice_dz) = if nsl > 0 && offsets.len() >= nsl {
            (
                offsets[..nsl].iter().map(|t| t * scale).collect(),
                float(3),
                float(4),
            )
        } else {
            (Vec::new(), 0.0, 0.0)
        };
        // AFNI fills in an unset slice z-origin/spacing from the grid (2025).
        if !slice_offsets.is_empty() && slice_z_origin == 0.0 && slice_dz == 0.0 {
            if let (Some(origin), Some(delta)) = (self.origin(), self.delta()) {
                slice_z_origin = origin[2];
                slice_dz = delta[2];
            }
        }
        Some(TimeAxis {
            nt: (*nums.first()?).max(0) as usize,
            origin: float(0) * scale,
            step: float(1) * scale,
            duration: float(2) * scale,
            stored_units,
            slice_offsets,
            slice_z_origin,
            slice_dz,
        })
    }

    // --- Statistics (see `crate::stat`) -------------------------------------

    /// The statistic held by each sub-brick (`nvals` entries; `None` where a
    /// sub-brick is not a statistic).
    ///
    /// Follows `thd_initdblk.c`: a non-empty `BRICK_STATSYM` (e.g.
    /// `Ttest(23);none;Ftest(2,40)`) wins; otherwise `BRICK_STATAUX` records
    /// `[sub-brick, code, nparams, params...]` are used, with parameters
    /// zero-filled or truncated to the count the statistic needs. Unlike
    /// AFNI, codes 11–24 (e.g. `Normal`) are kept rather than dropped.
    pub fn brick_stats(&self) -> Vec<Option<StatSpec>> {
        let nvals = self.nvals();
        let mut stats = vec![None; nvals];
        if let Some(symbols) = self
            .string("BRICK_STATSYM")
            .filter(|s| !s.trim().is_empty())
        {
            for (p, spec) in parse_statsym_list(symbols)
                .into_iter()
                .take(nvals)
                .enumerate()
            {
                stats[p] = spec;
            }
            return stats;
        }
        let aux = self.floats("BRICK_STATAUX").unwrap_or_default();
        let mut pos = 0;
        while pos + 3 <= aux.len() {
            let (p, code, count) = (aux[pos], aux[pos + 1], aux[pos + 2]);
            pos += 3;
            let count = (count.max(0.0) as usize).min(aux.len() - pos);
            let params = &aux[pos..pos + count];
            pos += count;
            if p >= 0.0 && (p as usize) < nvals {
                if let Some(kind) = StatKind::from_code(code as i64) {
                    stats[p as usize] = Some(StatSpec::new(kind, params, 0.0));
                }
            }
        }
        stats
    }

    /// The label table in `VALUE_LABEL_DTABLE` (set by `3drefit
    /// -labeltable`), which maps the integer values of a label volume such
    /// as `aparc+aseg` to region names. The attribute holds a NIML
    /// `VALUE_LABEL_DTABLE` element as text.
    pub fn value_label_table(&self) -> Result<Option<crate::labels::LabelTable>> {
        let Some(text) = self.string("VALUE_LABEL_DTABLE") else {
            return Ok(None);
        };
        crate::labels::LabelTable::find(&crate::niml::parse_str(text.trim_end_matches('\0'))?)
    }

    /// Store `table` as `VALUE_LABEL_DTABLE`, as `3drefit -labeltable` does.
    pub fn set_value_label_table(&mut self, table: &crate::labels::LabelTable) {
        let text = crate::niml::serialize(&[table.to_value_label_dtable()]);
        self.set("VALUE_LABEL_DTABLE", AttributeValue::String(text));
    }

    /// The FDR curve for sub-brick `p` (`FDRCURVE_%06d`), if present.
    pub fn fdr_curve(&self, p: usize) -> Option<ThresholdCurve> {
        ThresholdCurve::from_values(&self.floats(&format!("FDRCURVE_{p:06}"))?)
    }

    /// The missed-detection-fraction curve for sub-brick `p`
    /// (`MDFCURVE_%06d`), if present.
    pub fn mdf_curve(&self, p: usize) -> Option<ThresholdCurve> {
        ThresholdCurve::from_values(&self.floats(&format!("MDFCURVE_{p:06}"))?)
    }

    // --- NIML form: `AFNI_atr` elements ---------------------------------------

    /// Build a header from NIML `AFNI_atr` elements, as AFNI does for the
    /// NIfTI AFNI extension and for NIML datasets (`THD_dblkatr_from_niml`,
    /// `thd_nimlatr.c`).
    ///
    /// Every `AFNI_atr` element (name matched case-insensitively) in `group`
    /// and its sub-groups with a non-empty `atr_name` (or `AFNI_name`) and a
    /// single non-empty column becomes an attribute: integer columns become
    /// [`AttributeValue::Int`], floating-point columns
    /// [`AttributeValue::Float`], and strings are joined and decoded like
    /// `.HEAD` strings (`~` becomes NUL, plus a terminating NUL). Anything else
    /// is skipped, as AFNI skips it. A `self_idcode` (or `AFNI_idcode`) on the
    /// group overrides `IDCODE_STRING`.
    pub fn from_niml(group: &NimlElement) -> Self {
        fn collect(group: &NimlElement, header: &mut Header) {
            let NimlData::Group(children) = &group.data else {
                return;
            };
            for child in children {
                if matches!(child.data, NimlData::Group(_)) {
                    collect(child, header);
                    continue;
                }
                if !child.name.eq_ignore_ascii_case("AFNI_atr") {
                    continue;
                }
                let Some(name) = child
                    .attrs
                    .get("atr_name")
                    .or_else(|| child.attrs.get("AFNI_name"))
                    .filter(|n| !n.is_empty())
                else {
                    continue;
                };
                let value = match &child.data {
                    NimlData::Numeric(m) if m.column_count() == 1 && m.rows > 0 => {
                        let column = &m.columns[0];
                        if column.dtype().is_float() {
                            AttributeValue::Float(column.to_f64_vec())
                        } else {
                            AttributeValue::Int(
                                column.to_f64_vec().iter().map(|&v| v as i64).collect(),
                            )
                        }
                    }
                    NimlData::Text(text) => AttributeValue::String(decode_string(text) + "\0"),
                    NimlData::Mixed(t)
                        if t.column_count() == 1
                            && t.rows > 0
                            && t.values.iter().all(|v| matches!(v, NimlValue::Text(_))) =>
                    {
                        let joined: String = t
                            .values
                            .iter()
                            .map(|v| match v {
                                NimlValue::Text(s) => s.as_str(),
                                _ => "",
                            })
                            .collect();
                        AttributeValue::String(decode_string(&joined) + "\0")
                    }
                    _ => continue,
                };
                header.set(name.clone(), value);
            }
        }

        let mut header = Header::default();
        collect(group, &mut header);
        if let Some(id) = group
            .attrs
            .get("self_idcode")
            .or_else(|| group.attrs.get("AFNI_idcode"))
            .filter(|id| !id.is_empty())
        {
            // THD_set_string_atr stores the terminating NUL, like every other
            // string attribute.
            header.set("IDCODE_STRING", AttributeValue::String(format!("{id}\0")));
        }
        header
    }

    /// The header as an `AFNI_attributes` NIML group of `AFNI_atr` elements,
    /// the layout AFNI writes into a NIfTI AFNI extension
    /// (`THD_nimlize_dsetatr`). Strings are encoded like `.HEAD` strings
    /// (NUL becomes `~`) without their final terminator.
    pub fn to_niml(&self) -> NimlElement {
        let children = self
            .attributes
            .iter()
            .map(|attr| {
                let mut attrs = BTreeMap::new();
                attrs.insert("atr_name".to_string(), attr.name.clone());
                let data = match &attr.value {
                    AttributeValue::Int(v) => NimlData::Numeric(NumericMatrix {
                        rows: v.len(),
                        columns: vec![TypedArray::Int32(v.iter().map(|&x| x as i32).collect())],
                    }),
                    AttributeValue::Float(v) => NimlData::Numeric(NumericMatrix {
                        rows: v.len(),
                        columns: vec![TypedArray::Float32(v.iter().map(|&x| x as f32).collect())],
                    }),
                    AttributeValue::String(s) => {
                        NimlData::Text(s.strip_suffix('\0').unwrap_or(s).replace('\0', "~"))
                    }
                };
                NimlElement {
                    name: "AFNI_atr".to_string(),
                    attrs,
                    data,
                }
            })
            .collect();
        let mut attrs = BTreeMap::new();
        if let Some(id) = self.idcode() {
            attrs.insert("self_idcode".to_string(), id.to_string());
        }
        NimlElement::group("AFNI_attributes", attrs, children)
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
            // AFNI writes `count` characters, the last being the terminating
            // NUL, shown as `~` (`THD_write_atr`). A string read from a file
            // already ends in NUL; one built in code may not.
            out.push('\'');
            out.push_str(&encode_string(s));
            if !s.ends_with('\0') {
                out.push('~');
            }
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

    fn geometry_header() -> Header {
        let mut h = Header::default();
        h.set("DATASET_RANK", AttributeValue::Int(vec![3, 3]));
        // LPI: x runs L->R, y P->A, z I->S.
        h.set("ORIENT_SPECIFIC", AttributeValue::Int(vec![1, 2, 4]));
        h.set("ORIGIN", AttributeValue::Float(vec![12.5, -7.0, -3.0]));
        h.set("DELTA", AttributeValue::Float(vec![-1.5, -2.0, 2.5]));
        h
    }

    #[test]
    fn cardinal_matrix_follows_thd_daxes_to_mat44() {
        let h = geometry_header();
        let expected = [
            [-1.5, 0.0, 0.0, 12.5],
            [0.0, -2.0, 0.0, -7.0],
            [0.0, 0.0, 2.5, -3.0],
            [0.0, 0.0, 0.0, 1.0],
        ];
        assert_eq!(h.ijk_to_dicom_cardinal().unwrap(), expected);
        // No IJK_TO_DICOM_REAL: falls back to the cardinal matrix, plumb.
        assert_eq!(h.ijk_to_dicom().unwrap(), expected);
        assert!(!h.is_oblique());
        assert_eq!(h.orientation_string().as_deref(), Some("LPI"));

        // An axis order such as ASL permutes columns into DICOM rows.
        let mut h = geometry_header();
        h.set("ORIENT_SPECIFIC", AttributeValue::Int(vec![3, 5, 1]));
        let m = h.ijk_to_dicom_cardinal().unwrap();
        assert_eq!((m[1][0], m[2][1], m[0][2]), (-1.5, -2.0, 2.5));
        assert_eq!((m[1][3], m[2][3], m[0][3]), (12.5, -7.0, -3.0));

        h.set("ORIENT_SPECIFIC", AttributeValue::Int(vec![0, 1, 4]));
        assert!(
            h.ijk_to_dicom_cardinal().is_err(),
            "R2L and L2R share an axis"
        );
    }

    #[test]
    fn time_axis_converts_milliseconds_and_checks_offsets() {
        let mut h = geometry_header();
        h.set("TAXIS_NUMS", AttributeValue::Int(vec![3, 3, 77001]));
        h.set(
            "TAXIS_FLOATS",
            AttributeValue::Float(vec![100.0, 2500.0, 0.0, 0.0, 0.0]),
        );
        h.set(
            "TAXIS_OFFSETS",
            AttributeValue::Float(vec![0.0, 1250.0, 500.0]),
        );
        let t = h.time_axis().unwrap();
        assert_eq!(t.stored_units, TimeUnits::Milliseconds);
        assert_eq!((t.nt, t.origin, t.tr_seconds()), (3, 0.1, Some(2.5)));
        assert_eq!(t.slice_offsets, [0.0, 1.25, 0.5]);
        // Unset slice z-origin/spacing are taken from the grid.
        assert_eq!((t.slice_z_origin, t.slice_dz), (-3.0, 2.5));

        // Fewer offsets than TAXIS_NUMS[1] says: AFNI drops slice timing.
        h.set("TAXIS_NUMS", AttributeValue::Int(vec![3, 6, 0]));
        let t = h.time_axis().unwrap();
        assert_eq!(t.stored_units, TimeUnits::Seconds);
        assert!(t.slice_offsets.is_empty());
        assert_eq!(t.tr_seconds(), Some(2500.0));

        h.set("TAXIS_NUMS", AttributeValue::Int(vec![3, 0, 77003]));
        assert_eq!(h.time_axis().unwrap().tr_seconds(), None);
    }

    #[test]
    fn brick_labels_keep_positions_and_default_like_afni() {
        let mut h = Header::default();
        h.set("DATASET_RANK", AttributeValue::Int(vec![3, 4]));
        // On disk: 'a~~c~  ->  fields "a", "", "c", then nothing for #3.
        h.set("BRICK_LABS", AttributeValue::String(decode_string("a~~c~")));
        assert_eq!(h.brick_labels(), ["a", "#1", "c", "#3"]);
        h.attributes.clear();
        h.set("DATASET_RANK", AttributeValue::Int(vec![3, 2]));
        assert_eq!(h.brick_labels(), ["#0", "#1"]);
    }

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
