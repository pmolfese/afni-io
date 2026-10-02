//! Byte-level NIML parser (ASCII and binary element bodies).

use std::collections::BTreeMap;

use crate::error::{from_utf8, Error, Result};

use super::{
    expand_ni_type, unescape, MixedTable, NimlData, NimlElement, NimlValue, NimlValueType,
    NumericMatrix,
};

/// Parse every top-level element from a NIML byte stream.
pub fn parse_bytes(bytes: &[u8]) -> Result<Vec<NimlElement>> {
    Parser::new(bytes).parse_all()
}

struct Parser<'a> {
    input: &'a [u8],
    pos: usize,
}

impl<'a> Parser<'a> {
    fn new(input: &'a [u8]) -> Self {
        Self { input, pos: 0 }
    }

    fn parse_all(&mut self) -> Result<Vec<NimlElement>> {
        let mut elements = Vec::new();
        loop {
            self.skip_whitespace();
            if self.pos >= self.input.len() {
                return Ok(elements);
            }
            // Skip XML declarations / processing instructions.
            if self.peek(b"<?") {
                self.consume_until(b">")?;
                continue;
            }
            elements.push(self.parse_element()?);
        }
    }

    fn parse_element(&mut self) -> Result<NimlElement> {
        self.skip_whitespace();
        self.expect(b"<")?;
        if self.peek(b"/") {
            return Err(Error::parse(format!(
                "unexpected closing tag at byte {}",
                self.pos
            )));
        }

        let name = self.read_name()?;
        let raw_header = self.read_header()?;
        let header = raw_header.trim();
        let self_closing = header.ends_with('/');
        let header = if self_closing {
            header.trim_end_matches('/').trim_end()
        } else {
            header
        };
        let attrs = parse_attrs(header)?;

        if self_closing {
            return Ok(NimlElement {
                name,
                attrs,
                data: NimlData::None,
            });
        }

        let data = if attrs.get("ni_form").is_some_and(|v| v == "ni_group") {
            let mut children = Vec::new();
            let end_marker = format!("</{name}>");
            loop {
                self.skip_whitespace();
                if self.peek(end_marker.as_bytes()) {
                    break;
                }
                if self.pos >= self.input.len() {
                    return Err(Error::parse(format!(
                        "missing closing tag for group {name}"
                    )));
                }
                children.push(self.parse_element()?);
            }
            self.expect(end_marker.as_bytes())?;
            NimlData::Group(children)
        } else {
            let end_marker = format!("</{name}>");
            if is_binary(&attrs) {
                let payload_len = binary_payload_len(&attrs)?;
                if self.pos + payload_len > self.input.len() {
                    return Err(Error::parse(format!(
                        "binary NIML payload for {name} ended early"
                    )));
                }
                let body = &self.input[self.pos..self.pos + payload_len];
                self.pos += payload_len;
                self.skip_whitespace();
                self.expect(end_marker.as_bytes())?;
                parse_body(&attrs, body)?
            } else {
                let Some(rel_end) = find_bytes(&self.input[self.pos..], end_marker.as_bytes())
                else {
                    return Err(Error::parse(format!("missing closing tag for {name}")));
                };
                let end = self.pos + rel_end;
                let body = &self.input[self.pos..end];
                self.pos = end + end_marker.len();
                parse_body(&attrs, body)?
            }
        };

        Ok(NimlElement { name, attrs, data })
    }

    fn skip_whitespace(&mut self) {
        while self
            .input
            .get(self.pos)
            .is_some_and(u8::is_ascii_whitespace)
        {
            self.pos += 1;
        }
    }

    fn peek(&self, token: &[u8]) -> bool {
        self.input[self.pos..].starts_with(token)
    }

    fn expect(&mut self, token: &[u8]) -> Result<()> {
        if !self.peek(token) {
            return Err(Error::parse(format!(
                "expected {:?} at byte {}",
                String::from_utf8_lossy(token),
                self.pos
            )));
        }
        self.pos += token.len();
        Ok(())
    }

