//! The XML subset a broadcast TTML document uses: elements, attributes,
//! character data, the five predefined entities and numeric character
//! references; the XML declaration, comments, processing instructions and
//! CDATA sections are skipped or kept as text. No DTD, no external
//! entities: a document that needs them does not parse.

use std::fmt;

/// One element: its qualified name (prefix kept), attributes in order
/// and children.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Element {
    /// The qualified name as written (`tt`, `tts:origin`, …).
    pub name: String,
    /// `(qualified name, unescaped value)` pairs.
    pub attributes: Vec<(String, String)>,
    /// The content.
    pub children: Vec<Node>,
}

/// A content node.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Node {
    /// A child element.
    Element(Element),
    /// Character data, unescaped.
    Text(String),
}

impl Element {
    /// The local part of the name (after any `prefix:`).
    #[must_use]
    pub fn local_name(&self) -> &str {
        local(&self.name)
    }

    /// The value of the attribute whose *local* name is `name` (`begin`
    /// matches `begin`; `xml:id` is asked for as `id`).
    #[must_use]
    pub fn attr(&self, name: &str) -> Option<&str> {
        self.attributes
            .iter()
            .find(|(k, _)| local(k) == name)
            .map(|(_, v)| v.as_str())
    }

    /// The child elements.
    pub fn elements(&self) -> impl Iterator<Item = &Element> {
        self.children.iter().filter_map(|n| match n {
            Node::Element(e) => Some(e),
            Node::Text(_) => None,
        })
    }
}

fn local(name: &str) -> &str {
    name.rsplit_once(':').map_or(name, |(_, l)| l)
}

/// Why a document did not parse: the byte offset and what was expected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error {
    /// Byte offset into the document.
    pub offset: usize,
    /// What went wrong.
    pub what: &'static str,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "XML at byte {}: {}", self.offset, self.what)
    }
}

impl std::error::Error for Error {}

/// Parse a document to its root element.
///
/// # Errors
///
/// Anything outside the subset, or a malformed document.
pub fn parse(doc: &str) -> Result<Element, Error> {
    let mut p = Parser { s: doc, at: 0 };
    p.misc()?;
    if !p.s[p.at..].starts_with('<') {
        return Err(p.err("no root element"));
    }
    let root = p.element()?;
    p.misc()?;
    if p.at != p.s.len() {
        return Err(p.err("content after the root element"));
    }
    Ok(root)
}

struct Parser<'a> {
    s: &'a str,
    at: usize,
}

