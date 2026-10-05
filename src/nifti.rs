//! NIfTI-1 and NIfTI-2 volumes (`.nii`, `.nii.gz`, `.hdr`/`.img`).
//!
//! NIfTI is the dominant volume format in neuroimaging and the one AFNI's
//! SurfVol and most modern pipelines use. This module owns the binary header
//! (both the 348-byte NIfTI-1 and 540-byte NIfTI-2 variants), byte-order
//! detection, scale factors, and the qform/sform spatial transforms.
//!
//! Reading handles:
//! * single-file `.nii` (magic `n+1`/`n+2`) and gzip-compressed `.nii.gz`,
//! * detached `.hdr`/`.img` pairs (magic `ni1`/`ni2`),
//! * little- and big-endian data.
//!
//! Header extensions (the `(esize, ecode, data)` records between the header
//! and `vox_offset`) are read into [`Nifti::extensions`] and written back
//! out. AFNI stores its full `.HEAD` attribute set in one with
//! `ecode = 4`; [`Nifti::afni_header`] decodes it, so sub-brick labels and
//! statistics (`BRICK_LABS`, `BRICK_STATAUX`, ...) survive in `.nii` files
//! written by `3dttest++`, `3dDeconvolve` and other AFNI programs.
//!
//! Writing emits a single-file `.nii` matching the header's version, in
//! little-endian.
//!
//! References: the [NIfTI-1](https://nifti.nimh.nih.gov/nifti-1) and
//! [NIfTI-2](https://nifti.nimh.nih.gov/nifti-2) header definitions; for the
//! AFNI extension, `afni/src/thd_niftiread.c` (`THD_nifti_process_afni_ext`),
//! `thd_niftiwrite.c` and `thd_nimlatr.c`.

use std::fs::{self, File, OpenOptions};
use std::io::{self, BufReader, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use crate::array::{self, DataType, TypedArray};
use crate::error::{self, Error, Result};
use crate::head::Header;
use crate::niml::{NimlData, NimlElement};

/// Which NIfTI header layout a volume uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NiftiVersion {
    /// 348-byte NIfTI-1 header.
    Nifti1,
    /// 540-byte NIfTI-2 header.
    Nifti2,
}

/// File-publication policy for a NIfTI write.
///
/// The complete file is staged and flushed next to its destination before it
/// is published. This makes a single-file NIfTI replacement atomic on the
/// filesystem and gives `overwrite: false` race-safe no-clobber behavior.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NiftiWriteOptions {
    /// Whether an existing destination file may be replaced.
    pub overwrite: bool,
}

impl Default for NiftiWriteOptions {
    fn default() -> Self {
        Self { overwrite: true }
    }
}

/// A NIfTI header, unified across versions (NIfTI-1 fields are widened to the
/// NIfTI-2 types).
#[derive(Debug, Clone, PartialEq)]
pub struct NiftiHeader {
    /// The header layout version.
    pub version: NiftiVersion,
    /// Whether the on-disk data is little-endian.
    pub little_endian: bool,
    /// `dim[0]` is the number of dimensions; `dim[1..=dim[0]]` are the sizes.
    pub dim: [i64; 8],
    /// Intent parameters.
    pub intent_p1: f64,
    /// Intent parameters.
    pub intent_p2: f64,
    /// Intent parameters.
    pub intent_p3: f64,
    /// NIfTI intent code.
    pub intent_code: i32,
    /// Datatype code (see [`crate::array::DataType`]).
    pub datatype: i32,
    /// Bits per voxel.
    pub bitpix: i32,
    /// First slice index.
    pub slice_start: i64,
    /// `pixdim[0]` is qfac; `pixdim[1..]` are voxel sizes.
    pub pixdim: [f64; 8],
    /// Byte offset into the file/`.img` where voxel data begins.
    pub vox_offset: i64,
    /// Data scaling slope (0 means no scaling).
    pub scl_slope: f64,
    /// Data scaling intercept.
    pub scl_inter: f64,
    /// Last slice index.
    pub slice_end: i64,
    /// Slice timing order code.
    pub slice_code: i32,
    /// Packed space/time units code.
    pub xyzt_units: i32,
    /// Display calibration max.
    pub cal_max: f64,
    /// Display calibration min.
    pub cal_min: f64,
    /// Time for one slice.
    pub slice_duration: f64,
    /// Time-axis shift.
    pub toffset: f64,
    /// Free-form description (max 80 chars).
    pub descrip: String,
    /// Auxiliary filename (max 24 chars).
    pub aux_file: String,
    /// Quaternion transform code.
    pub qform_code: i32,
    /// Affine transform code.
    pub sform_code: i32,
    /// Quaternion b component.
    pub quatern_b: f64,
    /// Quaternion c component.
    pub quatern_c: f64,
    /// Quaternion d component.
    pub quatern_d: f64,
    /// Quaternion x offset.
    pub qoffset_x: f64,
    /// Quaternion y offset.
    pub qoffset_y: f64,
    /// Quaternion z offset.
    pub qoffset_z: f64,
    /// First row of the sform affine.
    pub srow_x: [f64; 4],
    /// Second row of the sform affine.
    pub srow_y: [f64; 4],
    /// Third row of the sform affine.
    pub srow_z: [f64; 4],
    /// Name/meaning of the intent.
    pub intent_name: String,
    /// MRI slice ordering hints.
    pub dim_info: u8,
    /// The magic string (`n+1`, `ni1`, `n+2`, `ni2`).
    pub magic: String,
}

impl NiftiHeader {
    /// Number of dimensions (`dim[0]`).
    pub fn ndim(&self) -> usize {
        self.dim[0].clamp(0, 7) as usize
    }

    /// The spatial/temporal shape (`dim[1..=ndim]`).
    pub fn shape(&self) -> Vec<usize> {
        (1..=self.ndim())
            .map(|i| self.dim[i].max(0) as usize)
            .collect()
    }

    /// Total number of voxels (product of the shape).
    pub fn voxel_count(&self) -> usize {
        self.shape().iter().product()
    }

    /// The decoded datatype, if it is one of the supported scalar types.
    pub fn data_type(&self) -> Result<DataType> {
        DataType::from_code(self.datatype)
            .ok_or_else(|| Error::unsupported(format!("NIfTI datatype code {}", self.datatype)))
    }

