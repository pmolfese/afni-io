# afni-io to AFNI/SUMA C API crosswalk

This is a translation guide, not a claim that the Rust and C APIs have the
same shape. AFNI commonly spreads one operation across a constructor, macros,
lazy loading, and a writer. `afni-io` instead returns owned values, uses
`Result` for failures, and validates invariants at API boundaries.

The cross-crate inventory of covered and missing AFNI `thd_*.[ch]` operations
is maintained in
[`afni-core/docs/THD_LIBRARY_COVERAGE.md`](https://github.com/pmolfese/afni-core/blob/main/docs/THD_LIBRARY_COVERAGE.md).

The tables include every public module and every public *callable family*.
Mechanical accessors such as `len`, `is_empty`, `get`, `attributes`, and enum
code/name conversions are grouped with their owning type rather than repeated
one row at a time. Rustdoc remains the exhaustive signature reference.

## How to read the C column

| Mark | Meaning |
|---|---|
| **Direct** | A Rust operation deliberately implements one named AFNI/SUMA C operation. |
| **Workflow** | The Rust call replaces a normal sequence of C calls or macros. |
| **Format** | The behavior comes from a file-format specification or low-level format code, not one AFNI function. |
| **Rust** | Rust-side validation, adaptation, iteration, or convenience with no useful C analogue. |

Names refer to the [official AFNI source tree](https://github.com/afni/afni/tree/master/src).
The source file is included where it is more useful than a broad subsystem
name.

## Start here: common program operations

| Rust | What it does | AFNI/SUMA C equivalent |
|---|---|---|
| `volume::read_any` | Load an AFNI HEAD/BRIK or NIfTI dataset through one API. | **Workflow:** `THD_open_dataset` + `DSET_load`; format selection is performed inside AFNI's dataset opener. |
| `VolumeReader::open` | Open metadata and read scaled frames on demand. | **Workflow:** `THD_open_dataset`, then `DSET_load_one` / brick loading as needed. |
| `VolumeReader::frame`, `Volume::frame` | Return one scalar frame as true `f32` values. | **Direct/workflow:** `THD_extract_float_brick`; AFNI often reaches the same result through `DSET_BRICK_ARRAY` after `DSET_load`. |
| `VolumeReader::voxel_series`, `Volume::voxel_series` | Gather all frames for one voxel. | **Workflow:** loop over `DSET_NVALS` and `DSET_BRICK_ARRAY(dset, iv)[voxel]`, applying each brick factor. |
| `Volume::grid`, `GridSpec` | Describe dimensions and the real `ijk -> RAS` affine. | **Workflow:** `DSET_NX/NY/NZ` plus `DSET_IJK_TO_DICOM_REAL`; RAI-to-RAS conversion is explicit in Rust. |
| `VolumeMask::read`, `VolumeMask::from_nonzero` | Read the first frame and select finite, nonzero voxels. | **Workflow:** the common `THD_open_dataset` / `DSET_load` / nonzero-mask loop used by AFNI programs. This is not `THD_automask`. |
| `VolumeBuilder`, `VolumeWriteSpec`, `VolumeFrameWriter` | Construct and stream an AFNI or NIfTI output. | **Workflow:** `EDIT_empty_copy` or `EDIT_geometry_constructor`, `EDIT_dset_items`, `EDIT_substitute_brick`, then `DSET_write`. |
| `Brik::read` / `Brik::write` | Read or write the native AFNI HEAD/BRIK representation. | **Workflow:** `THD_open_one_dataset` + `DSET_load`; writing corresponds to `THD_write_3dim_dataset` / `DSET_write`. |
| `SurfaceDatasetReader::open` | Read NIML or GIfTI surface data into one validated core dataset. | **Workflow:** SUMA dataset input (`SUMA_LoadDset_s`) followed by its format-specific NIML/GIfTI path. |
| `NimlDataset::read` / `write` | Read or write a SUMA `AFNI_dataset` NIML group. | **Workflow:** `SUMA_LoadDset_s`, `SUMA_NewDsetPointer`, and `SUMA_WriteDset_s` in `suma_datasets.c`. |
| `Spec::read` / `resolve` / `write` | Parse, validate, resolve, or emit a SUMA spec file. | **Direct:** `SUMA_Read_SpecFile`, `SUMA_CheckOnSpecFile`, `SUMA_Write_SpecFile`. |
| `NodeRoi::read_all` / `write_all` | Read or write drawn SUMA ROIs. | **Direct/workflow:** `SUMA_OpenDrawnROI_NIML`, `SUMA_NIMLDrawnROI_to_DrawnROI`, `SUMA_DrawnROI_to_NIMLDrawnROI`. |
| `read_aff12_series` / `write_aff12_series` | Read or write one or more `aff12.1D` transforms. | **Format/workflow:** the 12-number matrices consumed by `THD_read_affine_1D` and programs such as `cat_matvec`, `3dAllineate`, and `3dvolreg`. |

## Volumes: `volume`, `brik`, `head`, and `geometry`

### Format-independent volume API

| Rust callable family | Purpose | AFNI/SUMA C equivalent |
|---|---|---|
| `VolumeReader::{open, open_spec, into_volume, into_dataset}` | Open a dataset name (including an AFNI selector), inspect it, or finish loading it. | **Workflow:** `THD_open_dataset` / `THD_open_one_dataset`, selector handling, `DSET_load`. `into_dataset` also performs the Rust-only core adaptation. |
| `VolumeReader::{dimensions, voxels, nvols, source_nvols, selected_indices}` | Dataset shape and selected-brick metadata. | **Workflow/macros:** `DSET_NX`, `DSET_NY`, `DSET_NZ`, `DSET_NVOX`, `DSET_NVALS`; selection corresponds to AFNI sub-brick selectors. |
| `VolumeReader::{ijk_to_ras, grid, afni_header, nifti_header, nifti_extensions, labels, stats}` | Common geometry and metadata without loading all data. | **Workflow:** `DSET_IJK_TO_DICOM_REAL`, `DSET_BRICK_LABEL`, `DSET_BRICK_STATCODE/PARAM`; NIfTI extension access comes from `thd_niftiread.c`. |
| `VolumeReader::{frame, frame_into, frames}` | Streaming scaled frame access. | **Direct/workflow:** `THD_extract_float_brick` or one-brick load plus scale. |
| `VolumeReader::{voxel_series, voxel_series_ijk}` | Time/sub-brick series at one voxel. | **Workflow:** repeated `DSET_BRICK_ARRAY` access. |
| `read_any`, `read_any_volumes` | Eager format-independent loading, optionally for selected frames. | **Workflow:** `THD_open_dataset` and AFNI's sub-brick selection/loading path. |
| `Volume::{grid, dimensions, voxels, nvols, ijk_to_ras, ijk_to_dicom}` | Common loaded-volume geometry. | **Workflow/macros:** the corresponding `DSET_*` geometry macros; `ijk_to_dicom` is AFNI's native RAI matrix. |
| `Volume::{afni_header, labels, stats, stats_with_origin}` | Decode AFNI metadata from HEAD attributes or a NIfTI extension/intent. | **Workflow:** `THD_nifti_process_afni_ext`, `THD_store_datablock_stataux`, and dataset brick metadata macros. |
| `Volume::{frame_f32, frame, value, voxel_series_into, voxel_series, voxel_series_ijk}` | Loaded scalar access. | **Direct/workflow:** `THD_extract_float_brick`, `DSET_BRICK_ARRAY`, and brick-factor application. |
| `Volume::{write, write_with_options, as_afni, as_nifti}` | Format-preserving output or downcast. | **Workflow:** `DSET_write` for AFNI; `THD_write_nifti` path for NIfTI. Downcasts are **Rust**. |
| `GridSpec::{new, voxels, to_domain, ijk_to_world, world_to_ijk, linear_index, ijk, crop, pad}` | Validated grid math. | **Workflow:** dataset dimension macros and AFNI matrix/vector transforms. Cropping/padding descriptions and core conversion are **Rust**. |
| `GridSpec::compatibility`, `GridCompatibility::{is_compatible, require_compatible}` | Compare dimensions and real affines. | **Workflow:** the grid checks repeated in AFNI programs; there is no single universal C function with exactly these rules. |
| `VolumeMask::{read, from_reader, from_nonzero, new, count, require_grid, for_dataset, to_sample_mask, from_sample_mask}` | File-grid mask and checked bridge to `afni-core`. | Nonzero-mask construction is an AFNI-program **workflow**; domain-checked conversion is **Rust**. |

### Native AFNI HEAD/BRIK

| Rust callable family | Purpose | AFNI/SUMA C equivalent |
|---|---|---|
| `AfniPaths::{is_afni_name, resolve}` | Recognize and resolve `.HEAD`, `.BRIK[.gz]`, and `prefix+view` names. | **Workflow:** AFNI dataset-name parsing in the `THD_open_*dataset` path. |
| `BrikReader::{open, read_sub_brick, read_frame, read_frame_into}` | On-demand native brick loading. | **Workflow:** `THD_open_one_dataset`, `DSET_load_one`; scaled output matches `THD_extract_float_brick`. |
| `Brik::{read, read_sub_bricks}` | Eagerly read all or selected sub-bricks. | **Workflow:** `THD_open_one_dataset`, `DSET_load` / selected brick loading. |
| `Brik::{value, frame_f32}` and `SubBrick::{value, to_f32, scaled_values, copy_scaled_into}` | Apply `BRICK_FLOAT_FACS` and expose scalar values. | **Direct:** `THD_extract_float_brick` (`thd_dsetto3D.c`). Unlike AFNI, RGB(A) is not converted to luminance and nonfinite floats are retained. |
| `SubBrick::from_f32`, `BrikBuilder::{values, sub_brick}` | Choose stored datum and scaling for true float values. | **Workflow:** `EDIT_substitute_brick` plus brick-factor setup; `StoragePolicy::AutoShort` is a Rust policy. |
| `BrikBuilder::{cardinal, affine, like_grid, labels, time_axis, history, build}` | Construct a validated AFNI dataset. | **Workflow:** `EDIT_geometry_constructor`, `EDIT_empty_copy`, and `EDIT_dset_items`. |
| `Brik::{write, write_with_options}` | Publish a HEAD/BRIK pair, optionally compressed. | **Workflow:** `THD_write_3dim_dataset` / `DSET_write`. Transactional no-clobber staging is **Rust**. |
| `BrikType` and `BrickData` code/type/byte/scalar methods | Represent `MRI_TYPE` and stored brick arrays. | **Direct:** `MRI_TYPE` constants and `MRI_*` image storage in `mrilib.h`. |
| `Header::{read, parse, write, to_head_string}` | Parse or serialize the flat `.HEAD` attribute list. | **Format:** AFNI attribute I/O (`thd_atr.c`, `README.attributes`); the public C dataset API usually hides this layer. |
| `Header::{get, ints, floats, string, set, remove}` | Typed attribute access and mutation. | **Workflow:** `THD_find_*_atr`, `THD_set_*_atr`, and `ATR_*` structures. |
| `Header::{history, set_history, append_history}` | Access `HISTORY_NOTE`. | **Workflow:** `tross_Copy_History`, `tross_Append_History`, `tross_Make_History`. |
| `Header::{rank, nvals, dimensions, delta, origin, orientation, orientations, orientation_string, view}` | Decode core dataset geometry attributes. | **Workflow/macros:** `DSET_RANK`, `DSET_NVALS`, `DSET_NX/NY/NZ`, `DSET_DX/DY/DZ`, `DSET_ORIENT`, `DSET_VIEWCODE`. |
| `Header::{ijk_to_dicom_cardinal, ijk_to_dicom_real, ijk_to_dicom, ijk_to_ras}` | Construct or retrieve voxel-to-world matrices. | **Direct/workflow:** `THD_daxes_to_mat44`, `THD_set_daxes_to_dicomm`, `DSET_IJK_TO_DICOM_REAL`; RAS conversion is explicit in Rust. |
| `Header::{obliquity, is_oblique}` and `geometry::oblique_angle` | Measure real-affine obliquity. | **Direct:** `THD_compute_oblique_angle`; threshold is `OBLIQ_ANGLE_THRESH`. |
| `Header::{time_axis, set_time_axis, clear_time_axis}` | Decode or update `TAXIS_*` attributes. | **Workflow:** `DSET_TR`, `DSET_TIMEUNITS`, `DSET_UNMSEC`, and `EDIT_dset_items`. |
| `Header::{brick_types, brick_type_code, brick_float_facs, brick_factor, brick_labels, set_brick_labels}` | Per-sub-brick storage and labels. | **Direct/workflow:** `THD_init_datablock_brick`, `DSET_BRICK_TYPE`, `DSET_BRICK_FACTOR`, `DSET_BRICK_LABEL`. |
| `Header::{brick_stats, set_brick_stats, fdr_curve, mdf_curve}` | Statistic and threshold-curve metadata. | **Direct/workflow:** `THD_store_datablock_stataux`, `THD_fdrcurve_*`, and `thd_initdblk.c`. |
| `Header::{value_label_table, set_value_label_table, from_niml, to_niml}` | Label tables and AFNI NIfTI-extension representation. | **Workflow:** `THD_nimlize_dsetatr`, `THD_nifti_process_afni_ext`, label-table helpers used by `3drefit`. |
| `geometry::{identity, dicom_to_ras, flip_xy, transform_point, mat44_from_3x4}` | Coordinate/matrix helpers. | Matrix application resembles `MAT44_VEC`; DICOM/RAS conversion and checked construction are **Rust** helpers. |
| `Orientation`, `View`, `TimeUnits`, `TimeAxis` methods | Decode AFNI integer codes and timing units. | **Direct:** `ORIENT_*`, `VIEW_*`, and `UNITS_*_TYPE` constants/macros in `3ddata.h`. |

## NIML and surface datasets

| Rust callable family | Purpose | AFNI/SUMA C equivalent |
|---|---|---|
| `niml::{parse, parse_str, read}` / `parse_bytes` | Parse complete NIML elements. | **Direct/workflow:** `NI_read_element`, `NI_read_element_fromstring`, and parsing code in `niml_elemio.c` / `niml_header.c`. |
| `niml::parse_stream` | Consume only complete elements from an arriving stream. | **Workflow:** AFNI/SUMA's incremental NI stream receive loop. |
| `niml::{serialize, serialize_binary, write, write_binary}` | Encode ASCII or little-endian binary NIML. | **Direct/workflow:** `NI_write_element` and `NI_stream_writecheck`. |
| `NimlElement::{group, text, numeric, child, children}` | Build and inspect an element tree. | **Workflow:** `NI_new_group_element`, `NI_new_data_element`, `NI_add_to_group`, `NI_search_group`. |
| `NimlValueType::{from_name, canonical_name, data_type, from_data_type, record_fields, byte_width}` | Decode `ni_type`, including SUMA ROI and tract records. | **Direct/workflow:** `NI_rowtype_define`, `NI_rowtype_name`, and `NI_*` type constants. |
| `expand_ni_type`, `ni_type_string` | Expand/compact forms such as `int,3*float`. | **Format:** NIML `ni_type` grammar in `niml_types.c`. |
| `NumericMatrix`, `RecordTable`, `MixedTable` constructors/accessors | Validated representations of NIML bodies. | **Rust** ownership/validation around `NI_element` data. |
| `NimlDataset::{read, from_bytes, from_element, to_element, to_niml_string, write, write_binary}` | File/tree conversion for `AFNI_dataset`. | **Workflow:** `SUMA_LoadDset_s`, `SUMA_NewDsetPointer`, `SUMA_WriteDset_s`; layout logic is in `suma_datasets.c`. |
| `NimlDataset` column/index/metadata methods | Labels, types, stats, ranges, index lists, time step, and label tables. | **Workflow:** `SUMA_FindDsetAttribute`, `SUMA_GetDsetCol*`, `SUMA_AddDsetNelCol`, and `AFNI_atr` helpers. |
| `SurfaceDatasetReader::{open, open_with_options, dataset, into_dataset, format, niml_extras, niml_envelope, gifti}` | Unified NIML/GIfTI surface-data reading. | **Workflow:** the format dispatch performed around `SUMA_LoadDset_s`; conversion to `afni_core::Dataset` is **Rust**. |
| `LabelTable::{read, find, from_element, get, key_for, to_value_label_dtable, to_afni_labeltable, write}` | Read, query, and emit AFNI/SUMA label-table layouts. | **Workflow:** label-table operations in `suma_datasets.c`, `SUMA_dset_to_Label_Dset`, and the path used by `3drefit -labeltable`. |
| `NodeRoi` and ROI enum conversion methods | Read/write `Node_ROI`, inspect codes, and collect nodes. | **Direct/workflow:** `SUMA_OpenDrawnROI_NIML`, `SUMA_NIMLDrawnROI_to_DrawnROI`, `SUMA_DrawnROI_to_NIMLDrawnROI`; enum codes come from `SUMA_define.h` and `suma_datasets.h`. |

## Other file formats

| Rust callable family | Purpose | AFNI/SUMA C equivalent |
|---|---|---|
| `NiftiReader::{open, read_frame, read_frame_into}`; `Nifti::{read, from_bytes, write, write_with_options}` | Streaming or eager NIfTI I/O. | **Workflow:** `THD_open_nifti`, `THD_nifti_dataset`, and `THD_write_nifti` (`thd_niftiread.c`, `thd_niftiwrite.c`); the binary layout follows the NIfTI standard. |
| `NiftiHeader::{parse, serialize_single_file, ndim, shape, voxel_count, data_type, affine}` | Header coding and geometry. | **Format:** NIfTI-1/2 structures and `nifti_make_orthog_mat44` / qform-sform helpers. |
| `Nifti::{afni_header, set_afni_header}` | Decode/replace ecode-4 AFNI attributes. | **Direct/workflow:** `THD_nifti_process_afni_ext`, `THD_nimlize_dsetatr`. |
| `Gifti::{read, parse, write, to_xml_string, array_with_intent, pointset, triangles, to_surface}` | GIfTI document, data, and mesh I/O. | **Workflow:** `gifti_read_image`, `gifti_write_image` and SUMA's `suma_gifti.c`; the current Rust implementation follows the GIfTI format directly. |
| `DataArray::{element_count, stat, stat_with_origin}` and `gifti::intent` mappings | Array size and NIfTI-intent statistic metadata. | **Format:** GIfTI/NIfTI intent fields; AFNI uses the same intent codes through `NI_STAT_*`. |
| `Surface::{read, parse, from_geometry, to_asc_string, write}` | FreeSurfer/SUMA ASCII triangular surfaces. | **Direct/workflow:** `SUMA_FreeSurfer_Read` and the FreeSurfer ASCII format. |
| `FreeSurferSurface::{new, read, is_triangle_surface, parse, to_bytes, write, to_surface, from_surface, footer_values}` | FreeSurfer binary triangle surfaces. | **Direct:** `SUMA_FreeSurfer_ReadBin_eng`; conversion and footer querying are **Rust**. |
| `Spec` / `SpecSurface` / `ResolvedSurface` methods | Parse, resolve, inspect, create, and write spec files. | **Direct:** `SUMA_Read_SpecFile`, `SUMA_CheckOnSpecFile`, `SUMA_Write_SpecFile`; constructors/accessors are **Rust**. |
| `OneD::{read, parse, from_rows, get, column, to_1d_string, write}` | Plain numeric `.1D` tables. | **Format/workflow:** `mri_read_1D`, `THD_1Ddset_to_mri`, `thd_1Ddset.c`; also comparable to `Read_1D.m`. |
| `XmatFile::{read, looks_like, parse, from_core, to_xmat_string, write}` | AFNI design matrix plus its commented metadata header. | **Workflow:** `3dDeconvolve.c::write_xsave`, `3dREMLfit -matrix`; semantic conversion to `DesignMatrix` is **Rust**. |
| `Aff12File` and `read_aff12_*` / `write_aff12_*` | `aff12.1D` transforms with comment preservation. | **Format/workflow:** `THD_read_affine_1D` and `cat_matvec`; validation/math lives in `afni-core`. |
| `GraphBucket::{read, from_element, to_element, write, write_binary, network_file}` | FATCAT/SUMA `Graph_Bucket` raw files. | **Workflow:** SUMA dataset I/O and `ConvertDset -graphize`; there is no single graph C object with the same owned shape. |
| `TractNetwork::{read, from_element, to_element, write, write_binary}` | FATCAT `.niml.tract` raw files. | **Format/workflow:** `TrackIO.c` / `TrackIO.h` and NIML element I/O. |
| `Stc::{read, parse, time, to_bytes, write, to_dataset}` | MNE-C `.stc` source estimates. | **No AFNI equivalent.** AFNI has no `.stc` reader; `to_dataset` is **Rust**. |

## Core adapters and low-level support

| Rust callable family | Purpose | AFNI/SUMA C equivalent |
|---|---|---|
| `adapt::{niml_to_core, gifti_to_core, volume_to_core, volume_to_dataset, volume_metadata}` and `_with` variants | Convert raw file models into validated `afni-core` models while applying an explicit metadata policy. | **Rust.** AFNI uses mutable `THD_3dim_dataset` / `SUMA_DSET` structures rather than a file-neutral semantic layer. |
| `NimlEnvelope`, `VolumeDataset`, `VolumeEnvelope`, `RoiEnvelope` methods | Preserve format-specific metadata around a core object and derive outputs safely. | **Rust.** Closest C pattern is `EDIT_empty_copy` followed by selective metadata editing. |
| `VolumeDataset::slice_time_correct` | Apply any public `3dTshift` slice-time interpolation mode using loaded TR/offset metadata, then clear consumed offsets and record the common time origin. | **Direct workflow:** `THD_dataset_tshift` and the `3dTshift` timing-metadata update. |
| `label_table_*`, `roi_*`, `graph_*`, `tracts_*`, `surface_to_core`, `onedee_to_core` | Convert raw I/O types to/from core semantic types. | **Rust** adapters; the C code generally uses one mutable representation throughout. |
| `VolumeBuilder`, `VolumeWriteSpec`, `VolumeFrameWriter` | Whole-buffer and streaming output construction. | **Workflow:** AFNI `EDIT_*` calls plus `DSET_write`; the typestate-like validation is **Rust**. |
| `array::{decode_binary, decode_ascii}`, `DataType`, `TypedArray` methods | Shared NIfTI/GIfTI scalar storage coding. | **Format:** NIfTI datatype constants and GIfTI/NIfTI conversion helpers. |
| `stat::parse_statsym_list`, `ThresholdCurve::{from_values, to_values}` | Decode `BRICK_STATSYM` / `COLMS_STATSYM` and curve attributes. | **Direct:** `NI_stat_decode`, `NI_stat_encode`, `THD_fdrcurve_*`. |
| `selector::{DatasetSpec, SubBrickSelector}` parse/resolve methods | Parse dataset names and ordered selectors such as `[0,2..10(2),$]`. | **Workflow:** AFNI dataset-name and sub-brick selector parsing used by `THD_open_dataset`. |
| `Error::{parse, missing, invalid, unsupported}` | Structured failure construction. | **Rust.** AFNI C typically returns null/status or uses `ERROR_exit`. |

## Names worth reviewing before 1.0

These are review prompts, not rename decisions.

| Current name | Why it may slow an AFNI C reader down | Candidate direction |
|---|---|---|
| `Brik` | The type is the entire HEAD/BRIK dataset, while AFNI uses “brick” for one stored volume too. | `AfniDataset` or `HeadBrikDataset`; keep `SubBrick` for one brick. |
| `Header` | Unqualified beside NIfTI/GIfTI headers and easy to import ambiguously. | `AfniHeader`. |
| `NimlDataset` | It is specifically a SUMA `AFNI_dataset`, not any dataset encoded in NIML. | `SumaDataset` or `NimlSurfaceDataset`. |
| `Surface` | It specifically models the FreeSurfer/SUMA ASCII surface file, while other mesh representations exist. | `AsciiSurface` or make a clearly documented canonical raw mesh name. |
| `OneD` | Names the suffix but not the matrix/table concept. | `OneDTable` or `OneDMatrix`. |
| `read_any` / `read_any_volumes` | “Any” hides the returned abstraction, and “volumes” competes with “frames” and AFNI “sub-bricks.” | `Volume::read` / `Volume::read_frames`, or `read_volume` / `read_volume_frames`. |
| `frame`, `volume`, `sub_brick`, `nvals` | Four words currently refer to closely related fourth-dimension entries at different layers. | Document and enforce: **frame** at the format-neutral layer, **sub-brick** only for native AFNI storage, `frame_count` in new APIs. |
| `to_core`, `to_dataset`, `to_envelope` | The destination type is sometimes not obvious at a call site. | Prefer `into_core_dataset`, `to_core_dataset`, or constructors on the destination type. |
| `from_element` / `to_element` | Correct but context-free in modules with multiple NIML layouts. | Keep when type-qualified; otherwise consider `from_niml_element` / `to_niml_element`. |
| `parse`, `from_bytes`, `read` | The split is mostly consistent but not universal across formats. | Reserve `parse` for borrowed in-memory syntax, `from_bytes` for owned binary decoding only if needed, and `read` for paths. |

## Maintenance rule

When a public callable is added or renamed, update the relevant family above.
When its behavior deliberately follows a particular AFNI/SUMA C function,
also put that symbol in the Rustdoc for the callable itself. If the match is
only a workflow or file-format correspondence, say so; a false one-to-one
mapping is worse than no mapping.
