//! Ergonomic dataset-programming APIs used by AFNI-style command-line tools.

mod common;

use std::path::PathBuf;

use afni_core::domain::DomainId;
use afni_core::numeric::NonFinitePolicy;
use afni_core::processing::{summarize_time_series, TimeSeriesStatistic};
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
fn grids_and_masks_bridge_directly_to_core_types() {
    let loaded = VolumeDataset::open(common::data("volume/u8+orig.HEAD")).unwrap();
    let grid = GridSpec::try_from(loaded.dataset().domain()).unwrap();
    let domain = grid
        .to_domain(Some(DomainId::new("grid-id").unwrap()))
        .unwrap();
    assert_eq!(GridSpec::try_from(&domain).unwrap(), grid);

    let ijk = [1.25, 2.0, 3.5];
    let world = grid.ijk_to_world(ijk).unwrap();
    let back = grid.world_to_ijk(world).unwrap();
    for (actual, expected) in back.into_iter().zip(ijk) {
        assert!((actual - expected).abs() < 1.0e-10);
    }
    let index = grid.linear_index(2, 2, 3).unwrap();
    assert_eq!(grid.ijk(index).unwrap(), [2, 2, 3]);

    let mask = VolumeMask::read(common::data("volume/u8+orig.HEAD")).unwrap();
    let core_mask = mask.for_dataset(loaded.dataset()).unwrap();
    assert_eq!(core_mask.domain(), loaded.dataset().domain());
    assert_eq!(core_mask.count(), mask.count());
    assert_eq!(VolumeMask::try_from(&core_mask).unwrap(), mask);

    let mismatched = VolumeDataset::open(common::data("volume/lpi+orig.HEAD")).unwrap();
    assert!(mask.for_dataset(mismatched.dataset()).is_err());
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
fn derived_volume_retains_grid_and_history_but_rebuilds_frame_metadata() {
    let source = VolumeDataset::open(common::data("volume/timeseries+orig.HEAD")).unwrap();
    let summary = summarize_time_series(
        source.dataset(),
        None,
        [TimeSeriesStatistic::Mean.output("mean", 0.0).unwrap()],
        NonFinitePolicy::Skip,
    )
    .unwrap();
    let derived = source
        .derive(summary)
        .unwrap()
        .with_history("3dRustMean -input timeseries+orig");

    assert_eq!(derived.dataset().columns().len(), 1);
    assert_eq!(derived.dataset().columns()[0].label(), "mean");
    assert_eq!(derived.dataset().time_step_seconds(), None);
    assert_eq!(derived.dataset().domain(), source.dataset().domain());
    assert!(derived
        .metadata()
        .afni_header
        .as_ref()
        .unwrap()
        .history()
        .unwrap()
        .contains("3dRustMean -input timeseries+orig"));

    let directory = scratch("derived_volume");
    let output_path = directory.join("mean+orig");
    derived.write(&output_path).unwrap();
    let output = read_any(&output_path).unwrap();
    assert_eq!(output.dimensions(), [4, 5, 6]);
    assert_eq!(output.nvols(), 1);
    assert_eq!(output.labels().unwrap(), ["mean"]);
    assert_eq!(output.stats().unwrap(), [None]);
    let output_header = output.afni_header().unwrap().unwrap();
    assert!(output_header.time_axis().is_none());
    assert!(output_header
        .history()
        .unwrap()
        .contains("3dRustMean -input timeseries+orig"));
    assert_ne!(
        output_header.idcode(),
        source.metadata().afni_header.as_ref().unwrap().idcode()
    );
    std::fs::remove_dir_all(directory).ok();
}

#[test]
fn derived_volume_rejects_a_different_grid() {
    let source = VolumeDataset::open(common::data("volume/u8+orig.HEAD")).unwrap();
    let other = VolumeDataset::open(common::data("volume/lpi+orig.HEAD")).unwrap();
    assert!(source.derive(other.dataset).is_err());
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

#[test]
fn volume_metadata_preserves_slice_timing_and_private_nifti_extensions() {
    let grid = GridSpec {
        dimensions: [1, 1, 3],
        ijk_to_ras: [
            [1.0, 0.0, 0.0, 0.0],
            [0.0, 1.0, 0.0, 0.0],
            [0.0, 0.0, 2.0, -2.0],
            [0.0, 0.0, 0.0, 1.0],
        ],
    };
    let envelope = VolumeBuilder::new(grid)
        .unwrap()
        .values(vec![1.0, 2.0, 3.0])
        .unwrap()
        .values(vec![4.0, 5.0, 6.0])
        .unwrap()
        .time_axis_seconds(2.0, -0.5)
        .build()
        .unwrap();
    let mut source = envelope.to_nifti(NiftiVersion::Nifti1).unwrap();
    source.header.dim_info = 3 << 4;
    source.header.slice_start = 0;
    source.header.slice_end = 2;
    source.header.slice_code = 3;
    source.header.xyzt_units = 2 | 16;
    source.header.pixdim[4] = 2000.0;
    source.header.toffset = -500.0;
    source.header.slice_duration = 250.0;
    source.extensions.push(NiftiExtension {
        code: 44,
        data: b"private!".to_vec(),
    });
    // Exercise a standards-only NIfTI source: timing must not depend on an
    // AFNI extension being present.
    source.extensions.retain(|extension| extension.code != 4);

    let loaded = Volume::Nifti(Box::new(source));
    let retained = volume_to_envelope(&loaded).unwrap();
    assert_eq!(retained.dataset.time_step_seconds(), Some(2.0));
    assert_eq!(
        retained
            .metadata()
            .time_axis
            .as_ref()
            .unwrap()
            .slice_offsets,
        [0.0, 0.5, 0.25]
    );
    assert_eq!(
        retained.metadata().nifti.as_ref().unwrap().extensions[0].code,
        44
    );
    let output = retained.to_nifti(NiftiVersion::Nifti2).unwrap();
    assert_eq!(output.header.slice_code, 3);
    assert_eq!(output.header.slice_duration, 0.25);
    assert_eq!(output.header.xyzt_units, 2 | 8);
    assert_eq!(output.header.dim_info, 3 << 4);
    assert_eq!(
        output
            .extensions
            .iter()
            .find(|extension| extension.code == 44)
            .unwrap()
            .data,
        b"private!"
    );
}

#[test]
fn detailed_afni_time_axis_survives_builder_and_format_conversion() {
    let axis = TimeAxis {
        nt: 2,
        origin: -0.125,
        step: 1.5,
        duration: 0.5,
        stored_units: TimeUnits::Seconds,
        slice_offsets: vec![0.0, 0.25, 0.125],
        slice_z_origin: -2.0,
        slice_dz: 2.0,
    };
    let grid = GridSpec {
        dimensions: [1, 1, 3],
        ijk_to_ras: [
            [1.0, 0.0, 0.0, 0.0],
            [0.0, 1.0, 0.0, 0.0],
            [0.0, 0.0, 2.0, -2.0],
            [0.0, 0.0, 0.0, 1.0],
        ],
    };
    let envelope = VolumeBuilder::new(grid)
        .unwrap()
        .values(vec![1.0; 3])
        .unwrap()
        .values(vec![2.0; 3])
        .unwrap()
        .time_axis(axis.clone())
        .build()
        .unwrap();
    assert_eq!(
        envelope
            .to_brik(StoragePolicy::Float)
            .unwrap()
            .header
            .time_axis()
            .unwrap(),
        axis
    );
    assert_eq!(
        envelope
            .to_nifti(NiftiVersion::Nifti1)
            .unwrap()
            .afni_header()
            .unwrap()
            .unwrap()
            .time_axis()
            .unwrap(),
        axis
    );
}

#[test]
fn frame_writer_streams_afni_and_nifti_and_cleans_up_incomplete_output() {
    let dir = scratch("frame_writer");
    let grid = GridSpec {
        dimensions: [2, 1, 1],
        ijk_to_ras: [
            [2.0, 0.0, 0.0, -1.0],
            [0.0, 2.0, 0.0, 1.0],
            [0.0, 0.0, 2.0, 3.0],
            [0.0, 0.0, 0.0, 1.0],
        ],
    };
    let axis = TimeAxis {
        nt: 2,
        origin: 0.0,
        step: 0.8,
        duration: 0.0,
        stored_units: TimeUnits::Seconds,
        slice_offsets: Vec::new(),
        slice_z_origin: 0.0,
        slice_dz: 0.0,
    };
    let spec = VolumeWriteSpec::new(grid.clone(), 2)
        .unwrap()
        .labels(["first", "second"])
        .time_axis(axis);
    let frames = [[-3.0, 4.0], [5.0, 9.0]];

    let afni_path = dir.join("streamed+orig.BRIK.gz");
    let mut afni_writer = VolumeFrameWriter::create(
        &afni_path,
        spec.clone(),
        VolumeWriteOptions {
            afni_storage: StoragePolicy::AutoShort,
            history_entry: Some("streaming AFNI test".into()),
            ..VolumeWriteOptions::default()
        },
    )
    .unwrap();
    for frame in &frames {
        afni_writer.write_frame(frame).unwrap();
    }
    assert_eq!(afni_writer.frames_written(), 2);
    afni_writer.finish().unwrap();
    let afni = read_any(&afni_path).unwrap();
    assert_eq!(afni.labels().unwrap(), ["first", "second"]);
    for (index, expected) in frames.iter().enumerate() {
        for (actual, expected) in afni.frame(index).unwrap().iter().zip(expected) {
            assert!((actual - expected).abs() < 0.001);
        }
    }

    let nifti_path = dir.join("streamed.nii.gz");
    let mut nifti_writer =
        VolumeFrameWriter::create(&nifti_path, spec.clone(), VolumeWriteOptions::default())
            .unwrap();
    for frame in &frames {
        nifti_writer.write_frame(frame).unwrap();
    }
    nifti_writer.finish().unwrap();
    let nifti = read_any(&nifti_path).unwrap();
    assert_eq!(nifti.frame(0).unwrap(), frames[0]);
    assert_eq!(nifti.frame(1).unwrap(), frames[1]);

    let incomplete_path = dir.join("incomplete.nii");
    let mut incomplete =
        VolumeFrameWriter::create(&incomplete_path, spec, VolumeWriteOptions::default()).unwrap();
    incomplete.write_frame(&frames[0]).unwrap();
    assert!(incomplete.finish().is_err());
    assert!(!incomplete_path.exists());
    assert!(std::fs::read_dir(&dir).unwrap().all(|entry| {
        !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .contains(".afni-io-")
    }));
    std::fs::remove_dir_all(dir).ok();
}
