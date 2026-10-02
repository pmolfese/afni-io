//! Surface datasets: the `AFNI_dataset` NIML group written as `.niml.dset`.
//!
//! A SUMA surface dataset stores one value per (surface node, sub-brick). The
//! NIML group contains:
//!
//! * `SPARSE_DATA` — the value matrix (`rows` nodes x `columns` sub-bricks),
//! * `INDEX_LIST` — the surface node index for each row (absent ⇒ dense, row
//!   *i* maps to node *i*),
//! * a series of `AFNI_atr` metadata elements (`COLMS_LABS`, `COLMS_TYPE`,
//!   `COLMS_RANGE`, `COLMS_STATSYM`, `HISTORY_NOTE`).
//!
//! References: `afni/src/matlab/afni_niml_writesimple.m`,
//! `SUMAvista/src/pysuma/niml.py:read_niml_dset`.

use std::collections::BTreeMap;
use std::path::Path;

use crate::error::{self, Error, Result};
use crate::niml::{self, NimlData, NimlElement, NimlValueType, NumericMatrix};

/// A parsed `AFNI_dataset` surface dataset.
#[derive(Debug, Clone, PartialEq)]
pub struct NimlDataset {
    /// The `dset_type` attribute, e.g. `Node_Bucket`.
    pub dset_type: String,
    /// The dataset's own ID code (`self_idcode`), if present.
    pub self_idcode: Option<String>,
    /// The originating filename recorded in the header, if present.
    pub filename: Option<String>,
    /// The display label, if present.
    pub label: Option<String>,
    /// The value matrix (`SPARSE_DATA`).
    pub data: NumericMatrix,
    /// Node index for each row (`INDEX_LIST`). `None` means a dense dataset
    /// where row *i* corresponds to node *i*.
    pub node_indices: Option<Vec<u32>>,
    /// Per-column labels (`COLMS_LABS`).
    pub labels: Vec<String>,
    /// Per-column AFNI type names (`COLMS_TYPE`).
    pub types: Vec<String>,
    /// Per-column min/max range strings (`COLMS_RANGE`).
    pub ranges: Vec<String>,
    /// Per-column statistic symbols (`COLMS_STATSYM`).
    pub stats: Vec<String>,
    /// The history note (`HISTORY_NOTE`), if present.
    pub history: Option<String>,
}

impl NimlDataset {
    /// Read and parse a `.niml.dset` file.
    pub fn read(path: impl AsRef<Path>) -> Result<Self> {
        Self::from_bytes(&error::read_file(path.as_ref())?)
    }

