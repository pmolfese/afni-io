# Test fixtures

Everything here is small enough to commit (about 1.4 MB total) and contains no
subject anatomy, usernames, hostnames or study paths. Shared helpers for using
it live in `tests/common/mod.rs`.

| Directory | What | Origin |
|-----------|------|--------|
| `volume/` | Tiny 4×5×6 AFNI and NIfTI volumes, plus AFNI's own reference dumps | `make_volume_fixtures.sh` |
| `surface/` | A 42-node icosahedron in several formats, and surface datasets | `make_surface_fixtures.sh` |
| `real/` | Real AFNI/SUMA/sumaru output, copied from `sumaru/testing/` | see below |

Both scripts were last run with **AFNI_26.2.08** (macOS ARM). They need AFNI on
`PATH`, are idempotent, and remove their previous output first:

```sh
tests/data/make_volume_fixtures.sh
tests/data/make_surface_fixtures.sh
```

## `volume/`

Voxel values are formulas over `(i, j, k, t)`, not random, so a test can work
out the expected value of any voxel. The header of `make_volume_fixtures.sh`
lists the formulas.

| Dataset | Covers |
|---------|--------|
| `s16`, `u8`, `f32` | short / byte / float datums |
| `s16scaled` | short with a `BRICK_FLOAT_FACS` scale factor |
| `mixed` | byte and float sub-bricks in the same dataset |
| `rgb` | datum code 6 (no `.dump.txt`, because `3dmaskdump` can't print RGB) |
| `s16_msb` | big-endian (`MSB_FIRST`) |
| `s16gz` | `.BRIK.gz` |
| `stat` | `BRICK_STATAUX`/`BRICK_STATSYM` (`Ttest(23)`, `Ftest(2,40)`). The label was given as `Fstat~with~tildes`, but AFNI stores `~` (the `BRICK_LABS` separator) as `*` |
| `timeseries` | 3D+time with TR = 2 s |
| `slicetimed` | TR = 2.5 s with six slice offsets (`TAXIS_OFFSETS`). `3drefit` no longer accepts millisecond TRs, so that case is a unit test |
| `lpi` | LPI orientation, non-zero origin, voxel sizes 1.5 × 2 × 2.5 mm |
| `u8tlrc+tlrc` | the tlrc view |
| `oblique` | `IJK_TO_DICOM_REAL` set to a 10° rotation about z |
| `stat.nii` | NIfTI **with** the AFNI ecode-4 extension |
| `stat_pure.nii` | the same data **without** the extension (`-pure`) |
| `s16.nii.gz`, `lpi.nii`, `oblique.nii` | gzipped NIfTI, NIfTI with a non-RAI orientation, and an oblique NIfTI |

The reference files for each dataset:

- `*.3dinfo.txt`: `3dinfo -verb`.
- `*.aform.txt`: `3dinfo -aform_real -is_oblique -obliquity -datum -orient -nv -tr -av_space`.
  The matrix is in AFNI's DICOM (RAI) convention. `-tr` prints 0 when there
  is no time axis.
- `*.slice_timing.txt`: `3dinfo -slice_timing`, which prints one zero per
  slice when the dataset has no slice timing.
- `*.dump.txt`: one line per voxel, `i j k v0 v1 …`, with values scaled to their
  true values.

Not covered here: datum codes 2 (int), 4 (double) and 7 (rgba). `3dcalc` can't
write them, so Phase 1 builds them inside the Rust tests.

## `surface/`

All of it comes from `CreateIcosahedron -ld 2` (42 nodes, 80 triangles).

- **Surfaces:** `ico.asc`, `ico_{ascii,b64,b64gz}.gii`, `ico.ply`,
  `ico.1D.coord`/`.topo`, and `ico_mirror.asc` (the same shape with x and y
  negated).
- **Specs:** `ico.spec`, written by `CreateIcosahedron`, and `ico_states.spec`,
  two states from `quickspec`, one non-anatomical with a `LocalDomainParent`.
- **Datasets:** `dense_*` (42 rows), `sparse_*` (11 rows, nodes 0, 4, …, 40) and
  `stat.niml.dset` (`Ttest(10)`, `Ftest(2,30)`). Each is written as NIML ASCII
  (`_asc`), NIML binary (`_bi`) and GIfTI ASCII/B64/B64GZ.
- **References:** `*.dump.txt` holds `ConvertDset -o_1D_stdout` output with the
  node index as the first column. `ico_ref.coord.1D.dset` holds `SurfaceMetrics -coords`.

Things the AFNI tools do that matter for tests:

- `ConvertSurface` **negates x and y when it writes GIfTI**. SUMA holds
  surfaces in RAI and always writes GIfTI as RAS (`suma_gifti.c`), but it never
  converted this sphere from RAS when reading it (`SUMA_Align_to_VolPar` skips
  spheres). So `.asc`, `.1D.coord` and `SurfaceMetrics` agree with each other,
  and the GIfTI is flipped relative to them. See
  `surface_encodings_agree_with_1d_coords` and the `geometry` module docs.
- `ConvertDset` writes valueless attributes (`domain_parent_idcode` with no
  `=`), which AFNI's own NIML parser accepts.
- A dense GIfTI dset has no `NODE_INDEX` array, and
  `ConvertDset -prepend_node_index_1D` segfaults on it. The script falls back to
  numbering the rows.
- SUMA programs ignore `AFNI_HISTORY_NAME`, so the script rewrites `[user@host:`
  in `.spec`/`.dset` history strings afterwards. Never do this to a `.HEAD`
  file: its string attributes store their length.

## `real/`

Copied from `sumaru/testing/`. Every file was checked for usernames, hostnames
and paths before copying.

| File(s) | Written by |
|---------|-----------|
| `roi/demo.lh.*.niml.roi`, `roi/suma_clickmiddle_joined*.niml.roi` | SUMA |
| `roi/sumapy_lh.white.1.niml.roi` | not recorded (the name suggests a Python writer) |
| `roi/sumaru_rois.niml.roi`, `roi/debug.lh.sumaru_rois.niml.roi`, `roi/test_lh_sumaru_clusters.niml.roi` | sumaru |
| `roi/roi_*.niml.roi`, `roi/test*.niml.roi` | not recorded (SUMA or sumaru) |
| `labels/*.niml.lt` | `@SUMA_Make_Spec_FS` (FreeSurfer aparc label tables) |
| `spec/std.141.sub-3_both.spec` | `@SUMA_Make_Spec_FS` + `MapIcosahedron` |
| `dset/test_lh_full_sumaru_clusters.niml.dset` | sumaru (cluster output) |

Anything written by sumaru shows what sumaru produces. It is **not** a
reference for what AFNI produces.

## Reference fixtures (not committed)

Large or non-public files stay in `sumaru/testing/`:

- the subject surfaces (`rh.white.gii`, `lh.inflated.gii`, `rh.thickness.gii`,
  `fs_lowres_std-*.gii`)
- the `SUMA/` FreeSurfer session
- the ISC datasets
- `afni_niml/` talk recordings
- sumaru's DriveSuma captures

Tests that use them call `common::reference(..)` and are skipped unless you set:

```sh
AFNI_IO_REFERENCE_DIR=../sumaru/testing cargo test
```
