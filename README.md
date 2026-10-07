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
| `dset` | Surface datasets (`AFNI_dataset`), incl. label datasets and time series | `.niml.dset` | ✅ | ✅ |
| `labels` | Label tables (`VALUE_LABEL_DTABLE`, `AFNI_labeltable`) | `.niml.lt`, `.niml.cmap` | ✅ | ✅ |
| `roi`  | Drawn surface ROIs (`Node_ROI`) | `.niml.roi` | ✅ | ✅ |
| `graph` | FATCAT/SUMA network datasets (`Graph_Bucket`) | `.niml.dset` | ✅ | ✅ |
| `tract` | FATCAT tract networks (`TAYLOR_TRACT_DATUM`) | `.niml.tract` | ✅ | ✅ |
| `brik` | AFNI volume datasets (`.HEAD`/`.BRIK` pair), all 8 datum types | `.HEAD` + `.BRIK`/`.BRIK.gz` | ✅ | ✅ |
| `head` | `.HEAD` attributes only (header of the pair) | `.HEAD` | ✅ | ✅ |
| `spec` | SUMA surface spec files, as SUMA resolves them (`Spec::resolve`) | `.spec` | ✅ | ✅ |
| `surface` | FreeSurfer/SUMA ASCII surfaces | `.asc` | ✅ | ✅ |
| `gifti` | GIfTI surface/data XML | `.gii`, `.gii.gz`, `.gii.dset` | ✅ | ✅ |
| `surface_dataset` | NIML or GIfTI surface data through one core-facing API | `.niml.dset`, `.gii.dset`, `*.gii` | ✅ | NIML |
| `nifti` | NIfTI-1 / NIfTI-2 volumes, with header extensions (incl. AFNI's) | `.nii`, `.nii.gz`, `.hdr`/`.img` | ✅ | ✅ |
| `volume` | Either of the above through one API (`read_any`) | any of the above | ✅ | — |
| `onedee` | Numeric text tables | `.1D` | ✅ | ✅ |
| `talk` | AFNI ⇄ SUMA talk protocol encoding: port numbers, framing, `SUMA_ixyz`/`SUMA_ijk`/crosshair/`SUMA_irgba` elements (feature `talk`, no sockets) | TCP | ✅ | ✅ |
| `freesurfer` | FreeSurfer binary triangle surfaces | `lh.white`, `rh.pial`, … | ✅ | ✅ |
| `stc` | MNE source estimates (time series on vertices) | `.stc` | ✅ | ✅ |
| `adapt` | Adapters into `afni-core` datasets | any of the above | ✅ | NIML only |

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

**One volume API** (`volume::read_any` and `VolumeReader`): AFNI and NIfTI volumes read the same
way. You get the grid, the number of volumes, the `ijk -> RAS` matrix, each
volume as scaled `f32`, and labels and statistics (from the `.HEAD` or the
NIfTI's AFNI extension).

**One surface-dataset API** (`SurfaceDatasetReader`): `.niml.dset` and GIfTI
data files open as the same validated `afni_core::Dataset`. Format detection
uses the document root, not just the suffix. Dense datasets need only a path;
genuinely sparse datasets also need the surface's complete node count through
`SurfaceDatasetReadOptions`. Reading is eager because the XML/NIML container is
decoded as a whole. NIML inputs retain their extra attributes for lossless
write-back, while GIfTI inputs retain the original document for inspection.

**NIML** (`niml`): numeric columns are stored in their declared type
(`byte`/`short`/`int`/`float`/`double`), so a `float` dataset takes half the
memory it would as `f64`. Variable-length records (drawn-ROI strokes, tracts)
are read and written in ASCII and binary. `serialize_binary` writes the
`binary.lsbfirst` form AFNI and SUMA use for large data, and `parse_stream`
parses a stream that is still arriving (e.g. from an AFNI or SUMA socket).
It returns the complete elements plus the number of bytes they used.

**Surface datasets** (`dset`): every `AFNI_atr` attribute is kept, so a
dataset survives a read and write intact: history, FDR curves, unique-value
lists, and anything else AFNI adds. Column labels, types, statistics and
ranges are read by position. The time step of a time series is read, and so
is the label table of a label dataset (e.g. a FreeSurfer annotation).
`write` and `write_binary` produce files AFNI reads back value for value.
**Label tables** (`labels`) read and write both AFNI layouts, and
`Header::value_label_table` gives a label volume's table (`3drefit
-labeltable`), from a `.HEAD` or from a NIfTI's AFNI extension.

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

// Either surface-dataset format through one API
let surface_data = SurfaceDatasetReader::open("lh.thickness.niml.dset")?;
let dset = surface_data.dataset();
println!("{} stored nodes x {} columns", dset.row_count(), dset.columns().len());

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

## Writing an AFNI-style command

The higher-level volume API is intended to cover the routine work that an
AFNI C program normally performs with `THD_*` and `EDIT_*`: read scaled data,
check grids, extract voxel time series, construct an output on an existing
grid, choose its disk representation, attach metadata, and write it without
silently replacing an existing dataset.

```rust,no_run
use afni_io::prelude::*;

fn run(input_name: &str, mask_name: &str, output_name: &str) -> Result<()> {
    // AFNI HEAD/BRIK and NIfTI inputs use the same read/access API.
    let input = read_any(input_name)?;
    let mask_volume = read_any(mask_name)?;
    let mask = VolumeMask::from_nonzero(&mask_volume, 0)?;
    mask.require_grid(&input.grid()?, 1e-5)?;

    // Checked access gives a useful error for an absent/non-scalar frame.
    let first_frame = input.frame(0)?;
    let mean_inside_mask = first_frame
        .iter()
        .zip(mask.values())
        .filter_map(|(&value, &selected)| selected.then_some(value))
        .sum::<f32>()
        / mask.count() as f32;
    println!("mean = {mean_inside_mask}");

    // Time series are scaled to their true values, independent of how each
    // input sub-brick is stored. `voxel_series_into` accepts a reusable buffer
    // when this operation is performed in a loop.
    let series = input.voxel_series_ijk(10, 12, 4)?;
    println!("voxel has {} time points", series.len());

    // Make an AFNI output on exactly the source grid, retaining an oblique
    // affine when the source has one. AutoShort chooses BRICK_FLOAT_FACS and
    // quantizes to signed shorts; use StoragePolicy::Float for lossless f32.
    let source = input
        .as_afni()
        .ok_or_else(|| Error::invalid("this output example requires an AFNI input"))?;
    let output = BrikBuilder::like_grid(source)?
        .values(first_frame, StoragePolicy::AutoShort)?
        .labels(["masked mean input"])
        .build()?;

    output.write_with_options(
        output_name,
        &BrikWriteOptions {
            overwrite: false,
            history_entry: Some(std::env::args().collect::<Vec<_>>().join(" ")),
        },
    )?;
    Ok(())
}
# Ok::<(), afni_io::Error>(())
```

For core-facing programs, the file mask bridge removes the manual grid check
and Boolean copy:

```rust,no_run
use afni_io::prelude::*;

let input = VolumeDataset::open("stats+orig")?;
let file_mask = VolumeMask::read("mask+orig")?;
let mask = file_mask.for_dataset(input.dataset())?;
println!("{} selected voxels", mask.count());
# Ok::<(), afni_io::Error>(())
```

`GridSpec` and `afni_core::VolumeDomain` have checked conversions in both
directions. Both expose i-fastest linear indexing, `ijk_to_world`,
`world_to_ijk`, and affine-correct `crop` and `pad` grid descriptions. A file
mask can therefore become a domain-checked `SampleMask` without program code
copying its Boolean values or reconstructing a domain.

AFNI spatial transforms are separate from those grid affines. A one-row or
multi-row `*.aff12.1D` file is read into validated core transform types:

```rust,no_run
use afni_io::prelude::*;

let transforms = read_aff12_series("motion.aff12.1D")?;
let moved = transforms.apply_point(0, [10.0, 20.0, 30.0])?;
let inverse = transforms.inverse()?;
write_aff12_series("motion_inverse.aff12.1D", &inverse)?;
# Ok::<(), Box<dyn std::error::Error>>(())
```

Use `read_aff12_transform` when exactly one row is required. `Aff12File`
preserves comments. Files are explicitly AFNI DICOM/RAI; a RAS transform must
be converted before it can be written, preventing a silent coordinate-system
mistake.

For a new grid, use `BrikBuilder::cardinal` or `BrikBuilder::affine`. A loaded
`Brik` can also be edited with `replace_sub_brick`, `push_sub_brick`, and
`set_brick_labels`. `Header` has typed setters for history, labels, time axes,
and per-brick statistics. For algorithms that operate through `afni-core`,
`VolumeDataset::open` and `VolumeDataset::to_brik` preserve source
attributes that core does not model while rebuilding storage-dependent ones.

HEAD/BRIK output is staged and flushed in the destination directory before it
is published. The BRIK is installed first and the HEAD last, so a new dataset
is not discoverable until both files are ready. Replacing an existing pair
temporarily moves the old files to private backups and restores them if any
commit step fails. `overwrite: false` uses atomic no-replace publication, so a
file created by another process during the write is not silently overwritten.
As with any two-file format, the operating system cannot replace the pair in a
single syscall; rollback covers reported I/O failures, while abrupt process or
machine termination can leave clearly named hidden staging/backup files for
manual recovery.

The same processed volume can be written as AFNI or NIfTI without changing the
algorithm. `Volume::write` and `VolumeDataset::write` infer the format from the
output name: `.nii`/`.nii.gz` means NIfTI, an AFNI `+view` name means
HEAD/BRIK, and a bare prefix follows AFNI command-line convention. Use one
options type for either format:

```rust,no_run
use afni_io::prelude::*;

let input = read_any("stats+orig")?;
let output = VolumeBuilder::like(&input)?
    .values(input.frame(0)?)?
    .labels(["processed statistic"])
    .history("created by the Rust implementation")
    .build()?;
let options = VolumeWriteOptions {
    overwrite: false,
    history_entry: Some("3dRustCommand -input stats+orig".into()),
    afni_storage: StoragePolicy::AutoShort,
    nifti_version: NiftiVersion::Nifti2,
    ..VolumeWriteOptions::default()
};

// The same values and semantic metadata can target either representation.
output.write_with_options("result+orig", &options)?;
output.write_with_options("result.nii.gz", &options)?;
# Ok::<(), afni_io::Error>(())
```

For algorithms using `afni-core`, open a `VolumeDataset` and call the same
methods on it;
`to_brik` and `to_nifti` are available when the concrete in-memory format is
needed. Both outputs retain the grid, values, labels, timing, statistics,
history, label tables, and FDR/MDF curves. `VolumeMetadata` additionally keeps
detailed slice timing, the original NIfTI header template, and arbitrary NIfTI
extensions that `afni-core` does not model. Structural fields are regenerated,
so retained metadata cannot restore stale dimensions, transforms, or scaling.
NIfTI stores values as unscaled `f32` and carries AFNI-specific or per-frame metadata in its AFNI extension.
Single-frame statistics are also written to the NIfTI intent fields (with
AFNI's three-parameter convention for correlation).
NIfTI publication is staged and atomic, and no-clobber mode is race-safe.
Detached `.hdr`/`.img` output is deliberately rejected for now.

### Load a volume for an `afni-core` algorithm

`VolumeDataset` is the ordinary eager type for a command that needs the whole
dataset. It reads all selected frames into an `afni-core::Dataset` and keeps the
source-only metadata beside it for a later AFNI or NIfTI write:

```rust,no_run
use afni_io::prelude::*;

let source = VolumeDataset::open("rest+orig[0..99]")?;
let result = afni_core::processing::summarize_time_series(
    source.dataset(),
    None,
    [afni_core::processing::TimeSeriesStatistic::Mean.output("mean", 0.0)?],
    afni_core::numeric::NonFinitePolicy::Skip,
)?;

// The result keeps the source grid and safe source-only metadata. Labels,
// statistics, timing, dimensions, datatype, and scaling are rebuilt from the
// newly computed Dataset rather than copied from the input frames.
source
    .derive(result)?
    .with_history("3dRustMean -input rest+orig[0..99]")
    .write("mean+orig")?;
# Ok::<(), afni_io::Error>(())
```

`VolumeDataset::open` is therefore the convenient default for most programs.
Use `VolumeReader` when the algorithm can operate frame-by-frame and should not
hold every frame in memory. Calling `VolumeReader::into_dataset` explicitly
crosses from that lazy interface to the fully loaded representation.

### Select and stream frames

`VolumeReader` reads only metadata when it opens a dataset and keeps at most
one decoded frame in memory. Its path accepts AFNI's trailing sub-brick syntax:

```rust,no_run
use afni_io::prelude::*;

// Inclusive ranges, `$` for the last frame, strides, descending ranges,
// labels, duplicates, and caller-specified order are supported.
let mut input = VolumeReader::open("rest+orig[0,10..$(5),Full_Fstat]")?;
println!("source frames: {:?}", input.selected_indices());
println!("selected labels: {:?}", input.labels());

// Reuse one allocation while processing an arbitrarily large time series.
let mut frame = vec![0.0; input.voxels()];
for t in 0..input.nvols() {
    input.frame_into(t, &mut frame)?;
    // Process `frame` here; the other frames remain on disk.
}

// Or use an iterator, which likewise loads one frame at a time.
for frame in input.frames() {
    let frame = frame?;
    println!("first voxel: {}", frame[0]);
}
# Ok::<(), afni_io::Error>(())
```

The same interface handles `.HEAD`/`.BRIK`, `.BRIK.gz`, `.nii`, `.nii.gz`,
and detached `.hdr`/`.img` pairs. Ordinary files use absolute seeks. Gzip
files are reopened and streamed up to the requested frame, which stays
memory-bounded but means the decoding cost grows with the source-frame offset.
Repeated random access to late gzip frames is therefore more expensive than
using the eager `read_any` API when the entire dataset fits in memory.

`VolumeFrameWriter` is the matching memory-bounded output API. Declare the
grid and frame metadata first, then hand it one processed frame at a time. It
writes directly to a hidden staging file, supports gzip and AFNI short
scaling, and does not publish a partial dataset if processing fails early:

```rust,no_run
use afni_io::prelude::*;

let mut input = VolumeReader::open("rest+orig")?;
let spec = VolumeWriteSpec::like_reader(&input)?;
let options = VolumeWriteOptions {
    afni_storage: StoragePolicy::AutoShort,
    history_entry: Some("3dRustCommand -input rest+orig".into()),
    ..VolumeWriteOptions::default()
};
let mut output = VolumeFrameWriter::create("result+orig.BRIK.gz", spec, options)?;
let mut frame = vec![0.0; input.voxels()];
for t in 0..input.nvols() {
    input.frame_into(t, &mut frame)?;
    // Transform `frame` in place here.
    output.write_frame(&frame)?;
}
output.finish()?;
# Ok::<(), afni_io::Error>(())
```

The same code can target `.nii` or `.nii.gz` by changing the output name.
`finish()` also verifies that the declared number of frames was supplied.

`read_any("rest+orig[0,2,$]")` also accepts selectors and materializes only
those frames into the ordinary eager `Volume` representation. Labels,
statistics, and timing metadata follow AFNI's selector behavior: the source TR
is retained for any multi-frame result, while a single selected frame becomes
a bucket without a time axis.

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

## Meaning lives in afni-core

`afni-io` decodes and encodes files; what the decoded data *means* (p-values,
FDR q-values, color maps, thresholds) is in
[`afni-core`](https://github.com/pmolfese/afni-core), which this crate depends on.
The `adapt` module converts decoded files into core types: `NimlDataset`,
`Gifti`, `Volume` and `OneD` become an `afni_core::dataset::Dataset`, label tables
become core label tables, and a core dataset can be written back through a
`NimlEnvelope` that keeps every attribute core does not model. A drawn `NodeRoi` converts
to a core `Roi` with `adapt::roi_to_core` (codes SUMA does not define are kept, and the
attributes core cannot express travel in a `RoiEnvelope`, so writing back is lossless).
`graph_to_core` / `graph_from_core` and `tracts_to_core` / `tracts_from_core` do the same for
networks and tracts (pass the original file as the template to keep its history, links and
grid datasets). `AFNI_IO_LIVE=1 cargo test` also hands the graphs we write back to `ConvertDset`. `StatKind` and
`StatSpec` are defined in `afni-core` and re-exported from `afni_io::stat`.

The two crates are separate repositories. `Cargo.toml` fetches `afni-core` from
GitHub (tracking `main` for now; it will move to tagged releases), so a fresh
clone of `afni-io` builds on its own.

### Building against a local `afni-core`

To edit both crates together, check them out side by side and create a
git-ignored `.cargo/config.toml` in `afni-io` that patches the git dependency to
the sibling checkout:

```toml
[patch."https://github.com/pmolfese/afni-core"]
afni-core = { path = "../afni-core" }
```

```text
afni_rust/
  afni-core/
  afni-io/
```

## Testing

```sh
cargo test                  # unit tests + committed fixtures
cargo test --features talk  # also the AFNI/SUMA talk protocol (`afni_io::talk`)
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
