//! Label tables: integer keys with names (and, for surfaces, colours).
//!
//! AFNI and SUMA store the same idea in two NIML layouts:
//!
//! * `VALUE_LABEL_DTABLE` (`ni_type="2*String"`): rows of `"key" "name"`.
//!   Volumes keep one in the `VALUE_LABEL_DTABLE` attribute of a `.HEAD` or a
//!   NIfTI's AFNI extension ([`crate::head::Header::value_label_table`]), and
//!   `.niml.lt` files hold one on its own (`3drefit -labeltable`,
//!   `@MakeLabelTable`).
//! * `AFNI_labeltable`: a dataset-shaped group whose `SPARSE_DATA` has the
//!   columns R, G, B, A, key, name (`ni_type="4*float,int,String"`). SUMA
//!   label datasets (`dset_type="Node_Label"`, e.g. from FreeSurfer
//!   annotations) carry one, and `.niml.cmap` files from `MakeColorMap
//!   -suma_cmap` are one (`suma_datasets.c`, `SUMA_dset_to_Label_Dset`).
//!
//! [`LabelTable`] reads either and writes either.

use std::collections::BTreeMap;
use std::path::Path;

use crate::error::{self, Error, Result};
use crate::niml::{self, MixedTable, NimlData, NimlElement, NimlValue, NimlValueType};

/// One label.
#[derive(Debug, Clone, PartialEq)]
pub struct LabelEntry {
    /// The integer stored in the data.
    pub key: i64,
    /// The label text.
    pub name: String,
    /// Display colour, RGBA in `[0, 1]`. `VALUE_LABEL_DTABLE` tables have
    /// none.
    pub rgba: Option<[f32; 4]>,
}

/// A label table: keys to names, in file order.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct LabelTable {
    /// The entries, in file order.
    pub entries: Vec<LabelEntry>,
    /// The table element's attributes other than the NIML structural ones
    /// (e.g. `pbar_name`, or `Name`, `Sgn`, `M0` on an `AFNI_labeltable`),
    /// kept so a table can be written back unchanged.
    pub attrs: BTreeMap<String, String>,
}

impl LabelTable {
    /// Read a label table from a `.niml.lt`, a `.niml.cmap`, or a dataset
    /// that contains one: the first `VALUE_LABEL_DTABLE` or `AFNI_labeltable`
    /// found.
    pub fn read(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        Self::find(&niml::read(path)?)?
            .ok_or_else(|| Error::missing(format!("label table in {}", path.display())))
    }

    /// The first label table among `elements` or inside them, if any.
    pub fn find(elements: &[NimlElement]) -> Result<Option<Self>> {
        for element in elements {
            if is_table(element) {
                return Self::from_element(element).map(Some);
            }
            if let NimlData::Group(children) = &element.data {
                if let Some(table) = Self::find(children)? {
                    return Ok(Some(table));
                }
            }
        }
        Ok(None)
    }

    /// Interpret a `VALUE_LABEL_DTABLE` or `AFNI_labeltable` element.
    pub fn from_element(element: &NimlElement) -> Result<Self> {
        let mut attrs = element.attrs.clone();
        for key in ["ni_type", "ni_dimen", "ni_form"] {
            attrs.remove(key);
        }
        let entries = match element.name.as_str() {
            "VALUE_LABEL_DTABLE" => dtable_entries(element)?,
            "AFNI_labeltable" => labeltable_entries(element)?,
            other => return Err(Error::parse(format!("{other} is not a label table"))),
        };
        Ok(Self { entries, attrs })
    }

    /// The entry for `key`, if any.
    pub fn get(&self, key: i64) -> Option<&LabelEntry> {
        self.entries.iter().find(|e| e.key == key)
    }

    /// The key labelled `name`, if any.
    pub fn key_for(&self, name: &str) -> Option<i64> {
        self.entries.iter().find(|e| e.name == name).map(|e| e.key)
    }

    /// The table as a `VALUE_LABEL_DTABLE` element (keys and names only).
    pub fn to_value_label_dtable(&self) -> NimlElement {
        let values = self
            .entries
            .iter()
            .flat_map(|e| {
                [
                    NimlValue::Text(e.key.to_string()),
                    NimlValue::Text(e.name.clone()),
                ]
            })
            .collect();
        let table = MixedTable::new(
            vec![NimlValueType::String, NimlValueType::String],
            self.entries.len(),
            values,
        )
        .expect("two values per row");
        NimlElement {
            name: "VALUE_LABEL_DTABLE".into(),
            attrs: self.attrs.clone(),
            data: NimlData::Mixed(table),
        }
    }

