//! GIfTI surface-geometry and data files (`.gii`).
//!
//! GIfTI is the XML interchange format used by SUMA, FreeSurfer, Connectome
//! Workbench, and others for surface meshes and per-vertex data. A file is a
//! `<GIFTI>` root holding optional file-level `<MetaData>` and `<LabelTable>`,
//! then one or more `<DataArray>` elements. Each data array carries a NIfTI
//! intent (what it means) and datatype (how it is stored), and its payload is
//! encoded as ASCII text, Base64, or gzipped Base64.
//!
//! This module reads all three encodings and writes ASCII or Base64. It is
//! built on the crate's own XML, Base64, and [`crate::array`] code — no
//! external GIfTI or XML dependency.
//!
//! References: the [GIfTI 1.0 spec](https://www.nitrc.org/projects/gifti/) and
//! `SUMAvista/src/pysuma/io.py`.

use std::path::Path;

use crate::array::{self, DataType, TypedArray};
use crate::error::{self, from_utf8, Error, Result};
use crate::xml::{self, XmlNode};

/// NIfTI intent codes used by GIfTI data arrays.
///
/// Constants cover the geometry/surface codes (1002–1009, 2001–2005) and the
/// full set of statistical codes (2–24) from the NIfTI-1 specification.
pub mod intent {
    // --- Geometry / surface codes ----------------------------------------
    /// Vertex coordinates (`NIFTI_INTENT_POINTSET`).
    pub const POINTSET: i32 = 1008;
    /// Triangle indices (`NIFTI_INTENT_TRIANGLE`).
    pub const TRIANGLE: i32 = 1009;
    /// Per-vertex time series (`NIFTI_INTENT_TIME_SERIES`).
    pub const TIME_SERIES: i32 = 2001;
    /// Node index list (`NIFTI_INTENT_NODE_INDEX`).
    pub const NODE_INDEX: i32 = 2002;
    /// Per-vertex scalar shape data (`NIFTI_INTENT_SHAPE`).
    pub const SHAPE: i32 = 2005;
    /// Per-vertex labels (`NIFTI_INTENT_LABEL`).
    pub const LABEL: i32 = 1002;
    /// Per-vertex surface normals (`NIFTI_INTENT_NORMAL`, geometry code).
    pub const SURFACE_NORMAL: i32 = 1007;
    /// No specific intent (`NIFTI_INTENT_NONE`).
    pub const NONE: i32 = 0;

    // --- Statistical codes (NIfTI-1 Table C-2) ----------------------------
    /// Correlation coefficient `r` (1 param: dof).
    pub const CORREL: i32 = 2;
    /// Student t statistic (1 param: dof).
    pub const TTEST: i32 = 3;
    /// F statistic (2 params: num/den dof).
    pub const FTEST: i32 = 4;
    /// Standard normal Z.
    pub const ZSCORE: i32 = 5;
    /// Chi-squared (1 param: dof).
    pub const CHISQ: i32 = 6;
    /// Beta distribution (2 params: a, b).
    pub const BETA: i32 = 7;
    /// Binomial distribution (2 params: n, p).
    pub const BINOM: i32 = 8;
    /// Gamma distribution (2 params: shape, scale).
    pub const GAMMA: i32 = 9;
    /// Poisson distribution (1 param: mean).
    pub const POISSON: i32 = 10;
    /// Normal distribution (2 params: mean, σ).
    pub const NORMAL: i32 = 11;
    /// Noncentral F (3 params: num dof, den dof, λ).
    pub const FTEST_NONC: i32 = 12;
    /// Noncentral chi-squared (2 params: dof, λ).
    pub const CHISQ_NONC: i32 = 13;
    /// Logistic distribution (2 params: loc, scale).
    pub const LOGISTIC: i32 = 14;
    /// Laplace distribution (2 params: loc, scale).
    pub const LAPLACE: i32 = 15;
    /// Uniform distribution (2 params: lower, upper).
    pub const UNIFORM: i32 = 16;
    /// Noncentral t (2 params: dof, λ).
    pub const TTEST_NONC: i32 = 17;
    /// Weibull distribution (3 params: loc, scale, power).
    pub const WEIBULL: i32 = 18;
    /// Chi distribution (1 param: dof).
    pub const CHI: i32 = 19;
    /// Inverse Gaussian (2 params: μ, λ).
    pub const INVGAUSS: i32 = 20;
    /// Extreme-value type I (2 params: loc, scale).
    pub const EXTVAL: i32 = 21;
    /// p-value.
    pub const PVAL: i32 = 22;
    /// Log p-value (base 10).
    pub const LOGPVAL: i32 = 23;
    /// Log10 p-value.
    pub const LOG10PVAL: i32 = 24;

