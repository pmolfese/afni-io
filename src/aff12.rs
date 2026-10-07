//! AFNI `*.aff12.1D` spatial affine files.
//!
//! Each non-comment row contains twelve values in row-major 3x4 order. A file
//! may hold one static transform or a series (for example, one motion
//! transform per volume). AFNI writes these matrices in its DICOM/RAI world
//! coordinate convention. The transform math and validation live in
//! [`afni_core::affine`]; this module supplies file parsing, serialization, and
//! preservation of `.1D` comments.
//!
//! The format does not store source and target space names. In particular,
//! `3dAllineate -1Dmatrix_save` and `3dvolreg -1Dmatrix_save` conventionally
//! write base-to-source transforms, but that direction comes from the command
//! that produced the file rather than the twelve numbers themselves.

use std::path::Path;

use afni_core::affine::{AffineTransform, AffineTransformSeries, CoordinateConvention};

use crate::onedee::OneD;
use crate::{Error, Result};

/// A parsed `aff12.1D` file, including its comments.
#[derive(Debug, Clone, PartialEq)]
pub struct Aff12File {
    transforms: AffineTransformSeries,
    comments: Vec<String>,
}

impl Aff12File {
    /// Read and validate a static or multi-row `aff12.1D` file.
    pub fn read(path: impl AsRef<Path>) -> Result<Self> {
        Self::from_onedee(OneD::read(path)?)
    }

    /// Parse and validate `aff12.1D` text.
    pub fn parse(text: &str) -> Result<Self> {
        Self::from_onedee(OneD::parse(text)?)
    }

    /// Convert a generic `.1D` table into a typed AFNI affine file.
    pub fn from_onedee(table: OneD) -> Result<Self> {
        if table.rows == 0 {
            return Err(Error::parse("aff12.1D contains no transforms"));
        }
        if table.cols != 12 {
            return Err(Error::parse(format!(
                "aff12.1D must have 12 columns per row, found {}",
                table.cols
            )));
        }

        let mut transforms = Vec::with_capacity(table.rows);
        for (row_index, row) in table.values.chunks_exact(12).enumerate() {
            let values: [f64; 12] = row.try_into().expect("chunks_exact yields 12 values");
            let transform = AffineTransform::from_aff12_row(values).map_err(|error| {
                Error::parse(format!("aff12.1D row {}: {error}", row_index + 1))
            })?;
            transforms.push(transform);
        }
        let transforms = AffineTransformSeries::new(transforms)
            .map_err(|error| Error::parse(format!("aff12.1D: {error}")))?;
        Ok(Self {
            transforms,
            comments: table.comments,
        })
    }

    /// Build a one-row AFNI affine file.
    ///
    /// The transform must already use AFNI DICOM/RAI coordinates. Convert an
    /// RAS transform explicitly with [`AffineTransform::convert_coordinates`]
    /// so a coordinate change can never happen silently during serialization.
    pub fn from_transform(transform: AffineTransform) -> Result<Self> {
        Self::from_series(AffineTransformSeries::new(vec![transform]).map_err(core_error)?)
    }

    /// Build a multi-row AFNI affine file.
    pub fn from_series(transforms: AffineTransformSeries) -> Result<Self> {
        if transforms.convention() != CoordinateConvention::AfniDICOM {
            return Err(Error::invalid(
                "aff12.1D requires AFNI DICOM/RAI coordinates; convert explicitly before writing",
            ));
        }
        Ok(Self {
            transforms,
            comments: Vec::new(),
        })
    }

    /// Add or replace comment lines (without leading `#` characters).
    pub fn with_comments(mut self, comments: Vec<String>) -> Self {
        self.comments = comments;
        self
    }

    /// The non-empty transform series in file order.
    pub fn transforms(&self) -> &AffineTransformSeries {
        &self.transforms
    }

    /// Consume the file wrapper and return its transform series.
    pub fn into_transforms(self) -> AffineTransformSeries {
        self.transforms
    }

    /// Comment lines in file order, without leading `#` characters.
    pub fn comments(&self) -> &[String] {
        &self.comments
    }

