//! Surface datasets: the `AFNI_dataset` NIML group written as `.niml.dset`.
//!
//! A SUMA surface dataset stores one value per (surface node, column). The
//! NIML group contains:
//!
//! * `SPARSE_DATA`: the values, one typed column per sub-brick; its
//!   `ni_timestep` attribute gives the time step of a time series,
//! * `INDEX_LIST`: the surface node of each row (absent means dense: row *i*
//!   is node *i*),
//! * `AFNI_atr` elements: the same attribute system as a `.HEAD` file,
//!   including `COLMS_LABS`, `COLMS_TYPE`, `COLMS_RANGE`, `COLMS_STATSYM`,
//!   `HISTORY_NOTE`, `FDRCURVE_%06d` and `UNIQUE_VALS_%06d`,
//! * for label datasets (`dset_type="Node_Label"`), an `AFNI_labeltable`.
//!
//! The `AFNI_atr` elements are kept as a [`Header`]
//! ([`NimlDataset::attributes`]), so every attribute, known or not,
//! survives a read and write, and the `.HEAD` accessors apply (e.g.
//! [`Header::fdr_curve`]). Column lists are `;`-separated and positional:
//! entry *c* belongs to column *c*, and an empty entry stays empty.
//!
//! References: `afni/src/suma_datasets.c`, `afni/src/matlab/afni_niml_*.m`.

use std::collections::BTreeMap;
use std::path::Path;

use crate::array::DataType;
use crate::error::{self, Error, Result};
use crate::head::{AttributeValue, Header};
use crate::labels::LabelTable;
use crate::niml::{self, NimlData, NimlElement, NumericMatrix};
use crate::stat::{parse_statsym_list, StatSpec};

/// A parsed `AFNI_dataset` surface dataset.
#[derive(Debug, Clone, PartialEq)]
pub struct NimlDataset {
    /// The `dset_type` attribute, e.g. `Node_Bucket` or `Node_Label`.
    pub dset_type: String,
    /// The dataset's own ID code (`self_idcode`), if present. A new one is
    /// generated on write when this is `None`.
    pub self_idcode: Option<String>,
    /// ID code of the surface whose nodes the rows refer to
    /// (`domain_parent_idcode`). `None` when absent, empty, or AFNI's empty
    /// marker `~`.
    pub domain_parent_idcode: Option<String>,
    /// ID code of the surface the dataset's geometry came from
    /// (`geometry_parent_idcode`), with the same conventions.
    pub geometry_parent_idcode: Option<String>,
    /// The originating filename recorded in the header, if present.
    pub filename: Option<String>,
    /// The display label, if present.
    pub label: Option<String>,
    /// Any other attributes of the `AFNI_dataset` element, kept as read.
    pub other_attrs: BTreeMap<String, String>,
    /// The values (`SPARSE_DATA`), one typed column per sub-brick.
    pub data: NumericMatrix,
    /// Node index for each row (`INDEX_LIST`). `None` means a dense dataset
    /// where row *i* is node *i*.
    pub node_indices: Option<Vec<u32>>,
    /// Time between columns in seconds, for a time series (`ni_timestep` on
    /// `SPARSE_DATA`, set by `3drefit -TR`).
    pub time_step: Option<f64>,
    /// Every `AFNI_atr` element, decoded like `.HEAD` attributes.
    pub attributes: Header,
    /// The label table of a label dataset (`AFNI_labeltable`).
    pub label_table: Option<LabelTable>,
    /// Child elements this module does not interpret, kept as read.
    pub other_elements: Vec<NimlElement>,
}

/// A column's range as recorded in `COLMS_RANGE`: the smallest and largest
/// values and the nodes where they occur.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ColumnRange {
    /// Smallest value.
    pub min: f64,
    /// Largest value.
    pub max: f64,
    /// Node holding the smallest value.
    pub min_node: i64,
    /// Node holding the largest value.
    pub max_node: i64,
}

