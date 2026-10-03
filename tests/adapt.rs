//! Adapters into `afni-core`: every committed surface, label, statistic,
//! time-series, GIfTI, and volume fixture converts, and NIML round trips keep
//! values, indices, statistics, curves, and label tables.

mod common;

use afni_core::column::{ColumnData, ColumnRole, MissingFill};
use afni_core::dataset::{Dataset, DatasetKind};
use afni_core::domain::Domain;
use afni_io::adapt::{
    gifti_to_core, niml_to_core, niml_to_core_with, onedee_to_core, volume_to_core, AdaptOptions,
    CurvePolicy, NimlEnvelope,
};
use afni_io::prelude::*;

/// Nodes in the committed icosahedron fixture surface.
const ICO_NODES: usize = 42;

const NIML_FIXTURES: &[&str] = &[
    "dense_asc.niml.dset",
    "dense_bi.niml.dset",
    "sparse_asc.niml.dset",
    "sparse_bi.niml.dset",
    "fdr.niml.dset",
    "labels.niml.dset",
    "stat.niml.dset",
    "timeseries.niml.dset",
];

fn read(name: &str) -> NimlDataset {
    NimlDataset::read(common::data(&format!("surface/{name}"))).unwrap()
}

/// Convert, supplying the node count only where the file is sparse.
fn convert(name: &str) -> (NimlDataset, NimlEnvelope) {
    let raw = read(name);
    let nodes = raw.is_sparse().then_some(ICO_NODES);
    let env = niml_to_core(&raw, nodes).unwrap_or_else(|e| panic!("{name}: {e}"));
    (raw, env)
}

/// Column `c` of a core dataset as f64s, to compare with raw arrays.
fn core_col(ds: &Dataset, c: usize) -> Vec<f64> {
    let v = ds.columns()[c].values();
    (0..v.len()).map(|r| v.get_f64(r).unwrap()).collect()
}

#[test]
fn every_niml_fixture_converts_with_matching_shape_and_values() {
    for name in NIML_FIXTURES {
        let (raw, env) = convert(name);
        let ds = &env.core;
        assert_eq!(ds.row_count(), raw.rows(), "{name}");
        assert_eq!(ds.columns().len(), raw.column_count(), "{name}");
        assert_eq!(ds.is_sparse(), raw.is_sparse(), "{name}");
        // Rows keep their order and node numbers.
        for row in 0..raw.rows() {
            assert_eq!(
                ds.sample_for_row(row),
                raw.node_for_row(row),
                "{name} row {row}"
            );
        }
        // Values are identical, column by column.
        for (c, array) in raw.data.columns.iter().enumerate() {
            assert_eq!(core_col(ds, c), array.to_f64_vec(), "{name} column {c}");
        }
        // Statistics were decoded into typed specs.
        for (c, stat) in raw.column_stats().iter().enumerate() {
            assert_eq!(ds.columns()[c].stat(), stat.as_ref(), "{name} stat {c}");
        }
    }
}

#[test]
fn kinds_roles_and_time_step_are_interpreted() {
    let (_, labels) = convert("labels.niml.dset");
    assert_eq!(labels.core.kind(), &DatasetKind::Label);
    let col = &labels.core.columns()[0];
    assert_eq!(col.role(), &ColumnRole::Label);
    // The label table arrived with integer keys and names, in file order.
    let table = col.label_table().expect("label table on label column");
    assert_eq!(table.get(1).unwrap().name, "Big_House");
    assert_eq!(table.entries()[0].key, 0);

    let (raw, ts) = convert("timeseries.niml.dset");
    assert_eq!(ts.core.kind(), &DatasetKind::TimeSeries);
    assert_eq!(ts.core.time_step_seconds(), raw.time_step);
    assert!(raw.time_step.is_some());
}

#[test]
fn fdr_curves_become_validated_core_curves() {
    let (raw, env) = convert("fdr.niml.dset");
    let mut found = 0;
    for (c, col) in env.core.columns().iter().enumerate() {
        if let Some(curve) = raw.attributes.fdr_curve(c) {
            let core = col.fdr_curve().expect("fdr curve carried over");
            assert_eq!(core.x0(), curve.x0);
            assert_eq!(core.dx(), curve.dx);
            assert_eq!(core.samples(), curve.values.as_slice());
            found += 1;
        }
    }
    assert!(found > 0, "fixture should contain at least one FDR curve");
}

