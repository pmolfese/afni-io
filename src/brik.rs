//! AFNI volume datasets: the `.HEAD`/`.BRIK` pair.
//!
//! An AFNI volume is a single dataset split across two files. The `.HEAD` holds
//! the ASCII attributes parsed by [`crate::head`] (geometry, datum type, byte
//! order, scale factors); the `.BRIK` (or gzipped `.BRIK.gz`) holds the raw
//! binary voxel data those attributes describe. The two always travel together
//! and share a base name, so [`Brik::read`] takes any member of the pair, or
//! the bare `prefix+view` name, and loads the whole dataset. [`AfniPaths`]
//! exposes that name resolution on its own.
//!
//! Sub-bricks are stored back-to-back: for sub-brick *p* there are `nx*ny*nz`
//! voxels of the datum named by `BRICK_TYPES[p]`, in `(i + j*nx + k*nx*ny)`
//! order. Each sub-brick is kept in its stored type ([`BrickData`]) together
//! with its `BRICK_FLOAT_FACS` scale factor; [`SubBrick::value`] and
//! [`SubBrick::to_f32`] give scaled "true" values for the scalar datums.
//!
//! Behaviour follows AFNI's own loader:
//!
//! * A missing `BRICK_TYPES` means every sub-brick is short; a list shorter
//!   than the number of sub-bricks repeats its last entry
//!   (`THD_init_datablock_brick`, `thd_initdblk.c`).
//! * A missing or unrecognised `BYTEORDER_STRING` means host byte order
//!   (`thd_initdblk.c`).
//! * A scale factor of 0 means unscaled; any other factor, including a
//!   negative one, multiplies the stored value (`THD_extract_float_brick`,
//!   `thd_dsetto3D.c`).
//!
//! Two deliberate differences from AFNI:
//!
//! * NaN and Inf are returned as stored. AFNI's `THD_extract_float_brick`
//!   replaces them with 0 (`thd_floatscan`); callers that want that should do
//!   it themselves.
//! * RGB and RGBA sub-bricks have no scalar value here. AFNI converts them to
//!   luminance (`0.299 R + 0.587 G + 0.114 B`), which is a display decision
//!   left to the caller. Read [`BrickData::Rgb`] / [`BrickData::Rgba`].
//!
//! Double sub-bricks are byte-swapped like every other multi-byte datum.
//! AFNI's loader (`thd_loaddblk.c`) does not swap doubles, so a big-endian
//! double dataset read by AFNI on a little-endian host would be garbled; this
//! crate reads the bytes as the header describes them.
//!
//! References: `afni/src/thd_initdblk.c`, `thd_loaddblk.c`, `thd_dsetto3D.c`,
//! `mrilib.h` (`MRI_TYPE`), `README.attributes`, `matlab/BrikLoad.m`.

use std::fs::{self, File, OpenOptions};
use std::io::{self, BufReader, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use crate::error::{Error, Result};
use crate::geometry::{Mat44, Orientation, TimeAxis, View};
use crate::head::{AttributeValue, Header};

/// AFNI sub-brick storage types (the `BRICK_TYPES` codes, which are the
/// `MRI_TYPE` enum in `mrilib.h`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BrikType {
    /// Code 0: unsigned 8-bit.
    Byte,
    /// Code 1: signed 16-bit.
    Short,
    /// Code 2: signed 32-bit.
    Int,
    /// Code 3: 32-bit IEEE float.
    Float,
    /// Code 4: 64-bit IEEE float.
    Double,
    /// Code 5: complex (two 32-bit floats: real, imaginary).
    Complex,
    /// Code 6: three unsigned bytes (red, green, blue).
    Rgb,
    /// Code 7: four unsigned bytes (red, green, blue, alpha).
    Rgba,
}

impl BrikType {
    /// Map an AFNI `BRICK_TYPES` code onto a variant.
    pub fn from_code(code: i64) -> Result<Self> {
        Ok(match code {
            0 => Self::Byte,
            1 => Self::Short,
            2 => Self::Int,
            3 => Self::Float,
            4 => Self::Double,
            5 => Self::Complex,
            6 => Self::Rgb,
            7 => Self::Rgba,
            other => return Err(Error::invalid(format!("unknown BRICK_TYPES code {other}"))),
        })
    }

    /// The `BRICK_TYPES` code for this variant.
    pub fn code(self) -> i64 {
        match self {
            Self::Byte => 0,
            Self::Short => 1,
            Self::Int => 2,
            Self::Float => 3,
            Self::Double => 4,
            Self::Complex => 5,
            Self::Rgb => 6,
            Self::Rgba => 7,
        }
    }

    /// Bytes occupied by one voxel of this type.
    pub fn byte_width(self) -> usize {
        match self {
            Self::Byte => 1,
            Self::Short => 2,
            Self::Rgb => 3,
            Self::Int | Self::Float | Self::Rgba => 4,
            Self::Double | Self::Complex => 8,
        }
    }
}

/// The voxel values of one sub-brick, in the type they are stored as.
///
/// Values are exactly as stored on disk (after byte-order correction); the
/// sub-brick's scale factor is held separately in [`SubBrick::factor`].
#[derive(Debug, Clone, PartialEq)]
pub enum BrickData {
    /// [`BrikType::Byte`].
    Byte(Vec<u8>),
    /// [`BrikType::Short`].
    Short(Vec<i16>),
    /// [`BrikType::Int`].
    Int(Vec<i32>),
    /// [`BrikType::Float`].
    Float(Vec<f32>),
    /// [`BrikType::Double`].
    Double(Vec<f64>),
    /// [`BrikType::Complex`], as `[real, imaginary]`.
    Complex(Vec<[f32; 2]>),
    /// [`BrikType::Rgb`], as `[r, g, b]`.
    Rgb(Vec<[u8; 3]>),
    /// [`BrikType::Rgba`], as `[r, g, b, a]`.
    Rgba(Vec<[u8; 4]>),
}

impl BrickData {
    /// The datum type of this data.
    pub fn brik_type(&self) -> BrikType {
        match self {
            Self::Byte(_) => BrikType::Byte,
            Self::Short(_) => BrikType::Short,
            Self::Int(_) => BrikType::Int,
            Self::Float(_) => BrikType::Float,
            Self::Double(_) => BrikType::Double,
            Self::Complex(_) => BrikType::Complex,
            Self::Rgb(_) => BrikType::Rgb,
            Self::Rgba(_) => BrikType::Rgba,
        }
    }

    /// Number of voxels.
    pub fn len(&self) -> usize {
        match self {
            Self::Byte(v) => v.len(),
            Self::Short(v) => v.len(),
            Self::Int(v) => v.len(),
            Self::Float(v) => v.len(),
            Self::Double(v) => v.len(),
            Self::Complex(v) => v.len(),
            Self::Rgb(v) => v.len(),
            Self::Rgba(v) => v.len(),
        }
    }

    /// Whether there are no voxels.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The unscaled scalar value at `index`: the stored number for scalar
    /// types, the magnitude for complex (as AFNI's `THD_extract_float_brick`
    /// does), and `None` for RGB/RGBA or an out-of-range index.
    pub fn raw_scalar(&self, index: usize) -> Option<f64> {
        Some(match self {
            Self::Byte(v) => f64::from(*v.get(index)?),
            Self::Short(v) => f64::from(*v.get(index)?),
            Self::Int(v) => f64::from(*v.get(index)?),
            Self::Float(v) => f64::from(*v.get(index)?),
            Self::Double(v) => *v.get(index)?,
            Self::Complex(v) => {
                let [re, im] = *v.get(index)?;
                f64::from(re).hypot(f64::from(im))
            }
            Self::Rgb(_) | Self::Rgba(_) => return None,
        })
    }

    /// The voxels as little-endian bytes, the layout written to a `.BRIK`.
    pub fn to_le_bytes(&self) -> Vec<u8> {
        match self {
            Self::Byte(v) => v.clone(),
            Self::Short(v) => v.iter().flat_map(|x| x.to_le_bytes()).collect(),
            Self::Int(v) => v.iter().flat_map(|x| x.to_le_bytes()).collect(),
            Self::Float(v) => v.iter().flat_map(|x| x.to_le_bytes()).collect(),
            Self::Double(v) => v.iter().flat_map(|x| x.to_le_bytes()).collect(),
            Self::Complex(v) => v
                .iter()
                .flat_map(|[re, im]| [re.to_le_bytes(), im.to_le_bytes()].concat())
                .collect(),
            Self::Rgb(v) => v.iter().flatten().copied().collect(),
            Self::Rgba(v) => v.iter().flatten().copied().collect(),
        }
    }

    /// Decode `bytes` (exactly `voxels * ty.byte_width()` long) as `ty`.
    fn decode(ty: BrikType, bytes: &[u8], little: bool) -> Self {
        fn arr<const N: usize>(chunk: &[u8], little: bool) -> [u8; N] {
            let mut out = [0u8; N];
            out.copy_from_slice(chunk);
            if !little {
                out.reverse();
            }
            out
        }
        // Every multi-byte scalar is decoded little-endian after `arr` has put
        // its bytes in little-endian order.
        match ty {
            BrikType::Byte => Self::Byte(bytes.to_vec()),
            BrikType::Short => Self::Short(
                bytes
                    .chunks_exact(2)
                    .map(|c| i16::from_le_bytes(arr(c, little)))
                    .collect(),
            ),
            BrikType::Int => Self::Int(
                bytes
                    .chunks_exact(4)
                    .map(|c| i32::from_le_bytes(arr(c, little)))
                    .collect(),
            ),
            BrikType::Float => Self::Float(
                bytes
                    .chunks_exact(4)
                    .map(|c| f32::from_le_bytes(arr(c, little)))
                    .collect(),
            ),
            BrikType::Double => Self::Double(
                bytes
                    .chunks_exact(8)
                    .map(|c| f64::from_le_bytes(arr(c, little)))
                    .collect(),
            ),
            // Each half of a complex voxel is swapped on its own, as
            // `mri_swap4(2*nvox, ...)` does in `thd_loaddblk.c`.
            BrikType::Complex => Self::Complex(
                bytes
                    .chunks_exact(8)
                    .map(|c| {
                        [
                            f32::from_le_bytes(arr(&c[..4], little)),
                            f32::from_le_bytes(arr(&c[4..], little)),
                        ]
                    })
                    .collect(),
            ),
            BrikType::Rgb => Self::Rgb(bytes.chunks_exact(3).map(|c| [c[0], c[1], c[2]]).collect()),
            BrikType::Rgba => Self::Rgba(
                bytes
                    .chunks_exact(4)
                    .map(|c| [c[0], c[1], c[2], c[3]])
                    .collect(),
            ),
        }
    }
}

