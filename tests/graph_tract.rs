//! Graph_Bucket and `.niml.tract` files written by AFNI, read through `afni-io` and
//! converted to `afni-core`'s `Graph` and `TractSet`.
//!
//! Graph files come from `ConvertDset -graphize` run on inputs whose contents are
//! known (`tests/data/make_graph_fixtures.sh`), so what a reader recovers can be
//! checked against what was put in. Tract files come from AFNI's own FATCAT writer
//! (`tests/data/make_tract_fixtures.sh`), in ASCII and binary, with the network as it
//! was built and the lengths `Tract_Length` reports.
//!
//! With `AFNI_IO_LIVE=1` the graphs we write are also handed back to AFNI
//! (`ConvertDset`), which must accept and re-save them unchanged in content.

mod common;

use std::path::PathBuf;

use afni_core::domain::flip_dicom_ras;
use afni_core::graph::{EdgeLayout, Graph};
use afni_io::adapt::{graph_from_core, graph_to_core, tracts_from_core, tracts_to_core};
use afni_io::graph::{GraphBucket, MatrixShape};
use afni_io::tract::TractNetwork;

fn graph(name: &str) -> GraphBucket {
    GraphBucket::read(common::data(&format!("graph/{name}.niml.dset"))).unwrap()
}

fn table(name: &str) -> Vec<Vec<f64>> {
    std::fs::read_to_string(common::data(&format!("graph/{name}.truth")))
        .unwrap()
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| l.split_whitespace().map(|v| v.parse().unwrap()).collect())
        .collect()
}

fn names() -> Vec<String> {
    std::fs::read_to_string(common::data("graph/nodes.names.truth"))
        .unwrap()
        .lines()
        .map(|l| l.split_whitespace().nth(1).unwrap().to_owned())
        .collect()
}

#[test]
fn full_graph_matches_the_matrix_that_went_in() {
    let raw = graph("full");
    assert_eq!(raw.shape, MatrixShape::Full);
    let input = table("full");
    assert_eq!(raw.measures.len(), 2);
    for (m, values) in raw.measures.iter().enumerate() {
        let want: Vec<f32> = input.iter().map(|r| r[m] as f32).collect();
        assert_eq!(values, &want, "measure {m}");
    }
    let g = graph_to_core(&raw).unwrap();
    assert_eq!(g.layout(), &EdgeLayout::Full);
    assert_eq!((g.node_count(), g.edge_count()), (4, 16));
    // The input stacked the matrix by columns: row i of the input is the element
    // (row = i % 4, column = i / 4), and the value there is column * 10 + row.
    for row in 0..4 {
        for col in 0..4 {
            assert_eq!(
                g.value(row, col, 0),
                Some((col * 10 + row) as f32),
                "({row},{col})"
            );
        }
    }
    let labels: Vec<&str> = g.nodes().iter().map(|n| n.label.as_str()).collect();
    assert_eq!(
        labels,
        names().iter().map(String::as_str).collect::<Vec<_>>()
    );
    // Coordinates were given in AFNI's own frame and are stored unchanged.
    let xyz = table("nodes.xyz");
    for (node, want) in g.nodes().iter().zip(&xyz) {
        assert_eq!(
            node.position,
            [want[0] as f32, want[1] as f32, want[2] as f32]
        );
    }
}

#[test]
fn lpi_coordinates_are_flipped_to_afnis_frame_exactly_as_core_flips_them() {
    // The same nodes were given to ConvertDset as LPI; AFNI wrote them as RAI.
    let g = graph_to_core(&graph("lpi")).unwrap();
    let xyz = table("nodes.xyz");
    for (node, want) in g.nodes().iter().zip(&xyz) {
        let input = [want[0] as f32, want[1] as f32, want[2] as f32];
        assert_eq!(node.position, flip_dicom_ras(input), "{}", node.label);
    }
    // And the RAS view of an AFNI-written graph is the LPI input itself.
    for (ras, want) in g.positions_ras().iter().zip(&xyz) {
        assert_eq!(*ras, [want[0] as f32, want[1] as f32, want[2] as f32]);
    }
}