    // --- Misc codes -------------------------------------------------------
    /// Generic vector (`NIFTI_INTENT_VECTOR`).
    pub const VECTOR: i32 = 1006;
    /// RGB colour vector (`NIFTI_INTENT_RGB_VECTOR`).
    pub const RGB_VECTOR: i32 = 2003;
    /// RGBA colour vector (`NIFTI_INTENT_RGBA_VECTOR`).
    pub const RGBA_VECTOR: i32 = 2004;

    /// The `NIFTI_INTENT_*` name for a code, if known.
    pub fn name_for_code(code: i32) -> Option<&'static str> {
        Some(match code {
            POINTSET => "NIFTI_INTENT_POINTSET",
            TRIANGLE => "NIFTI_INTENT_TRIANGLE",
            TIME_SERIES => "NIFTI_INTENT_TIME_SERIES",
            NODE_INDEX => "NIFTI_INTENT_NODE_INDEX",
            SHAPE => "NIFTI_INTENT_SHAPE",
            LABEL => "NIFTI_INTENT_LABEL",
            SURFACE_NORMAL => "NIFTI_INTENT_NORMAL",
            NONE => "NIFTI_INTENT_NONE",
            CORREL => "NIFTI_INTENT_CORREL",
            TTEST => "NIFTI_INTENT_TTEST",
            FTEST => "NIFTI_INTENT_FTEST",
            ZSCORE => "NIFTI_INTENT_ZSCORE",
            CHISQ => "NIFTI_INTENT_CHISQ",
            BETA => "NIFTI_INTENT_BETA",
            BINOM => "NIFTI_INTENT_BINOM",
            GAMMA => "NIFTI_INTENT_GAMMA",
            POISSON => "NIFTI_INTENT_POISSON",
            NORMAL => "NIFTI_INTENT_NORMAL",
            FTEST_NONC => "NIFTI_INTENT_FTEST_NONC",
            CHISQ_NONC => "NIFTI_INTENT_CHISQ_NONC",
            LOGISTIC => "NIFTI_INTENT_LOGISTIC",
            LAPLACE => "NIFTI_INTENT_LAPLACE",
            UNIFORM => "NIFTI_INTENT_UNIFORM",
            TTEST_NONC => "NIFTI_INTENT_TTEST_NONC",
            WEIBULL => "NIFTI_INTENT_WEIBULL",
            CHI => "NIFTI_INTENT_CHI",
            INVGAUSS => "NIFTI_INTENT_INVGAUSS",
            EXTVAL => "NIFTI_INTENT_EXTVAL",
            PVAL => "NIFTI_INTENT_PVAL",
            LOGPVAL => "NIFTI_INTENT_LOGPVAL",
            LOG10PVAL => "NIFTI_INTENT_LOG10PVAL",
            VECTOR => "NIFTI_INTENT_VECTOR",
            RGB_VECTOR => "NIFTI_INTENT_RGB_VECTOR",
            RGBA_VECTOR => "NIFTI_INTENT_RGBA_VECTOR",
            _ => return None,
        })
    }

    /// The integer code for a `NIFTI_INTENT_*` name, if known.
    pub fn code_for_name(name: &str) -> Option<i32> {
        Some(match name {
            "NIFTI_INTENT_POINTSET" => POINTSET,
            "NIFTI_INTENT_TRIANGLE" => TRIANGLE,
            "NIFTI_INTENT_TIME_SERIES" => TIME_SERIES,
            "NIFTI_INTENT_NODE_INDEX" => NODE_INDEX,
            "NIFTI_INTENT_SHAPE" => SHAPE,
            "NIFTI_INTENT_LABEL" => LABEL,
            "NIFTI_INTENT_NORMAL" => SURFACE_NORMAL,
            "NIFTI_INTENT_NONE" => NONE,
            "NIFTI_INTENT_CORREL" => CORREL,
            "NIFTI_INTENT_TTEST" => TTEST,
            "NIFTI_INTENT_FTEST" => FTEST,
            "NIFTI_INTENT_ZSCORE" => ZSCORE,
            "NIFTI_INTENT_CHISQ" => CHISQ,
            "NIFTI_INTENT_BETA" => BETA,
            "NIFTI_INTENT_BINOM" => BINOM,
            "NIFTI_INTENT_GAMMA" => GAMMA,
            "NIFTI_INTENT_POISSON" => POISSON,
            "NIFTI_INTENT_FTEST_NONC" => FTEST_NONC,
            "NIFTI_INTENT_CHISQ_NONC" => CHISQ_NONC,
            "NIFTI_INTENT_LOGISTIC" => LOGISTIC,
            "NIFTI_INTENT_LAPLACE" => LAPLACE,
            "NIFTI_INTENT_UNIFORM" => UNIFORM,
            "NIFTI_INTENT_TTEST_NONC" => TTEST_NONC,
            "NIFTI_INTENT_WEIBULL" => WEIBULL,
            "NIFTI_INTENT_CHI" => CHI,
            "NIFTI_INTENT_INVGAUSS" => INVGAUSS,
            "NIFTI_INTENT_EXTVAL" => EXTVAL,
            "NIFTI_INTENT_PVAL" => PVAL,
            "NIFTI_INTENT_LOGPVAL" => LOGPVAL,
            "NIFTI_INTENT_LOG10PVAL" => LOG10PVAL,
            "NIFTI_INTENT_VECTOR" => VECTOR,
            "NIFTI_INTENT_RGB_VECTOR" => RGB_VECTOR,
            "NIFTI_INTENT_RGBA_VECTOR" => RGBA_VECTOR,
            _ => return None,
        })
    }
}