    /// Parse a dataset from raw NIML bytes.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        let elements = niml::parse(bytes)?;
        let element = elements
            .iter()
            .find(|e| e.name == "AFNI_dataset")
            .ok_or_else(|| Error::missing("AFNI_dataset element"))?;
        Self::from_element(element)
    }

    /// Interpret an already-parsed `AFNI_dataset` element.
    pub fn from_element(element: &NimlElement) -> Result<Self> {
        if element.name != "AFNI_dataset" {
            return Err(Error::parse(format!(
                "expected AFNI_dataset, got {}",
                element.name
            )));
        }

        let mut data = None;
        let mut node_indices = None;
        let mut labels = Vec::new();
        let mut types = Vec::new();
        let mut ranges = Vec::new();
        let mut stats = Vec::new();
        let mut history = None;

        for child in element.children() {
            match child.name.as_str() {
                "SPARSE_DATA" => {
                    let NimlData::Numeric(matrix) = &child.data else {
                        return Err(Error::parse("SPARSE_DATA payload is not numeric"));
                    };
                    data = Some(matrix.clone());
                }
                "INDEX_LIST" => {
                    let NimlData::Numeric(matrix) = &child.data else {
                        return Err(Error::parse("INDEX_LIST payload is not numeric"));
                    };
                    node_indices = Some(node_indices_from_matrix(matrix)?);
                }
                "AFNI_atr" => {
                    let Some(atr_name) = child.attrs.get("atr_name") else {
                        continue;
                    };
                    let text = match &child.data {
                        NimlData::Text(text) => text.as_str(),
                        _ => "",
                    };
                    match atr_name.as_str() {
                        "COLMS_LABS" => labels = split_semicolons(text),
                        "COLMS_TYPE" => types = split_semicolons(text),
                        "COLMS_RANGE" => ranges = split_semicolons(text),
                        "COLMS_STATSYM" => stats = split_semicolons(text),
                        "HISTORY_NOTE" => history = Some(text.trim().to_string()),
                        _ => {}
                    }
                }
                _ => {}
            }
        }

        let data = data.ok_or_else(|| Error::missing("SPARSE_DATA element"))?;

        Ok(Self {
            dset_type: element
                .attrs
                .get("dset_type")
                .cloned()
                .unwrap_or_else(|| "Node_Bucket".to_string()),
            self_idcode: element.attrs.get("self_idcode").cloned(),
            filename: element.attrs.get("filename").cloned(),
            label: element.attrs.get("label").cloned(),
            data,
            node_indices,
            labels,
            types,
            ranges,
            stats,
            history,
        })
    }

    /// Number of nodes (rows in `SPARSE_DATA`).
    pub fn rows(&self) -> usize {
        self.data.rows
    }

    /// Number of sub-bricks (columns in `SPARSE_DATA`).
    pub fn columns(&self) -> usize {
        self.data.columns()
    }

    /// Whether the dataset carries an explicit node index list.
    pub fn is_sparse(&self) -> bool {
        self.node_indices.is_some()
    }

    /// The label for a column, falling back to `col_<i>`.
    pub fn column_label(&self, column: usize) -> String {
        self.labels
            .get(column)
            .filter(|s| !s.trim().is_empty())
            .cloned()
            .unwrap_or_else(|| format!("col_{column}"))
    }

    /// Build the `AFNI_dataset` NIML element for this dataset.
    pub fn to_element(&self) -> Result<NimlElement> {
        let node_indices = self
            .node_indices
            .clone()
            .unwrap_or_else(|| (0..self.data.rows as u32).collect());
        if node_indices.len() != self.data.rows {
            return Err(Error::invalid(
                "node index count does not match SPARSE_DATA rows",
            ));
        }

        let mut root_attrs = BTreeMap::new();
        root_attrs.insert("dset_type".into(), self.dset_type.clone());
        for (key, value) in [
            ("self_idcode", &self.self_idcode),
            ("filename", &self.filename),
            ("label", &self.label),
        ] {
            if let Some(value) = value {
                root_attrs.insert(key.into(), value.clone());
            }
        }

        let mut sparse_attrs = BTreeMap::new();
        sparse_attrs.insert("data_type".into(), "Node_Bucket_data".into());

        let sorted = node_indices.windows(2).all(|w| w[0] <= w[1]);
        let mut index_attrs = BTreeMap::new();
        index_attrs.insert("data_type".into(), "Node_Bucket_node_indices".into());
        index_attrs.insert("COLMS_LABS".into(), "Node Indices".into());
        index_attrs.insert("COLMS_TYPE".into(), "Node_Index".into());
        index_attrs.insert(
            "sorted_node_def".into(),
            if sorted { "Yes" } else { "No" }.into(),
        );
        let index_matrix = NumericMatrix::new(
            vec![NimlValueType::Int32],
            node_indices.len(),
            node_indices.iter().map(|v| *v as f64).collect(),
        )?;

        let mut children = vec![
            NimlElement::numeric("SPARSE_DATA", sparse_attrs, self.data.clone()),
            NimlElement::numeric("INDEX_LIST", index_attrs, index_matrix),
        ];
        push_atr(&mut children, "COLMS_RANGE", &join_semicolons(&self.ranges));
        push_atr(&mut children, "COLMS_LABS", &join_semicolons(&self.labels));
        push_atr(&mut children, "COLMS_TYPE", &join_semicolons(&self.types));
        push_atr(
            &mut children,
            "COLMS_STATSYM",
            &join_semicolons(&self.stats),
        );
        if let Some(history) = &self.history {
            push_atr(&mut children, "HISTORY_NOTE", history);
        }

        Ok(NimlElement::group("AFNI_dataset", root_attrs, children))
    }

    /// Serialise the dataset to an ASCII NIML string.
    pub fn to_niml_string(&self) -> Result<String> {
        Ok(niml::serialize(&[self.to_element()?]))
    }

    /// Write the dataset to a `.niml.dset` file (ASCII form).
    pub fn write(&self, path: impl AsRef<Path>) -> Result<()> {
        error::write_file(path.as_ref(), self.to_niml_string()?.as_bytes())
    }
}

