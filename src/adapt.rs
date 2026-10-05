// PUBLIC DOMAIN NOTICE
//
// This file is part of afni-io, which was written by employees of the United
// States Government (National Institutes of Health) as part of their official
// duties. It is a "United States Government Work" (17 U.S.C. 105) and is in the
// public domain; outside the US, rights are waived under CC0 1.0. See LICENSE.
//
// ---------------------------------------------------------------------------
// WHAT THIS FILE IS
//
// Adapters between this crate's raw file types and `afni-core`'s format-neutral
// `Dataset`:
//
//     NimlDataset  <-->  NimlEnvelope { core: Dataset, extras }
//     Gifti        ---->  Dataset
//     Volume       ---->  Dataset
//     OneD         ---->  Dataset      (caller supplies the domain)
//     NodeRoi      <-->  RoiEnvelope { core: Roi, extras }
//     GraphBucket  <-->  afni_core::graph::Graph   (a template keeps the file's extras)
//     TractNetwork <-->  afni_core::tract::TractSet
//
// HOW IT RELATES TO THE REST OF THE CRATE
//
// * This is the ONLY module that knows about both worlds. The dependency
//   points `afni-io -> afni-core`; `afni-core` never sees NIML/HEAD syntax.
// * Raw types (`NimlDataset`, `Header`, ...) keep every attribute so files
//   round-trip. A core `Dataset` keeps only what it understands. Anything it
//   does not understand travels in `NimlExtras`, bundled with the core dataset
//   in a `NimlEnvelope`, so writing back loses nothing.
//
// POLICIES
//
// * A domain is never guessed. A dense NIML dataset's node count is its row
//   count; a SPARSE one cannot say how many nodes the surface has, so the
//   caller must pass `node_count`.
// * Malformed metadata (a bad FDR curve, duplicate label keys, an out-of-domain
//   index) is an error, not a silent drop.
// * Integer data stays integer (narrower ints widen losslessly; a `u64` above
//   `i64::MAX` is rejected). Float data keeps its width.
// ---------------------------------------------------------------------------

//! Adapters from file types to [`afni_core::dataset::Dataset`].

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use afni_core::column::{ColumnData, ColumnRange, ColumnRole, DataColumn, RecordedRange};
use afni_core::curve::ThresholdCurve as CoreCurve;
use afni_core::dataset::{Dataset, DatasetKind, ParentIds};
use afni_core::domain::{Domain, DomainId, SurfaceDomain, VolumeDomain};
use afni_core::labels::{LabelEntry as CoreEntry, LabelTable as CoreTable};
use afni_core::mapping::SampleMap;
use afni_core::roi::{Roi as CoreRoi, RoiStroke as CoreStroke};
use afni_core::stat::{IntentOrigin, StatSpec};

use crate::array::{DataType, TypedArray};
use crate::brik::{AfniPaths, Brik, BrikBuilder, BrikWriteOptions, StoragePolicy};
use crate::dset::NimlDataset;
use crate::error::{Error, Result};
use crate::geometry::{dicom_to_ras, TimeAxis, TimeUnits};
use crate::gifti::{self, Gifti};
use crate::graph::{GraphBucket, MatrixShape, NodeRow};
use crate::head::{AttributeValue, Header};
use crate::labels::{LabelEntry, LabelTable};
use crate::nifti::{Nifti, NiftiHeader, NiftiVersion, NiftiWriteOptions};
use crate::niml::{NimlElement, NumericMatrix};
use crate::onedee::OneD;
use crate::roi::{NodeRoi, RoiDatum};
use crate::stat::ThresholdCurve;
use crate::surface::Surface;
use crate::tract::{RawBundle, RawTract, TractNetwork};
use crate::volume::{GridSpec, Volume};

/// Convert a core error into this crate's error type.
fn core_err(e: afni_core::Error) -> Error {
    Error::invalid(e.to_string())
}

// ---------------------------------------------------------------------------
// Small conversions shared by every adapter
// ---------------------------------------------------------------------------

/// Raw typed numbers -> core column values.
///
/// Small integers widen to `i32` (lossless). `u64` becomes `i64` if it fits.
fn typed_to_column(array: &TypedArray) -> Result<ColumnData> {
    Ok(match array {
        TypedArray::UInt8(v) => ColumnData::Int32(v.iter().map(|&x| i32::from(x)).collect()),
        TypedArray::Int8(v) => ColumnData::Int32(v.iter().map(|&x| i32::from(x)).collect()),
        TypedArray::UInt16(v) => ColumnData::Int32(v.iter().map(|&x| i32::from(x)).collect()),
        TypedArray::Int16(v) => ColumnData::Int32(v.iter().map(|&x| i32::from(x)).collect()),
        TypedArray::Int32(v) => ColumnData::Int32(v.clone()),
        TypedArray::UInt32(v) => ColumnData::UInt32(v.clone()),
        TypedArray::Int64(v) => ColumnData::Int64(v.clone()),
        TypedArray::UInt64(v) => ColumnData::Int64(
            v.iter()
                .map(|&x| {
                    i64::try_from(x)
                        .map_err(|_| Error::invalid(format!("u64 value {x} exceeds i64::MAX")))
                })
                .collect::<Result<_>>()?,
        ),
        TypedArray::Float32(v) => ColumnData::Float32(v.clone()),
        TypedArray::Float64(v) => ColumnData::Float64(v.clone()),
    })
}

/// Core column values -> raw typed numbers (for writing). Text has no numeric
/// NIML form, so it is reported as unsupported rather than mangled.
fn column_to_typed(data: &ColumnData) -> Result<TypedArray> {
    Ok(match data {
        ColumnData::Int32(v) => TypedArray::Int32(v.clone()),
        ColumnData::UInt32(v) => TypedArray::UInt32(v.clone()),
        ColumnData::Int64(v) => TypedArray::Int64(v.clone()),
        ColumnData::Float32(v) => TypedArray::Float32(v.clone()),
        ColumnData::Float64(v) => TypedArray::Float64(v.clone()),
        ColumnData::Text(_) => {
            return Err(Error::unsupported(
                "text columns cannot be written to a numeric NIML dataset",
            ))
        }
    })
}

/// Raw (unvalidated) threshold curve -> validated core curve.
fn curve_to_core(raw: &ThresholdCurve) -> Result<CoreCurve> {
    CoreCurve::new(raw.x0, raw.dx, raw.values.clone()).map_err(core_err)
}

/// What to do when a piece of metadata is malformed: an FDR/MDF curve (zero
/// spacing, fewer than two samples, non-finite values) or a statistic (for
/// example a correlation whose parameters fit neither AFNI's nor the NIfTI
/// standard's convention).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MetadataPolicy {
    /// Fail the whole conversion with an error. The default, and what the
    /// plain `*_to_core` functions do.
    #[default]
    Strict,
    /// Leave that metadata off and record an [`AdaptWarning`], so the data stays
    /// usable (for example in a viewer) while the problem is visible.
    SkipWithWarning,
}

/// Former name of [`MetadataPolicy`], from when it covered only curves.
pub type CurvePolicy = MetadataPolicy;

/// Options for the `*_to_core_with` adapters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct AdaptOptions {
    /// How malformed FDR/MDF curves are handled.
    pub curves: MetadataPolicy,
    /// How a malformed statistic is handled (see [`MetadataPolicy`]).
    pub statistics: MetadataPolicy,
    /// Who wrote NIfTI/GIfTI `intent_p1..3`. Only correlation depends on it.
    /// [`IntentOrigin::Unknown`] (the default) classifies by structure, which is
    /// correct for every file AFNI or a standards-following tool writes; set
    /// `Afni` or `Standard` to override when the producer is known.
    pub intent_origin: IntentOrigin,
}

/// Something that was dropped or altered during conversion without failing it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdaptWarning {
    /// Index of the column (or sub-brick) concerned.
    pub column: usize,
    /// What was affected, e.g. `"FDR curve"`.
    pub what: &'static str,
    /// Why, in plain words.
    pub message: String,
}

