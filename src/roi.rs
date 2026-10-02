//! Drawn surface ROIs: the `Node_ROI` NIML element written as `.niml.roi`.
//!
//! SUMA's "Draw ROI" tool stores each ROI as a single `Node_ROI` element whose
//! body is a list of `SUMA_NIML_ROI_DATUM` records. Each record is a
//! variable-length integer row:
//!
//! ```text
//! <action> <element-type> <n> <node_0> <node_1> ... <node_{n-1}>
//! ```
//!
//! SUMA writes `.niml.roi` files with the entire element commented out (each
//! line prefixed by `#`); [`crate::niml::parse`] removes those prefixes before
//! parsing, so both commented and bare files are accepted here.
//!
//! References: `afni/src/SUMA/SUMA_Surface_IO.c:SUMA_OpenDrawnROI_NIML`,
//! `afni/src/SUMA/SUMA_define.h:SUMA_ROI_DATUM`.

use std::collections::BTreeMap;
use std::path::Path;

use crate::error::{self, Error, Result};
use crate::niml::{self, NimlData, NimlElement, NimlValueType};

/// An RGBA colour with components in `[0, 1]`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rgba {
    /// Red.
    pub r: f32,
    /// Green.
    pub g: f32,
    /// Blue.
    pub b: f32,
    /// Alpha (opacity). Defaults to 1.0 when a file gives only three values.
    pub a: f32,
}

/// One `SUMA_NIML_ROI_DATUM` record within a [`NodeRoi`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoiDatum {
    /// The action code that produced this stroke (append, fill, join, …).
    pub action: i32,
    /// The element-type code (node group, edge group, face group, segment).
    pub element_type: i32,
    /// The ordered list of node indices that make up this stroke.
    pub nodes: Vec<u32>,
}

/// A single drawn ROI on a surface.
#[derive(Debug, Clone, PartialEq)]
pub struct NodeRoi {
    /// The ROI's own ID code (`self_idcode` / `idcode_str`), if present.
    pub self_idcode: Option<String>,
    /// The ID code of the surface this ROI was drawn on
    /// (`domain_parent_idcode` / `Parent_idcode_str`).
    pub domain_parent_idcode: Option<String>,
    /// The hemisphere / side string (`Parent_side`), e.g. `L`, `R`.
    pub parent_side: Option<String>,
    /// The human-readable ROI label (`Label`).
    pub label: String,
    /// The integer label assigned to the ROI (`iLabel`).
    pub integer_label: i32,
    /// The drawing-type code (`Type`): 0 open path, 1 closed path, 2 filled,
    /// 4 collection.
    pub roi_type: Option<i32>,
    /// The colour-plane name (`ColPlaneName`).
    pub color_plane: Option<String>,
    /// The fill colour (`FillColor`), if present.
    pub fill_color: Option<Rgba>,
    /// The edge colour (`EdgeColor`), if present.
    pub edge_color: Option<Rgba>,
    /// The edge thickness (`EdgeThickness`), if present.
    pub edge_thickness: Option<u32>,
    /// The ordered stroke records.
    pub data: Vec<RoiDatum>,
}

impl NodeRoi {
    /// Read every `Node_ROI` from a `.niml.roi` file.
    pub fn read_all(path: impl AsRef<Path>) -> Result<Vec<Self>> {
        Self::from_bytes(&error::read_file(path.as_ref())?)
    }

    /// Parse every `Node_ROI` element from raw NIML bytes.
    pub fn from_bytes(bytes: &[u8]) -> Result<Vec<Self>> {
        niml::parse(bytes)?
            .iter()
            .filter(|e| e.name == "Node_ROI")
            .map(Self::from_element)
            .collect()
    }

