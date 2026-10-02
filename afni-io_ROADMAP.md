# afni-io roadmap

Goal: make `afni-io` the single file-I/O crate for sumaru and a future 2D AFNI
slice viewer, so that sumaru can drop its private readers and the `nifti` and
`gifti-rs` dependencies. The long-term workspace is `afni-io` (I/O),
`afni-core` (stats, color, overlay, cluster), `sumaru`, and `afni-view`.

`UPDATE_AFNI_CRATE.md` is the detailed brief for Phases 1–4.

Status: ✅ done · 🚧 in progress · ⬜ not started

| # | Phase | Status |
|---|-------|--------|
| 0 | Setup: repo, name, license, fixtures, test harness | ✅ |
| 1 | HEAD/BRIK reader parity | ✅ |
| 2 | Geometry | ✅ |
| 3 | Stat metadata and the NIfTI AFNI extension | ⬜ |
| 4 | BRIK writing, housekeeping, sumaru volume swap | ⬜ |
| 5 | NIML core parity | ⬜ |
| 6 | `.niml.dset` / ROI parity | ⬜ |
| 7 | GIfTI swap in sumaru | ⬜ |
| 8 | Remaining formats | ⬜ |
| 9 | AFNI talk protocol encoding | ⬜ |
| 10 | Beyond sumaru's current needs | ⬜ |

---

## Phase 0 — Setup ✅

- [x] Rename the crate to `afni-io` (`afni_io`); repository `github.com/pmolfese/afni-io`
- [x] License: US Government work, public domain (CC0 1.0 outside the US)
- [x] Fixture generators that need AFNI on `PATH`: `tests/data/make_volume_fixtures.sh` and `make_surface_fixtures.sh`
- [x] Copy the clean real-world files into `tests/data/real/`, with provenance in `tests/data/README.md`
- [x] Test helpers in `tests/common/mod.rs`. `AFNI_IO_REFERENCE_DIR` enables tests on large or private files
- [x] Baseline tests against AFNI's own dumps (`tests/fixtures_baseline.rs`); known gaps are `#[ignore = "Phase N: …"]`

## Phase 1 — HEAD/BRIK reader parity ✅

Brief task 1. Ported from sumaru's `volume.rs` and checked against AFNI's C
loader (`thd_initdblk.c`, `thd_loaddblk.c`, `thd_dsetto3D.c`).

- [x] All datum codes 0–7. `BrikType` gains `Int`, `Double`, `Rgb`, `Rgba`, plus `code()`
- [x] Typed storage: `BrickData` (one variant per datum) inside `SubBrick { data, factor }`, with scaled `value()`/`to_f32()`. Complex gives its magnitude (as AFNI does); RGB/RGBA give `None` instead of luminance
- [x] Mixed datums across sub-bricks
- [x] AFNI defaults: missing `BRICK_TYPES` means short; a short list repeats its last entry; `BYTEORDER_STRING` is a prefix match with host order as the fallback; any non-zero factor applies, negative included (`Header::brick_type_code`, `Header::brick_factor`)
- [x] Public `AfniPaths::resolve` / `AfniPaths::is_afni_name` for `.HEAD`, `.BRIK`, `.BRIK.gz`, `prefix+orig|acpc|tlrc[.]`. Prefers `.BRIK` unless the `.gz` was named. `.bz2`/`.Z` give `Error::Unsupported`. A header-only dataset gives `brik: None`
- [x] Checked size arithmetic; a short plain BRIK is reported before decoding, and a short gzip stream at the sub-brick where it ran out
- [x] `Brik::read_sub_bricks`: unrequested sub-bricks are skipped (by seeking in plain files) and reading stops after the last one needed. gzip is decoded as it streams (`MultiGzDecoder`) and detected from its magic bytes
- [x] NaN/Inf kept as stored (documented in `brik` module docs)
- [x] NIML attributes with no value (pulled forward from Phase 5): the parser now follows AFNI's header grammar, and the writer writes empty values as bare names
- [x] `BRICK_LABS` parsing (pulled forward from Phase 3): one label per sub-brick, defaulting to `#p`

Public API changes:
- `Brik::sub_bricks` is now `Vec<Option<SubBrick>>` (was `Vec<Vec<f32>>`).
- New `BrikType` variants.
- `Brik::value` gives the magnitude for complex (was the real part) and `None` for RGB.
- `Header::brick_labels()` returns `nvals` entries.
- `Header::brik_is_little_endian` matches by prefix and is case-sensitive, as AFNI is.