/// Convert an optional raw curve under `policy`, pushing a warning if it is
/// skipped. `Ok(None)` means "no curve" (absent, or skipped with a warning).
fn curve_with_policy(
    raw: Option<ThresholdCurve>,
    policy: MetadataPolicy,
    column: usize,
    what: &'static str,
    warnings: &mut Vec<AdaptWarning>,
) -> Result<Option<CoreCurve>> {
    let Some(raw) = raw else { return Ok(None) };
    match (curve_to_core(&raw), policy) {
        (Ok(curve), _) => Ok(Some(curve)),
        (Err(e), MetadataPolicy::Strict) => Err(e),
        (Err(e), MetadataPolicy::SkipWithWarning) => {
            warnings.push(AdaptWarning {
                column,
                what,
                message: e.to_string(),
            });
            Ok(None)
        }
    }
}

/// Validated core curve -> the raw attribute layout `[x0, dx, v0, v1, ...]`.
fn curve_to_attr(curve: &CoreCurve) -> AttributeValue {
    let mut v = vec![curve.x0(), curve.dx()];
    v.extend_from_slice(curve.samples());
    AttributeValue::Float(v)
}

/// Convert a raw label table into a core one, keeping integer keys and the
/// file's order, and each color exactly as stored (colors read from 8-bit data
/// stay `value / 255`; nothing is re-quantized). Fails if two entries share a
/// key, since "what is label 7?" would then have no single answer.
pub fn label_table_to_core(raw: &LabelTable) -> Result<CoreTable> {
    labels_to_core(raw)
}

/// Raw label table -> core label table (validates unique keys).
fn labels_to_core(raw: &LabelTable) -> Result<CoreTable> {
    CoreTable::new(
        raw.entries
            .iter()
            .map(|e| CoreEntry {
                key: e.key,
                name: e.name.clone(),
                rgba: e.rgba,
            })
            .collect(),
    )
    .map_err(core_err)
}

/// Convert a core label table back to a raw one for writing, carrying `attrs`
/// (the table element's extra attributes) over from an earlier raw table, or an
/// empty map for a new one.
pub fn label_table_from_core(core: &CoreTable, attrs: BTreeMap<String, String>) -> LabelTable {
    labels_to_raw(core, attrs)
}

/// Core label table -> raw table, keeping `attrs` from an earlier raw table.
fn labels_to_raw(core: &CoreTable, attrs: BTreeMap<String, String>) -> LabelTable {
    LabelTable {
        entries: core
            .entries()
            .iter()
            .map(|e| LabelEntry {
                key: e.key,
                name: e.name.clone(),
                rgba: e.rgba,
            })
            .collect(),
        attrs,
    }
}

/// An id string -> `Option<DomainId>`; blank means "no id".
fn domain_id(text: Option<&str>) -> Option<DomainId> {
    text.and_then(|t| DomainId::new(t).ok())
}

// ---------------------------------------------------------------------------
// NIML surface datasets
// ---------------------------------------------------------------------------

/// What the core `Dataset` does not model, kept so a NIML dataset can be
/// written back without losing attributes.
///
/// This is the "I/O-side envelope" from the roadmap: format syntax stays here,
/// never in `afni-core`.
#[derive(Debug, Clone, PartialEq)]
pub struct NimlExtras {
    /// The original `dset_type` attribute (e.g. `Node_Bucket`).
    pub dset_type: String,
    /// The `filename` attribute.
    pub filename: Option<String>,
    /// The `label` attribute.
    pub label: Option<String>,
    /// Other attributes of the `AFNI_dataset` element.
    pub other_attrs: BTreeMap<String, String>,
    /// Every `AFNI_atr` attribute as read (including ones core also models,
    /// such as `COLMS_LABS`; the modelled ones are overwritten on write from
    /// the core dataset, all others are kept).
    pub attributes: Header,
    /// The raw label table, kept for its extra attributes (`pbar_name`, ...).
    pub label_table: Option<LabelTable>,
    /// Child elements this crate does not interpret.
    pub other_elements: Vec<NimlElement>,
}

/// A core dataset together with the raw extras of the file it came from.
#[derive(Debug, Clone, PartialEq)]
pub struct NimlEnvelope {
    /// The format-neutral dataset; edit this.
    pub core: Dataset,
    /// Everything else, preserved for writing back.
    pub extras: NimlExtras,
}

/// The core role for a SUMA `COLMS_TYPE` string.
fn role_from_suma_type(t: &str) -> ColumnRole {
    match t.trim() {
        "Node_Index" => ColumnRole::NodeIndex,
        "Node_Index_Label" => ColumnRole::Label,
        "Generic_Float" | "Generic_Int" | "" => ColumnRole::Generic,
        other => ColumnRole::Other(other.to_owned()),
    }
}

/// The `COLMS_TYPE` string to write for a role. Roles with no SUMA name fall
/// back to the generic type matching the data, as SUMA itself does.
fn suma_type_for(role: &ColumnRole, data: &ColumnData) -> String {
    match role {
        ColumnRole::NodeIndex => "Node_Index".into(),
        ColumnRole::Label => "Node_Index_Label".into(),
        ColumnRole::Other(raw) => raw.clone(),
        _ => match data {
            ColumnData::Float32(_) | ColumnData::Float64(_) => "Generic_Float".into(),
            _ => "Generic_Int".into(),
        },
    }
}

/// Convert a `.niml.dset` into a core dataset plus extras.
///
/// `node_count` is the number of nodes in the surface. It may be `None` only
/// for a dense dataset (the row count is then the node count); a sparse
/// dataset needs it because the file does not record it. If both are known and
/// disagree, that is an error.
///
/// Malformed FDR/MDF curves are an error here; use [`niml_to_core_with`] to
/// skip them with a warning instead.
pub fn niml_to_core(dset: &NimlDataset, node_count: Option<usize>) -> Result<NimlEnvelope> {
    niml_to_core_with(dset, node_count, &AdaptOptions::default()).map(|(env, _)| env)
}

/// Like [`niml_to_core`], with options, also returning any warnings.
pub fn niml_to_core_with(
    dset: &NimlDataset,
    node_count: Option<usize>,
    options: &AdaptOptions,
) -> Result<(NimlEnvelope, Vec<AdaptWarning>)> {
    let mut warnings = Vec::new();
    // --- Domain and row mapping ------------------------------------------
    let nodes =
        match (dset.is_sparse(), node_count) {
            (false, None) => dset.rows(),
            (false, Some(n)) if n == dset.rows() => n,
            (false, Some(n)) => {
                return Err(Error::invalid(format!(
                    "dense dataset has {} rows but the surface has {n} nodes",
                    dset.rows()
                )))
            }
            (true, Some(n)) => n,
            (true, None) => return Err(Error::invalid(
                "a sparse dataset needs `node_count`: the file does not record the surface size",
            )),
        };
    let domain = Domain::Surface(
        SurfaceDomain::new(domain_id(dset.domain_parent_idcode.as_deref()), nodes)
            .map_err(core_err)?,
    );
    let map = match &dset.node_indices {
        Some(idx) => SampleMap::indexed(idx.clone(), nodes).map_err(core_err)?,
        None => SampleMap::dense(dset.rows()),
    };

    // --- Columns -----------------------------------------------------------
    let types = dset.column_types();
    let stats = dset.column_stats();
    let ranges = dset.column_ranges();
    let core_table = dset.label_table.as_ref().map(labels_to_core).transpose()?;

    let mut columns = Vec::with_capacity(dset.column_count());
    for (c, array) in dset.data.columns.iter().enumerate() {
        let role = role_from_suma_type(&types[c]);
        let recorded = ranges[c].and_then(|r| {
            // A recorded range that is not a valid interval (e.g. AFNI's
            // "0 0 -1 -1" placeholder is fine; NaN is not) cannot be modelled.
            let range = ColumnRange::new(r.min, r.max).ok()?;
            Some(RecordedRange {
                range,
                min_sample: u32::try_from(r.min_node).ok(),
                max_sample: u32::try_from(r.max_node).ok(),
            })
        });
        let mut column =
            DataColumn::new(dset.column_label(c), role.clone(), typed_to_column(array)?)
                .map_err(core_err)?
                .with_stat(stats[c].clone())
                .with_recorded_range(recorded)
                .with_fdr_curve(curve_with_policy(
                    dset.attributes.fdr_curve(c),
                    options.curves,
                    c,
                    "FDR curve",
                    &mut warnings,
                )?)
                .with_mdf_curve(curve_with_policy(
                    dset.attributes.mdf_curve(c),
                    options.curves,
                    c,
                    "MDF curve",
                    &mut warnings,
                )?);
        // The label table describes every label-role column.
        if role == ColumnRole::Label {
            column = column.with_label_table(core_table.clone());
        }
        columns.push(column);
    }

    // --- Dataset-level ----------------------------------------------------
    let kind = match dset.dset_type.as_str() {
        "Node_Label" => DatasetKind::Label,
        "Node_ROI" => DatasetKind::Roi,
        "Node_Bucket" if dset.time_step.is_some() => DatasetKind::TimeSeries,
        "Node_Bucket" => DatasetKind::Scalar,
        other => DatasetKind::Other(other.to_owned()),
    };
    let core = Dataset::new(kind, domain, map, columns)
        .map_err(core_err)?
        .with_time_step_seconds(dset.time_step)
        .map_err(core_err)?
        .with_parent_ids(ParentIds {
            self_id: dset.self_idcode.clone(),
            domain_parent: dset.domain_parent_idcode.clone(),
            geometry_parent: dset.geometry_parent_idcode.clone(),
        });

    let extras = NimlExtras {
        dset_type: dset.dset_type.clone(),
        filename: dset.filename.clone(),
        label: dset.label.clone(),
        other_attrs: dset.other_attrs.clone(),
        attributes: dset.attributes.clone(),
        label_table: dset.label_table.clone(),
        other_elements: dset.other_elements.clone(),
    };
    Ok((NimlEnvelope { core, extras }, warnings))
}

