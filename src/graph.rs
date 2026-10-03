// PUBLIC DOMAIN NOTICE
//
// This file is part of afni-io, which was written by employees of the United
// States Government (National Institutes of Health) as part of their official
// duties. It is a "United States Government Work" (17 U.S.C. 105) and is in the
// public domain; outside the US, rights are waived under CC0 1.0. See LICENSE.
//
// ---------------------------------------------------------------------------
// WHAT THIS FILE IS
//
// Reading and writing AFNI/SUMA `Graph_Bucket` datasets: the NIML files that
// `ConvertDset -graphize`, `3dNetCorr` and `3dTrackID` write for networks (a matrix of
// edge measures and the nodes' coordinates and labels). This module is the FILE
// layer: it keeps what the file says and nothing more. What the numbers mean (edge
// layouts, matrix views, thresholds) lives in `afni_core::graph`; the conversion is in
// `adapt::{graph_to_core, graph_from_core}`.
//
// THE FILE (all parts are children of an `<AFNI_dataset dset_type="Graph_Bucket">`)
//
//   SPARSE_DATA    one row per edge, one numeric column per measure; its attributes
//                  `matrix_shape` (`full`, `tri`, `tri_diag` or `sparse`) and
//                  `matrix_size` say how the rows map to a matrix
//   INDEX_LIST     (sparse graphs) three integers per edge: its id and the INDEX of
//                  each end node
//   NODE_COORDS    one row per node: index, x, y, z (AFNI DICOM frame), label
//   AFNI_atr ...   column labels (`COLMS_LABS`), types, ranges, history, ...
//   network_link   an optional link to a `.niml.tract` file
//
// Everything this module does not interpret is kept (`extras`, `*_attrs`) and written
// back, so reading and writing a file keeps its history, ranges and links. Column
// ranges are dropped when a graph is rewritten from different numbers (they would be
// stale); AFNI recomputes them.
// ---------------------------------------------------------------------------

//! `Graph_Bucket` NIML datasets: the raw file model.

use std::collections::BTreeMap;
use std::path::Path;

use crate::array::TypedArray;
use crate::error::{Error, Result};
use crate::niml::{
    self, MixedTable, NimlData, NimlElement, NimlValue, NimlValueType, NumericMatrix,
};

/// The file's `matrix_shape`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MatrixShape {
    /// `full`: an n x n matrix, column-major.
    Full,
    /// `tri`: the strict lower triangle.
    Tri,
    /// `tri_diag`: the lower triangle with the diagonal.
    TriDiag,
    /// `sparse` (or no attribute): an explicit edge list in `INDEX_LIST`.
    Sparse,
}

impl MatrixShape {
    /// The attribute value to write.
    pub fn name(&self) -> &'static str {
        match self {
            Self::Full => "full",
            Self::Tri => "tri",
            Self::TriDiag => "tri_diag",
            Self::Sparse => "sparse",
        }
    }

    /// The shape for a `matrix_shape` value (unknown or missing means sparse, as
    /// the SUMA reader does for anything it does not recognize).
    pub fn from_name(name: Option<&str>) -> Self {
        match name.map(|n| n.trim().to_ascii_lowercase()).as_deref() {
            Some("full") => Self::Full,
            Some("tri") => Self::Tri,
            Some("tri_diag") => Self::TriDiag,
            _ => Self::Sparse,
        }
    }
}

/// One row of `NODE_COORDS`.
#[derive(Debug, Clone, PartialEq)]
pub struct NodeRow {
    /// The node's index.
    pub index: i32,
    /// Its position, AFNI DICOM frame.
    pub xyz: [f32; 3],
    /// Its label.
    pub label: String,
}

/// A `Graph_Bucket` file.
#[derive(Debug, Clone, PartialEq)]
pub struct GraphBucket {
    /// Attributes of the `AFNI_dataset` element (`self_idcode`, `filename`, ...).
    pub root_attrs: BTreeMap<String, String>,
    /// How the edge rows map to a matrix.
    pub shape: MatrixShape,
    /// The `matrix_size` attribute as written (`" 4 4"`), if present.
    pub matrix_size: Option<String>,
    /// Other `SPARSE_DATA` attributes (`data_type`, ...).
    pub data_attrs: BTreeMap<String, String>,
    /// One vector of edge values per measure, in file order.
    pub measures: Vec<Vec<f32>>,
    /// One label per measure (from `COLMS_LABS`; empty strings if the file has none).
    pub measure_labels: Vec<String>,
    /// `INDEX_LIST` rows `[edge id, first node index, second node index]`.
    pub edges: Option<Vec<[i32; 3]>>,
    /// Other `INDEX_LIST` attributes.
    pub edge_attrs: BTreeMap<String, String>,
    /// The nodes, in file order.
    pub nodes: Vec<NodeRow>,
    /// Other `NODE_COORDS` attributes.
    pub node_attrs: BTreeMap<String, String>,
    /// Every other child element, in file order (history, links, column types).
    pub extras: Vec<NimlElement>,
}

