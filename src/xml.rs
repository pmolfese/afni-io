//! A small XML reader, sufficient for the GIfTI 1.0 schema.
//!
//! It builds a simple DOM ([`XmlNode`]) and handles the constructs GIfTI files
//! actually use: the `<?xml?>` prolog, `<!DOCTYPE ...>`, comments, attributes
//! (single or double quoted), self-closing tags, `<![CDATA[...]]>` sections,
//! and the five predefined entities plus numeric character references.
//!
//! It is intentionally not a full XML 1.0 implementation (no namespaces,
//! no DTD validation, no entity definitions), but it round-trips the element
//! tree GIfTI depends on.

use crate::error::{Error, Result};

/// A parsed XML element.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct XmlNode {
    /// The tag name.
    pub name: String,
    /// Attributes in document order.
    pub attrs: Vec<(String, String)>,
    /// Child elements.
    pub children: Vec<XmlNode>,
    /// Concatenated direct text/CDATA content (entity-decoded).
    pub text: String,
}

impl XmlNode {
    /// The value of attribute `name`, if present.
    pub fn attr(&self, name: &str) -> Option<&str> {
        self.attrs
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }

    /// The first child element with the given tag name.
    pub fn child(&self, name: &str) -> Option<&XmlNode> {
        self.children.iter().find(|c| c.name == name)
    }

    /// An iterator over child elements with the given tag name.
    pub fn children_named<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a XmlNode> + 'a {
        self.children.iter().filter(move |c| c.name == name)
    }

    /// The text of the first child element with the given tag name.
    pub fn child_text(&self, name: &str) -> Option<&str> {
        self.child(name).map(|c| c.text.as_str())
    }
}

/// Parse an XML document, returning its root element.
pub fn parse(xml: &str) -> Result<XmlNode> {
    let mut parser = Parser {
        bytes: xml.as_bytes(),
        pos: 0,
    };
    parser.skip_prolog()?;
    let root = parser.parse_element()?;
    Ok(root)
}