/// Ordered key/value metadata (GIfTI `<MetaData>`); duplicate keys are legal.
pub type Meta = Vec<(String, String)>;

/// Look up the first value for a metadata key.
pub fn meta_get<'a>(meta: &'a Meta, key: &str) -> Option<&'a str> {
    meta.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
}

/// On-disk payload encoding of a [`DataArray`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Encoding {
    /// Whitespace-separated ASCII numbers.
    Ascii,
    /// Base64-encoded binary.
    Base64Binary,
    /// gzip-then-Base64 binary.
    GZipBase64Binary,
    /// Payload stored in a separate file (not supported).
    ExternalFileBinary,
}

impl Encoding {
    fn from_str(s: &str) -> Result<Self> {
        Ok(match s {
            // Modern spellings and the legacy `GIFTI_ENCODING_*` aliases.
            "ASCII" | "GIFTI_ENCODING_ASCII" => Self::Ascii,
            "Base64Binary" | "GIFTI_ENCODING_B64BIN" => Self::Base64Binary,
            "GZipBase64Binary" | "GIFTI_ENCODING_B64GZ" => Self::GZipBase64Binary,
            "ExternalFileBinary" | "GIFTI_ENCODING_EXTBIN" => Self::ExternalFileBinary,
            other => return Err(Error::unsupported(format!("GIfTI Encoding {other:?}"))),
        })
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Ascii => "ASCII",
            Self::Base64Binary => "Base64Binary",
            Self::GZipBase64Binary => "GZipBase64Binary",
            Self::ExternalFileBinary => "ExternalFileBinary",
        }
    }
}

