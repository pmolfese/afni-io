//! One entry point for volumes in either format: AFNI `.HEAD`/`.BRIK` or
//! NIfTI.
//!
//! [`read_any`] picks the reader from the name (anything
//! [`AfniPaths::is_afni_name`] accepts is AFNI; everything else is NIfTI) and
//! returns a [`Volume`] with format-independent accessors: grid size, number
//! of volumes, the voxel-to-world matrix, one volume at a time as scaled
//! `f32`, and the AFNI attributes (the `.HEAD` itself, or the NIfTI's AFNI
//! extension), which carry sub-brick labels and statistics.
//!
//! Voxel order is the same in both formats: `i + nx * (j + ny * k)`, then
//! volume after volume.

use std::path::Path;

use crate::array::{DataType, TypedArray};
use crate::brik::{AfniPaths, Brik, BrikReader};
use crate::error::{Error, Result};
use crate::geometry::{dicom_to_ras, Mat44};
use crate::head::{AttributeValue, Header};
use crate::nifti::{Nifti, NiftiReader};
use crate::selector::DatasetSpec;
use crate::stat::StatSpec;

/// A volume read by [`read_any`].
#[derive(Debug, Clone)]
pub enum Volume {
    /// An AFNI `.HEAD`/`.BRIK` dataset.
    Afni(Brik),
    /// A NIfTI-1 or NIfTI-2 volume (boxed: its header is much larger than
    /// a [`Brik`]).
    Nifti(Box<Nifti>),
}

/// File format backing a [`VolumeReader`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VolumeFormat {
    /// AFNI `.HEAD`/`.BRIK` pair.
    Afni,
    /// NIfTI `.nii`, `.nii.gz`, or `.hdr`/`.img`.
    Nifti,
}

#[derive(Debug)]
enum VolumeReaderSource {
    Afni(BrikReader),
    Nifti(Box<NiftiReader>),
}

/// Memory-bounded, selector-aware volume reader.
///
/// Opening reads only file metadata. [`VolumeReader::frame`] loads one selected
/// frame, and [`VolumeReader::frame_into`] lets command-line programs reuse a
/// caller-owned buffer. A trailing AFNI selector changes the logical frame
/// order without copying values, for example `epi+orig[0,2..10(2),$]`.
#[derive(Debug)]
pub struct VolumeReader {
    spec: DatasetSpec,
    source: VolumeReaderSource,
    selected: Vec<usize>,
    dimensions: [usize; 3],
    ijk_to_ras: Mat44,
    source_header: Option<Header>,
    labels: Vec<String>,
    stats: Vec<Option<StatSpec>>,
}

