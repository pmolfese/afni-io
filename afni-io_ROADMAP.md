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
| 3 | Stat metadata and the NIfTI AFNI extension | ✅ |
| 4 | BRIK writing, `read_any`, housekeeping | ✅ |
| 5 | NIML core parity | ✅ |
| 6 | `.niml.dset` / ROI parity | ⬜ |
| 7 | GIfTI swap in sumaru | ⬜ |
| 8 | Remaining formats | ⬜ |
| 9 | AFNI talk protocol encoding | ⬜ |
| 10 | Beyond sumaru's current needs | ⬜ |
| — | Moving sumaru onto afni-io (you, separately) | ⬜ see checklist |

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

## Phase 3 — Stat metadata and the NIfTI AFNI extension ✅

Brief tasks 3–4.

- [x] `BRICK_LABS` parsing (done in Phase 1)
- [x] New `stat` module: `StatKind` (the 23 AFNI stat codes, which are NIfTI intent codes 2–24), `StatSpec { kind, params }`, `parse_statsym_list`, and `ThresholdCurve` for `FDRCURVE_*`/`MDFCURVE_*`. Parsing ports `NI_stat_decode`, formatting ports `NI_stat_encode`/`NI_fval_to_char`, so output matches AFNI's text exactly
- [x] One type for all four sources: `Header::brick_stats()` (STATSYM, else STATAUX), the NIfTI AFNI extension (same `Header`), NIML `COLMS_STATSYM` (`parse_statsym_list`), and GIfTI (`DataArray::stat()`, from `Intent` + `intent_p1..3`). Tests check `Ttest(10);Ftest(2,30)` comes out identical from `.niml.dset` and `.gii.dset`
- [x] `Header::from_niml` / `Header::to_niml`: a port of `THD_dblkatr_from_niml`, plus the reverse
- [x] `Nifti::extensions` (raw `(code, bytes)`, unknown ones kept), read for single-file and `.hdr`, and written back (esize padded to 16, `vox_offset` updated). `Nifti::afni_header()` and `Nifti::set_afni_header()`
- [x] Test: every attribute in `stat.nii`'s extension equals `stat+orig.HEAD`, except the ID code `3dAFNItoNIFTI` regenerates
- [x] Checked against AFNI: a `.nii` whose extension was edited by afni-io reads in `3dinfo`/`3dAttribute` with the new labels and statcodes (`fizt`, `fitt` dof 5.5)
- [x] p-value math kept out (it goes to `afni-core`)
- [x] `examples/inspect.rs` prints per-sub-brick labels and statistics for `.HEAD` and for a NIfTI's AFNI extension

Fixed along the way (NIML strings; needed for the extension, pulled forward from Phase 5):
- [x] Reading: a `String` body is split into strings *before* entities are decoded, so `&quot;` no longer ends a string early. A single string has its quotes removed; several become a one-column table
- [x] Writing: `String`/`CString` bodies are now quoted. Without quotes AFNI reads only up to the first blank (confirmed with `3dinfo -label`)
- [x] Entity decoding is a single pass and handles `&#ddd;` / `&#xhh;`, as `unescape_inplace` does

Public API changes:
- `Nifti` has a new `extensions` field.
- `NimlData::Text` for a `String` element no longer includes its surrounding quotes.
- A `String` element with several strings parses as `NimlData::Mixed`.

## Phase 4 — BRIK writing, `read_any`, housekeeping ✅

