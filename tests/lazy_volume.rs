//! Selector and frame-at-a-time volume reader tests.

mod common;

use afni_io::prelude::*;

fn selected_name(relative: &str, selector: &str) -> String {
    format!("{}[{selector}]", common::data(relative).display())
}

#[test]
fn afni_selector_controls_logical_order_labels_and_duplicates() {
    let eager = read_any(common::data("volume/timeseries+orig.HEAD")).unwrap();
    let mut reader = VolumeReader::open(selected_name(
        "volume/timeseries+orig.HEAD",
        "$,#0,2..0(-1),#1",
    ))
    .unwrap();

    assert_eq!(reader.format(), VolumeFormat::Afni);
    assert_eq!(reader.source_nvols(), 3);
    assert_eq!(reader.selected_indices(), [2, 0, 2, 1, 0, 1]);
    assert_eq!(reader.labels(), ["#2", "#0", "#2", "#1", "#0", "#1"]);
    assert_eq!(reader.dimensions(), eager.dimensions());
    assert_eq!(reader.ijk_to_ras(), eager.ijk_to_ras().unwrap());

    for (logical, &source) in reader.selected_indices().to_vec().iter().enumerate() {
        assert_eq!(reader.frame(logical).unwrap(), eager.frame(source).unwrap());
    }
}

#[test]
fn read_any_materializes_selected_afni_and_nifti_frames() {
    let source = read_any(common::data("volume/timeseries+orig.HEAD")).unwrap();
    let selected = read_any(selected_name("volume/timeseries+orig.HEAD", "2,0,2")).unwrap();
    assert_eq!(selected.nvols(), 3);
    assert_eq!(selected.labels().unwrap(), ["#2", "#0", "#2"]);
    assert_eq!(selected.frame(0).unwrap(), source.frame(2).unwrap());
    assert_eq!(selected.frame(1).unwrap(), source.frame(0).unwrap());
    assert_eq!(selected.frame(2).unwrap(), source.frame(2).unwrap());
    let axis = selected
        .afni_header()
        .unwrap()
        .unwrap()
        .time_axis()
        .unwrap();
    assert_eq!(axis.nt, 3);
    assert_eq!(axis.step, 2.0);

    let source = read_any(common::data("volume/stat.nii")).unwrap();
    let selected = read_any(selected_name("volume/stat.nii", "$,0")).unwrap();
    assert_eq!(selected.nvols(), 2);
    assert_eq!(
        selected.frame(0).unwrap(),
        source.frame(source.nvols() - 1).unwrap()
    );
    assert_eq!(selected.frame(1).unwrap(), source.frame(0).unwrap());
    assert_eq!(selected.labels().unwrap()[1], source.labels().unwrap()[0]);
}

#[test]
fn eager_selection_matches_afnis_time_axis_policy() {
    let selected = read_any(selected_name("volume/timeseries+orig.HEAD", "0..$(2)")).unwrap();
    let axis = selected
        .afni_header()
        .unwrap()
        .unwrap()
        .time_axis()
        .unwrap();
    assert_eq!(axis.nt, 2);
    assert_eq!(axis.origin, 0.0);
    assert_eq!(axis.step, 2.0);

    let one = read_any(selected_name("volume/timeseries+orig.HEAD", "1")).unwrap();
    assert!(one.afni_header().unwrap().unwrap().time_axis().is_none());
}

#[test]
fn lazy_afni_plain_and_gzip_match_eager_values() {
    for relative in ["volume/mixed+orig.HEAD", "volume/s16gz+orig.HEAD"] {
        let eager = read_any(common::data(relative)).unwrap();
        let mut reader = VolumeReader::open(common::data(relative)).unwrap();
        assert_eq!(reader.is_compressed(), relative.contains("s16gz"));
        for frame in 0..reader.nvols() {
            assert_eq!(reader.frame(frame).unwrap(), eager.frame(frame).unwrap());
        }
    }
}

#[test]
fn lazy_nifti_plain_and_gzip_match_eager_values() {
    for relative in ["volume/stat.nii", "volume/s16.nii.gz"] {
        let eager = read_any(common::data(relative)).unwrap();
        let mut reader = VolumeReader::open(common::data(relative)).unwrap();
        assert_eq!(reader.format(), VolumeFormat::Nifti);
        assert_eq!(reader.is_compressed(), relative.ends_with(".gz"));
        assert_eq!(reader.labels(), eager.labels().unwrap());
        assert_eq!(reader.stats(), eager.stats().unwrap());
        for frame in 0..reader.nvols() {
            assert_eq!(reader.frame(frame).unwrap(), eager.frame(frame).unwrap());
        }
    }
}