    /// Interpret an already-parsed `Node_ROI` element.
    pub fn from_element(element: &NimlElement) -> Result<Self> {
        if element.name != "Node_ROI" {
            return Err(Error::parse(format!(
                "expected Node_ROI, got {}",
                element.name
            )));
        }
        let NimlData::Text(body) = &element.data else {
            return Err(Error::parse(
                "Node_ROI body is not SUMA_NIML_ROI_DATUM text",
            ));
        };
        let data = parse_datum_records(body)?;
        let attrs = &element.attrs;

        Ok(Self {
            self_idcode: first_attr(attrs, &["self_idcode", "idcode_str", "Object_ID"]),
            domain_parent_idcode: first_attr(
                attrs,
                &["domain_parent_idcode", "Parent_idcode_str", "Parent_ID"],
            ),
            parent_side: attrs.get("Parent_side").cloned(),
            label: attrs.get("Label").cloned().unwrap_or_else(|| "ROI".into()),
            integer_label: parse_i32(attrs, "iLabel")?.unwrap_or(0),
            roi_type: parse_i32(attrs, "Type")?,
            color_plane: attrs.get("ColPlaneName").cloned(),
            fill_color: attrs.get("FillColor").map(|v| parse_rgba(v)).transpose()?,
            edge_color: attrs.get("EdgeColor").map(|v| parse_rgba(v)).transpose()?,
            edge_thickness: parse_u32(attrs, "EdgeThickness")?,
            data,
        })
    }

    /// The sorted, de-duplicated set of node indices across all strokes.
    pub fn unique_nodes(&self) -> Vec<u32> {
        let mut nodes: Vec<u32> = self
            .data
            .iter()
            .flat_map(|d| d.nodes.iter().copied())
            .collect();
        nodes.sort_unstable();
        nodes.dedup();
        nodes
    }

    /// Build the `Node_ROI` NIML element for this ROI.
    pub fn to_element(&self) -> NimlElement {
        let mut attrs = BTreeMap::new();
        if let Some(v) = &self.self_idcode {
            attrs.insert("self_idcode".into(), v.clone());
        }
        if let Some(v) = &self.domain_parent_idcode {
            attrs.insert("domain_parent_idcode".into(), v.clone());
        }
        if let Some(v) = &self.parent_side {
            attrs.insert("Parent_side".into(), v.clone());
        }
        attrs.insert("Label".into(), self.label.clone());
        attrs.insert("iLabel".into(), self.integer_label.to_string());
        if let Some(v) = self.roi_type {
            attrs.insert("Type".into(), v.to_string());
        }
        if let Some(v) = &self.color_plane {
            attrs.insert("ColPlaneName".into(), v.clone());
        }
        if let Some(v) = self.fill_color {
            attrs.insert("FillColor".into(), rgba_to_string(v));
        }
        if let Some(v) = self.edge_color {
            attrs.insert("EdgeColor".into(), rgba_to_string(v));
        }
        if let Some(v) = self.edge_thickness {
            attrs.insert("EdgeThickness".into(), v.to_string());
        }
        // Mark the column type explicitly so the writer emits the right ni_type.
        attrs.insert(
            "ni_type".into(),
            NimlValueType::SumaRoiDatum.canonical_name().into(),
        );
        attrs.insert("ni_dimen".into(), self.data.len().to_string());

        NimlElement {
            name: "Node_ROI".into(),
            attrs,
            data: NimlData::Text(serialize_datum_records(&self.data)),
        }
    }

    /// Serialise this ROI to an ASCII NIML string.
    pub fn to_niml_string(&self) -> String {
        niml::serialize(&[self.to_element()])
    }

    /// Write a set of ROIs to a `.niml.roi` file.
    pub fn write_all(path: impl AsRef<Path>, rois: &[NodeRoi]) -> Result<()> {
        let elements: Vec<NimlElement> = rois.iter().map(NodeRoi::to_element).collect();
        error::write_file(path.as_ref(), niml::serialize(&elements).as_bytes())
    }
}

fn parse_datum_records(body: &str) -> Result<Vec<RoiDatum>> {
    let values = body
        .split_whitespace()
        .map(|t| {
            t.parse::<i32>()
                .map_err(|_| Error::parse(format!("invalid SUMA_NIML_ROI_DATUM value {t:?}")))
        })
        .collect::<Result<Vec<_>>>()?;

    let mut records = Vec::new();
    let mut pos = 0;
    while pos < values.len() {
        if pos + 3 > values.len() {
            return Err(Error::parse("malformed SUMA_NIML_ROI_DATUM record header"));
        }
        let action = values[pos];
        let element_type = values[pos + 1];
        let count = values[pos + 2];
        if count < 0 {
            return Err(Error::invalid(
                "SUMA_NIML_ROI_DATUM has negative node count",
            ));
        }
        pos += 3;
        let count = count as usize;
        if pos + count > values.len() {
            return Err(Error::parse("malformed SUMA_NIML_ROI_DATUM node path"));
        }
        let mut nodes = Vec::with_capacity(count);
        for value in &values[pos..pos + count] {
            if *value < 0 {
                return Err(Error::invalid("SUMA_NIML_ROI_DATUM node index is negative"));
            }
            nodes.push(*value as u32);
        }
        pos += count;
        records.push(RoiDatum {
            action,
            element_type,
            nodes,
        });
    }
    Ok(records)
}

