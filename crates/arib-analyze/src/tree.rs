//! Generic field tree of decoded tables, built from the decoders' own
//! `#[derive(Debug)]` output.
//!
//! The `arib-ts`/`arib-si` types carry every syntax element (reserved bits
//! included) and derive `Debug`; parsing that output yields a complete,
//! type-agnostic tree without a second, hand-maintained description of
//! each table and descriptor.  The tree is then normalised for humans:
//! `Some(x)` → `x`, `None` → null, ARIB strings decoded with `arib-text`
//! (B24 8-unit code, B10 Annex A), MJD+BCD times rendered in JST (B10
//! Annex C), BCD durations/offsets as `HH:MM:SS` / `HH:MM`, ISO 639 / 3166
//! 3-byte codes as text, byte vectors as hex, and raw `arib-ts`
//! descriptors (PMT/CAT loops) re-decoded with the typed B10 descriptor
//! decoders.
//!
//! The tree is used for JSON output, text dumps and field-level diffs.
//! It is *not* used to modify tables (see [`crate::mutate`], which edits the
//! typed structs).

use serde::ser::{SerializeMap, SerializeSeq};
use serde::{Serialize, Serializer};
use std::fmt::Write as _;

/// One node of a decoded-table tree.
#[derive(Debug, Clone, PartialEq)]
pub enum Node {
    /// `None`.
    Null,
    /// Boolean.
    Bool(bool),
    /// Integer.
    Int(i128),
    /// Plain string (Debug string literal, or a rendered time/code).
    Str(String),
    /// Decoded ARIB string (text, raw bytes).
    Text(String, Vec<u8>),
    /// Byte vector, shown as hex.
    Bytes(Vec<u8>),
    /// List.
    List(Vec<Node>),
    /// Struct / tuple struct / enum variant: type name and fields
    /// (tuple fields are named `"0"`, `"1"`, …).
    Struct(String, Vec<(String, Node)>),
}

impl Serialize for Node {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            Node::Null => s.serialize_unit(),
            Node::Bool(b) => s.serialize_bool(*b),
            Node::Int(i) => {
                if let Ok(u) = u64::try_from(*i) {
                    s.serialize_u64(u)
                } else if let Ok(v) = i64::try_from(*i) {
                    s.serialize_i64(v)
                } else {
                    s.serialize_str(&i.to_string())
                }
            }
            Node::Str(t) | Node::Text(t, _) => s.serialize_str(t),
            Node::Bytes(b) => s.serialize_str(&hex(b)),
            Node::List(v) => {
                let mut q = s.serialize_seq(Some(v.len()))?;
                for x in v {
                    q.serialize_element(x)?;
                }
                q.end()
            }
            Node::Struct(name, fields) => {
                let mut m = s.serialize_map(Some(fields.len() + 1))?;
                m.serialize_entry("_type", name)?;
                for (k, v) in fields {
                    m.serialize_entry(k, v)?;
                }
                m.end()
            }
        }
    }
}

/// Lower-case hex of `b`.
pub fn hex(b: &[u8]) -> String {
    let mut s = String::with_capacity(b.len() * 2);
    for x in b {
        let _ = write!(s, "{x:02x}");
    }
    s
}

/// Parse lower/upper-case hex (whitespace ignored).
pub fn unhex(s: &str) -> Option<Vec<u8>> {
    let t: Vec<u8> = s.bytes().filter(|c| !c.is_ascii_whitespace()).collect();
    if !t.len().is_multiple_of(2) {
        return None;
    }
    t.chunks(2)
        .map(|c| u8::from_str_radix(std::str::from_utf8(c).ok()?, 16).ok())
        .collect()
}

// ---------------------------------------------------------------------------
// Debug-output parser
// ---------------------------------------------------------------------------

struct P<'a> {
    s: &'a [u8],
    i: usize,
}