#[test]
fn volume_dataset_is_the_eager_counterpart_to_volume_reader() {
    let path = selected_name("volume/timeseries+orig.HEAD", "2,0,2");

    let loaded = VolumeDataset::open(&path).unwrap();
    let from_reader = VolumeReader::open(&path).unwrap().into_dataset().unwrap();

    assert_eq!(loaded.dataset, from_reader.dataset);
    assert_eq!(loaded.metadata, from_reader.metadata);
    assert_eq!(loaded.dataset.columns().len(), 3);
    assert_eq!(
        loaded
            .dataset
            .columns()
            .iter()
            .map(|column| column.label())
            .collect::<Vec<_>>(),
        ["#2", "#0", "#2"]
    );
    assert_eq!(loaded.dataset().time_step_seconds(), Some(2.0));

    let (dataset, metadata) = loaded.into_parts();
    assert_eq!(dataset.columns().len(), 3);
    assert!(metadata.afni_header.is_some());
}

#[test]
fn volume_dataset_loads_nifti_values_and_retains_nifti_metadata() {
    let loaded = VolumeDataset::open(common::data("volume/stat.nii")).unwrap();
    let raw = read_any(common::data("volume/stat.nii")).unwrap();

    assert_eq!(loaded.dataset.columns().len(), raw.nvols());
    assert_eq!(
        loaded
            .dataset
            .columns()
            .iter()
            .map(|column| column.label())
            .collect::<Vec<_>>(),
        raw.labels().unwrap()
    );
    assert!(loaded.metadata.nifti.is_some());
    assert!(loaded.metadata.afni_header.is_some());
}

#[test]
fn nifti_selectors_use_afni_extension_labels() {
    let eager = read_any(common::data("volume/stat.nii")).unwrap();
    let labels = eager.labels().unwrap();
    assert!(labels.len() >= 2);
    let expression = format!("{},$", labels[0]);
    let mut reader = VolumeReader::open(selected_name("volume/stat.nii", &expression)).unwrap();
    assert_eq!(reader.selected_indices(), [0, eager.nvols() - 1]);
    assert_eq!(reader.labels()[0], labels[0]);
    assert_eq!(reader.frame(0).unwrap(), eager.frame(0).unwrap());
    assert_eq!(
        reader.frame(1).unwrap(),
        eager.frame(eager.nvols() - 1).unwrap()
    );
}

#[test]
fn reusable_buffers_iterators_and_lazy_time_series_agree() {
    let mut reader =
        VolumeReader::open(selected_name("volume/timeseries+orig.HEAD", "0..$(2)")).unwrap();
    let expected: Vec<Vec<f32>> = (0..reader.nvols())
        .map(|frame| reader.frame(frame).unwrap())
        .collect();

    let mut buffer = vec![f32::NAN; reader.voxels()];
    reader.frame_into(1, &mut buffer).unwrap();
    assert_eq!(buffer, expected[1]);

    let iterated: Vec<Vec<f32>> = reader.frames().collect::<Result<Vec<_>>>().unwrap();
    assert_eq!(iterated, expected);

    let voxel = 70;
    let series = reader.voxel_series(voxel).unwrap();
    assert_eq!(
        series,
        expected
            .iter()
            .map(|frame| frame[voxel])
            .collect::<Vec<_>>()
    );
    assert!(reader.frame(reader.nvols()).is_err());
    assert!(reader.frame_into(0, &mut buffer[..2]).is_err());
}

#[test]
fn lazy_detached_nifti_opens_both_hdr_and_img_names() {
    let source_path = common::data("volume/lpi.nii");
    let eager = Nifti::read(&source_path).unwrap();
    assert_eq!(eager.header.version, NiftiVersion::Nifti1);
    let expected: Vec<f32> = (0..4 * 5 * 6)
        .map(|index| eager.get_scaled(index).unwrap() as f32)
        .collect();
    let source = std::fs::read(source_path).unwrap();
    let offset = eager.header.vox_offset as usize;

    let directory =
        std::env::temp_dir().join(format!("afni_io_lazy_detached_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&directory);
    std::fs::create_dir_all(&directory).unwrap();
    let header_path = directory.join("volume.hdr");
    let image_path = directory.join("volume.img");
    let mut header = source[..offset].to_vec();
    header[344..348].copy_from_slice(b"ni1\0");
    header[108..112].copy_from_slice(&0f32.to_le_bytes());
    std::fs::write(&header_path, header).unwrap();
    std::fs::write(&image_path, &source[offset..]).unwrap();

    for path in [&header_path, &image_path] {
        let mut reader = NiftiReader::open(path).unwrap();
        assert_eq!(reader.dimensions(), [4, 5, 6]);
        assert_eq!(reader.read_frame(0).unwrap(), expected);
    }
    std::fs::remove_dir_all(directory).ok();
}