impl NimlEnvelope {
    /// Wrap a core dataset that did not come from a file, with default extras.
    pub fn from_core(core: Dataset) -> Self {
        let dset_type = match core.kind() {
            DatasetKind::Label => "Node_Label",
            DatasetKind::Roi => "Node_ROI",
            DatasetKind::Other(s) => s.as_str(),
            _ => "Node_Bucket",
        }
        .to_owned();
        Self {
            core,
            extras: NimlExtras {
                dset_type,
                filename: None,
                label: None,
                other_attrs: BTreeMap::new(),
                attributes: Header::default(),
                label_table: None,
                other_elements: Vec::new(),
            },
        }
    }

    /// Build the raw `NimlDataset` to write.
    ///
    /// Values, node indices, labels, types, statistics, curves, label table,
    /// time step and parent ids come from the core dataset; every other raw
    /// attribute comes from [`NimlExtras`]. `COLMS_RANGE` is recomputed from
    /// the data by the NIML writer (see [`NimlDataset::to_element`]).
    pub fn to_niml(&self) -> Result<NimlDataset> {
        let core = &self.core;
        let columns = core
            .columns()
            .iter()
            .map(|c| column_to_typed(c.values()))
            .collect::<Result<Vec<_>>>()?;
        let data = NumericMatrix {
            rows: core.row_count(),
            columns,
        };
        let indices = match core.map() {
            SampleMap::Dense { .. } => None,
            SampleMap::Indexed { indices } => Some(indices.clone()),
        };

        let mut out = NimlDataset::new(self.extras.dset_type.clone(), data, indices);
        out.self_idcode = core.parent_ids().self_id.clone();
        out.domain_parent_idcode = core.parent_ids().domain_parent.clone();
        out.geometry_parent_idcode = core.parent_ids().geometry_parent.clone();
        out.filename = self.extras.filename.clone();
        out.label = self.extras.label.clone();
        out.other_attrs = self.extras.other_attrs.clone();
        out.time_step = core.time_step_seconds();
        out.attributes = self.extras.attributes.clone();
        out.other_elements = self.extras.other_elements.clone();

        // Column metadata owned by the core model.
        let names: Vec<&str> = core.columns().iter().map(|c| c.label()).collect();
        out.set_column_labels(&names)?;
        // Keep the file's original COLMS_TYPE text where the role still means
        // the same thing; otherwise derive it from the role.
        let original = self.extras.attributes_column_types();
        let types: Vec<String> = core
            .columns()
            .iter()
            .enumerate()
            .map(|(i, c)| match original.get(i) {
                Some(raw) if role_from_suma_type(raw) == *c.role() && !raw.is_empty() => {
                    raw.clone()
                }
                _ => suma_type_for(c.role(), c.values()),
            })
            .collect();
        out.set_column_types(&types)?;
        let stats: Vec<_> = core.columns().iter().map(|c| c.stat().cloned()).collect();
        out.set_column_stats(&stats);
        for (i, c) in core.columns().iter().enumerate() {
            if let Some(curve) = c.fdr_curve() {
                out.attributes
                    .set(format!("FDRCURVE_{i:06}"), curve_to_attr(curve));
            }
            if let Some(curve) = c.mdf_curve() {
                out.attributes
                    .set(format!("MDFCURVE_{i:06}"), curve_to_attr(curve));
            }
        }
        // The label table: core's version wins; the raw table contributes its
        // extra attributes.
        let raw_attrs = self.extras.label_table.as_ref().map(|t| t.attrs.clone());
        let core_table = core.columns().iter().find_map(|c| c.label_table());
        out.label_table = match (core_table, &self.extras.label_table) {
            (Some(t), _) => Some(labels_to_raw(t, raw_attrs.unwrap_or_default())),
            (None, raw) => raw.clone(),
        };
        Ok(out)
    }
}

impl NimlExtras {
    /// The `COLMS_TYPE` entries of the original file, positionally.
    fn attributes_column_types(&self) -> Vec<String> {
        self.attributes
            .string("COLMS_TYPE")
            .map(|s| {
                s.strip_suffix(';')
                    .unwrap_or(s)
                    .split(';')
                    .map(|t| t.trim().to_owned())
                    .collect()
            })
            .unwrap_or_default()
    }
}

// ---------------------------------------------------------------------------
// GIfTI
// ---------------------------------------------------------------------------

/// Convert the per-node data arrays of a GIfTI file into a dense core dataset.
///
/// Geometry arrays (`POINTSET`, `TRIANGLE`) are skipped. A `NODE_INDEX` array
/// makes the dataset sparse, and then `node_count` is required (the file does
/// not record the surface size); otherwise the dataset is dense and
/// `node_count`, if given, must equal the array length.
/// Every other array must be one-dimensional (`n` or `n x 1`) with the same
/// `n`, which becomes the node count. Arrays with a statistical intent get a
/// [`afni_core::stat::StatSpec`] taken from `intent_p1..3` exactly as
/// [`gifti::DataArray::stat`] does (note the AFNI-versus-NIfTI correlation
/// ambiguity recorded in the afni-core roadmap).
///
/// A statistic whose parameters cannot be read (for example a correlation fitting
/// neither convention) is an error here; use [`gifti_to_core_with`] to skip it
/// with a warning or to state who wrote the parameters.
pub fn gifti_to_core(gii: &Gifti, node_count: Option<usize>) -> Result<Dataset> {
    gifti_to_core_with(gii, node_count, &AdaptOptions::default()).map(|(ds, _)| ds)
}

