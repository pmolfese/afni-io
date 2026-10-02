# Update the `afni-io` crate

Handoff brief for bringing `afni_rust` (crate name `afni-io`, imported as `afni_io`) up to date with the
AFNI-format knowledge that has accumulated in `sumaru`, so that it can become the
single file-I/O crate shared by sumaru and a future 2D AFNI slice viewer.

## Background

- `afni_rust/` (this repo): pure-Rust readers/writers for HEAD/BRIK, NIfTI-1/2,
  GIfTI, NIML datasets/ROIs, `.spec`, `.asc` surfaces, `.1D`. Only deps are
  `thiserror` and `flate2`. `unsafe_code = "forbid"`. MSRV 1.74, edition 2021.
- `../sumaru/`: an egui + wgpu SUMA-style viewer (~54k lines). It does **not**
  depend on this crate. It has its own HEAD/BRIK reader in `src/volume.rs`, uses
  the third-party `nifti` crate (pulls in nalgebra/ndarray) and `gifti-rs`, and
  its own NIML code in `src/io/niml.rs`.
- `../afni/src/`: the AFNI C source, which is the reference implementation.
  Useful files: `thd_opendset.c`, `thd_loaddblk.c`, `thd_dsetdblk.c`,
  `thd_niftiread.c`, `thd_niftiwrite.c`, `thd_nimlatr.c`, `thd_writedset.c`,
  `thd_writedblk.c`, `3ddata.h`, `mrilib.h`, and `README.attributes` (attribute
  semantics).

The long-term plan is a Cargo workspace (`afni-io` I/O, `afni-core` for
stats/color/overlay/cluster, `sumaru`, `afni-view`). **That restructuring is
out of scope here.** This task only improves this crate.

## Ground rules

- Keep `unsafe_code = "forbid"` and the minimal dependency list. Don't add
  nalgebra/ndarray. Use plain `[[f64; 4]; 4]` or `[[f32; 4]; 4]` for matrices.
- Don't break the existing public API without a reason. If you must change it,
  note it in the summary at the end.
- **Do not `git add`, commit, or push.** The user does all git operations
  themselves. When finished, tell them what changed and give them the commands.
  The repo is `github.com/pmolfese/afni-io`.
- Match the existing style: module-level `//!` docs that explain the format,
  `Error::invalid` / `Error::missing` / `Error::unsupported` / `Error::parse`
  from `src/error.rs`, and tests next to the code.
- Cross-check behavior against the AFNI C source and cite the C file in doc
  comments, as `brik.rs` already does.

## Tasks (in priority order)

### 1. HEAD/BRIK reader: port sumaru's improvements

Source: `../sumaru/src/volume.rs` (`read_afni`, `AfniPaths::resolve`,
`AfniHeader::voxel_to_ras`, `decode_afni_brick`).

1. **All AFNI datum codes.** `BrikType::from_code` currently rejects 2 (int),
   4 (double) and 6 (rgb). Support 0 byte, 1 short, 2 int, 3 float, 4 double,
   5 complex, 6 rgb, 7 rgba (check `MRI_TYPE` in `mrilib.h` to confirm 7).
   - Unlike sumaru, **don't collapse RGB to luminance.** That's a viewer
     decision. Keep the data's real type available. Suggested approach: keep a
     typed representation per sub-brick (an enum over `Vec<u8>`, `Vec<i16>`,
     `Vec<i32>`, `Vec<f32>`, `Vec<f64>`, complex, rgb/rgba) and offer
     convenience accessors that return scaled `f32`. If changing `sub_bricks:
     Vec<Vec<f32>>` is too disruptive, add a new typed field or API and keep
     the f32 path for scalar types. Your call; explain the choice.
   - Datasets with mixed types across sub-bricks are legal and must work.
2. **Missing `BRICK_TYPES` defaults to short** for every sub-brick, as AFNI's
   loader does.
3. **Missing `BYTEORDER_STRING` defaults to native byte order** (AFNI's legacy
   behavior).
4. **Path resolution.** Accept `.HEAD`, `.BRIK`, `.BRIK.gz`, and prefix forms
   `name+orig`, `name+orig.`, `+acpc`, `+tlrc` (with or without a trailing
   dot). Prefer `.BRIK` over `.BRIK.gz` unless the user named the `.gz`
   explicitly. Make this public (for example `brik::resolve_pair` or
   `AfniPaths`), so callers can tell whether a path is an AFNI dataset.
   - Optional: `.BRIK.bz2` / `.BRIK.Z` exist in the wild. Return
     `Error::unsupported` with a clear message rather than a parse error.
5. **Truncation and overflow safety.** Use `checked_mul` for voxel and byte
   counts (as sumaru does), and give a clear error for a short BRIK.