    fn consume_until(&mut self, token: &[u8]) -> Result<()> {
        let Some(index) = find_bytes(&self.input[self.pos..], token) else {
            return Err(Error::parse(format!(
                "did not find marker {:?}",
                String::from_utf8_lossy(token)
            )));
        };
        self.pos += index + token.len();
        Ok(())
    }

    fn read_name(&mut self) -> Result<String> {
        let start = self.pos;
        while let Some(byte) = self.input.get(self.pos) {
            if byte.is_ascii_whitespace() || *byte == b'>' || *byte == b'/' {
                break;
            }
            self.pos += 1;
        }
        if self.pos == start {
            return Err(Error::parse("NIML element has no name"));
        }
        from_utf8(&self.input[start..self.pos], "NIML tag name")
    }

    fn read_header(&mut self) -> Result<String> {
        let start = self.pos;
        let mut in_quote = false;
        while let Some(byte) = self.input.get(self.pos) {
            if *byte == b'"' {
                in_quote = !in_quote;
            } else if *byte == b'>' && !in_quote {
                let header = from_utf8(&self.input[start..self.pos], "NIML header")?;
                self.pos += 1;
                return Ok(header);
            }
            self.pos += 1;
        }
        Err(Error::parse("unterminated NIML element header"))
    }
}

fn parse_body(attrs: &BTreeMap<String, String>, body: &[u8]) -> Result<NimlData> {
    let Some(ni_type) = attrs.get("ni_type") else {
        let text = from_utf8(body, "NIML text payload")?;
        return Ok(NimlData::Text(unescape(text.trim())));
    };
    let column_types = expand_ni_type(ni_type)?;
    let rows = attrs
        .get("ni_dimen")
        .map(|v| v.parse::<usize>())
        .transpose()
        .map_err(|_| Error::parse("invalid NIML ni_dimen"))?
        .unwrap_or(0);

    if is_binary(attrs) {
        if !column_types.iter().all(NimlValueType::is_numeric) {
            return Err(Error::unsupported(
                "binary NIML payloads with string or variable columns",
            ));
        }
        return Ok(NimlData::Numeric(parse_binary_matrix(
            body,
            column_types,
            rows,
            attrs.get("ni_form").map(String::as_str),
        )?));
    }

    let text = from_utf8(body, "NIML text payload")?;

    if column_types.iter().all(NimlValueType::is_numeric) {
        return Ok(NimlData::Numeric(parse_ascii_matrix(
            &text,
            column_types,
            rows,
        )?));
    }

    if column_types.len() == 1
        && matches!(
            column_types[0],
            NimlValueType::String | NimlValueType::CString
        )
    {
        // One string becomes Text; several (AFNI splits long attributes)
        // become a one-column table.
        let mut strings = split_strings(&text);
        if strings.len() <= 1 {
            return Ok(NimlData::Text(strings.pop().unwrap_or_default()));
        }
        let rows = strings.len();
        let values = strings.into_iter().map(NimlValue::Text).collect();
        return Ok(NimlData::Mixed(MixedTable::new(
            column_types,
            rows,
            values,
        )?));
    }

    // SUMA_NIML_ROI_DATUM and other variable-length rows are exposed verbatim
    // as text; crate::roi interprets them. Anything else becomes a mixed table.
    if column_types == [NimlValueType::SumaRoiDatum] {
        return Ok(NimlData::Text(text.trim().to_string()));
    }

    Ok(NimlData::Mixed(parse_mixed(&text, column_types, rows)?))
}