/// Like [`gifti_to_core`], with options, also returning any warnings.
pub fn gifti_to_core_with(
    gii: &Gifti,
    node_count: Option<usize>,
    options: &AdaptOptions,
) -> Result<(Dataset, Vec<AdaptWarning>)> {
    let mut warnings = Vec::new();
    let data_arrays: Vec<&gifti::DataArray> = gii
        .data_arrays
        .iter()
        .filter(|a| {
            !matches!(
                a.intent,
                gifti::intent::POINTSET | gifti::intent::TRIANGLE | gifti::intent::NODE_INDEX
            )
        })
        .collect();
    let first = data_arrays
        .first()
        .ok_or_else(|| Error::missing("GIfTI data arrays (only geometry found)"))?;
    let nodes = first.dims.first().copied().unwrap_or(0);

    // The label table of a .label.gii, shared by all label arrays.
    let table = if gii.label_table.is_empty() {
        None
    } else {
        Some(
            CoreTable::new(
                gii.label_table
                    .iter()
                    .map(|l| CoreEntry {
                        key: i64::from(l.key),
                        name: l.text.clone(),
                        rgba: l.rgba,
                    })
                    .collect(),
            )
            .map_err(core_err)?,
        )
    };

    let mut columns = Vec::new();
    for (i, a) in data_arrays.iter().enumerate() {
        let one_d = matches!(a.dims.as_slice(), [_] | [_, 1]);
        if !one_d || a.dims[0] != nodes {
            return Err(Error::unsupported(format!(
                "GIfTI array {i} has dims {:?}; expected a vector of {nodes} values",
                a.dims
            )));
        }
        let is_label = a.intent == gifti::intent::LABEL;
        let role = if is_label {
            ColumnRole::Label
        } else {
            ColumnRole::Generic
        };
        let name = gifti::meta_get(&a.meta, "Name")
            .filter(|n| !n.trim().is_empty())
            .map_or_else(|| format!("col_{i}"), str::to_owned);
        let stat = match a.stat_with_origin(options.intent_origin) {
            Ok(stat) => stat,
            Err(e) if options.statistics == MetadataPolicy::SkipWithWarning => {
                warnings.push(AdaptWarning {
                    column: i,
                    what: "statistic",
                    message: e.to_string(),
                });
                None
            }
            Err(e) => return Err(e),
        };
        let mut c = DataColumn::new(name, role, typed_to_column(&a.data)?)
            .map_err(core_err)?
            .with_stat(stat);
        if is_label {
            c = c.with_label_table(table.clone());
        }
        columns.push(c);
    }
    let kind = if columns.iter().any(|c| *c.role() == ColumnRole::Label) {
        DatasetKind::Label
    } else {
        DatasetKind::Scalar
    };
    // A NODE_INDEX array lists the node of each row (sparse data).
    let index_array = gii
        .data_arrays
        .iter()
        .find(|a| a.intent == gifti::intent::NODE_INDEX);
    let (total, map) = match (index_array, node_count) {
        (Some(a), Some(n)) => {
            let indices = a
                .data
                .to_f64_vec()
                .into_iter()
                .map(|x| {
                    // Exact, in-range integers only; no silent truncation.
                    (x.fract() == 0.0 && (0.0..=f64::from(u32::MAX)).contains(&x))
                        .then_some(x as u32)
                        .ok_or_else(|| Error::invalid(format!("bad node index {x}")))
                })
                .collect::<Result<Vec<u32>>>()?;
            (n, SampleMap::indexed(indices, n).map_err(core_err)?)
        }
        (Some(_), None) => return Err(Error::invalid(
            "a sparse GIfTI dataset needs `node_count`: the file does not record the surface size",
        )),
        (None, Some(n)) if n != nodes => {
            return Err(Error::invalid(format!(
                "dense GIfTI data has {nodes} values but the surface has {n} nodes"
            )))
        }
        (None, _) => (nodes, SampleMap::dense(nodes)),
    };
    let domain = Domain::Surface(SurfaceDomain::new(None, total).map_err(core_err)?);
    let ds = Dataset::new(kind, domain, map, columns).map_err(core_err)?;
    Ok((ds, warnings))
}

// ---------------------------------------------------------------------------
// Volumes
// ---------------------------------------------------------------------------

/// A file-neutral volume dataset together with an optional source AFNI header.
///
/// The header preserves history and private attributes while the core dataset
/// is processed. [`VolumeEnvelope::to_brik`] and [`VolumeEnvelope::to_nifti`]
/// regenerate structural and per-frame attributes so stale storage metadata
/// is not copied.
#[derive(Debug, Clone)]
pub struct VolumeEnvelope {
    /// File-neutral values and semantic metadata.
    pub dataset: Dataset,
    /// Original AFNI attributes, when the source had them.
    pub source_header: Option<Header>,
}

/// Disk format selected by the format-neutral volume writer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VolumeOutputFormat {
    /// AFNI `.HEAD` plus `.BRIK` or `.BRIK.gz`.
    Afni,
    /// Single-file NIfTI `.nii` or `.nii.gz`.
    Nifti,
}

impl VolumeOutputFormat {
    fn from_path(path: &Path) -> Result<Self> {
        let text = path
            .to_str()
            .ok_or_else(|| Error::invalid("non-UTF-8 volume output path"))?;
        if text.ends_with(".nii") || text.ends_with(".nii.gz") {
            return Ok(Self::Nifti);
        }
        if text.ends_with(".hdr") || text.ends_with(".img") {
            return Err(Error::unsupported(
                "detached NIfTI output (.hdr/.img) is not supported; use .nii or .nii.gz",
            ));
        }
        if AfniPaths::is_afni_name(path) || path.extension().is_none() {
            return Ok(Self::Afni);
        }
        Err(Error::invalid(format!(
            "cannot infer volume output format from {}; use .nii/.nii.gz, an AFNI +view name, or set VolumeWriteOptions::format",
            path.display()
        )))
    }
}

/// Policy shared by AFNI and NIfTI output.
#[derive(Debug, Clone, PartialEq)]
pub struct VolumeWriteOptions {
    /// Explicit format, or `None` to infer it from the destination name.
    pub format: Option<VolumeOutputFormat>,
    /// Whether existing output files may be replaced.
    pub overwrite: bool,
    /// Optional command line appended to the AFNI `HISTORY_NOTE`. NIfTI output
    /// carries the same history in its AFNI header extension.
    pub history_entry: Option<String>,
    /// Storage policy for AFNI BRIKs. NIfTI output is always unscaled `f32`.
    pub afni_storage: StoragePolicy,
    /// Header version for NIfTI output.
    pub nifti_version: NiftiVersion,
}

impl Default for VolumeWriteOptions {
    fn default() -> Self {
        Self {
            format: None,
            overwrite: true,
            history_entry: None,
            afni_storage: StoragePolicy::Float,
            nifti_version: NiftiVersion::Nifti1,
        }
    }
}

/// Files produced by a format-neutral volume write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WrittenVolume {
    /// The two AFNI dataset paths.
    Afni(AfniPaths),
    /// The single NIfTI file.
    Nifti(PathBuf),
}

impl WrittenVolume {
    /// Format that was written.
    pub fn format(&self) -> VolumeOutputFormat {
        match self {
            Self::Afni(_) => VolumeOutputFormat::Afni,
            Self::Nifti(_) => VolumeOutputFormat::Nifti,
        }
    }
}

/// Builder for a new file-neutral volume dataset.
///
/// Geometry is supplied in the common voxel-to-RAS convention. Frames are
/// true `f32` values; AFNI storage scaling is chosen only when the finished
/// envelope is written. This lets a command construct its result before it
/// knows whether the user requested HEAD/BRIK or NIfTI output.
#[derive(Debug, Clone)]
pub struct VolumeBuilder {
    grid: GridSpec,
    frames: Vec<Vec<f32>>,
    labels: Option<Vec<String>>,
    stats: Option<Vec<Option<StatSpec>>>,
    time_step_seconds: Option<f64>,
    time_start_seconds: Option<f64>,
    history: Option<String>,
    source_header: Option<Header>,
}

impl VolumeBuilder {
    /// Start an empty output on a validated RAS grid.
    pub fn new(grid: GridSpec) -> Result<Self> {
        VolumeDomain::new(None, grid.dimensions, Some(grid.ijk_to_ras)).map_err(core_err)?;
        Ok(Self {
            grid,
            frames: Vec::new(),
            labels: None,
            stats: None,
            time_step_seconds: None,
            time_start_seconds: None,
            history: None,
            source_header: None,
        })
    }

    /// Start an empty output on a loaded volume's exact grid, retaining its
    /// AFNI/private attributes for a later format-neutral round trip. Values
    /// and per-frame metadata are deliberately not copied.
    pub fn like(source: &Volume) -> Result<Self> {
        let mut builder = Self::new(source.grid()?)?;
        builder.source_header = source.afni_header()?;
        Ok(builder)
    }

    /// Append one frame of true values in i-fastest voxel order.
    pub fn values(mut self, values: Vec<f32>) -> Result<Self> {
        if values.len() != self.grid.voxels() {
            return Err(Error::invalid(format!(
                "frame has {} voxels but grid has {}",
                values.len(),
                self.grid.voxels()
            )));
        }
        self.frames.push(values);
        Ok(self)
    }

    /// Set one label per frame. The count is checked by [`Self::build`].
    pub fn labels<S: Into<String>>(mut self, labels: impl IntoIterator<Item = S>) -> Self {
        self.labels = Some(labels.into_iter().map(Into::into).collect());
        self
    }