/// One loaded sub-brick: its stored data and its scale factor.
#[derive(Debug, Clone, PartialEq)]
pub struct SubBrick {
    /// The voxel values as stored.
    pub data: BrickData,
    /// `BRICK_FLOAT_FACS` for this sub-brick. `0.0` means unscaled.
    pub factor: f32,
}

/// How true floating-point values should be stored in an AFNI sub-brick.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum StoragePolicy {
    /// Store every value directly as an IEEE `float` with no scale factor.
    Float,
    /// Store signed shorts and choose a scale factor from the largest absolute
    /// finite value, like `EDIT_substscale_brick(..., MRI_short, -1.0)`.
    AutoShort,
    /// Store signed shorts using this explicit `BRICK_FLOAT_FACS` value. The
    /// factor must be finite and positive.
    Short { factor: f32 },
}

/// A non-allocating iterator over the true, scaled scalar values in a sub-brick.
#[derive(Debug, Clone)]
pub struct ScaledValues<'a> {
    sub_brick: &'a SubBrick,
    index: usize,
}

impl Iterator for ScaledValues<'_> {
    type Item = f32;

    fn next(&mut self) -> Option<Self::Item> {
        let value = self.sub_brick.value(self.index)?;
        self.index += 1;
        Some(value)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let remaining = self.sub_brick.len().saturating_sub(self.index);
        (remaining, Some(remaining))
    }
}

impl ExactSizeIterator for ScaledValues<'_> {}

impl SubBrick {
    /// Build a scalar sub-brick from true floating-point values according to a
    /// storage policy.
    pub fn from_f32(values: Vec<f32>, storage: StoragePolicy) -> Result<Self> {
        if values.is_empty() {
            return Err(Error::invalid("a sub-brick cannot be empty"));
        }
        match storage {
            StoragePolicy::Float => Ok(Self {
                data: BrickData::Float(values),
                factor: 0.0,
            }),
            StoragePolicy::AutoShort => {
                let largest = values
                    .iter()
                    .copied()
                    .filter(|v| v.is_finite())
                    .map(f32::abs)
                    .fold(0.0_f32, f32::max);
                if largest == 0.0 {
                    return Ok(Self {
                        data: BrickData::Short(vec![0; values.len()]),
                        factor: 0.0,
                    });
                }
                Self::from_f32(
                    values,
                    StoragePolicy::Short {
                        factor: largest / i16::MAX as f32,
                    },
                )
            }
            StoragePolicy::Short { factor } => {
                if !factor.is_finite() || factor <= 0.0 {
                    return Err(Error::invalid(format!(
                        "short scale factor must be finite and positive, got {factor}"
                    )));
                }
                let data = values
                    .into_iter()
                    .map(|value| {
                        if value.is_nan() {
                            0
                        } else {
                            (value / factor)
                                .round()
                                .clamp(i16::MIN as f32, i16::MAX as f32)
                                as i16
                        }
                    })
                    .collect();
                Ok(Self {
                    data: BrickData::Short(data),
                    factor,
                })
            }
        }
    }

    /// The datum type.
    pub fn brik_type(&self) -> BrikType {
        self.data.brik_type()
    }

    /// Number of voxels.
    pub fn len(&self) -> usize {
        self.data.len()
    }

    /// Whether there are no voxels.
    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }

    /// The multiplier applied to stored values: the factor, or 1 when the
    /// factor is 0.
    pub fn scale(&self) -> f32 {
        if self.factor == 0.0 {
            1.0
        } else {
            self.factor
        }
    }

    /// The scaled value at flat voxel `index`. Complex sub-bricks give their
    /// scaled magnitude; RGB/RGBA give `None` (see the module docs).
    pub fn value(&self, index: usize) -> Option<f32> {
        self.data
            .raw_scalar(index)
            .map(|v| (v * f64::from(self.scale())) as f32)
    }

    /// The `[min, max]` AFNI records in `BRICK_STATS`: the range of the
    /// scaled finite values, using magnitude for complex and luminance
    /// (`0.299 R + 0.587 G + 0.114 B`) for RGB/RGBA, as `THD_load_statistics`
    /// does. `[0, 0]` when there are no finite values.
    pub fn stats(&self) -> [f64; 2] {
        let luminance = |r: u8, g: u8, b: u8| {
            0.299 * f64::from(r) + 0.587 * f64::from(g) + 0.114 * f64::from(b)
        };
        let scale = f64::from(self.scale());
        let values: Box<dyn Iterator<Item = f64>> = match &self.data {
            BrickData::Rgb(v) => Box::new(v.iter().map(move |&[r, g, b]| luminance(r, g, b))),
            BrickData::Rgba(v) => Box::new(v.iter().map(move |&[r, g, b, _]| luminance(r, g, b))),
            _ => Box::new(
                (0..self.len())
                    .filter_map(|i| self.data.raw_scalar(i))
                    .map(move |v| v * scale),
            ),
        };
        values
            .filter(|v| v.is_finite())
            .fold(None, |range: Option<[f64; 2]>, v| {
                Some(range.map_or([v, v], |[lo, hi]| [lo.min(v), hi.max(v)]))
            })
            .unwrap_or([0.0, 0.0])
    }

    /// Every voxel as a scaled `f32`, in `(i + j*nx + k*nx*ny)` order, or
    /// `None` for RGB/RGBA.
    pub fn to_f32(&self) -> Option<Vec<f32>> {
        let scale = self.scale();
        let scaled = |v: f32| if scale == 1.0 { v } else { v * scale };
        Some(match &self.data {
            BrickData::Byte(v) => v.iter().map(|&x| scaled(f32::from(x))).collect(),
            BrickData::Short(v) => v.iter().map(|&x| scaled(f32::from(x))).collect(),
            BrickData::Float(v) => v.iter().map(|&x| scaled(x)).collect(),
            BrickData::Int(_) | BrickData::Double(_) | BrickData::Complex(_) => (0..self.len())
                .map(|i| self.value(i).unwrap_or(f32::NAN))
                .collect(),
            BrickData::Rgb(_) | BrickData::Rgba(_) => return None,
        })
    }

    /// Iterate over scaled scalar values without allocating. RGB and RGBA
    /// sub-bricks are rejected because they have no scalar interpretation in
    /// `afni-io`.
    pub fn scaled_values(&self) -> Result<ScaledValues<'_>> {
        if matches!(self.data, BrickData::Rgb(_) | BrickData::Rgba(_)) {
            return Err(Error::unsupported(
                "RGB/RGBA sub-brick has no scalar values",
            ));
        }
        Ok(ScaledValues {
            sub_brick: self,
            index: 0,
        })
    }

    /// Copy scaled scalar values into a caller-owned buffer, allowing the same
    /// allocation to be reused across sub-bricks.
    pub fn copy_scaled_into(&self, output: &mut [f32]) -> Result<()> {
        if output.len() != self.len() {
            return Err(Error::invalid(format!(
                "output buffer has {} values but sub-brick has {}",
                output.len(),
                self.len()
            )));
        }
        for (dst, value) in output.iter_mut().zip(self.scaled_values()?) {
            *dst = value;
        }
        Ok(())
    }
}

/// The two files of an AFNI dataset, as resolved from any name AFNI accepts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AfniPaths {
    /// The `.HEAD` file (always exists after [`AfniPaths::resolve`]).
    pub head: PathBuf,
    /// The `.BRIK` or `.BRIK.gz` holding the voxels, or `None` when neither
    /// exists (a header-only dataset).
    pub brik: Option<PathBuf>,
}

/// Command-level policy for writing an AFNI HEAD/BRIK pair.
///
/// Writes are transactionally staged and flushed regardless of these options.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrikWriteOptions {
    /// Whether an existing target pair may be replaced.
    pub overwrite: bool,
    /// Optional command-history line appended to `HISTORY_NOTE` before writing.
    pub history_entry: Option<String>,
}

impl Default for BrikWriteOptions {
    fn default() -> Self {
        Self {
            // Preserve the historical behavior of `Brik::write`.
            overwrite: true,
            history_entry: None,
        }
    }
}

/// Suffixes that name an AFNI dataset file, longest first so that
/// `.BRIK.gz` is matched before `.BRIK`.
const FILE_SUFFIXES: [&str; 5] = [".BRIK.bz2", ".BRIK.gz", ".BRIK.Z", ".BRIK", ".HEAD"];
/// AFNI view names, as the `+view` part of `prefix+view`.
pub(crate) const VIEWS: [&str; 3] = ["+orig", "+acpc", "+tlrc"];

impl AfniPaths {
    /// Whether `path` is spelled like an AFNI dataset: it ends in `.HEAD`,
    /// `.BRIK`, a compressed `.BRIK.*`, or a bare `+orig`, `+acpc` or `+tlrc`
    /// (with or without a trailing `.`). The filesystem is not consulted.
    pub fn is_afni_name(path: impl AsRef<Path>) -> bool {
        Self::base_name(path.as_ref()).is_some()
    }