impl P<'_> {
    fn ws(&mut self) {
        while self.i < self.s.len() && self.s[self.i].is_ascii_whitespace() {
            self.i += 1;
        }
    }
    fn peek(&self) -> Option<u8> {
        self.s.get(self.i).copied()
    }
    fn eat(&mut self, c: u8) -> bool {
        self.ws();
        if self.peek() == Some(c) {
            self.i += 1;
            true
        } else {
            false
        }
    }
    fn ident(&mut self) -> String {
        self.ws();
        let st = self.i;
        while self.i < self.s.len() {
            let c = self.s[self.i];
            if c.is_ascii_alphanumeric() || c == b'_' {
                self.i += 1;
            } else if c == b':' && self.s.get(self.i + 1) == Some(&b':') {
                self.i += 2;
            } else {
                break;
            }
        }
        String::from_utf8_lossy(&self.s[st..self.i]).into_owned()
    }
    fn string(&mut self, quote: u8) -> String {
        // Rust Debug string literal; escapes \" \\ \n \t \r \0 \' \u{..}
        self.i += 1;
        let mut out = String::new();
        let mut bytes = Vec::new();
        while self.i < self.s.len() {
            let c = self.s[self.i];
            if c == quote {
                self.i += 1;
                break;
            }
            if c == b'\\' && self.i + 1 < self.s.len() {
                let e = self.s[self.i + 1];
                self.i += 2;
                let ch = match e {
                    b'n' => '\n',
                    b't' => '\t',
                    b'r' => '\r',
                    b'0' => '\0',
                    b'u' => {
                        // \u{XXXX}
                        let st = self.i + 1;
                        while self.i < self.s.len() && self.s[self.i] != b'}' {
                            self.i += 1;
                        }
                        let h = std::str::from_utf8(&self.s[st..self.i]).unwrap_or("");
                        self.i += 1;
                        char::from_u32(u32::from_str_radix(h, 16).unwrap_or(0xFFFD))
                            .unwrap_or('\u{FFFD}')
                    }
                    other => other as char,
                };
                out.push_str(&String::from_utf8_lossy(&bytes));
                bytes.clear();
                out.push(ch);
            } else {
                bytes.push(c);
                self.i += 1;
            }
        }
        out.push_str(&String::from_utf8_lossy(&bytes));
        out
    }
    fn value(&mut self) -> Node {
        self.ws();
        match self.peek() {
            None => Node::Null,
            Some(b'[') => {
                self.i += 1;
                let mut v = Vec::new();
                loop {
                    if self.eat(b']') {
                        break;
                    }
                    v.push(self.value());
                    if !self.eat(b',') {
                        self.eat(b']');
                        break;
                    }
                }
                Node::List(v)
            }
            Some(b'"') => Node::Str(self.string(b'"')),
            Some(b'\'') => Node::Str(self.string(b'\'')),
            Some(c) if c == b'-' || c.is_ascii_digit() => {
                let st = self.i;
                self.i += 1;
                while self.i < self.s.len()
                    && (self.s[self.i].is_ascii_alphanumeric()
                        || self.s[self.i] == b'.'
                        || self.s[self.i] == b'_')
                {
                    self.i += 1;
                }
                let t = std::str::from_utf8(&self.s[st..self.i]).unwrap_or("0");
                t.parse::<i128>()
                    .map(Node::Int)
                    .unwrap_or_else(|_| Node::Str(t.to_string()))
            }
            Some(_) => {
                let name = self.ident();
                if name.is_empty() {
                    // Unparseable byte: skip it.
                    self.i += 1;
                    return Node::Null;
                }
                match name.as_str() {
                    "true" => return Node::Bool(true),
                    "false" => return Node::Bool(false),
                    _ => {}
                }
                self.ws();
                match self.peek() {
                    Some(b'{') => {
                        self.i += 1;
                        let mut fields = Vec::new();
                        loop {
                            if self.eat(b'}') {
                                break;
                            }
                            // `..` (non_exhaustive) marker
                            self.ws();
                            if self.s[self.i..].starts_with(b"..") {
                                self.i += 2;
                                continue;
                            }
                            let k = self.ident();
                            self.eat(b':');
                            let v = self.value();
                            fields.push((k, v));
                            if !self.eat(b',') {
                                self.eat(b'}');
                                break;
                            }
                        }
                        Node::Struct(name, fields)
                    }
                    Some(b'(') => {
                        self.i += 1;
                        let mut fields = Vec::new();
                        let mut n = 0;
                        loop {
                            if self.eat(b')') {
                                break;
                            }
                            fields.push((n.to_string(), self.value()));
                            n += 1;
                            if !self.eat(b',') {
                                self.eat(b')');
                                break;
                            }
                        }
                        Node::Struct(name, fields)
                    }
                    _ => {
                        if name == "None" {
                            Node::Null
                        } else {
                            Node::Struct(name, Vec::new())
                        }
                    }
                }
            }
        }
    }
}