/// The `atr_name` of an `AFNI_atr` element.
fn atr_name(e: &NimlElement) -> Option<&str> {
    (e.name == "AFNI_atr").then(|| e.attrs.get("atr_name").map(String::as_str))?
}

/// Whether an element body is empty (no body at all, or only whitespace).
fn is_empty_body(data: &NimlData) -> bool {
    match data {
        NimlData::None => true,
        NimlData::Text(t) => t.trim().is_empty(),
        _ => false,
    }
}

/// Attribute names that describe the data layout and are rebuilt, not copied.
const LAYOUT_ATTRS: [&str; 3] = ["ni_type", "ni_dimen", "ni_form"];

/// Column attributes that go stale when the numbers change.
const COLUMN_ATRS: [&str; 4] = ["COLMS_RANGE", "COLMS_LABS", "COLMS_TYPE", "COLMS_STATSYM"];

impl GraphBucket {
    /// Interpret an `AFNI_dataset` element.
    pub fn from_element(root: &NimlElement) -> Result<Self> {
        if root.name != "AFNI_dataset" {
            return Err(Error::parse(format!(
                "expected AFNI_dataset, got {}",
                root.name
            )));
        }
        let is_graph = root
            .attrs
            .get("dset_type")
            .is_some_and(|t| t.eq_ignore_ascii_case("Graph_Bucket"));
        if !is_graph {
            return Err(Error::invalid("the dataset is not a Graph_Bucket"));
        }
        let NimlData::Group(children) = &root.data else {
            return Err(Error::parse("Graph_Bucket is not a NIML group"));
        };
        let sparse = root
            .child("SPARSE_DATA")
            .ok_or_else(|| Error::missing("Graph_Bucket SPARSE_DATA"))?;
        let NimlData::Numeric(matrix) = &sparse.data else {
            return Err(Error::parse("SPARSE_DATA is not numeric"));
        };
        let measures: Vec<Vec<f32>> = matrix
            .columns
            .iter()
            .map(|c| c.to_f64_vec().into_iter().map(|v| v as f32).collect())
            .collect();

        let coords = root
            .child("NODE_COORDS")
            .ok_or_else(|| Error::missing("Graph_Bucket NODE_COORDS"))?;
        let NimlData::Mixed(table) = &coords.data else {
            return Err(Error::parse("NODE_COORDS is not a mixed table"));
        };
        if table.column_count() < 5 {
            return Err(Error::parse(
                "NODE_COORDS needs index, x, y, z and label columns",
            ));
        }
        let nodes = (0..table.rows)
            .map(|r| {
                let int = |c: usize| match table.get(r, c) {
                    Some(NimlValue::Integer(v)) => i32::try_from(*v).map_err(|_| {
                        Error::parse(format!("NODE_COORDS row {r}: index out of range"))
                    }),
                    _ => Err(Error::parse(format!(
                        "NODE_COORDS row {r} column {c} is not an integer"
                    ))),
                };
                let num = |c: usize| match table.get(r, c) {
                    Some(NimlValue::Float(v)) => Ok(*v as f32),
                    Some(NimlValue::Integer(v)) => Ok(*v as f32),
                    _ => Err(Error::parse(format!(
                        "NODE_COORDS row {r} column {c} is not numeric"
                    ))),
                };
                let label = match table.get(r, 4) {
                    Some(NimlValue::Text(t)) => t.clone(),
                    _ => {
                        return Err(Error::parse(format!(
                            "NODE_COORDS row {r}: label is not text"
                        )))
                    }
                };
                Ok(NodeRow {
                    index: int(0)?,
                    xyz: [num(1)?, num(2)?, num(3)?],
                    label,
                })
            })
            .collect::<Result<Vec<_>>>()?;

        // Layouts that imply their edges write an INDEX_LIST with no body; it is kept
        // as an extra, verbatim, and there are no explicit edges.
        let (edges, edge_attrs) = match root.child("INDEX_LIST") {
            Some(list) if is_empty_body(&list.data) => (None, BTreeMap::new()),
            Some(list) => {
                let NimlData::Numeric(m) = &list.data else {
                    return Err(Error::parse("INDEX_LIST is not numeric"));
                };
                if m.columns.len() < 3 {
                    return Err(Error::parse(
                        "INDEX_LIST needs edge, first and second node columns",
                    ));
                }
                let col: Vec<Vec<f64>> = m
                    .columns
                    .iter()
                    .take(3)
                    .map(TypedArray::to_f64_vec)
                    .collect();
                let rows = (0..m.rows)
                    .map(|r| [col[0][r] as i32, col[1][r] as i32, col[2][r] as i32])
                    .collect();
                (Some(rows), copy_attrs(&list.attrs, &["COLMS_RANGE"]))
            }
            None => (None, BTreeMap::new()),
        };

        // Labels come from the COLMS_LABS attribute element.
        let mut measure_labels: Vec<String> = children
            .iter()
            .find(|c| atr_name(c) == Some("COLMS_LABS"))
            .and_then(|c| match &c.data {
                NimlData::Text(t) => Some(
                    t.trim_matches(|ch| ch == ' ' || ch == '"')
                        .split(';')
                        .filter(|s| !s.is_empty())
                        .map(str::to_owned)
                        .collect::<Vec<_>>(),
                ),
                _ => None,
            })
            .unwrap_or_default();
        measure_labels.resize(measures.len(), String::new());

        let handled = ["SPARSE_DATA", "NODE_COORDS"];
        let extras = children
            .iter()
            .filter(|c| {
                // A real INDEX_LIST is parsed; an empty one is kept as an extra.
                !handled.contains(&c.name.as_str())
                    && !(c.name == "INDEX_LIST" && !is_empty_body(&c.data))
            })
            .filter(|c| atr_name(c) != Some("COLMS_LABS"))
            .cloned()
            .collect();

        Ok(Self {
            root_attrs: root.attrs.clone(),
            shape: MatrixShape::from_name(sparse.attrs.get("matrix_shape").map(String::as_str)),
            matrix_size: sparse.attrs.get("matrix_size").cloned(),
            data_attrs: copy_attrs(&sparse.attrs, &["matrix_shape", "matrix_size"]),
            measures,
            measure_labels,
            edges,
            edge_attrs,
            nodes,
            node_attrs: copy_attrs(&coords.attrs, &["COLMS_RANGE"]),
            extras,
        })
    }