impl NimlDataset {
    /// Build a dataset from values, with no attributes yet. `dset_type` is
    /// usually `Node_Bucket`.
    pub fn new(
        dset_type: impl Into<String>,
        data: NumericMatrix,
        node_indices: Option<Vec<u32>>,
    ) -> Self {
        Self {
            dset_type: dset_type.into(),
            self_idcode: None,
            domain_parent_idcode: None,
            geometry_parent_idcode: None,
            filename: None,
            label: None,
            other_attrs: BTreeMap::new(),
            data,
            node_indices,
            time_step: None,
            attributes: Header::default(),
            label_table: None,
            other_elements: Vec::new(),
        }
    }

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
        let mut time_step = None;
        let mut label_table = None;
        let mut atrs = Vec::new();
        let mut other_elements = Vec::new();
        for child in element.children() {
            match child.name.as_str() {
                "SPARSE_DATA" => {
                    let NimlData::Numeric(matrix) = &child.data else {
                        return Err(Error::parse("SPARSE_DATA payload is not numeric"));
                    };
                    data = Some(matrix.clone());
                    time_step = child
                        .attrs
                        .get("ni_timestep")
                        .and_then(|v| v.trim().parse::<f64>().ok())
                        .filter(|v| v.is_finite() && *v > 0.0);
                }
                "INDEX_LIST" => {
                    let NimlData::Numeric(matrix) = &child.data else {
                        return Err(Error::parse("INDEX_LIST payload is not numeric"));
                    };
                    node_indices = Some(node_indices_from_matrix(matrix)?);
                }
                "AFNI_atr" => atrs.push(child.clone()),
                "AFNI_labeltable" => label_table = Some(LabelTable::from_element(child)?),
                _ => other_elements.push(child.clone()),
            }
        }
        let data = data.ok_or_else(|| Error::missing("SPARSE_DATA element"))?;
        // Only this dataset's own AFNI_atr elements, not the label table's.
        let attributes = Header::from_niml(&NimlElement::group("atrs", BTreeMap::new(), atrs));

        let mut other_attrs = element.attrs.clone();
        let mut take = |key: &str| other_attrs.remove(key);
        let dset_type = take("dset_type").unwrap_or_else(|| "Node_Bucket".to_string());
        let self_idcode = take("self_idcode");
        let domain_parent_idcode = take("domain_parent_idcode").and_then(non_empty_id);
        let geometry_parent_idcode = take("geometry_parent_idcode").and_then(non_empty_id);
        let filename = take("filename");
        let label = take("label");
        take("ni_form");

