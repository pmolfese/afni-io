//! Unified reading of NIML and GIfTI surface data sets.
//!
//! [`SurfaceDatasetReader`] is the surface-data counterpart to the common
//! entry point provided by [`crate::volume::VolumeReader`]. Unlike the volume
//! reader it is eager: XML/NIML containers and compressed GIfTI payloads are
//! parsed completely when opened. Both formats are adapted to the same
//! [`afni_core::dataset::Dataset`] model before this type is returned.
//!
//! Geometry-only GIfTI files are not surface *datasets*. Read those with
//! [`crate::gifti::Gifti::read`] and [`crate::gifti::Gifti::to_surface`].

use std::path::{Path, PathBuf};

use afni_core::dataset::Dataset;

use crate::adapt::{
    gifti_to_core_with, niml_to_core_with, AdaptOptions, AdaptWarning, NimlEnvelope, NimlExtras,
};
use crate::dset::NimlDataset;
use crate::error::{self, Error, Result};
use crate::gifti::Gifti;

/// On-disk surface dataset syntax detected by [`SurfaceDatasetReader`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SurfaceDatasetFormat {
    /// SUMA `AFNI_dataset` NIML (`.niml.dset`).
    Niml,
    /// GIfTI data arrays (`.gii.dset`, `.func.gii`, `.shape.gii`, etc.).
    Gifti,
}

/// Options for unified surface-dataset reading.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SurfaceDatasetReadOptions {
    /// Total nodes in the surface domain.
    ///
    /// This may be omitted for dense data, where the row count determines the
    /// node count. Sparse NIML and GIfTI datasets need it because their files
    /// list stored node indices but do not record the complete surface size.
    pub node_count: Option<usize>,
    /// Policy for malformed statistics and FDR/MDF curves during conversion.
    pub adapt: AdaptOptions,
}

/// Format-specific material retained beside the common core dataset.
#[derive(Debug, Clone, PartialEq)]
enum SurfaceDatasetSource {
    Niml(NimlExtras),
    Gifti(Gifti),
}

/// An eagerly decoded NIML or GIfTI surface dataset.
///
/// Open either format through [`open`](Self::open), then use [`dataset`](Self::dataset)
/// for file-neutral processing. NIML inputs retain a [`NimlExtras`] envelope for
/// lossless write-back; GIfTI inputs retain the original [`Gifti`] document for
/// metadata or geometry inspection.
#[derive(Debug, Clone, PartialEq)]
pub struct SurfaceDatasetReader {
    path: PathBuf,
    format: SurfaceDatasetFormat,
    dataset: Dataset,
    source: SurfaceDatasetSource,
    warnings: Vec<AdaptWarning>,
}

impl SurfaceDatasetReader {
    /// Read a dense NIML or GIfTI surface dataset with strict metadata checks.
    ///
    /// Sparse inputs return an error explaining that a node count is required;
    /// use [`open_with_options`](Self::open_with_options) for those files.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        Self::open_with_options(path, SurfaceDatasetReadOptions::default())
    }

    /// Read a NIML or GIfTI surface dataset with explicit options.
    ///
    /// Detection uses the decoded document root rather than only the filename,
    /// so conventional GIfTI suffixes and `.gii.dset` all follow the same path.
    /// An outer gzip stream is accepted for either syntax.
    pub fn open_with_options(
        path: impl AsRef<Path>,
        options: SurfaceDatasetReadOptions,
    ) -> Result<Self> {
        let path = path.as_ref();
        let mut bytes = error::read_file(path)?;
        if crate::compress::is_gzip(&bytes) {
            bytes = crate::compress::gunzip(&bytes)?;
        }
        let format = detect_format(path, &bytes)?;

        let (dataset, source, warnings) = match format {
            SurfaceDatasetFormat::Niml => {
                let raw = NimlDataset::from_bytes(&bytes)?;
                let node_count = options
                    .node_count
                    .or_else(|| identity_niml_node_count(&raw));
                let (envelope, warnings) = niml_to_core_with(&raw, node_count, &options.adapt)?;
                (
                    envelope.core,
                    SurfaceDatasetSource::Niml(envelope.extras),
                    warnings,
                )
            }
            SurfaceDatasetFormat::Gifti => {
                let text = error::from_utf8(&bytes, "GIfTI surface dataset")?;
                let raw = Gifti::parse(&text)?;
                let node_count = options
                    .node_count
                    .or_else(|| identity_gifti_node_count(&raw));
                let (dataset, warnings) = gifti_to_core_with(&raw, node_count, &options.adapt)?;
                (dataset, SurfaceDatasetSource::Gifti(raw), warnings)
            }
        };

        Ok(Self {
            path: path.to_path_buf(),
            format,
            dataset,
            source,
            warnings,
        })
    }

    /// Input path used to open this dataset.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Detected on-disk syntax.
    pub fn format(&self) -> SurfaceDatasetFormat {
        self.format
    }

    /// File-neutral validated dataset used for scientific processing.
    pub fn dataset(&self) -> &Dataset {
        &self.dataset
    }

    /// Consume the reader and return its file-neutral dataset.
    pub fn into_dataset(self) -> Dataset {
        self.dataset
    }

    /// Non-fatal metadata problems skipped under the configured policy.
    pub fn warnings(&self) -> &[AdaptWarning] {
        &self.warnings
    }

    /// NIML extras retained for lossless write-back, or `None` for GIfTI.
    pub fn niml_extras(&self) -> Option<&NimlExtras> {
        match &self.source {
            SurfaceDatasetSource::Niml(extras) => Some(extras),
            SurfaceDatasetSource::Gifti(_) => None,
        }
    }

    /// Build a writeable NIML envelope from this reader, or `None` for GIfTI.
    ///
    /// The returned envelope owns a clone of the core dataset. Replace its
    /// `core` field with a processed dataset before calling
    /// [`NimlEnvelope::to_niml`] when writing a derived result while preserving
    /// unmodelled input attributes.
    pub fn niml_envelope(&self) -> Option<NimlEnvelope> {
        self.niml_extras().map(|extras| NimlEnvelope {
            core: self.dataset.clone(),
            extras: extras.clone(),
        })
    }

    /// Original parsed GIfTI document, or `None` for NIML.
    ///
    /// This retains file metadata and any geometry arrays. Core processing uses
    /// [`dataset`](Self::dataset), not these raw arrays.
    pub fn gifti(&self) -> Option<&Gifti> {
        match &self.source {
            SurfaceDatasetSource::Niml(_) => None,
            SurfaceDatasetSource::Gifti(gifti) => Some(gifti),
        }
    }
}