struct Parser<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Parser<'a> {
    fn skip_ws(&mut self) {
        while self
            .bytes
            .get(self.pos)
            .is_some_and(u8::is_ascii_whitespace)
        {
            self.pos += 1;
        }
    }

    fn starts_with(&self, token: &[u8]) -> bool {
        self.bytes[self.pos..].starts_with(token)
    }

    fn rest(&self) -> &'a [u8] {
        &self.bytes[self.pos..]
    }

    /// Skip the prolog: declaration, DOCTYPE, comments, and whitespace, up to
    /// the first real element.
    fn skip_prolog(&mut self) -> Result<()> {
        loop {
            self.skip_ws();
            if self.starts_with(b"<?") {
                self.consume_until(b"?>")?;
            } else if self.starts_with(b"<!--") {
                self.consume_until(b"-->")?;
            } else if self.starts_with(b"<!DOCTYPE") {
                self.skip_doctype()?;
            } else {
                return Ok(());
            }
        }
    }

    /// DOCTYPE may carry an internal subset in `[...]`; skip to the matching
    /// top-level `>`.
    fn skip_doctype(&mut self) -> Result<()> {
        let mut depth = 0i32;
        while let Some(&byte) = self.bytes.get(self.pos) {
            self.pos += 1;
            match byte {
                b'[' => depth += 1,
                b']' => depth -= 1,
                b'>' if depth <= 0 => return Ok(()),
                _ => {}
            }
        }
        Err(Error::parse("XML: unterminated DOCTYPE"))
    }

    fn consume_until(&mut self, marker: &[u8]) -> Result<()> {
        match find(self.rest(), marker) {
            Some(i) => {
                self.pos += i + marker.len();
                Ok(())
            }
            None => Err(Error::parse(format!(
                "XML: missing {:?}",
                String::from_utf8_lossy(marker)
            ))),
        }
    }

    fn parse_element(&mut self) -> Result<XmlNode> {
        self.skip_ws();
        if !self.starts_with(b"<") {
            return Err(Error::parse("XML: expected element"));
        }
        self.pos += 1; // '<'

        let name = self.read_name()?;
        let mut node = XmlNode {
            name,
            ..XmlNode::default()
        };

        // Attributes.
        loop {
            self.skip_ws();
            match self.bytes.get(self.pos) {
                Some(b'/') => {
                    // Self-closing.
                    self.pos += 1;
                    self.expect(b'>')?;
                    return Ok(node);
                }
                Some(b'>') => {
                    self.pos += 1;
                    break;
                }
                Some(_) => {
                    let (key, value) = self.read_attribute()?;
                    node.attrs.push((key, value));
                }
                None => return Err(Error::parse("XML: unterminated start tag")),
            }
        }

        // Content until the matching close tag.
        loop {
            if self.starts_with(b"<![CDATA[") {
                self.pos += b"<![CDATA[".len();
                let end = find(self.rest(), b"]]>")
                    .ok_or_else(|| Error::parse("XML: unterminated CDATA"))?;
                node.text.push_str(
                    std::str::from_utf8(&self.rest()[..end])
                        .map_err(|e| Error::parse(format!("XML: invalid UTF-8 in CDATA: {e}")))?,
                );
                self.pos += end + 3;
            } else if self.starts_with(b"<!--") {
                self.consume_until(b"-->")?;
            } else if self.starts_with(b"</") {
                self.pos += 2;
                let close = self.read_name()?;
                self.skip_ws();
                self.expect(b'>')?;
                if close != node.name {
                    return Err(Error::parse(format!(
                        "XML: </{}> does not match <{}>",
                        close, node.name
                    )));
                }
                return Ok(node);
            } else if self.starts_with(b"<") {
                node.children.push(self.parse_element()?);
            } else {
                // Text run up to the next '<'.
                let end = find(self.rest(), b"<")
                    .ok_or_else(|| Error::parse("XML: unterminated element text"))?;
                let raw = std::str::from_utf8(&self.rest()[..end])
                    .map_err(|e| Error::parse(format!("XML: invalid UTF-8 in text: {e}")))?;
                node.text.push_str(&decode_entities(raw));
                self.pos += end;
            }
        }
    }

    fn read_name(&mut self) -> Result<String> {
        let start = self.pos;
        while let Some(&byte) = self.bytes.get(self.pos) {
            if byte.is_ascii_whitespace() || byte == b'>' || byte == b'/' || byte == b'=' {
                break;
            }
            self.pos += 1;
        }
        if self.pos == start {
            return Err(Error::parse("XML: empty name"));
        }
        Ok(String::from_utf8_lossy(&self.bytes[start..self.pos]).into_owned())
    }

    fn read_attribute(&mut self) -> Result<(String, String)> {
        let key = self.read_name()?;
        self.skip_ws();
        self.expect(b'=')?;
        self.skip_ws();
        let quote = match self.bytes.get(self.pos) {
            Some(&q @ (b'"' | b'\'')) => q,
            _ => {
                return Err(Error::parse(format!(
                    "XML: attribute {key} value not quoted"
                )))
            }
        };
        self.pos += 1;
        let end = find(self.rest(), &[quote])
            .ok_or_else(|| Error::parse(format!("XML: attribute {key} not terminated")))?;
        let raw = std::str::from_utf8(&self.rest()[..end])
            .map_err(|e| Error::parse(format!("XML: invalid UTF-8 in attribute: {e}")))?;
        let value = decode_entities(raw);
        self.pos += end + 1;
        Ok((key, value))
    }

    fn expect(&mut self, byte: u8) -> Result<()> {
        if self.bytes.get(self.pos) == Some(&byte) {
            self.pos += 1;
            Ok(())
        } else {
            Err(Error::parse(format!("XML: expected {:?}", byte as char)))
        }
    }
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() {
        return Some(0);
    }
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// Decode the five predefined entities and numeric character references.
fn decode_entities(text: &str) -> String {
    if !text.contains('&') {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len());
    let mut chars = text.char_indices().peekable();
    while let Some((i, ch)) = chars.next() {
        if ch != '&' {
            out.push(ch);
            continue;
        }
        if let Some(semi) = text[i..].find(';') {
            let entity = &text[i + 1..i + semi];
            let decoded = match entity {
                "lt" => Some('<'),
                "gt" => Some('>'),
                "amp" => Some('&'),
                "quot" => Some('"'),
                "apos" => Some('\''),
                _ if entity.starts_with("#x") || entity.starts_with("#X") => {
                    u32::from_str_radix(&entity[2..], 16)
                        .ok()
                        .and_then(char::from_u32)
                }
                _ if entity.starts_with('#') => {
                    entity[1..].parse::<u32>().ok().and_then(char::from_u32)
                }
                _ => None,
            };
            if let Some(c) = decoded {
                out.push(c);
                // Advance past the entity body and ';'.
                for _ in 0..semi {
                    chars.next();
                }
                continue;
            }
        }
        out.push('&');
    }
    out
}

/// Escape text for inclusion in XML element content or a CDATA-free attribute.
pub fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_nested_with_cdata_and_doctype() {
        let xml = r#"<?xml version="1.0"?>
<!DOCTYPE GIFTI SYSTEM "gifti.dtd">
<GIFTI Version="1.0">
  <MetaData>
    <MD><Name><![CDATA[date]]></Name><Value><![CDATA[today]]></Value></MD>
  </MetaData>
  <Empty/>
</GIFTI>"#;
        let root = parse(xml).unwrap();
        assert_eq!(root.name, "GIFTI");
        assert_eq!(root.attr("Version"), Some("1.0"));
        let md = root.child("MetaData").unwrap().child("MD").unwrap();
        assert_eq!(md.child_text("Name"), Some("date"));
        assert_eq!(md.child_text("Value"), Some("today"));
        assert!(root.child("Empty").is_some());
    }

    #[test]
    fn decodes_entities() {
        let root = parse(r#"<a x="1 &lt; 2">a &amp; b &#65;</a>"#).unwrap();
        assert_eq!(root.attr("x"), Some("1 < 2"));
        assert_eq!(root.text.trim(), "a & b A");
    }
}