        Ok(Self {
            dset_type,
            self_idcode,
            domain_parent_idcode,
            geometry_parent_idcode,
            filename,
            label,
            other_attrs,
            data,
            node_indices,
            time_step,
            attributes,
            label_table,
            other_elements,
        })
    }

    /// Number of rows (nodes with data).
    pub fn rows(&self) -> usize {
        self.data.rows
    }

    /// Number of columns (sub-bricks).
    pub fn column_count(&self) -> usize {
        self.data.column_count()
    }

    /// Whether the dataset carries an explicit node index list.
    pub fn is_sparse(&self) -> bool {
        self.node_indices.is_some()
    }

    /// The surface node of row `row`.
    pub fn node_for_row(&self, row: usize) -> Option<u32> {
        match &self.node_indices {
            Some(nodes) => nodes.get(row).copied(),
            None => (row < self.rows()).then_some(row as u32),
        }
    }

    // --- Column metadata -----------------------------------------------------

    /// `COLMS_LABS`: one label per column; missing ones are empty.
    pub fn column_labels(&self) -> Vec<String> {
        self.column_list("COLMS_LABS")
    }

    /// The label of `column`, or `col_<column>` when it has none.
    pub fn column_label(&self, column: usize) -> String {
        self.column_labels()
            .get(column)
            .filter(|s| !s.trim().is_empty())
            .cloned()
            .unwrap_or_else(|| format!("col_{column}"))
    }

    /// `COLMS_TYPE`: SUMA's column type names (e.g. `Generic_Float`,
    /// `Node_Index_Label`); missing ones are empty.
    pub fn column_types(&self) -> Vec<String> {
        self.column_list("COLMS_TYPE")
    }

    /// `COLMS_STATSYM`: the statistic in each column, if any.
    pub fn column_stats(&self) -> Vec<Option<StatSpec>> {
        let mut stats = self
            .attributes
            .string("COLMS_STATSYM")
            .map(parse_statsym_list)
            .unwrap_or_default();
        stats.resize(self.column_count(), None);
        stats
    }

    /// `COLMS_RANGE` as recorded in the file (`min max min_node max_node`
    /// per column). It is recomputed from the data on write.
    pub fn column_ranges(&self) -> Vec<Option<ColumnRange>> {
        self.column_list("COLMS_RANGE")
            .iter()
            .map(|entry| {
                let v: Vec<f64> = entry
                    .split_whitespace()
                    .filter_map(|t| t.parse().ok())
                    .collect();
                (v.len() == 4).then(|| ColumnRange {
                    min: v[0],
                    max: v[1],
                    min_node: v[2] as i64,
                    max_node: v[3] as i64,
                })
            })
            .collect()
    }

    /// `HISTORY_NOTE`, if present.
    pub fn history(&self) -> Option<&str> {
        self.attributes.string("HISTORY_NOTE")
    }

    /// Set `COLMS_LABS`. A label may not contain `;`, the list separator,
    /// which AFNI has no way to escape.
    pub fn set_column_labels<S: AsRef<str>>(&mut self, labels: &[S]) -> Result<()> {
        self.set_column_list("COLMS_LABS", labels)
    }

    /// Set `COLMS_TYPE` (no `;` allowed, as for labels).
    pub fn set_column_types<S: AsRef<str>>(&mut self, types: &[S]) -> Result<()> {
        self.set_column_list("COLMS_TYPE", types)
    }

    /// Set `COLMS_STATSYM` (`None` is written as `none`).
    pub fn set_column_stats(&mut self, stats: &[Option<StatSpec>]) {
        let symbols: Vec<String> = stats
            .iter()
            .map(|s| s.as_ref().map_or("none".into(), StatSpec::to_statsym))
            .collect();
        self.set_column_list("COLMS_STATSYM", &symbols)
            .expect("STATSYM entries never contain ';'");
    }

    /// Set `HISTORY_NOTE`.
    pub fn set_history(&mut self, history: impl Into<String>) {
        self.attributes
            .set("HISTORY_NOTE", AttributeValue::String(history.into()));
    }

    /// A `;`-separated column list, one entry per column. AFNI ends lists
    /// with `;`; a list may also be longer than the column count
    /// (`ConvertDset -labelize` writes two entries for one column).
    fn column_list(&self, name: &str) -> Vec<String> {
        let text = self.attributes.string(name).unwrap_or_default();
        let text = text.strip_suffix(';').unwrap_or(text);
        let mut items: Vec<String> = if text.is_empty() {
            Vec::new()
        } else {
            text.split(';').map(|s| s.trim().to_string()).collect()
        };
        items.resize(self.column_count(), String::new());
        items
    }

    fn set_column_list<S: AsRef<str>>(&mut self, name: &str, items: &[S]) -> Result<()> {
        if let Some(bad) = items.iter().find(|s| s.as_ref().contains(';')) {
            return Err(Error::invalid(format!(
                "{name} entry {:?} contains ';', the column separator",
                bad.as_ref()
            )));
        }
        let mut text: String = items.iter().map(|s| format!("{};", s.as_ref())).collect();
        if text.is_empty() {
            text.push(';');
        }
        self.attributes.set(name, AttributeValue::String(text));
        Ok(())
    }

    /// Attach a label table, making this a label dataset (`Node_Label`) as
    /// `ConvertDset -labelize` does.
    pub fn with_label_table(mut self, table: LabelTable) -> Self {
        self.dset_type = "Node_Label".into();
        self.label_table = Some(table);
        self
    }

    // --- Writing ---------------------------------------------------------------

    /// Build the `AFNI_dataset` NIML element.
    ///
    /// As SUMA does on write: `COLMS_RANGE` is recomputed from the data, a
    /// missing `COLMS_TYPE` is filled from the column types (`Generic_Float`,
    /// `Generic_Int`, ...), and a missing `self_idcode` is generated. Parent
    /// ID codes that are `None` are written as bare attributes, AFNI's NULL.
    pub fn to_element(&self) -> Result<NimlElement> {
        if let Some(nodes) = &self.node_indices {
            if nodes.len() != self.rows() {
                return Err(Error::invalid(format!(
                    "{} node indices for {} rows",
                    nodes.len(),
                    self.rows()
                )));
            }
        }

        let mut root = self.other_attrs.clone();
        root.insert("dset_type".into(), self.dset_type.clone());
        root.insert(
            "self_idcode".into(),
            self.self_idcode
                .clone()
                .unwrap_or_else(crate::brik::new_idcode),
        );
        root.insert(
            "domain_parent_idcode".into(),
            self.domain_parent_idcode.clone().unwrap_or_default(),
        );
        root.insert(
            "geometry_parent_idcode".into(),
            self.geometry_parent_idcode.clone().unwrap_or_default(),
        );
        for (key, value) in [("filename", &self.filename), ("label", &self.label)] {
            if let Some(value) = value {
                root.insert(key.into(), value.clone());
            }
        }

        let mut sparse_attrs = BTreeMap::new();
        sparse_attrs.insert("data_type".into(), format!("{}_data", self.dset_type));
        if let Some(step) = self.time_step {
            sparse_attrs.insert("ni_timestep".into(), niml::format_float(step));
        }
        let mut children = vec![NimlElement::numeric(
            "SPARSE_DATA",
            sparse_attrs,
            self.data.clone(),
        )];

        if let Some(nodes) = &self.node_indices {
            let mut attrs = BTreeMap::new();
            attrs.insert(
                "data_type".into(),
                format!("{}_node_indices", self.dset_type),
            );
            attrs.insert("COLMS_LABS".into(), "Node Indices".into());
            attrs.insert("COLMS_TYPE".into(), "Node_Index".into());
            let sorted = nodes.windows(2).all(|w| w[0] <= w[1]);
            attrs.insert(
                "sorted_node_def".into(),
                if sorted { "Yes" } else { "No" }.into(),
            );
            let index = NumericMatrix::from_columns(vec![crate::array::TypedArray::Int32(
                nodes.iter().map(|&n| n as i32).collect(),
            )])?;
            children.push(NimlElement::numeric("INDEX_LIST", attrs, index));
        }

        let mut attributes = self.attributes.clone();
        let ranges: Vec<String> = (0..self.column_count())
            .map(|c| self.computed_range(c))
            .collect();
        attributes.set(
            "COLMS_RANGE",
            AttributeValue::String(format!("{};", ranges.join(";"))),
        );
        if attributes.get("COLMS_TYPE").is_none() {
            let types: Vec<&str> = self
                .data
                .columns
                .iter()
                .map(|c| generic_type_name(c.dtype()))
                .collect();
            attributes.set(
                "COLMS_TYPE",
                AttributeValue::String(format!("{};", types.join(";"))),
            );
        }
        children.extend(attributes.to_niml().children().iter().cloned());

        if let Some(table) = &self.label_table {
            children.push(table.to_afni_labeltable());
        }
        children.extend(self.other_elements.iter().cloned());
        Ok(NimlElement::group("AFNI_dataset", root, children))
    }

    /// `min max min_node max_node` for `column`, as SUMA computes
    /// `COLMS_RANGE` (finite values only; `0 0 -1 -1` when there are none).
    fn computed_range(&self, column: usize) -> String {
        let mut best: Option<(f64, usize, f64, usize)> = None;
        for row in 0..self.rows() {
            let Some(v) = self.data.get(row, column).filter(|v| v.is_finite()) else {
                continue;
            };
            best = Some(match best {
                None => (v, row, v, row),
                Some((lo, lo_row, hi, hi_row)) => (
                    if v < lo { v } else { lo },
                    if v < lo { row } else { lo_row },
                    if v > hi { v } else { hi },
                    if v > hi { row } else { hi_row },
                ),
            });
        }
        match best {
            Some((lo, lo_row, hi, hi_row)) => format!(
                "{} {} {} {}",
                niml::format_float(lo),
                niml::format_float(hi),
                self.node_for_row(lo_row).unwrap_or(0),
                self.node_for_row(hi_row).unwrap_or(0)
            ),
            None => "0 0 -1 -1".into(),
        }
    }

    /// Serialise the dataset to an ASCII NIML string.
    pub fn to_niml_string(&self) -> Result<String> {
        Ok(niml::serialize(&[self.to_element()?]))
    }

    /// Write the dataset to a `.niml.dset` file (ASCII form).
    pub fn write(&self, path: impl AsRef<Path>) -> Result<()> {
        error::write_file(path.as_ref(), self.to_niml_string()?.as_bytes())
    }

    /// Write the dataset with binary data columns (`binary.lsbfirst`), the
    /// compact form AFNI programs write by default.
    pub fn write_binary(&self, path: impl AsRef<Path>) -> Result<()> {
        niml::write_binary(path, &[self.to_element()?])
    }
}

