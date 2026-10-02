//! Byte-level NIML parser (ASCII and binary element bodies).

use std::collections::BTreeMap;

use crate::error::{from_utf8, Error, Result};

use super::{
    expand_ni_type, unescape, MixedTable, NimlData, NimlElement, NimlValue, NimlValueType,
    NumericMatrix, RecordField, RecordTable,
};
use crate::array;

/// Parse every top-level element from a NIML byte stream.
pub fn parse_bytes(bytes: &[u8]) -> Result<Vec<NimlElement>> {
    Parser::new(bytes).parse_all()
}

/// Parse the complete top-level elements at the start of a NIML stream that
/// may still be arriving (e.g. an AFNI or SUMA socket).
///
/// Returns the elements and how many bytes they used; any partial element
/// after them is left for the next call, once more bytes have arrived
/// (`buffer.drain(..consumed)`, append, call again). Only input that can
/// never become valid NIML is an error. Processing instructions such as
/// `<?ni_do ... ?>` are skipped and count as consumed.
///
/// An element whose closing tag never comes is indistinguishable from one
/// still arriving, so a reader should cap how much it buffers.
pub fn parse_stream(bytes: &[u8]) -> Result<(Vec<NimlElement>, usize)> {
    let mut parser = Parser::new(bytes);
    let mut elements = Vec::new();
    let mut consumed = 0;
    loop {
        parser.skip_whitespace();
        if parser.pos >= bytes.len() {
            // Only blanks after the last element: they are consumed too.
            return Ok((elements, bytes.len()));
        }
        let start = parser.pos;
        let step = if parser.peek(b"<?") {
            parser.consume_until(b">").map(|()| None)
        } else {
            parser.parse_element().map(Some)
        };
        match step {
            Ok(element) => {
                elements.extend(element);
                consumed = parser.pos;
            }
            Err(_) if parser.hit_end => {
                // Ran out of input inside this element: wait for more.
                let _ = start;
                return Ok((elements, consumed));
            }
            Err(e) => return Err(e),
        }
    }
}

struct Parser<'a> {
    input: &'a [u8],
    pos: usize,
    /// Set when parsing failed only because the input ended early, which
    /// [`parse_stream`] treats as "wait for more bytes".
    hit_end: bool,
}