#[test]
fn triangular_graph_uses_the_documented_packing() {
    let raw = graph("tri");
    assert_eq!(raw.shape, MatrixShape::Tri);
    let g = graph_to_core(&raw).unwrap();
    assert_eq!((g.node_count(), g.edge_count()), (4, 6));
    // AFNI's source comment: seg 0 -> (1,0), seg 1 -> (2,0), ... For inputs 1..=6:
    let want = [
        (1, 0, 1.0),
        (2, 0, 2.0),
        (3, 0, 3.0),
        (2, 1, 4.0),
        (3, 1, 5.0),
        (3, 2, 6.0),
    ];
    for (row, col, value) in want {
        assert_eq!(g.value(row, col, 0), Some(value));
        assert_eq!(g.value(col, row, 0), Some(value), "mirrored");
    }
    assert_eq!(g.value(2, 2, 0), None, "no diagonal");
    assert_eq!(
        g.matrix(0).unwrap().iter().filter(|c| c.is_some()).count(),
        12
    );
}

#[test]
fn sparse_graph_names_nodes_by_index() {
    let raw = graph("sparse");
    assert_eq!(raw.shape, MatrixShape::Sparse);
    assert_eq!(raw.edges.as_ref().unwrap().len(), 3);
    let g = graph_to_core(&raw).unwrap();
    // Nodes are numbered 5..=8; the edge list "5 6", "6 7", "8 5" refers to those
    // indices, which are positions 0-1, 1-2 and 3-0.
    assert_eq!(
        g.nodes().iter().map(|n| n.index).collect::<Vec<_>>(),
        vec![5, 6, 7, 8]
    );
    assert_eq!(g.edge_endpoints(0), Some((0, 1)));
    assert_eq!(g.edge_endpoints(1), Some((1, 2)));
    assert_eq!(g.edge_endpoints(2), Some((3, 0)));
    assert_eq!(g.value(3, 0, 0), Some(3.0));
    assert_eq!(g.value(3, 0, 1), Some(30.0));
    assert_eq!(g.value(0, 3, 0), None, "sparse keeps the listed direction");
    // Reading edge ends as POSITIONS (a plausible mistake) would give a different graph.
    assert_ne!(
        raw.edges.as_ref().unwrap()[0][1],
        g.edge_endpoints(0).unwrap().0 as i32
    );
}