/// SUMA's generic column type name for an array type
/// (`SUMA_Col_Type_Name`, `suma_datasets.c`).
fn generic_type_name(dtype: DataType) -> &'static str {
    match dtype {
        DataType::UInt8 => "Generic_Byte",
        DataType::Int16 => "Generic_Short",
        DataType::Float32 => "Generic_Float",
        DataType::Float64 => "Generic_Double",
        _ => "Generic_Int",
    }
}

/// AFNI marks an absent parent ID as a NULL attribute, an empty string, or
/// `~` (`SUMA_EMPTY_ATTR`).
fn non_empty_id(id: String) -> Option<String> {
    let trimmed = id.trim();
    (!trimmed.is_empty() && trimmed != "~").then_some(id)
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

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
<AFNI_dataset ni_form="ni_group" dset_type="Node_Bucket" self_idcode="XYZ_TEST" domain_parent_idcode="~" filename="toy.niml.dset" >
<SPARSE_DATA ni_type="2*float" ni_dimen="2" data_type="Node_Bucket_data" >
1.5 2.5
3.5 -4.5
</SPARSE_DATA>
<INDEX_LIST ni_type="int" ni_dimen="2" data_type="Node_Bucket_node_indices" >
10
12
</INDEX_LIST>
<AFNI_atr ni_type="String" ni_dimen="1" atr_name="COLMS_LABS" >"effect;;"</AFNI_atr>
<AFNI_atr ni_type="String" ni_dimen="1" atr_name="MY_OWN_ATTR" >"kept"</AFNI_atr>
</AFNI_dataset>
"#;

    #[test]
    fn reads_sparse_dataset() {
        let dset = NimlDataset::from_bytes(SAMPLE.as_bytes()).unwrap();
        assert_eq!(dset.dset_type, "Node_Bucket");
        assert_eq!(dset.rows(), 2);
        assert_eq!(dset.column_count(), 2);
        assert_eq!(dset.node_indices, Some(vec![10, 12]));
        // Positional: the empty second label stays empty.
        assert_eq!(dset.column_labels(), ["effect", ""]);
        assert_eq!(dset.column_label(1), "col_1");
        assert_eq!(dset.data.get(1, 1), Some(-4.5));
        assert_eq!(dset.domain_parent_idcode, None, "~ means empty");
        assert_eq!(dset.node_for_row(1), Some(12));
    }

    #[test]
    fn round_trips_through_niml() {
        let dset = NimlDataset::from_bytes(SAMPLE.as_bytes()).unwrap();
        let reparsed = NimlDataset::from_bytes(dset.to_niml_string().unwrap().as_bytes()).unwrap();
        assert_eq!(reparsed.node_indices, Some(vec![10, 12]));
        assert_eq!(reparsed.data, dset.data);
        assert_eq!(reparsed.column_labels(), ["effect", ""]);
        assert_eq!(reparsed.attributes.string("MY_OWN_ATTR"), Some("kept"));
        // Written by us: ranges and types are filled in.
        assert_eq!(
            reparsed.column_ranges()[1],
            Some(ColumnRange {
                min: -4.5,
                max: 2.5,
                min_node: 12,
                max_node: 10
            })
        );
        assert_eq!(reparsed.column_types(), ["Generic_Float", "Generic_Float"]);
    }

    #[test]
    fn labels_cannot_contain_the_separator() {
        let mut dset = NimlDataset::from_bytes(SAMPLE.as_bytes()).unwrap();
        assert!(dset.set_column_labels(&["a;b", "c"]).is_err());
        dset.set_column_labels(&["a b", "c"]).unwrap();
        assert_eq!(dset.column_labels(), ["a b", "c"]);
    }
}
