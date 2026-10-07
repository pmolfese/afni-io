//! Unified NIML/GIfTI surface-dataset entry point.

mod common;

use afni_io::prelude::{SurfaceDatasetFormat, SurfaceDatasetReadOptions, SurfaceDatasetReader};

const ICO_NODES: usize = 42;

fn column_values(reader: &SurfaceDatasetReader, column: usize) -> Vec<f64> {
    let values = reader.dataset().columns()[column].values();
    (0..values.len())
        .map(|row| values.get_f64(row).unwrap())
        .collect()
}

#[test]
fn dense_niml_and_gifti_share_one_core_facing_api() {
    let niml = SurfaceDatasetReader::open(common::data("surface/dense_asc.niml.dset")).unwrap();
    let gifti = SurfaceDatasetReader::open(common::data("surface/dense_asc.gii.dset")).unwrap();

    assert_eq!(niml.format(), SurfaceDatasetFormat::Niml);
    assert_eq!(gifti.format(), SurfaceDatasetFormat::Gifti);
    assert_eq!(niml.dataset().row_count(), gifti.dataset().row_count());
    assert_eq!(
        niml.dataset().columns().len(),
        gifti.dataset().columns().len()
    );
    for column in 0..niml.dataset().columns().len() {
        assert_eq!(column_values(&niml, column), column_values(&gifti, column));
    }
    assert!(niml.warnings().is_empty());
    assert!(gifti.warnings().is_empty());
    assert!(niml.niml_extras().is_some());
    assert!(niml.gifti().is_none());
    assert!(gifti.niml_extras().is_none());
    assert!(gifti.gifti().is_some());
}

#[test]
fn sparse_inputs_require_and_use_the_complete_surface_size() {
    let niml_path = common::data("surface/sparse_asc.niml.dset");
    let gifti_path = common::data("surface/sparse_asc.gii.dset");
    assert!(SurfaceDatasetReader::open(&niml_path).is_err());
    assert!(SurfaceDatasetReader::open(&gifti_path).is_err());

    let options = SurfaceDatasetReadOptions {
        node_count: Some(ICO_NODES),
        ..SurfaceDatasetReadOptions::default()
    };
    let niml = SurfaceDatasetReader::open_with_options(&niml_path, options).unwrap();
    let gifti = SurfaceDatasetReader::open_with_options(&gifti_path, options).unwrap();
    assert!(niml.dataset().is_sparse());
    assert!(gifti.dataset().is_sparse());
    assert_eq!(niml.dataset().domain().sample_count(), ICO_NODES);
    assert_eq!(gifti.dataset().domain().sample_count(), ICO_NODES);
    for row in 0..niml.dataset().row_count() {
        assert_eq!(
            niml.dataset().sample_for_row(row),
            gifti.dataset().sample_for_row(row)
        );
    }
}

#[test]
fn niml_reader_rebuilds_a_lossless_write_envelope() {
    let reader = SurfaceDatasetReader::open(common::data("surface/fdr.niml.dset")).unwrap();
    let envelope = reader.niml_envelope().unwrap();
    let raw = envelope.to_niml().unwrap();

    assert_eq!(raw.rows(), reader.dataset().row_count());
    assert_eq!(raw.column_count(), reader.dataset().columns().len());
    assert_eq!(
        raw.domain_parent_idcode,
        reader.dataset().parent_ids().domain_parent
    );
    assert!(reader
        .dataset()
        .columns()
        .iter()
        .enumerate()
        .any(|(column, core)| core.fdr_curve().is_some()
            && raw.attributes.fdr_curve(column).is_some()));
}

#[test]
fn geometry_only_gifti_stays_on_the_geometry_api() {
    let error = SurfaceDatasetReader::open(common::data("surface/ico_ascii.gii")).unwrap_err();
    assert!(error.to_string().contains("only geometry found"));
}
