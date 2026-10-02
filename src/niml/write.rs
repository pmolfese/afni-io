//! ASCII serialisation of NIML element trees.

use std::collections::BTreeMap;

use super::{escape, format_float, ni_type_string, NimlData, NimlElement, NimlValue};

/// Serialise a slice of elements to an ASCII NIML string.
pub fn serialize(elements: &[NimlElement]) -> String {
    let mut out = String::new();
    for element in elements {
        write_element(element, &mut out);
    }
    out
}

fn write_element(element: &NimlElement, out: &mut String) {
    // Derive the structural attributes (ni_form / ni_type / ni_dimen) from the
    // body so callers never have to keep them in sync by hand.
    let mut attrs: BTreeMap<String, String> = element.attrs.clone();
    match &element.data {
        NimlData::Group(_) => {
            attrs.insert("ni_form".into(), "ni_group".into());
        }
        NimlData::Numeric(matrix) => {
            attrs.insert("ni_type".into(), ni_type_string(&matrix.column_types));
            attrs.insert("ni_dimen".into(), matrix.rows.to_string());
        }
        NimlData::Mixed(table) => {
            attrs.insert("ni_type".into(), ni_type_string(&table.column_types));
            attrs.insert("ni_dimen".into(), table.rows.to_string());
        }
        NimlData::Text(_) => {
            attrs
                .entry("ni_type".into())
                .or_insert_with(|| "String".into());
            attrs.entry("ni_dimen".into()).or_insert_with(|| "1".into());
        }
        NimlData::None => {}
    }

    out.push('<');
    out.push_str(&element.name);
    for (key, value) in &attrs {
        out.push_str("\n  ");
        out.push_str(key);
        // An empty value is written as a bare name, which AFNI reads back as
        // a NULL right-hand side. Writing `key=""` would not round-trip:
        // SUMA's SUMA_IS_EMPTY_STR_ATTR treats NULL (and "~") as empty but ""
        // as a real value (suma_datasets.h).
        if !value.is_empty() {
            out.push_str("=\"");
            out.push_str(&escape(value));
            out.push('"');
        }
    }
    out.push_str(" >");

    match &element.data {
        NimlData::Group(children) => {
            out.push('\n');
            for child in children {
                write_element(child, out);
            }
        }
        NimlData::Numeric(matrix) => {
            for row in 0..matrix.rows {
                out.push('\n');
                for column in 0..matrix.columns() {
                    if column > 0 {
                        out.push(' ');
                    }
                    let value = matrix.get(row, column).unwrap_or(0.0);
                    if matrix.column_types[column].is_integer() {
                        out.push_str(&(value as i64).to_string());
                    } else {
                        out.push_str(&format_float(value));
                    }
                }
            }
            out.push('\n');
        }
        NimlData::Mixed(table) => {
            for row in 0..table.rows {
                out.push('\n');
                for column in 0..table.columns() {
                    if column > 0 {
                        out.push(' ');
                    }
                    if let Some(value) = table.get(row, column) {
                        out.push_str(&format_value(value));
                    }
                }
            }
            out.push('\n');
        }
        NimlData::Text(text) => {
            out.push('\n');
            out.push_str(&escape(text));
            out.push('\n');
        }
        NimlData::None => {}
    }

    out.push_str("</");
    out.push_str(&element.name);
    out.push_str(">\n");
}

fn format_value(value: &NimlValue) -> String {
    match value {
        NimlValue::Integer(v) => v.to_string(),
        NimlValue::Float(v) => format_float(*v),
        NimlValue::Text(v) => {
            if v.chars().any(|c| c.is_whitespace() || c == ';') {
                format!("\"{}\"", escape(v))
            } else {
                escape(v)
            }
        }
    }
}