/// Array indexing order of a [`DataArray`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IndexOrder {
    /// C order (`RowMajorOrder`).
    RowMajor,
    /// Fortran order (`ColumnMajorOrder`).
    ColumnMajor,
}

impl IndexOrder {
    fn as_str(self) -> &'static str {
        match self {
            Self::RowMajor => "RowMajorOrder",
            Self::ColumnMajor => "ColumnMajorOrder",
        }
    }
}

/// One `<CoordinateSystemTransformMatrix>` record.
#[derive(Debug, Clone, PartialEq)]
pub struct CoordSystem {
    /// The source space name (e.g. `NIFTI_XFORM_TALAIRACH`).
    pub data_space: String,
    /// The destination space name.
    pub transformed_space: String,
    /// The 4x4 transform, row-major.
    pub xform: [[f64; 4]; 4],
}

/// One `<Label>` entry in a file-level `<LabelTable>`.
#[derive(Debug, Clone, PartialEq)]
pub struct Label {
    /// The label key (`Key`, or legacy `Index`).
    pub key: i32,
    /// RGBA components in `[0, 1]`, if present.
    pub rgba: Option<[f32; 4]>,
    /// The label text.
    pub text: String,
}

/// One `<DataArray>`: a typed buffer plus its descriptive attributes.
#[derive(Debug, Clone, PartialEq)]
pub struct DataArray {
    /// NIfTI intent code (see [`intent`]).
    pub intent: i32,
    /// Dimensions (`Dim0`, `Dim1`, …).
    pub dims: Vec<usize>,
    /// Indexing order.
    pub index_order: IndexOrder,
    /// The encoding the array was read from / should be written with.
    pub encoding: Encoding,
    /// Coordinate-system records (common on POINTSET arrays).
    pub coordsys: Vec<CoordSystem>,
    /// Per-array metadata.
    pub meta: Meta,
    /// The decoded numeric data.
    pub data: TypedArray,
}

impl DataArray {
    /// The total element count implied by `dims`.
    pub fn element_count(&self) -> usize {
        self.dims.iter().copied().product()
    }

    /// The statistic this array holds, from its `Intent` (a NIfTI intent
    /// code, which is also AFNI's stat code) and its `intent_p1`..`intent_p3`
    /// metadata, as AFNI's GIfTI code writes them. `None` when the intent is
    /// not a statistic (e.g. `NIFTI_INTENT_NONE`, `POINTSET`).
    pub fn stat(&self) -> Option<crate::stat::StatSpec> {
        let param = |key| {
            meta_get(&self.meta, key)
                .and_then(|v| v.trim().parse().ok())
                .unwrap_or(0.0)
        };
        crate::stat::StatSpec::from_nifti_intent(
            i64::from(self.intent),
            [param("intent_p1"), param("intent_p2"), param("intent_p3")],
        )
    }
}

/// A complete in-memory GIfTI image.
#[derive(Debug, Clone, PartialEq)]
pub struct Gifti {
    /// The `Version` attribute (default `1.0`).
    pub version: String,
    /// File-level metadata.
    pub meta: Meta,
    /// File-level label table (used by `.label.gii`).
    pub label_table: Vec<Label>,
    /// The data arrays, in document order.
    pub data_arrays: Vec<DataArray>,
}

impl Gifti {
    /// Read a GIfTI file (`.gii`, optionally gzip-compressed).
    pub fn read(path: impl AsRef<Path>) -> Result<Self> {
        let mut bytes = error::read_file(path.as_ref())?;
        if crate::compress::is_gzip(&bytes) {
            bytes = crate::compress::gunzip(&bytes)?;
        }
        Self::parse(&from_utf8(&bytes, "GIfTI file")?)
    }

