# afni-io

Native Rust readers and writers for the file formats used by
[AFNI](https://afni.nimh.nih.gov/) and its surface viewer **SUMA**.

Everything is parsed in pure Rust — no AFNI binaries are invoked. The NIfTI and
GIfTI readers are owned in-crate (their own binary header, XML, and Base64
code); the only dependencies are
[`thiserror`](https://crates.io/crates/thiserror) and
[`flate2`](https://crates.io/crates/flate2) (gzip, pure-Rust backend).

## Supported formats

| Module | Format | Extensions | Read | Write |
|--------|--------|-----------|:----:|:-----:|
| `niml` | Generic NIML element trees (ASCII, binary, base64), including streams | `.niml`, `.niml.asc` | ✅ | ✅ |
| `dset` | Surface datasets (`AFNI_dataset`) | `.niml.dset` | ✅ | ✅ |
| `roi`  | Drawn surface ROIs (`Node_ROI`) | `.niml.roi` | ✅ | ✅ |
| `brik` | AFNI volume datasets (`.HEAD`/`.BRIK` pair), all 8 datum types | `.HEAD` + `.BRIK`/`.BRIK.gz` | ✅ | ✅ |
| `head` | `.HEAD` attributes only (header of the pair) | `.HEAD` | ✅ | ✅ |
| `spec` | SUMA surface spec files | `.spec` | ✅ | — |
| `surface` | FreeSurfer/SUMA ASCII surfaces | `.asc` | ✅ | ✅ |
| `gifti` | GIfTI surface/data XML | `.gii`, `.gii.gz`, `.gii.dset` | ✅ | ✅ |
| `nifti` | NIfTI-1 / NIfTI-2 volumes, with header extensions (incl. AFNI's) | `.nii`, `.nii.gz`, `.hdr`/`.img` | ✅ | ✅ |
| `volume` | Either of the above through one API (`read_any`) | any of the above | ✅ | — |
| `onedee` | Numeric text tables | `.1D` | ✅ | ✅ |

An AFNI volume is one dataset stored as a `.HEAD`/`.BRIK` pair. `Brik::read`
accepts any name AFNI does (`.HEAD`, `.BRIK`, `.BRIK.gz`, or `prefix+orig`,
`+acpc`, `+tlrc` with or without a trailing `.`), and `AfniPaths` exposes that
lookup on its own. All eight AFNI datum types are read (byte, short, int,
float, double, complex, rgb, rgba), including datasets that mix them. Each
sub-brick keeps its stored type and scale factor, and has scaled `f32`
accessors. `Brik::read_sub_bricks` loads only the sub-bricks you ask for, and
`.BRIK.gz` is decompressed as it streams. `Brik::write` writes a dataset
(plain or gzipped) that AFNI reads back voxel-for-voxel, and `Brik::new`
builds one from scratch. Binary NIML (`binary.lsbfirst` / `binary.msbfirst`)
is also read.

**One volume API** (`volume::read_any`): AFNI and NIfTI volumes read the same
way. You get the grid, the number of volumes, the `ijk -> RAS` matrix, each
volume as scaled `f32`, and labels and statistics (from the `.HEAD` or the
NIfTI's AFNI extension).

**NIML** (`niml`): numeric columns are stored in their declared type
(`byte`/`short`/`int`/`float`/`double`), so a `float` dataset takes half the
memory it would as `f64`. Variable-length records (drawn-ROI strokes, tracts)
are read and written in ASCII and binary. `serialize_binary` writes the
`binary.lsbfirst` form AFNI and SUMA use for large data, and `parse_stream`
parses a stream that is still arriving (e.g. from an AFNI or SUMA socket).
It returns the complete elements plus the number of bytes they used.

**Geometry** (`geometry`, plus methods on `head::Header`): the voxel-to-world
matrix in AFNI's DICOM/RAI convention (`ijk_to_dicom`, using
`IJK_TO_DICOM_REAL` when present) and in RAS (`ijk_to_ras`, the same as the
sform `3dAFNItoNIFTI` writes), obliquity as `3dinfo` computes it, orientation
string, view, and the time axis (TR, slice timing; milliseconds converted to
seconds as AFNI does). Coordinates are always returned in the convention of
the file they came from. The `geometry` module docs explain RAI vs RAS for
AFNI, NIfTI, GIfTI and FreeSurfer.

**Statistics** (`stat`): one `StatSpec` type (e.g. `Ttest(23)`) for every
place AFNI records what a sub-brick or column holds: `.HEAD`
`BRICK_STATSYM`/`BRICK_STATAUX` (`Header::brick_stats`), the NIfTI AFNI
extension, NIML `COLMS_STATSYM`, and GIfTI intents (`DataArray::stat`). AFNI
stat codes are NIfTI intent codes, so these all map to the same 23 kinds.
p-value maths is left to other crates.

**NIfTI**: both the 348-byte NIfTI-1 and 540-byte NIfTI-2 headers, automatic
byte-order detection, `scl_slope`/`scl_inter` scaling, and the qform/sform
voxel-to-world affine. Single-file `.nii`/`.nii.gz` and detached `.hdr`/`.img`
pairs are read; writing emits a single-file little-endian `.nii(.gz)`. Header
extensions are kept and written back. `Nifti::afni_header()` decodes AFNI's
extension (ecode 4) into the same `Header` type a `.HEAD` file gives, so
labels, statistics and geometry read the same way from either format.

**GIfTI**: ASCII, `Base64Binary`, and `GZipBase64Binary` data arrays (plus the
legacy `GIFTI_ENCODING_*` token spellings), metadata, coordinate systems, and
label tables. Writing emits ASCII or Base64.

## Example

```rust
use afni_io::prelude::*;

// Surface dataset
let dset = NimlDataset::read("lh.thickness.niml.dset")?;
println!("{} nodes x {} columns", dset.rows(), dset.columns());

// Volume
let brik = Brik::read("anat+orig.HEAD")?;
println!("{:?} x {} sub-bricks", brik.dimensions, brik.nvals());
let v = brik.value(32, 32, 15, 0); // scaled value at a voxel

// Drawn ROIs
for roi in NodeRoi::read_all("V1.niml.roi")? {
    println!("{}: {} nodes", roi.label, roi.unique_nodes().len());
}

// GIfTI surface (any encoding)
let gii = Gifti::read("rh.white.gii")?;
let mesh = gii.to_surface()?; // -> afni_io::surface::Surface
println!("{} vertices, {} faces", mesh.n_vertices(), mesh.n_faces());

// Either volume format through one API
let vol = read_any("stats+tlrc.HEAD")?; // or "stats.nii.gz"
let first = vol.frame_f32(0); // scaled values, i fastest
println!("{:?} x {} volumes, labels {:?}", vol.dimensions(), vol.nvols(), vol.labels()?);

// NIfTI volume (.nii / .nii.gz / .hdr+.img)
let vol = Nifti::read("epi.nii.gz")?;
println!("shape {:?}, affine {:?}", vol.shape(), vol.header.affine());
let intensity = vol.voxel(32, 32, 12, 0); // scaled value at a voxel
# Ok::<(), afni_io::Error>(())
```

A small CLI that dispatches on extension lives in `examples/inspect.rs`:

```sh
cargo run --example inspect -- path/to/file.niml.dset
```

## Design

The `niml` module is the foundation: it parses any NIML stream into a generic
`NimlElement` tree and serialises it back to ASCII. The `dset` and `roi`
modules interpret the specific tag layouts on top of that tree, so unusual
files can always be inspected element-by-element.

Format details were cross-checked against AFNI's C source, its MATLAB readers
(`BrikInfo.m`, `afni_niml_*.m`, `README.attributes`), the SUMAvista Python
implementation, and the NIfTI-1/NIfTI-2 and GIfTI 1.0 specifications. The
readers are validated against real-world fixtures (nibabel's GIfTI test files
in ASCII/Base64/GZipBase64 form and `example4d.nii.gz`).

## Testing

```sh
cargo test                  # unit tests + committed fixtures
cargo test -- --ignored     # known gaps, each labelled with its roadmap phase
AFNI_IO_REFERENCE_DIR=../sumaru/testing cargo test   # also large/private files
```

The integration tests check the readers against AFNI's own output for the same
files (`3dmaskdump`, `3dinfo`, `ConvertDset`). The fixtures and the scripts that
regenerate them are described in [`tests/data/README.md`](tests/data/README.md).

## License

Public domain. afni-io is a United States Government work (17 U.S.C. § 105).
Outside the US, rights are waived under
[CC0 1.0](https://creativecommons.org/publicdomain/zero/1.0/). See
[`LICENSE`](LICENSE).
