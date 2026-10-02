//! Baseline: what the current readers get right on the committed fixtures,
//! checked against AFNI's own text dumps of the same files.
//!
//! Tests marked `#[ignore = "Phase N: ..."]` are known gaps from the roadmap in
//! `afni-io_ROADMAP.md`. Run them with `cargo test -- --ignored`; un-ignore
//! each one as its phase lands.

mod common;

use afni_io::prelude::*;

/// Compare every voxel of an AFNI dataset with its `3dmaskdump` reference
/// (`i j k v0 v1 ...`).
fn assert_brik_matches_dump(dataset: &str, tolerance: f32) {
    let brik = Brik::read(common::data(&format!("volume/{dataset}.HEAD"))).unwrap();
    let rows = common::read_numeric_dump(&format!("volume/{dataset}.dump.txt"));
    assert_eq!(rows.len(), brik.voxels(), "{dataset}: voxel count");
    for row in rows {
        let [i, j, k] = [row[0] as usize, row[1] as usize, row[2] as usize];
        for (p, &expected) in row[3..].iter().enumerate() {
            let actual = brik.value(i, j, k, p).unwrap();
            assert!(
                (actual - expected as f32).abs() <= tolerance,
                "{dataset}[{p}] at ({i},{j},{k}): got {actual}, AFNI says {expected}"
            );
        }
    }
}

#[test]
fn brik_scalar_datums_match_3dmaskdump() {
    for dataset in ["s16+orig", "u8+orig", "f32+orig", "mixed+orig", "stat+orig"] {
        assert_brik_matches_dump(dataset, 0.0);
    }
    // 3dmaskdump prints scaled shorts with 6 significant digits.
    assert_brik_matches_dump("s16scaled+orig", 1e-5);
}

#[test]
fn brik_byte_order_compression_and_views_match_3dmaskdump() {
    for dataset in [
        "s16_msb+orig",
        "s16gz+orig",
        "timeseries+orig",
        "lpi+orig",
        "oblique+orig",
        "u8tlrc+tlrc",
    ] {
        assert_brik_matches_dump(dataset, 0.0);
    }
}

#[test]
fn nifti_values_match_source_dataset_dump() {
    for (nii, source) in [
        ("s16.nii.gz", "s16+orig"),
        ("stat.nii", "stat+orig"),
        ("stat_pure.nii", "stat+orig"),
        ("lpi.nii", "lpi+orig"),
    ] {
        let vol = Nifti::read(common::data(&format!("volume/{nii}"))).unwrap();
        for row in common::read_numeric_dump(&format!("volume/{source}.dump.txt")) {
            let [i, j, k] = [row[0] as usize, row[1] as usize, row[2] as usize];
            for (t, &expected) in row[3..].iter().enumerate() {
                let actual = vol.voxel(i, j, k, t).unwrap();
                assert_eq!(actual, expected, "{nii}[{t}] at ({i},{j},{k})");
            }
        }
    }
}

#[test]
fn brick_labels_match_3dinfo() {
    // `3dinfo -label`: "Tstat#0|Fstat*with*tildes" and "#0|#0".
    let stat = Header::read(common::data("volume/stat+orig.HEAD")).unwrap();
    assert_eq!(stat.brick_labels(), ["Tstat#0", "Fstat*with*tildes"]);
    let mixed = Header::read(common::data("volume/mixed+orig.HEAD")).unwrap();
    assert_eq!(mixed.brick_labels(), ["#0", "#0"]);
}

#[test]
fn brik_rgb_keeps_channels() {
    // make_volume_fixtures.sh: r = 60i, g = 50j, b = 40k.
    let brik = Brik::read(common::data("volume/rgb+orig.HEAD")).unwrap();
    let sub = brik.sub_brick(0).unwrap();
    let BrickData::Rgb(rgb) = &sub.data else {
        panic!("expected RGB, got {:?}", sub.brik_type());
    };
    for (k, j, i) in (0..6).flat_map(|k| (0..5).flat_map(move |j| (0..4).map(move |i| (k, j, i)))) {
        let index = brik.voxel_index(i, j, k).unwrap();
        assert_eq!(
            rgb[index],
            [60 * i as u8, 50 * j as u8, 40 * k as u8],
            "({i},{j},{k})"
        );
    }
    assert_eq!(brik.value(1, 1, 1, 0), None, "RGB has no scalar value");
}

#[test]
fn brik_every_spelling_and_sub_brick_selection() {
    let full = Brik::read(common::data("volume/s16+orig.HEAD")).unwrap();
    let base = common::data("volume/s16+orig.HEAD").display().to_string();
    let base = base.trim_end_matches(".HEAD");
    for name in [format!("{base}.BRIK"), base.to_string(), format!("{base}.")] {
        let brik = Brik::read(&name).unwrap();
        assert_eq!(brik.sub_bricks, full.sub_bricks, "{name}");
    }
    let gz = common::data("volume/s16gz+orig.BRIK.gz");
    let only_last = Brik::read_sub_bricks(&gz, &[2]).unwrap();
    assert!(only_last.sub_brick(0).is_none());
    assert_eq!(only_last.sub_brick(2), full.sub_brick(2));
}