    /// Resolve any AFNI dataset name to its `.HEAD` and `.BRIK` files.
    ///
    /// Accepts `name+view.HEAD`, `name+view.BRIK`, `name+view.BRIK.gz`, and
    /// `name+view` or `name+view.` for the views `orig`, `acpc` and `tlrc`.
    /// When both `.BRIK` and `.BRIK.gz` exist, the uncompressed one is used
    /// unless the `.BRIK.gz` was named explicitly. `.BRIK.bz2` and `.BRIK.Z`
    /// are recognised but give [`Error::Unsupported`].
    pub fn resolve(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let base = Self::base_name(path).ok_or_else(|| {
            Error::invalid(format!(
                "{}: not an AFNI dataset name (expected .HEAD, .BRIK, .BRIK.gz, or prefix+orig/acpc/tlrc)",
                path.display()
            ))
        })?;
        let text = path.to_string_lossy();
        let sibling = |suffix: &str| PathBuf::from(format!("{base}{suffix}"));

        for suffix in [".BRIK.bz2", ".BRIK.Z"] {
            if text.ends_with(suffix) {
                return Err(unsupported_compression(path));
            }
        }

        let head = sibling(".HEAD");
        if !head.is_file() {
            return Err(Error::missing(format!("AFNI header {}", head.display())));
        }

        let plain = sibling(".BRIK");
        let gz = sibling(".BRIK.gz");
        let brik = if text.ends_with(".BRIK.gz") && gz.is_file() {
            Some(gz)
        } else if plain.is_file() {
            Some(plain)
        } else if gz.is_file() {
            Some(gz)
        } else {
            for suffix in [".BRIK.bz2", ".BRIK.Z"] {
                if sibling(suffix).is_file() {
                    return Err(unsupported_compression(&sibling(suffix)));
                }
            }
            None
        };
        Ok(Self { head, brik })
    }

    /// The dataset name without any file suffix or trailing `.`, e.g.
    /// `dir/anat+orig`.
    pub(crate) fn base_name(path: &Path) -> Option<String> {
        let text = path.to_str()?;
        if let Some(base) = FILE_SUFFIXES.iter().find_map(|s| text.strip_suffix(s)) {
            return Some(base.to_string());
        }
        let base = text.strip_suffix('.').unwrap_or(text);
        VIEWS
            .iter()
            .any(|view| base.ends_with(view))
            .then(|| base.to_string())
    }
}

fn unsupported_compression(path: &Path) -> Error {
    Error::unsupported(format!(
        "{}: only uncompressed and gzip (.BRIK.gz) BRIKs are supported; decompress it first",
        path.display()
    ))
}

/// A loaded AFNI volume: its header plus the sub-bricks that were read.
#[derive(Debug, Clone)]
pub struct Brik {
    /// The parsed `.HEAD` header.
    pub header: Header,
    /// Voxel dimensions `[nx, ny, nz]`.
    pub dimensions: [usize; 3],
    /// One entry per sub-brick in the dataset. An entry is `None` when that
    /// sub-brick was not requested from [`Brik::read_sub_bricks`].
    pub sub_bricks: Vec<Option<SubBrick>>,
}

#[derive(Debug, Clone, Copy)]
struct BrikFrameLayout {
    brik_type: BrikType,
    factor: f32,
    offset: u64,
    bytes: usize,
}

#[derive(Debug)]
enum BrikReaderSource {
    Plain(BufReader<File>),
    Gzip(PathBuf),
}

/// On-demand reader for one AFNI sub-brick at a time.
///
/// An uncompressed BRIK remains open and frames are reached with an absolute
/// seek. A gzipped BRIK is reopened and streamed only as far as the requested
/// frame, so it remains memory-bounded but arbitrary reverse access is more
/// expensive. Use [`Brik::read`] when every frame is needed at once.
#[derive(Debug)]
pub struct BrikReader {
    header: Header,
    dimensions: [usize; 3],
    data_path: PathBuf,
    little_endian: bool,
    layout: Vec<BrikFrameLayout>,
    source: BrikReaderSource,
}

impl BrikReader {
    /// Open an AFNI dataset without loading any voxel values.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let paths = AfniPaths::resolve(path)?;
        let header = Header::read(&paths.head)?;
        let dimensions = header
            .dimensions()
            .ok_or_else(|| Error::missing("DATASET_DIMENSIONS"))?;
        let voxels = dimensions
            .iter()
            .try_fold(1usize, |count, &dimension| count.checked_mul(dimension))
            .ok_or_else(|| Error::invalid(format!("DATASET_DIMENSIONS {dimensions:?} overflow")))?;
        let data_path = paths.brik.ok_or_else(|| {
            Error::missing(format!(
                "voxel data for {}: no .BRIK or .BRIK.gz next to it",
                paths.head.display()
            ))
        })?;

        let mut offset = 0u64;
        let mut layout = Vec::with_capacity(header.nvals());
        for frame in 0..header.nvals() {
            let brik_type = BrikType::from_code(header.brick_type_code(frame))?;
            let bytes = voxels
                .checked_mul(brik_type.byte_width())
                .ok_or_else(|| Error::invalid(format!("sub-brick {frame} size overflows")))?;
            layout.push(BrikFrameLayout {
                brik_type,
                factor: header.brick_factor(frame) as f32,
                offset,
                bytes,
            });
            offset = offset
                .checked_add(bytes as u64)
                .ok_or_else(|| Error::invalid("dataset size overflows"))?;
        }

        let mut file = File::open(&data_path).map_err(|source| Error::Io {
            path: data_path.clone(),
            source,
        })?;
        let file_len = file
            .metadata()
            .map_err(|source| Error::Io {
                path: data_path.clone(),
                source,
            })?
            .len();
        let mut magic = [0u8; 2];
        let magic_len = file.read(&mut magic).map_err(|source| Error::Io {
            path: data_path.clone(),
            source,
        })?;
        file.seek(SeekFrom::Start(0)).map_err(|source| Error::Io {
            path: data_path.clone(),
            source,
        })?;
        let source = if crate::compress::is_gzip(&magic[..magic_len]) {
            BrikReaderSource::Gzip(data_path.clone())
        } else {
            if file_len < offset {
                return Err(Error::parse(format!(
                    "{}: BRIK is {file_len} bytes but the header describes {offset}",
                    data_path.display()
                )));
            }
            BrikReaderSource::Plain(BufReader::new(file))
        };

        Ok(Self {
            little_endian: header.brik_is_little_endian(),
            header,
            dimensions,
            data_path,
            layout,
            source,
        })
    }

    /// Parsed `.HEAD` attributes.
    pub fn header(&self) -> &Header {
        &self.header
    }

    /// Voxel dimensions `[nx, ny, nz]`.
    pub fn dimensions(&self) -> [usize; 3] {
        self.dimensions
    }

    /// Number of source sub-bricks.
    pub fn nvals(&self) -> usize {
        self.layout.len()
    }

    /// Number of voxels in each sub-brick.
    pub fn voxels(&self) -> usize {
        self.dimensions.iter().product()
    }

    /// Whether the voxel stream is gzip-compressed.
    pub fn is_compressed(&self) -> bool {
        matches!(self.source, BrikReaderSource::Gzip(_))
    }

    /// Load one source sub-brick while leaving all others on disk.
    pub fn read_sub_brick(&mut self, frame: usize) -> Result<SubBrick> {
        let layout = *self.layout.get(frame).ok_or_else(|| {
            Error::invalid(format!(
                "sub-brick {frame} is outside 0..{}",
                self.layout.len().saturating_sub(1)
            ))
        })?;
        let mut raw = vec![0u8; layout.bytes];
        match &mut self.source {
            BrikReaderSource::Plain(reader) => {
                reader
                    .seek(SeekFrom::Start(layout.offset))
                    .map_err(|error| read_error(&self.data_path, frame, error))?;
                reader
                    .read_exact(&mut raw)
                    .map_err(|error| read_error(&self.data_path, frame, error))?;
            }
            BrikReaderSource::Gzip(path) => {
                let file = File::open(&*path).map_err(|source| Error::Io {
                    path: path.clone(),
                    source,
                })?;
                let mut decoder = flate2::read::MultiGzDecoder::new(BufReader::new(file));
                let skipped = io::copy(&mut decoder.by_ref().take(layout.offset), &mut io::sink())
                    .map_err(|error| read_error(path, frame, error))?;
                if skipped < layout.offset {
                    return Err(truncated(path, frame, layout.offset, skipped));
                }
                decoder
                    .read_exact(&mut raw)
                    .map_err(|error| read_error(path, frame, error))?;
            }
        }
        Ok(SubBrick {
            data: BrickData::decode(layout.brik_type, &raw, self.little_endian),
            factor: layout.factor,
        })
    }

    /// Load one source sub-brick as true scaled values.
    pub fn read_frame(&mut self, frame: usize) -> Result<Vec<f32>> {
        self.read_sub_brick(frame)?.to_f32().ok_or_else(|| {
            Error::unsupported(format!("sub-brick {frame} does not contain scalar data"))
        })
    }

    /// Copy one scaled source sub-brick into a reusable caller-owned buffer.
    pub fn read_frame_into(&mut self, frame: usize, output: &mut [f32]) -> Result<()> {
        if output.len() != self.voxels() {
            return Err(Error::invalid(format!(
                "frame buffer has {} values but dataset has {} voxels",
                output.len(),
                self.voxels()
            )));
        }
        self.read_sub_brick(frame)?.copy_scaled_into(output)
    }
}

