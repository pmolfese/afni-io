//! Surface datasets and ROIs against AFNI-written files: label tables, FDR
//! curves, time series, round trips, and the ROI code enums.

mod common;

use std::collections::BTreeMap;

use afni_io::prelude::*;

fn dset(name: &str) -> NimlDataset {
    NimlDataset::read(common::data(&format!("surface/{name}"))).unwrap()
}

#[test]
fn label_dataset_carries_its_table() {
    // make_surface_fixtures.sh: ConvertDset -labelize toylut.niml.cmap of
    // node labels n % 4 + 1.
    let labels = dset("labels.niml.dset");
    assert_eq!(labels.dset_type, "Node_Label");
    let table = labels.label_table.as_ref().expect("AFNI_labeltable");
    let names: Vec<_> = table
        .entries
        .iter()
        .map(|e| (e.key, e.name.as_str()))
        .collect();
    // MakeColorMap adds key 0 "undefined".
    assert_eq!(
        names,
        [
            (0, "undefined"),
            (1, "Big_House"),
            (2, "Small_Face"),
            (3, "Electric"),
            (4, "Atomic")
        ]
    );
    // Colours are stored as 8-bit fractions: 0.3 -> 76/255.
    let rgba = table.get(1).unwrap().rgba.unwrap();
    assert!((rgba[0] - 76.0 / 255.0).abs() < 1e-6, "{rgba:?}");

    // Node 5 holds 5 % 4 + 1 = 2.
    let key = labels.data.get(5, 0).unwrap() as i64;
    assert_eq!(table.get(key).unwrap().name, "Small_Face");

    // The same table read from the .niml.cmap file on its own.
    let cmap = LabelTable::read(common::data("surface/toylut.niml.cmap")).unwrap();
    assert_eq!(cmap.entries, table.entries);

    // ConvertDset writes two COLMS_LABS entries for the one column; the
    // dataset's own list is read (3dinfo -label shows the label table's "R").
    assert_eq!(labels.column_labels(), ["numeric"]);
    assert_eq!(labels.column_types(), ["Node_Index_Label"]);
}

#[test]
fn volume_label_table_matches_3dinfo() {
    // 3drefit -labeltable aparc+aseg_REN_all.niml.lt labelled+orig
    let head = Header::read(common::data("volume/labelled+orig.HEAD")).unwrap();
    let table = head
        .value_label_table()
        .unwrap()
        .expect("VALUE_LABEL_DTABLE");
    let source = LabelTable::read(common::data("real/labels/aparc+aseg_REN_all.niml.lt")).unwrap();
    assert_eq!(table.entries, source.entries);
    assert_eq!(table.entries.len(), 118);

    // 3dinfo -labeltable lists the same pairs (in hash order).
    let text =
        std::fs::read_to_string(common::data("volume/labelled+orig.labeltable.txt")).unwrap();
    let from_3dinfo = LabelTable::find(&afni_io::niml::parse_str(&text).unwrap())
        .unwrap()
        .unwrap();
    let as_map = |t: &LabelTable| -> BTreeMap<i64, String> {
        t.entries.iter().map(|e| (e.key, e.name.clone())).collect()
    };
    assert_eq!(as_map(&from_3dinfo), as_map(&table));

    // It travels in the NIfTI's AFNI extension too.
    let nii = Nifti::read(common::data("volume/labelled.nii")).unwrap();
    let from_nifti = nii
        .afni_header()
        .unwrap()
        .unwrap()
        .value_label_table()
        .unwrap()
        .unwrap();
    assert_eq!(from_nifti.entries, table.entries);

    // Writing a table and reading it back.
    let mut copy = head.clone();
    copy.set_value_label_table(&source);
    assert_eq!(
        copy.value_label_table().unwrap().unwrap().entries,
        source.entries
    );
}

#[test]
fn fdr_curves_time_steps_and_stats() {
    // 3drefit -addFDR on the Ttest/Ftest dataset.
    let fdr = dset("fdr.niml.dset");
    for c in 0..2 {
        let curve = fdr.attributes.fdr_curve(c).expect("FDRCURVE");
        assert!(curve.values.len() > 2 && curve.dx > 0.0, "{curve:?}");
    }
    let stats: Vec<_> = fdr
        .column_stats()
        .iter()
        .map(|s| s.as_ref().unwrap().to_statsym())
        .collect();
    assert_eq!(stats, ["Ttest(10)", "Ftest(2,30)"]);

    // 3drefit -TR 2.5: v = n + c in 3 columns.
    let ts = dset("timeseries.niml.dset");
    assert_eq!(ts.time_step, Some(2.5));
    assert_eq!(ts.column_count(), 3);
    assert_eq!(ts.data.get(7, 2), Some(9.0));
    let info =
        std::fs::read_to_string(common::data("surface/timeseries.niml.dset.info.txt")).unwrap();
    assert!(info.trim().ends_with("2.500000"), "{info}");
}

