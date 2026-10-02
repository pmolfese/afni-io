# afni

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
| `niml` | Generic NIML element trees (ASCII + binary) | `.niml`, `.niml.asc` | ✅ | ✅ (ASCII) |
| `dset` | Surface datasets (`AFNI_dataset`) | `.niml.dset` | ✅ | ✅ |
| `roi`  | Drawn surface ROIs (`Node_ROI`) | `.niml.roi` | ✅ | ✅ |
| `brik` | AFNI volume datasets (`.HEAD`/`.BRIK` pair) | `.HEAD` + `.BRIK`/`.BRIK.gz` | ✅ | — |
| `head` | `.HEAD` attributes only (header of the pair) | `.HEAD` | ✅ | ✅ |
| `spec` | SUMA surface spec files | `.spec` | ✅ | — |
| `surface` | FreeSurfer/SUMA ASCII surfaces | `.asc` | ✅ | ✅ |
| `gifti` | GIfTI surface/data XML | `.gii`, `.gii.gz`, `.gii.dset` | ✅ | ✅ |
| `nifti` | NIfTI-1 / NIfTI-2 volumes | `.nii`, `.nii.gz`, `.hdr`/`.img` | ✅ | ✅ |
| `onedee` | Numeric text tables | `.1D` | ✅ | ✅ |

An AFNI volume is one dataset stored as a `.HEAD`/`.BRIK` pair; `Brik::read`
takes any member (`.HEAD`, `.BRIK`, or `.BRIK.gz`), finds its sibling, and
decompresses a gzipped `.BRIK.gz` automatically. Binary NIML
(`binary.lsbfirst` / `binary.msbfirst`) and the AFNI BRIK datum types `byte`,
`short`, `float`, and `complex` are handled, including per sub-brick scale
factors and byte order.

**NIfTI**: both the 348-byte NIfTI-1 and 540-byte NIfTI-2 headers, automatic
byte-order detection, `scl_slope`/`scl_inter` scaling, and the qform/sform
voxel-to-world affine. Single-file `.nii`/`.nii.gz` and detached `.hdr`/`.img`
pairs are read; writing emits a single-file little-endian `.nii(.gz)`.

**GIfTI**: ASCII, `Base64Binary`, and `GZipBase64Binary` data arrays (plus the
legacy `GIFTI_ENCODING_*` token spellings), metadata, coordinate systems, and
label tables. Writing emits ASCII or Base64.

## Example

```rust
use afni::prelude::*;

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
let mesh = gii.to_surface()?; // -> afni::surface::Surface
println!("{} vertices, {} faces", mesh.n_vertices(), mesh.n_faces());

// NIfTI volume (.nii / .nii.gz / .hdr+.img)
let vol = Nifti::read("epi.nii.gz")?;
println!("shape {:?}, affine {:?}", vol.shape(), vol.header.affine());
let intensity = vol.voxel(32, 32, 12, 0); // scaled value at a voxel
# Ok::<(), afni::Error>(())
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

## License

MIT OR Apache-2.0