#[test]
fn sparse_datasets_need_a_node_count_and_densify_equivalently() {
    let raw = read("sparse_asc.niml.dset");
    assert!(
        niml_to_core(&raw, None).is_err(),
        "sparse without node_count"
    );
    // A node count smaller than a stored index is rejected, not clipped.
    let max = raw
        .node_indices
        .as_ref()
        .unwrap()
        .iter()
        .max()
        .copied()
        .unwrap() as usize;
    assert!(niml_to_core(&raw, Some(max)).is_err());

    let env = niml_to_core(&raw, Some(ICO_NODES)).unwrap();
    let dense = env.core.to_dense(&MissingFill::default()).unwrap();
    assert_eq!(dense.row_count(), ICO_NODES);
    for row in 0..env.core.row_count() {
        let node = env.core.sample_for_row(row).unwrap() as usize;
        for c in 0..env.core.columns().len() {
            assert_eq!(
                env.core.columns()[c].values().get_f64(row),
                dense.columns()[c].values().get_f64(node),
                "row {row} column {c}"
            );
        }
    }
}

#[test]
fn truly_dense_niml_needs_no_node_count_and_round_trips_without_an_index() {
    // NOTE: AFNI's own "dense" files still carry an INDEX_LIST of 0..n-1, so
    // they count as sparse here. Build a dataset with no index list.
    let with_index = read("dense_asc.niml.dset");
    let raw = NimlDataset::new("Node_Bucket", with_index.data.clone(), None);
    assert!(!raw.is_sparse());

    let env = niml_to_core(&raw, None).unwrap();
    assert!(!env.core.is_sparse());
    assert_eq!(env.core.row_count(), ICO_NODES);
    // The same count passed explicitly is fine; a different one is an error.
    assert!(niml_to_core(&raw, Some(ICO_NODES)).is_ok());
    assert!(niml_to_core(&raw, Some(ICO_NODES + 1)).is_err());
    // Writing back keeps it index-free.
    assert_eq!(env.to_niml().unwrap().node_indices, None);

    // The file's identity index list is the same data as the dense form.
    let indexed = niml_to_core(&with_index, Some(ICO_NODES)).unwrap();
    assert!(indexed.core.map().is_identity(ICO_NODES));
    for c in 0..env.core.columns().len() {
        assert_eq!(core_col(&env.core, c), core_col(&indexed.core, c));
    }
}

#[test]
fn real_afni_output_converts() {
    let raw = NimlDataset::read(common::data(
        "real/dset/test_lh_full_sumaru_clusters.niml.dset",
    ))
    .unwrap();
    // Dense files give their own node count; sparse ones need the caller's.
    let nodes = raw.is_sparse().then(|| {
        raw.node_indices
            .as_ref()
            .unwrap()
            .iter()
            .max()
            .copied()
            .unwrap() as usize
            + 1
    });
    let env = niml_to_core(&raw, nodes).unwrap();
    assert_eq!(env.core.row_count(), raw.rows());
}

#[test]
fn niml_round_trip_preserves_the_core_dataset() {
    for name in NIML_FIXTURES {
        let (raw, env) = convert(name);
        let written = env.to_niml().unwrap();
        // Values, indices, and time step survive exactly.
        assert_eq!(written.data, raw.data, "{name} data");
        assert_eq!(written.node_indices, raw.node_indices, "{name} indices");
        assert_eq!(written.time_step, raw.time_step, "{name} time step");
        assert_eq!(written.column_stats(), raw.column_stats(), "{name} stats");
        assert_eq!(written.column_types(), raw.column_types(), "{name} types");
        assert_eq!(written.label_table, raw.label_table, "{name} label table");
        // Unknown attributes travel in the extras.
        assert_eq!(written.other_attrs, raw.other_attrs, "{name} other attrs");
        assert_eq!(
            written.other_elements, raw.other_elements,
            "{name} elements"
        );

        // Through the actual text writer and back into core: unchanged.
        let text = written.to_niml_string().unwrap();
        let reread = NimlDataset::from_bytes(text.as_bytes()).unwrap();
        let nodes = reread.is_sparse().then_some(ICO_NODES);
        let again = niml_to_core(&reread, nodes).unwrap();
        assert_eq!(again.core.map(), env.core.map(), "{name} map");
        for c in 0..env.core.columns().len() {
            let (a, b) = (&env.core.columns()[c], &again.core.columns()[c]);
            assert_eq!(a.values(), b.values(), "{name} column {c}");
            assert_eq!(a.stat(), b.stat(), "{name} stat {c}");
            assert_eq!(a.fdr_curve(), b.fdr_curve(), "{name} fdr {c}");
            assert_eq!(a.label_table(), b.label_table(), "{name} labels {c}");
            assert_eq!(a.role(), b.role(), "{name} role {c}");
        }
    }
}