/// Parse `format!("{:?}", x)` of a derive(Debug) value into a raw tree.
pub fn parse_debug(s: &str) -> Node {
    P {
        s: s.as_bytes(),
        i: 0,
    }
    .value()
}

/// Build the normalised tree of any decoded value.
pub fn of<T: std::fmt::Debug>(v: &T) -> Node {
    normalize(parse_debug(&format!("{v:?}")), "")
}

/// Field names whose `Vec<u8>` values are opaque bytes (shown as hex).
const BYTE_FIELDS: &[&str] = &[
    "data",
    "private_data",
    "selector",
    "bytes",
    "additional_identification_info",
    "additional_data_component_info",
    "additional_info",
    "reserved_future_use_bytes",
    "component_refs",
    "description",
    "stuffing",
    "pack_header",
    "reserved_bytes",
    "transport_private_data",
    "private_data_byte",
];

fn ints(v: &[Node]) -> Option<Vec<i128>> {
    v.iter()
        .map(|n| if let Node::Int(i) = n { Some(*i) } else { None })
        .collect()
}

fn code3(v: &[Node]) -> Option<String> {
    let i = ints(v)?;
    if i.len() == 3 && i.iter().all(|&c| (0x20..0x7F).contains(&c)) {
        Some(i.iter().map(|&c| c as u8 as char).collect())
    } else {
        None
    }
}

fn field_int(fields: &[(String, Node)], k: &str) -> Option<i128> {
    fields
        .iter()
        .find(|(n, _)| n == k)
        .and_then(|(_, v)| if let Node::Int(i) = v { Some(*i) } else { None })
}

fn bcd2(b: u32) -> String {
    format!("{:x}{:x}", (b >> 4) & 0xF, b & 0xF)
}

/// Normalise a raw tree (see module docs).  `field` is the name of the
/// field holding `n` (drives code/bytes detection).
pub fn normalize(n: Node, field: &str) -> Node {
    match n {
        Node::Struct(name, mut fields) => {
            match name.as_str() {
                "Some" if fields.len() == 1 => return normalize(fields.pop().unwrap().1, field),
                "AribString" if fields.len() == 1 => {
                    if let Node::List(v) = &fields[0].1
                        && let Some(i) = ints(v)
                    {
                        let raw: Vec<u8> = i.iter().map(|&x| x as u8).collect();
                        let text = arib_text::AribString(raw.clone()).to_string_lossy();
                        return Node::Text(text, raw);
                    }
                }
                "MjdTime" => {
                    if let (Some(mjd), Some(bcd)) =
                        (field_int(&fields, "mjd"), field_int(&fields, "bcd_hms"))
                    {
                        let t = arib_si::time::MjdTime {
                            mjd: mjd as u16,
                            bcd_hms: bcd as u32,
                        };
                        return Node::Str(if t.is_undefined() {
                            "undefined".into()
                        } else if let Some(dt) = t.to_jst() {
                            format!("{dt} JST")
                        } else {
                            format!("invalid(mjd={mjd} bcd={bcd:06x})")
                        });
                    }
                }
                "BcdDuration" if fields.len() == 1 => {
                    if let Node::Int(v) = fields[0].1 {
                        let v = v as u32;
                        return Node::Str(if v == 0xFF_FFFF {
                            "undefined".into()
                        } else {
                            format!("{}:{}:{}", bcd2(v >> 16), bcd2(v >> 8), bcd2(v))
                        });
                    }
                }
                "BcdOffset" if fields.len() == 1 => {
                    if let Node::Int(v) = fields[0].1 {
                        let v = v as u32;
                        return Node::Str(format!("{}:{}", bcd2(v >> 8), bcd2(v)));
                    }
                }
                // arib-ts raw descriptor (PMT/CAT loops): decode with the
                // typed B10 descriptor decoders.
                "Descriptor"
                    if fields.len() == 2 && fields[0].0 == "tag" && fields[1].0 == "data" =>
                {
                    if let (Node::Int(tag), Node::List(d)) = (&fields[0].1, &fields[1].1)
                        && let Some(d) = ints(d)
                    {
                        let data: Vec<u8> = d.iter().map(|&x| x as u8).collect();
                        return match arib_si::Descriptor::decode_body(*tag as u8, &data) {
                            Ok(td) => descriptor(&td),
                            Err(e) => Node::Struct(
                                "Undecodable".into(),
                                vec![
                                    ("tag".into(), Node::Int(*tag)),
                                    ("data".into(), Node::Bytes(data)),
                                    ("error".into(), Node::Str(e.to_string())),
                                ],
                            ),
                        };
                    }
                }
                _ => {}
            }
            let fields: Vec<(String, Node)> = fields
                .into_iter()
                .map(|(k, v)| {
                    let ctx = if k.chars().all(|c| c.is_ascii_digit()) {
                        field.to_string()
                    } else {
                        k.clone()
                    };
                    let v = normalize(v, &ctx);
                    (k, v)
                })
                .collect();
            // Collapse single-field tuple wrappers (enum variants / newtypes):
            // `NetworkName(NetworkName { .. })` → `NetworkName { .. }`,
            // `NitBody(NetworkLoops { .. })` → `NitBody { .. }`.
            if fields.len() == 1 && fields[0].0 == "0" {
                let inner = fields.into_iter().next().unwrap().1;
                return match inner {
                    Node::Struct(_, f) => Node::Struct(name, f),
                    other => Node::Struct(name, vec![("0".into(), other)]),
                };
            }
            Node::Struct(name, fields)
        }
        Node::List(v) => {
            let is_code = field.contains("language")
                || field.contains("country_code")
                || field.contains("country_codes");
            if is_code && let Some(c) = code3(&v) {
                return Node::Str(c);
            }
            if BYTE_FIELDS.contains(&field)
                && let Some(i) = ints(&v)
                && i.iter().all(|&x| (0..=255).contains(&x))
            {
                return Node::Bytes(i.iter().map(|&x| x as u8).collect());
            }
            Node::List(v.into_iter().map(|x| normalize(x, field)).collect())
        }
        other => other,
    }
}

