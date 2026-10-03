//! ROIs: the core ROI model against AFNI's own `ROI2dataset`.
//!
//! `tests/data/roi_dataset/` holds what `ROI2dataset` wrote for real `.niml.roi`
//! files (regenerate with `tests/data/make_roi_dataset_fixtures.sh`). Each file is
//! read here, converted with `adapt::roi_to_core`, and fed through
//! `afni_core::roi::rois_to_dataset`; the rows must match AFNI's exactly, including
//! which ROI keeps a node that two ROIs share and what padding does. For single
//! files, the drawn order of nodes is compared with `ROI2dataset -nodelist`.
//!
//! The same ROI files are also round-tripped through the adapter: file to core and
//! back must give the identical `NodeRoi`, including codes SUMA does not define.

mod common;

use std::collections::BTreeMap;

use afni_core::domain::SurfaceDomain;
use afni_core::roi::{rois_to_dataset, RoiDatasetOptions};
use afni_io::adapt::{roi_from_core, roi_to_core};
use afni_io::prelude::*;

/// One line of `cases.txt`.
struct Case {
    name: String,
    files: Vec<String>,
    pad_to: Option<u32>,
    pad_label: i32,
}

fn cases() -> Vec<Case> {
    let text = std::fs::read_to_string(common::data("roi_dataset/cases.txt")).unwrap();
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .map(|line| {
            let f: Vec<&str> = line.split(" | ").collect();
            let opts: Vec<&str> = f[2].split_whitespace().collect();
            let after = |flag: &str| {
                opts.iter()
                    .position(|o| *o == flag)
                    .map(|i| opts[i + 1].parse::<i64>().unwrap())
            };
            Case {
                name: f[0].to_owned(),
                files: f[1].split(',').map(str::to_owned).collect(),
                pad_to: after("-pad_to_node").map(|v| v as u32),
                pad_label: after("-pad_label").unwrap_or(0) as i32,
            }
        })
        .collect()
}

fn read_rois(file: &str) -> Vec<NodeRoi> {
    // Derived inputs (relabelled copies) live beside the expected outputs.
    let derived = format!("roi_dataset/inputs/{file}");
    let path = if common::data_exists(&derived) {
        common::data(&derived)
    } else {
        common::data(&format!("real/roi/{file}"))
    };
    NodeRoi::read_all(path).unwrap()
}

/// `(node, label)` rows of a `.dset` fixture.
fn read_rows(name: &str) -> Vec<(u32, i32)> {
    std::fs::read_to_string(common::data(&format!("roi_dataset/{name}.dset")))
        .unwrap()
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| {
            let mut it = l.split_whitespace();
            (
                it.next().unwrap().parse().unwrap(),
                it.next().unwrap().parse().unwrap(),
            )
        })
        .collect()
}