impl VolumeReader {
    /// Open an AFNI or NIfTI dataset, including an optional terminal selector.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        Self::open_spec(DatasetSpec::from_path(path)?)
    }

    /// Open an already-parsed dataset specification.
    pub fn open_spec(spec: DatasetSpec) -> Result<Self> {
        if AfniPaths::is_afni_name(spec.path()) {
            let reader = BrikReader::open(spec.path())?;
            let dimensions = reader.dimensions();
            let ijk_to_ras = reader.header().ijk_to_ras()?;
            let source_header = reader.header().clone();
            let all_labels = source_header.brick_labels();
            let all_stats = source_header.brick_stats();
            let selected = spec.resolve(reader.nvals(), &all_labels)?;
            let labels = selected
                .iter()
                .map(|&frame| all_labels[frame].clone())
                .collect();
            let stats = selected
                .iter()
                .map(|&frame| all_stats.get(frame).cloned().unwrap_or(None))
                .collect();
            Ok(Self {
                spec,
                source: VolumeReaderSource::Afni(reader),
                selected,
                dimensions,
                ijk_to_ras,
                source_header: Some(source_header),
                labels,
                stats,
            })
        } else {
            let reader = NiftiReader::open(spec.path())?;
            let dimensions = reader.dimensions();
            let ijk_to_ras = reader.header().affine();
            let source_header = reader.afni_header()?;
            let source_nvols = reader.nvols();
            let mut all_labels = source_header
                .as_ref()
                .map(Header::brick_labels)
                .unwrap_or_default();
            all_labels.resize_with(source_nvols, String::new);
            for (frame, label) in all_labels.iter_mut().enumerate() {
                if label.is_empty() {
                    *label = format!("#{frame}");
                }
            }
            let all_stats = nifti_reader_stats(&reader, source_header.as_ref())?;
            let selected = spec.resolve(source_nvols, &all_labels)?;
            let labels = selected
                .iter()
                .map(|&frame| all_labels[frame].clone())
                .collect();
            let stats = selected
                .iter()
                .map(|&frame| all_stats.get(frame).cloned().unwrap_or(None))
                .collect();
            Ok(Self {
                spec,
                source: VolumeReaderSource::Nifti(Box::new(reader)),
                selected,
                dimensions,
                ijk_to_ras,
                source_header,
                labels,
                stats,
            })
        }
    }

    /// Parsed source path and selector.
    pub fn spec(&self) -> &DatasetSpec {
        &self.spec
    }

    /// Source file format.
    pub fn format(&self) -> VolumeFormat {
        match self.source {
            VolumeReaderSource::Afni(_) => VolumeFormat::Afni,
            VolumeReaderSource::Nifti(_) => VolumeFormat::Nifti,
        }
    }

    /// Spatial dimensions `[nx, ny, nz]`.
    pub fn dimensions(&self) -> [usize; 3] {
        self.dimensions
    }

    /// Voxels per frame.
    pub fn voxels(&self) -> usize {
        self.dimensions.iter().product()
    }

    /// Number of logical frames after selection.
    pub fn nvols(&self) -> usize {
        self.selected.len()
    }

    /// Number of frames in the source before selection.
    pub fn source_nvols(&self) -> usize {
        match &self.source {
            VolumeReaderSource::Afni(reader) => reader.nvals(),
            VolumeReaderSource::Nifti(reader) => reader.nvols(),
        }
    }

    /// Source frame index for every logical frame. Order and duplicates are
    /// preserved exactly as written in the selector.
    pub fn selected_indices(&self) -> &[usize] {
        &self.selected
    }

    /// Voxel-to-RAS affine.
    pub fn ijk_to_ras(&self) -> Mat44 {
        self.ijk_to_ras
    }

    /// Spatial grid in the common RAS convention.
    pub fn grid(&self) -> GridSpec {
        GridSpec {
            dimensions: self.dimensions,
            ijk_to_ras: self.ijk_to_ras,
        }
    }

    /// AFNI attributes from a `.HEAD` or NIfTI extension, when present.
    /// Metadata describes the source dataset; [`labels`](Self::labels) and
    /// [`stats`](Self::stats) are the selector-adjusted views.
    pub fn afni_header(&self) -> Option<&Header> {
        self.source_header.as_ref()
    }

    /// One label per selected frame.
    pub fn labels(&self) -> &[String] {
        &self.labels
    }

    /// One statistical description per selected frame.
    pub fn stats(&self) -> &[Option<StatSpec>] {
        &self.stats
    }

    /// Whether the underlying voxel stream is gzip-compressed.
    pub fn is_compressed(&self) -> bool {
        match &self.source {
            VolumeReaderSource::Afni(reader) => reader.is_compressed(),
            VolumeReaderSource::Nifti(reader) => reader.is_compressed(),
        }
    }

    /// Load one logical frame as scaled `f32` values.
    pub fn frame(&mut self, frame: usize) -> Result<Vec<f32>> {
        let source_frame = self.source_frame(frame)?;
        match &mut self.source {
            VolumeReaderSource::Afni(reader) => reader.read_frame(source_frame),
            VolumeReaderSource::Nifti(reader) => reader.read_frame(source_frame),
        }
    }

    /// Copy one logical frame into a reusable caller-owned buffer.
    pub fn frame_into(&mut self, frame: usize, output: &mut [f32]) -> Result<()> {
        let source_frame = self.source_frame(frame)?;
        match &mut self.source {
            VolumeReaderSource::Afni(reader) => reader.read_frame_into(source_frame, output),
            VolumeReaderSource::Nifti(reader) => reader.read_frame_into(source_frame, output),
        }
    }

    /// Iterate over selected frames, loading one at a time.
    pub fn frames(&mut self) -> VolumeFrames<'_> {
        VolumeFrames {
            reader: self,
            next: 0,
        }
    }

    /// Load every selected frame and return the existing eager [`Volume`]
    /// representation. Frame order, duplicates, labels and statistics follow
    /// the selector. This is what [`read_any`] uses for a selected path.
    pub fn into_volume(self) -> Result<Volume> {
        let Self {
            source,
            selected,
            labels,
            stats,
            ..
        } = self;
        match source {
            VolumeReaderSource::Afni(mut reader) => {
                let mut sub_bricks = Vec::with_capacity(selected.len());
                for &source_frame in &selected {
                    sub_bricks.push(Some(reader.read_sub_brick(source_frame)?));
                }
                let mut header = reader.header().clone();
                select_header_metadata(&mut header, &selected, &labels, &stats)?;
                Ok(Volume::Afni(Brik {
                    header,
                    dimensions: reader.dimensions(),
                    sub_bricks,
                }))
            }
            VolumeReaderSource::Nifti(mut reader) => {
                let mut values = Vec::with_capacity(
                    reader
                        .voxels()
                        .checked_mul(selected.len())
                        .ok_or_else(|| Error::invalid("selected NIfTI size overflows"))?,
                );
                for &source_frame in &selected {
                    values.extend(reader.read_frame(source_frame)?);
                }
                let mut header = reader.header().clone();
                header.dim[0] = header.dim[0].max(4);
                header.dim[4] = selected.len() as i64;
                for dimension in &mut header.dim[5..] {
                    *dimension = 1;
                }
                // AFNI retains the source TR for any multi-frame selector,
                // including irregular and descending ones. A single selected
                // frame is treated as a bucket with no time axis.
                if selected.len() == 1 {
                    header.pixdim[4] = 0.0;
                    header.toffset = 0.0;
                }
                header.datatype = DataType::Float32.code();
                header.bitpix = i32::from(DataType::Float32.bitpix());
                header.scl_slope = 0.0;
                header.scl_inter = 0.0;
                let mut nifti = Nifti {
                    header,
                    extensions: reader.extensions().to_vec(),
                    data: TypedArray::Float32(values),
                };
                if let Some(mut afni_header) = reader.afni_header()? {
                    select_header_metadata(&mut afni_header, &selected, &labels, &stats)?;
                    afni_header.set("BRICK_TYPES", AttributeValue::Int(vec![3; selected.len()]));
                    afni_header.set(
                        "BRICK_FLOAT_FACS",
                        AttributeValue::Float(vec![0.0; selected.len()]),
                    );
                    nifti.set_afni_header(&afni_header);
                }
                Ok(Volume::Nifti(Box::new(nifti)))
            }
        }
    }

    /// Read the selected time series at flat voxel `index`.
    ///
    /// This remains memory-bounded to one frame. For compressed inputs it may
    /// be substantially slower than sequential frame processing.
    pub fn voxel_series(&mut self, index: usize) -> Result<Vec<f32>> {
        if index >= self.voxels() {
            return Err(Error::invalid(format!(
                "voxel index {index} is out of range for {} voxels",
                self.voxels()
            )));
        }
        let mut frame = vec![0.0; self.voxels()];
        let mut series = Vec::with_capacity(self.nvols());
        for logical in 0..self.nvols() {
            self.frame_into(logical, &mut frame)?;
            series.push(frame[index]);
        }
        Ok(series)
    }

    /// Read the selected time series at voxel `(i, j, k)`.
    pub fn voxel_series_ijk(&mut self, i: usize, j: usize, k: usize) -> Result<Vec<f32>> {
        let [nx, ny, nz] = self.dimensions;
        if i >= nx || j >= ny || k >= nz {
            return Err(Error::invalid(format!(
                "voxel ({i},{j},{k}) is outside grid [{nx},{ny},{nz}]"
            )));
        }
        self.voxel_series(i + nx * (j + ny * k))
    }

    fn source_frame(&self, logical: usize) -> Result<usize> {
        self.selected.get(logical).copied().ok_or_else(|| {
            Error::invalid(format!(
                "selected frame {logical} is outside 0..{}",
                self.nvols().saturating_sub(1)
            ))
        })
    }
}