    /// The 4x4 voxel-to-world affine. Uses the sform if `sform_code > 0`,
    /// otherwise the qform, otherwise a scaling-only fallback from `pixdim`.
    pub fn affine(&self) -> [[f64; 4]; 4] {
        if self.sform_code > 0 {
            [self.srow_x, self.srow_y, self.srow_z, [0.0, 0.0, 0.0, 1.0]]
        } else if self.qform_code > 0 {
            self.qform_affine()
        } else {
            [
                [self.pixdim[1], 0.0, 0.0, 0.0],
                [0.0, self.pixdim[2], 0.0, 0.0],
                [0.0, 0.0, self.pixdim[3], 0.0],
                [0.0, 0.0, 0.0, 1.0],
            ]
        }
    }

    /// The qform affine, derived from the quaternion and `pixdim`.
    fn qform_affine(&self) -> [[f64; 4]; 4] {
        let b = self.quatern_b;
        let c = self.quatern_c;
        let d = self.quatern_d;
        let a = (1.0 - (b * b + c * c + d * d)).max(0.0).sqrt();
        let qfac = if self.pixdim[0] < 0.0 { -1.0 } else { 1.0 };
        let (dx, dy, dz) = (self.pixdim[1], self.pixdim[2], self.pixdim[3] * qfac);

        let r = [
            [
                a * a + b * b - c * c - d * d,
                2.0 * (b * c - a * d),
                2.0 * (b * d + a * c),
            ],
            [
                2.0 * (b * c + a * d),
                a * a + c * c - b * b - d * d,
                2.0 * (c * d - a * b),
            ],
            [
                2.0 * (b * d - a * c),
                2.0 * (c * d + a * b),
                a * a + d * d - b * b - c * c,
            ],
        ];
        [
            [r[0][0] * dx, r[0][1] * dy, r[0][2] * dz, self.qoffset_x],
            [r[1][0] * dx, r[1][1] * dy, r[1][2] * dz, self.qoffset_y],
            [r[2][0] * dx, r[2][1] * dy, r[2][2] * dz, self.qoffset_z],
            [0.0, 0.0, 0.0, 1.0],
        ]
    }
}

/// `ecode` of the AFNI header extension (`NIFTI_ECODE_AFNI`).
pub const ECODE_AFNI: i32 = 4;

/// One NIfTI header extension, kept as raw bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NiftiExtension {
    /// The extension code (`ecode`), e.g. [`ECODE_AFNI`].
    pub code: i32,
    /// The payload, without the 8-byte `esize`/`ecode` prefix. It includes
    /// any padding the writer added to reach a multiple of 16 bytes.
    pub data: Vec<u8>,
}

/// A loaded NIfTI volume: its header, extensions, and decoded voxel buffer.
#[derive(Debug, Clone, PartialEq)]
pub struct Nifti {
    /// The parsed header.
    pub header: NiftiHeader,
    /// Header extensions, in file order.
    pub extensions: Vec<NiftiExtension>,
    /// The voxel data, flat in column-major (Fortran) order as NIfTI stores it.
    pub data: TypedArray,
}

#[derive(Debug)]
enum NiftiReaderSource {
    Plain(BufReader<File>),
    Gzip(PathBuf),
}

/// On-demand reader for one NIfTI volume at a time.
///
/// Only the header and extensions are loaded when the reader is opened.
/// Uncompressed `.nii` and `.img` data are reached with absolute seeks. A
/// `.nii.gz` stream is reopened and decoded only as far as the requested
/// frame, keeping memory use to one frame.
#[derive(Debug)]
pub struct NiftiReader {
    header: NiftiHeader,
    extensions: Vec<NiftiExtension>,
    data_path: PathBuf,
    data_offset: u64,
    dimensions: [usize; 3],
    nvols: usize,
    voxels: usize,
    frame_bytes: usize,
    source: NiftiReaderSource,
}

impl NiftiReader {
    /// Open `.nii`, `.nii.gz`, `.hdr`, or `.img` without loading voxel data.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let requested = path.as_ref();
        let (header_path, detached_data_path) = detached_paths(requested)?;
        let header_prefix = read_stream_prefix(&header_path, 540)?;
        let header = NiftiHeader::parse(&header_prefix)?;

        let header_bytes = match header.version {
            NiftiVersion::Nifti1 => 348usize,
            NiftiVersion::Nifti2 => 540usize,
        };
        let (data_path, data_offset, extension_end) = if header.magic.starts_with("n+") {
            let offset = checked_offset(header.vox_offset, "NIfTI vox_offset")?;
            if offset < header_bytes + 4 {
                return Err(Error::invalid(format!(
                    "single-file NIfTI vox_offset {offset} precedes its header"
                )));
            }
            (header_path.clone(), offset as u64, offset)
        } else if header.magic.starts_with("ni") {
            let data_path = detached_data_path.ok_or_else(|| {
                Error::invalid(format!(
                    "{} is a detached NIfTI header but no .img path could be resolved",
                    header_path.display()
                ))
            })?;
            let offset = checked_offset(header.vox_offset, "NIfTI vox_offset")?;
            let header_len = stream_len(&header_path)?;
            (data_path, offset as u64, header_len)
        } else {
            return Err(Error::parse(format!(
                "unsupported NIfTI magic {:?}",
                header.magic
            )));
        };

        let metadata = read_stream_prefix(&header_path, extension_end.max(header_bytes + 4))?;
        let extensions = parse_extensions(&header, &metadata, extension_end);
        let shape = header.shape();
        let dimension = |index: usize| shape.get(index).copied().unwrap_or(1).max(1);
        let dimensions = [dimension(0), dimension(1), dimension(2)];
        let voxels = dimensions
            .iter()
            .try_fold(1usize, |count, &value| count.checked_mul(value))
            .ok_or_else(|| Error::invalid(format!("NIfTI dimensions {dimensions:?} overflow")))?;
        let nvols = shape
            .iter()
            .skip(3)
            .try_fold(1usize, |count, &value| count.checked_mul(value.max(1)))
            .ok_or_else(|| Error::invalid("NIfTI volume count overflows"))?
            .max(1);
        let frame_bytes = voxels
            .checked_mul(header.data_type()?.elem_size())
            .ok_or_else(|| Error::invalid("NIfTI frame size overflows"))?;
        let total_bytes = frame_bytes
            .checked_mul(nvols)
            .and_then(|bytes| (data_offset as usize).checked_add(bytes))
            .ok_or_else(|| Error::invalid("NIfTI data size overflows"))?;