fn node_indices_from_matrix(matrix: &NumericMatrix) -> Result<Vec<u32>> {
    let mut indices = Vec::with_capacity(matrix.rows);
    for row in 0..matrix.rows {
        let value = matrix
            .get(row, 0)
            .ok_or_else(|| Error::parse("INDEX_LIST row has no first column"))?;
        if !value.is_finite() || value < 0.0 || value.fract() != 0.0 {
            return Err(Error::invalid(format!(
                "INDEX_LIST contains non-node-index value {value}"
            )));
        }
        indices.push(value as u32);
    }
    Ok(indices)
}

fn push_atr(children: &mut Vec<NimlElement>, atr_name: &str, text: &str) {
    if text.trim().is_empty() {
        return;
    }
    let mut attrs = BTreeMap::new();
    attrs.insert("atr_name".to_string(), atr_name.to_string());
    children.push(NimlElement::text("AFNI_atr", attrs, text.to_string()));
}

/// Split a `value1;value2;` AFNI list, tolerating surrounding quotes.
fn split_semicolons(text: &str) -> Vec<String> {
    strip_quotes(text.trim())
        .split(';')
        .map(|p| strip_quotes(p.trim()).to_string())
        .filter(|p| !p.is_empty())
        .collect()
}

fn join_semicolons(values: &[String]) -> String {
    if values.is_empty() {
        String::new()
    } else {
        format!("{};", values.join(";"))
    }
}

fn strip_quotes(text: &str) -> &str {
    if text.len() >= 2 && text.starts_with('"') && text.ends_with('"') {
        &text[1..text.len() - 1]
    } else {
        text
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
<AFNI_dataset ni_form="ni_group" dset_type="Node_Bucket" self_idcode="XYZ_TEST" filename="toy.niml.dset" >
<SPARSE_DATA ni_type="float,float" ni_dimen="2" data_type="Node_Bucket_data" >
1.5 2.5
3.5 4.5
</SPARSE_DATA>
<INDEX_LIST ni_type="int" ni_dimen="2" data_type="Node_Bucket_node_indices" >
10
12
</INDEX_LIST>
<AFNI_atr ni_type="String" ni_dimen="1" atr_name="COLMS_LABS" >effect;stat;</AFNI_atr>
</AFNI_dataset>
"#;

    #[test]
    fn reads_sparse_dataset() {
        let dset = NimlDataset::from_bytes(SAMPLE.as_bytes()).unwrap();
        assert_eq!(dset.dset_type, "Node_Bucket");
        assert_eq!(dset.rows(), 2);
        assert_eq!(dset.columns(), 2);
        assert_eq!(dset.node_indices, Some(vec![10, 12]));
        assert_eq!(dset.labels, vec!["effect", "stat"]);
        assert_eq!(dset.data.get(1, 1), Some(4.5));
    }

    #[test]
    fn round_trips_through_niml() {
        let dset = NimlDataset::from_bytes(SAMPLE.as_bytes()).unwrap();
        let serialized = dset.to_niml_string().unwrap();
        let reparsed = NimlDataset::from_bytes(serialized.as_bytes()).unwrap();
        assert_eq!(reparsed.node_indices, Some(vec![10, 12]));
        assert_eq!(reparsed.data.values, vec![1.5, 2.5, 3.5, 4.5]);
        assert_eq!(reparsed.labels, vec!["effect", "stat"]);
    }
}