/// Builder for an AFNI volume dataset with validated geometry and metadata.
#[derive(Debug, Clone)]
pub struct BrikBuilder {
    dimensions: [usize; 3],
    orientation: [Orientation; 3],
    origin: [f64; 3],
    delta: [f64; 3],
    real_affine: Option<Mat44>,
    sub_bricks: Vec<SubBrick>,
    labels: Option<Vec<String>>,
    time_axis: Option<TimeAxis>,
    history: Option<String>,
}

impl BrikBuilder {
    /// Start a dataset on a cardinal grid.
    pub fn cardinal(
        dimensions: [usize; 3],
        orientation: [Orientation; 3],
        origin: [f64; 3],
        delta: [f64; 3],
    ) -> Self {
        Self {
            dimensions,
            orientation,
            origin,
            delta,
            real_affine: None,
            sub_bricks: Vec::new(),
            labels: None,
            time_axis: None,
            history: None,
        }
    }

    /// Start a dataset from a possibly oblique voxel-to-DICOM/RAI affine.
    /// The closest cardinal orientation is derived for AFNI's display grid;
    /// the exact matrix is retained as `IJK_TO_DICOM_REAL`.
    pub fn affine(dimensions: [usize; 3], affine: Mat44) -> Result<Self> {
        let (orientation, origin, delta) = cardinal_parts(&affine)?;
        Ok(Self {
            dimensions,
            orientation,
            origin,
            delta,
            real_affine: Some(affine),
            sub_bricks: Vec::new(),
            labels: None,
            time_axis: None,
            history: None,
        })
    }

    /// Start an empty output on the exact grid of an existing AFNI dataset,
    /// including its real oblique affine. Sub-bricks and per-brick metadata are
    /// deliberately not copied.
    pub fn like_grid(source: &Brik) -> Result<Self> {
        Self::affine(source.dimensions, source.header.ijk_to_dicom()?)
    }

    /// Append one output sub-brick.
    pub fn sub_brick(mut self, sub_brick: SubBrick) -> Self {
        self.sub_bricks.push(sub_brick);
        self
    }

    /// Append true `f32` values using a storage policy.
    pub fn values(mut self, values: Vec<f32>, storage: StoragePolicy) -> Result<Self> {
        self.sub_bricks.push(SubBrick::from_f32(values, storage)?);
        Ok(self)
    }

    /// Set one label per output sub-brick.
    pub fn labels<S: Into<String>>(mut self, labels: impl IntoIterator<Item = S>) -> Self {
        self.labels = Some(labels.into_iter().map(Into::into).collect());
        self
    }

    /// Attach a time axis.
    pub fn time_axis(mut self, axis: TimeAxis) -> Self {
        self.time_axis = Some(axis);
        self
    }

    /// Set `HISTORY_NOTE`.
    pub fn history(mut self, history: impl Into<String>) -> Self {
        self.history = Some(history.into());
        self
    }

    /// Validate and construct the dataset.
    pub fn build(self) -> Result<Brik> {
        let mut brik = Brik::new(
            self.dimensions,
            self.orientation,
            self.origin,
            self.delta,
            self.sub_bricks,
        )?;
        if let Some(affine) = self.real_affine {
            let flat: Vec<f64> = affine[..3].iter().flatten().copied().collect();
            brik.header
                .set("IJK_TO_DICOM_REAL", AttributeValue::Float(flat));
        }
        if let Some(labels) = self.labels {
            brik.set_brick_labels(&labels)?;
        }
        if let Some(axis) = self.time_axis {
            if axis.nt != brik.nvals() {
                return Err(Error::invalid(format!(
                    "time axis has {} points but dataset has {} sub-bricks",
                    axis.nt,
                    brik.nvals()
                )));
            }
            brik.header.set_time_axis(&axis)?;
        }
        if let Some(history) = self.history {
            brik.header.set_history(history);
        }
        Ok(brik)
    }
}

/// Derive AFNI's cardinal orientation/origin/delta attributes from a real
/// voxel-to-DICOM affine. Each voxel axis must have a distinct dominant world
/// axis; shears and rotations remain in the real affine.
fn cardinal_parts(affine: &Mat44) -> Result<([Orientation; 3], [f64; 3], [f64; 3])> {
    if affine.iter().flatten().any(|v| !v.is_finite()) || affine[3] != [0.0, 0.0, 0.0, 1.0] {
        return Err(Error::invalid(
            "affine must be finite with last row [0,0,0,1]",
        ));
    }
    let determinant = affine[0][0] * (affine[1][1] * affine[2][2] - affine[1][2] * affine[2][1])
        - affine[0][1] * (affine[1][0] * affine[2][2] - affine[1][2] * affine[2][0])
        + affine[0][2] * (affine[1][0] * affine[2][1] - affine[1][1] * affine[2][0]);
    if determinant.abs() <= f64::EPSILON {
        return Err(Error::invalid("affine spatial transform is singular"));
    }

    // Choose the axis permutation with the largest total alignment. Selecting
    // each column's largest component independently can assign the same world
    // axis twice for a valid, strongly oblique matrix.
    const PERMUTATIONS: [[usize; 3]; 6] = [
        [0, 1, 2],
        [0, 2, 1],
        [1, 0, 2],
        [1, 2, 0],
        [2, 0, 1],
        [2, 1, 0],
    ];
    let rows = PERMUTATIONS
        .into_iter()
        .max_by(|a, b| {
            let score =
                |p: &[usize; 3]| (0..3).map(|axis| affine[p[axis]][axis].abs()).sum::<f64>();
            score(a).total_cmp(&score(b))
        })
        .expect("six axis permutations");
    let mut orientation = [Orientation::R2L; 3];
    let mut origin = [0.0; 3];
    let mut delta = [0.0; 3];
    for axis in 0..3 {
        let row = rows[axis];
        let component = affine[row][axis];
        if component == 0.0 {
            return Err(Error::invalid(
                "affine does not have three distinct spatial axes",
            ));
        }
        orientation[axis] = match (row, component.is_sign_positive()) {
            (0, true) => Orientation::R2L,
            (0, false) => Orientation::L2R,
            (1, true) => Orientation::A2P,
            (1, false) => Orientation::P2A,
            (2, true) => Orientation::I2S,
            (2, false) => Orientation::S2I,
            _ => unreachable!(),
        };
        origin[axis] = affine[row][3];
        delta[axis] = component.signum()
            * (affine[0][axis].powi(2) + affine[1][axis].powi(2) + affine[2][axis].powi(2)).sqrt();
    }
    Ok((orientation, origin, delta))
}

impl Brik {
    /// Load every sub-brick of an AFNI dataset, given any name
    /// [`AfniPaths::resolve`] accepts. A `.BRIK.gz` is decompressed as it is
    /// read.
    pub fn read(path: impl AsRef<Path>) -> Result<Self> {
        Self::read_selected(path.as_ref(), None)
    }

    /// Load only the sub-bricks listed in `indices` (in any order; duplicates
    /// are fine). The others are skipped without being decoded, and reading
    /// stops after the last one needed. Unrequested entries of
    /// [`Brik::sub_bricks`] are `None`.
    pub fn read_sub_bricks(path: impl AsRef<Path>, indices: &[usize]) -> Result<Self> {
        Self::read_selected(path.as_ref(), Some(indices))
    }

    /// Combine an already-parsed header with raw (uncompressed) `.BRIK` bytes,
    /// loading every sub-brick.
    pub fn from_parts(header: Header, brik_bytes: &[u8]) -> Result<Self> {
        let source = Source::Seekable(
            Box::new(io::Cursor::new(brik_bytes)),
            brik_bytes.len() as u64,
        );
        Self::load(header, source, None, Path::new("<memory>"))
    }

    fn read_selected(path: &Path, indices: Option<&[usize]>) -> Result<Self> {
        let paths = AfniPaths::resolve(path)?;
        let header = Header::read(&paths.head)?;
        let brik_path = paths.brik.ok_or_else(|| {
            Error::missing(format!(
                "voxel data for {}: no .BRIK or .BRIK.gz next to it",
                paths.head.display()
            ))
        })?;
        let source = Source::open(&brik_path)?;
        Self::load(header, source, indices, &brik_path)
    }

    fn load(
        header: Header,
        mut source: Source,
        indices: Option<&[usize]>,
        path: &Path,
    ) -> Result<Self> {
        let dimensions = header
            .dimensions()
            .ok_or_else(|| Error::missing("DATASET_DIMENSIONS"))?;
        let voxels = dimensions
            .iter()
            .try_fold(1usize, |n, &d| n.checked_mul(d))
            .ok_or_else(|| Error::invalid(format!("DATASET_DIMENSIONS {dimensions:?} overflow")))?;
        let nvals = header.nvals();

        // Per-sub-brick datum, factor and byte size, plus the total.
        let mut layout = Vec::with_capacity(nvals);
        let mut total: u64 = 0;
        for p in 0..nvals {
            let ty = BrikType::from_code(header.brick_type_code(p))?;
            let bytes = voxels
                .checked_mul(ty.byte_width())
                .ok_or_else(|| Error::invalid(format!("sub-brick {p} size overflows")))?;
            total = total
                .checked_add(bytes as u64)
                .ok_or_else(|| Error::invalid("dataset size overflows"))?;
            layout.push((ty, header.brick_factor(p) as f32, bytes));
        }
        if let Source::Seekable(_, len) = &source {
            if *len < total {
                return Err(Error::parse(format!(
                    "{}: BRIK is {len} bytes but the header describes {total} \
                     ({nvals} sub-brick(s) of {dimensions:?} voxels)",
                    path.display()
                )));
            }
        }

        let mut wanted = vec![indices.is_none(); nvals];
        for &p in indices.unwrap_or(&[]) {
            *wanted.get_mut(p).ok_or_else(|| {
                Error::invalid(format!(
                    "sub-brick {p} requested but the dataset has {nvals}"
                ))
            })? = true;
        }
        let last_wanted = wanted.iter().rposition(|&w| w);

        let little = header.brik_is_little_endian();
        let mut sub_bricks = vec![None; nvals];
        for (p, &(ty, factor, bytes)) in layout.iter().enumerate() {
            if Some(p) > last_wanted || last_wanted.is_none() {
                break;
            }
            if !wanted[p] {
                source.skip(bytes as u64, p, path)?;
                continue;
            }
            let raw = source.read(bytes, p, path)?;
            sub_bricks[p] = Some(SubBrick {
                data: BrickData::decode(ty, &raw, little),
                factor,
            });
        }

        Ok(Self {
            header,
            dimensions,
            sub_bricks,
        })
    }