#[test]
fn core_edits_reach_the_written_file() {
    let (_, env) = convert("stat.niml.dset");
    // Drop the statistic on column 0 through the core model and write.
    let mut edited = env.clone();
    let cols: Vec<_> = edited.core.columns().to_vec();
    let first = cols[0].clone().with_stat(None);
    let mut new_cols = vec![first];
    new_cols.extend_from_slice(&cols[1..]);
    edited.core = Dataset::new(
        edited.core.kind().clone(),
        edited.core.domain().clone(),
        edited.core.map().clone(),
        new_cols,
    )
    .unwrap();
    assert_eq!(edited.to_niml().unwrap().column_stats()[0], None);
}

#[test]
fn text_columns_cannot_be_written_to_numeric_niml() {
    let (_, env) = convert("dense_asc.niml.dset");
    let n = env.core.row_count();
    let text = afni_core::column::DataColumn::new(
        "names",
        ColumnRole::Generic,
        ColumnData::Text(vec!["x".into(); n]),
    )
    .unwrap();
    let ds = Dataset::dense(DatasetKind::Scalar, env.core.domain().clone(), vec![text]).unwrap();
    assert!(NimlEnvelope::from_core(ds).to_niml().is_err());
}

#[test]
fn gifti_dsets_match_their_niml_twins() {
    for (gii, niml) in [
        ("dense_asc.gii.dset", "dense_asc.niml.dset"),
        ("dense_b64gz.gii.dset", "dense_asc.niml.dset"),
        ("stat.gii.dset", "stat.niml.dset"),
    ] {
        let g = Gifti::read(common::data(&format!("surface/{gii}"))).unwrap();
        let from_gii = gifti_to_core(&g, None).unwrap_or_else(|e| panic!("{gii}: {e}"));
        let (_, from_niml) = convert(niml);
        assert_eq!(from_gii.row_count(), from_niml.core.row_count(), "{gii}");
        for c in 0..from_gii.columns().len() {
            assert_eq!(
                core_col(&from_gii, c),
                core_col(&from_niml.core, c),
                "{gii} col {c}"
            );
            assert_eq!(
                from_gii.columns()[c].stat(),
                from_niml.core.columns()[c].stat(),
                "{gii} stat {c}"
            );
        }
    }
}

#[test]
fn sparse_gifti_needs_node_count() {
    let g = Gifti::read(common::data("surface/sparse_asc.gii.dset")).unwrap();
    assert!(gifti_to_core(&g, None).is_err());
    let ds = gifti_to_core(&g, Some(ICO_NODES)).unwrap();
    let (_, niml) = convert("sparse_asc.niml.dset");
    assert_eq!(ds.map(), niml.core.map());
    assert_eq!(core_col(&ds, 0), core_col(&niml.core, 0));
}