/// Split the body of a `String` column into its strings, as
/// `NI_decode_one_string` (`niml_elemio.c`) does: each is either quoted with
/// `"` or `'` (up to the matching quote) or a run of non-blank characters.
/// Entities are decoded after splitting, so an escaped quote never ends a
/// string early. An unterminated quote runs to the end of the body.
///
/// A body that does not start with a quote is kept whole as one string.
/// sumaru wrote multi-word strings unquoted before October 2026 (e.g. its
/// `HISTORY_NOTE`); AFNI would read only their first word, but those files
/// should still read completely.
fn split_strings(body: &str) -> Vec<String> {
    let body = body.trim();
    if !body.starts_with(['"', '\'']) {
        return if body.is_empty() {
            Vec::new()
        } else {
            vec![unescape(body)]
        };
    }
    let bytes = body.as_bytes();
    let mut out = Vec::new();
    let mut pos = 0;
    loop {
        while bytes.get(pos).is_some_and(u8::is_ascii_whitespace) {
            pos += 1;
        }
        let Some(&first) = bytes.get(pos) else {
            break;
        };
        let (start, end, next) = if first == b'"' || first == b'\'' {
            let start = pos + 1;
            let end = body[start..]
                .find(first as char)
                .map_or(body.len(), |i| start + i);
            (start, end, end + 1)
        } else {
            let end = body[pos..]
                .find(|c: char| c.is_ascii_whitespace())
                .map_or(body.len(), |i| pos + i);
            (pos, end, end)
        };
        out.push(unescape(&body[start..end]));
        pos = next;
    }
    out
}

fn parse_ascii_matrix(
    body: &str,
    column_types: Vec<NimlValueType>,
    rows: usize,
) -> Result<NumericMatrix> {
    let values = body
        .split_whitespace()
        .map(|token| {
            token
                .parse::<f64>()
                .map_err(|_| Error::parse(format!("invalid NIML numeric value {token:?}")))
        })
        .collect::<Result<Vec<_>>>()?;
    let rows = if rows == 0 {
        values.len() / column_types.len()
    } else {
        rows
    };
    NumericMatrix::new(column_types, rows, values)
}

fn parse_binary_matrix(
    body: &[u8],
    column_types: Vec<NimlValueType>,
    rows: usize,
    ni_form: Option<&str>,
) -> Result<NumericMatrix> {
    let little = byte_order_is_little(ni_form.unwrap_or("binary"))?;
    let mut values = Vec::with_capacity(rows * column_types.len());
    let mut offset = 0;
    for _ in 0..rows {
        for ty in &column_types {
            let width = ty.byte_width()?;
            let chunk = &body[offset..offset + width];
            offset += width;
            values.push(decode_binary(ty, chunk, little));
        }
    }
    NumericMatrix::new(column_types, rows, values)
}

fn parse_mixed(body: &str, column_types: Vec<NimlValueType>, rows: usize) -> Result<MixedTable> {
    let mut parser = MixedParser::new(body);
    let mut values = Vec::with_capacity(rows * column_types.len());
    for _ in 0..rows {
        for ty in &column_types {
            values.push(parser.parse_value(ty)?);
        }
    }
    MixedTable::new(column_types, rows, values)
}

struct MixedParser<'a> {
    input: &'a str,
    pos: usize,
}

impl<'a> MixedParser<'a> {
    fn new(input: &'a str) -> Self {
        Self { input, pos: 0 }
    }

    fn parse_value(&mut self, ty: &NimlValueType) -> Result<NimlValue> {
        self.skip_delims();
        match ty {
            NimlValueType::String | NimlValueType::CString => {
                self.parse_token().map(NimlValue::Text)
            }
            ty if ty.is_integer() => {
                let token = self.parse_token()?;
                token
                    .parse::<i64>()
                    .map(NimlValue::Integer)
                    .map_err(|_| Error::parse(format!("invalid NIML integer {token:?}")))
            }
            ty if ty.is_numeric() => {
                let token = self.parse_token()?;
                token
                    .parse::<f64>()
                    .map(NimlValue::Float)
                    .map_err(|_| Error::parse(format!("invalid NIML float {token:?}")))
            }
            other => Err(Error::unsupported(format!(
                "NIML mixed value of type {}",
                other.canonical_name()
            ))),
        }
    }

    fn parse_token(&mut self) -> Result<String> {
        self.skip_delims();
        let bytes = self.input.as_bytes();
        if self.pos >= bytes.len() {
            return Err(Error::parse("NIML mixed payload ended early"));
        }
        let first = bytes[self.pos];
        if first == b'"' || first == b'\'' {
            let quote = first;
            self.pos += 1;
            let start = self.pos;
            while self.pos < bytes.len() && bytes[self.pos] != quote {
                self.pos += 1;
            }
            if self.pos >= bytes.len() {
                return Err(Error::parse("unterminated quoted NIML value"));
            }
            let token = unescape(&self.input[start..self.pos]);
            self.pos += 1;
            return Ok(token);
        }
        let start = self.pos;
        while self.pos < bytes.len()
            && !bytes[self.pos].is_ascii_whitespace()
            && bytes[self.pos] != b';'
        {
            self.pos += 1;
        }
        if self.pos == start {
            return Err(Error::parse("empty NIML mixed token"));
        }
        Ok(unescape(&self.input[start..self.pos]))
    }