    /// The table as a SUMA `AFNI_labeltable` group. Entries without a colour
    /// are given opaque mid-grey.
    pub fn to_afni_labeltable(&self) -> NimlElement {
        let mut values = Vec::with_capacity(self.entries.len() * 6);
        for e in &self.entries {
            let [r, g, b, a] = e.rgba.unwrap_or([0.5, 0.5, 0.5, 1.0]);
            values.extend([r, g, b, a].map(|c| NimlValue::Float(f64::from(c))));
            values.push(NimlValue::Integer(e.key));
            values.push(NimlValue::Text(e.name.clone()));
        }
        let mut column_types = vec![NimlValueType::Float32; 4];
        column_types.extend([NimlValueType::Int32, NimlValueType::String]);
        let table = MixedTable::new(column_types, self.entries.len(), values).expect("6 per row");
        let mut sparse_attrs = BTreeMap::new();
        sparse_attrs.insert("data_type".into(), "LabelTableObject_data".into());
        let atr = |name: &str, text: &str| {
            let mut attrs = BTreeMap::new();
            attrs.insert("atr_name".to_string(), name.to_string());
            NimlElement::text("AFNI_atr", attrs, text)
        };
        let mut attrs = self.attrs.clone();
        attrs
            .entry("dset_type".into())
            .or_insert_with(|| "LabelTableObject".into());
        NimlElement::group(
            "AFNI_labeltable",
            attrs,
            vec![
                NimlElement {
                    name: "SPARSE_DATA".into(),
                    attrs: sparse_attrs,
                    data: NimlData::Mixed(table),
                },
                atr("COLMS_LABS", "R;G;B;A;key;name"),
                atr(
                    "COLMS_TYPE",
                    "R_col;G_col;B_col;A_col;Node_Index_Label;Node_String_Label",
                ),
                atr("COLMS_STATSYM", "none;none;none;none;none;none"),
            ],
        )
    }

    /// Write the table as a `.niml.lt` file (`VALUE_LABEL_DTABLE`), the form
    /// `3drefit -labeltable` reads.
    pub fn write(&self, path: impl AsRef<Path>) -> Result<()> {
        error::write_file(
            path.as_ref(),
            niml::serialize(&[self.to_value_label_dtable()]).as_bytes(),
        )
    }
}

fn is_table(element: &NimlElement) -> bool {
    matches!(
        element.name.as_str(),
        "VALUE_LABEL_DTABLE" | "AFNI_labeltable"
    )
}

fn text_cell(table: &MixedTable, row: usize, column: usize) -> Option<&str> {
    match table.get(row, column)? {
        NimlValue::Text(s) => Some(s),
        _ => None,
    }
}

fn number_cell(table: &MixedTable, row: usize, column: usize) -> Option<f64> {
    match table.get(row, column)? {
        NimlValue::Integer(v) => Some(*v as f64),
        NimlValue::Float(v) => Some(*v),
        NimlValue::Text(s) => s.trim().parse().ok(),
    }
}

/// `"key" "name"` rows. An empty table has no body.
fn dtable_entries(element: &NimlElement) -> Result<Vec<LabelEntry>> {
    let table = match &element.data {
        NimlData::Mixed(t) if t.column_count() == 2 => t,
        NimlData::None => return Ok(Vec::new()),
        _ => {
            return Err(Error::parse(
                "VALUE_LABEL_DTABLE is not a 2-column string table",
            ))
        }
    };
    (0..table.rows)
        .map(|row| {
            let key = text_cell(table, row, 0)
                .and_then(|k| k.trim().parse::<i64>().ok())
                .ok_or_else(|| Error::parse(format!("VALUE_LABEL_DTABLE row {row}: bad key")))?;
            let name = text_cell(table, row, 1).unwrap_or_default().to_string();
            Ok(LabelEntry {
                key,
                name,
                rgba: None,
            })
        })
        .collect()
}

/// R, G, B, A, key, name rows of an `AFNI_labeltable`'s `SPARSE_DATA`.
fn labeltable_entries(element: &NimlElement) -> Result<Vec<LabelEntry>> {
    let table = element
        .children()
        .iter()
        .find_map(|c| match (&c.name[..], &c.data) {
            ("SPARSE_DATA", NimlData::Mixed(t)) => Some(t),
            _ => None,
        })
        .ok_or_else(|| Error::parse("AFNI_labeltable has no R,G,B,A,key,name SPARSE_DATA"))?;
    if table.column_count() < 6 {
        return Err(Error::parse(
            "AFNI_labeltable SPARSE_DATA has fewer than 6 columns",
        ));
    }
    (0..table.rows)
        .map(|row| {
            let channel = |c| number_cell(table, row, c).map(|v| v as f32);
            let rgba = match (channel(0), channel(1), channel(2), channel(3)) {
                (Some(r), Some(g), Some(b), Some(a)) => Some([r, g, b, a]),
                _ => None,
            };
            let key = number_cell(table, row, 4)
                .ok_or_else(|| Error::parse(format!("AFNI_labeltable row {row}: bad key")))?;
            Ok(LabelEntry {
                key: key as i64,
                name: text_cell(table, row, 5).unwrap_or_default().to_string(),
                rgba,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn both_layouts_round_trip() {
        let table = LabelTable {
            entries: vec![
                LabelEntry {
                    key: 0,
                    name: "Unknown".into(),
                    rgba: Some([0.0, 0.0, 0.0, 0.0]),
                },
                LabelEntry {
                    key: 17,
                    name: "Left Hippo".into(),
                    rgba: Some([0.5, 1.0, 0.25, 1.0]),
                },
            ],
            attrs: BTreeMap::new(),
        };
        let as_cmap = niml::parse_str(&niml::serialize(&[table.to_afni_labeltable()])).unwrap();
        let back = LabelTable::find(&as_cmap).unwrap().unwrap();
        assert_eq!(back.entries, table.entries);
        assert_eq!(back.key_for("Left Hippo"), Some(17));

        let as_lt = niml::parse_str(&niml::serialize(&[table.to_value_label_dtable()])).unwrap();
        let back = LabelTable::find(&as_lt).unwrap().unwrap();
        assert_eq!(back.get(17).unwrap().name, "Left Hippo");
        assert_eq!(back.get(17).unwrap().rgba, None);
    }
}