    /// Parse GIfTI XML from a string.
    pub fn parse(text: &str) -> Result<Self> {
        let root = xml::parse(text)?;
        if root.name != "GIFTI" {
            return Err(Error::parse(format!(
                "expected <GIFTI> root, got <{}>",
                root.name
            )));
        }
        let version = root.attr("Version").unwrap_or("1.0").to_string();

        let mut meta = Vec::new();
        let mut label_table = Vec::new();
        let mut data_arrays = Vec::new();
        for child in &root.children {
            match child.name.as_str() {
                "MetaData" => meta = parse_meta(child),
                "LabelTable" => label_table = parse_label_table(child),
                "DataArray" => data_arrays.push(parse_data_array(child)?),
                _ => {}
            }
        }

        Ok(Self {
            version,
            meta,
            label_table,
            data_arrays,
        })
    }

    /// The first data array with the given intent code.
    pub fn array_with_intent(&self, intent: i32) -> Option<&DataArray> {
        self.data_arrays.iter().find(|a| a.intent == intent)
    }

    /// Convenience: vertices from the `POINTSET` array as `[x, y, z]` triples.
    pub fn pointset(&self) -> Option<Vec<[f32; 3]>> {
        let array = self.array_with_intent(intent::POINTSET)?;
        let flat = array.data.to_f64_vec();
        Some(
            flat.chunks_exact(3)
                .map(|c| [c[0] as f32, c[1] as f32, c[2] as f32])
                .collect(),
        )
    }

    /// Convenience: triangles from the `TRIANGLE` array as index triples.
    pub fn triangles(&self) -> Option<Vec<[u32; 3]>> {
        let array = self.array_with_intent(intent::TRIANGLE)?;
        let flat = array.data.to_f64_vec();
        Some(
            flat.chunks_exact(3)
                .map(|c| [c[0] as u32, c[1] as u32, c[2] as u32])
                .collect(),
        )
    }

    /// Convert to the crate's plain [`crate::surface::Surface`] mesh, if this is
    /// a surface (has both POINTSET and TRIANGLE arrays).
    pub fn to_surface(&self) -> Result<crate::surface::Surface> {
        let vertices = self
            .pointset()
            .ok_or_else(|| Error::missing("GIfTI POINTSET array"))?;
        let faces = self
            .triangles()
            .ok_or_else(|| Error::missing("GIfTI TRIANGLE array"))?;
        Ok(crate::surface::Surface::from_geometry(vertices, faces))
    }

    /// Serialise to GIfTI XML. Each array is written with its own `encoding`
    /// (gzip arrays fall back to plain Base64).
    pub fn to_xml_string(&self) -> String {
        let mut out = String::new();
        out.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
        out.push_str(
            "<!DOCTYPE GIFTI SYSTEM \"http://www.nitrc.org/frs/download.php/115/gifti.dtd\">\n",
        );
        out.push_str(&format!(
            "<GIFTI Version=\"{}\" NumberOfDataArrays=\"{}\">\n",
            xml::escape(&self.version),
            self.data_arrays.len()
        ));
        write_meta(&self.meta, &mut out, 1);
        write_label_table(&self.label_table, &mut out);
        for array in &self.data_arrays {
            write_data_array(array, &mut out);
        }
        out.push_str("</GIFTI>\n");
        out
    }

    /// Write to a `.gii` file (plain XML; gzip the result yourself for
    /// `.gii.gz`).
    pub fn write(&self, path: impl AsRef<Path>) -> Result<()> {
        error::write_file(path.as_ref(), self.to_xml_string().as_bytes())
    }
}

fn parse_meta(node: &XmlNode) -> Meta {
    node.children_named("MD")
        .map(|md| {
            (
                md.child_text("Name").unwrap_or("").to_string(),
                md.child_text("Value").unwrap_or("").to_string(),
            )
        })
        .collect()
}