6. **Non-finite values:** sumaru maps NaN/Inf to 0. Decide whether the crate
   should keep raw values (probably yes) and leave sanitising to callers.
   Document the choice.

### 2. Geometry: voxel ↔ world affine

Add to `head::Header` (or a new `geometry` module):

- `ijk_to_dicom()`: a 4×4 matrix in AFNI's native DICOM/LPS ("RAI") convention.
  Build it from `IJK_TO_DICOM_REAL` (12 floats, row-major 3×4) when present.
  Otherwise build it from `ORIENT_SPECIFIC` + `ORIGIN` + `DELTA`. Orientation
  codes 0–5 are R2L, L2R, P2A, A2P, I2S, S2I. **Check sign handling against
  `THD_daxes_to_mat44` / `thd_coords.c`**: sumaru's fallback places `DELTA` and
  `ORIGIN` directly, which is correct only because AFNI stores signed deltas.
  Confirm this.
- `ijk_to_ras()`: the same with the first two rows negated (LPS → RAS),
  matching NIfTI's sform convention. This is what sumaru's `voxel_to_ras` returns.
- `is_oblique()`: compare `IJK_TO_DICOM_REAL` with the cardinal matrix built
  from `ORIENT_SPECIFIC`.
- Also expose the view type (`SCENE_DATA[0]`: 0 = orig, 1 = acpc, 2 = tlrc)
  and `TAXIS_*` timing (TR, nt) if they aren't already exposed.

Test: for a dataset converted with `3dAFNItoNIFTI`, `ijk_to_ras()` from the
HEAD must equal the NIfTI's sform (within float tolerance).

### 3. Stat metadata on sub-bricks

AFNI thresholding depends on it. Parse:

- `BRICK_STATAUX`: a flat float list of repeating records
  `[sub-brick index, stat code, npar, par1..parN]`. Stat codes are the
  `FUNC_*_TYPE` constants in `3ddata.h` (for example FUNC_COR_TYPE,
  FUNC_TT_TYPE, FUNC_FT_TYPE, FUNC_ZT_TYPE, FUNC_CT_TYPE, ...). Expose a typed
  enum per sub-brick, such as `StatKind::TTest { dof }` or
  `StatKind::FTest { num, den }`.
- `BRICK_STATSYM` (newer string form, for example `Ttest(23);none;Ftest(2,40)`)
  when present.
- `BRICK_LABS` already works through `brick_labels()`. Make sure
  `~`-separated labels round-trip.
- FDR curves (`FDRCURVE_*` / `MDFCURVE_*` attributes) are optional here. sumaru
  already reads them from NIML (`sumaru/src/dataset.rs`, `AfniFdrCurve`).

Compare against `../sumaru/src/stats.rs` (`AfniStatSpec::parse`), which
parses the string labels. The structs should map onto it cleanly so sumaru can
use this crate's types later. **Don't move sumaru's p-value math into this
crate.** It belongs in the future `afni-core`.

### 4. NIfTI: AFNI header extension (ecode 4)

Currently `src/nifti.rs` ignores extensions ("no header extensions"). AFNI
writes its full attribute set into a NIfTI extension with
`ecode == NIFTI_ECODE_AFNI (4)`. Without it, a `.nii` from `3dttest++`,
`3dDeconvolve` and similar tools loses its stat codes, degrees of freedom and
sub-brick labels.

- Read the extension list after the header. NIfTI-1: 4-byte extender at byte
  348, then `(esize, ecode, data)` records padded to 16 bytes, up to
  `vox_offset`. NIfTI-2 starts at 540.
- For ecode 4, the payload is NIML XML: an `AFNI_attributes` group of
  `AFNI_atr` elements, each with `atr_name`, `ni_type` and values. See
  `thd_niftiread.c` around lines 790–870 and `THD_dblkatr_from_niml` in
  `thd_nimlatr.c`; the write side is `thd_niftiwrite.c` around line 739.
  Parse it with the crate's existing `niml` module. Don't write a new parser.
- Expose it as `Nifti::afni_header() -> Option<Header>` (reusing
  `head::Header`), so tasks 2–3 work the same for NIfTI and HEAD/BRIK.
- Keep unknown extensions as raw `(ecode, Vec<u8>)` so they're not lost.
- Optional: write the AFNI extension back out on NIfTI write.

### 5. HEAD/BRIK writing

`head::Header::write` exists; `Brik` has no writer. Add `Brik::write(prefix)`
that writes `.HEAD` + `.BRIK` (optionally gzipped), updating `BRICK_TYPES`,
`BRICK_FLOAT_FACS`, `DATASET_RANK`, `BYTEORDER_STRING` and `IDCODE_STRING`
consistently. Round-trip test: read → write → read gives identical header
attributes (except IDCODE/date) and identical voxel data.