impl Parser<'_> {
    fn err(&self, what: &'static str) -> Error {
        Error {
            offset: self.at,
            what,
        }
    }

    fn rest(&self) -> &str {
        &self.s[self.at..]
    }

    fn skip_ws(&mut self) {
        let n = self.rest().len() - self.rest().trim_start().len();
        self.at += n;
    }

    /// Skip up to and past `end`.
    fn skip_past(&mut self, end: &str, what: &'static str) -> Result<(), Error> {
        match self.rest().find(end) {
            Some(i) => {
                self.at += i + end.len();
                Ok(())
            }
            None => Err(self.err(what)),
        }
    }

    /// The prolog / epilog: whitespace, the declaration, comments, PIs.
    fn misc(&mut self) -> Result<(), Error> {
        loop {
            self.skip_ws();
            if self.rest().starts_with("<?") {
                self.skip_past("?>", "an unterminated processing instruction")?;
            } else if self.rest().starts_with("<!--") {
                self.skip_past("-->", "an unterminated comment")?;
            } else if self.rest().starts_with("<!") {
                return Err(self.err("a DTD is not supported"));
            } else {
                return Ok(());
            }
        }
    }

    fn name(&mut self) -> Result<String, Error> {
        let n = self
            .rest()
            .find(|c: char| c.is_whitespace() || matches!(c, '/' | '>' | '=' | '<'))
            .unwrap_or(self.rest().len());
        if n == 0 {
            return Err(self.err("a name expected"));
        }
        let name = self.rest()[..n].to_owned();
        self.at += n;
        Ok(name)
    }

    fn element(&mut self) -> Result<Element, Error> {
        self.at += 1; // '<'
        let name = self.name()?;
        let mut attributes = Vec::new();
        loop {
            self.skip_ws();
            if self.rest().starts_with("/>") {
                self.at += 2;
                return Ok(Element {
                    name,
                    attributes,
                    children: Vec::new(),
                });
            }
            if self.rest().starts_with('>') {
                self.at += 1;
                break;
            }
            let key = self.name()?;
            self.skip_ws();
            if !self.rest().starts_with('=') {
                return Err(self.err("'=' expected after an attribute name"));
            }
            self.at += 1;
            self.skip_ws();
            let Some(quote @ ('"' | '\'')) = self.rest().chars().next() else {
                return Err(self.err("a quoted attribute value expected"));
            };
            self.at += 1;
            let Some(end) = self.rest().find(quote) else {
                return Err(self.err("an unterminated attribute value"));
            };
            let raw = &self.s[self.at..self.at + end];
            let value = unescape(raw).ok_or_else(|| self.err("a bad reference"))?;
            self.at += end + 1;
            attributes.push((key, value));
        }
        let mut children = Vec::new();
        loop {
            let r = self.rest();
            if r.is_empty() {
                return Err(self.err("an unterminated element"));
            }
            if r.starts_with("</") {
                self.at += 2;
                let close = self.name()?;
                if close != name {
                    return Err(self.err("a mismatched end tag"));
                }
                self.skip_ws();
                if !self.rest().starts_with('>') {
                    return Err(self.err("'>' expected"));
                }
                self.at += 1;
                return Ok(Element {
                    name,
                    attributes,
                    children,
                });
            }
            if r.starts_with("<!--") {
                self.skip_past("-->", "an unterminated comment")?;
            } else if let Some(body) = r.strip_prefix("<![CDATA[") {
                let Some(end) = body.find("]]>") else {
                    return Err(self.err("an unterminated CDATA section"));
                };
                let text = body[..end].to_owned();
                push_text(&mut children, &text);
                self.at += 9 + end + 3;
            } else if r.starts_with("<?") {
                self.skip_past("?>", "an unterminated processing instruction")?;
            } else if r.starts_with('<') {
                children.push(Node::Element(self.element()?));
            } else {
                let end = r.find('<').unwrap_or(r.len());
                let text = unescape(&r[..end]);
                let text = text.ok_or_else(|| self.err("a bad reference"))?;
                push_text(&mut children, &text);
                self.at += end;
            }
        }
    }
}

fn push_text(children: &mut Vec<Node>, text: &str) {
    if let Some(Node::Text(t)) = children.last_mut() {
        t.push_str(text);
    } else {
        children.push(Node::Text(text.to_owned()));
    }
}

/// Resolve the predefined entities and character references; `None` on
/// an unknown entity or a bad reference.
fn unescape(s: &str) -> Option<String> {
    if !s.contains('&') {
        return Some(s.to_owned());
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        let tail = &rest[i + 1..];
        let end = tail.find(';')?;
        let name = &tail[..end];
        let c = match name {
            "lt" => '<',
            "gt" => '>',
            "amp" => '&',
            "quot" => '"',
            "apos" => '\'',
            _ => {
                let code =
                    if let Some(h) = name.strip_prefix("#x").or_else(|| name.strip_prefix("#X")) {
                        u32::from_str_radix(h, 16).ok()?
                    } else {
                        name.strip_prefix('#')?.parse().ok()?
                    };
                char::from_u32(code)?
            }
        };
        out.push(c);
        rest = &tail[end + 1..];
    }
    out.push_str(rest);
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn elements_attributes_text() {
        let e = parse(
            "<?xml version=\"1.0\"?><!-- c --><tt xml:lang='ja'><p a=\"1 &amp; 2\">x&lt;y&#x3042;<br/>z</p></tt>",
        )
        .unwrap();
        assert_eq!(e.local_name(), "tt");
        assert_eq!(e.attr("lang"), Some("ja"));
        let p = e.elements().next().unwrap();
        assert_eq!(p.attr("a"), Some("1 & 2"));
        assert_eq!(
            p.children,
            vec![
                Node::Text("x<yあ".into()),
                Node::Element(Element {
                    name: "br".into(),
                    attributes: vec![],
                    children: vec![]
                }),
                Node::Text("z".into()),
            ]
        );
    }

    #[test]
    fn rejects_malformed() {
        assert!(parse("<a><b></a>").is_err());
        assert!(parse("<a>").is_err());
        assert!(parse("<a x=1/>").is_err());
        assert!(parse("<!DOCTYPE a><a/>").is_err());
        assert!(parse("<a>&bogus;</a>").is_err());
        assert!(parse("<a/><b/>").is_err());
    }
}