    /// Set one optional statistic per frame. The count is checked by
    /// [`Self::build`].
    pub fn stats(mut self, stats: impl IntoIterator<Item = Option<StatSpec>>) -> Self {
        self.stats = Some(stats.into_iter().collect());
        self
    }

    /// Mark frames as a regularly sampled time series, in seconds.
    pub fn time_axis_seconds(mut self, step: f64, start: f64) -> Self {
        self.time_step_seconds = Some(step);
        self.time_start_seconds = Some(start);
        self
    }

    /// Set the complete `HISTORY_NOTE`. A command line supplied to
    /// [`VolumeWriteOptions`] is appended when the result is written.
    pub fn history(mut self, history: impl Into<String>) -> Self {
        self.history = Some(history.into());
        self
    }

    /// Retain a template AFNI header's private attributes. Structural and
    /// per-frame fields are regenerated from this builder when written.
    pub fn source_header(mut self, header: Header) -> Self {
        self.source_header = Some(header);
        self
    }

    /// Validate the frames and metadata and build a file-neutral envelope.
    pub fn build(self) -> Result<VolumeEnvelope> {
        if self.frames.is_empty() {
            return Err(Error::invalid("a volume output needs at least one frame"));
        }
        let frame_count = self.frames.len();
        let labels = match self.labels {
            Some(labels) if labels.len() != frame_count => {
                return Err(Error::invalid(format!(
                    "got {} labels for {frame_count} frames",
                    labels.len()
                )))
            }
            Some(labels) => labels,
            None => (0..frame_count).map(|frame| format!("#{frame}")).collect(),
        };
        if labels
            .iter()
            .any(|label| label.contains('\0') || label.contains('~'))
        {
            return Err(Error::invalid("volume labels cannot contain NUL or '~'"));
        }
        let stats = match self.stats {
            Some(stats) if stats.len() != frame_count => {
                return Err(Error::invalid(format!(
                    "got {} statistics for {frame_count} frames",
                    stats.len()
                )))
            }
            Some(stats) => stats,
            None => vec![None; frame_count],
        };
        let is_time_series = self.time_step_seconds.is_some();
        let columns = self
            .frames
            .into_iter()
            .zip(labels)
            .zip(stats)
            .map(|((values, label), stat)| {
                let role = if stat.is_some() {
                    ColumnRole::Statistic
                } else if is_time_series {
                    ColumnRole::TimePoint
                } else {
                    ColumnRole::Generic
                };
                DataColumn::new(label, role, ColumnData::Float32(values))
                    .map_err(core_err)
                    .map(|column| column.with_stat(stat))
            })
            .collect::<Result<Vec<_>>>()?;
        let id = self
            .source_header
            .as_ref()
            .and_then(|header| domain_id(header.idcode()));
        let domain = Domain::Volume(
            VolumeDomain::new(id, self.grid.dimensions, Some(self.grid.ijk_to_ras))
                .map_err(core_err)?,
        );
        let kind = if is_time_series {
            DatasetKind::TimeSeries
        } else {
            DatasetKind::Scalar
        };
        let mut dataset = Dataset::dense(kind, domain, columns).map_err(core_err)?;
        dataset = dataset
            .with_time_step_seconds(self.time_step_seconds)
            .map_err(core_err)?;
        dataset = dataset
            .with_time_start_seconds(self.time_start_seconds)
            .map_err(core_err)?;

        let mut source_header = self.source_header;
        if let Some(history) = self.history {
            source_header
                .get_or_insert_with(Header::default)
                .set_history(history);
        }
        Ok(VolumeEnvelope {
            dataset,
            source_header,
        })
    }
}

impl VolumeEnvelope {
    /// Wrap a core dataset that did not come from a file.
    pub fn from_core(dataset: Dataset) -> Self {
        Self {
            dataset,
            source_header: None,
        }
    }

    /// Convert the core volume back into an AFNI HEAD/BRIK dataset.
    pub fn to_brik(&self, storage: StoragePolicy) -> Result<Brik> {
        let Domain::Volume(domain) = self.dataset.domain() else {
            return Err(Error::invalid(
                "only a volume-domain dataset can become a BRIK",
            ));
        };
        if self.dataset.is_sparse() {
            return Err(Error::unsupported(
                "a sparse volume dataset must be made dense before BRIK output",
            ));
        }
        let ras = *domain
            .affine()
            .ok_or_else(|| Error::missing("volume-domain affine"))?;
        // DICOM<->RAS negates the first two rows and is its own inverse.
        let dicom = dicom_to_ras(&ras);
        let mut builder = BrikBuilder::affine(domain.dims(), dicom)?;
        let mut labels = Vec::with_capacity(self.dataset.columns().len());
        for column in self.dataset.columns() {
            if !column.values().is_numeric() {
                return Err(Error::unsupported(format!(
                    "text column {:?} cannot be written as a volume",
                    column.label()
                )));
            }
            let values = (0..column.len())
                .map(|row| {
                    column
                        .values()
                        .get_f64(row)
                        .map(|value| value as f32)
                        .ok_or_else(|| {
                            Error::invalid(format!(
                                "column {:?} has no numeric value at row {row}",
                                column.label()
                            ))
                        })
                })
                .collect::<Result<Vec<_>>>()?;
            builder = builder.values(values, storage)?;
            labels.push(column.label().to_owned());
        }
        builder = builder.labels(labels);
        if let Some(step) = self.dataset.time_step_seconds() {
            builder = builder.time_axis(TimeAxis {
                nt: self.dataset.columns().len(),
                origin: self.dataset.time_start_seconds().unwrap_or(0.0),
                step,
                duration: 0.0,
                stored_units: TimeUnits::Seconds,
                slice_offsets: Vec::new(),
                slice_z_origin: 0.0,
                slice_dz: 0.0,
            });
        }
        let mut brik = builder.build()?;

        if let Some(source) = &self.source_header {
            let generated = brik.header.clone();
            let mut header = source.clone();
            // Values, statistics, labels, curves and timing describe the old
            // sub-bricks. Preserve unrelated/private attributes, then overlay
            // the newly generated structural metadata below.
            header.attributes.retain(|attribute| {
                let name = attribute.name.as_str();
                !name.starts_with("BRICK_")
                    && !name.starts_with("FDRCURVE_")
                    && !name.starts_with("MDFCURVE_")
                    && name != "VALUE_LABEL_DTABLE"
                    && !matches!(name, "TAXIS_NUMS" | "TAXIS_FLOATS" | "TAXIS_OFFSETS")
            });
            for attribute in generated.attributes {
                header.set(attribute.name, attribute.value);
            }
            brik.header = header;
        }

        let stats: Vec<Option<afni_core::stat::StatSpec>> = self
            .dataset
            .columns()
            .iter()
            .map(|column| column.stat().cloned())
            .collect();
        brik.header.set_brick_stats(&stats)?;
        if let Some(table) = self
            .dataset
            .columns()
            .iter()
            .find_map(|column| column.label_table())
        {
            brik.header
                .set_value_label_table(&label_table_from_core(table, BTreeMap::new()));
        }
        for (index, column) in self.dataset.columns().iter().enumerate() {
            if let Some(curve) = column.fdr_curve() {
                brik.header
                    .set(format!("FDRCURVE_{index:06}"), curve_to_attr(curve));
            }
            if let Some(curve) = column.mdf_curve() {
                brik.header
                    .set(format!("MDFCURVE_{index:06}"), curve_to_attr(curve));
            }
        }
        Ok(brik)
    }