        let (mut data_file, compressed) = open_and_detect_gzip(&data_path)?;
        let source = if compressed {
            NiftiReaderSource::Gzip(data_path.clone())
        } else {
            let len = data_file
                .metadata()
                .map_err(|source| Error::Io {
                    path: data_path.clone(),
                    source,
                })?
                .len();
            if len < total_bytes as u64 {
                return Err(Error::parse(format!(
                    "{}: NIfTI data is {len} bytes but {total_bytes} are required",
                    data_path.display()
                )));
            }
            data_file
                .seek(SeekFrom::Start(0))
                .map_err(|source| Error::Io {
                    path: data_path.clone(),
                    source,
                })?;
            NiftiReaderSource::Plain(BufReader::new(data_file))
        };

        Ok(Self {
            header,
            extensions,
            data_path,
            data_offset,
            dimensions,
            nvols,
            voxels,
            frame_bytes,
            source,
        })
    }

    /// Parsed NIfTI header.
    pub fn header(&self) -> &NiftiHeader {
        &self.header
    }

    /// Header extensions, loaded when the reader was opened.
    pub fn extensions(&self) -> &[NiftiExtension] {
        &self.extensions
    }

    /// Spatial dimensions `[nx, ny, nz]`.
    pub fn dimensions(&self) -> [usize; 3] {
        self.dimensions
    }

    /// Number of frames (the product of dimensions four and above).
    pub fn nvols(&self) -> usize {
        self.nvols
    }

    /// Number of voxels per frame.
    pub fn voxels(&self) -> usize {
        self.voxels
    }

    /// Whether the voxel stream is gzip-compressed.
    pub fn is_compressed(&self) -> bool {
        matches!(self.source, NiftiReaderSource::Gzip(_))
    }

    /// Decode the AFNI header extension, when present.
    pub fn afni_header(&self) -> Result<Option<Header>> {
        afni_header_from_extensions(&self.extensions)
    }

    /// Load one source frame as scaled `f32` values.
    pub fn read_frame(&mut self, frame: usize) -> Result<Vec<f32>> {
        let mut output = vec![0.0; self.voxels];
        self.read_frame_into(frame, &mut output)?;
        Ok(output)
    }

    /// Copy one scaled source frame into a reusable caller-owned buffer.
    pub fn read_frame_into(&mut self, frame: usize, output: &mut [f32]) -> Result<()> {
        if frame >= self.nvols {
            return Err(Error::invalid(format!(
                "NIfTI frame {frame} is outside 0..{}",
                self.nvols.saturating_sub(1)
            )));
        }
        if output.len() != self.voxels {
            return Err(Error::invalid(format!(
                "frame buffer has {} values but NIfTI has {} voxels",
                output.len(),
                self.voxels
            )));
        }
        let frame_offset = (frame as u64)
            .checked_mul(self.frame_bytes as u64)
            .and_then(|offset| self.data_offset.checked_add(offset))
            .ok_or_else(|| Error::invalid("NIfTI frame offset overflows"))?;
        let mut raw = vec![0u8; self.frame_bytes];
        match &mut self.source {
            NiftiReaderSource::Plain(reader) => {
                reader
                    .seek(SeekFrom::Start(frame_offset))
                    .map_err(|source| Error::Io {
                        path: self.data_path.clone(),
                        source,
                    })?;
                reader.read_exact(&mut raw).map_err(|source| Error::Io {
                    path: self.data_path.clone(),
                    source,
                })?;
            }
            NiftiReaderSource::Gzip(path) => {
                let file = File::open(&*path).map_err(|source| Error::Io {
                    path: path.clone(),
                    source,
                })?;
                let mut decoder = flate2::read::MultiGzDecoder::new(BufReader::new(file));
                let skipped = io::copy(&mut decoder.by_ref().take(frame_offset), &mut io::sink())
                    .map_err(|source| Error::Io {
                    path: path.clone(),
                    source,
                })?;
                if skipped < frame_offset {
                    return Err(Error::parse(format!(
                        "{}: NIfTI stream ended at byte {skipped}, before frame {frame}",
                        path.display()
                    )));
                }
                decoder.read_exact(&mut raw).map_err(|source| Error::Io {
                    path: path.clone(),
                    source,
                })?;
            }
        }
        let values = array::decode_binary(
            &raw,
            self.header.data_type()?,
            self.header.little_endian,
            self.voxels,
        )?;
        for (index, destination) in output.iter_mut().enumerate() {
            let raw = values
                .get_f64(index)
                .ok_or_else(|| Error::parse("decoded NIfTI frame is too short"))?;
            *destination = if self.header.scl_slope != 0.0 {
                (self.header.scl_slope * raw + self.header.scl_inter) as f32
            } else {
                raw as f32
            };
        }
        Ok(())
    }
}

impl Nifti {
    /// Read a NIfTI volume from `.nii`, `.nii.gz`, `.hdr`, or `.img`.
    pub fn read(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let mut bytes = error::read_file(path)?;
        if crate::compress::is_gzip(&bytes) {
            bytes = crate::compress::gunzip(&bytes)?;
        }
        let header = NiftiHeader::parse(&bytes)?;

        // Single-file (n+1/n+2): extensions run up to vox_offset and data
        // follows in the same buffer. Detached (ni1/ni2): extensions run to
        // the end of the .hdr and data lives in the sibling `.img`.
        if header.magic.starts_with("n+") {
            let extensions = parse_extensions(&header, &bytes, header.vox_offset.max(0) as usize);
            let data = read_data(&header, &bytes, header.vox_offset as usize)?;
            Ok(Self {
                header,
                extensions,
                data,
            })
        } else {
            let extensions = parse_extensions(&header, &bytes, bytes.len());
            let img_bytes = read_img_sibling(path)?;
            let data = read_data(&header, &img_bytes, header.vox_offset.max(0) as usize)?;
            Ok(Self {
                header,
                extensions,
                data,
            })
        }
    }