Checked against real data: a 256³ × 3 byte `.BRIK.gz` built from the sumaru
subject T1 (not committed) matched `3dmaskdump`/`3dBrickStat`. Reading one
sub-brick took 33 ms, all three 111 ms.

## Phase 2 — Geometry ✅

Brief task 2. New `geometry` module, plus methods on `head::Header`.

- [x] `ijk_to_dicom_cardinal()` (a port of `THD_daxes_to_mat44`), `ijk_to_dicom_real()`, `ijk_to_dicom()` (real when present, otherwise cardinal; this is `3dinfo -aform_real`), `ijk_to_ras()`
- [x] Confirmed the brief's sign question: AFNI maps axes to DICOM with a pure permutation (`THD_set_daxes_to_dicomm`), and the signs live in the signed `DELTA`/`ORIGIN`. The LPI fixture's cardinal matrix equals AFNI's real matrix
- [x] `obliquity()` / `is_oblique()`, a port of `THD_compute_oblique_angle` (0.01° threshold)
- [x] `orientations()` / `orientation_string()` (`Orientation` enum), `view()` (`View`: `+orig`/`+acpc`/`+tlrc`)
- [x] `time_axis()` (`TimeAxis`): nt, origin, TR, duration, slice offsets and z-origin/spacing. Milliseconds are converted to seconds; slice offsets are dropped when there are too few (`thd_dsetdblk.c`)
- [x] Helpers: `Mat44`, `dicom_to_ras`, `flip_xy`, `transform_point`, `oblique_angle`, `mat44_from_3x4`
- [x] Tests: every fixture HEAD matches `3dinfo`'s matrix, obliquity, orientation, nv, TR, view and slice timing; HEAD `ijk_to_ras` equals the NIfTI sform from `3dAFNItoNIFTI`, including the oblique one
- [x] Surface coordinate convention pinned down and documented in the `geometry` module (see log). No implicit conversion anywhere
- [x] New fixtures: `slicetimed+orig`, `oblique.nii`, `*.slice_timing.txt`, and `-obliquity` in `*.aform.txt`
- [x] `examples/inspect.rs` prints orientation, view, obliquity, the matrix and the time axis

No breaking API changes; only additions.

## Phase 3 — Stat metadata and the NIfTI AFNI extension ⬜

Brief tasks 3–4.

- [x] `BRICK_LABS` parsing, done in Phase 1 (labels were being glued together, see log)
- [ ] One shared `StatKind` type, filled the same way from HEAD `BRICK_STATAUX`/`STATSYM`, the NIfTI extension, NIML `COLMS_STATSYM` and GIfTI intent codes
- [ ] Converter between `AFNI_atr` elements and `head::Header` (port of `THD_dblkatr_from_niml`)
- [ ] NIfTI extension list; `Nifti::afni_header()` for ecode 4; keep other extensions as raw bytes
- [ ] p-value math stays out of this crate; it goes to `afni-core`

## Phase 4 — BRIK writing, housekeeping, sumaru volume swap ⬜

Brief tasks 5–6.

- [ ] `Brik::write`, with a round-trip test
- [ ] README format table, `examples/inspect.rs` stat info (geometry printing was done in Phase 2)
- [ ] Optional `read_any()` covering both NIfTI and BRIK
- [ ] Move `sumaru/src/volume.rs` onto the crate and remove the `nifti` dependency

## Phase 5 — NIML core parity ⬜

- [ ] Binary NIML writer
- [ ] Variable-length records (`SUMA_NIML_ROI_DATUM`, `TAYLOR_TRACT_DATUM`) in both ASCII and binary
- [x] Attributes with no value, and single-quoted or unquoted values (done in Phase 1)
- [ ] Incremental parsing (`Incomplete` vs. `(elements, consumed)`), replacing sumaru's retry loop
- [ ] Decide whether numeric matrices stay `f64` or become typed columns (this breaks the public API)
- [ ] Settle the naming differences with sumaru (`NumericMatrix`/`NimlNumericMatrix`, `columns`/`column_count`)

## Phase 6 — `.niml.dset` / ROI parity ⬜