#[test]
fn surface_encodings_agree_with_1d_coords() {
    let reference = OneD::read(common::data("surface/ico.1D.coord")).unwrap();
    let asc = Surface::read(common::data("surface/ico.asc")).unwrap();
    assert_eq!(asc.n_vertices(), 42);
    assert_eq!(asc.n_faces(), 80);
    for (n, vertex) in asc.vertices.iter().enumerate() {
        for (axis, &actual) in vertex.iter().enumerate() {
            let expected = reference.get(n, axis).unwrap() as f32;
            assert!((actual - expected).abs() < 1e-5, "asc node {n}");
        }
    }
    // SUMA holds surfaces in RAI and always writes GIfTI as RAS, negating x
    // and y (`flip_float_triples`, suma_gifti.c). The .asc here is SUMA's own
    // RAI output: SUMA only converts FreeSurfer RAS to RAI when aligning to a
    // surface volume, and skips spheres (`SUMA_Align_to_VolPar`). So for this
    // icosahedron the GIfTI is the .asc with x and y negated. afni-io returns
    // each file's coordinates as stored; see the `geometry` module docs.
    for encoding in ["ascii", "b64", "b64gz"] {
        let gii = Gifti::read(common::data(&format!("surface/ico_{encoding}.gii"))).unwrap();
        let mesh = gii.to_surface().unwrap();
        let rai: Vec<[f32; 3]> = mesh.vertices.iter().map(|&[x, y, z]| [-x, -y, z]).collect();
        assert_eq!(rai, asc.vertices, "{encoding}: vertices (x, y negated)");
        assert_eq!(mesh.faces, asc.faces, "{encoding}: faces");
    }
}

#[test]
fn spec_files_list_their_surfaces() {
    let spec = Spec::read(common::data("surface/ico_states.spec")).unwrap();
    let names: Vec<_> = spec
        .surfaces
        .iter()
        .filter_map(|s| s.surface_name())
        .collect();
    assert_eq!(names, ["ico.asc", "ico_mirror.asc"]);
    assert_eq!(spec.surfaces[1].anatomical(), Some(false));

    let real = Spec::read(common::data("real/spec/std.141.sub-3_both.spec")).unwrap();
    assert_eq!(real.surfaces.len(), 14);
}

#[test]
fn stat_niml_dset_matches_convertdset_dump() {
    let dset = NimlDataset::read(common::data("surface/stat.niml.dset")).unwrap();
    let stats: Vec<_> = dset
        .column_stats()
        .iter()
        .map(|s| s.as_ref().unwrap().to_statsym())
        .collect();
    assert_eq!(stats, ["Ttest(10)", "Ftest(2,30)"]);
    assert_eq!(dset.column_labels(), ["T#0", "F#1"]);
    let rows = common::read_numeric_dump("surface/stat.niml.dset.dump.txt");
    assert_eq!(rows.len(), dset.rows());
    for (r, row) in rows.iter().enumerate() {
        // Dump columns: node index, then one per data column.
        let node = dset.node_indices.as_ref().map_or(r as u32, |idx| idx[r]);
        assert_eq!(node as f64, row[0]);
        for c in 0..dset.column_count() {
            assert!((dset.data.get(r, c).unwrap() - row[c + 1]).abs() < 1e-6);
        }
    }
}

#[test]
fn convertdset_niml_dsets_match_their_dumps() {
    // ConvertDset writes valueless attributes (`domain_parent_idcode`).
    for name in ["dense_asc", "dense_bi", "sparse_asc", "sparse_bi"] {
        let dset = NimlDataset::read(common::data(&format!("surface/{name}.niml.dset"))).unwrap();
        let rows = common::read_numeric_dump(&format!("surface/{name}.niml.dset.dump.txt"));
        assert_eq!(rows.len(), dset.rows(), "{name}");
        for (r, row) in rows.iter().enumerate() {
            let node = dset.node_indices.as_ref().map_or(r as u32, |idx| idx[r]);
            assert_eq!(node as f64, row[0], "{name} row {r}");
            for c in 0..dset.column_count() {
                assert_eq!(dset.data.get(r, c).unwrap(), row[c + 1], "{name} [{r},{c}]");
            }
        }
    }
}

#[test]
fn real_rois_and_label_tables_parse() {
    let roi_dir = common::data("real/roi");
    let mut count = 0;
    for entry in std::fs::read_dir(roi_dir).unwrap() {
        let path = entry.unwrap().path();
        let rois = NodeRoi::read_all(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        assert!(!rois.is_empty(), "{}: no ROIs", path.display());
        count += 1;
    }
    assert!(count >= 17);

    for name in [
        "aparc+aseg_REN_all.niml.lt",
        "aparc.a2009s+aseg_REN_all.niml.lt",
    ] {
        let elements = afni_io::niml::read(common::data(&format!("real/labels/{name}"))).unwrap();
        assert_eq!(elements.len(), 1, "{name}");
    }

    let clusters = NimlDataset::read(common::data(
        "real/dset/test_lh_full_sumaru_clusters.niml.dset",
    ))
    .unwrap();
    assert!(clusters.is_sparse());
    // Written by sumaru before it quoted String bodies: the unquoted,
    // multi-line history must still read whole.
    let history = clusters.history().unwrap();
    assert!(history.starts_with("SurfClust -i "), "{history}");
    assert!(history.contains("-out_fulllist"), "{history}");
    assert_eq!(clusters.column_labels(), ["Cluster"]);
}