    /// Parse a volume from a single in-memory `.nii` byte buffer.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        let owned;
        let bytes = if crate::compress::is_gzip(bytes) {
            owned = crate::compress::gunzip(bytes)?;
            owned.as_slice()
        } else {
            bytes
        };
        let header = NiftiHeader::parse(bytes)?;
        let offset = if header.magic.starts_with("n+") {
            header.vox_offset as usize
        } else {
            return Err(Error::unsupported(
                "detached ni1/ni2 header passed to from_bytes; use read()",
            ));
        };
        let extensions = parse_extensions(&header, bytes, offset);
        let data = read_data(&header, bytes, offset)?;
        Ok(Self {
            header,
            extensions,
            data,
        })
    }

    /// The AFNI attributes stored in the first AFNI extension
    /// ([`ECODE_AFNI`]), as a [`Header`] with the same accessors as a
    /// `.HEAD` file (`brick_labels`, `brick_stats`, `ijk_to_dicom`, ...).
    ///
    /// `Ok(None)` when there is no AFNI extension, or (as in
    /// `THD_nifti_process_afni_ext`) when it is too short or holds no
    /// `AFNI_attributes` group. An extension that does not start with an XML
    /// prolog, or whose NIML does not parse, is an error.
    pub fn afni_header(&self) -> Result<Option<Header>> {
        afni_header_from_extensions(&self.extensions)
    }

    /// Store `header` as the AFNI extension, replacing any existing one, in
    /// the layout `3dAFNItoNIFTI` writes (an XML prolog, then an
    /// `AFNI_attributes` NIML group).
    pub fn set_afni_header(&mut self, header: &Header) {
        let mut data = b"<?xml version='1.0' ?>\n".to_vec();
        data.extend_from_slice(crate::niml::serialize(&[header.to_niml()]).as_bytes());
        data.push(0);
        let ext = NiftiExtension {
            code: ECODE_AFNI,
            data,
        };
        match self.extensions.iter_mut().find(|e| e.code == ECODE_AFNI) {
            Some(existing) => *existing = ext,
            None => self.extensions.push(ext),
        }
    }

    /// The spatial/temporal shape.
    pub fn shape(&self) -> Vec<usize> {
        self.header.shape()
    }

    /// The raw value at a flat index, widened to `f64`.
    pub fn get(&self, index: usize) -> Option<f64> {
        self.data.get_f64(index)
    }

    /// The scaled value at a flat index: `scl_slope * raw + scl_inter`
    /// (or the raw value when `scl_slope == 0`).
    pub fn get_scaled(&self, index: usize) -> Option<f64> {
        let raw = self.data.get_f64(index)?;
        Some(if self.header.scl_slope != 0.0 {
            self.header.scl_slope * raw + self.header.scl_inter
        } else {
            raw
        })
    }

    /// The scaled value at voxel `(i, j, k)` of volume `t` (column-major).
    pub fn voxel(&self, i: usize, j: usize, k: usize, t: usize) -> Option<f64> {
        let shape = self.shape();
        let nx = *shape.first()?;
        let ny = *shape.get(1)?;
        let nz = *shape.get(2)?;
        if i >= nx || j >= ny || k >= nz {
            return None;
        }
        let vol_stride = nx * ny * nz;
        self.get_scaled(t * vol_stride + i + j * nx + k * nx * ny)
    }

    /// Serialise to a single-file `.nii` byte buffer, including extensions.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = self.header.serialize_single_file();
        if !self.extensions.is_empty() {
            // The 4-byte extender follows the header; its first byte set
            // means extensions follow.
            let extender = out.len() - 4;
            out[extender] = 1;
            for ext in &self.extensions {
                // esize counts the 8-byte prefix and is a multiple of 16.
                let esize = (8 + ext.data.len()).div_ceil(16) * 16;
                out.extend_from_slice(&(esize as i32).to_le_bytes());
                out.extend_from_slice(&ext.code.to_le_bytes());
                out.extend_from_slice(&ext.data);
                out.resize(out.len() + esize - 8 - ext.data.len(), 0);
            }
            let vox_offset = out.len();
            match self.header.version {
                NiftiVersion::Nifti1 => {
                    out[108..112].copy_from_slice(&(vox_offset as f32).to_le_bytes())
                }
                NiftiVersion::Nifti2 => {
                    out[168..176].copy_from_slice(&(vox_offset as i64).to_le_bytes())
                }
            }
        }
        out.extend_from_slice(&self.data.to_bytes(true));
        out
    }

    /// Write to a `.nii` file, gzipping automatically for a `.nii.gz` path.
    /// Existing files are replaced atomically.
    pub fn write(&self, path: impl AsRef<Path>) -> Result<()> {
        self.write_with_options(path, &NiftiWriteOptions::default())
    }

    /// Write with an explicit overwrite policy. The complete `.nii` or
    /// `.nii.gz` is staged and flushed before publication.
    pub fn write_with_options(
        &self,
        path: impl AsRef<Path>,
        options: &NiftiWriteOptions,
    ) -> Result<()> {
        let path = path.as_ref();
        let bytes = self.to_bytes();
        let bytes = if path.extension().and_then(|e| e.to_str()) == Some("gz") {
            crate::compress::gzip(&bytes)
        } else {
            bytes
        };
        publish_nifti(path, &bytes, options.overwrite)
    }
}

/// Remove a staged file on every early return. After a successful rename its
/// old name no longer exists, so the same cleanup remains harmless.
#[derive(Debug)]
struct StagedNifti(PathBuf);

impl Drop for StagedNifti {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

fn publish_nifti(target: &Path, bytes: &[u8], overwrite: bool) -> Result<()> {
    if target.exists() && !target.is_file() {
        return Err(Error::invalid(format!(
            "{} exists but is not a file",
            target.display()
        )));
    }

    let staged_path = unique_sibling(target, "tmp");
    let mut staged_file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&staged_path)
        .map_err(|source| Error::Io {
            path: staged_path.clone(),
            source,
        })?;
    let staged = StagedNifti(staged_path);
    staged_file
        .write_all(bytes)
        .and_then(|()| staged_file.sync_all())
        .map_err(|source| Error::Io {
            path: staged.0.clone(),
            source,
        })?;
    drop(staged_file);

    if !overwrite {
        // Linking the already-flushed inode publishes without the check/write
        // race of `exists()`, and fails if another process won the name.
        fs::hard_link(&staged.0, target).map_err(|source| Error::Io {
            path: target.to_path_buf(),
            source,
        })?;
        return Ok(());
    }

    // A same-directory rename is the single-file commit point: readers see
    // either the previous complete file or the new complete file.
    fs::rename(&staged.0, target).map_err(|source| Error::Io {
        path: target.to_path_buf(),
        source,
    })
}

fn unique_sibling(target: &Path, suffix: &str) -> PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let parent = target.parent().unwrap_or_else(|| Path::new("."));
    let name = target
        .file_name()
        .map(|name| name.to_string_lossy())
        .unwrap_or_else(|| "volume.nii".into());
    loop {
        let sequence = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = parent.join(format!(
            ".{name}.afni-io-{}-{sequence}.{suffix}",
            std::process::id()
        ));
        if !path.exists() {
            return path;
        }
    }
}