fn parse_label_table(node: &XmlNode) -> Vec<Label> {
    node.children_named("Label")
        .map(|label| {
            let key = label
                .attr("Key")
                .or_else(|| label.attr("Index"))
                .and_then(|s| s.parse().ok())
                .unwrap_or(0);
            let rgba = match (
                label.attr("Red").and_then(|s| s.parse().ok()),
                label.attr("Green").and_then(|s| s.parse().ok()),
                label.attr("Blue").and_then(|s| s.parse().ok()),
                label.attr("Alpha").and_then(|s| s.parse().ok()),
            ) {
                (Some(r), Some(g), Some(b), a) => Some([r, g, b, a.unwrap_or(1.0)]),
                _ => None,
            };
            Label {
                key,
                rgba,
                text: label.text.trim().to_string(),
            }
        })
        .collect()
}

fn parse_data_array(node: &XmlNode) -> Result<DataArray> {
    let intent = parse_intent(
        node.attr("Intent")
            .ok_or_else(|| Error::missing("DataArray Intent"))?,
    )?;
    let dtype = parse_datatype(
        node.attr("DataType")
            .ok_or_else(|| Error::missing("DataArray DataType"))?,
    )?;
    let encoding = Encoding::from_str(node.attr("Encoding").unwrap_or("ASCII"))?;
    let little = !matches!(node.attr("Endian"), Some("BigEndian" | "GIFTI_ENDIAN_BIG"));
    let index_order = match node.attr("ArrayIndexingOrder").unwrap_or("RowMajorOrder") {
        "ColumnMajorOrder" | "GIFTI_IND_ORD_COL_MAJOR" => IndexOrder::ColumnMajor,
        _ => IndexOrder::RowMajor,
    };
    let dims = parse_dims(node)?;

    let mut coordsys = Vec::new();
    let mut meta = Vec::new();
    let mut data_text = "";
    for child in &node.children {
        match child.name.as_str() {
            "MetaData" => meta = parse_meta(child),
            "CoordinateSystemTransformMatrix" => coordsys.push(parse_coord_system(child)),
            "Data" => data_text = &child.text,
            _ => {}
        }
    }

    let total: usize = dims.iter().copied().product();
    let data = match encoding {
        Encoding::Ascii => array::decode_ascii(data_text, dtype, total)?,
        Encoding::Base64Binary => {
            let bytes = crate::base64::decode(data_text)?;
            array::decode_binary(&bytes, dtype, little, total)?
        }
        Encoding::GZipBase64Binary => {
            let packed = crate::base64::decode(data_text)?;
            let bytes = crate::compress::inflate_flexible(&packed)?;
            array::decode_binary(&bytes, dtype, little, total)?
        }
        Encoding::ExternalFileBinary => {
            return Err(Error::unsupported("GIfTI ExternalFileBinary encoding"));
        }
    };

    Ok(DataArray {
        intent,
        dims,
        index_order,
        encoding,
        coordsys,
        meta,
        data,
    })
}

fn parse_coord_system(node: &XmlNode) -> CoordSystem {
    let mut xform = [[0.0; 4]; 4];
    for (i, row) in xform.iter_mut().enumerate() {
        row[i] = 1.0;
    }
    if let Some(matrix) = node.child("MatrixData") {
        let nums: Vec<f64> = matrix
            .text
            .split_whitespace()
            .filter_map(|t| t.parse().ok())
            .collect();
        if nums.len() == 16 {
            for i in 0..4 {
                for j in 0..4 {
                    xform[i][j] = nums[i * 4 + j];
                }
            }
        }
    }
    CoordSystem {
        data_space: node
            .child_text("DataSpace")
            .unwrap_or("NIFTI_XFORM_UNKNOWN")
            .trim()
            .to_string(),
        transformed_space: node
            .child_text("TransformedSpace")
            .unwrap_or("NIFTI_XFORM_UNKNOWN")
            .trim()
            .to_string(),
        xform,
    }
}

