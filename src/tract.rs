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
// Reading and writing FATCAT tract files (`.niml.tract`), the NIML `<network>` that
// `3dTrackID` writes. This is the FILE layer; tracts as geometry (length, selection,
// bounds) live in `afni_core::tract`, and `adapt::{tracts_to_core, tracts_from_core}`
// converts between the two.
//
// THE FILE
//
//   <network N_tracts="4" ni_form="ni_group">
//     <tracts ni_type="TAYLOR_TRACT_DATUM" ni_dimen="2" Bundle_Tag="7"
//             Bundle_Alt_Tag="3" Bundle_Ends="A-B"> ... </tracts>   one per bundle
//     <tracts ...> ... </tracts>
//     optional grid / FA datasets (groups)
//   </network>
//
// Each `TAYLOR_TRACT_DATUM` row is `id, N_values, x y z x y z ...`: note that the
// second number counts VALUES (three per point), not points. Coordinates are AFNI's
// DICOM frame. AFNI can also write each tract as its own `<tract id=".." >` element
// with x, y, z columns (the slow mode); those are read into one untagged bundle.
//
// Everything this module does not interpret (the grid and FA datasets, other
// attributes) is kept in `extras`/`attrs` and written back.
// ---------------------------------------------------------------------------

//! FATCAT `.niml.tract` files: the raw file model.

use std::collections::BTreeMap;
use std::path::Path;

use crate::error::{Error, Result};
use crate::niml::{self, NimlData, NimlElement, NimlValueType, RecordTable};

/// One tract as stored.
#[derive(Debug, Clone, PartialEq)]
pub struct RawTract {
    /// The tract id.
    pub id: i32,
    /// Its points (DICOM frame).
    pub points: Vec<[f32; 3]>,
}

/// One `<tracts>` element.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct RawBundle {
    /// `Bundle_Tag`.
    pub tag: Option<i32>,
    /// `Bundle_Alt_Tag`.
    pub alt_tag: Option<i32>,
    /// `Bundle_Ends`.
    pub ends: Option<String>,
    /// Other attributes (not `ni_type`, `ni_dimen` or the three above).
    pub attrs: BTreeMap<String, String>,
    /// The tracts, in file order.
    pub tracts: Vec<RawTract>,
}

/// A `.niml.tract` network.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct TractNetwork {
    /// Attributes of `<network>` other than `N_tracts` (which is recomputed).
    pub root_attrs: BTreeMap<String, String>,
    /// The bundles, in file order.
    pub bundles: Vec<RawBundle>,
    /// Other children of the network (grid and FA datasets), kept verbatim.
    pub extras: Vec<NimlElement>,
}

const BUNDLE_ATTRS: [&str; 6] = [
    "ni_type",
    "ni_dimen",
    "ni_form",
    "Bundle_Tag",
    "Bundle_Alt_Tag",
    "Bundle_Ends",
];

fn parse_i32(attrs: &BTreeMap<String, String>, key: &str) -> Result<Option<i32>> {
    attrs
        .get(key)
        .map(|v| {
            v.trim()
                .parse::<i32>()
                .map_err(|_| Error::parse(format!("invalid {key} {v:?}")))
        })
        .transpose()
}