fn afni_header_from_extensions(extensions: &[NiftiExtension]) -> Result<Option<Header>> {
    // AFNI requires esize > 32, i.e. more than 24 payload bytes.
    let Some(ext) = extensions
        .iter()
        .find(|e| e.code == ECODE_AFNI && e.data.len() > 24)
    else {
        return Ok(None);
    };
    let end = ext
        .data
        .iter()
        .position(|&b| b == 0)
        .unwrap_or(ext.data.len());
    let text = std::str::from_utf8(&ext.data[..end])
        .map_err(|e| Error::parse(format!("NIfTI AFNI extension is not UTF-8: {e}")))?;
    if !text.starts_with("<?xml") {
        return Err(Error::parse(
            "NIfTI AFNI extension does not start with <?xml",
        ));
    }
    let body = text
        .find("?>")
        .map(|i| &text[i + 2..])
        .ok_or_else(|| Error::parse("NIfTI AFNI extension has no end to its XML prolog"))?;
    let elements = crate::niml::parse_str(body)?;
    Ok(elements
        .iter()
        .find_map(find_afni_attributes)
        .map(Header::from_niml))
}

fn detached_paths(requested: &Path) -> Result<(PathBuf, Option<PathBuf>)> {
    let name = requested
        .to_str()
        .ok_or_else(|| Error::invalid("non-UTF-8 NIfTI path"))?;
    if let Some(stem) = name.strip_suffix(".hdr") {
        Ok((
            requested.to_path_buf(),
            Some(PathBuf::from(format!("{stem}.img"))),
        ))
    } else if let Some(stem) = name.strip_suffix(".img") {
        Ok((
            PathBuf::from(format!("{stem}.hdr")),
            Some(requested.to_path_buf()),
        ))
    } else {
        Ok((requested.to_path_buf(), None))
    }
}

fn checked_offset(offset: i64, name: &str) -> Result<usize> {
    usize::try_from(offset)
        .map_err(|_| Error::invalid(format!("{name} is not a valid byte offset: {offset}")))
}

fn open_and_detect_gzip(path: &Path) -> Result<(File, bool)> {
    let mut file = File::open(path).map_err(|source| Error::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let mut magic = [0u8; 2];
    let count = file.read(&mut magic).map_err(|source| Error::Io {
        path: path.to_path_buf(),
        source,
    })?;
    file.seek(SeekFrom::Start(0)).map_err(|source| Error::Io {
        path: path.to_path_buf(),
        source,
    })?;
    Ok((file, crate::compress::is_gzip(&magic[..count])))
}

fn read_stream_prefix(path: &Path, bytes: usize) -> Result<Vec<u8>> {
    let (file, compressed) = open_and_detect_gzip(path)?;
    let reader: Box<dyn Read> = if compressed {
        Box::new(flate2::read::MultiGzDecoder::new(BufReader::new(file)))
    } else {
        Box::new(BufReader::new(file))
    };
    let mut output = Vec::with_capacity(bytes.min(1 << 20));
    reader
        .take(bytes as u64)
        .read_to_end(&mut output)
        .map_err(|source| Error::Io {
            path: path.to_path_buf(),
            source,
        })?;
    Ok(output)
}

fn stream_len(path: &Path) -> Result<usize> {
    let (file, compressed) = open_and_detect_gzip(path)?;
    if !compressed {
        return file
            .metadata()
            .map(|metadata| metadata.len() as usize)
            .map_err(|source| Error::Io {
                path: path.to_path_buf(),
                source,
            });
    }
    let mut decoder = flate2::read::MultiGzDecoder::new(BufReader::new(file));
    let mut output = Vec::new();
    decoder
        .read_to_end(&mut output)
        .map_err(|source| Error::Io {
            path: path.to_path_buf(),
            source,
        })?;
    Ok(output.len())
}

/// The `AFNI_attributes` group: `element` itself or, as AFNI does with
/// `NI_search_group_deep`, the first one nested inside it.
fn find_afni_attributes(element: &NimlElement) -> Option<&NimlElement> {
    let NimlData::Group(children) = &element.data else {
        return None;
    };
    if element.name == "AFNI_attributes" {
        return Some(element);
    }
    children.iter().find_map(find_afni_attributes)
}

/// Read the extension list that follows the header, stopping at `end` (the
/// voxel offset, or the end of a detached `.hdr`). A malformed record ends
/// the list rather than failing the read, so the voxels stay readable.
fn parse_extensions(header: &NiftiHeader, bytes: &[u8], end: usize) -> Vec<NiftiExtension> {
    let start = match header.version {
        NiftiVersion::Nifti1 => 348,
        NiftiVersion::Nifti2 => 540,
    };
    let end = end.min(bytes.len());
    // No extender, or its first byte is 0: no extensions.
    if start + 4 > end || bytes[start] == 0 {
        return Vec::new();
    }
    let int = |at: usize| {
        let raw = [bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]];
        if header.little_endian {
            i32::from_le_bytes(raw)
        } else {
            i32::from_be_bytes(raw)
        }
    };
    let mut out = Vec::new();
    let mut pos = start + 4;
    while pos + 8 <= end {
        let esize = int(pos);
        if esize < 8 || pos + esize as usize > end {
            break;
        }
        out.push(NiftiExtension {
            code: int(pos + 4),
            data: bytes[pos + 8..pos + esize as usize].to_vec(),
        });
        pos += esize as usize;
    }
    out
}

fn read_data(header: &NiftiHeader, bytes: &[u8], offset: usize) -> Result<TypedArray> {
    let dtype = header.data_type()?;
    let count = header.voxel_count();
    let need = count * dtype.elem_size();
    if offset + need > bytes.len() {
        return Err(Error::parse(format!(
            "NIfTI data ends early: need {need} bytes at offset {offset}, file has {}",
            bytes.len()
        )));
    }
    array::decode_binary(
        &bytes[offset..offset + need],
        dtype,
        header.little_endian,
        count,
    )
}

fn read_img_sibling(head_path: &Path) -> Result<Vec<u8>> {
    let name = head_path
        .to_str()
        .ok_or_else(|| Error::invalid("non-UTF-8 NIfTI path"))?;
    let stem = name
        .strip_suffix(".hdr")
        .or_else(|| name.strip_suffix(".img"))
        .ok_or_else(|| Error::invalid(format!("{name}: expected a .hdr or .img path")))?;
    let img = PathBuf::from(format!("{stem}.img"));
    let mut bytes = error::read_file(&img)?;
    if crate::compress::is_gzip(&bytes) {
        bytes = crate::compress::gunzip(&bytes)?;
    }
    Ok(bytes)
}