#[test]
fn roi_datasets_match_roi2dataset() {
    let mut compared = 0;
    // Nodes claimed by two ROIs with DIFFERENT labels: only these show whether the
    // right ROI wins, so make sure the cases really contain some.
    let mut contested = 0;
    for case in cases() {
        // Every file of a case becomes core ROIs, in file order.
        let rois: Vec<_> = case
            .files
            .iter()
            .flat_map(|f| read_rois(f))
            .map(|r| roi_to_core(&r).unwrap().core)
            .collect();
        for (i, a) in rois.iter().enumerate() {
            for b in &rois[i + 1..] {
                if a.integer_label != b.integer_label {
                    contested += a.node_set().intersection(&b.node_set()).len();
                }
            }
        }
        let max_node = rois
            .iter()
            .filter_map(|r| r.node_range().map(|(_, hi)| hi))
            .max()
            .unwrap();
        // The domain only needs to be big enough; ROI files do not say how big a
        // surface is.
        let size = case.pad_to.unwrap_or(max_node) as usize + 1;
        // Use the ROIs' own parent domain, so the domain check is exercised too.
        let parent = rois.iter().find_map(|r| r.parent_domain.clone());
        let domain = SurfaceDomain::new(parent, size).unwrap();
        let options = RoiDatasetOptions {
            pad_to: case.pad_to,
            pad_label: case.pad_label,
            ..Default::default()
        };
        let ds = rois_to_dataset(&rois, &domain, &options).unwrap();
        let labels = match ds.columns()[0].values() {
            afni_core::column::ColumnData::Int32(v) => v.clone(),
            _ => panic!("labels are Int32"),
        };
        let rows: Vec<(u32, i32)> = (0..ds.row_count())
            .map(|r| (ds.sample_for_row(r).unwrap(), labels[r]))
            .collect();

        let expected = read_rows(&case.name);
        let contested = case.name.starts_with("contested");
        if contested {
            // SUMA sorts the nodes with the C library's `qsort`, which does not keep
            // equal keys in order, so WHICH label wins a shared node is platform
            // luck. The node set and every uncontested label must still agree, and
            // a contested node must carry one of the labels that claimed it.
            let claims = |node: u32| -> Vec<i32> {
                rois.iter()
                    .filter(|r| r.node_set().contains(node))
                    .map(|r| r.integer_label)
                    .collect()
            };
            assert_eq!(
                rows.iter().map(|r| r.0).collect::<Vec<_>>(),
                expected.iter().map(|r| r.0).collect::<Vec<_>>(),
                "{}: node set",
                case.name
            );
            for (&(node, ours), &(_, theirs)) in rows.iter().zip(&expected) {
                let labels = claims(node);
                assert!(labels.contains(&theirs), "{}: node {node}", case.name);
                if labels.iter().all(|&l| l == labels[0]) {
                    assert_eq!(ours, theirs, "{}: uncontested node {node}", case.name);
                }
            }
            // Ours is the documented deterministic choice: the first ROI's label.
            assert!(
                rows.iter().all(|&(n, l)| l == claims(n)[0]),
                "{}",
                case.name
            );
        } else if case.pad_to.is_some() {
            // The fixture keeps only ROI rows; the row count is stored separately.
            let want_rows: usize =
                std::fs::read_to_string(common::data(&format!("roi_dataset/{}.pad", case.name)))
                    .unwrap()
                    .trim()
                    .strip_prefix("rows ")
                    .unwrap()
                    .parse()
                    .unwrap();
            assert_eq!(rows.len(), want_rows, "{}: padded row count", case.name);
            let roi_rows: Vec<_> = rows
                .iter()
                .copied()
                .filter(|&(_, l)| l != case.pad_label)
                .collect();
            assert_eq!(roi_rows, expected, "{}: ROI rows", case.name);
            // Padded rows are nodes 0..=pad_to in order.
            assert!(rows.iter().enumerate().all(|(i, &(n, _))| n as usize == i));
        } else {
            assert_eq!(rows, expected, "{}", case.name);
        }
        compared += rows.len();
    }
    assert!(compared > 5000, "only {compared} rows compared");
    assert!(
        contested > 20,
        "only {contested} contested nodes: the overlap cases prove little"
    );
}

/// Read a `.nodes`/`.nodups` fixture: label -> nodes.
fn read_lists(name: &str, kind: &str) -> BTreeMap<i32, Vec<u32>> {
    let text =
        std::fs::read_to_string(common::data(&format!("roi_dataset/{name}.{kind}"))).unwrap();
    let mut lists: BTreeMap<i32, Vec<u32>> = BTreeMap::new();
    let mut label = None;
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        match line.strip_prefix("label ") {
            Some(l) => {
                label = Some(l.trim().parse::<i32>().unwrap());
                lists.entry(label.unwrap()).or_default();
            }
            None => lists
                .get_mut(&label.unwrap())
                .unwrap()
                .push(line.trim().parse().unwrap()),
        }
    }
    lists
}