/// Iterator returned by [`VolumeReader::frames`].
#[derive(Debug)]
pub struct VolumeFrames<'a> {
    reader: &'a mut VolumeReader,
    next: usize,
}

impl Iterator for VolumeFrames<'_> {
    type Item = Result<Vec<f32>>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.next >= self.reader.nvols() {
            return None;
        }
        let frame = self.reader.frame(self.next);
        self.next += 1;
        Some(frame)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let remaining = self.reader.nvols().saturating_sub(self.next);
        (remaining, Some(remaining))
    }
}

impl ExactSizeIterator for VolumeFrames<'_> {}

fn nifti_reader_stats(
    reader: &NiftiReader,
    afni_header: Option<&Header>,
) -> Result<Vec<Option<StatSpec>>> {
    if let Some(header) = afni_header {
        let mut stats = header.brick_stats();
        stats.resize(reader.nvols(), None);
        return Ok(stats);
    }
    let header = reader.header();
    let per_voxel = header.shape().get(4).copied().unwrap_or(1) > 1;
    let stat = if per_voxel {
        None
    } else {
        StatSpec::from_intent(
            i64::from(header.intent_code),
            [header.intent_p1, header.intent_p2, header.intent_p3],
            afni_core::stat::IntentOrigin::Unknown,
        )
        .map_err(|error| Error::invalid(format!("NIfTI statistic: {error}")))?
    };
    Ok(vec![stat; reader.nvols()])
}