// --- Binary header parsing -------------------------------------------------

impl NiftiHeader {
    /// Parse a NIfTI header from the start of `bytes`, detecting version and
    /// byte order automatically.
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        if bytes.len() < 4 {
            return Err(Error::parse("NIfTI file too short for a header"));
        }
        let (version, little) = detect(bytes)?;
        let mut r = Reader {
            bytes,
            pos: 0,
            little,
        };
        match version {
            NiftiVersion::Nifti1 => parse_n1(&mut r),
            NiftiVersion::Nifti2 => parse_n2(&mut r),
        }
    }

    /// Serialise a single-file `.nii` header (little-endian) plus the 4-byte
    /// "no extensions" flag, so voxel data follows immediately after.
    pub fn serialize_single_file(&self) -> Vec<u8> {
        match self.version {
            NiftiVersion::Nifti1 => serialize_n1(self),
            NiftiVersion::Nifti2 => serialize_n2(self),
        }
    }
}

/// Detect `(version, little_endian)` from the `sizeof_hdr` field.
fn detect(bytes: &[u8]) -> Result<(NiftiVersion, bool)> {
    let head = [bytes[0], bytes[1], bytes[2], bytes[3]];
    let le = i32::from_le_bytes(head);
    let be = i32::from_be_bytes(head);
    match (le, be) {
        (348, _) => Ok((NiftiVersion::Nifti1, true)),
        (_, 348) => Ok((NiftiVersion::Nifti1, false)),
        (540, _) => Ok((NiftiVersion::Nifti2, true)),
        (_, 540) => Ok((NiftiVersion::Nifti2, false)),
        _ => Err(Error::parse(
            "not a NIfTI file (sizeof_hdr is neither 348 nor 540)",
        )),
    }
}

struct Reader<'a> {
    bytes: &'a [u8],
    pos: usize,
    little: bool,
}

impl Reader<'_> {
    fn seek(&mut self, pos: usize) {
        self.pos = pos;
    }
    fn take(&mut self, n: usize) -> &[u8] {
        let s = &self.bytes[self.pos..self.pos + n];
        self.pos += n;
        s
    }
    fn i16(&mut self) -> i64 {
        let a: [u8; 2] = self.take(2).try_into().unwrap();
        (if self.little {
            i16::from_le_bytes(a)
        } else {
            i16::from_be_bytes(a)
        }) as i64
    }
    fn i32(&mut self) -> i64 {
        let a: [u8; 4] = self.take(4).try_into().unwrap();
        (if self.little {
            i32::from_le_bytes(a)
        } else {
            i32::from_be_bytes(a)
        }) as i64
    }
    fn i64(&mut self) -> i64 {
        let a: [u8; 8] = self.take(8).try_into().unwrap();
        if self.little {
            i64::from_le_bytes(a)
        } else {
            i64::from_be_bytes(a)
        }
    }
    fn f32(&mut self) -> f64 {
        let a: [u8; 4] = self.take(4).try_into().unwrap();
        (if self.little {
            f32::from_le_bytes(a)
        } else {
            f32::from_be_bytes(a)
        }) as f64
    }
    fn f64(&mut self) -> f64 {
        let a: [u8; 8] = self.take(8).try_into().unwrap();
        if self.little {
            f64::from_le_bytes(a)
        } else {
            f64::from_be_bytes(a)
        }
    }
    fn cstr(&mut self, n: usize) -> String {
        let raw = self.take(n);
        let end = raw.iter().position(|&b| b == 0).unwrap_or(raw.len());
        String::from_utf8_lossy(&raw[..end]).into_owned()
    }
}

fn parse_n1(r: &mut Reader) -> Result<NiftiHeader> {
    r.seek(38); // skip to dim_info (after sizeof_hdr, data_type, db_name, extents, session_error, regular)
    let dim_info = r.take(1)[0];
    r.seek(40);
    let mut dim = [0i64; 8];
    for d in &mut dim {
        *d = r.i16();
    }
    let intent_p1 = r.f32();
    let intent_p2 = r.f32();
    let intent_p3 = r.f32();
    let intent_code = r.i16() as i32;
    let datatype = r.i16() as i32;
    let bitpix = r.i16() as i32;
    let slice_start = r.i16();
    let mut pixdim = [0f64; 8];
    for p in &mut pixdim {
        *p = r.f32();
    }
    let vox_offset = r.f32() as i64;
    let scl_slope = r.f32();
    let scl_inter = r.f32();
    let slice_end = r.i16();
    let slice_code = r.take(1)[0] as i32;
    let xyzt_units = r.take(1)[0] as i32;
    let cal_max = r.f32();
    let cal_min = r.f32();
    let slice_duration = r.f32();
    let toffset = r.f32();
    let _glmax = r.i32();
    let _glmin = r.i32();
    let descrip = r.cstr(80);
    let aux_file = r.cstr(24);
    let qform_code = r.i16() as i32;
    let sform_code = r.i16() as i32;
    let quatern_b = r.f32();
    let quatern_c = r.f32();
    let quatern_d = r.f32();
    let qoffset_x = r.f32();
    let qoffset_y = r.f32();
    let qoffset_z = r.f32();
    let srow_x = [r.f32(), r.f32(), r.f32(), r.f32()];
    let srow_y = [r.f32(), r.f32(), r.f32(), r.f32()];
    let srow_z = [r.f32(), r.f32(), r.f32(), r.f32()];
    let intent_name = r.cstr(16);
    let magic = r.cstr(4);

    Ok(NiftiHeader {
        version: NiftiVersion::Nifti1,
        little_endian: r.little,
        dim,
        intent_p1,
        intent_p2,
        intent_p3,
        intent_code,
        datatype,
        bitpix,
        slice_start,
        pixdim,
        vox_offset,
        scl_slope,
        scl_inter,
        slice_end,
        slice_code,
        xyzt_units,
        cal_max,
        cal_min,
        slice_duration,
        toffset,
        descrip,
        aux_file,
        qform_code,
        sform_code,
        quatern_b,
        quatern_c,
        quatern_d,
        qoffset_x,
        qoffset_y,
        qoffset_z,
        srow_x,
        srow_y,
        srow_z,
        intent_name,
        dim_info,
        magic,
    })
}