    /// Convert the core volume into a single-file, unscaled `f32` NIfTI.
    ///
    /// Labels, per-frame statistics, history, label tables, curves, and
    /// private source attributes are carried in an AFNI header extension. A
    /// one-frame statistic is also placed in the NIfTI intent fields (using
    /// AFNI's parameter convention for correlation); a multi-frame bucket
    /// needs the extension because NIfTI has only one intent for the whole
    /// file.
    pub fn to_nifti(&self, version: NiftiVersion) -> Result<Nifti> {
        // `to_brik(Float)` is the single checked path that already converts a
        // dense core volume, rebuilds storage-dependent metadata, and merges
        // source attributes. Reusing it keeps AFNI and NIfTI output identical.
        let brik = self.to_brik(StoragePolicy::Float)?;
        let Domain::Volume(domain) = self.dataset.domain() else {
            unreachable!("to_brik already checked the volume domain");
        };
        let dimensions = domain.dims();
        let nvols = self.dataset.columns().len();
        if version == NiftiVersion::Nifti1
            && dimensions
                .into_iter()
                .chain([nvols])
                .any(|dimension| dimension > i16::MAX as usize)
        {
            return Err(Error::invalid(
                "a NIfTI-1 dimension exceeds 32767; select NiftiVersion::Nifti2",
            ));
        }

        let expected = domain
            .voxel_count()
            .checked_mul(nvols)
            .ok_or_else(|| Error::invalid("NIfTI voxel count overflows"))?;
        let mut values = Vec::with_capacity(expected);
        for (frame, sub_brick) in brik.sub_bricks.iter().enumerate() {
            values.extend(
                sub_brick
                    .as_ref()
                    .ok_or_else(|| Error::missing(format!("output frame {frame}")))?
                    .scaled_values()?,
            );
        }

        let ras = *domain
            .affine()
            .ok_or_else(|| Error::missing("volume-domain affine"))?;
        let mut dim = [1_i64; 8];
        let time_step = self.dataset.time_step_seconds();
        dim[0] = if nvols > 1 || time_step.is_some() {
            4
        } else {
            3
        };
        dim[1] = dimensions[0] as i64;
        dim[2] = dimensions[1] as i64;
        dim[3] = dimensions[2] as i64;
        dim[4] = nvols as i64;
        let mut pixdim = [0.0_f64; 8];
        pixdim[0] = 1.0;
        for axis in 0..3 {
            pixdim[axis + 1] = (0..3).map(|row| ras[row][axis].powi(2)).sum::<f64>().sqrt();
        }
        if let Some(step) = time_step {
            pixdim[4] = step;
        }
        let finite_range = values
            .iter()
            .copied()
            .filter(|value| value.is_finite())
            .fold(None, |range: Option<(f32, f32)>, value| {
                Some(match range {
                    Some((min, max)) => (min.min(value), max.max(value)),
                    None => (value, value),
                })
            });
        let (cal_min, cal_max) = finite_range.unwrap_or((0.0, 0.0));
        let mut header = NiftiHeader {
            version,
            little_endian: true,
            dim,
            intent_p1: 0.0,
            intent_p2: 0.0,
            intent_p3: 0.0,
            intent_code: 0,
            datatype: DataType::Float32.code(),
            bitpix: i32::from(DataType::Float32.bitpix()),
            slice_start: 0,
            pixdim,
            vox_offset: 0,
            scl_slope: 0.0,
            scl_inter: 0.0,
            slice_end: dimensions[2].saturating_sub(1) as i64,
            slice_code: 0,
            // NIFTI_UNITS_MM, plus NIFTI_UNITS_SEC for a true time axis.
            xyzt_units: if time_step.is_some() { 2 | 8 } else { 2 },
            cal_max: f64::from(cal_max),
            cal_min: f64::from(cal_min),
            slice_duration: 0.0,
            toffset: time_step
                .and(self.dataset.time_start_seconds())
                .unwrap_or(0.0),
            descrip: "written by afni-io".into(),
            aux_file: String::new(),
            qform_code: 0,
            sform_code: 1,
            quatern_b: 0.0,
            quatern_c: 0.0,
            quatern_d: 0.0,
            qoffset_x: ras[0][3],
            qoffset_y: ras[1][3],
            qoffset_z: ras[2][3],
            srow_x: ras[0],
            srow_y: ras[1],
            srow_z: ras[2],
            intent_name: String::new(),
            dim_info: 0,
            magic: match version {
                NiftiVersion::Nifti1 => "n+1".into(),
                NiftiVersion::Nifti2 => "n+2".into(),
            },
        };
        if nvols == 1 {
            if let Some(stat) = self.dataset.columns()[0].stat() {
                header.intent_code = stat.kind.code() as i32;
                for (destination, source) in [
                    &mut header.intent_p1,
                    &mut header.intent_p2,
                    &mut header.intent_p3,
                ]
                .into_iter()
                .zip(stat.params.iter().copied())
                {
                    *destination = source;
                }
                header.intent_name = stat.kind.name().into();
            }
        }

        let mut nifti = Nifti {
            header,
            extensions: Vec::new(),
            data: TypedArray::Float32(values),
        };
        nifti.set_afni_header(&brik.header);
        Ok(nifti)
    }

    /// Write AFNI or NIfTI output, inferring the format from `path`.
    pub fn write(&self, path: impl AsRef<Path>) -> Result<WrittenVolume> {
        self.write_with_options(path, &VolumeWriteOptions::default())
    }

    /// Write AFNI or NIfTI output with one policy object. AFNI names and bare
    /// prefixes select HEAD/BRIK; `.nii` and `.nii.gz` select NIfTI unless
    /// [`VolumeWriteOptions::format`] overrides that inference.
    pub fn write_with_options(
        &self,
        path: impl AsRef<Path>,
        options: &VolumeWriteOptions,
    ) -> Result<WrittenVolume> {
        let path = path.as_ref();
        let format = match options.format {
            Some(format) => format,
            None => VolumeOutputFormat::from_path(path)?,
        };
        match format {
            VolumeOutputFormat::Afni => {
                let brik = self.to_brik(options.afni_storage)?;
                let paths = brik.write_with_options(
                    path,
                    &BrikWriteOptions {
                        overwrite: options.overwrite,
                        history_entry: options.history_entry.clone(),
                    },
                )?;
                Ok(WrittenVolume::Afni(paths))
            }
            VolumeOutputFormat::Nifti => {
                let mut nifti = self.to_nifti(options.nifti_version)?;
                if let Some(entry) = &options.history_entry {
                    let mut header = nifti
                        .afni_header()?
                        .ok_or_else(|| Error::missing("generated NIfTI AFNI extension"))?;
                    header.append_history(entry);
                    nifti.set_afni_header(&header);
                }
                nifti.write_with_options(
                    path,
                    &NiftiWriteOptions {
                        overwrite: options.overwrite,
                    },
                )?;
                Ok(WrittenVolume::Nifti(path.to_path_buf()))
            }
        }
    }
}

/// Convert a volume into a core dataset while retaining its AFNI header for a
/// later round trip.
pub fn volume_to_envelope(vol: &Volume) -> Result<VolumeEnvelope> {
    Ok(VolumeEnvelope {
        dataset: volume_to_core(vol)?,
        source_header: vol.afni_header()?,
    })
}

/// Convert every sub-brick of a volume into a dense core dataset on a
/// [`VolumeDomain`], one `Float32` column per sub-brick.
///
/// The domain affine is the voxel-to-RAS matrix ([`Volume::ijk_to_ras`]).
/// Statistics, FDR/MDF curves, and a value-label table come from the AFNI
/// attributes when present. All sub-bricks must have been read (see
/// [`crate::volume::read_any_volumes`]) and hold scalar data; otherwise this
/// returns an error rather than a partial dataset.
///
/// Malformed FDR/MDF curves are an error here; use [`volume_to_core_with`] to
/// skip them with a warning instead.
pub fn volume_to_core(vol: &Volume) -> Result<Dataset> {
    volume_to_core_with(vol, &AdaptOptions::default()).map(|(ds, _)| ds)
}