fn select_header_metadata(
    header: &mut Header,
    selected: &[usize],
    labels: &[String],
    stats: &[Option<StatSpec>],
) -> Result<()> {
    let types = selected
        .iter()
        .map(|&frame| header.brick_type_code(frame))
        .collect();
    let factors = selected
        .iter()
        .map(|&frame| header.brick_factor(frame))
        .collect();
    let ranges = header.floats("BRICK_STATS").and_then(|ranges| {
        selected
            .iter()
            .map(|&frame| Some([*ranges.get(2 * frame)?, *ranges.get(2 * frame + 1)?]))
            .collect::<Option<Vec<_>>>()
    });
    let time_axis = header.time_axis();
    header.set(
        "DATASET_RANK",
        AttributeValue::Int(vec![3, selected.len() as i64]),
    );
    header.set("BRICK_TYPES", AttributeValue::Int(types));
    header.set("BRICK_FLOAT_FACS", AttributeValue::Float(factors));
    if let Some(ranges) = ranges {
        header.set(
            "BRICK_STATS",
            AttributeValue::Float(ranges.into_iter().flatten().collect()),
        );
    } else {
        header.remove("BRICK_STATS");
    }
    header.set_brick_labels(labels)?;
    header.set_brick_stats(stats)?;

    if let Some(mut axis) = time_axis {
        if selected.len() > 1 {
            axis.nt = selected.len();
            header.set_time_axis(&axis)?;
        } else {
            header.clear_time_axis();
        }
    }
    Ok(())
}

/// Format-neutral spatial grid used for compatibility checks and masks.
#[derive(Debug, Clone, PartialEq)]
pub struct GridSpec {
    /// Voxel dimensions `[nx, ny, nz]`.
    pub dimensions: [usize; 3],
    /// Voxel-to-RAS affine.
    pub ijk_to_ras: Mat44,
}

impl GridSpec {
    /// Number of voxels in the grid.
    pub fn voxels(&self) -> usize {
        self.dimensions.iter().product()
    }

    /// Compare dimensions and affine entries. `tolerance` is an absolute
    /// tolerance in the affine's millimetre units.
    pub fn compatibility(&self, other: &Self, tolerance: f64) -> GridCompatibility {
        let max_affine_difference = self
            .ijk_to_ras
            .iter()
            .flatten()
            .zip(other.ijk_to_ras.iter().flatten())
            .map(|(a, b)| (a - b).abs())
            .fold(0.0_f64, f64::max);
        GridCompatibility {
            dimensions_match: self.dimensions == other.dimensions,
            affine_matches: tolerance.is_finite()
                && tolerance >= 0.0
                && max_affine_difference <= tolerance,
            max_affine_difference,
        }
    }
}

/// Detailed result of comparing two volume grids.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GridCompatibility {
    /// Whether `[nx, ny, nz]` matches exactly.
    pub dimensions_match: bool,
    /// Whether every affine entry is within the requested tolerance.
    pub affine_matches: bool,
    /// Largest absolute difference between corresponding affine entries.
    pub max_affine_difference: f64,
}

impl GridCompatibility {
    /// Whether both dimensions and affine match.
    pub fn is_compatible(self) -> bool {
        self.dimensions_match && self.affine_matches
    }

    /// Return a diagnostic error unless the grids are compatible.
    pub fn require_compatible(self) -> Result<()> {
        if self.is_compatible() {
            Ok(())
        } else {
            Err(Error::invalid(format!(
                "volume grid mismatch: dimensions_match={}, affine_matches={}, \
                 max_affine_difference={}",
                self.dimensions_match, self.affine_matches, self.max_affine_difference
            )))
        }
    }
}