#[test]
fn datasets_round_trip_with_everything_they_carry() {
    let names = [
        "labels.niml.dset",
        "fdr.niml.dset",
        "timeseries.niml.dset",
        "stat.niml.dset",
        "dense_bi.niml.dset",
        "sparse_asc.niml.dset",
    ];
    let dir = std::env::temp_dir().join(format!("afni_io_dset_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let mut paths: Vec<_> = names
        .iter()
        .map(|n| common::data(&format!("surface/{n}")))
        .collect();
    paths.push(common::data(
        "real/dset/test_lh_full_sumaru_clusters.niml.dset",
    ));
    for path in paths {
        let original = NimlDataset::read(&path).unwrap();
        for binary in [false, true] {
            let out = dir.join("out.niml.dset");
            if binary {
                original.write_binary(&out).unwrap();
            } else {
                original.write(&out).unwrap();
            }
            let back = NimlDataset::read(&out).unwrap();
            let what = format!("{} (binary: {binary})", path.display());
            assert_eq!(back.data, original.data, "{what}");
            assert_eq!(back.node_indices, original.node_indices, "{what}");
            assert_eq!(back.time_step, original.time_step, "{what}");
            assert_eq!(back.label_table, original.label_table, "{what}");
            match &original.self_idcode {
                Some(id) => assert_eq!(back.self_idcode.as_ref(), Some(id), "{what}"),
                // sumaru's cluster dset has none: one is generated on write.
                None => assert!(
                    back.self_idcode.as_deref().unwrap().starts_with("AFN_"),
                    "{what}"
                ),
            }
            assert_eq!(
                back.domain_parent_idcode, original.domain_parent_idcode,
                "{what}"
            );
            assert_eq!(back.column_labels(), original.column_labels(), "{what}");
            assert_eq!(back.column_stats(), original.column_stats(), "{what}");
            // Every attribute survives; COLMS_RANGE is recomputed and must
            // agree with the one AFNI (or sumaru) wrote.
            for attr in &original.attributes.attributes {
                if attr.name != "COLMS_RANGE" {
                    assert_eq!(
                        back.attributes.get(&attr.name),
                        Some(&attr.value),
                        "{what}: {}",
                        attr.name
                    );
                }
            }
            for (ours, theirs) in back.column_ranges().iter().zip(original.column_ranges()) {
                let (Some(ours), Some(theirs)) = (ours, theirs) else {
                    continue;
                };
                assert!(
                    (ours.min - theirs.min).abs() < 1e-4 && (ours.max - theirs.max).abs() < 1e-4,
                    "{what}: {ours:?} vs {theirs:?}"
                );
            }
        }
    }
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn roi_codes_have_typed_meanings() {
    let read = |name: &str| NodeRoi::read_all(common::data(&format!("real/roi/{name}"))).unwrap();
    // SUMA's demo ROIs: open path, joined (closed), filled.
    for (file, kind) in [
        ("demo.lh.1.nojoin.niml.roi", RoiDrawingType::OpenPath),
        ("demo.lh.2.join.niml.roi", RoiDrawingType::ClosedPath),
        ("demo.lh.3.filled.niml.roi", RoiDrawingType::FilledArea),
    ] {
        let roi = &read(file)[0];
        assert_eq!(roi.drawing_type(), Some(kind), "{file}");
        assert_eq!(roi.side(), Some(Side::Left), "{file}");
        assert!(roi
            .data
            .iter()
            .all(|d| d.action_kind().is_some() && d.element_kind().is_some()));
    }
    let filled = &read("demo.lh.3.filled.niml.roi")[0];
    assert!(filled
        .data
        .iter()
        .any(|d| d.action_kind() == Some(BrushAction::FillArea)));

    // sumaru's cluster ROIs say Type="4", which SUMA does not define
    // (Collection is 3): no typed meaning, but the code round-trips.
    let clusters = read("test_lh_sumaru_clusters.niml.roi");
    assert_eq!(clusters[0].roi_type, Some(4));
    assert_eq!(clusters[0].drawing_type(), None);
    let back = NodeRoi::from_bytes(clusters[0].to_niml_string().as_bytes()).unwrap();
    assert_eq!(back[0].roi_type, Some(4));
}

#[test]
fn reference_annotation_label_dataset() {
    let Some(path) = common::reference("SUMA/lh.aparc.a2009s.annot.niml.dset") else {
        return;
    };
    let annot = NimlDataset::read(path).unwrap();
    assert_eq!(annot.dset_type, "Node_Label");
    assert_eq!(annot.rows(), 134_631);
    let table = annot.label_table.as_ref().unwrap();
    assert_eq!(table.entries.len(), 100);
    assert!(table
        .attrs
        .get("Name")
        .unwrap()
        .starts_with("FreeSurferColorLUT"));
    // Every node's value is a key in the table.
    for row in (0..annot.rows()).step_by(997) {
        let key = annot.data.get(row, 0).unwrap() as i64;
        assert!(table.get(key).is_some(), "row {row}: key {key}");
    }
}