/// Like [`volume_to_core`], with options, also returning any warnings.
pub fn volume_to_core_with(
    vol: &Volume,
    options: &AdaptOptions,
) -> Result<(Dataset, Vec<AdaptWarning>)> {
    let mut warnings = Vec::new();
    let header = vol.afni_header()?;
    let id = header.as_ref().and_then(|h| domain_id(h.idcode()));
    let domain = Domain::Volume(
        VolumeDomain::new(id, vol.dimensions(), vol.ijk_to_ras().ok()).map_err(core_err)?,
    );

    let labels = vol.labels()?;
    let stats = match vol.stats_with_origin(options.intent_origin) {
        Ok(stats) => stats,
        Err(e) if options.statistics == MetadataPolicy::SkipWithWarning => {
            warnings.push(AdaptWarning {
                column: 0,
                what: "statistic",
                message: e.to_string(),
            });
            vec![None; vol.nvols()]
        }
        Err(e) => return Err(e),
    };
    let table = match &header {
        Some(h) => h
            .value_label_table()?
            .as_ref()
            .map(labels_to_core)
            .transpose()?,
        None => None,
    };

    let mut columns = Vec::with_capacity(vol.nvols());
    for t in 0..vol.nvols() {
        let frame = vol.frame_f32(t).ok_or_else(|| {
            Error::unsupported(format!("sub-brick {t} was not read or has no scalar value"))
        })?;
        let role = if table.is_some() {
            ColumnRole::Label
        } else if stats[t].is_some() {
            ColumnRole::Statistic
        } else {
            ColumnRole::Generic
        };
        let fdr = curve_with_policy(
            header.as_ref().and_then(|h| h.fdr_curve(t)),
            options.curves,
            t,
            "FDR curve",
            &mut warnings,
        )?;
        let mdf = curve_with_policy(
            header.as_ref().and_then(|h| h.mdf_curve(t)),
            options.curves,
            t,
            "MDF curve",
            &mut warnings,
        )?;
        columns.push(
            DataColumn::new(labels[t].clone(), role, ColumnData::Float32(frame))
                .map_err(core_err)?
                .with_stat(stats[t].clone())
                .with_fdr_curve(fdr)
                .with_mdf_curve(mdf)
                .with_label_table(table.clone()),
        );
    }

    let time = header.as_ref().and_then(|h| h.time_axis());
    let kind = match (&time, table.is_some()) {
        (_, true) => DatasetKind::Label,
        (Some(_), _) if vol.nvols() > 1 => DatasetKind::TimeSeries,
        _ => DatasetKind::Scalar,
    };
    let mut ds = Dataset::dense(kind, domain, columns).map_err(core_err)?;
    if let Some(axis) = &time {
        if let Some(tr) = axis.tr_seconds().filter(|tr| *tr > 0.0) {
            ds = ds.with_time_step_seconds(Some(tr)).map_err(core_err)?;
        }
        ds = ds
            .with_time_start_seconds(Some(axis.origin))
            .map_err(core_err)?;
    }
    Ok((ds, warnings))
}

// ---------------------------------------------------------------------------
// Surfaces
// ---------------------------------------------------------------------------

/// Convert a decoded surface (`.asc`, GIfTI geometry, ...) into a core
/// [`afni_core::mesh::SurfaceMesh`]: connectivity plus coordinates, ready for
/// normals, areas, distance searches and clustering.
///
/// Fails if a triangle names a node that does not exist or a coordinate is not
/// finite. Per-node and per-triangle flag columns (`Surface::vertex_flags`,
/// `face_flags`) are file bookkeeping and are not carried over.
pub fn surface_to_core(surface: &Surface) -> Result<afni_core::mesh::SurfaceMesh> {
    afni_core::mesh::SurfaceMesh::from_triangles(surface.vertices.clone(), surface.faces.clone())
        .map_err(core_err)
}

// ---------------------------------------------------------------------------
// .1D tables
// ---------------------------------------------------------------------------

/// Convert a `.1D` table into a dense core dataset on `domain`, one `Float64`
/// column per table column, named `col_0`, `col_1`, ...
///
/// A `.1D` file knows nothing about what its rows are, so the caller supplies
/// the domain; the row count must equal the domain's sample count.
pub fn onedee_to_core(table: &OneD, domain: Domain) -> Result<Dataset> {
    let columns = (0..table.cols)
        .map(|c| {
            let values = table.column(c).ok_or_else(|| Error::missing("1D column"))?;
            DataColumn::new(
                format!("col_{c}"),
                ColumnRole::Generic,
                ColumnData::Float64(values),
            )
            .map_err(core_err)
        })
        .collect::<Result<Vec<_>>>()?;
    Dataset::dense(DatasetKind::Scalar, domain, columns).map_err(core_err)
}

// ---------------------------------------------------------------------------
// ROIs (.niml.roi)
// ---------------------------------------------------------------------------

/// What a `NodeRoi` holds that a core [`CoreRoi`] cannot express, so that writing
/// the ROI back reproduces the file's attributes.
///
/// The core ROI always has a fill color, an edge color, an edge thickness and a
/// drawing type; a file may omit any of them. The flags remember which were
/// omitted. Identifiers are trimmed by core, so an id with stray spaces is kept
/// verbatim here.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RoiExtras {
    /// The file had no `FillColor`.
    pub fill_color_absent: bool,
    /// The file had no `EdgeColor`.
    pub edge_color_absent: bool,
    /// The file had no `EdgeThickness`.
    pub edge_thickness_absent: bool,
    /// The file had no `Type`.
    pub type_absent: bool,
    /// The `self_idcode` exactly as written, when core's trimmed id differs.
    pub self_idcode_raw: Option<String>,
    /// The `domain_parent_idcode` exactly as written, when core's differs.
    pub domain_parent_raw: Option<String>,
}

/// A core ROI together with what core cannot hold. Write it back with
/// [`to_node_roi`](Self::to_node_roi).
#[derive(Debug, Clone, PartialEq)]
pub struct RoiEnvelope {
    /// The ROI as `afni-core` understands it.
    pub core: CoreRoi,
    /// Everything else, for a faithful write.
    pub extras: RoiExtras,
}

fn core_color(c: crate::roi::Rgba) -> afni_core::color::Rgba {
    afni_core::color::Rgba {
        r: c.r,
        g: c.g,
        b: c.b,
        a: c.a,
    }
}

fn raw_color(c: afni_core::color::Rgba) -> crate::roi::Rgba {
    crate::roi::Rgba {
        r: c.r,
        g: c.g,
        b: c.b,
        a: c.a,
    }
}

/// Convert a `NodeRoi` into a core ROI.
///
/// Codes that SUMA does not define (an unknown `Type`, element type or action)
/// are kept as `Other(code)`, never dropped. A blank label is an error.
pub fn roi_to_core(raw: &NodeRoi) -> Result<RoiEnvelope> {
    let mut extras = RoiExtras::default();
    let mut core = CoreRoi::new(raw.label.clone(), raw.integer_label).map_err(core_err)?;
    core.source = afni_core::roi::RoiSource::NimlRoi;
    core.draw_status = afni_core::roi::RoiDrawStatus::Finished;

    // Identifiers: core trims them; remember the original if that changed it.
    core.id = match &raw.self_idcode {
        Some(text) => match afni_core::roi::RoiId::new(text.clone()) {
            Ok(id) => {
                if id.as_str() != text {
                    extras.self_idcode_raw = Some(text.clone());
                }
                Some(id)
            }
            Err(_) => {
                extras.self_idcode_raw = Some(text.clone());
                None
            }
        },
        None => None,
    };
    core.parent_domain = match &raw.domain_parent_idcode {
        Some(text) => match DomainId::new(text.clone()) {
            Ok(id) => {
                if id.as_str() != text {
                    extras.domain_parent_raw = Some(text.clone());
                }
                Some(id)
            }
            Err(_) => {
                extras.domain_parent_raw = Some(text.clone());
                None
            }
        },
        None => None,
    };
    core.parent_side = raw
        .parent_side
        .as_deref()
        .map(afni_core::roi::RoiSide::from_name);
    core.color_plane = raw.color_plane.clone();
    match raw.fill_color {
        Some(c) => core.fill_color = core_color(c),
        None => extras.fill_color_absent = true,
    }
    match raw.edge_color {
        Some(c) => core.edge_color = core_color(c),
        None => extras.edge_color_absent = true,
    }
    match raw.edge_thickness {
        Some(t) => core.edge_thickness = t,
        None => extras.edge_thickness_absent = true,
    }
    match raw.roi_type {
        Some(code) => core.drawing_type = afni_core::roi::RoiDrawingType::from_code(code),
        None => extras.type_absent = true,
    }
    core.strokes = raw
        .data
        .iter()
        .map(|d| {
            CoreStroke::new(
                afni_core::roi::RoiElementKind::from_code(d.element_type),
                afni_core::roi::RoiBrushAction::from_code(d.action),
                d.nodes.clone(),
            )
        })
        .collect();
    Ok(RoiEnvelope { core, extras })
}