#[test]
fn volumes_convert_with_domain_values_and_metadata() {
    for name in [
        "f32+orig.HEAD",
        "u8+orig.HEAD",
        "s16+orig.HEAD",
        "s16scaled+orig.HEAD",
        "mixed+orig.HEAD",
        "lpi+orig.HEAD",
        "stat+orig.HEAD",
        "timeseries+orig.HEAD",
        "labelled+orig.HEAD",
        "stat.nii",
        "labelled.nii",
        "s16.nii.gz",
    ] {
        let vol = read_any(common::data(&format!("volume/{name}"))).unwrap();
        let ds = volume_to_core(&vol).unwrap_or_else(|e| panic!("{name}: {e}"));
        let Domain::Volume(dom) = ds.domain() else {
            panic!("{name}: not a volume domain")
        };
        assert_eq!(dom.dims(), vol.dimensions(), "{name}");
        assert_eq!(ds.columns().len(), vol.nvols(), "{name}");
        // Spot-check every voxel of every sub-brick against the raw reader.
        for t in 0..vol.nvols() {
            let v = ds.columns()[t].values();
            for k in 0..dom.dims()[2] {
                for j in 0..dom.dims()[1] {
                    for i in 0..dom.dims()[0] {
                        let row = dom.linear_index(i as i64, j as i64, k as i64).unwrap();
                        assert_eq!(
                            v.get_f64(row),
                            vol.value(i, j, k, t).map(f64::from),
                            "{name} ({i},{j},{k},{t})"
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn volume_statistics_labels_and_time_axis_are_typed() {
    let stat = volume_to_core(&read_any(common::data("volume/stat+orig.HEAD")).unwrap()).unwrap();
    let kinds: Vec<_> = stat
        .columns()
        .iter()
        .map(|c| c.stat().map(|s| s.kind))
        .collect();
    assert!(kinds.contains(&Some(StatKind::Ttest)) && kinds.contains(&Some(StatKind::Ftest)));
    assert_eq!(
        stat.columns()
            .iter()
            .find(|c| c.stat().is_some())
            .unwrap()
            .role(),
        &ColumnRole::Statistic
    );

    let lab =
        volume_to_core(&read_any(common::data("volume/labelled+orig.HEAD")).unwrap()).unwrap();
    assert_eq!(lab.kind(), &DatasetKind::Label);
    assert!(lab.columns()[0].label_table().unwrap().len() > 10);

    // timeseries fixture: TR = 2 s.
    let ts =
        volume_to_core(&read_any(common::data("volume/timeseries+orig.HEAD")).unwrap()).unwrap();
    assert_eq!(ts.kind(), &DatasetKind::TimeSeries);
    assert_eq!(ts.time_step_seconds(), Some(2.0));
}

#[test]
fn partially_read_or_rgb_volumes_are_errors_not_partial_datasets() {
    let partial = read_any_volumes(common::data("volume/s16gz+orig.BRIK.gz"), &[2]).unwrap();
    assert!(volume_to_core(&partial).is_err());
    let rgb = read_any(common::data("volume/rgb+orig.HEAD")).unwrap();
    assert!(volume_to_core(&rgb).is_err());
}

#[test]
fn onedee_tables_need_a_matching_domain() {
    let table = OneD::parse("1 2\n3 4\n5 6\n").unwrap();
    let ok = Domain::Surface(afni_core::domain::SurfaceDomain::new(None, 3).unwrap());
    let ds = onedee_to_core(&table, ok).unwrap();
    assert_eq!(core_col(&ds, 1), vec![2.0, 4.0, 6.0]);
    let wrong = Domain::Surface(afni_core::domain::SurfaceDomain::new(None, 4).unwrap());
    assert!(onedee_to_core(&table, wrong).is_err());
}

#[test]
fn file_representation_is_kept_identity_index_lists_stay_indexed() {
    // AFNI writes an INDEX_LIST of 0..n-1 even for full datasets. The adapter
    // keeps that representation (and writes it back) rather than collapsing it.
    let (raw, env) = convert("dense_asc.niml.dset");
    assert!(env.core.is_sparse());
    assert!(env.core.map().is_identity(ICO_NODES));
    assert_eq!(env.to_niml().unwrap().node_indices, raw.node_indices);
}

/// A copy of `fdr.niml.dset` whose first FDR curve has zero spacing.
fn with_broken_curve() -> NimlDataset {
    let mut raw = read("fdr.niml.dset");
    let curve = raw
        .attributes
        .fdr_curve(0)
        .expect("fixture has an FDR curve for column 0");
    let mut values = curve.to_values();
    values[1] = 0.0; // dx = 0: not a valid curve
    raw.attributes
        .set("FDRCURVE_000000", AttributeValue::Float(values));
    raw
}

#[test]
fn malformed_curve_is_an_error_by_default() {
    assert!(niml_to_core(&with_broken_curve(), Some(ICO_NODES)).is_err());
}

#[test]
fn malformed_curve_can_be_skipped_with_a_warning() {
    let opts = AdaptOptions {
        curves: CurvePolicy::SkipWithWarning,
        ..AdaptOptions::default()
    };
    let (env, warnings) = niml_to_core_with(&with_broken_curve(), Some(ICO_NODES), &opts).unwrap();
    // The data is intact; only the bad curve is missing.
    assert!(env.core.columns()[0].fdr_curve().is_none());
    assert_eq!(warnings.len(), 1);
    assert_eq!((warnings[0].column, warnings[0].what), (0, "FDR curve"));
    assert!(!warnings[0].message.is_empty());
    // A healthy file produces no warnings under the lenient policy.
    let (_, none) = niml_to_core_with(&read("fdr.niml.dset"), Some(ICO_NODES), &opts).unwrap();
    assert!(none.is_empty());
}

#[test]
fn surfaces_convert_to_core_meshes_with_matching_geometry() {
    use afni_io::adapt::surface_to_core;
    // The 42-node icosahedron fixture, in two file formats.
    let asc = Surface::read(common::data("surface/ico.asc")).unwrap();
    let mesh = surface_to_core(&asc).unwrap();
    assert_eq!(mesh.vertices().len(), 42);
    assert_eq!(mesh.topology().face_count(), asc.faces.len());
    let report = mesh.topology().report();
    assert!(
        report.is_clean() && report.euler_characteristic == 2,
        "{report:?}"
    );
    // Node areas partition the total area.
    let sum: f64 = mesh.node_areas().iter().sum();
    assert!((sum - mesh.total_area()).abs() < 1e-9 * sum);
    // GIfTI geometry gives the same mesh.
    let gii = Gifti::read(common::data("surface/ico_ascii.gii")).unwrap();
    let from_gii = surface_to_core(&gii.to_surface().unwrap()).unwrap();
    assert_eq!(from_gii.topology().id(), mesh.topology().id());
    // A triangle that names a missing node is an error, not a panic.
    let mut bad = asc.clone();
    bad.faces[0][0] = 9999;
    assert!(surface_to_core(&bad).is_err());
}