/// Tree of a typed B10 descriptor; the variant name becomes the node name
/// and its tag is added as the first field.
pub fn descriptor(d: &arib_si::Descriptor) -> Node {
    match of(d) {
        Node::Struct(name, mut f) => {
            if !f.iter().any(|(k, _)| k == "tag") {
                f.insert(0, ("tag".into(), Node::Int(d.tag() as i128)));
            }
            Node::Struct(name, f)
        }
        other => other,
    }
}

/// Whether integer field `k` is better shown in hex.
pub fn hex_field(k: &str) -> bool {
    k.ends_with("_id")
        || k.ends_with("_ids")
        || k.ends_with("pid")
        || k.ends_with("_tag")
        || k == "tag"
        || k.ends_with("type")
        || k.ends_with("_extension")
        || k == "table_id"
        || k == "last_table_id"
        || k.starts_with("reserved")
        || k.ends_with("area_code")
        || k == "program_number"
        || k.ends_with("_flags")
        || k == "transport_stream_id"
        || k == "original_network_id"
}

/// One-line rendering of a scalar (or compact rendering of a small list).
pub fn scalar(n: &Node, k: &str) -> String {
    match n {
        Node::Null => "-".into(),
        Node::Bool(b) => b.to_string(),
        Node::Int(i) => {
            if hex_field(k) && *i >= 0 {
                if *i > 0xFF {
                    format!("{i:#06x}")
                } else {
                    format!("{i:#04x}")
                }
            } else if k == "frequencies" || k == "frequency" {
                format!("{i} ({:.6} MHz)", *i as f64 / 7.0)
            } else {
                i.to_string()
            }
        }
        Node::Str(s) => format!("{s:?}"),
        Node::Text(t, _) => format!("\"{t}\""),
        Node::Bytes(b) => {
            if b.is_empty() {
                "''".into()
            } else {
                format!("h'{}'", hex(b))
            }
        }
        Node::List(v) => {
            let items: Vec<String> = v.iter().map(|x| scalar(x, k)).collect();
            format!("[{}]", items.join(", "))
        }
        Node::Struct(name, f) => {
            let items: Vec<String> = f
                .iter()
                .map(|(fk, v)| format!("{fk}: {}", scalar(v, fk)))
                .collect();
            format!("{name} {{ {} }}", items.join(", "))
        }
    }
}