fn parse_n2(r: &mut Reader) -> Result<NiftiHeader> {
    r.seek(4);
    let magic = r.cstr(8);
    let datatype = r.i16() as i32;
    let bitpix = r.i16() as i32;
    let mut dim = [0i64; 8];
    for d in &mut dim {
        *d = r.i64();
    }
    let intent_p1 = r.f64();
    let intent_p2 = r.f64();
    let intent_p3 = r.f64();
    let mut pixdim = [0f64; 8];
    for p in &mut pixdim {
        *p = r.f64();
    }
    let vox_offset = r.i64();
    let scl_slope = r.f64();
    let scl_inter = r.f64();
    let cal_max = r.f64();
    let cal_min = r.f64();
    let slice_duration = r.f64();
    let toffset = r.f64();
    let slice_start = r.i64();
    let slice_end = r.i64();
    let descrip = r.cstr(80);
    let aux_file = r.cstr(24);
    let qform_code = r.i32() as i32;
    let sform_code = r.i32() as i32;
    let quatern_b = r.f64();
    let quatern_c = r.f64();
    let quatern_d = r.f64();
    let qoffset_x = r.f64();
    let qoffset_y = r.f64();
    let qoffset_z = r.f64();
    let srow_x = [r.f64(), r.f64(), r.f64(), r.f64()];
    let srow_y = [r.f64(), r.f64(), r.f64(), r.f64()];
    let srow_z = [r.f64(), r.f64(), r.f64(), r.f64()];
    let slice_code = r.i32() as i32;
    let xyzt_units = r.i32() as i32;
    let intent_code = r.i32() as i32;
    let intent_name = r.cstr(16);
    let dim_info = r.take(1)[0];

    Ok(NiftiHeader {
        version: NiftiVersion::Nifti2,
        little_endian: r.little,
        dim,
        intent_p1,
        intent_p2,
        intent_p3,
        intent_code,
        datatype,
        bitpix,
        slice_start,
        pixdim,
        vox_offset,
        scl_slope,
        scl_inter,
        slice_end,
        slice_code,
        xyzt_units,
        cal_max,
        cal_min,
        slice_duration,
        toffset,
        descrip,
        aux_file,
        qform_code,
        sform_code,
        quatern_b,
        quatern_c,
        quatern_d,
        qoffset_x,
        qoffset_y,
        qoffset_z,
        srow_x,
        srow_y,
        srow_z,
        intent_name,
        dim_info,
        magic,
    })
}

// --- Binary header serialisation (little-endian, single file) --------------

struct Writer {
    buf: Vec<u8>,
}

impl Writer {
    fn new(len: usize) -> Self {
        Self {
            buf: vec![0u8; len],
        }
    }
    fn i16(&mut self, at: usize, v: i64) {
        self.buf[at..at + 2].copy_from_slice(&(v as i16).to_le_bytes());
    }
    fn i32(&mut self, at: usize, v: i64) {
        self.buf[at..at + 4].copy_from_slice(&(v as i32).to_le_bytes());
    }
    fn i64(&mut self, at: usize, v: i64) {
        self.buf[at..at + 8].copy_from_slice(&v.to_le_bytes());
    }
    fn f32(&mut self, at: usize, v: f64) {
        self.buf[at..at + 4].copy_from_slice(&(v as f32).to_le_bytes());
    }
    fn f64(&mut self, at: usize, v: f64) {
        self.buf[at..at + 8].copy_from_slice(&v.to_le_bytes());
    }
    fn cstr(&mut self, at: usize, n: usize, s: &str) {
        let b = s.as_bytes();
        let take = b.len().min(n.saturating_sub(1));
        self.buf[at..at + take].copy_from_slice(&b[..take]);
        // remaining bytes are already zero
    }
}

fn serialize_n1(h: &NiftiHeader) -> Vec<u8> {
    let mut w = Writer::new(352); // 348 header + 4-byte extension flag (all zero)
    w.i32(0, 348);
    w.buf[39] = h.dim_info;
    for (i, d) in h.dim.iter().enumerate() {
        w.i16(40 + i * 2, *d);
    }
    w.f32(56, h.intent_p1);
    w.f32(60, h.intent_p2);
    w.f32(64, h.intent_p3);
    w.i16(68, h.intent_code as i64);
    w.i16(70, h.datatype as i64);
    w.i16(72, h.bitpix as i64);
    w.i16(74, h.slice_start);
    for (i, p) in h.pixdim.iter().enumerate() {
        w.f32(76 + i * 4, *p);
    }
    w.f32(108, 352.0); // vox_offset for single file
    w.f32(112, h.scl_slope);
    w.f32(116, h.scl_inter);
    w.i16(120, h.slice_end);
    w.buf[122] = h.slice_code as u8;
    w.buf[123] = h.xyzt_units as u8;
    w.f32(124, h.cal_max);
    w.f32(128, h.cal_min);
    w.f32(132, h.slice_duration);
    w.f32(136, h.toffset);
    w.cstr(148, 80, &h.descrip);
    w.cstr(228, 24, &h.aux_file);
    w.i16(252, h.qform_code as i64);
    w.i16(254, h.sform_code as i64);
    w.f32(256, h.quatern_b);
    w.f32(260, h.quatern_c);
    w.f32(264, h.quatern_d);
    w.f32(268, h.qoffset_x);
    w.f32(272, h.qoffset_y);
    w.f32(276, h.qoffset_z);
    for (i, v) in h.srow_x.iter().enumerate() {
        w.f32(280 + i * 4, *v);
    }
    for (i, v) in h.srow_y.iter().enumerate() {
        w.f32(296 + i * 4, *v);
    }
    for (i, v) in h.srow_z.iter().enumerate() {
        w.f32(312 + i * 4, *v);
    }
    w.cstr(328, 16, &h.intent_name);
    w.cstr(344, 4, "n+1");
    w.buf
}