/// A validated boolean mask tied to a spatial grid.
#[derive(Debug, Clone, PartialEq)]
pub struct VolumeMask {
    grid: GridSpec,
    values: Vec<bool>,
}

impl VolumeMask {
    /// Build a mask from one volume, selecting finite nonzero values. AFNI's
    /// float extraction likewise removes NaN and infinity before mask creation.
    pub fn from_nonzero(volume: &Volume, frame: usize) -> Result<Self> {
        let values = volume
            .frame(frame)?
            .into_iter()
            .map(|value| value.is_finite() && value != 0.0)
            .collect();
        Ok(Self {
            grid: volume.grid()?,
            values,
        })
    }

    /// Build from explicit values and a grid.
    pub fn new(grid: GridSpec, values: Vec<bool>) -> Result<Self> {
        if values.len() != grid.voxels() {
            return Err(Error::invalid(format!(
                "mask has {} voxels but grid has {}",
                values.len(),
                grid.voxels()
            )));
        }
        Ok(Self { grid, values })
    }

    /// The mask's spatial grid.
    pub fn grid(&self) -> &GridSpec {
        &self.grid
    }

    /// Boolean mask values in i-fastest voxel order.
    pub fn values(&self) -> &[bool] {
        &self.values
    }

    /// Number of selected voxels.
    pub fn count(&self) -> usize {
        self.values.iter().filter(|&&value| value).count()
    }

    /// Require this mask to match `grid`.
    pub fn require_grid(&self, grid: &GridSpec, tolerance: f64) -> Result<()> {
        self.grid
            .compatibility(grid, tolerance)
            .require_compatible()
    }
}

/// Read an AFNI dataset or a NIfTI volume, choosing by name.
///
/// A terminal AFNI sub-brick selector is accepted for either format. Selected
/// frames are returned in selector order with adjusted labels, statistics and
/// timing metadata. Selected NIfTI values are materialized as scaled `f32`;
/// use [`VolumeReader`] to keep any format frame-at-a-time.
pub fn read_any(path: impl AsRef<Path>) -> Result<Volume> {
    let spec = DatasetSpec::from_path(path)?;
    if spec.selector().is_some() {
        VolumeReader::open_spec(spec)?.into_volume()
    } else {
        read_volumes(spec.path(), None)
    }
}

/// Like [`read_any`], but for an AFNI dataset only the sub-bricks in
/// `volumes` are read ([`Brik::read_sub_bricks`]); the rest give `None` from
/// [`Volume::frame_f32`]. A NIfTI file is read whole.
pub fn read_any_volumes(path: impl AsRef<Path>, volumes: &[usize]) -> Result<Volume> {
    let spec = DatasetSpec::from_path(path)?;
    if spec.selector().is_some() {
        return Err(Error::invalid(
            "use either a trailing dataset selector or read_any_volumes indices, not both",
        ));
    }
    read_volumes(spec.path(), Some(volumes))
}

fn read_volumes(path: &Path, volumes: Option<&[usize]>) -> Result<Volume> {
    if AfniPaths::is_afni_name(path) {
        Ok(Volume::Afni(match volumes {
            Some(v) => Brik::read_sub_bricks(path, v)?,
            None => Brik::read(path)?,
        }))
    } else {
        Ok(Volume::Nifti(Box::new(Nifti::read(path)?)))
    }
}

impl Volume {
    /// Spatial grid in the common RAS convention.
    pub fn grid(&self) -> Result<GridSpec> {
        Ok(GridSpec {
            dimensions: self.dimensions(),
            ijk_to_ras: self.ijk_to_ras()?,
        })
    }

    /// Grid size `[nx, ny, nz]`. A NIfTI image with fewer than three
    /// dimensions is padded with 1s.
    pub fn dimensions(&self) -> [usize; 3] {
        match self {
            Volume::Afni(b) => b.dimensions,
            Volume::Nifti(n) => {
                let shape = n.shape();
                let dim = |i: usize| shape.get(i).copied().unwrap_or(1).max(1);
                [dim(0), dim(1), dim(2)]
            }
        }
    }

    /// Voxels per volume.
    pub fn voxels(&self) -> usize {
        self.dimensions().iter().product()
    }

    /// Number of volumes: sub-bricks for AFNI; for NIfTI the product of
    /// dimensions 4 and up (time, then any further dimensions).
    pub fn nvols(&self) -> usize {
        match self {
            Volume::Afni(b) => b.nvals(),
            Volume::Nifti(n) => n.shape().iter().skip(3).product::<usize>().max(1),
        }
    }