- [ ] `ni_timestep`, `FDRCURVE_*`, label tables (`VALUE_LABEL_DTABLE`), parent idcodes, keeping unknown `AFNI_atr` elements on round trip
- [ ] Typed ROI enums: side, drawing type, element kind, brush action
- [ ] Reuse `StatKind` and the `AFNI_atr` converter from Phase 3

## Phase 7 — GIfTI swap in sumaru ⬜

- [ ] Map NIfTI intent codes to `StatKind`; helpers to detect data columns; FDR curves stored in metadata
- [ ] Port sumaru's `io/gifti.rs` and the GIfTI parts of `surface.rs`/`color.rs`; remove `gifti-rs`

## Phase 8 — Remaining formats ⬜

- [ ] Detailed `.spec` handling (path normalisation, states, hemisphere), plus writing
- [ ] Binary FreeSurfer surfaces, MNE `.stc`, `.niml.tract`, `Graph_Bucket`

## Phase 9 — AFNI talk protocol encoding ⬜

- [ ] Behind a `talk` feature: port numbering, message framing, `SUMA_ixyz`/`SUMA_ijk`/crosshair element builders. Sockets stay in sumaru

## Phase 10 — Beyond sumaru's current needs ⬜

- [ ] FreeSurfer annot/curv/label/MGH, `.1D.dset` and `[]{}` selectors
- [ ] PLY, SureFit/1D `.coord`/`.topo`, BYU, BrainVoyager `.srf`, MNI `.obj`, STL; GIfTI `ExternalFileBinary`

---

## Discovery log

Newest first. Record anything about AFNI, SUMA or sumaru behaviour that
affects the design, with the phase it was found in.

- **2026-10-02 · Phase 2.** **Surface coordinates.** SUMA holds surfaces in
  RAI (DICOM). It flips GIfTI x/y on read and on write (`flip_float_triples`
  in `suma_gifti.c`, unless `AFNI_GIFTI_IN_RAI=YES`), and also flips the GIfTI
  `CoordinateSystemTransformMatrix` (`AFF44_LPI_RAI_FLIP`). It flips FreeSurfer
  surfaces from RAS to RAI only in `SUMA_Align_to_VolPar` (`SUMA_VolData.c`),
  i.e. when aligning them to a surface volume, and skips spheres and flat
  patches. That is why our sphere's GIfTI is flipped relative to its `.asc`.
  sumaru does its own RAI→RAS conversion (`afni_rai_to_ras`,
  `surface_uses_lpi_coordinates`); when sumaru moves onto afni-io (Phases
  7–8), check those against these rules, especially the sphere/flat exception.
- **2026-10-02 · Phase 2.** AFNI names an axis by the side it *starts*
  from; nibabel and FreeSurfer name it by the side it points *toward*. So
  AFNI "RAI" is LPS (DICOM, +x Left), and AFNI "LPI" is what nibabel calls
  RAS (+x Right). SUMA's GIfTI code says "LPI" for GIfTI's RAS. Check the
  matrix, not the letters.
- **2026-10-02 · Phase 2.** AFNI keeps both a cardinal matrix (from
  `ORIGIN`/`DELTA`/`ORIENT_SPECIFIC`) and `IJK_TO_DICOM_REAL`. Display uses the
  cardinal grid; `3dinfo -aform_real`, obliquity and NIfTI output use the real
  one. Editing `IJK_TO_DICOM_REAL` alone (as the oblique fixture does) leaves
  the cardinal matrix plumb. A viewer that wants to look like AFNI should use
  the cardinal matrix for slicing and the real one for world coordinates.
- **2026-10-02 · Phase 2.** `3drefit` refuses millisecond TRs ("no longer
  allowed"), and AFNI converts any stored millisecond time axis to seconds on
  load unless `AFNI_ALLOW_MILLISECONDS` is set. Current AFNI writes
  `TAXIS_NUMS[2] = 0`, not 77002, for seconds; anything other than
  77001/77003 means seconds. `TAXIS_NUMS`, `TAXIS_FLOATS` and `SCENE_DATA`
  are padded to 8 entries with -999 / -999999.
- **2026-10-02 · Phase 1.** **Bug fixed:** `Header::brick_labels()` split on
  `~`, but `.HEAD` string decoding had already turned `~` into NUL, so every
  multi-sub-brick dataset returned one glued-together label. It also dropped
  empty labels, which shifted later labels onto the wrong sub-brick. Now it
  follows `thd_initdblk.c`: field *p* is label *p*, and an empty field becomes
  `#p`. Other `~`-separated string attributes (e.g. `BRICK_KEYWORDS`) need the
  same care in Phase 3.
- **2026-10-02 · Phase 1.** SUMA's `SUMA_IS_EMPTY_STR_ATTR` (`suma_datasets.h`)
  treats a NULL attribute or `"~"` as empty, but `""` as a real value. So a
  value-less NIML attribute must be written back as a bare name, never as
  `name=""`. The `afni_io` NIML writer now does this; sumaru's `io/niml.rs`
  should be checked for the same thing.
- **2026-10-02 · Phase 1.** AFNI's NIML header grammar (`niml_header.c`,
  `niml_private.h`) is looser than the crate assumed. A value may be
  `"double"`- or `'single'`-quoted, or bare characters up to whitespace or
  `<>/=`; the `=value` part is optional.