impl RoiEnvelope {
    /// Rebuild the `NodeRoi`, including the attributes the file had and core
    /// cannot hold. Converting a `NodeRoi` to core and back gives the same
    /// `NodeRoi`.
    pub fn to_node_roi(&self) -> NodeRoi {
        let c = &self.core;
        let x = &self.extras;
        // Use the original text of an identifier if core's copy is unchanged.
        let id_text = |core: Option<&str>, raw: &Option<String>| match (core, raw) {
            (Some(t), Some(r)) if r.trim() == t => Some(r.clone()),
            (Some(t), _) => Some(t.to_owned()),
            (None, raw) => raw.clone(),
        };
        NodeRoi {
            self_idcode: id_text(c.id.as_ref().map(|i| i.as_str()), &x.self_idcode_raw),
            domain_parent_idcode: id_text(
                c.parent_domain.as_ref().map(|d| d.as_str()),
                &x.domain_parent_raw,
            ),
            parent_side: c.parent_side.as_ref().map(|s| s.name().to_owned()),
            label: c.label.clone(),
            integer_label: c.integer_label,
            roi_type: (!x.type_absent).then(|| c.drawing_type.code()),
            color_plane: c.color_plane.clone(),
            fill_color: (!x.fill_color_absent).then(|| raw_color(c.fill_color)),
            edge_color: (!x.edge_color_absent).then(|| raw_color(c.edge_color)),
            edge_thickness: (!x.edge_thickness_absent).then_some(c.edge_thickness),
            data: c
                .strokes
                .iter()
                .map(|s| RoiDatum {
                    action: s.action.code(),
                    element_type: s.kind.code(),
                    nodes: s.nodes.clone(),
                })
                .collect(),
        }
    }
}

/// Convert a core ROI into a `NodeRoi` with every attribute present (use
/// [`RoiEnvelope::to_node_roi`] to reproduce a file that omitted some).
pub fn roi_from_core(roi: &CoreRoi) -> NodeRoi {
    RoiEnvelope {
        core: roi.clone(),
        extras: RoiExtras::default(),
    }
    .to_node_roi()
}

// ---------------------------------------------------------------------------
// Graphs (Graph_Bucket) and tracts (.niml.tract)
// ---------------------------------------------------------------------------

/// Convert a `Graph_Bucket` file into a core [`afni_core::graph::Graph`].
///
/// Sparse graphs need their `INDEX_LIST`; its end nodes are node INDICES (the first
/// column of `NODE_COORDS`), which core resolves to positions. A graph whose edge
/// table does not fit its node count or layout is an error.
pub fn graph_to_core(raw: &GraphBucket) -> Result<afni_core::graph::Graph> {
    use afni_core::graph::{EdgeLayout, EdgeMeasure, Graph, GraphNode, SparseEdge};
    let nodes = raw
        .nodes
        .iter()
        .map(|n| GraphNode {
            index: n.index,
            position: n.xyz,
            label: n.label.clone(),
        })
        .collect();
    let layout = match raw.shape {
        MatrixShape::Full => EdgeLayout::Full,
        MatrixShape::Tri => EdgeLayout::LowerTriangle,
        MatrixShape::TriDiag => EdgeLayout::LowerTriangleWithDiagonal,
        MatrixShape::Sparse => {
            let edges = raw
                .edges
                .as_ref()
                .ok_or_else(|| Error::missing("INDEX_LIST of a sparse graph"))?;
            EdgeLayout::Sparse(
                edges
                    .iter()
                    .map(|&[id, row_node, column_node]| SparseEdge {
                        id,
                        row_node,
                        column_node,
                    })
                    .collect(),
            )
        }
    };
    let measures = raw
        .measures
        .iter()
        .zip(&raw.measure_labels)
        .map(|(values, label)| EdgeMeasure {
            label: label.clone(),
            values: values.clone(),
        })
        .collect();
    Graph::new(nodes, layout, measures).map_err(core_err)
}

/// Convert a core graph into a `Graph_Bucket` file model.
///
/// With a `template` (the file the graph came from) the file's other attributes,
/// history and links are kept and only the numbers are replaced. Without one,
/// the attributes AFNI expects are written fresh.
pub fn graph_from_core(
    graph: &afni_core::graph::Graph,
    template: Option<&GraphBucket>,
) -> GraphBucket {
    use afni_core::graph::EdgeLayout;
    let n = graph.node_count();
    let (shape, edges) = match graph.layout() {
        EdgeLayout::Full => (MatrixShape::Full, None),
        EdgeLayout::LowerTriangle => (MatrixShape::Tri, None),
        EdgeLayout::LowerTriangleWithDiagonal => (MatrixShape::TriDiag, None),
        EdgeLayout::Sparse(list) => (
            MatrixShape::Sparse,
            Some(
                list.iter()
                    .map(|e| [e.id, e.row_node, e.column_node])
                    .collect::<Vec<_>>(),
            ),
        ),
    };
    let attrs = |pairs: &[(&str, &str)]| -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect()
    };
    let (root_attrs, data_attrs, edge_attrs, node_attrs, extras) = match template {
        Some(t) => (
            t.root_attrs.clone(),
            t.data_attrs.clone(),
            t.edge_attrs.clone(),
            t.node_attrs.clone(),
            t.extras.clone(),
        ),
        None => (
            attrs(&[("dset_type", "Graph_Bucket"), ("ni_form", "ni_group")]),
            attrs(&[("data_type", "Graph_Bucket_data")]),
            attrs(&[("data_type", "Graph_Bucket_edge_indices")]),
            attrs(&[("data_type", "Graph_Bucket_node_coordinates")]),
            Vec::new(),
        ),
    };
    GraphBucket {
        root_attrs,
        shape,
        matrix_size: Some(format!(" {n} {n}")),
        data_attrs,
        measures: graph.measures().iter().map(|m| m.values.clone()).collect(),
        measure_labels: graph.measures().iter().map(|m| m.label.clone()).collect(),
        edges,
        edge_attrs,
        nodes: graph
            .nodes()
            .iter()
            .map(|node| NodeRow {
                index: node.index,
                xyz: node.position,
                label: node.label.clone(),
            })
            .collect(),
        node_attrs,
        extras,
    }
}

/// Convert a tract network file into core tracts. A tract with no points or a
/// non-finite coordinate is an error.
pub fn tracts_to_core(raw: &TractNetwork) -> Result<afni_core::tract::TractSet> {
    use afni_core::tract::{Tract, TractBundle, TractSet};
    let bundles = raw
        .bundles
        .iter()
        .map(|b| {
            Ok(TractBundle {
                tag: b.tag,
                alt_tag: b.alt_tag,
                ends: b.ends.clone(),
                tracts: b
                    .tracts
                    .iter()
                    .map(|t| Tract::new(t.id, t.points.clone()).map_err(core_err))
                    .collect::<Result<Vec<_>>>()?,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(TractSet { bundles })
}

/// Convert core tracts into a tract network file model. With a `template` (the
/// file they came from), the network's other attributes and its grid/FA datasets are
/// kept, and each bundle keeps its extra attributes when the bundle counts agree.
pub fn tracts_from_core(
    set: &afni_core::tract::TractSet,
    template: Option<&TractNetwork>,
) -> TractNetwork {
    let keep_bundle_attrs = template.is_some_and(|t| t.bundles.len() == set.bundles.len());
    let bundles = set
        .bundles
        .iter()
        .enumerate()
        .map(|(i, b)| RawBundle {
            tag: b.tag,
            alt_tag: b.alt_tag,
            ends: b.ends.clone(),
            attrs: if keep_bundle_attrs {
                template
                    .map(|t| t.bundles[i].attrs.clone())
                    .unwrap_or_default()
            } else {
                BTreeMap::new()
            },
            tracts: b
                .tracts
                .iter()
                .map(|t| RawTract {
                    id: t.id,
                    points: t.points.clone(),
                })
                .collect(),
        })
        .collect();
    TractNetwork {
        root_attrs: template.map_or_else(
            || {
                let mut a = BTreeMap::new();
                a.insert("ni_form".to_owned(), "ni_group".to_owned());
                a
            },
            |t| t.root_attrs.clone(),
        ),
        bundles,
        extras: template.map(|t| t.extras.clone()).unwrap_or_default(),
    }
}