### 6. Housekeeping

- Add integration tests for `brik`/`head` (there are none in `tests/` today).
- Update `README.md`'s format table (HEAD/BRIK write, NIfTI AFNI extension,
  datum types).
- Update `examples/inspect.rs` to print geometry, oblique flag and per-sub-brick
  stat info.
- Run `cargo fmt`, `cargo clippy --all-targets`, and `cargo test`.

## Test fixtures

Done in Phase 0. See `tests/data/README.md`. `tests/data/volume/` already has
the cases this brief needs: every datum type `3dcalc` can write, mixed datums,
RGB, big-endian, `.BRIK.gz`, stat aux/symbols, TR, LPI with non-zero origin,
tlrc, oblique, and NIfTI with and without the AFNI extension. Each comes with
`3dinfo`/`3dmaskdump` reference dumps. Use `tests/common/mod.rs` to load them.

- Int (2), double (4) and rgba (7) can't be made with `3dcalc`. Build them
  inside the tests (write a `.HEAD` plus raw bytes).
- `tests/fixtures_baseline.rs` has `#[ignore = "Phase N: ..."]` tests for known
  gaps. Remove the `ignore` as each one is fixed.
- To add a case, extend `make_volume_fixtures.sh` and rerun it. Don't hand-edit
  the generated files.

`../sumaru/tests/local_reference_files.rs` and the sumaru sample volume used by
`volume.rs`'s `loads_surfvol_geometry_and_data` test can serve as a
cross-check for the affine (run with `AFNI_IO_REFERENCE_DIR=../sumaru/testing`).

## Definition of done

- Tasks 1–4 are complete with tests. Tasks 5–6 are complete, or explicitly listed
  as remaining.
- `cargo test`, `cargo clippy --all-targets -- -D warnings` and `cargo fmt --check`
  pass.
- A short summary for the user covering: public API changes, decisions made
  (typed storage, NaN handling), anything in the AFNI C source that disagreed
  with sumaru's implementation, and suggested git commands (not run).

## Next step (not part of this task)

Switch sumaru to depend on this crate: replace `sumaru/src/volume.rs`'s private
AFNI reader and the `nifti` / `gifti-rs` dependencies, then start the
`afni-core` extraction (`stats`, `color`, `overlay`, `cluster`, `dataset`).

## Roadmap

This brief covers Phases 1–4. The later phases are listed so the design leaves
room for them.

| # | Phase | Brief task |
|---|-------|-----------|
| 0 | Repo, name (`afni-io`), fixtures, test harness ✅ | — |
| 1 | HEAD/BRIK reader: all datums, typed storage, path lookup, overflow checks, reading only requested sub-bricks, streaming gunzip | 1 |
| 2 | Geometry: affine, oblique flag, view type, TAXIS | 2 |
| 3 | Shared `StatKind`; `BRICK_STATAUX`/`STATSYM`; `AFNI_atr` ↔ `Header` converter; NIfTI ecode-4 extension | 3, 4 |
| 4 | BRIK writing, housekeeping, optional `read_any()`; then move sumaru's volume code onto the crate | 5, 6 |
| 5 | NIML core: binary write, variable-length binary records, incremental parsing, valueless attributes; decide f64 vs typed matrix storage | — |
| 6 | `.niml.dset`/ROI parity with sumaru, reusing `StatKind` and the converter | — |
| 7 | GIfTI swap in sumaru (intent code maps to `StatKind`), drop `gifti-rs` | — |
| 8 | spec, binary FreeSurfer surfaces, `.stc`, `.niml.tract`, Graph_Bucket | — |
| 9 | AFNI talk protocol encoding (behind a `talk` feature) | — |
| 10 | FreeSurfer annot/curv/MGH, `.1D.dset` and selectors, PLY/SureFit/BYU/… surfaces, GIfTI ExternalFileBinary | — |

Found while building the Phase 0 fixtures:

- **Valueless NIML attributes** (`domain_parent_idcode` with no `=`, written by
  `ConvertDset`) break `niml::parse`. AFNI (`niml_header.c`) and sumaru
  (`io/niml.rs`) both accept them. It's a small fix and blocks reading
  real AFNI dsets, so do it early in Phase 1 even though it belongs to Phase 5.
- **SUMA's GIfTI writer negates x and y** compared with `.asc`/`.1D.coord`,
  while declaring `NIFTI_XFORM_UNKNOWN`. Pin down the convention (RAI internally,
  RAS in GIfTI?) in Phase 2 and document it in `gifti`/`surface`.