- **2026-10-02 · Phase 1.** AFNI's loader (`thd_loaddblk.c`) byte-swaps short,
  int, float and complex but **not double**, so AFNI itself would misread a
  big-endian double BRIK on a little-endian machine. afni-io swaps doubles
  correctly; this is an AFNI quirk, not something to copy.
- **2026-10-02 · Phase 1.** `THD_extract_float_brick` (`thd_dsetto3D.c`)
  defines AFNI's "float view" of a sub-brick: complex becomes its magnitude,
  RGB becomes luminance, NaN/Inf become 0 (`thd_floatscan`), and the factor
  applies whenever it's non-zero, including negative. Int and double are
  compiled out there (`#if 0`). afni-io matches the complex and factor rules
  and deliberately differs on RGB and NaN. sumaru matched the RGB rule but only
  applied factors > 0.
- **2026-10-02 · Phase 1.** A `BRICK_TYPES` list shorter than `nvals` is legal:
  AFNI repeats the last entry (`THD_init_datablock_brick`). An unrecognised
  `BYTEORDER_STRING` falls back to host order with a warning; it is not an
  error (sumaru bailed). `BRICK_FLOAT_FACS` entries beyond the list mean 0.
- **2026-10-02 · Phase 0.** In `BRICK_LABS`, `3drefit -sublabel` stores a `~`
  inside a label as `*`, because `~` is the separator. Round-trip tests should
  expect `*`.
- **2026-10-02 · Phase 0.** `ConvertDset -prepend_node_index_1D` segfaults on a
  dense `.gii.dset`. Dense GIfTI dsets have no `NODE_INDEX` array; sparse ones
  put it first.
- **2026-10-02 · Phase 0.** Run on a whole mixed-datum dataset (byte|float),
  `3dmaskdump` prints the float sub-brick as zeros; dumping each sub-brick with
  `[n]` gives the right values. Don't trust whole-dataset dumps for mixed
  datums.
- **2026-10-02 · Phase 0.** `ConvertSurface` negates x and y when it writes
  GIfTI, compared with `.asc`, `.1D.coord` and `SurfaceMetrics`, while
  declaring `NIFTI_XFORM_UNKNOWN`. Likely SUMA's internal RAI written out as
  GIfTI RAS. To confirm in Phase 2.
- **2026-10-02 · Phase 0.** `ConvertDset` writes attributes with no value
  (`domain_parent_idcode`, `geometry_parent_idcode`). AFNI's `niml_header.c`
  stores them with a NULL value and sumaru stores `""`; `afni_io::niml` fails
  to parse. Fix scheduled for Phase 1.
- **2026-10-02 · Phase 0.** SUMA programs call
  `tross_username()`/`tross_hostname()` directly and ignore
  `AFNI_HISTORY_NAME`, which only affects AFNI programs (`thd_notes.c`). Never
  edit a `.HEAD` string attribute by hand: its `count` stores the length.
- **2026-10-02 · Phase 0.** `3dcalc` can't write int (2), double (4) or rgba (7)
  datums. Your `~/.afnirc` sets `AFNI_COMPRESSOR=GZIP`, so the fixture scripts
  force `NONE`.
- **2026-10-02 · Phase 0.** sumaru's `volume.rs` reads only `BRICK_TYPES[0]` and
  `BRICK_FLOAT_FACS[0]`, so multi-sub-brick and mixed-datum datasets aren't
  handled there. That's one reason to finish Phase 1 before moving sumaru onto
  the crate.