    fn skip_delims(&mut self) {
        while let Some(byte) = self.input.as_bytes().get(self.pos) {
            if byte.is_ascii_whitespace() || *byte == b';' {
                self.pos += 1;
            } else {
                break;
            }
        }
    }
}

fn decode_binary(ty: &NimlValueType, bytes: &[u8], little: bool) -> f64 {
    match ty {
        NimlValueType::UInt8 => bytes[0] as f64,
        NimlValueType::Int16 => {
            let raw = [bytes[0], bytes[1]];
            if little {
                i16::from_le_bytes(raw) as f64
            } else {
                i16::from_be_bytes(raw) as f64
            }
        }
        NimlValueType::Int32 => {
            let raw = [bytes[0], bytes[1], bytes[2], bytes[3]];
            if little {
                i32::from_le_bytes(raw) as f64
            } else {
                i32::from_be_bytes(raw) as f64
            }
        }
        NimlValueType::Float32 => {
            let raw = [bytes[0], bytes[1], bytes[2], bytes[3]];
            if little {
                f32::from_le_bytes(raw) as f64
            } else {
                f32::from_be_bytes(raw) as f64
            }
        }
        NimlValueType::Float64 => {
            let raw = [
                bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
            ];
            if little {
                f64::from_le_bytes(raw)
            } else {
                f64::from_be_bytes(raw)
            }
        }
        // Unreachable: binary bodies are gated to numeric types above.
        _ => 0.0,
    }
}

fn byte_order_is_little(ni_form: &str) -> Result<bool> {
    match ni_form.to_ascii_lowercase().as_str() {
        "binary" => Ok(cfg!(target_endian = "little")),
        "binary.lsbfirst" => Ok(true),
        "binary.msbfirst" => Ok(false),
        other => Err(Error::unsupported(format!("binary NIML form {other:?}"))),
    }
}

fn binary_payload_len(attrs: &BTreeMap<String, String>) -> Result<usize> {
    let ni_type = attrs
        .get("ni_type")
        .ok_or_else(|| Error::missing("ni_type on binary NIML element"))?;
    let rows: usize = attrs
        .get("ni_dimen")
        .ok_or_else(|| Error::missing("ni_dimen on binary NIML element"))?
        .parse()
        .map_err(|_| Error::parse("invalid binary NIML ni_dimen"))?;
    let column_types = expand_ni_type(ni_type)?;
    let row_width: usize = column_types
        .iter()
        .map(NimlValueType::byte_width)
        .collect::<Result<Vec<_>>>()?
        .into_iter()
        .sum();
    Ok(rows * row_width)
}

fn is_binary(attrs: &BTreeMap<String, String>) -> bool {
    attrs
        .get("ni_form")
        .is_some_and(|v| v.to_ascii_lowercase().starts_with("binary"))
}

fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() {
        return Some(0);
    }
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// Parse the attribute portion of an element header into a map.
///
/// Follows `parse_header_stuff` in AFNI's `niml/niml_header.c`:
///
/// * an attribute may have no `=value` at all (e.g. `ConvertDset` writes a
///   bare `domain_parent_idcode`); AFNI stores a NULL right-hand side and this
///   map stores an empty string;
/// * a value may be double-quoted, single-quoted, or an unquoted run of
///   non-blank characters other than `<`, `>`, `/` and `=`.
pub(super) fn parse_attrs(header: &str) -> Result<BTreeMap<String, String>> {
    let mut attrs = BTreeMap::new();
    let bytes = header.as_bytes();
    let mut pos = 0;
    let skip_blanks = |pos: &mut usize| {
        while bytes.get(*pos).is_some_and(u8::is_ascii_whitespace) {
            *pos += 1;
        }
    };

    loop {
        skip_blanks(&mut pos);
        if pos >= bytes.len() {
            break;
        }
        let key_start = pos;
        while pos < bytes.len() && !bytes[pos].is_ascii_whitespace() && bytes[pos] != b'=' {
            pos += 1;
        }
        let key = &header[key_start..pos];
        if key.is_empty() {
            return Err(Error::parse("NIML attribute has empty name"));
        }
        skip_blanks(&mut pos);
        if bytes.get(pos) != Some(&b'=') {
            // No right-hand side.
            attrs.insert(key.to_string(), String::new());
            continue;
        }
        pos += 1;
        skip_blanks(&mut pos);
        let value = match bytes.get(pos) {
            Some(&quote @ (b'"' | b'\'')) => {
                pos += 1;
                let value_start = pos;
                while pos < bytes.len() && bytes[pos] != quote {
                    pos += 1;
                }
                if pos >= bytes.len() {
                    return Err(Error::parse(format!(
                        "NIML attribute {key} has no closing quote"
                    )));
                }
                let value = &header[value_start..pos];
                pos += 1;
                value
            }
            Some(_) => {
                let value_start = pos;
                while pos < bytes.len()
                    && !bytes[pos].is_ascii_whitespace()
                    && !matches!(bytes[pos], b'<' | b'>' | b'/' | b'=')
                {
                    pos += 1;
                }
                if pos == value_start {
                    return Err(Error::parse(format!(
                        "NIML attribute {key} has an empty value"
                    )));
                }
                &header[value_start..pos]
            }
            None => return Err(Error::parse(format!("NIML attribute {key} ends after '='"))),
        };
        attrs.insert(key.to_string(), unescape(value));
    }
    Ok(attrs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attributes_follow_afni_header_grammar() {
        let attrs = parse_attrs(
            "dset_type=\"Node_Bucket\"\n  domain_parent_idcode\n  geometry_parent_idcode \
             single='a b' bare=12.5 spaced = \"x\" escaped=\"&lt;&quot;\"",
        )
        .unwrap();
        assert_eq!(attrs["dset_type"], "Node_Bucket");
        assert_eq!(attrs["domain_parent_idcode"], "");
        assert_eq!(attrs["geometry_parent_idcode"], "");
        assert_eq!(attrs["single"], "a b");
        assert_eq!(attrs["bare"], "12.5");
        assert_eq!(attrs["spaced"], "x");
        assert_eq!(attrs["escaped"], "<\"");
        assert!(parse_attrs("open=\"never closed").is_err());
    }

    #[test]
    fn binary_little_endian_matrix() {
        let mut bytes =
            b"<SPARSE_DATA ni_type=\"float,int\" ni_dimen=\"2\" ni_form=\"binary.lsbfirst\" >"
                .to_vec();
        bytes.extend_from_slice(&1.5f32.to_le_bytes());
        bytes.extend_from_slice(&10i32.to_le_bytes());
        bytes.extend_from_slice(&2.5f32.to_le_bytes());
        bytes.extend_from_slice(&12i32.to_le_bytes());
        bytes.extend_from_slice(b"</SPARSE_DATA>");

        let elements = parse_bytes(&bytes).unwrap();
        let NimlData::Numeric(matrix) = &elements[0].data else {
            panic!("expected numeric");
        };
        assert_eq!(matrix.get(0, 0), Some(1.5));
        assert_eq!(matrix.get(1, 1), Some(12.0));
    }

    #[test]
    fn mixed_table_preserves_columns() {
        let elements = parse_bytes(
            br#"<MIXED ni_type="int,String,float" ni_dimen="2" >
1 "alpha beta" 2.5
2 gamma 3.5
</MIXED>"#,
        )
        .unwrap();
        let NimlData::Mixed(table) = &elements[0].data else {
            panic!("expected mixed");
        };
        assert_eq!(table.get(0, 1), Some(&NimlValue::Text("alpha beta".into())));
        assert_eq!(table.get(1, 2), Some(&NimlValue::Float(3.5)));
    }
}
