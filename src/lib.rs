//! Native Rust readers and writers for the file formats used by
//! [AFNI](https://afni.nimh.nih.gov/) and its surface viewer SUMA.
//!
//! The crate is organised around the formats AFNI/SUMA actually emit and
//! consume. None of the readers shell out to AFNI binaries; everything is
//! parsed in pure Rust.
//!
//! | Module | Format | Typical extensions |
//! |--------|--------|--------------------|
//! | [`aff12`] | AFNI spatial affine transforms | `.aff12.1D` |
//! | [`niml`] | NIML element trees (ASCII and binary) | `.niml`, `.niml.asc` |
//! | [`dset`] | Surface datasets (`AFNI_dataset`) | `.niml.dset` |
//! | [`roi`]  | Drawn surface ROIs (`Node_ROI`) | `.niml.roi` |
//! | [`labels`] | Label tables | `.niml.lt`, `.niml.cmap` |
//! | [`graph`] | FATCAT/SUMA network datasets (`Graph_Bucket`) | `.niml.dset` |
//! | [`tract`] | FATCAT tract networks (`TAYLOR_TRACT_DATUM`) | `.niml.tract` |
//! | [`brik`] | AFNI volume datasets (`.HEAD`/`.BRIK` pair) | `.HEAD` + `.BRIK`/`.BRIK.gz` |
//! | [`volume`] | AFNI or NIfTI volumes through one API | any of the above |
//! | [`spec`] | SUMA surface spec files | `.spec` |
//! | [`surface`] | FreeSurfer/SUMA ASCII surfaces | `.asc` |
//! | [`surface_dataset`] | Unified NIML/GIfTI surface datasets | `.niml.dset`, `.gii.dset`, `*.gii` |
//! | [`freesurfer`] | FreeSurfer binary triangle surfaces | `lh.white`, `rh.pial`, … |
//! | [`stc`] | MNE source estimates | `.stc` |
//! | [`gifti`] | GIfTI surface/data XML | `.gii`, `.gii.gz`, `.gii.dset` |
//! | [`nifti`] | NIfTI-1/2 volumes | `.nii`, `.nii.gz`, `.hdr`/`.img` |
//! | [`onedee`] | AFNI numeric text tables | `.1D` |
//! | [`xmat`] | AFNI regression design matrices | `X.xmat.1D`, `X.mat.1D` |
//! | `talk` | AFNI ⇄ SUMA talk protocol encoding (feature `talk`) | (TCP, no files) |
//! | [`adapt`] | Adapters into `afni-core` datasets and label tables | (any of the above) |
//!
//! An AFNI volume is a single dataset stored as two files: a `.HEAD` of ASCII
//! attributes and a `.BRIK` (optionally gzipped to `.BRIK.gz`) of binary voxel
//! data. [`brik::Brik::read`] loads the dataset from any member of the pair;
//! [`head::Header`] is available when only the attributes are needed.
//!
//! # Quick start
//!
//! ```no_run
//! use afni_io::surface_dataset::SurfaceDatasetReader;
//!
//! let reader = SurfaceDatasetReader::open("lh.thickness.niml.dset")?;
//! let dset = reader.dataset();
//! println!("{} stored nodes x {} columns", dset.row_count(), dset.columns().len());
//! # Ok::<(), afni_io::Error>(())
//! ```
//!
//! The lowest layer, [`niml`], exposes the generic element tree that the
//! higher-level [`dset`] and [`roi`] modules are built on, so unusual files
//! can always be inspected element-by-element.

#![warn(missing_debug_implementations)]

pub mod adapt;
pub mod aff12;
pub mod array;
mod base64;
pub mod brik;
mod compress;
pub mod dset;
pub mod error;
pub mod freesurfer;
pub mod geometry;
pub mod gifti;
pub mod graph;
pub mod head;
pub mod labels;
pub mod nifti;
pub mod niml;
pub mod onedee;
pub mod roi;
pub mod selector;
pub mod spec;
pub mod stat;
pub mod stc;
pub mod surface;
pub mod surface_dataset;
#[cfg(feature = "talk")]
pub mod talk;
pub mod tract;
pub mod volume;
pub mod xmat;
mod xml;

pub use error::{Error, Result};

/// Re-exports of the most commonly used types.
pub mod prelude {
    pub use crate::adapt::{
        NiftiMetadata, VolumeBuilder, VolumeDataset, VolumeEnvelope, VolumeFrameWriter,
        VolumeMetadata, VolumeOutputFormat, VolumeWriteOptions, VolumeWriteSpec, WrittenVolume,
    };
    pub use crate::aff12::{
        read_aff12_series, read_aff12_transform, write_aff12_series, write_aff12_transform,
        Aff12File,
    };
    pub use crate::array::{DataType, TypedArray};
    pub use crate::brik::{
        AfniPaths, BrickData, Brik, BrikBuilder, BrikReader, BrikType, BrikWriteOptions,
        ScaledValues, StoragePolicy, SubBrick,
    };
    pub use crate::dset::{ColumnRange, NimlDataset};
    pub use crate::error::{Error, Result};
    pub use crate::geometry::{Mat44, Orientation, TimeAxis, TimeUnits, View};
    pub use crate::gifti::{DataArray, Gifti};
    pub use crate::head::{Attribute, AttributeValue, Header};
    pub use crate::labels::{LabelEntry, LabelTable};
    pub use crate::nifti::{
        Nifti, NiftiExtension, NiftiHeader, NiftiReader, NiftiVersion, NiftiWriteOptions,
    };
    pub use crate::niml::{NimlData, NimlElement, NimlValue, NimlValueType};
    pub use crate::onedee::OneD;
    pub use crate::roi::{BrushAction, NodeRoi, RoiDatum, RoiDrawingType, RoiElementType, Side};
    pub use crate::selector::{DatasetSpec, SubBrickSelector};
    pub use crate::spec::{Spec, SpecSurface};
    pub use crate::stat::{StatKind, StatSpec, ThresholdCurve};
    pub use crate::surface::Surface;
    pub use crate::surface_dataset::{
        SurfaceDatasetFormat, SurfaceDatasetReadOptions, SurfaceDatasetReader,
    };
    pub use crate::volume::{
        read_any, read_any_volumes, GridCompatibility, GridSpec, Volume, VolumeFormat,
        VolumeFrames, VolumeMask, VolumeReader, DEFAULT_GRID_TOLERANCE,
    };
    pub use crate::xmat::{XmatAttribute, XmatExtras, XmatFile};
    pub use afni_core::affine::{AffineTransform, AffineTransformSeries, CoordinateConvention};
    pub use afni_core::design::{DesignMatrix, Regressor, RegressorRole, Stimulus};
}