Brief tasks 5–6. Moving sumaru onto the crate is listed separately below
(you're doing it outside this work).

- [x] `Brik::write(path)`: writes `.HEAD` + `.BRIK`, or `.BRIK.gz` when the path says so. Accepts any AFNI name, or a bare prefix (the view comes from the header). Updates `DATASET_RANK`, `DATASET_DIMENSIONS`, `BRICK_TYPES`, `BRICK_FLOAT_FACS`, `BRICK_STATS`, `BYTEORDER_STRING` (always LSB_FIRST), the view in `SCENE_DATA`, and a fresh `IDCODE_STRING`/`IDCODE_DATE`. Refuses to write if a sub-brick isn't loaded or has the wrong size, or if the other BRIK form (`.BRIK` vs `.BRIK.gz`) exists and would be stale
- [x] `Brik::new(dims, orientation, origin, delta, sub_bricks)`: a new bucket dataset on a cardinal grid
- [x] `SubBrick::stats()` (AFNI's `BRICK_STATS`: scaled range, magnitude for complex, luminance for RGB); `BrickData::to_le_bytes()`
- [x] Round-trip test: every fixture, plain and gzipped, gives identical voxels and identical attributes apart from the refreshed ones; our `BRICK_STATS` match AFNI's
- [x] **AFNI reads what we write:** `3dmaskdump` of rewritten big-endian, mixed, stat, scaled+gz, oblique and slice-timed datasets is byte-identical to the originals' dumps. `3dinfo` reports the same datum, orientation, obliquity, TR, view, labels and statcodes. A `Brik::new` LPI dataset written as `+tlrc` shows the expected matrix and view
- [x] `volume::read_any` / `read_any_volumes` → `Volume` (AFNI or NIfTI): `dimensions`, `nvols`, `ijk_to_ras`/`ijk_to_dicom`, `frame_f32(t)`, `value`, `labels`, `stats` (AFNI attributes first, else the NIfTI intent for every volume, as AFNI does), `afni_header`
- [x] Test: the AFNI and NIfTI versions of each fixture give the same grid, matrix, every volume's values, labels and stats. On real data (sumaru's `sub-3_SurfVol.nii`, `T1.nii.gz`, the 256³×3 BRIK.gz) `read_any` matches AFNI's `3dinfo`/`3dBrickStat`/`3dmaskdump`
- [x] README format table (BRIK write, NIfTI extensions, `volume`), crate docs, `examples/inspect.rs` (stats were done in Phase 3)
- [x] Fixed: `.HEAD` string `count` (see log) and float precision in every writer (see log)

Public API changes:
- `format_float` output changed: shortest exact form, e.g. `0.3`, no longer padded to 10 decimals.
- `AttributeValue::count()` includes the terminating NUL for strings.

## sumaru migration checklist (you're doing this separately) ⬜

What sumaru can now replace, and what to keep in sumaru's adapters:

- [ ] `src/volume.rs` → `afni_io::volume::read_any`. Map `Volume::frame_f32(0)` → `Volume.data`, `ijk_to_ras()` → `VolumeSpace` (as `f32`), and `AfniPaths::is_afni_name` for the "is this AFNI?" check. Keep in sumaru: NaN/Inf → 0 and RGB → luminance (`0.299 R + 0.587 G + 0.114 B`). afni-io returns raw values and `None` for RGB on purpose
- [ ] sumaru only read `BRICK_TYPES[0]`/`BRICK_FLOAT_FACS[0]`. With afni-io, mixed-datum datasets and per-sub-brick factors work, and negative factors apply (sumaru ignored them)
- [ ] Remove the `nifti` dependency (pulls in nalgebra/ndarray). Check `flate2` is still needed (`niml_debug.rs`)
- [ ] NIML (after Phase 6): sumaru → afni-io names are `NimlNumericMatrix` → `NumericMatrix` (typed columns; `column_count`, `get`), `NimlMixedTable` → `MixedTable`, `NimlData::RoiDatums`/`TractDatums` → `NimlData::Records`, `parse_niml_bytes` → `niml::parse_bytes`, `serialize_niml_ascii` → `niml::serialize`, `serialize_niml_binary` → `niml::serialize_binary`, `expand_niml_type` → `niml::expand_ni_type`. The talk reader's parse-and-retry loop and `expected_binary_niml_message_len` (`afni.rs`) can become `niml::parse_stream`
- [ ] sumaru's NIML reader treats `ni_form="binary"` as host byte order; AFNI means big-endian (see Phase 5 log)
- [ ] Later phases: GIfTI (Phase 7, drops `gifti-rs`), dset/ROI (Phase 6), spec/FreeSurfer/STC/tract/graph (Phase 8), talk protocol (Phase 9)

## Phase 5 — NIML core parity ✅

- [x] **Typed columns (decision: yes, breaking).** `NumericMatrix { rows, columns: Vec<TypedArray> }`, so each column keeps its declared type. A `float` dataset takes half the memory: a 163,842-node × 300-column float dset is 197 MB instead of 393 MB. `new(types, rows, row-major f64)` is kept as a convenience constructor; `from_columns`, `column`, `column_types`, `get` added
- [x] **Variable-length records, generic:** `NimlData::Records(RecordTable)`, driven by AFNI's rowtype definitions (`NimlValueType::record_fields`): `SUMA_NIML_ROI_DATUM` = `int,int,int,int[#3]`, `TAYLOR_TRACT_DATUM` = `int,int,float[#2]`. ASCII and binary, read and write. Writers set each length field from its array, so callers can't get it wrong. `roi.rs` now uses it
- [x] **Binary writer:** `niml::serialize_binary` / `niml::write_binary` (`binary.lsbfirst` for numeric and record bodies; groups, text and mixed stay ASCII in the same stream, which AFNI allows)
- [x] **Incremental parsing:** `niml::parse_stream(bytes) -> (elements, consumed)`. Partial elements are left for later; malformed input is an error; processing instructions are skipped. Tested byte by byte and in chunks against a one-shot parse of a mixed ASCII/binary stream
- [x] **Naming:** `column_count()` (sumaru's name) replaces `columns()` on `NumericMatrix` and `MixedTable`. The full sumaru → afni-io name map is in the migration checklist
- [x] `ni_type` written the way AFNI writes it, with repeats grouped (`3*float`, not `float,float,float`)
- [x] `base64` bodies (`ni_form="base64.*"`) are read
- [x] Fixed: AFNI's byte-order rule (bare `binary` is big-endian; see log) and a stale-`ni_form` bug in the writer (see log)
- [x] **Checked against AFNI:** binary dsets written by afni-io read in `ConvertDset` identically to AFNI's own dumps (dense and sparse), keep their stat codes in `3dinfo`, and binary ROI files convert through `ROI2dataset` row-for-row identically to the originals
- [x] Test: every committed NIML fixture (AFNI dsets, SUMA/sumaru ROIs, label tables, the cluster dset) round-trips through binary and back through ASCII

Public API changes:
- `NumericMatrix` fields are now `rows` and `columns`; `column_types()` is a method; `values` is gone (use `get` / `column`).
- `NumericMatrix::columns()` and `MixedTable::columns()` are now `column_count()`.
- `NimlData::Records` is new; ROI bodies are `Records`, no longer `Text`.
- `NimlValueType::TaylorTractDatum` is new.
- `ni_type_string` groups repeats.

## Phase 6 — `.niml.dset` / ROI parity ⬜

- [ ] `ni_timestep`, `FDRCURVE_*`, label tables (`VALUE_LABEL_DTABLE`), parent idcodes, keeping unknown `AFNI_atr` elements on round trip
- [ ] Typed ROI enums: side, drawing type, element kind, brush action
- [ ] Reuse `StatSpec` and `Header::from_niml` from Phase 3. `NimlDataset` should expose `stats()` and keep its `AFNI_atr`s as a `Header`, so unknown attributes round-trip
- [ ] `split_semicolons` in `dset.rs` drops empty entries, so `COLMS_LABS`/`COLMS_STATSYM` can shift columns, the same bug `brick_labels` had (see log)

## Phase 7 — GIfTI swap in sumaru ⬜

- [x] Map NIfTI intent codes to `StatKind` (`DataArray::stat`, Phase 3)
- [ ] Helpers to detect data columns; FDR curves stored in metadata
- [ ] Port sumaru's `io/gifti.rs` and the GIfTI parts of `surface.rs`/`color.rs`; remove `gifti-rs`

## Phase 8 — Remaining formats ⬜

- [ ] Detailed `.spec` handling (path normalisation, states, hemisphere), plus writing
- [ ] Binary FreeSurfer surfaces, MNE `.stc`, `.niml.tract`, `Graph_Bucket`

## Phase 9 — AFNI talk protocol encoding ⬜

- [ ] Behind a `talk` feature: port numbering, message framing, `SUMA_ixyz`/`SUMA_ijk`/crosshair element builders. Sockets stay in sumaru

## Phase 10 — Beyond sumaru's current needs ⬜

- [ ] FreeSurfer annot/curv/label/MGH, `.1D.dset` and `[]{}` selectors
- [ ] PLY, SureFit/1D `.coord`/`.topo`, BYU, BrainVoyager `.srf`, MNI `.obj`, STL; GIfTI `ExternalFileBinary`
- [ ] NIfTI complex and RGB datatypes (see the Phase 4 log); streaming / single-volume NIfTI reads

---

## Discovery log

Newest first. Record anything about AFNI, SUMA or sumaru behaviour that
affects the design, with the phase it was found in.

- **2026-10-02 · Phase 5.** **Bug fixed, byte order:** AFNI decides a binary
  body's byte order by substring and **defaults to big-endian** unless
  `ni_form` contains `lsb` (`niml_elemio.c`: `order=NI_MSB_FIRST`). afni-io
  (and sumaru) read a bare `ni_form="binary"` as the host's order, which is
  little-endian on Intel and Apple Silicon, so the opposite of AFNI. AFNI itself always
  writes `binary.lsbfirst`/`.msbfirst`, so this only bites on hand-made or
  third-party files.
- **2026-10-02 · Phase 5.** **Bug fixed, writer:** serialising an element that
  had been parsed from binary kept its `ni_form="binary.lsbfirst"` attribute
  but wrote an ASCII body, so the output could not be read back. The writer
  now drops any incoming `ni_form` and sets its own.
- **2026-10-02 · Phase 5.** AFNI's variable-length rowtypes are defined at run
  time with `NI_rowtype_define`: `SUMA_NIML_ROI_DATUM` = `int,int,int,int[#3]`
  (`SUMA_niml.c`) and `TAYLOR_TRACT_DATUM` = `int,int,float[#2]`
  (`ptaylor/TrackIO.h`). `type[#k]` means "an array whose length is field k
  (1-based)". A file only names the rowtype in `ni_type`, so a reader must
  already know the definitions. Binary bodies have no length prefix: the
  only way to find where a record body ends is to walk the records.
- **2026-10-02 · Phase 5.** AFNI writes grouped `ni_type`s (`3*float`,
  `2*String`); our writer used to spell them out. Equivalent, but AFNI's form
  keeps headers short for wide datasets.
- **2026-10-02 · Phase 5.** `parse_stream` can't tell an element whose closing
  tag never comes from one still arriving (`<a>text</b>` might be followed by
  `</a>`). Talk readers must cap their buffer.
- **2026-10-02 · Phase 5.** `ni_form="base64.*"` exists (AFNI's `nisurf`
  writes it). It is read here; nothing writes it, since AFNI programs use
  binary. AFNI's `ROI2dataset` reads binary `Node_ROI` files fine, though SUMA
  always writes ROIs as ASCII.
- **2026-10-02 · Phase 4.** **Bug fixed, every writer:** floats were written
  with a fixed 10 decimals. A scale factor such as 3.0517578e-05 (1/32768,
  common for scaled shorts) became 0.0000305176, rescaling every voxel after
  a read/write, and values below 1e-10 (small p-values) became 0. Now each
  value is written as the shortest decimal that reads back to the same number:
  `f32`'s shortest form for values that are exact `f32`s (all AFNI floats),
  `f64`'s otherwise. This affects `.HEAD`, NIML, GIfTI ASCII, `.1D`, `.asc` and
  ROI colours.
- **2026-10-02 · Phase 4.** **Bug fixed:** `.HEAD` string attributes were
  written with an extra `~` that `count` didn't include (`'abc~~` with
  count 4). A string set in code without a trailing NUL got no terminator at
  all. Now, as AFNI's `THD_write_atr` does, the terminating NUL is always
  counted and written as `~`.
- **2026-10-02 · Phase 4.** AFNI's `BRICK_STATS` for RGB is the luminance
  range (`0 194.02` for the RGB fixture), and for scaled shorts the scaled
  range (AFNI prints ~7 significant digits: 67.87499).
- **2026-10-02 · Phase 4.** AFNI's NIfTI reader prefers the sform over the
  qform by default (`form_priority = 'S'`, overridable with
  `AFNI_NIFTI_PRIORITY`), the same as `NiftiHeader::affine()`. It applies a
  statistical `intent_code` to every sub-brick, but not when `dim[5] > 1`
  (per-voxel parameters), and the AFNI extension then overrides it
  (`thd_niftiread.c`). It only understands stat codes up to 10 from the
  intent.
- **2026-10-02 · Phase 4.** **Gap:** the NIfTI reader has no complex (32,
  1792, 2048) or RGB (128, 2304) datatypes; `TypedArray` covers 8–64-bit ints
  and floats only. sumaru's `nifti`-crate path (`into_ndarray::<f32>`) can't
  read them either, so this isn't a regression. Added to Phase 10.
- **2026-10-02 · Phase 3.** **sumaru bug, fixed in sumaru.** sumaru's NIML
  writer (`src/io/niml.rs`) wrote `String` bodies unquoted, so AFNI read only
  the first word. `3dNotes` showed sumaru's cluster history as just
  `SurfClust`, and multi-word column labels were cut at the first blank,
  pushing later columns to `#n`. Fixed in sumaru: `String`/`CString` bodies
  are quoted on write, quoted bodies are split before entities are decoded,
  and `&#ddd;`/`&#xhh;` are decoded. Checked with `3dinfo -label` and
  `3dNotes` on sumaru output. Both readers (sumaru's and afni-io's) still read
  an **unquoted** body as one whole string, so older sumaru files keep their
  full history (strict AFNI parsing would keep only the first word). The
  `real/dset` sumaru cluster fixture now tests this in afni-io.
- **2026-10-02 · Phase 3.** **sumaru quirk, not fixed:**
  `strip_niml_comment_prefixes` (`sumaru/src/io/niml.rs`) removes a leading
  `# ` from *every line* of a NIML file before parsing, so it can read SUMA's
  comment-wrapped `.niml.roi` files. It also alters string content: the
  `# note: …` line in sumaru's cluster history comes back as `note: …`.
  *(Corrected in Phase 5: afni-io has a similar pass, `strip_comment_prefixes`
  in `niml::parse`, but it runs only when a line starts with `# <`, i.e. on
  comment-wrapped files. It still edits the whole file when it does run.)*
  Resolve this when sumaru's NIML reading moves onto afni-io.
- **2026-10-02 · Phase 3.** AFNI stat codes **are** NIfTI intent codes (2 =
  Correl … 24 = Log10Pval; `niml_stat.c`). But AFNI's `.HEAD` loader keeps
  only the classic codes 2–10 (`FUNC_IS_STAT`) and ignores e.g. `Normal`; the
  crate keeps all 23. When both exist, `BRICK_STATSYM` wins over
  `BRICK_STATAUX`. Missing parameters are filled with **1.0** from STATSYM
  (`NI_stat_decode`) but **0.0** from STATAUX (`THD_store_datablock_stataux`).
- **2026-10-02 · Phase 3.** The NIfTI AFNI extension holds the *entire*
  `.HEAD` attribute set: every attribute in `stat.nii` matches
  `stat+orig.HEAD`, including `HISTORY_NOTE`, except `IDCODE_STRING`, which
  `3dAFNItoNIFTI` regenerates and also puts in the group's `self_idcode`.
  AFNI ignores an extension of 32 bytes or less, or one not starting with
  `<?xml`. A NIfTI with several sub-bricks gets `intent_code = 0`; the stats
  live only in the extension, which is why `-pure` NIfTIs lose them.
- **2026-10-02 · Phase 3.** In the extension, string attributes are joined
  and `~` is turned into NUL (`THD_unzblock`; `ZBLOCK` = `~` = 126), the same
  encoding as `.HEAD`. So one `Header` type covers both. `THD_set_string_atr`
  stores a terminating NUL, which `Header::string()` trims.
- **2026-10-02 · Phase 3.** AFNI's GIfTI datasets carry each column's
  statistic as the DataArray `Intent` plus `intent_p1..3` **metadata**
  entries, not attributes; the reference ISC GIfTI has six `Ttest(48)`
  columns.
- **2026-10-02 · Phase 3.** `NimlDataset`'s `split_semicolons` (and sumaru's)
  filters out empty entries. A dataset with an empty label in the middle of
  `COLMS_LABS` would shift later labels onto the wrong columns. Fix in
  Phase 6.
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