    /// The `ijk -> RAS` matrix: AFNI's [`Header::ijk_to_ras`], or the NIfTI
    /// sform/qform affine (sform first, AFNI's default as well).
    pub fn ijk_to_ras(&self) -> Result<Mat44> {
        match self {
            Volume::Afni(b) => b.header.ijk_to_ras(),
            Volume::Nifti(n) => Ok(n.header.affine()),
        }
    }

    /// The `ijk -> DICOM (RAI)` matrix, AFNI's native convention.
    pub fn ijk_to_dicom(&self) -> Result<Mat44> {
        Ok(dicom_to_ras(&self.ijk_to_ras()?))
    }

    /// The AFNI attributes: the `.HEAD` of an AFNI dataset, or the AFNI
    /// extension of a NIfTI file (`None` if it has none).
    pub fn afni_header(&self) -> Result<Option<Header>> {
        match self {
            Volume::Afni(b) => Ok(Some(b.header.clone())),
            Volume::Nifti(n) => n.afni_header(),
        }
    }

    /// One label per volume, from the AFNI attributes when there are any,
    /// otherwise AFNI's defaults `#0`, `#1`, ...
    pub fn labels(&self) -> Result<Vec<String>> {
        Ok(match self.afni_header()? {
            Some(h) => {
                let mut labels = h.brick_labels();
                labels.resize_with(self.nvols(), String::new);
                labels
                    .into_iter()
                    .enumerate()
                    .map(|(p, l)| if l.is_empty() { format!("#{p}") } else { l })
                    .collect()
            }
            None => (0..self.nvols()).map(|p| format!("#{p}")).collect(),
        })
    }

    /// The statistic in each volume. AFNI attributes win when present.
    /// Otherwise a NIfTI statistical intent applies to every volume, as in
    /// AFNI's `thd_niftiread.c`, except when `dim[5] > 1` (per-voxel
    /// parameters, which AFNI does not support either).
    ///
    /// For a header intent, correlation parameters are classified by structure
    /// (see [`afni_core::stat::StatSpec::from_intent`]); a malformed set is an
    /// error, not a missing statistic. Use
    /// [`stats_with_origin`](Self::stats_with_origin) to state who wrote them.
    pub fn stats(&self) -> Result<Vec<Option<StatSpec>>> {
        self.stats_with_origin(afni_core::stat::IntentOrigin::Unknown)
    }

    /// Like [`stats`](Self::stats), with the writer of a NIfTI header intent
    /// stated. `origin` only affects header intents (never AFNI attributes) and
    /// only correlation.
    pub fn stats_with_origin(
        &self,
        origin: afni_core::stat::IntentOrigin,
    ) -> Result<Vec<Option<StatSpec>>> {
        if let Some(h) = self.afni_header()? {
            let mut stats = h.brick_stats();
            stats.resize(self.nvols(), None);
            return Ok(stats);
        }
        let Volume::Nifti(n) = self else {
            unreachable!("an AFNI volume always has a header");
        };
        let h = &n.header;
        let per_voxel = h.shape().get(4).copied().unwrap_or(1) > 1;
        let stat = if per_voxel {
            None
        } else {
            StatSpec::from_intent(
                i64::from(h.intent_code),
                [h.intent_p1, h.intent_p2, h.intent_p3],
                origin,
            )
            .map_err(|e| Error::invalid(format!("NIfTI statistic: {e}")))?
        };
        Ok(vec![stat; self.nvols()])
    }

    /// Volume `t` as scaled `f32` values in `i + nx * (j + ny * k)` order.
    /// `None` when `t` is out of range, the sub-brick was not read, or the
    /// data has no scalar value (AFNI RGB/RGBA). NaN and Inf are kept.
    pub fn frame_f32(&self, t: usize) -> Option<Vec<f32>> {
        match self {
            Volume::Afni(b) => b.sub_brick(t)?.to_f32(),
            Volume::Nifti(n) => {
                if t >= self.nvols() {
                    return None;
                }
                let voxels = self.voxels();
                let start = t * voxels;
                (start..start + voxels)
                    .map(|i| n.get_scaled(i).map(|v| v as f32))
                    .collect()
            }
        }
    }

