//! Ergonomic dataset-programming APIs used by AFNI-style command-line tools.

mod common;

use std::path::PathBuf;

use afni_io::adapt::volume_to_envelope;
use afni_io::geometry::{TimeAxis, TimeUnits};
use afni_io::prelude::*;

fn scratch(name: &str) -> PathBuf {
    let dir =
        std::env::temp_dir().join(format!("afni_io_programming_{name}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn storage_policies_scale_and_reuse_buffers() {
    let values = vec![-10.0, -1.25, 0.0, 2.5, 9.0];
    let float = SubBrick::from_f32(values.clone(), StoragePolicy::Float).unwrap();
    assert_eq!(float.to_f32().unwrap(), values);

    let short = SubBrick::from_f32(values.clone(), StoragePolicy::AutoShort).unwrap();
    assert_eq!(short.brik_type(), BrikType::Short);
    assert!(short.factor > 0.0);
    let decoded: Vec<f32> = short.scaled_values().unwrap().collect();
    for (actual, expected) in decoded.iter().zip(&values) {
        assert!((actual - expected).abs() <= short.factor);
    }

    let mut reused = vec![f32::NAN; values.len()];
    short.copy_scaled_into(&mut reused).unwrap();
    assert_eq!(reused, decoded);
    assert!(short.copy_scaled_into(&mut reused[..2]).is_err());
}

#[test]
fn builder_handles_oblique_geometry_labels_time_and_history() {
    let affine = [
        [2.0, 0.2, 0.0, -10.0],
        [0.1, 3.0, 0.0, 20.0],
        [0.0, 0.0, 4.0, -30.0],
        [0.0, 0.0, 0.0, 1.0],
    ];
    let axis = TimeAxis {
        nt: 2,
        origin: 0.0,
        step: 2.0,
        duration: 0.0,
        stored_units: TimeUnits::Seconds,
        slice_offsets: Vec::new(),
        slice_z_origin: 0.0,
        slice_dz: 0.0,
    };
    let dataset = BrikBuilder::affine([2, 2, 1], affine)
        .unwrap()
        .values(vec![1.0, 2.0, 3.0, 4.0], StoragePolicy::Float)
        .unwrap()
        .values(vec![5.0, 6.0, 7.0, 8.0], StoragePolicy::AutoShort)
        .unwrap()
        .labels(["first", "second"])
        .time_axis(axis.clone())
        .history("made by test")
        .build()
        .unwrap();

    assert_eq!(dataset.header.ijk_to_dicom().unwrap(), affine);
    assert!(dataset.header.is_oblique());
    assert_eq!(dataset.header.brick_labels(), ["first", "second"]);
    assert_eq!(dataset.header.time_axis(), Some(axis));
    assert_eq!(dataset.header.history(), Some("made by test"));
}

#[test]
fn checked_frames_and_voxel_series_match_direct_access() {
    let brik = Brik::read(common::data("volume/timeseries+orig.HEAD")).unwrap();
    let volume = Volume::Afni(brik.clone());
    let index = 70;
    let expected: Vec<f32> = (0..brik.nvals())
        .map(|p| brik.sub_brick(p).unwrap().value(index).unwrap())
        .collect();

    assert_eq!(brik.voxel_series(index).unwrap(), expected);
    assert_eq!(volume.voxel_series(index).unwrap(), expected);
    assert_eq!(volume.voxel_series_ijk(2, 2, 3).unwrap(), expected);
    let mut reused = vec![0.0; expected.len()];
    volume.voxel_series_into(index, &mut reused).unwrap();
    assert_eq!(reused, expected);
    assert!(volume.frame(volume.nvols()).is_err());
    assert!(volume.voxel_series(volume.voxels()).is_err());
}

#[test]
fn masks_are_grid_checked() {
    let volume = read_any(common::data("volume/u8+orig.HEAD")).unwrap();
    let mask = VolumeMask::from_nonzero(&volume, 0).unwrap();
    assert_eq!(mask.values().len(), volume.voxels());
    assert!(mask.count() > 0);
    mask.require_grid(&volume.grid().unwrap(), 1e-9).unwrap();

    let mut shifted = volume.grid().unwrap();
    shifted.ijk_to_ras[0][3] += 0.5;
    let report = mask.grid().compatibility(&shifted, 1e-6);
    assert!(report.dimensions_match);
    assert!(!report.affine_matches);
    assert_eq!(report.max_affine_difference, 0.5);
    assert!(mask.require_grid(&shifted, 1e-6).is_err());
}

#[test]
fn mutation_validates_lengths_and_labels() {
    let mut brik = BrikBuilder::cardinal(
        [2, 1, 1],
        [Orientation::R2L, Orientation::A2P, Orientation::I2S],
        [0.0; 3],
        [1.0; 3],
    )
    .values(vec![1.0, 2.0], StoragePolicy::Float)
    .unwrap()
    .labels(["old"])
    .time_axis(TimeAxis {
        nt: 1,
        origin: 0.0,
        step: 1.0,
        duration: 0.0,
        stored_units: TimeUnits::Seconds,
        slice_offsets: Vec::new(),
        slice_z_origin: 0.0,
        slice_dz: 0.0,
    })
    .build()
    .unwrap();

    let old = brik
        .replace_sub_brick(
            0,
            SubBrick::from_f32(vec![3.0, 4.0], StoragePolicy::Float).unwrap(),
        )
        .unwrap();
    assert_eq!(old.unwrap().to_f32().unwrap(), [1.0, 2.0]);
    assert!(brik
        .replace_sub_brick(
            0,
            SubBrick::from_f32(vec![1.0], StoragePolicy::Float).unwrap(),
        )
        .is_err());
    brik.push_sub_brick(SubBrick::from_f32(vec![5.0, 6.0], StoragePolicy::Float).unwrap())
        .unwrap();
    assert_eq!(brik.header.time_axis().unwrap().nt, 2);
    brik.set_brick_labels(&["new", "added"]).unwrap();
    assert_eq!(brik.header.brick_labels(), ["new", "added"]);
}

#[test]
fn typed_header_setters_reject_shifted_metadata() {
    let mut header = Header::default();
    header.set("DATASET_RANK", AttributeValue::Int(vec![3, 2]));
    assert!(header.set_brick_labels(&["only one"]).is_err());
    assert!(header
        .set_time_axis(&TimeAxis {
            nt: 1,
            origin: 0.0,
            step: 1.0,
            duration: 0.0,
            stored_units: TimeUnits::Seconds,
            slice_offsets: Vec::new(),
            slice_z_origin: 0.0,
            slice_dz: 0.0,
        })
        .is_err());
    assert!(header.set_brick_stats(&[None]).is_err());
}

#[test]
fn volume_envelope_round_trips_values_metadata_and_storage_policy() {
    let source = read_any(common::data("volume/stat+orig.HEAD")).unwrap();
    let envelope = volume_to_envelope(&source).unwrap();
    let output = envelope.to_brik(StoragePolicy::Float).unwrap();

    assert_eq!(output.dimensions, source.dimensions());
    assert_eq!(output.header.brick_labels(), source.labels().unwrap());
    assert_eq!(output.header.brick_stats(), source.stats().unwrap());
    assert_eq!(
        output.header.ijk_to_ras().unwrap(),
        source.ijk_to_ras().unwrap()
    );
    for p in 0..source.nvols() {
        assert_eq!(
            output.sub_brick(p).unwrap().to_f32().unwrap(),
            source.frame(p).unwrap()
        );
        assert_eq!(output.sub_brick(p).unwrap().brik_type(), BrikType::Float);
    }
}

#[test]
fn write_options_protect_outputs_and_append_history() {
    let dir = scratch("write_options");
    let brik = BrikBuilder::cardinal(
        [1, 1, 1],
        [Orientation::R2L, Orientation::A2P, Orientation::I2S],
        [0.0; 3],
        [1.0; 3],
    )
    .values(vec![42.0], StoragePolicy::Float)
    .unwrap()
    .build()
    .unwrap();
    let path = dir.join("result");
    let options = BrikWriteOptions {
        overwrite: false,
        history_entry: Some("toy -input source".into()),
    };
    let written = brik.write_with_options(&path, &options).unwrap();
    assert_eq!(
        Header::read(&written.head).unwrap().history(),
        Some("toy -input source")
    );
    assert!(brik.write_with_options(&path, &options).is_err());
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn format_neutral_writer_chooses_nifti_and_preserves_afni_metadata() {
    let dir = scratch("format_neutral_nifti");
    let source = read_any(common::data("volume/stat+orig.HEAD")).unwrap();
    let path = dir.join("result.nii.gz");
    let options = VolumeWriteOptions {
        overwrite: false,
        history_entry: Some("3dRustDemo -input stat+orig".into()),
        ..VolumeWriteOptions::default()
    };

    let written = source.write_with_options(&path, &options).unwrap();
    assert_eq!(written, WrittenVolume::Nifti(path.clone()));
    assert_eq!(written.format(), VolumeOutputFormat::Nifti);
    let raw_nifti = Nifti::read(&path).unwrap();
    let source_time = volume_to_envelope(&source)
        .unwrap()
        .dataset
        .time_step_seconds();
    assert_eq!(raw_nifti.header.pixdim[4], source_time.unwrap_or(0.0));
    assert_eq!(
        raw_nifti.header.xyzt_units,
        if source_time.is_some() { 2 | 8 } else { 2 }
    );
    let output = read_any(&path).unwrap();
    assert_eq!(output.dimensions(), source.dimensions());
    assert_eq!(output.nvols(), source.nvols());
    assert_eq!(output.labels().unwrap(), source.labels().unwrap());
    assert_eq!(output.stats().unwrap(), source.stats().unwrap());
    output
        .grid()
        .unwrap()
        .compatibility(&source.grid().unwrap(), 1e-5)
        .require_compatible()
        .unwrap();
    for frame in 0..source.nvols() {
        assert_eq!(output.frame(frame).unwrap(), source.frame(frame).unwrap());
    }
    assert!(output
        .afni_header()
        .unwrap()
        .unwrap()
        .history()
        .unwrap()
        .contains("3dRustDemo -input stat+orig"));

    // No-clobber is checked at publication time; the complete old file stays.
    assert!(source.write_with_options(&path, &options).is_err());
    assert!(read_any(&path).is_ok());
    source.write(&path).unwrap();
    assert!(read_any(&path).is_ok());
    assert!(std::fs::read_dir(&dir).unwrap().all(|entry| {
        !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .contains(".afni-io-")
    }));
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn format_neutral_writer_supports_nifti2_and_afni_path_inference() {
    let dir = scratch("format_neutral_formats");
    let source = read_any(common::data("volume/timeseries+orig.HEAD")).unwrap();

    let nii_path = dir.join("series.nii");
    let nii_options = VolumeWriteOptions {
        nifti_version: NiftiVersion::Nifti2,
        ..VolumeWriteOptions::default()
    };
    source.write_with_options(&nii_path, &nii_options).unwrap();
    let nifti = Nifti::read(&nii_path).unwrap();
    let source_axis = source.afni_header().unwrap().unwrap().time_axis().unwrap();
    assert_eq!(nifti.header.version, NiftiVersion::Nifti2);
    assert_eq!(nifti.header.pixdim[4], source_axis.step);
    assert_eq!(nifti.header.toffset, source_axis.origin);

    // A bare prefix follows AFNI command-line convention and gains +orig.
    let afni_prefix = dir.join("series_copy");
    let written = source.write(&afni_prefix).unwrap();
    let WrittenVolume::Afni(paths) = written else {
        panic!("bare prefix should select AFNI output");
    };
    assert!(paths.head.ends_with("series_copy+orig.HEAD"));
    let afni = read_any(&paths.head).unwrap();
    assert_eq!(afni.dimensions(), source.dimensions());
    assert_eq!(afni.nvols(), source.nvols());
    assert_eq!(afni.labels().unwrap(), source.labels().unwrap());
    for frame in 0..source.nvols() {
        assert_eq!(afni.frame(frame).unwrap(), source.frame(frame).unwrap());
    }
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn format_override_and_detached_nifti_diagnostic_are_explicit() {
    let dir = scratch("format_neutral_override");
    let source = read_any(common::data("volume/correl+orig.HEAD")).unwrap();
    let path = dir.join("extensionless_nifti");
    let options = VolumeWriteOptions {
        format: Some(VolumeOutputFormat::Nifti),
        ..VolumeWriteOptions::default()
    };
    source.write_with_options(&path, &options).unwrap();
    let output = Nifti::read(&path).unwrap();
    let stat = source.stats().unwrap()[0].clone().unwrap();
    assert_eq!(output.header.intent_code, stat.kind.code() as i32);
    assert_eq!(output.header.intent_p1, stat.params[0]);
    assert_eq!(output.header.intent_p2, stat.params[1]);
    assert_eq!(output.header.intent_p3, stat.params[2]);

    let error = source.write(dir.join("unsupported.hdr")).unwrap_err();
    assert!(error.to_string().contains("detached NIfTI"));
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn volume_builder_constructs_the_same_new_dataset_for_both_formats() {
    let dir = scratch("volume_builder");
    let grid = GridSpec {
        dimensions: [2, 1, 1],
        ijk_to_ras: [
            [2.0, 0.1, 0.0, -10.0],
            [0.0, 3.0, 0.0, 20.0],
            [0.0, 0.0, 4.0, 30.0],
            [0.0, 0.0, 0.0, 1.0],
        ],
    };
    let ttest = StatSpec::new(StatKind::Ttest, &[12.0], 0.0);
    let envelope = VolumeBuilder::new(grid.clone())
        .unwrap()
        .values(vec![1.0, 2.0])
        .unwrap()
        .values(vec![3.0, 4.0])
        .unwrap()
        .labels(["signal", "t statistic"])
        .stats([None, Some(ttest.clone())])
        .time_axis_seconds(0.8, -0.2)
        .history("created from scratch")
        .build()
        .unwrap();

    let afni_path = dir.join("built+orig");
    let nifti_path = dir.join("built.nii");
    envelope.write(&afni_path).unwrap();
    envelope.write(&nifti_path).unwrap();
    for output in [
        read_any(&afni_path).unwrap(),
        read_any(&nifti_path).unwrap(),
    ] {
        output
            .grid()
            .unwrap()
            .compatibility(&grid, 1e-5)
            .require_compatible()
            .unwrap();
        assert_eq!(output.frame(0).unwrap(), [1.0, 2.0]);
        assert_eq!(output.frame(1).unwrap(), [3.0, 4.0]);
        assert_eq!(output.labels().unwrap(), ["signal", "t statistic"]);
        assert_eq!(output.stats().unwrap(), [None, Some(ttest.clone())]);
        let axis = output.afni_header().unwrap().unwrap().time_axis().unwrap();
        assert!((axis.step - 0.8).abs() < 1e-6);
        assert!((axis.origin + 0.2).abs() < 1e-6);
        assert_eq!(
            output.afni_header().unwrap().unwrap().history(),
            Some("created from scratch")
        );
    }

    assert!(VolumeBuilder::new(grid.clone())
        .unwrap()
        .values(vec![1.0])
        .is_err());
    assert!(VolumeBuilder::new(grid)
        .unwrap()
        .values(vec![1.0, 2.0])
        .unwrap()
        .labels(["too", "many"])
        .build()
        .is_err());
    std::fs::remove_dir_all(dir).ok();
}