#[test]
fn drawn_node_order_matches_roi2dataset_nodelist() {
    let mut checked = 0;
    for case in cases() {
        if case.files.len() != 1 || case.pad_to.is_some() {
            continue;
        }
        // ROIs that share an integer label are combined by `ROI2dataset`, so combine
        // ours the same way: concatenate per label, then drop repeats for `.nodups`.
        let mut all: BTreeMap<i32, Vec<u32>> = BTreeMap::new();
        for raw in read_rois(&case.files[0]) {
            let core = roi_to_core(&raw).unwrap().core;
            all.entry(core.integer_label)
                .or_default()
                .extend(core.drawn_nodes());
        }
        assert_eq!(
            all,
            read_lists(&case.name, "nodes"),
            "{} (-nodelist)",
            case.name
        );
        let unique: BTreeMap<i32, Vec<u32>> = all
            .iter()
            .map(|(&label, nodes)| {
                let mut seen = std::collections::BTreeSet::new();
                (
                    label,
                    nodes.iter().copied().filter(|&n| seen.insert(n)).collect(),
                )
            })
            .collect();
        assert_eq!(
            unique,
            read_lists(&case.name, "nodups"),
            "{} (.nodups)",
            case.name
        );
        checked += 1;
    }
    assert!(checked >= 10, "only {checked} node orders compared");
}

#[test]
fn every_roi_file_round_trips_through_core() {
    let dir = common::data("real/roi");
    let mut files = 0;
    let mut rois = 0;
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if !path.to_string_lossy().ends_with(".niml.roi") {
            continue;
        }
        files += 1;
        for raw in NodeRoi::read_all(&path).unwrap() {
            let envelope = roi_to_core(&raw).unwrap();
            assert_eq!(envelope.to_node_roi(), raw, "{}", path.display());
            // The text written from the round-tripped ROI parses back to the same ROI.
            let again =
                NodeRoi::from_bytes(envelope.to_node_roi().to_niml_string().as_bytes()).unwrap();
            assert_eq!(again[0], raw, "{}", path.display());
            rois += 1;
        }
    }
    assert!(files >= 16 && rois >= 20, "{files} files, {rois} ROIs");
}

#[test]
fn unknown_codes_and_absent_attributes_survive() {
    // sumaru's cluster ROIs say Type="4", which SUMA does not define.
    let raw = &read_rois("test_lh_sumaru_clusters.niml.roi")[0];
    let env = roi_to_core(raw).unwrap();
    assert_eq!(
        env.core.drawing_type,
        afni_core::roi::RoiDrawingType::Other(4)
    );
    assert_eq!(env.to_node_roi().roi_type, Some(4));

    // Typed meanings where SUMA defines them.
    let join = &read_rois("demo.lh.2.join.niml.roi")[0];
    let core = roi_to_core(join).unwrap().core;
    assert_eq!(
        core.drawing_type,
        afni_core::roi::RoiDrawingType::ClosedPath
    );
    assert_eq!(core.parent_side, Some(afni_core::roi::RoiSide::Left));

    // A hand-built ROI with an odd stroke code, no color, no type and padded ids.
    let odd = NodeRoi {
        self_idcode: Some("  id-1 ".into()),
        domain_parent_idcode: Some("dom".into()),
        parent_side: Some("sideways".into()),
        label: "odd".into(),
        integer_label: 5,
        roi_type: None,
        color_plane: None,
        fill_color: None,
        edge_color: None,
        edge_thickness: None,
        data: vec![RoiDatum {
            action: 99,
            element_type: 0,
            nodes: vec![3, 1, 2],
        }],
    };
    let env = roi_to_core(&odd).unwrap();
    assert!(env.extras.fill_color_absent && env.extras.type_absent);
    assert_eq!(env.core.strokes[0].action.code(), 99);
    assert_eq!(env.core.parent_side.as_ref().unwrap().name(), "sideways");
    assert_eq!(env.to_node_roi(), odd);
    // Without the envelope every attribute is written, and the codes still survive.
    let full = roi_from_core(&env.core);
    assert_eq!(full.data, odd.data);
    assert!(full.fill_color.is_some() && full.roi_type.is_some());
}