    /// Return the sole transform, rejecting a multi-row file.
    pub fn single_transform(&self) -> Result<&AffineTransform> {
        if self.transforms.len() != 1 {
            return Err(Error::invalid(format!(
                "expected one static affine transform, found {} rows",
                self.transforms.len()
            )));
        }
        Ok(self
            .transforms
            .get(0)
            .expect("a validated transform series is non-empty"))
    }

    /// Convert back to the generic `.1D` table representation.
    pub fn to_onedee(&self) -> Result<OneD> {
        let rows = self
            .transforms
            .as_slice()
            .iter()
            .map(|transform| transform.to_aff12_row().to_vec())
            .collect();
        let mut table = OneD::from_rows(rows)?;
        table.comments.clone_from(&self.comments);
        Ok(table)
    }

    /// Serialize as AFNI-compatible `.aff12.1D` text.
    pub fn to_aff12_string(&self) -> Result<String> {
        Ok(self.to_onedee()?.to_1d_string())
    }

    /// Write an AFNI-compatible `.aff12.1D` file.
    pub fn write(&self, path: impl AsRef<Path>) -> Result<()> {
        self.to_onedee()?.write(path)
    }
}

/// Read a one-row static AFNI affine transform.
pub fn read_aff12_transform(path: impl AsRef<Path>) -> Result<AffineTransform> {
    Ok(Aff12File::read(path)?.single_transform()?.clone())
}

/// Read a static or multi-row AFNI affine transform series.
pub fn read_aff12_series(path: impl AsRef<Path>) -> Result<AffineTransformSeries> {
    Ok(Aff12File::read(path)?.into_transforms())
}

/// Write one static AFNI DICOM/RAI affine transform.
pub fn write_aff12_transform(path: impl AsRef<Path>, transform: &AffineTransform) -> Result<()> {
    Aff12File::from_transform(transform.clone())?.write(path)
}

/// Write a static or multi-row AFNI DICOM/RAI affine transform series.
pub fn write_aff12_series(
    path: impl AsRef<Path>,
    transforms: &AffineTransformSeries,
) -> Result<()> {
    Aff12File::from_series(transforms.clone())?.write(path)
}

fn core_error(error: afni_core::Error) -> Error {
    Error::invalid(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    const IDENTITY: &str = "1 0 0 0 0 1 0 0 0 0 1 0";

    #[test]
    fn parses_static_and_multi_row_files() {
        let static_file = Aff12File::parse(&format!("# base to source\n{IDENTITY}\n")).unwrap();
        assert_eq!(static_file.transforms().len(), 1);
        assert_eq!(static_file.comments(), &["base to source"]);
        assert_eq!(
            static_file.single_transform().unwrap().convention(),
            CoordinateConvention::AfniDICOM
        );

        let series = Aff12File::parse(&format!("{IDENTITY}\n1 0 0 2 0 1 0 3 0 0 1 4\n")).unwrap();
        assert_eq!(series.transforms().len(), 2);
        assert!(series.single_transform().is_err());
        assert_eq!(
            series.transforms().apply_point(1, [1.0, 1.0, 1.0]).unwrap(),
            [3.0, 4.0, 5.0]
        );
    }

    #[test]
    fn comments_and_values_round_trip() {
        let parsed = Aff12File::parse(&format!(
            "# made by a test\n{IDENTITY}\n1 0 0 2.5 0 1 0 -3 0 0 1 4\n"
        ))
        .unwrap();
        let reparsed = Aff12File::parse(&parsed.to_aff12_string().unwrap()).unwrap();
        assert_eq!(reparsed, parsed);
    }

    #[test]
    fn rejects_wrong_shapes_and_invalid_transforms() {
        assert!(Aff12File::parse("").is_err());
        assert!(Aff12File::parse("1 0 0\n").is_err());
        assert!(Aff12File::parse("0 0 0 0 0 0 0 0 0 0 0 0\n").is_err());
        assert!(Aff12File::parse("1 0 0 NaN 0 1 0 0 0 0 1 0\n").is_err());
    }

    #[test]
    fn ras_must_be_converted_explicitly_before_writing() {
        let ras = AffineTransform::identity(CoordinateConvention::Ras);
        assert!(Aff12File::from_transform(ras.clone()).is_err());
        let rai = ras
            .convert_coordinates(CoordinateConvention::AfniDICOM)
            .unwrap();
        assert!(Aff12File::from_transform(rai).is_ok());
    }
}