    /// Build a new dataset from scratch on a cardinal (non-oblique) grid.
    ///
    /// `origin` and `delta` are in AFNI's signed DICOM sense (see
    /// [`crate::geometry`]); for example an LPI grid with 2 mm voxels has
    /// `delta = [-2.0, -2.0, 2.0]`. The header gets the attributes AFNI
    /// writes for a new bucket dataset (`3DIM_HEAD_FUNC`, `+orig`), and
    /// [`Brik::write`] fills in the datum, scale and statistics attributes.
    pub fn new(
        dimensions: [usize; 3],
        orientation: [Orientation; 3],
        origin: [f64; 3],
        delta: [f64; 3],
        sub_bricks: Vec<SubBrick>,
    ) -> Result<Self> {
        let mut header = Header::default();
        header.set(
            "TYPESTRING",
            AttributeValue::String("3DIM_HEAD_FUNC".into()),
        );
        header.set("IDCODE_STRING", AttributeValue::String(new_idcode()));
        header.set("IDCODE_DATE", AttributeValue::String(ctime_utc()));
        // [view, func_type, type]: +orig, FUNC_BUCK_TYPE, HEAD_FUNC_TYPE.
        header.set("SCENE_DATA", AttributeValue::Int(vec![0, 11, 1]));
        header.set(
            "ORIENT_SPECIFIC",
            AttributeValue::Int(orientation.iter().map(|o| o.code()).collect()),
        );
        header.set("ORIGIN", AttributeValue::Float(origin.to_vec()));
        header.set("DELTA", AttributeValue::Float(delta.to_vec()));
        let dims: Vec<i64> = dimensions.iter().map(|&d| d as i64).collect();
        header.set(
            "DATASET_RANK",
            AttributeValue::Int(vec![3, sub_bricks.len() as i64]),
        );
        header.set("DATASET_DIMENSIONS", AttributeValue::Int(dims));
        let cardinal = header.ijk_to_dicom_cardinal()?;
        let flat: Vec<f64> = cardinal[..3].iter().flatten().copied().collect();
        header.set("IJK_TO_DICOM", AttributeValue::Float(flat.clone()));
        header.set("IJK_TO_DICOM_REAL", AttributeValue::Float(flat));
        header.set("TEMPLATE_SPACE", AttributeValue::String("ORIG".into()));

        let brik = Self {
            header,
            dimensions,
            sub_bricks: sub_bricks.into_iter().map(Some).collect(),
        };
        brik.check_complete()?;
        Ok(brik)
    }

    /// Write the dataset as a `.HEAD` + `.BRIK` pair, returning the files
    /// written. `path` is any name [`AfniPaths::resolve`] accepts, or a bare
    /// prefix, in which case the view comes from the header (`+orig` if it
    /// has none). A path ending in `.BRIK.gz` writes a gzipped BRIK.
    ///
    /// Every sub-brick must be loaded. The voxels are written little-endian,
    /// and the header written alongside is brought into line with them, as
    /// AFNI's `THD_write_3dim_dataset` does: `DATASET_RANK`,
    /// `DATASET_DIMENSIONS`, `BRICK_TYPES`, `BRICK_FLOAT_FACS`,
    /// `BRICK_STATS`, `BYTEORDER_STRING` and the view in `SCENE_DATA` are
    /// updated, and the dataset gets a new `IDCODE_STRING` and `IDCODE_DATE`.
    /// Other attributes (labels, statistics, geometry, history) are written
    /// as they are, so keep them consistent if you change the sub-bricks.
    ///
    /// Both files are staged and flushed in the destination directory before
    /// publication. The BRIK is published first and the HEAD last, so a new
    /// dataset does not become discoverable until its voxel file is ready. If
    /// replacement of an existing pair fails, the old pair is restored.
    /// Existing files are overwritten. To avoid leaving a stale copy that
    /// [`AfniPaths::resolve`] would pick up, it is an error if the other form
    /// of the BRIK (`.BRIK` vs `.BRIK.gz`) already exists.
    pub fn write(&self, path: impl AsRef<Path>) -> Result<AfniPaths> {
        self.write_with_options(path, &BrikWriteOptions::default())
    }

    /// Write with explicit overwrite and history policy. Both files are staged
    /// and flushed before the transactional publication described by
    /// [`Brik::write`].
    pub fn write_with_options(
        &self,
        path: impl AsRef<Path>,
        options: &BrikWriteOptions,
    ) -> Result<AfniPaths> {
        self.check_complete()?;
        let path = path.as_ref();
        let text = path
            .to_str()
            .ok_or_else(|| Error::invalid("non-UTF-8 dataset path"))?;
        let gzip = text.ends_with(".BRIK.gz");
        let (base, view) = match AfniPaths::base_name(path) {
            Some(base) => {
                let view = VIEWS
                    .iter()
                    .position(|v| base.ends_with(v))
                    .and_then(|i| View::from_code(i as i64));
                (base, view)
            }
            None => {
                let view = self.header.view().unwrap_or(View::Orig);
                (format!("{text}{}", view.suffix()), Some(view))
            }
        };
        let head = PathBuf::from(format!("{base}.HEAD"));
        let brik = PathBuf::from(format!("{base}.BRIK{}", if gzip { ".gz" } else { "" }));
        let other = PathBuf::from(format!("{base}.BRIK{}", if gzip { "" } else { ".gz" }));
        if !options.overwrite {
            if let Some(existing) = [&head, &brik].into_iter().find(|path| path.exists()) {
                return Err(Error::invalid(format!(
                    "{} exists; enable overwrite to replace it",
                    existing.display()
                )));
            }
        }
        if other.exists() {
            return Err(Error::invalid(format!(
                "{} exists; remove it before writing {}",
                other.display(),
                brik.display()
            )));
        }
        for target in [&head, &brik] {
            if target.exists() && !target.is_file() {
                return Err(Error::invalid(format!(
                    "{} exists but is not a file",
                    target.display()
                )));
            }
        }

        let mut header = self.header_for_writing(view);
        if let Some(entry) = &options.history_entry {
            header.append_history(entry);
        }
        let mut bytes = Vec::new();
        for sub in self.sub_bricks.iter().flatten() {
            bytes.extend_from_slice(&sub.data.to_le_bytes());
        }
        if gzip {
            bytes = crate::compress::gzip(&bytes);
        }
        let mut staged = StagedFiles::default();
        let staged_brik = stage_file(&brik, &bytes, "brik")?;
        staged.track(staged_brik.clone());
        let staged_head = stage_file(&head, header.to_head_string().as_bytes(), "head")?;
        staged.track(staged_head.clone());
        commit_staged_pair(&staged_head, &staged_brik, &head, &brik, options.overwrite)?;
        Ok(AfniPaths {
            head,
            brik: Some(brik),
        })
    }

    /// The header [`Brik::write`] writes: this one with its datum, scale,
    /// size, statistics, byte order, view and ID attributes updated.
    fn header_for_writing(&self, view: Option<View>) -> Header {
        let mut h = self.header.clone();
        let subs: Vec<&SubBrick> = self.sub_bricks.iter().flatten().collect();
        let nvals = subs.len() as i64;

        let mut rank = h
            .ints("DATASET_RANK")
            .map(<[i64]>::to_vec)
            .unwrap_or_default();
        rank.resize(rank.len().max(2), 0);
        rank[0] = 3;
        rank[1] = nvals;
        h.set("DATASET_RANK", AttributeValue::Int(rank));

        let mut dims = h
            .ints("DATASET_DIMENSIONS")
            .map(<[i64]>::to_vec)
            .unwrap_or_default();
        dims.resize(dims.len().max(3), 0);
        for (d, &n) in dims.iter_mut().zip(&self.dimensions) {
            *d = n as i64;
        }
        h.set("DATASET_DIMENSIONS", AttributeValue::Int(dims));

        h.set(
            "BRICK_TYPES",
            AttributeValue::Int(subs.iter().map(|s| s.brik_type().code()).collect()),
        );
        h.set(
            "BRICK_FLOAT_FACS",
            AttributeValue::Float(subs.iter().map(|s| f64::from(s.factor)).collect()),
        );
        h.set(
            "BRICK_STATS",
            AttributeValue::Float(subs.iter().flat_map(|s| s.stats()).collect()),
        );
        h.set(
            "BYTEORDER_STRING",
            AttributeValue::String("LSB_FIRST".into()),
        );
        if let Some(view) = view {
            let mut scene = h
                .ints("SCENE_DATA")
                .map(<[i64]>::to_vec)
                .unwrap_or_default();
            scene.resize(scene.len().max(3), 0);
            scene[0] = view as i64;
            h.set("SCENE_DATA", AttributeValue::Int(scene));
        }
        h.set("IDCODE_STRING", AttributeValue::String(new_idcode()));
        h.set("IDCODE_DATE", AttributeValue::String(ctime_utc()));
        h
    }