/// Identify the root container, falling back to a conventional suffix only so
/// malformed known files still receive their format-specific parser error.
fn detect_format(path: &Path, bytes: &[u8]) -> Result<SurfaceDatasetFormat> {
    if contains_ascii(bytes, b"<GIFTI") {
        return Ok(SurfaceDatasetFormat::Gifti);
    }
    if contains_ascii(bytes, b"<AFNI_dataset") {
        return Ok(SurfaceDatasetFormat::Niml);
    }

    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    if name.ends_with(".gii")
        || name.ends_with(".gii.dset")
        || name.ends_with(".gii.gz")
        || name.ends_with(".gii.dset.gz")
    {
        Ok(SurfaceDatasetFormat::Gifti)
    } else if name.ends_with(".niml.dset") || name.ends_with(".niml.dset.gz") {
        Ok(SurfaceDatasetFormat::Niml)
    } else {
        Err(Error::unsupported(format!(
            "surface dataset format for {}; expected a GIfTI or .niml.dset document",
            path.display()
        )))
    }
}

/// Byte search for an ASCII root tag; works even when later NIML payload bytes
/// are binary and the complete file is not valid UTF-8.
fn contains_ascii(bytes: &[u8], needle: &[u8]) -> bool {
    bytes.windows(needle.len()).any(|window| window == needle)
}

/// An explicit `0..rows` NIML index list carries enough information to infer
/// the complete domain even though the raw representation is indexed.
fn identity_niml_node_count(dataset: &NimlDataset) -> Option<usize> {
    let indices = dataset.node_indices.as_ref()?;
    indices
        .iter()
        .enumerate()
        .all(|(row, &sample)| usize::try_from(sample).ok() == Some(row))
        .then_some(indices.len())
}

/// GIfTI equivalent of [`identity_niml_node_count`].
fn identity_gifti_node_count(gifti: &Gifti) -> Option<usize> {
    let indices = gifti.array_with_intent(crate::gifti::intent::NODE_INDEX)?;
    let values = indices.data.to_f64_vec();
    values
        .iter()
        .enumerate()
        .all(|(row, &sample)| sample == row as f64)
        .then_some(values.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn root_detection_wins_over_a_misleading_suffix() {
        assert_eq!(
            detect_format(Path::new("wrong.niml.dset"), b"<?xml?><GIFTI></GIFTI>").unwrap(),
            SurfaceDatasetFormat::Gifti
        );
        assert_eq!(
            detect_format(Path::new("wrong.gii"), b"<AFNI_dataset></AFNI_dataset>").unwrap(),
            SurfaceDatasetFormat::Niml
        );
    }

    #[test]
    fn unknown_content_and_suffix_are_rejected() {
        assert!(matches!(
            detect_format(Path::new("values.txt"), b"1 2 3"),
            Err(Error::Unsupported(_))
        ));
    }
}
