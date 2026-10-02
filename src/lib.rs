//! Native Rust readers and writers for the file formats used by
//! [AFNI](https://afni.nimh.nih.gov/) and its surface viewer SUMA.
//!
//! The crate is organised around the formats AFNI/SUMA actually emit and
//! consume. None of the readers shell out to AFNI binaries; everything is
//! parsed in pure Rust.
//!
//! | Module | Format | Typical extensions |
//! |--------|--------|--------------------|
//! | [`niml`] | NIML element trees (ASCII and binary) | `.niml`, `.niml.asc` |
//! | [`dset`] | Surface datasets (`AFNI_dataset`) | `.niml.dset` |
//! | [`roi`]  | Drawn surface ROIs (`Node_ROI`) | `.niml.roi` |
//! | [`brik`] | AFNI volume datasets (`.HEAD`/`.BRIK` pair) | `.HEAD` + `.BRIK`/`.BRIK.gz` |
//! | [`spec`] | SUMA surface spec files | `.spec` |
//! | [`surface`] | FreeSurfer/SUMA ASCII surfaces | `.asc` |
//! | [`gifti`] | GIfTI surface/data XML | `.gii`, `.gii.gz`, `.gii.dset` |
//! | [`nifti`] | NIfTI-1/2 volumes | `.nii`, `.nii.gz`, `.hdr`/`.img` |
//! | [`onedee`] | AFNI numeric text tables | `.1D` |
//!
//! An AFNI volume is a single dataset stored as two files: a `.HEAD` of ASCII
//! attributes and a `.BRIK` (optionally gzipped to `.BRIK.gz`) of binary voxel
//! data. [`brik::Brik::read`] loads the dataset from any member of the pair;
//! [`head::Header`] is available when only the attributes are needed.
//!
//! # Quick start
//!
//! ```no_run
//! use afni_io::dset::NimlDataset;
//!
//! let dset = NimlDataset::read("lh.thickness.niml.dset")?;
//! println!("{} nodes x {} columns", dset.rows(), dset.columns());
//! # Ok::<(), afni_io::Error>(())
//! ```
//!
//! The lowest layer, [`niml`], exposes the generic element tree that the
//! higher-level [`dset`] and [`roi`] modules are built on, so unusual files
//! can always be inspected element-by-element.

#![warn(missing_debug_implementations)]

pub mod array;
mod base64;
pub mod brik;
mod compress;
pub mod dset;
pub mod error;
pub mod gifti;
pub mod head;
pub mod nifti;
pub mod niml;
pub mod onedee;
pub mod roi;
pub mod spec;
pub mod surface;
mod xml;

pub use error::{Error, Result};

/// Re-exports of the most commonly used types.
pub mod prelude {
    pub use crate::array::{DataType, TypedArray};
    pub use crate::brik::{Brik, BrikType};
    pub use crate::dset::NimlDataset;
    pub use crate::error::{Error, Result};
    pub use crate::gifti::{DataArray, Gifti};
    pub use crate::head::{Attribute, AttributeValue, Header};
    pub use crate::nifti::{Nifti, NiftiHeader, NiftiVersion};
    pub use crate::niml::{NimlData, NimlElement, NimlValue, NimlValueType};
    pub use crate::onedee::OneD;
    pub use crate::roi::{NodeRoi, RoiDatum};
    pub use crate::spec::{Spec, SpecSurface};
    pub use crate::surface::Surface;
}