    /// Every sub-brick is loaded and has one value per voxel.
    fn check_complete(&self) -> Result<()> {
        let voxels = self.voxels();
        for (p, sub) in self.sub_bricks.iter().enumerate() {
            let sub = sub
                .as_ref()
                .ok_or_else(|| Error::invalid(format!("sub-brick {p} is not loaded")))?;
            if sub.len() != voxels {
                return Err(Error::invalid(format!(
                    "sub-brick {p} has {} voxels but the grid has {voxels}",
                    sub.len()
                )));
            }
        }
        if self.sub_bricks.is_empty() {
            return Err(Error::invalid("a dataset needs at least one sub-brick"));
        }
        Ok(())
    }

    /// Number of sub-bricks in the dataset (loaded or not).
    pub fn nvals(&self) -> usize {
        self.sub_bricks.len()
    }

    /// Number of voxels per sub-brick (`nx*ny*nz`).
    pub fn voxels(&self) -> usize {
        self.dimensions.iter().product()
    }

    /// Sub-brick `p`, if it exists and was loaded.
    pub fn sub_brick(&self, p: usize) -> Option<&SubBrick> {
        self.sub_bricks.get(p)?.as_ref()
    }

    /// The flat index `i + j*nx + k*nx*ny` of voxel `(i, j, k)`, or `None`
    /// outside the grid.
    pub fn voxel_index(&self, i: usize, j: usize, k: usize) -> Option<usize> {
        let [nx, ny, nz] = self.dimensions;
        (i < nx && j < ny && k < nz).then(|| i + nx * (j + ny * k))
    }

    /// The scaled value at voxel `(i, j, k)` in sub-brick `p` (see
    /// [`SubBrick::value`]). `None` outside the grid, for an unloaded
    /// sub-brick, or for RGB/RGBA.
    pub fn value(&self, i: usize, j: usize, k: usize, p: usize) -> Option<f32> {
        self.sub_brick(p)?.value(self.voxel_index(i, j, k)?)
    }

    /// Replace one sub-brick after validating its voxel count.
    pub fn replace_sub_brick(&mut self, p: usize, sub_brick: SubBrick) -> Result<Option<SubBrick>> {
        if sub_brick.len() != self.voxels() {
            return Err(Error::invalid(format!(
                "replacement sub-brick has {} voxels but grid has {}",
                sub_brick.len(),
                self.voxels()
            )));
        }
        let slot = self
            .sub_bricks
            .get_mut(p)
            .ok_or_else(|| Error::invalid(format!("sub-brick {p} is out of range")))?;
        Ok(slot.replace(sub_brick))
    }

    /// Append a sub-brick after validating its voxel count, returning its index.
    pub fn push_sub_brick(&mut self, sub_brick: SubBrick) -> Result<usize> {
        if sub_brick.len() != self.voxels() {
            return Err(Error::invalid(format!(
                "new sub-brick has {} voxels but grid has {}",
                sub_brick.len(),
                self.voxels()
            )));
        }
        let index = self.sub_bricks.len();
        self.sub_bricks.push(Some(sub_brick));
        self.header.set(
            "DATASET_RANK",
            AttributeValue::Int(vec![3, self.sub_bricks.len() as i64]),
        );
        // Appending a frame extends an existing time series. Keeping the old
        // TAXIS_NUMS[0] would leave the header internally inconsistent.
        if let Some(mut axis) = self.header.time_axis() {
            axis.nt = self.sub_bricks.len();
            self.header.set_time_axis(&axis)?;
        }
        Ok(index)
    }

    /// Set one label per sub-brick.
    pub fn set_brick_labels<S: AsRef<str>>(&mut self, labels: &[S]) -> Result<()> {
        if labels.len() != self.nvals() {
            return Err(Error::invalid(format!(
                "got {} labels for {} sub-bricks",
                labels.len(),
                self.nvals()
            )));
        }
        self.header.set_brick_labels(labels)
    }

    /// Copy the scaled time series at flat voxel `index` into `output`.
    pub fn voxel_series_into(&self, index: usize, output: &mut [f32]) -> Result<()> {
        if index >= self.voxels() {
            return Err(Error::invalid(format!(
                "voxel index {index} is out of range for {} voxels",
                self.voxels()
            )));
        }
        if output.len() != self.nvals() {
            return Err(Error::invalid(format!(
                "time-series buffer has {} values but dataset has {} sub-bricks",
                output.len(),
                self.nvals()
            )));
        }
        for (p, value) in output.iter_mut().enumerate() {
            *value = self
                .sub_brick(p)
                .ok_or_else(|| Error::invalid(format!("sub-brick {p} was not loaded")))?
                .value(index)
                .ok_or_else(|| Error::unsupported(format!("sub-brick {p} is not scalar")))?;
        }
        Ok(())
    }

    /// The scaled time series at flat voxel `index`.
    pub fn voxel_series(&self, index: usize) -> Result<Vec<f32>> {
        let mut values = vec![0.0; self.nvals()];
        self.voxel_series_into(index, &mut values)?;
        Ok(values)
    }
}

/// Staging paths are removed on every early return. Once renamed or linked
/// into place they no longer exist, so successful commits need no disarm step.
#[derive(Debug, Default)]
struct StagedFiles {
    paths: Vec<PathBuf>,
}

impl StagedFiles {
    fn track(&mut self, path: PathBuf) {
        self.paths.push(path);
    }
}

impl Drop for StagedFiles {
    fn drop(&mut self) {
        for path in &self.paths {
            let _ = fs::remove_file(path);
        }
    }
}

fn stage_file(target: &Path, bytes: &[u8], role: &str) -> Result<PathBuf> {
    for _ in 0..100 {
        let path = auxiliary_path(target, role, "tmp");
        let mut file = match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(source) => {
                return Err(Error::Io {
                    path: path.clone(),
                    source,
                })
            }
        };
        let result = file.write_all(bytes).and_then(|()| file.sync_all());
        if let Err(source) = result {
            drop(file);
            let _ = fs::remove_file(&path);
            return Err(Error::Io {
                path: path.clone(),
                source,
            });
        }
        return Ok(path);
    }
    Err(Error::invalid(format!(
        "could not create a unique staging file next to {}",
        target.display()
    )))
}

fn auxiliary_path(target: &Path, role: &str, suffix: &str) -> PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let parent = target.parent().unwrap_or_else(|| Path::new("."));
    let name = target
        .file_name()
        .map(|name| name.to_string_lossy())
        .unwrap_or_else(|| "dataset".into());
    let sequence = COUNTER.fetch_add(1, Ordering::Relaxed);
    parent.join(format!(
        ".{name}.afni-io-{role}-{}-{sequence}.{suffix}",
        std::process::id()
    ))
}

pub(crate) fn commit_staged_pair(
    staged_head: &Path,
    staged_brik: &Path,
    target_head: &Path,
    target_brik: &Path,
    overwrite: bool,
) -> Result<()> {
    for target in [target_head, target_brik] {
        if target.exists() && !target.is_file() {
            return Err(Error::invalid(format!(
                "{} exists but is not a file",
                target.display()
            )));
        }
    }
    if !overwrite {
        // A hard link publishes the already-flushed inode and, unlike rename
        // on Unix, atomically refuses to replace a name created by a racer.
        fs::hard_link(staged_brik, target_brik).map_err(|source| Error::Io {
            path: target_brik.to_path_buf(),
            source,
        })?;
        if let Err(source) = fs::hard_link(staged_head, target_head) {
            let rollback = fs::remove_file(target_brik);
            return Err(commit_error(target_head, source, rollback.err()));
        }
        let _ = fs::remove_file(staged_brik);
        let _ = fs::remove_file(staged_head);
        return Ok(());
    }

    let head_backup = target_head
        .exists()
        .then(|| unused_auxiliary_path(target_head, "head-backup", "bak"));
    let brik_backup = target_brik
        .exists()
        .then(|| unused_auxiliary_path(target_brik, "brik-backup", "bak"));
    let mut head_saved = false;
    let mut brik_saved = false;
    let mut head_installed = false;
    let mut brik_installed = false;

    let commit = (|| -> io::Result<()> {
        // Remove the HEAD first, making an old dataset temporarily
        // undiscoverable rather than exposing a mismatched pair.
        if let Some(backup) = &head_backup {
            fs::rename(target_head, backup)?;
            head_saved = true;
        }
        if let Some(backup) = &brik_backup {
            fs::rename(target_brik, backup)?;
            brik_saved = true;
        }
        fs::rename(staged_brik, target_brik)?;
        brik_installed = true;
        fs::rename(staged_head, target_head)?;
        head_installed = true;
        Ok(())
    })();

    if let Err(source) = commit {
        let rollback = rollback_pair(
            target_head,
            target_brik,
            head_backup.as_deref(),
            brik_backup.as_deref(),
            head_saved,
            brik_saved,
            head_installed,
            brik_installed,
        );
        return Err(commit_error(target_head, source, rollback.err()));
    }

    if let Some(backup) = head_backup {
        let _ = fs::remove_file(backup);
    }
    if let Some(backup) = brik_backup {
        let _ = fs::remove_file(backup);
    }
    Ok(())
}