fn serialize_datum_records(records: &[RoiDatum]) -> String {
    let mut out = String::new();
    for (i, record) in records.iter().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        out.push_str(&format!(
            "{} {} {}",
            record.action,
            record.element_type,
            record.nodes.len()
        ));
        for node in &record.nodes {
            out.push(' ');
            out.push_str(&node.to_string());
        }
    }
    out
}

fn first_attr(attrs: &BTreeMap<String, String>, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|k| attrs.get(*k).cloned())
}

fn parse_i32(attrs: &BTreeMap<String, String>, key: &str) -> Result<Option<i32>> {
    attrs
        .get(key)
        .map(|v| {
            v.parse::<i32>()
                .map_err(|_| Error::parse(format!("invalid {key}")))
        })
        .transpose()
}

fn parse_u32(attrs: &BTreeMap<String, String>, key: &str) -> Result<Option<u32>> {
    attrs
        .get(key)
        .map(|v| {
            v.parse::<u32>()
                .map_err(|_| Error::parse(format!("invalid {key}")))
        })
        .transpose()
}

fn parse_rgba(value: &str) -> Result<Rgba> {
    let c = value
        .split_whitespace()
        .map(|p| {
            p.parse::<f32>()
                .map_err(|_| Error::parse(format!("invalid colour component {p:?}")))
        })
        .collect::<Result<Vec<_>>>()?;
    if c.len() != 3 && c.len() != 4 {
        return Err(Error::invalid(
            "ROI colour must have three or four components",
        ));
    }
    Ok(Rgba {
        r: c[0],
        g: c[1],
        b: c[2],
        a: c.get(3).copied().unwrap_or(1.0),
    })
}

fn rgba_to_string(c: Rgba) -> String {
    format!(
        "{} {} {} {}",
        niml::format_float(c.r as f64),
        niml::format_float(c.g as f64),
        niml::format_float(c.b as f64),
        niml::format_float(c.a as f64)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
# <Node_ROI
#  ni_type = "SUMA_NIML_ROI_DATUM"
#  ni_dimen = "2"
#  self_idcode = "XYZ_ROI"
#  Parent_side = "L"
#  Label = "V1"
#  iLabel = "7"
#  Type = "4"
#  FillColor = "0.5 0.1 0.9 1.0"
#  EdgeThickness = "2"
# >
 1 4 3 1 2 3
 4 1 2 8 9
# </Node_ROI>
"#;

    #[test]
    fn reads_commented_roi() {
        let rois = NodeRoi::from_bytes(SAMPLE.as_bytes()).unwrap();
        assert_eq!(rois.len(), 1);
        let roi = &rois[0];
        assert_eq!(roi.self_idcode.as_deref(), Some("XYZ_ROI"));
        assert_eq!(roi.parent_side.as_deref(), Some("L"));
        assert_eq!(roi.label, "V1");
        assert_eq!(roi.integer_label, 7);
        assert_eq!(
            roi.fill_color,
            Some(Rgba {
                r: 0.5,
                g: 0.1,
                b: 0.9,
                a: 1.0
            })
        );
        assert_eq!(roi.data[0].action, 1);
        assert_eq!(roi.data[0].element_type, 4);
        assert_eq!(roi.data[0].nodes, vec![1, 2, 3]);
        assert_eq!(roi.unique_nodes(), vec![1, 2, 3, 8, 9]);
    }

    #[test]
    fn round_trips_records() {
        let roi = NodeRoi::from_bytes(SAMPLE.as_bytes()).unwrap().remove(0);
        let serialized = roi.to_niml_string();
        let reparsed = NodeRoi::from_bytes(serialized.as_bytes())
            .unwrap()
            .remove(0);
        assert_eq!(reparsed.label, "V1");
        assert_eq!(reparsed.data, roi.data);
    }
}
