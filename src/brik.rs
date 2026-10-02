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

use std::fs::File;
use std::io::{self, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use crate::error::{Error, Result};
use crate::head::Header;

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

impl SubBrick {
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

/// Suffixes that name an AFNI dataset file, longest first so that
/// `.BRIK.gz` is matched before `.BRIK`.
const FILE_SUFFIXES: [&str; 5] = [".BRIK.bz2", ".BRIK.gz", ".BRIK.Z", ".BRIK", ".HEAD"];
/// AFNI view names, as the `+view` part of `prefix+view`.
const VIEWS: [&str; 3] = ["+orig", "+acpc", "+tlrc"];

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
    fn base_name(path: &Path) -> Option<String> {
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
}