fn unused_auxiliary_path(target: &Path, role: &str, suffix: &str) -> PathBuf {
    loop {
        let path = auxiliary_path(target, role, suffix);
        if !path.exists() {
            return path;
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn rollback_pair(
    target_head: &Path,
    target_brik: &Path,
    head_backup: Option<&Path>,
    brik_backup: Option<&Path>,
    head_saved: bool,
    brik_saved: bool,
    head_installed: bool,
    brik_installed: bool,
) -> io::Result<()> {
    let mut first_error = None;
    if head_installed {
        if let Err(error) = fs::remove_file(target_head) {
            first_error.get_or_insert(error);
        }
    }
    if brik_installed {
        if let Err(error) = fs::remove_file(target_brik) {
            first_error.get_or_insert(error);
        }
    }
    if brik_saved {
        if let Some(backup) = brik_backup {
            if let Err(error) = fs::rename(backup, target_brik) {
                first_error.get_or_insert(error);
            }
        }
    }
    if head_saved {
        if let Some(backup) = head_backup {
            if let Err(error) = fs::rename(backup, target_head) {
                first_error.get_or_insert(error);
            }
        }
    }
    match first_error {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

fn commit_error(path: &Path, source: io::Error, rollback: Option<io::Error>) -> Error {
    match rollback {
        Some(rollback) => Error::invalid(format!(
            "publishing {} failed ({source}); rollback also failed ({rollback})",
            path.display()
        )),
        None => Error::Io {
            path: path.to_path_buf(),
            source,
        },
    }
}

/// Where `.BRIK` bytes come from. Seekable sources know their length, so a
/// truncated file is reported before anything is decoded.
enum Source<'a> {
    Seekable(Box<dyn ReadSeek + 'a>, u64),
    Stream(Box<dyn Read + 'a>),
}

trait ReadSeek: Read + Seek {}
impl<T: Read + Seek> ReadSeek for T {}

impl Source<'_> {
    /// Open a `.BRIK`, choosing gzip decoding from the file's magic bytes
    /// rather than its name.
    fn open(path: &Path) -> Result<Self> {
        let io_err = |source| Error::Io {
            path: path.to_path_buf(),
            source,
        };
        let file = File::open(path).map_err(io_err)?;
        let len = file.metadata().map_err(io_err)?.len();
        let mut reader = BufReader::new(file);
        let mut magic = [0u8; 2];
        let n = reader.read(&mut magic).map_err(io_err)?;
        reader.seek(SeekFrom::Start(0)).map_err(io_err)?;
        Ok(if crate::compress::is_gzip(&magic[..n]) {
            // Multi-member so that concatenated or `pigz` output decodes fully.
            Source::Stream(Box::new(flate2::read::MultiGzDecoder::new(reader)))
        } else {
            Source::Seekable(Box::new(reader), len)
        })
    }

    fn read(&mut self, bytes: usize, p: usize, path: &Path) -> Result<Vec<u8>> {
        let reader: &mut dyn Read = match self {
            Source::Seekable(r, _) => r,
            Source::Stream(r) => r,
        };
        // Grow as data arrives rather than trusting the header's size up front.
        let mut out = Vec::with_capacity(bytes.min(1 << 26));
        reader
            .take(bytes as u64)
            .read_to_end(&mut out)
            .map_err(|e| read_error(path, p, e))?;
        if out.len() < bytes {
            return Err(truncated(path, p, bytes as u64, out.len() as u64));
        }
        Ok(out)
    }

    fn skip(&mut self, bytes: u64, p: usize, path: &Path) -> Result<()> {
        match self {
            Source::Seekable(r, _) => {
                r.seek(SeekFrom::Current(bytes as i64))
                    .map_err(|e| read_error(path, p, e))?;
            }
            Source::Stream(r) => {
                let skipped = io::copy(&mut r.take(bytes), &mut io::sink())
                    .map_err(|e| read_error(path, p, e))?;
                if skipped < bytes {
                    return Err(truncated(path, p, bytes, skipped));
                }
            }
        }
        Ok(())
    }
}

/// A fresh dataset ID in AFNI's form: `AFN_` plus 22 characters from
/// `[A-Za-z0-9_-]` (`UNIQ_idcode`). Uniqueness comes from the standard
/// library's randomly keyed hasher plus the time and a counter.
pub(crate) fn new_idcode() -> String {
    use std::collections::hash_map::RandomState;
    use std::hash::{BuildHasher, Hasher};
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789_-";
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let count = COUNTER.fetch_add(1, Ordering::Relaxed);
    let mut id = String::from("AFN_");
    let mut bits = 0u128;
    for round in 0..2u64 {
        let mut h = RandomState::new().build_hasher();
        h.write_u128(nanos);
        h.write_u64(count);
        h.write_u64(round);
        h.write_u32(std::process::id());
        bits = (bits << 64) | u128::from(h.finish());
    }
    for _ in 0..22 {
        id.push(ALPHABET[(bits & 63) as usize] as char);
        bits >>= 6;
    }
    id
}

/// The current UTC time in C `ctime` form without the newline, as AFNI
/// writes `IDCODE_DATE`: `Fri Oct  2 16:59:33 2026`.
fn ctime_utc() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64);
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    const WEEKDAYS: [&str; 7] = ["Thu", "Fri", "Sat", "Sun", "Mon", "Tue", "Wed"];
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    format!(
        "{} {} {day:>2} {:02}:{:02}:{:02} {year}",
        WEEKDAYS[days.rem_euclid(7) as usize],
        MONTHS[(month - 1) as usize],
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

fn read_error(path: &Path, p: usize, e: io::Error) -> Error {
    Error::parse(format!("{}: reading sub-brick {p}: {e}", path.display()))
}

fn truncated(path: &Path, p: usize, needed: u64, got: u64) -> Error {
    Error::parse(format!(
        "{}: BRIK ended early in sub-brick {p} (needed {needed} bytes, found {got})",
        path.display()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::head::{AttributeValue, Header};

    fn header(nvals: i64, dims: [i64; 3], types: Option<Vec<i64>>, facs: Vec<f64>) -> Header {
        let mut h = Header::default();
        h.set("DATASET_RANK", AttributeValue::Int(vec![3, nvals]));
        h.set("DATASET_DIMENSIONS", AttributeValue::Int(dims.to_vec()));
        if let Some(types) = types {
            h.set("BRICK_TYPES", AttributeValue::Int(types));
        }
        h.set("BRICK_FLOAT_FACS", AttributeValue::Float(facs));
        h.set(
            "BYTEORDER_STRING",
            AttributeValue::String("LSB_FIRST".into()),
        );
        h
    }

    /// A unique scratch directory for one test.
    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("afni_io_brik_{name}_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn reads_scaled_short_brick() {
        // 2x1x1 volume, one short sub-brick, scale factor 0.5.
        let h = header(1, [2, 1, 1], Some(vec![1]), vec![0.5]);
        let bytes = [10i16.to_le_bytes(), 20i16.to_le_bytes()].concat();
        let brik = Brik::from_parts(h, &bytes).unwrap();
        assert_eq!(brik.nvals(), 1);
        assert_eq!(brik.value(0, 0, 0, 0), Some(5.0));
        assert_eq!(brik.value(1, 0, 0, 0), Some(10.0));
        assert_eq!(
            brik.sub_brick(0).unwrap().data,
            BrickData::Short(vec![10, 20])
        );
    }

    #[test]
    fn reads_float_brick_unscaled() {
        let h = header(1, [1, 1, 1], Some(vec![3]), vec![0.0]);
        let brik = Brik::from_parts(h, &1.25f32.to_le_bytes()).unwrap();
        assert_eq!(brik.value(0, 0, 0, 0), Some(1.25));
    }

    #[test]
    fn reads_every_datum_type_in_one_mixed_dataset() {
        // One voxel per sub-brick, one sub-brick of each of the 8 types.
        let h = header(8, [1, 1, 1], Some((0..8).collect()), vec![0.0; 8]);
        let bytes = [
            vec![200u8],
            (-3i16).to_le_bytes().to_vec(),
            (-70_000i32).to_le_bytes().to_vec(),
            2.5f32.to_le_bytes().to_vec(),
            1e300f64.to_le_bytes().to_vec(),
            [3f32.to_le_bytes(), 4f32.to_le_bytes()].concat(),
            vec![1, 2, 3],
            vec![4, 5, 6, 7],
        ]
        .concat();
        let brik = Brik::from_parts(h, &bytes).unwrap();
        let data: Vec<_> = brik
            .sub_bricks
            .iter()
            .map(|s| s.as_ref().unwrap().data.clone())
            .collect();
        assert_eq!(
            data,
            [
                BrickData::Byte(vec![200]),
                BrickData::Short(vec![-3]),
                BrickData::Int(vec![-70_000]),
                BrickData::Float(vec![2.5]),
                BrickData::Double(vec![1e300]),
                BrickData::Complex(vec![[3.0, 4.0]]),
                BrickData::Rgb(vec![[1, 2, 3]]),
                BrickData::Rgba(vec![[4, 5, 6, 7]]),
            ]
        );
        // Complex gives its magnitude; RGB/RGBA have no scalar value.
        assert_eq!(brik.value(0, 0, 0, 5), Some(5.0));
        assert_eq!(brik.value(0, 0, 0, 6), None);
        assert_eq!(brik.sub_brick(7).unwrap().to_f32(), None);
    }

    #[test]
    fn big_endian_multibyte_types_are_swapped() {
        let mut h = header(5, [1, 1, 1], Some(vec![1, 2, 3, 4, 5]), vec![0.0; 5]);
        h.set(
            "BYTEORDER_STRING",
            AttributeValue::String("MSB_FIRST".into()),
        );
        let bytes = [
            (-3i16).to_be_bytes().to_vec(),
            (-70_000i32).to_be_bytes().to_vec(),
            2.5f32.to_be_bytes().to_vec(),
            (-0.125f64).to_be_bytes().to_vec(),
            [3f32.to_be_bytes(), (-4f32).to_be_bytes()].concat(),
        ]
        .concat();
        let brik = Brik::from_parts(h, &bytes).unwrap();
        let values: Vec<_> = (0..5).map(|p| brik.value(0, 0, 0, p).unwrap()).collect();
        assert_eq!(values, [-3.0, -70_000.0, 2.5, -0.125, 5.0]);
        assert_eq!(
            brik.sub_brick(4).unwrap().data,
            BrickData::Complex(vec![[3.0, -4.0]])
        );
    }

    #[test]
    fn missing_brick_types_means_short_and_short_lists_repeat_the_last() {
        let h = header(2, [1, 1, 1], None, vec![]);
        let brik = Brik::from_parts(h, &[1, 0, 2, 0]).unwrap();
        assert_eq!(brik.sub_brick(1).unwrap().data, BrickData::Short(vec![2]));

        // BRICK_TYPES = [0, 3] for 3 sub-bricks: byte, float, float.
        let h = header(3, [1, 1, 1], Some(vec![0, 3]), vec![]);
        let bytes = [
            vec![9u8],
            1.5f32.to_le_bytes().to_vec(),
            2.5f32.to_le_bytes().to_vec(),
        ]
        .concat();
        let brik = Brik::from_parts(h, &bytes).unwrap();
        assert_eq!(brik.sub_brick(2).unwrap().data, BrickData::Float(vec![2.5]));
    }

    #[test]
    fn negative_factors_apply_and_short_factor_lists_mean_unscaled() {
        let h = header(2, [1, 1, 1], Some(vec![1]), vec![-0.5]);
        let bytes = [4i16.to_le_bytes(), 4i16.to_le_bytes()].concat();
        let brik = Brik::from_parts(h, &bytes).unwrap();
        assert_eq!(brik.value(0, 0, 0, 0), Some(-2.0));
        assert_eq!(brik.value(0, 0, 0, 1), Some(4.0));
    }

    #[test]
    fn non_finite_values_are_kept() {
        let h = header(1, [2, 1, 1], Some(vec![3]), vec![0.0]);
        let bytes = [f32::NAN.to_le_bytes(), f32::INFINITY.to_le_bytes()].concat();
        let values = Brik::from_parts(h, &bytes)
            .unwrap()
            .sub_brick(0)
            .unwrap()
            .to_f32()
            .unwrap();
        assert!(values[0].is_nan());
        assert_eq!(values[1], f32::INFINITY);
    }

    #[test]
    fn rejects_truncated_and_oversized_datasets() {
        let h = header(2, [2, 1, 1], Some(vec![1]), vec![]);
        let err = Brik::from_parts(h, &[0; 6]).unwrap_err().to_string();
        assert!(err.contains("6 bytes but the header describes 8"), "{err}");

        let h = header(1, [i64::MAX, i64::MAX, 2], Some(vec![3]), vec![]);
        let err = Brik::from_parts(h, &[]).unwrap_err().to_string();
        assert!(err.contains("overflow"), "{err}");

        let h = header(1, [1, 1, 1], Some(vec![9]), vec![]);
        assert!(Brik::from_parts(h, &[0; 8]).is_err());
    }

    #[test]
    fn resolves_every_dataset_spelling() {
        let dir = scratch("resolve");
        let stem = dir.join("anat+tlrc");
        let s = stem.display().to_string();
        std::fs::write(format!("{s}.HEAD"), "").unwrap();
        std::fs::write(format!("{s}.BRIK"), "").unwrap();
        std::fs::write(format!("{s}.BRIK.gz"), "").unwrap();

        let plain = PathBuf::from(format!("{s}.BRIK"));
        let gz = PathBuf::from(format!("{s}.BRIK.gz"));
        for name in [
            format!("{s}.HEAD"),
            format!("{s}.BRIK"),
            s.clone(),
            format!("{s}."),
        ] {
            let paths = AfniPaths::resolve(&name).unwrap();
            assert_eq!(paths.head, PathBuf::from(format!("{s}.HEAD")), "{name}");
            assert_eq!(paths.brik.as_ref(), Some(&plain), "{name}: prefers .BRIK");
        }
        // Naming the .gz explicitly picks it.
        assert_eq!(AfniPaths::resolve(&gz).unwrap().brik, Some(gz.clone()));

        // Only .BRIK.gz present.
        std::fs::remove_file(&plain).unwrap();
        assert_eq!(AfniPaths::resolve(&s).unwrap().brik, Some(gz.clone()));

        // Only .BRIK.bz2 present: recognised but unsupported.
        std::fs::remove_file(&gz).unwrap();
        std::fs::write(format!("{s}.BRIK.bz2"), "").unwrap();
        assert!(matches!(AfniPaths::resolve(&s), Err(Error::Unsupported(_))));
        std::fs::remove_file(format!("{s}.BRIK.bz2")).unwrap();

        // Header only.
        assert_eq!(AfniPaths::resolve(&s).unwrap().brik, None);
        assert!(matches!(Brik::read(&s), Err(Error::Missing(_))));

        assert!(AfniPaths::is_afni_name("x/epi+orig"));
        assert!(AfniPaths::is_afni_name("epi+acpc."));
        assert!(AfniPaths::is_afni_name("epi.BRIK.Z"));
        assert!(!AfniPaths::is_afni_name("epi.nii.gz"));
        assert!(!AfniPaths::is_afni_name("epi+orig.nii"));
        assert!(matches!(
            AfniPaths::resolve(dir.join("nope+orig")),
            Err(Error::Missing(_))
        ));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn reads_selected_sub_bricks_from_plain_and_gzipped_briks() {
        let dir = scratch("select");
        // 2 voxels x 4 short sub-bricks; sub-brick p holds [10p, 10p + 1].
        let h = header(4, [2, 1, 1], Some(vec![1]), vec![]);
        let raw: Vec<u8> = (0..4i16)
            .flat_map(|p| [10 * p, 10 * p + 1])
            .flat_map(i16::to_le_bytes)
            .collect();
        for (name, bytes) in [("plain", raw.clone()), ("gz", crate::compress::gzip(&raw))] {
            let stem = dir.join(format!("{name}+orig"));
            let suffix = if name == "gz" { ".BRIK.gz" } else { ".BRIK" };
            h.write(format!("{}.HEAD", stem.display())).unwrap();
            std::fs::write(format!("{}{suffix}", stem.display()), bytes).unwrap();

            let brik = Brik::read_sub_bricks(&stem, &[2, 1, 2]).unwrap();
            assert!(
                brik.sub_brick(0).is_none() && brik.sub_brick(3).is_none(),
                "{name}"
            );
            assert_eq!(brik.value(1, 0, 0, 1), Some(11.0), "{name}");
            assert_eq!(brik.value(0, 0, 0, 2), Some(20.0), "{name}");
            assert_eq!(
                Brik::read(&stem).unwrap().value(1, 0, 0, 3),
                Some(31.0),
                "{name}"
            );
            assert!(Brik::read_sub_bricks(&stem, &[4]).is_err(), "{name}");
        }

        // A truncated gzip stream is reported against the sub-brick it hit.
        let stem = dir.join("short+orig");
        h.write(format!("{}.HEAD", stem.display())).unwrap();
        std::fs::write(
            format!("{}.BRIK.gz", stem.display()),
            crate::compress::gzip(&raw[..10]),
        )
        .unwrap();
        let err = Brik::read(&stem).unwrap_err().to_string();
        assert!(err.contains("sub-brick 2"), "{err}");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn transactional_overwrite_restores_the_old_pair_on_commit_failure() {
        let dir = scratch("transaction_rollback");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let head = dir.join("result+orig.HEAD");
        let brik = dir.join("result+orig.BRIK");
        std::fs::write(&head, b"old head").unwrap();
        std::fs::write(&brik, b"old brik").unwrap();

        let staged_brik = stage_file(&brik, b"new brik", "test-brik").unwrap();
        let missing_head = dir.join("missing-head-stage");
        assert!(commit_staged_pair(&missing_head, &staged_brik, &head, &brik, true).is_err());
        assert_eq!(std::fs::read(&head).unwrap(), b"old head");
        assert_eq!(std::fs::read(&brik).unwrap(), b"old brik");
        assert!(!staged_brik.exists());
        assert!(std::fs::read_dir(&dir).unwrap().all(|entry| {
            !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .contains(".afni-io-")
        }));
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn transactional_no_clobber_removes_a_partially_published_new_pair() {
        let dir = scratch("transaction_no_clobber");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let head = dir.join("result+orig.HEAD");
        let brik = dir.join("result+orig.BRIK");
        std::fs::write(&head, b"someone else's head").unwrap();
        let staged_head = stage_file(&head, b"new head", "test-head").unwrap();
        let staged_brik = stage_file(&brik, b"new brik", "test-brik").unwrap();

        assert!(commit_staged_pair(&staged_head, &staged_brik, &head, &brik, false).is_err());
        assert_eq!(std::fs::read(&head).unwrap(), b"someone else's head");
        assert!(!brik.exists());
        // The caller's staging guard owns cleanup on failure. These direct
        // helper inputs remain available and are removed here.
        std::fs::remove_file(staged_head).unwrap();
        std::fs::remove_file(staged_brik).unwrap();
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn transactional_write_replaces_a_pair_without_auxiliary_files() {
        let dir = scratch("transaction_success");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let stem = dir.join("result+orig");
        std::fs::write(format!("{}.HEAD", stem.display()), b"old head").unwrap();
        std::fs::write(format!("{}.BRIK", stem.display()), b"old brik").unwrap();
        let dataset = Brik::new(
            [1, 1, 1],
            [Orientation::R2L, Orientation::A2P, Orientation::I2S],
            [0.0; 3],
            [1.0; 3],
            vec![SubBrick {
                data: BrickData::Float(vec![42.0]),
                factor: 0.0,
            }],
        )
        .unwrap();

        dataset.write(&stem).unwrap();
        assert_eq!(Brik::read(&stem).unwrap().value(0, 0, 0, 0), Some(42.0));
        assert!(std::fs::read_dir(&dir).unwrap().all(|entry| {
            !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .contains(".afni-io-")
        }));
        std::fs::remove_dir_all(dir).ok();
    }
}