fn is_scalar(n: &Node) -> bool {
    match n {
        Node::List(v) => {
            v.iter()
                .all(|x| matches!(x, Node::Int(_) | Node::Str(_) | Node::Bool(_)))
                && v.len() <= 32
        }
        Node::Struct(..) => false,
        _ => true,
    }
}

/// Indented multi-line rendering.
pub fn render(n: &Node, indent: usize, out: &mut String) {
    let pad = "  ".repeat(indent);
    match n {
        Node::Struct(name, fields) => {
            let _ = writeln!(out, "{pad}{name}");
            render_fields(fields, indent + 1, out);
        }
        Node::List(v) if !is_scalar(n) => {
            for (i, x) in v.iter().enumerate() {
                let _ = writeln!(out, "{pad}[{i}]");
                render(x, indent + 1, out);
            }
        }
        other => {
            let _ = writeln!(out, "{pad}{}", scalar(other, ""));
        }
    }
}

fn render_fields(fields: &[(String, Node)], indent: usize, out: &mut String) {
    let pad = "  ".repeat(indent);
    for (k, v) in fields {
        match v {
            Node::Struct(name, f) => {
                let _ = writeln!(out, "{pad}{k}: {name}");
                render_fields(f, indent + 1, out);
            }
            Node::List(items) if !is_scalar(v) => {
                if items.is_empty() {
                    let _ = writeln!(out, "{pad}{k}: []");
                }
                for (i, x) in items.iter().enumerate() {
                    match x {
                        Node::Struct(name, f) => {
                            let _ = writeln!(out, "{pad}{k}[{i}]: {name}");
                            render_fields(f, indent + 1, out);
                        }
                        other => {
                            let _ = writeln!(out, "{pad}{k}[{i}]: {}", scalar(other, k));
                        }
                    }
                }
            }
            other => {
                let _ = writeln!(out, "{pad}{k}: {}", scalar(other, k));
            }
        }
    }
}

/// Look up a field of a struct node.
pub fn get<'a>(n: &'a Node, k: &str) -> Option<&'a Node> {
    match n {
        Node::Struct(_, f) => f.iter().find(|(name, _)| name == k).map(|(_, v)| v),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[allow(dead_code)]
    #[derive(Debug)]
    struct Inner {
        name: arib_text::AribString,
        language: [u8; 3],
        data: Vec<u8>,
        ids: Vec<u16>,
    }
    #[allow(dead_code)]
    #[derive(Debug)]
    enum E {
        A(Inner),
        B {
            x: Option<u8>,
            y: Option<u8>,
            s: String,
        },
        U,
    }

    #[test]
    fn parses_debug_and_normalises() {
        let v = vec![
            E::A(Inner {
                name: arib_text::AribString::encode("ABC").unwrap(),
                language: *b"jpn",
                data: vec![1, 0xAB],
                ids: vec![0x400, 2],
            }),
            E::B {
                x: Some(3),
                y: None,
                s: "q\"z\\".into(),
            },
            E::U,
        ];
        let n = of(&v);
        let Node::List(items) = n else { panic!() };
        let Node::Struct(name, f) = &items[0] else {
            panic!()
        };
        assert_eq!(name, "A");
        assert!(matches!(&f[0].1, Node::Text(t, _) if t == "ABC"));
        assert_eq!(f[1].1, Node::Str("jpn".into()));
        assert_eq!(f[2].1, Node::Bytes(vec![1, 0xAB]));
        assert_eq!(f[3].1, Node::List(vec![Node::Int(0x400), Node::Int(2)]));
        let Node::Struct(_, f) = &items[1] else {
            panic!()
        };
        assert_eq!(f[0].1, Node::Int(3));
        assert_eq!(f[1].1, Node::Null);
        assert_eq!(f[2].1, Node::Str("q\"z\\".into()));
        assert_eq!(items[2], Node::Struct("U".into(), vec![]));
    }

    #[test]
    fn mjd_time_rendered_jst() {
        // B10 §5.2.7 Example 1: 0xC079124500 = 93/10/13 12:45:00
        let t = arib_si::time::MjdTime::from_u40(0xC0_7912_4500);
        let n = of(&t);
        assert_eq!(n, Node::Str(format!("{} JST", t.to_jst().unwrap())));
        assert!(matches!(&n, Node::Str(s) if s.contains("1993") && s.contains("12:45:00")));
    }
}