fn parse_intent(s: &str) -> Result<i32> {
    if let Ok(v) = s.parse::<i32>() {
        return Ok(v);
    }
    intent::code_for_name(s).ok_or_else(|| Error::parse(format!("unknown GIfTI Intent {s:?}")))
}

fn parse_datatype(s: &str) -> Result<DataType> {
    if let Ok(code) = s.parse::<i32>() {
        return DataType::from_code(code)
            .ok_or_else(|| Error::unsupported(format!("GIfTI DataType code {code}")));
    }
    DataType::from_name(s).ok_or_else(|| Error::parse(format!("unknown GIfTI DataType {s:?}")))
}

fn parse_dims(node: &XmlNode) -> Result<Vec<usize>> {
    let ndim: usize = node
        .attr("Dimensionality")
        .ok_or_else(|| Error::missing("DataArray Dimensionality"))?
        .parse()
        .map_err(|_| Error::parse("invalid GIfTI Dimensionality"))?;
    let mut dims = Vec::with_capacity(ndim);
    for i in 0..ndim {
        let key = format!("Dim{i}");
        let d = node
            .attr(&key)
            .ok_or_else(|| Error::missing(format!("DataArray {key}")))?
            .parse()
            .map_err(|_| Error::parse(format!("invalid GIfTI {key}")))?;
        dims.push(d);
    }
    Ok(dims)
}

fn write_meta(meta: &Meta, out: &mut String, indent: usize) {
    let pad = "   ".repeat(indent);
    out.push_str(&format!("{pad}<MetaData>\n"));
    for (key, value) in meta {
        out.push_str(&format!("{pad}   <MD>\n"));
        out.push_str(&format!("{pad}      <Name><![CDATA[{key}]]></Name>\n"));
        out.push_str(&format!("{pad}      <Value><![CDATA[{value}]]></Value>\n"));
        out.push_str(&format!("{pad}   </MD>\n"));
    }
    out.push_str(&format!("{pad}</MetaData>\n"));
}

fn write_label_table(labels: &[Label], out: &mut String) {
    if labels.is_empty() {
        out.push_str("   <LabelTable/>\n");
        return;
    }
    out.push_str("   <LabelTable>\n");
    for label in labels {
        let rgba = label
            .rgba
            .map(|c| {
                format!(
                    " Red=\"{}\" Green=\"{}\" Blue=\"{}\" Alpha=\"{}\"",
                    c[0], c[1], c[2], c[3]
                )
            })
            .unwrap_or_default();
        out.push_str(&format!(
            "      <Label Key=\"{}\"{}><![CDATA[{}]]></Label>\n",
            label.key, rgba, label.text
        ));
    }
    out.push_str("   </LabelTable>\n");
}

fn write_data_array(array: &DataArray, out: &mut String) {
    let intent_name = intent::name_for_code(array.intent)
        .map(str::to_string)
        .unwrap_or_else(|| array.intent.to_string());
    out.push_str("   <DataArray");
    out.push_str(&format!(" Intent=\"{intent_name}\""));
    out.push_str(&format!(" DataType=\"{}\"", array.data.dtype().as_name()));
    out.push_str(&format!(
        " ArrayIndexingOrder=\"{}\"",
        array.index_order.as_str()
    ));
    out.push_str(&format!(" Dimensionality=\"{}\"", array.dims.len()));
    for (i, d) in array.dims.iter().enumerate() {
        out.push_str(&format!(" Dim{i}=\"{d}\""));
    }
    // gzip output is not emitted; downgrade to plain Base64.
    let encoding = match array.encoding {
        Encoding::GZipBase64Binary => Encoding::Base64Binary,
        other => other,
    };
    out.push_str(&format!(" Encoding=\"{}\"", encoding.as_str()));
    out.push_str(" Endian=\"LittleEndian\"");
    out.push_str(" ExternalFileName=\"\" ExternalFileOffset=\"\">\n");

    write_meta(&array.meta, out, 2);
    for cs in &array.coordsys {
        write_coord_system(cs, out);
    }

    out.push_str("      <Data>");
    match encoding {
        Encoding::Ascii => out.push_str(&array.data.to_ascii()),
        Encoding::Base64Binary => out.push_str(&crate::base64::encode(&array.data.to_bytes(true))),
        _ => {}
    }
    out.push_str("</Data>\n");
    out.push_str("   </DataArray>\n");
}