impl<'a> Parser<'a> {
    fn new(input: &'a [u8]) -> Self {
        Self {
            input,
            pos: 0,
            hit_end: false,
        }
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

    /// An error for input that stopped before `what` was complete.
    fn ended(&mut self, what: impl std::fmt::Display) -> Error {
        self.hit_end = true;
        Error::parse(format!("NIML input ended inside {what}"))
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
                if self.pos >= self.input.len()
                    || end_marker.as_bytes().starts_with(&self.input[self.pos..])
                {
                    return Err(self.ended(format_args!("group {name}")));
                }
                children.push(self.parse_element()?);
            }
            self.expect(end_marker.as_bytes())?;
            NimlData::Group(children)
        } else {
            let end_marker = format!("</{name}>");
            if is_binary(&attrs) {
                let Some(payload_len) = binary_payload_len(&attrs, &self.input[self.pos..])? else {
                    return Err(self.ended(format_args!("the binary body of {name}")));
                };
                let body = &self.input[self.pos..self.pos + payload_len];
                self.pos += payload_len;
                self.skip_whitespace();
                self.expect(end_marker.as_bytes())?;
                parse_body(&attrs, body)?
            } else {
                let Some(rel_end) = find_bytes(&self.input[self.pos..], end_marker.as_bytes())
                else {
                    return Err(self.ended(format_args!("{name} (no closing tag yet)")));
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
        let rest = &self.input[self.pos..];
        if rest.len() < token.len() && token.starts_with(rest) {
            return Err(self.ended(format_args!("{:?}", String::from_utf8_lossy(token))));
        }
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
            return Err(self.ended(format_args!(
                "a section ending in {:?}",
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
            if self.pos >= self.input.len() {
                return Err(self.ended("an element name"));
            }
            return Err(Error::parse("NIML element has no name"));
        }
        if self.pos >= self.input.len() {
            return Err(self.ended("an element name"));
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
        Err(self.ended("an element header"))
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

    let record_type = record_type(&column_types);
    let decoded;
    let body = if is_base64(attrs) {
        let text = from_utf8(body, "NIML base64 payload")?;
        decoded = crate::base64::decode(&text)?;
        decoded.as_slice()
    } else {
        body
    };
    if is_binary(attrs) || is_base64(attrs) {
        let little = byte_order_is_little(attrs.get("ni_form").map_or("binary", String::as_str))?;
        if let Some(record_type) = record_type {
            return Ok(NimlData::Records(parse_binary_records(
                body,
                record_type,
                rows,
                little,
            )?));
        }
        if !column_types.iter().all(NimlValueType::is_numeric) {
            return Err(Error::unsupported(
                "binary NIML payloads with string columns",
            ));
        }
        let need: usize = column_types
            .iter()
            .map(NimlValueType::byte_width)
            .sum::<Result<usize>>()?
            * rows;
        if body.len() < need {
            return Err(Error::parse(format!(
                "binary NIML body has {} bytes but {rows} rows need {need}",
                body.len()
            )));
        }
        return Ok(NimlData::Numeric(parse_binary_matrix(
            body,
            &column_types,
            rows,
            little,
        )?));
    }

    let text = from_utf8(body, "NIML text payload")?;

    if let Some(record_type) = record_type {
        return Ok(NimlData::Records(parse_ascii_records(
            &text,
            record_type,
            rows,
        )?));
    }

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

/// Decode a row-interleaved binary matrix into one typed column per type.
fn parse_binary_matrix(
    body: &[u8],
    column_types: &[NimlValueType],
    rows: usize,
    little: bool,
) -> Result<NumericMatrix> {
    let widths = column_types
        .iter()
        .map(NimlValueType::byte_width)
        .collect::<Result<Vec<_>>>()?;
    let row_width: usize = widths.iter().sum();
    let mut offset = 0;
    let mut columns = Vec::with_capacity(column_types.len());
    for (ty, width) in column_types.iter().zip(&widths) {
        let dtype = ty.data_type().expect("numeric column");
        let bytes: Vec<u8> = if column_types.len() == 1 {
            body[..rows * width].to_vec()
        } else {
            (0..rows)
                .flat_map(|r| &body[r * row_width + offset..r * row_width + offset + width])
                .copied()
                .collect()
        };
        columns.push(array::decode_binary(&bytes, dtype, little, rows)?);
        offset += width;
    }
    Ok(NumericMatrix { rows, columns })
}

/// The rowtype of a variable-length record element, if `column_types` is one.
fn record_type(column_types: &[NimlValueType]) -> Option<&NimlValueType> {
    match column_types {
        [ty] if ty.record_fields().is_some() => Some(ty),
        _ => None,
    }
}

/// Walk `rows` binary records of `fields` at the start of `body`, calling
/// `visit` with each record. Returns the bytes used, or `None` if `body` ends
/// first.
fn walk_binary_records(
    body: &[u8],
    fields: &[RecordField],
    rows: usize,
    little: bool,
    mut visit: impl FnMut(Vec<Vec<f64>>),
) -> Result<Option<usize>> {
    let mut pos = 0;
    for _ in 0..rows {
        let mut record: Vec<Vec<f64>> = Vec::with_capacity(fields.len());
        for field in fields {
            let count = match field.length_from {
                None => 1,
                Some(k) => {
                    let n = record
                        .get(k)
                        .and_then(|v| v.first())
                        .copied()
                        .unwrap_or(0.0);
                    if !(0.0..=1e9).contains(&n) || n.fract() != 0.0 {
                        return Err(Error::parse(format!("invalid NIML record length {n}")));
                    }
                    n as usize
                }
            };
            let width = field.ty.elem_size();
            let Some(chunk) = body.get(pos..pos + count * width) else {
                return Ok(None);
            };
            record.push(array::decode_binary(chunk, field.ty, little, count)?.to_f64_vec());
            pos += count * width;
        }
        visit(record);
    }
    Ok(Some(pos))
}

fn parse_binary_records(
    body: &[u8],
    record_type: &NimlValueType,
    rows: usize,
    little: bool,
) -> Result<RecordTable> {
    let fields = record_type.record_fields().expect("record type");
    let mut out = Vec::with_capacity(rows);
    walk_binary_records(body, &fields, rows, little, |r| out.push(r))?
        .ok_or_else(|| Error::parse("binary NIML records ended early"))?;
    Ok(RecordTable {
        record_type: record_type.clone(),
        rows: out,
    })
}

fn parse_ascii_records(
    text: &str,
    record_type: &NimlValueType,
    rows: usize,
) -> Result<RecordTable> {
    let fields = record_type.record_fields().expect("record type");
    let mut tokens = text.split_whitespace().map(|t| {
        t.parse::<f64>().map_err(|_| {
            Error::parse(format!(
                "invalid {} value {t:?}",
                record_type.canonical_name()
            ))
        })
    });
    let mut out = Vec::with_capacity(rows);
    // ni_dimen gives the record count; read to the end if it is missing.
    while rows == 0 || out.len() < rows {
        let mut record: Vec<Vec<f64>> = Vec::with_capacity(fields.len());
        for field in &fields {
            let count = match field.length_from {
                None => 1,
                Some(k) => record[k][0].max(0.0) as usize,
            };
            let mut values = Vec::with_capacity(count);
            for _ in 0..count {
                match tokens.next() {
                    Some(v) => values.push(v?),
                    None if record.is_empty() && values.is_empty() && rows == 0 => {
                        return Ok(RecordTable {
                            record_type: record_type.clone(),
                            rows: out,
                        });
                    }
                    None => {
                        return Err(Error::parse(format!(
                            "{} record {} is incomplete",
                            record_type.canonical_name(),
                            out.len()
                        )))
                    }
                }
            }
            record.push(values);
        }
        out.push(record);
    }
    Ok(RecordTable {
        record_type: record_type.clone(),
        rows: out,
    })
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

/// Byte order of a binary or base64 body, as `niml_elemio.c` decides it: by
/// substring, and big-endian unless the form mentions `lsb`. So a bare
/// `ni_form="binary"` is big-endian, whatever machine wrote or reads it.
fn byte_order_is_little(ni_form: &str) -> Result<bool> {
    Ok(ni_form.to_ascii_lowercase().contains("lsb"))
}

/// Byte length of a binary element body at the start of `rest`, or `None` if
/// `rest` does not hold all of it yet. Fixed-width rows are counted from
/// `ni_dimen`; variable-length records are walked.
fn binary_payload_len(attrs: &BTreeMap<String, String>, rest: &[u8]) -> Result<Option<usize>> {
    let ni_type = attrs
        .get("ni_type")
        .ok_or_else(|| Error::missing("ni_type on binary NIML element"))?;
    let rows: usize = attrs
        .get("ni_dimen")
        .ok_or_else(|| Error::missing("ni_dimen on binary NIML element"))?
        .parse()
        .map_err(|_| Error::parse("invalid binary NIML ni_dimen"))?;
    let column_types = expand_ni_type(ni_type)?;
    if let Some(record_type) = record_type(&column_types) {
        let little = byte_order_is_little(attrs.get("ni_form").map_or("binary", String::as_str))?;
        let fields = record_type.record_fields().expect("record type");
        return walk_binary_records(rest, &fields, rows, little, |_| {});
    }
    let row_width: usize = column_types
        .iter()
        .map(NimlValueType::byte_width)
        .collect::<Result<Vec<_>>>()?
        .into_iter()
        .sum();
    let len = rows
        .checked_mul(row_width)
        .ok_or_else(|| Error::invalid("binary NIML payload size overflows"))?;
    Ok((len <= rest.len()).then_some(len))
}

/// A raw binary body (`ni_form` contains `binary`, as AFNI tests it).
fn is_binary(attrs: &BTreeMap<String, String>) -> bool {
    attrs
        .get("ni_form")
        .is_some_and(|v| v.to_ascii_lowercase().contains("binary"))
}

/// A base64-encoded binary body (`ni_form` contains `base64`). It is text up
/// to the closing tag, and decodes to the same bytes a binary body holds.
fn is_base64(attrs: &BTreeMap<String, String>) -> bool {
    attrs
        .get("ni_form")
        .is_some_and(|v| v.to_ascii_lowercase().contains("base64"))
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