fn serialize_n2(h: &NiftiHeader) -> Vec<u8> {
    let mut w = Writer::new(544); // 540 header + 4-byte extension flag
    w.i32(0, 540);
    w.cstr(4, 8, "n+2");
    w.i16(12, h.datatype as i64);
    w.i16(14, h.bitpix as i64);
    for (i, d) in h.dim.iter().enumerate() {
        w.i64(16 + i * 8, *d);
    }
    w.f64(80, h.intent_p1);
    w.f64(88, h.intent_p2);
    w.f64(96, h.intent_p3);
    for (i, p) in h.pixdim.iter().enumerate() {
        w.f64(104 + i * 8, *p);
    }
    w.i64(168, 544); // vox_offset
    w.f64(176, h.scl_slope);
    w.f64(184, h.scl_inter);
    w.f64(192, h.cal_max);
    w.f64(200, h.cal_min);
    w.f64(208, h.slice_duration);
    w.f64(216, h.toffset);
    w.i64(224, h.slice_start);
    w.i64(232, h.slice_end);
    w.cstr(240, 80, &h.descrip);
    w.cstr(320, 24, &h.aux_file);
    w.i32(344, h.qform_code as i64);
    w.i32(348, h.sform_code as i64);
    w.f64(352, h.quatern_b);
    w.f64(360, h.quatern_c);
    w.f64(368, h.quatern_d);
    w.f64(376, h.qoffset_x);
    w.f64(384, h.qoffset_y);
    w.f64(392, h.qoffset_z);
    for (i, v) in h.srow_x.iter().enumerate() {
        w.f64(400 + i * 8, *v);
    }
    for (i, v) in h.srow_y.iter().enumerate() {
        w.f64(432 + i * 8, *v);
    }
    for (i, v) in h.srow_z.iter().enumerate() {
        w.f64(464 + i * 8, *v);
    }
    w.i32(496, h.slice_code as i64);
    w.i32(500, h.xyzt_units as i64);
    w.i32(504, h.intent_code as i64);
    w.cstr(508, 16, &h.intent_name);
    w.buf[524] = h.dim_info;
    w.buf
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a minimal 2x2x2 single-volume float NIfTI-1 header.
    fn header(version: NiftiVersion) -> NiftiHeader {
        let mut dim = [0i64; 8];
        dim[0] = 3;
        dim[1] = 2;
        dim[2] = 2;
        dim[3] = 2;
        let mut pixdim = [0f64; 8];
        pixdim[0] = 1.0;
        pixdim[1] = 2.0;
        pixdim[2] = 2.0;
        pixdim[3] = 2.0;
        NiftiHeader {
            version,
            little_endian: true,
            dim,
            intent_p1: 0.0,
            intent_p2: 0.0,
            intent_p3: 0.0,
            intent_code: 0,
            datatype: DataType::Float32.code(),
            bitpix: 32,
            slice_start: 0,
            pixdim,
            vox_offset: 0,
            scl_slope: 2.0,
            scl_inter: 1.0,
            slice_end: 0,
            slice_code: 0,
            xyzt_units: 0,
            cal_max: 0.0,
            cal_min: 0.0,
            slice_duration: 0.0,
            toffset: 0.0,
            descrip: "test volume".into(),
            aux_file: String::new(),
            qform_code: 0,
            sform_code: 1,
            quatern_b: 0.0,
            quatern_c: 0.0,
            quatern_d: 0.0,
            qoffset_x: -10.0,
            qoffset_y: -10.0,
            qoffset_z: -10.0,
            srow_x: [2.0, 0.0, 0.0, -10.0],
            srow_y: [0.0, 2.0, 0.0, -10.0],
            srow_z: [0.0, 0.0, 2.0, -10.0],
            intent_name: String::new(),
            dim_info: 0,
            magic: "n+1".into(),
        }
    }

    fn volume(version: NiftiVersion) -> Nifti {
        Nifti {
            header: header(version),
            extensions: Vec::new(),
            data: TypedArray::Float32((0..8).map(|i| i as f32).collect()),
        }
    }

    #[test]
    fn extensions_round_trip_in_both_versions() {
        use crate::head::AttributeValue;
        for version in [NiftiVersion::Nifti1, NiftiVersion::Nifti2] {
            let mut vol = volume(version);
            vol.extensions.push(NiftiExtension {
                code: 6, // NIFTI_ECODE_COMMENT, kept as raw bytes
                data: b"hello".to_vec(),
            });
            let mut afni = crate::head::Header::default();
            afni.set("DATASET_RANK", AttributeValue::Int(vec![3, 2]));
            afni.set("BRICK_LABS", AttributeValue::String("a b\0c\0".into()));
            afni.set(
                "BRICK_STATAUX",
                AttributeValue::Float(vec![1.0, 3.0, 1.0, 23.0]),
            );
            vol.set_afni_header(&afni);

            let back = Nifti::from_bytes(&vol.to_bytes()).unwrap();
            assert_eq!(back.data, vol.data, "{version:?}");
            assert_eq!(back.extensions.len(), 2);
            assert_eq!(back.extensions[0].code, 6);
            assert_eq!(&back.extensions[0].data[..5], b"hello");
            // esize is padded to a multiple of 16 (8-byte prefix + payload).
            assert_eq!((8 + back.extensions[0].data.len()) % 16, 0);

            let got = back.afni_header().unwrap().unwrap();
            assert_eq!(got.brick_labels(), ["a b", "c"]);
            let stats = got.brick_stats();
            assert!(stats[0].is_none());
            assert_eq!(stats[1].as_ref().unwrap().to_statsym(), "Ttest(23)");
        }
        assert_eq!(volume(NiftiVersion::Nifti1).afni_header().unwrap(), None);
    }

    #[test]
    fn nifti1_round_trips() {
        let vol = volume(NiftiVersion::Nifti1);
        let bytes = vol.to_bytes();
        let back = Nifti::from_bytes(&bytes).unwrap();
        assert_eq!(back.header.version, NiftiVersion::Nifti1);
        assert_eq!(back.shape(), vec![2, 2, 2]);
        assert_eq!(back.header.descrip, "test volume");
        assert_eq!(back.header.affine()[0][0], 2.0);
        assert_eq!(back.data, vol.data);
        // raw 3 -> scaled 2*3+1 = 7
        assert_eq!(back.get_scaled(3), Some(7.0));
    }

    #[test]
    fn nifti2_round_trips() {
        let vol = volume(NiftiVersion::Nifti2);
        let bytes = vol.to_bytes();
        let back = Nifti::from_bytes(&bytes).unwrap();
        assert_eq!(back.header.version, NiftiVersion::Nifti2);
        assert_eq!(back.shape(), vec![2, 2, 2]);
        assert_eq!(back.header.magic, "n+2");
        assert_eq!(back.voxel(1, 1, 1, 0), Some(2.0 * 7.0 + 1.0));
    }

    #[test]
    fn detects_big_endian() {
        let vol = volume(NiftiVersion::Nifti1);
        let mut bytes = vol.to_bytes();
        // Sanity: little-endian sizeof_hdr is 348.
        assert_eq!(
            i32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]),
            348
        );
        // Flip nothing; just confirm detect() agrees.
        let (v, little) = detect(&bytes).unwrap();
        assert_eq!(v, NiftiVersion::Nifti1);
        assert!(little);
        bytes.clear();
    }
}