fn write_coord_system(cs: &CoordSystem, out: &mut String) {
    out.push_str("      <CoordinateSystemTransformMatrix>\n");
    out.push_str(&format!(
        "         <DataSpace><![CDATA[{}]]></DataSpace>\n",
        cs.data_space
    ));
    out.push_str(&format!(
        "         <TransformedSpace><![CDATA[{}]]></TransformedSpace>\n",
        cs.transformed_space
    ));
    out.push_str("         <MatrixData>\n");
    for row in &cs.xform {
        out.push_str("            ");
        out.push_str(
            &row.iter()
                .map(|v| crate::niml::format_float(*v))
                .collect::<Vec<_>>()
                .join(" "),
        );
        out.push('\n');
    }
    out.push_str("         </MatrixData>\n");
    out.push_str("      </CoordinateSystemTransformMatrix>\n");
}

#[cfg(test)]
mod tests {
    use super::*;

    const ASCII_SURF: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE GIFTI SYSTEM "http://www.nitrc.org/frs/download.php/115/gifti.dtd">
<GIFTI Version="1.0" NumberOfDataArrays="2">
   <MetaData>
      <MD><Name><![CDATA[date]]></Name><Value><![CDATA[today]]></Value></MD>
   </MetaData>
   <LabelTable/>
   <DataArray Intent="NIFTI_INTENT_POINTSET" DataType="NIFTI_TYPE_FLOAT32"
              ArrayIndexingOrder="RowMajorOrder" Dimensionality="2" Dim0="3" Dim1="3"
              Encoding="ASCII" Endian="LittleEndian">
      <Data>0 0 0  1 0 0  0 1 0</Data>
   </DataArray>
   <DataArray Intent="NIFTI_INTENT_TRIANGLE" DataType="NIFTI_TYPE_INT32"
              ArrayIndexingOrder="RowMajorOrder" Dimensionality="2" Dim0="1" Dim1="3"
              Encoding="ASCII" Endian="LittleEndian">
      <Data>0 1 2</Data>
   </DataArray>
</GIFTI>"#;

    #[test]
    fn reads_ascii_surface() {
        let gii = Gifti::parse(ASCII_SURF).unwrap();
        assert_eq!(gii.data_arrays.len(), 2);
        assert_eq!(meta_get(&gii.meta, "date"), Some("today"));
        assert_eq!(gii.pointset().unwrap()[1], [1.0, 0.0, 0.0]);
        assert_eq!(gii.triangles().unwrap()[0], [0, 1, 2]);
        let surf = gii.to_surface().unwrap();
        assert_eq!(surf.n_vertices(), 3);
        assert_eq!(surf.n_faces(), 1);
    }

    #[test]
    fn round_trips_through_base64() {
        let mut gii = Gifti::parse(ASCII_SURF).unwrap();
        for array in &mut gii.data_arrays {
            array.encoding = Encoding::Base64Binary;
        }
        let reparsed = Gifti::parse(&gii.to_xml_string()).unwrap();
        assert_eq!(reparsed.pointset().unwrap(), gii.pointset().unwrap());
        assert_eq!(reparsed.triangles().unwrap(), gii.triangles().unwrap());
    }
}