impl TractNetwork {
    /// Interpret a `<network>` element.
    pub fn from_element(root: &NimlElement) -> Result<Self> {
        if root.name != "network" {
            return Err(Error::parse(format!(
                "expected <network>, got <{}>",
                root.name
            )));
        }
        let NimlData::Group(children) = &root.data else {
            return Err(Error::parse("the tract network is not a NIML group"));
        };
        let mut bundles = Vec::new();
        let mut extras = Vec::new();
        // Slow-mode single tracts gather into one untagged bundle.
        let mut singles: Vec<RawTract> = Vec::new();
        for child in children {
            match child.name.as_str() {
                "tracts" => {
                    let NimlData::Records(table) = &child.data else {
                        return Err(Error::parse(
                            "<tracts> does not hold TAYLOR_TRACT_DATUM rows",
                        ));
                    };
                    let tracts = table
                        .rows
                        .iter()
                        .map(|row| {
                            let (id, count, values) = match row.as_slice() {
                                [id, count, values] if id.len() == 1 && count.len() == 1 => {
                                    (id[0], count[0], values)
                                }
                                _ => return Err(Error::parse("malformed TAYLOR_TRACT_DATUM row")),
                            };
                            if values.len() % 3 != 0 || count as usize != values.len() {
                                return Err(Error::parse(format!(
                                    "tract {id}: {} values announced, {} found (need a multiple of 3)",
                                    count,
                                    values.len()
                                )));
                            }
                            Ok(RawTract {
                                id: id as i32,
                                points: values
                                    .chunks_exact(3)
                                    .map(|p| [p[0] as f32, p[1] as f32, p[2] as f32])
                                    .collect(),
                            })
                        })
                        .collect::<Result<Vec<_>>>()?;
                    let mut attrs = child.attrs.clone();
                    for k in BUNDLE_ATTRS {
                        attrs.remove(k);
                    }
                    attrs.remove("Column_Labels");
                    bundles.push(RawBundle {
                        tag: parse_i32(&child.attrs, "Bundle_Tag")?,
                        alt_tag: parse_i32(&child.attrs, "Bundle_Alt_Tag")?,
                        ends: child.attrs.get("Bundle_Ends").cloned(),
                        attrs,
                        tracts,
                    });
                }
                "tract" => {
                    let NimlData::Numeric(m) = &child.data else {
                        return Err(Error::parse("<tract> is not numeric"));
                    };
                    if m.columns.len() < 3 {
                        return Err(Error::parse("<tract> needs x, y and z columns"));
                    }
                    let c: Vec<Vec<f64>> =
                        m.columns.iter().take(3).map(|a| a.to_f64_vec()).collect();
                    singles.push(RawTract {
                        id: parse_i32(&child.attrs, "id")?.unwrap_or(0),
                        points: (0..m.rows)
                            .map(|r| [c[0][r] as f32, c[1][r] as f32, c[2][r] as f32])
                            .collect(),
                    });
                }
                _ => extras.push(child.clone()),
            }
        }
        if !singles.is_empty() {
            bundles.push(RawBundle {
                tracts: singles,
                ..RawBundle::default()
            });
        }
        if bundles.is_empty() {
            return Err(Error::invalid("the tract network has no bundles"));
        }
        if let Some(declared) = root.attrs.get("N_tracts") {
            let declared: usize = declared
                .trim()
                .parse()
                .map_err(|_| Error::parse(format!("invalid N_tracts {declared:?}")))?;
            let actual: usize = bundles.iter().map(|b| b.tracts.len()).sum();
            if declared != actual {
                return Err(Error::invalid(format!(
                    "the network declares {declared} tracts but holds {actual}"
                )));
            }
        }
        let mut root_attrs = root.attrs.clone();
        root_attrs.remove("N_tracts");
        Ok(Self {
            root_attrs,
            bundles,
            extras,
        })
    }

    /// Build the `<network>` element (always in the fast, one-element-per-bundle form).
    pub fn to_element(&self) -> NimlElement {
        let mut children: Vec<NimlElement> = self
            .bundles
            .iter()
            .map(|b| {
                let mut attrs = b.attrs.clone();
                attrs.insert("Column_Labels".into(), "TaylorTract".into());
                if let Some(t) = b.tag {
                    attrs.insert("Bundle_Tag".into(), t.to_string());
                }
                if let Some(t) = b.alt_tag {
                    attrs.insert("Bundle_Alt_Tag".into(), t.to_string());
                }
                if let Some(e) = &b.ends {
                    attrs.insert("Bundle_Ends".into(), e.clone());
                }
                let rows = b
                    .tracts
                    .iter()
                    .map(|t| {
                        let values: Vec<f64> = t
                            .points
                            .iter()
                            .flat_map(|p| p.iter().map(|&c| f64::from(c)))
                            .collect();
                        vec![vec![f64::from(t.id)], vec![values.len() as f64], values]
                    })
                    .collect();
                NimlElement {
                    name: "tracts".into(),
                    attrs,
                    data: NimlData::Records(RecordTable {
                        record_type: NimlValueType::TaylorTractDatum,
                        rows,
                    }),
                }
            })
            .collect();
        children.extend(self.extras.iter().cloned());
        let mut attrs = self.root_attrs.clone();
        let total: usize = self.bundles.iter().map(|b| b.tracts.len()).sum();
        attrs.insert("N_tracts".into(), total.to_string());
        NimlElement::group("network", attrs, children)
    }