#[test]
fn graph_files_round_trip_through_the_raw_model_and_core() {
    let dir = std::env::temp_dir().join(format!("afni_io_graph_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    for name in ["full", "lpi", "tri", "sparse"] {
        let raw = graph(name);
        // Raw: write (ASCII and binary) and read back.
        for binary in [false, true] {
            let path = dir.join(format!("{name}_{binary}.niml.dset"));
            if binary {
                raw.write_binary(&path).unwrap();
            } else {
                raw.write(&path).unwrap();
            }
            let again = GraphBucket::read(&path).unwrap();
            assert_eq!(again.measures, raw.measures, "{name} binary={binary}");
            assert_eq!(again.nodes, raw.nodes);
            assert_eq!(again.edges, raw.edges);
            assert_eq!(again.shape, raw.shape);
            assert_eq!(again.measure_labels, raw.measure_labels);
            // The history string and ids survive.
            assert_eq!(
                again.root_attrs.get("self_idcode"),
                raw.root_attrs.get("self_idcode")
            );
            assert!(again
                .extras
                .iter()
                .any(|e| e.attrs.get("atr_name").map(String::as_str) == Some("HISTORY_NOTE")));
        }
        // Core: to core and back (with the file as template) keeps every number.
        let core = graph_to_core(&raw).unwrap();
        let back = graph_from_core(&core, Some(&raw));
        assert_eq!(back.measures, raw.measures);
        assert_eq!(back.nodes, raw.nodes);
        assert_eq!(back.edges, raw.edges);
        assert_eq!(graph_to_core(&back).unwrap(), core);
        // Without a template the attributes are written fresh and the graph is the same.
        let fresh = graph_from_core(&core, None);
        let path = dir.join(format!("{name}_fresh.niml.dset"));
        fresh.write(&path).unwrap();
        assert_eq!(
            graph_to_core(&GraphBucket::read(&path).unwrap()).unwrap(),
            core,
            "{name}"
        );
    }
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn bad_graphs_are_refused() {
    // A sparse file with no edge list cannot become a graph.
    let mut raw = graph("sparse");
    raw.edges = None;
    assert!(graph_to_core(&raw).is_err());
    // An edge table that does not fit the node count is refused by core.
    let mut raw = graph("full");
    raw.measures.iter_mut().for_each(|m| m.truncate(15));
    assert!(graph_to_core(&raw).is_err());
}

#[test]
fn afni_accepts_the_graphs_we_write() {
    if std::env::var_os("AFNI_IO_LIVE").is_none() {
        eprintln!("skipping: set AFNI_IO_LIVE=1 to hand our graphs back to ConvertDset");
        return;
    }
    let dir = std::env::temp_dir().join(format!("afni_io_graph_live_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    for name in ["full", "tri", "sparse"] {
        let core: Graph = graph_to_core(&graph(name)).unwrap();
        let ours = dir.join(format!("{name}.niml.dset"));
        graph_from_core(&core, None).write(&ours).unwrap();
        // AFNI reads our file and writes it out again.
        let out = std::process::Command::new("ConvertDset")
            .current_dir(&dir)
            .env("AFNI_DONT_LOGFILE", "YES")
            .args([
                "-i",
                ours.file_name().unwrap().to_str().unwrap(),
                "-o_niml_asc",
                "-prefix",
            ])
            .arg(format!("{name}_resaved.niml.dset"))
            .output()
            .expect("ConvertDset must be on PATH for AFNI_IO_LIVE");
        let resaved: PathBuf = dir.join(format!("{name}_resaved.niml.dset"));
        assert!(
            resaved.exists(),
            "{name}: ConvertDset did not re-save our file: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let again = graph_to_core(&GraphBucket::read(&resaved).unwrap()).unwrap();
        assert_eq!(again.nodes(), core.nodes(), "{name}");
        assert_eq!(again.layout(), core.layout(), "{name}");
        for (a, b) in again.measures().iter().zip(core.measures()) {
            assert_eq!(a.values, b.values, "{name}");
        }
    }
    std::fs::remove_dir_all(dir).ok();
}

// ---------------------------------------------------------------------------
// Tracts
// ---------------------------------------------------------------------------

/// The network as AFNI built it: `(tag, alt tag, ends, [(id, points)])` per bundle.
type Truth = Vec<(i32, i32, Option<String>, Vec<(i32, Vec<[f32; 3]>)>)>;

fn tract_truth() -> Truth {
    let text = std::fs::read_to_string(common::data("tract/net.truth")).unwrap();
    let mut out: Truth = Vec::new();
    for line in text.lines() {
        let w: Vec<&str> = line.split_whitespace().collect();
        match w[0] {
            "bundle" => out.push((
                w[1].parse().unwrap(),
                w[2].parse().unwrap(),
                (w[3] != "-").then(|| w[3..].join(" ")),
                Vec::new(),
            )),
            "tract" => {
                let id: i32 = w[1].parse().unwrap();
                let n: usize = w[2].parse().unwrap();
                let v: Vec<f32> = w[3..].iter().map(|x| x.parse().unwrap()).collect();
                assert_eq!(v.len(), 3 * n);
                out.last_mut()
                    .unwrap()
                    .3
                    .push((id, v.chunks(3).map(|c| [c[0], c[1], c[2]]).collect()));
            }
            other => panic!("{other}"),
        }
    }
    out
}

fn lengths() -> Vec<(usize, usize, f64)> {
    std::fs::read_to_string(common::data("tract/net.lengths"))
        .unwrap()
        .lines()
        .map(|l| {
            let w: Vec<&str> = l.split_whitespace().collect();
            (
                w[0].parse().unwrap(),
                w[1].parse().unwrap(),
                w[2].parse().unwrap(),
            )
        })
        .collect()
}

#[test]
fn tract_files_from_afni_match_the_network_that_was_written() {
    let truth = tract_truth();
    for file in ["net_ascii", "net_bin"] {
        let raw = TractNetwork::read(common::data(&format!("tract/{file}.niml.tract"))).unwrap();
        assert_eq!(raw.bundles.len(), truth.len(), "{file}");
        for (b, (tag, alt, ends, tracts)) in raw.bundles.iter().zip(&truth) {
            assert_eq!(
                (b.tag, b.alt_tag),
                (Some(*tag), (*alt >= 0).then_some(*alt)),
                "{file}"
            );
            assert_eq!(&b.ends, ends, "{file}");
            assert_eq!(b.tracts.len(), tracts.len());
            for (t, (id, points)) in b.tracts.iter().zip(tracts) {
                assert_eq!(t.id, *id);
                assert_eq!(&t.points, points, "{file} tract {id}");
            }
        }
    }
}

#[test]
fn core_lengths_equal_afnis_tract_length() {
    for file in ["net_ascii", "net_bin"] {
        let raw = TractNetwork::read(common::data(&format!("tract/{file}.niml.tract"))).unwrap();
        let set = tracts_to_core(&raw).unwrap();
        for (b, t, want) in lengths() {
            let ours = set.bundles[b].tracts[t].length();
            // Tract_Length works in 32-bit float.
            assert!(
                (ours - want).abs() < 1e-5 * want.max(1.0),
                "{file} bundle {b} tract {t}: {ours} vs {want}"
            );
        }
        assert_eq!(set.tract_count(), 6);
        // A selection by length keeps the right tracts: the lengths are 3.57, 11.74,
        // 22.44, 7.61, 17.08 and 0.
        let long = set
            .with_length(&afni_core::threshold::Threshold::Above(10.0))
            .unwrap()
            .without_empty_bundles();
        assert_eq!(long.tract_count(), 3);
    }
}

#[test]
fn tract_files_round_trip_through_core_and_the_raw_model() {
    let dir = std::env::temp_dir().join(format!("afni_io_tract_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    for file in ["net_ascii", "net_bin"] {
        let raw = TractNetwork::read(common::data(&format!("tract/{file}.niml.tract"))).unwrap();
        let core = tracts_to_core(&raw).unwrap();
        let back = tracts_from_core(&core, Some(&raw));
        assert_eq!(
            back, raw,
            "{file}: core round trip with the file as template"
        );
        for binary in [false, true] {
            let path = dir.join(format!("{file}_{binary}.niml.tract"));
            if binary {
                back.write_binary(&path).unwrap();
            } else {
                back.write(&path).unwrap();
            }
            assert_eq!(
                TractNetwork::read(&path).unwrap(),
                raw,
                "{file} binary={binary}"
            );
        }
        // Without a template the tracts are the same.
        assert_eq!(
            tracts_to_core(&tracts_from_core(&core, None)).unwrap(),
            core
        );
    }
    // The ASCII and binary files AFNI wrote describe the same network.
    let a = TractNetwork::read(common::data("tract/net_ascii.niml.tract")).unwrap();
    let b = TractNetwork::read(common::data("tract/net_bin.niml.tract")).unwrap();
    assert_eq!(a.bundles, b.bundles);
    std::fs::remove_dir_all(dir).ok();
}
