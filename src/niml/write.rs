//! Serialisation of NIML element trees, as ASCII or binary.
//!
//! Both forms derive the structural attributes (`ni_form`, `ni_type`,
//! `ni_dimen`) from the body, so callers never keep them in step by hand.
//! Binary output (`ni_form="binary.lsbfirst"`) applies to numeric matrices
//! and variable-length records; text and mixed elements are always ASCII,
//! which AFNI accepts element by element in the same stream.

use std::collections::BTreeMap;

use super::{escape, format_float, ni_type_string, NimlData, NimlElement, NimlValue};

/// Serialise a slice of elements to an ASCII NIML string.
pub fn serialize(elements: &[NimlElement]) -> String {
    let mut out = Vec::new();
    for element in elements {
        write_element(element, false, &mut out);
    }
    String::from_utf8(out).expect("ASCII NIML is UTF-8")
}

/// Serialise a slice of elements with numeric and record bodies in
/// little-endian binary (`ni_form="binary.lsbfirst"`), as AFNI and SUMA send
/// large payloads such as surface coordinates over their sockets. Groups,
/// text and mixed elements are written as in [`serialize`].
pub fn serialize_binary(elements: &[NimlElement]) -> Vec<u8> {
    let mut out = Vec::new();
    for element in elements {
        write_element(element, true, &mut out);
    }
    out
}

fn write_element(element: &NimlElement, binary: bool, out: &mut Vec<u8>) {
    let mut attrs: BTreeMap<String, String> = element.attrs.clone();
    // A stale ni_form (e.g. "binary.msbfirst" from a parsed file) must not
    // describe a body written differently.
    attrs.remove("ni_form");
    let binary_body = binary && matches!(element.data, NimlData::Numeric(_) | NimlData::Records(_));
    match &element.data {
        NimlData::Group(_) => {
            attrs.insert("ni_form".into(), "ni_group".into());
        }
        NimlData::Numeric(matrix) => {
            attrs.insert("ni_type".into(), ni_type_string(&matrix.column_types()));
            attrs.insert("ni_dimen".into(), matrix.rows.to_string());
        }
        NimlData::Records(table) => {
            attrs.insert("ni_type".into(), table.record_type.canonical_name().into());
            attrs.insert("ni_dimen".into(), table.rows.len().to_string());
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
    if binary_body {
        attrs.insert("ni_form".into(), "binary.lsbfirst".into());
    }

    let mut head = String::new();
    head.push('<');
    head.push_str(&element.name);
    for (key, value) in &attrs {
        head.push_str("\n  ");
        head.push_str(key);
        // An empty value is written as a bare name, which AFNI reads back as
        // a NULL right-hand side. Writing `key=""` would not round-trip:
        // SUMA's SUMA_IS_EMPTY_STR_ATTR treats NULL (and "~") as empty but ""
        // as a real value (suma_datasets.h).
        if !value.is_empty() {
            head.push_str("=\"");
            head.push_str(&escape(value));
            head.push('"');
        }
    }
    head.push_str(" >");
    out.extend_from_slice(head.as_bytes());

    let mut text = String::new();
    match &element.data {
        NimlData::Group(children) => {
            out.push(b'\n');
            for child in children {
                write_element(child, binary, out);
            }
        }
        NimlData::Numeric(matrix) if binary_body => {
            let widths: Vec<usize> = matrix
                .columns
                .iter()
                .map(|c| c.dtype().elem_size())
                .collect();
            let bytes: Vec<Vec<u8>> = matrix.columns.iter().map(|c| c.to_bytes(true)).collect();
            for row in 0..matrix.rows {
                for (column, width) in bytes.iter().zip(&widths) {
                    out.extend_from_slice(&column[row * width..(row + 1) * width]);
                }
            }
        }
        NimlData::Records(table) if binary_body => {
            let fields = table.fields();
            for r in 0..table.rows.len() {
                for (values, field) in table.normalized_row(r).iter().zip(&fields) {
                    let array = super::typed_from_f64(field.ty, values.iter().copied());
                    out.extend_from_slice(&array.to_bytes(true));
                }
            }
        }
        NimlData::Numeric(matrix) => {
            let integer: Vec<bool> = matrix
                .column_types()
                .iter()
                .map(|t| t.is_integer())
                .collect();
            for row in 0..matrix.rows {
                text.push('\n');
                for (column, &is_integer) in integer.iter().enumerate() {
                    if column > 0 {
                        text.push(' ');
                    }
                    let value = matrix.get(row, column).unwrap_or(0.0);
                    if is_integer {
                        text.push_str(&(value as i64).to_string());
                    } else {
                        text.push_str(&format_float(value));
                    }
                }
            }
            text.push('\n');
        }
        NimlData::Records(table) => {
            let fields = table.fields();
            for r in 0..table.rows.len() {
                text.push('\n');
                let mut first = true;
                for (values, field) in table.normalized_row(r).iter().zip(&fields) {
                    for &v in values {
                        if !first {
                            text.push(' ');
                        }
                        first = false;
                        if field.ty.is_float() {
                            text.push_str(&format_float(v));
                        } else {
                            text.push_str(&(v as i64).to_string());
                        }
                    }
                }
            }
            text.push('\n');
        }
        NimlData::Mixed(table) => {
            for row in 0..table.rows {
                text.push('\n');
                for column in 0..table.column_count() {
                    if column > 0 {
                        text.push(' ');
                    }
                    if let Some(value) = table.get(row, column) {
                        text.push_str(&format_value(value));
                    }
                }
            }
            text.push('\n');
        }
        NimlData::Text(body) => {
            // A String/CString body must be quoted: AFNI reads an unquoted
            // string only up to the first blank. Other text is written as is.
            let quoted = matches!(
                attrs.get("ni_type").map(String::as_str),
                Some("String" | "CString")
            );
            text.push('\n');
            if quoted {
                text.push('"');
            }
            text.push_str(&escape(body));
            if quoted {
                text.push('"');
            }
            text.push('\n');
        }
        NimlData::None => {}
    }
    out.extend_from_slice(text.as_bytes());

    out.extend_from_slice(b"</");
    out.extend_from_slice(element.name.as_bytes());
    out.extend_from_slice(b">\n");
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