    /// Build the `AFNI_dataset` element. Column-range attributes are not written
    /// (AFNI recomputes them); stale ones in `extras` are dropped for the same reason.
    pub fn to_element(&self) -> Result<NimlElement> {
        let rows = self.measures.first().map_or(0, Vec::len);
        if self.measures.is_empty() || self.measures.iter().any(|m| m.len() != rows) {
            return Err(Error::invalid(
                "graph measures are missing or differ in length",
            ));
        }
        let matrix = NumericMatrix::from_columns(
            self.measures
                .iter()
                .map(|m| TypedArray::Float32(m.clone()))
                .collect(),
        )?;
        let mut data_attrs = self.data_attrs.clone();
        if let Some(size) = &self.matrix_size {
            data_attrs.insert("matrix_size".into(), size.clone());
        }
        data_attrs.insert("matrix_shape".into(), self.shape.name().into());
        let mut children = vec![NimlElement::numeric("SPARSE_DATA", data_attrs, matrix)];

        if let Some(edges) = &self.edges {
            let col = |k: usize| TypedArray::Int32(edges.iter().map(|e| e[k]).collect());
            let m = NumericMatrix::from_columns(vec![col(0), col(1), col(2)])?;
            children.push(NimlElement::numeric(
                "INDEX_LIST",
                self.edge_attrs.clone(),
                m,
            ));
        }
        let mut values = Vec::with_capacity(self.nodes.len() * 5);
        for n in &self.nodes {
            values.push(NimlValue::Integer(i64::from(n.index)));
            values.extend(n.xyz.iter().map(|&c| NimlValue::Float(f64::from(c))));
            values.push(NimlValue::Text(n.label.clone()));
        }
        let table = MixedTable::new(
            vec![
                NimlValueType::Int32,
                NimlValueType::Float32,
                NimlValueType::Float32,
                NimlValueType::Float32,
                NimlValueType::String,
            ],
            self.nodes.len(),
            values,
        )?;
        children.push(NimlElement {
            name: "NODE_COORDS".into(),
            attrs: self.node_attrs.clone(),
            data: NimlData::Mixed(table),
        });

        // Column labels and types for the measures, then everything else we kept.
        let text_atr = |name: &str, body: String| {
            let mut a = BTreeMap::new();
            a.insert("atr_name".to_owned(), name.to_owned());
            NimlElement::text("AFNI_atr", a, body)
        };
        let labels: Vec<String> = self
            .measure_labels
            .iter()
            .map(|l| {
                if l.is_empty() {
                    "numeric".to_owned()
                } else {
                    l.clone()
                }
            })
            .collect();
        children.push(text_atr("COLMS_LABS", labels.join(";")));
        children.push(text_atr(
            "COLMS_TYPE",
            vec!["Generic_Float"; self.measures.len()].join(";"),
        ));
        children.extend(
            self.extras
                .iter()
                .filter(|e| !atr_name(e).is_some_and(|n| COLUMN_ATRS.contains(&n)))
                .cloned(),
        );
        Ok(NimlElement::group(
            "AFNI_dataset",
            self.root_attrs.clone(),
            children,
        ))
    }