    /// Read a `.niml.tract` file (ASCII or binary NIML).
    pub fn read(path: impl AsRef<Path>) -> Result<Self> {
        let elements = niml::read(path)?;
        match elements.as_slice() {
            [root] => Self::from_element(root),
            _ => Err(Error::parse("expected one top-level tract network")),
        }
    }

    /// Write as ASCII NIML.
    pub fn write(&self, path: impl AsRef<Path>) -> Result<()> {
        niml::write(path, &[self.to_element()])
    }

    /// Write with binary record bodies (what 3dTrackID writes by default).
    pub fn write_binary(&self, path: impl AsRef<Path>) -> Result<()> {
        niml::write_binary(path, &[self.to_element()])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TWO: &str = r#"<network N_tracts="3" ni_form="ni_group">
<tracts ni_type="TAYLOR_TRACT_DATUM" ni_dimen="2" Bundle_Tag="7" Bundle_Alt_Tag="3" Bundle_Ends="A-B">
7 6 1 2 3 4 5 6
8 3 -1 -2 -3
</tracts>
<tracts ni_type="TAYLOR_TRACT_DATUM" ni_dimen="1" Bundle_Tag="8">
9 6 0 0 0 1 1 1
</tracts>
</network>"#;

    #[test]
    fn reads_bundles_and_tracts() {
        let n = TractNetwork::from_element(&niml::parse_str(TWO).unwrap()[0]).unwrap();
        assert_eq!(n.bundles.len(), 2);
        assert_eq!(n.bundles[0].tag, Some(7));
        assert_eq!(n.bundles[0].alt_tag, Some(3));
        assert_eq!(n.bundles[0].ends.as_deref(), Some("A-B"));
        assert_eq!(
            n.bundles[0].tracts[0].points,
            vec![[1.0, 2.0, 3.0], [4.0, 5.0, 6.0]]
        );
        assert_eq!(n.bundles[0].tracts[1].id, 8);
        assert_eq!(n.bundles[1].alt_tag, None);
    }

    #[test]
    fn round_trips_through_ascii_and_binary() {
        let n = TractNetwork::from_element(&niml::parse_str(TWO).unwrap()[0]).unwrap();
        let ascii = niml::serialize(&[n.to_element()]);
        assert_eq!(
            TractNetwork::from_element(&niml::parse_str(&ascii).unwrap()[0]).unwrap(),
            n
        );
        let bin = niml::serialize_binary(&[n.to_element()]);
        assert_eq!(
            TractNetwork::from_element(&niml::parse(&bin).unwrap()[0]).unwrap(),
            n
        );
    }

    #[test]
    fn rejects_inconsistent_files() {
        // Declared count that does not match.
        let bad = TWO.replace("N_tracts=\"3\"", "N_tracts=\"4\"");
        assert!(TractNetwork::from_element(&niml::parse_str(&bad).unwrap()[0]).is_err());
        // Values not a multiple of three.
        let ragged = TWO.replace("8 3 -1 -2 -3", "8 2 -1 -2");
        assert!(TractNetwork::from_element(&niml::parse_str(&ragged).unwrap()[0]).is_err());
        // Not a network, and no bundles.
        assert!(TractNetwork::from_element(
            &niml::parse_str("<tracts ni_dimen=\"0\"></tracts>").unwrap()[0]
        )
        .is_err());
        let empty = niml::parse_str(r#"<network ni_form="ni_group"></network>"#).unwrap();
        assert!(TractNetwork::from_element(&empty[0]).is_err());
    }

    #[test]
    fn single_tract_elements_become_one_untagged_bundle() {
        let slow = r#"<network N_tracts="2" ni_form="ni_group">
<tract id="4" ni_type="3*float" ni_dimen="2" Column_Labels="x;y;z">1 2 3 4 5 6</tract>
<tract id="5" ni_type="3*float" ni_dimen="1" Column_Labels="x;y;z">7 8 9</tract>
</network>"#;
        let n = TractNetwork::from_element(&niml::parse_str(slow).unwrap()[0]).unwrap();
        assert_eq!(n.bundles.len(), 1);
        assert_eq!(n.bundles[0].tag, None);
        assert_eq!(n.bundles[0].tracts[0].id, 4);
        assert_eq!(n.bundles[0].tracts[1].points, vec![[7.0, 8.0, 9.0]]);
    }
}