    /// One volume as scaled `f32` values, with a diagnostic error that
    /// distinguishes an invalid index from unloaded or non-scalar data.
    pub fn frame(&self, t: usize) -> Result<Vec<f32>> {
        if t >= self.nvols() {
            return Err(Error::invalid(format!(
                "volume {t} is out of range for {} volumes",
                self.nvols()
            )));
        }
        self.frame_f32(t).ok_or_else(|| {
            Error::unsupported(format!(
                "volume {t} was not loaded or does not contain scalar data"
            ))
        })
    }

    /// Copy the scaled time series at flat voxel `index` into a reusable
    /// caller-owned buffer.
    pub fn voxel_series_into(&self, index: usize, output: &mut [f32]) -> Result<()> {
        if index >= self.voxels() {
            return Err(Error::invalid(format!(
                "voxel index {index} is out of range for {} voxels",
                self.voxels()
            )));
        }
        if output.len() != self.nvols() {
            return Err(Error::invalid(format!(
                "time-series buffer has {} values but volume has {} frames",
                output.len(),
                self.nvols()
            )));
        }
        match self {
            Volume::Afni(brik) => brik.voxel_series_into(index, output),
            Volume::Nifti(nifti) => {
                let stride = self.voxels();
                for (t, value) in output.iter_mut().enumerate() {
                    *value = nifti.get_scaled(t * stride + index).ok_or_else(|| {
                        Error::invalid(format!("NIfTI has no value at voxel {index}, frame {t}"))
                    })? as f32;
                }
                Ok(())
            }
        }
    }

    /// The scaled time series at flat voxel `index`.
    pub fn voxel_series(&self, index: usize) -> Result<Vec<f32>> {
        let mut values = vec![0.0; self.nvols()];
        self.voxel_series_into(index, &mut values)?;
        Ok(values)
    }

    /// The scaled time series at voxel `(i, j, k)`.
    pub fn voxel_series_ijk(&self, i: usize, j: usize, k: usize) -> Result<Vec<f32>> {
        let [nx, ny, nz] = self.dimensions();
        if i >= nx || j >= ny || k >= nz {
            return Err(Error::invalid(format!(
                "voxel ({i},{j},{k}) is outside grid [{nx},{ny},{nz}]"
            )));
        }
        self.voxel_series(i + nx * (j + ny * k))
    }

    /// The scaled value at voxel `(i, j, k)` of volume `t`.
    pub fn value(&self, i: usize, j: usize, k: usize, t: usize) -> Option<f32> {
        match self {
            Volume::Afni(b) => b.value(i, j, k, t),
            Volume::Nifti(n) => {
                let [nx, ny, nz] = self.dimensions();
                if i >= nx || j >= ny || k >= nz || t >= self.nvols() {
                    return None;
                }
                n.get_scaled(t * self.voxels() + i + nx * (j + ny * k))
                    .map(|v| v as f32)
            }
        }
    }

    /// Write this loaded volume as AFNI or NIfTI, choosing from the output
    /// name. Values are materialized through the format-neutral core envelope,
    /// which also preserves understood labels, statistics, timing, curves,
    /// label tables, history, and private AFNI attributes.
    pub fn write(&self, path: impl AsRef<Path>) -> Result<crate::adapt::WrittenVolume> {
        crate::adapt::volume_to_envelope(self)?.write(path)
    }

    /// Write this loaded volume with explicit format, overwrite, storage,
    /// history, and NIfTI-version policy.
    pub fn write_with_options(
        &self,
        path: impl AsRef<Path>,
        options: &crate::adapt::VolumeWriteOptions,
    ) -> Result<crate::adapt::WrittenVolume> {
        crate::adapt::volume_to_envelope(self)?.write_with_options(path, options)
    }

    /// The underlying AFNI dataset, if this is one.
    pub fn as_afni(&self) -> Option<&Brik> {
        match self {
            Volume::Afni(b) => Some(b),
            Volume::Nifti(_) => None,
        }
    }

    /// The underlying NIfTI volume, if this is one.
    pub fn as_nifti(&self) -> Option<&Nifti> {
        match self {
            Volume::Nifti(n) => Some(n),
            Volume::Afni(_) => None,
        }
    }
}

impl TryFrom<Volume> for Brik {
    type Error = Error;

    fn try_from(volume: Volume) -> Result<Self> {
        match volume {
            Volume::Afni(b) => Ok(b),
            Volume::Nifti(_) => Err(Error::invalid("not an AFNI dataset")),
        }
    }
}