    /// The path in the `network_link` element (a `.niml.tract` file), as written.
    pub fn network_file(&self) -> Option<&str> {
        self.extras
            .iter()
            .find(|e| e.name == "network_link")
            .and_then(|e| e.attrs.get("network_file"))
            .map(String::as_str)
    }

    /// Read a `Graph_Bucket` file (ASCII or binary NIML).
    pub fn read(path: impl AsRef<Path>) -> Result<Self> {
        let elements = niml::read(path)?;
        match elements.as_slice() {
            [root] => Self::from_element(root),
            _ => Err(Error::parse("expected one top-level graph dataset")),
        }
    }

    /// Write as ASCII NIML.
    pub fn write(&self, path: impl AsRef<Path>) -> Result<()> {
        niml::write(path, &[self.to_element()?])
    }

    /// Write with binary numeric bodies.
    pub fn write_binary(&self, path: impl AsRef<Path>) -> Result<()> {
        niml::write_binary(path, &[self.to_element()?])
    }
}

/// Attributes minus the layout ones (rebuilt on write) and the named extras.
fn copy_attrs(attrs: &BTreeMap<String, String>, drop: &[&str]) -> BTreeMap<String, String> {
    attrs
        .iter()
        .filter(|(k, _)| !LAYOUT_ATTRS.contains(&k.as_str()) && !drop.contains(&k.as_str()))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const FULL: &str = r#"<AFNI_dataset ni_form="ni_group" dset_type="Graph_Bucket">
<SPARSE_DATA ni_type="2*float" ni_dimen="4" matrix_size="2 2" matrix_shape="full">1 2 0 0 0 0 3 4</SPARSE_DATA>
<NODE_COORDS ni_type="int,3*float,String" ni_dimen="2">0 1 2 3 "A" 1 4 5 6 "B"</NODE_COORDS>
<network_link network_file="tracks.niml.tract" ni_form="ni_group"></network_link>
</AFNI_dataset>"#;

    #[test]
    fn reads_a_full_graph_and_its_link() {
        let root = niml::parse_str(FULL).unwrap().remove(0);
        let g = GraphBucket::from_element(&root).unwrap();
        assert_eq!(g.shape, MatrixShape::Full);
        assert_eq!(g.matrix_size.as_deref(), Some("2 2"));
        assert_eq!(
            g.measures,
            vec![vec![1.0, 0.0, 0.0, 3.0], vec![2.0, 0.0, 0.0, 4.0]]
        );
        assert_eq!(g.nodes[1].label, "B");
        assert_eq!(g.nodes[1].xyz, [4.0, 5.0, 6.0]);
        assert_eq!(g.network_file(), Some("tracks.niml.tract"));
        assert!(g.edges.is_none());
    }

    #[test]
    fn writes_and_reads_back_the_same_graph() {
        let root = niml::parse_str(FULL).unwrap().remove(0);
        let g = GraphBucket::from_element(&root).unwrap();
        let text = niml::serialize(&[g.to_element().unwrap()]);
        let again = GraphBucket::from_element(&niml::parse_str(&text).unwrap()[0]).unwrap();
        // Labels default to "numeric" when the file had none; everything else is equal.
        assert_eq!(again.measures, g.measures);
        assert_eq!(again.nodes, g.nodes);
        assert_eq!(again.shape, g.shape);
        assert_eq!(again.network_file(), g.network_file());
        // And again through the binary writer.
        let bin = niml::serialize_binary(&[g.to_element().unwrap()]);
        let from_bin = GraphBucket::from_element(&niml::parse(&bin).unwrap()[0]).unwrap();
        assert_eq!(from_bin.measures, g.measures);
    }

    #[test]
    fn rejects_files_that_are_not_graphs() {
        let not_graph = niml::parse_str(
            r#"<AFNI_dataset ni_form="ni_group" dset_type="Node_Bucket"></AFNI_dataset>"#,
        )
        .unwrap();
        assert!(GraphBucket::from_element(&not_graph[0]).is_err());
        let no_nodes = niml::parse_str(
            r#"<AFNI_dataset ni_form="ni_group" dset_type="Graph_Bucket"><SPARSE_DATA ni_type="float" ni_dimen="1" matrix_shape="sparse">1</SPARSE_DATA></AFNI_dataset>"#,
        )
        .unwrap();
        assert!(GraphBucket::from_element(&no_nodes[0]).is_err());
        assert_eq!(
            MatrixShape::from_name(Some("TRI_DIAG")),
            MatrixShape::TriDiag
        );
        assert_eq!(MatrixShape::from_name(Some("strange")), MatrixShape::Sparse);
        assert_eq!(MatrixShape::from_name(None), MatrixShape::Sparse);
    }
}
