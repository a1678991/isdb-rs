//! Typed table mutation by path expression: decode → modify the typed
//! struct → encode (lengths and CRC_32 recomputed by the codecs).  Never a
//! byte patch.
//!
//! Grammar:
//!
//! ```text
//! expr   := table ('@' tid)? ('[' ext ']')? ('.' seg)* '=' value
//! seg    := field ('[' sel ']')?          sel: decimal index | 0x… id/tag | 0xTT#k (k-th, 0-based)
//! ops    := a final segment `add` / `del` adds/removes a list element
//! ```
//!
//! Tables: `pat`, `cat`, `pmt[program]`, `nit` (`@0x41`), `sdt` (`@0x46`),
//! `bat`, `eit[service_id]` (`@0x50` …), `tot`, `tdt`, `bit`.
//! Header fields on any long table: `version`, `tidext`, `cni`, `sn`,
//! `lsn`.  Descriptor loops are `desc` (aliases `descriptors`,
//! `program_info`, `first`); `desc.add=<hex of tag+length+body>`,
//! `desc.del=0xTT`, `desc[0xTT]=<hex>` (replace), `desc[0xTT].<field>=v`.
//!
//! Examples: `nit.ts[0].desc[0xFA].guard_interval=2`, `nit.version=24`,
//! `pat.add=0x0e28:0x1fff`, `sdt.services[0x0400].desc[0x48].service_name="ABC"`,
//! `pmt[0x0408].streams.add=0x0f:0x0111`, `eit[0x0408].events[0].duration=01:00:00`.
//!
//! Values: integers (decimal or 0x…), booleans, `"text"` (ARIB-encoded
//! with `isdb-text`) or `hex:…` for strings, 3-letter codes, hex byte
//! strings, `[a,b,…]` lists, `YYYY-MM-DD HH:MM:SS` (JST) times,
//! `HH:MM:SS` durations, `HH:MM` offsets, `none` for absent optionals.

use crate::decode::Table;
use isdb_si::AnyTable;
use isdb_si::descriptor::{self as d, Descriptor};
use isdb_si::time::{BcdDuration, BcdOffset, Date, DateTime, MjdTime};
use isdb_text::AribString;
use isdb_ts::Pid;
use isdb_ts::section::LongHeader;

/// Why an expression or a value does not parse, or does not apply to the
/// section it targets (the grammar of the module docs).  The codecs' own
/// refusals arrive as [`MutateError::Codec`] with the codec error as
/// source.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
#[non_exhaustive]
pub enum MutateError {
    /// Text not in the form the grammar gives it.
    #[error("{problem}: {input:?}")]
    Syntax {
        /// The form expected (`"not an integer"`, `"stream as TYPE:PID"`).
        problem: &'static str,
        /// The text.
        input: String,
    },
    /// An integer too large for the field's type.
    #[error("{value} does not fit {ty}")]
    TooLarge {
        /// The integer.
        value: u64,
        /// The field's type (`u8`, `version_number (5 bits)`).
        ty: &'static str,
    },
    /// Not a 13-bit PID (H.222.0 Table 2-3).
    #[error("PID {value:#x} is not a 13-bit value (0..=0x1FFF)")]
    Pid {
        /// The integer given.
        value: u64,
    },
    /// A `frequency_mhz` outside the 16-bit field of 1/7 MHz units of the
    /// terrestrial_delivery_system_descriptor (B10 Part 2 §6.2.31).
    #[error("frequency_mhz {mhz} is outside the 16-bit 1/7 MHz field")]
    FrequencyMhz {
        /// The value given, MHz.
        mhz: f64,
    },
    /// A list index past the end.
    #[error("{what} {index} out of range (len {len})")]
    Index {
        /// `"index"` or `"descriptor index"`.
        what: &'static str,
        /// The index.
        index: usize,
        /// The list's length.
        len: usize,
    },
    /// No list element with the id, or descriptor with the tag, selected.
    #[error("no {what} {sel}")]
    NoElement {
        /// `"element with id"` or `"descriptor with tag"`.
        what: &'static str,
        /// The selector text.
        sel: String,
    },
    /// A table name the grammar does not know.
    #[error("unknown table {name:?} (pat, cat, pmt, nit, sdt, bat, eit, tdt, tot, bit)")]
    UnknownTable {
        /// The name.
        name: String,
    },
    /// A field the element does not have.
    #[error("{owner} has no field {field:?} (fields: {known})")]
    NoField {
        /// The element (`"PAT"`, `"PMT stream"`, `"element"`).
        owner: &'static str,
        /// The field named.
        field: String,
        /// The fields it has.
        known: String,
    },
    /// The path or the operation does not apply where it points.
    #[error("{at}: {problem}")]
    Path {
        /// The path segment or element.
        at: String,
        /// Why not.
        problem: &'static str,
    },
    /// The codec refused the value or the section (a time outside Annex C,
    /// a field wider than its bits, a section that does not decode).
    #[error("{what}")]
    Codec {
        /// What was being decoded or encoded.
        what: String,
        /// The codec's error.
        #[source]
        source: isdb_ts::Error,
    },
    /// The text has no ARIB STD-B24 encoding.
    #[error("ARIB encode of {text:?}")]
    Text {
        /// The text.
        text: String,
        /// The encoder's error.
        #[source]
        source: isdb_text::EncodeError,
    },
    /// The value of the named field.
    #[error("{name}")]
    Field {
        /// The field.
        name: &'static str,
        /// Why its value was refused.
        #[source]
        source: Box<MutateError>,
    },
}

/// Result alias using [`MutateError`].
pub type Result<T, E = MutateError> = std::result::Result<T, E>;

fn syntax(problem: &'static str, input: &str) -> MutateError {
    MutateError::Syntax {
        problem,
        input: input.to_string(),
    }
}

fn path_err(at: impl Into<String>, problem: &'static str) -> MutateError {
    MutateError::Path {
        at: at.into(),
        problem,
    }
}

fn codec(what: impl Into<String>) -> impl FnOnce(isdb_ts::Error) -> MutateError {
    let what = what.into();
    move |source| MutateError::Codec { what, source }
}

/// `s` parsed as a number, or [`MutateError::Syntax`] with `problem`.
fn num<T: std::str::FromStr>(s: &str, problem: &'static str) -> Result<T> {
    s.trim().parse().map_err(|_| syntax(problem, s))
}

/// A path segment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Seg {
    /// Field name.
    pub name: String,
    /// Selector inside `[...]`.
    pub sel: Option<String>,
}

/// Operation.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Op {
    /// `=value`.
    Set(String),
    /// `.add=value`.
    Add(String),
    /// `.del=value` (or `.del` after a selected element: value ignored).
    Del(String),
}

/// A parsed expression.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Expr {
    /// Table name.
    pub table: String,
    /// table_id override.
    pub tid: Option<u8>,
    /// table_id_extension filter (program_number / service_id …).
    pub ext: Option<u16>,
    /// Path below the table.
    pub path: Vec<Seg>,
    /// Operation.
    pub op: Op,
}

/// Which sections an expression applies to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    /// PIDs (empty = any PID).
    pub pids: Vec<Pid>,
    /// table_id.
    pub table_id: u8,
    /// table_id_extension.
    pub ext: Option<u16>,
}

impl Target {
    /// Whether a section on `pid` matches.
    pub fn matches(&self, pid: Pid, sec: &[u8]) -> bool {
        if sec.is_empty()
            || sec[0] != self.table_id
            || !(self.pids.is_empty() || self.pids.contains(&pid))
        {
            return false;
        }
        match self.ext {
            None => true,
            Some(e) => {
                sec.len() >= 5 && sec[1] & 0x80 != 0 && u16::from_be_bytes([sec[3], sec[4]]) == e
            }
        }
    }
}

/// A 13-bit PID (H.222.0 Table 2-3) written as [`int`] is.
pub fn pid(s: &str) -> Result<Pid> {
    let v = int(s)?;
    u16::try_from(v)
        .ok()
        .and_then(Pid::new)
        .ok_or(MutateError::Pid { value: v })
}

/// Parse an integer (decimal or 0x hex).
pub fn int(s: &str) -> Result<u64> {
    let s = s.trim();
    let r = if let Some(h) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        u64::from_str_radix(h, 16)
    } else {
        s.parse()
    };
    r.map_err(|_| syntax("not an integer", s))
}

fn split_top(s: &str, sep: char) -> Vec<String> {
    let mut out = Vec::new();
    let mut depth = 0;
    let mut cur = String::new();
    for c in s.chars() {
        match c {
            '[' => depth += 1,
            ']' => depth -= 1,
            _ => {}
        }
        if c == sep && depth == 0 {
            out.push(std::mem::take(&mut cur));
        } else {
            cur.push(c);
        }
    }
    out.push(cur);
    out
}

fn seg(s: &str) -> Result<Seg> {
    match s.find('[') {
        Some(i) => {
            let sel = s[i + 1..]
                .strip_suffix(']')
                .ok_or_else(|| syntax("unbalanced []", s))?;
            Ok(Seg {
                name: s[..i].to_string(),
                sel: Some(sel.to_string()),
            })
        }
        None => Ok(Seg {
            name: s.to_string(),
            sel: None,
        }),
    }
}

impl Expr {
    /// Parse `table.path=value`.
    pub fn parse(s: &str) -> Result<Self> {
        let (lhs, rhs) = s
            .split_once('=')
            .ok_or_else(|| syntax("expression needs '='", s))?;
        let mut parts = split_top(lhs.trim(), '.');
        let head = parts.remove(0);
        let hs = seg(&head)?;
        let (table, tid) = match hs.name.split_once('@') {
            Some((t, x)) => (t.to_lowercase(), Some(field(x, "table_id")?)),
            None => (hs.name.to_lowercase(), None),
        };
        let ext = hs
            .sel
            .as_deref()
            .map(|x| field(x, "table_id_extension"))
            .transpose()?;
        let mut path: Vec<Seg> = parts.iter().map(|p| seg(p)).collect::<Result<_>>()?;
        let v = rhs.to_string();
        let op = match path.last() {
            Some(Seg { name, sel: None }) if name == "add" => {
                path.pop();
                Op::Add(v)
            }
            Some(Seg { name, sel: None }) if name == "del" => {
                path.pop();
                Op::Del(v)
            }
            _ => Op::Set(v),
        };
        Ok(Expr {
            table,
            tid,
            ext,
            path,
            op,
        })
    }

    /// Sections this expression applies to.
    pub fn target(&self) -> Result<Target> {
        let t = |pids: &[u16], tid: u8| Target {
            // B10 Part 2 Table 5-1 PIDs, all below 0x2000.
            pids: pids.iter().map(|&p| Pid::from_wire(p)).collect(),
            table_id: self.tid.unwrap_or(tid),
            ext: self.ext,
        };
        Ok(match self.table.as_str() {
            "pat" => t(&[0x0000], 0x00),
            "cat" => t(&[0x0001], 0x01),
            "pmt" => t(&[], 0x02),
            "nit" => t(&[0x0010], 0x40),
            "sdt" => t(&[0x0011], 0x42),
            "bat" => t(&[0x0011], 0x4A),
            "eit" => t(&[0x0012, 0x0026, 0x0027], 0x4E),
            "tdt" => t(&[0x0014], 0x70),
            "tot" => t(&[0x0014], 0x73),
            "bit" => t(&[0x0024], 0xC4),
            o => {
                return Err(MutateError::UnknownTable {
                    name: o.to_string(),
                });
            }
        })
    }

    /// Apply to a decoded table.
    pub fn apply(&self, t: &mut Table) -> Result<()> {
        let p = &self.path[..];
        let op = &self.op;
        match t {
            Table::Pat(x) => {
                if p.is_empty() {
                    // pat.add=PN:PID / pat.del=PN
                    return list(
                        &mut x.body.entries,
                        &Seg {
                            name: "entries".into(),
                            sel: None,
                        },
                        &[],
                        op,
                        |e| u64::from(e.program_number),
                        pat_entry,
                        |_, _, _| Err(path_err("PAT entry", "no fields")),
                    );
                }
                if header(&mut x.header, p, op)? {
                    return Ok(());
                }
                match p[0].name.as_str() {
                    "entries" | "programs" => list(
                        &mut x.body.entries,
                        &p[0],
                        &p[1..],
                        op,
                        |e| u64::from(e.program_number),
                        pat_entry,
                        |e, r, op| {
                            let v = leaf(r, op)?;
                            match r[0].name.as_str() {
                                "program_number" => e.program_number = val(v)?,
                                "pid" => e.pid = val(v)?,
                                "reserved" => e.reserved = val(v)?,
                                f => {
                                    return Err(no_field(
                                        "PAT entry",
                                        f,
                                        "program_number, pid, reserved",
                                    ));
                                }
                            }
                            Ok(())
                        },
                    ),
                    f => Err(no_field("PAT", f, "entries, programs, version, tidext, …")),
                }
            }
            Table::Cat(x) => {
                if header(&mut x.header, p, op)? {
                    return Ok(());
                }
                need(p, "CAT", &["desc", "descriptors"])?;
                ts_desc_loop(&mut x.body.descriptors, &p[0], &p[1..], op)
            }
            Table::Pmt(x) => {
                if header(&mut x.header, p, op)? {
                    return Ok(());
                }
                if p.is_empty() {
                    return Err(path_err("PMT", "path required"));
                }
                let b = &mut x.body;
                match p[0].name.as_str() {
                    "pcr_pid" => b.pcr_pid = val(leaf(p, op)?)?,
                    "reserved_pcr" => b.reserved_pcr = val(leaf(p, op)?)?,
                    "reserved_info" => b.reserved_info = val(leaf(p, op)?)?,
                    "desc" | "descriptors" | "program_info" => {
                        ts_desc_loop(&mut b.program_info, &p[0], &p[1..], op)?
                    }
                    "streams" | "es" => list(
                        &mut b.streams,
                        &p[0],
                        &p[1..],
                        op,
                        |s| u64::from(s.elementary_pid),
                        |v| {
                            let (t, pid) = v
                                .split_once(':')
                                .ok_or_else(|| syntax("stream as TYPE:PID", v))?;
                            Ok(isdb_ts::psi::PmtStream::new(
                                field(t, "stream_type")?,
                                self::pid(pid)?,
                                vec![],
                            ))
                        },
                        |s, r, op| {
                            match r[0].name.as_str() {
                                "desc" | "descriptors" => {
                                    return ts_desc_loop(&mut s.descriptors, &r[0], &r[1..], op);
                                }
                                _ => {}
                            }
                            let v = leaf(r, op)?;
                            match r[0].name.as_str() {
                                "stream_type" => s.stream_type = val(v)?,
                                "pid" | "elementary_pid" => s.elementary_pid = val(v)?,
                                "reserved_pid" => s.reserved_pid = val(v)?,
                                "reserved_info" => s.reserved_info = val(v)?,
                                f => {
                                    return Err(no_field(
                                        "PMT stream",
                                        f,
                                        "stream_type, pid, reserved_pid, reserved_info, desc",
                                    ));
                                }
                            }
                            Ok(())
                        },
                    )?,
                    f => {
                        return Err(no_field("PMT", f, "pcr_pid, desc, streams, version …"));
                    }
                }
                Ok(())
            }
            Table::Si(a) => si_table(a, p, op),
        }
    }
}

fn no_field(owner: &'static str, field: &str, known: &str) -> MutateError {
    MutateError::NoField {
        owner,
        field: field.to_string(),
        known: known.to_string(),
    }
}

fn need(p: &[Seg], owner: &'static str, names: &[&str]) -> Result<()> {
    match p.first() {
        Some(s) if names.contains(&s.name.as_str()) => Ok(()),
        s => Err(no_field(
            owner,
            s.map_or("", |s| s.name.as_str()),
            &names.join(", "),
        )),
    }
}

fn pat_entry(v: &str) -> Result<isdb_ts::psi::PatEntry> {
    let (a, b) = v
        .split_once(':')
        .ok_or_else(|| syntax("PAT entry as PROGRAM:PID", v))?;
    Ok(isdb_ts::psi::PatEntry::new(
        field(a, "program_number")?,
        pid(b)?,
    ))
}

/// The value of a scalar leaf: exactly one segment, no selector, `Set`.
fn leaf<'a>(p: &[Seg], op: &'a Op) -> Result<&'a str> {
    match (p, op) {
        ([s], Op::Set(v)) if s.sel.is_none() => Ok(v),
        ([s], _) if s.sel.is_none() => Err(path_err(&s.name, "add/del on a scalar field")),
        _ => Err(path_err(
            p.iter()
                .map(|s| s.name.as_str())
                .collect::<Vec<_>>()
                .join("."),
            "not a scalar field path",
        )),
    }
}

/// Long-form header fields; returns true if handled.
fn header(h: &mut LongHeader, p: &[Seg], op: &Op) -> Result<bool> {
    if p.len() != 1 || p[0].sel.is_some() {
        return Ok(false);
    }
    let f = p[0].name.as_str();
    // Resolve the field before reading the value: a name that is not a
    // header field is left to the table (Ok(false)).
    let set: fn(&mut LongHeader, &str) -> Result<()> = match f {
        "version" | "version_number" => |h, v| {
            let x: u8 = val(v)?;
            if x > 31 {
                return Err(MutateError::TooLarge {
                    value: x.into(),
                    ty: "version_number (5 bits)",
                });
            }
            h.version_number = x;
            Ok(())
        },
        "tidext"
        | "table_id_extension"
        | "network_id"
        | "transport_stream_id_ext"
        | "program_number"
        | "service_id" => |h, v| {
            h.table_id_extension = val(v)?;
            Ok(())
        },
        "cni" | "current_next_indicator" => |h, v| {
            h.current_next_indicator = val(v)?;
            Ok(())
        },
        "sn" | "section_number" => |h, v| {
            h.section_number = val(v)?;
            Ok(())
        },
        "lsn" | "last_section_number" => |h, v| {
            h.last_section_number = val(v)?;
            Ok(())
        },
        "table_id" => |h, v| {
            h.table_id = val(v)?;
            Ok(())
        },
        "private_indicator" => |h, v| {
            h.private_indicator = val(v)?;
            Ok(())
        },
        "reserved1" => |h, v| {
            h.reserved1 = val(v)?;
            Ok(())
        },
        "reserved2" => |h, v| {
            h.reserved2 = val(v)?;
            Ok(())
        },
        _ => return Ok(false),
    };
    set(h, leaf(p, op)?)?;
    Ok(true)
}

/// Element selection in a list: decimal index or 0x… id.
fn select<T>(v: &[T], sel: &str, id: &impl Fn(&T) -> u64) -> Result<usize> {
    let sel = sel.trim();
    if sel.starts_with("0x") || sel.starts_with("0X") {
        let want = int(sel)?;
        v.iter()
            .position(|x| id(x) == want)
            .ok_or_else(|| MutateError::NoElement {
                what: "element with id",
                sel: sel.to_string(),
            })
    } else {
        let i: usize = num(sel, "bad index")?;
        if i >= v.len() {
            return Err(MutateError::Index {
                what: "index",
                index: i,
                len: v.len(),
            });
        }
        Ok(i)
    }
}

/// Generic list mutation (see module docs).
fn list<T>(
    v: &mut Vec<T>,
    seg: &Seg,
    rest: &[Seg],
    op: &Op,
    id: impl Fn(&T) -> u64,
    new: impl Fn(&str) -> Result<T>,
    mut f: impl FnMut(&mut T, &[Seg], &Op) -> Result<()>,
) -> Result<()> {
    match &seg.sel {
        Some(s) => {
            let i = select(v, s, &id)?;
            if rest.is_empty() {
                match op {
                    Op::Set(x) => v[i] = new(x)?,
                    Op::Del(_) => {
                        v.remove(i);
                    }
                    Op::Add(_) => {
                        return Err(path_err(&seg.name, "add needs a list, not an element"));
                    }
                }
                Ok(())
            } else {
                f(&mut v[i], rest, op)
            }
        }
        None => {
            if !rest.is_empty() {
                return Err(path_err(
                    &seg.name,
                    "select an element with [index] or [0xID]",
                ));
            }
            match op {
                Op::Add(x) => v.push(new(x)?),
                Op::Del(x) => {
                    let i = select(v, x, &id)?;
                    v.remove(i);
                }
                Op::Set(_) => {
                    return Err(path_err(
                        &seg.name,
                        "cannot set a whole list; use [i], add or del",
                    ));
                }
            }
            Ok(())
        }
    }
}

// ---------------------------------------------------------------------------
// values
// ---------------------------------------------------------------------------

/// Parse a value of a field type.
pub trait FromVal: Sized {
    /// Parse.
    fn from_val(s: &str) -> Result<Self>;
}

fn val<T: FromVal>(s: &str) -> Result<T> {
    T::from_val(s)
}

/// [`val`] with the field name on the error (`table_id: 300 does not fit u8`).
pub(crate) fn field<T: FromVal>(s: &str, name: &'static str) -> Result<T> {
    val(s).map_err(|e| MutateError::Field {
        name,
        source: Box::new(e),
    })
}

macro_rules! int_val {
    ($($t:ty),*) => {$(
        impl FromVal for $t {
            fn from_val(s: &str) -> Result<Self> {
                let v = int(s)?;
                <$t>::try_from(v).map_err(|_| MutateError::TooLarge { value: v, ty: stringify!($t) })
            }
        }
    )*};
}
int_val!(u8, u16, u32, u64);

impl FromVal for Pid {
    fn from_val(s: &str) -> Result<Self> {
        pid(s)
    }
}

impl FromVal for bool {
    fn from_val(s: &str) -> Result<Self> {
        match s.trim() {
            "1" | "true" => Ok(true),
            "0" | "false" => Ok(false),
            o => Err(syntax("not a boolean", o)),
        }
    }
}

impl FromVal for AribString {
    fn from_val(s: &str) -> Result<Self> {
        if let Some(h) = s.strip_prefix("hex:") {
            return Ok(AribString(
                crate::tree::unhex(h).ok_or_else(|| syntax("bad hex", h))?,
            ));
        }
        let t = s
            .strip_prefix('"')
            .and_then(|x| x.strip_suffix('"'))
            .unwrap_or(s);
        AribString::encode(t).map_err(|source| MutateError::Text {
            text: t.to_string(),
            source,
        })
    }
}

impl FromVal for [u8; 3] {
    fn from_val(s: &str) -> Result<Self> {
        match *s.trim().as_bytes() {
            [a, b, c] => Ok([a, b, c]),
            _ => Err(syntax("3-character code expected", s)),
        }
    }
}

impl FromVal for Vec<u8> {
    fn from_val(s: &str) -> Result<Self> {
        let h = s.trim().strip_prefix("hex:").unwrap_or(s.trim());
        crate::tree::unhex(h).ok_or_else(|| syntax("bad hex", h))
    }
}

impl FromVal for Vec<u16> {
    fn from_val(s: &str) -> Result<Self> {
        let t = s.trim().trim_start_matches('[').trim_end_matches(']');
        if t.trim().is_empty() {
            return Ok(vec![]);
        }
        t.split(',').map(val::<u16>).collect()
    }
}

impl FromVal for Option<u8> {
    fn from_val(s: &str) -> Result<Self> {
        match s.trim() {
            "none" | "-" | "" => Ok(None),
            o => Ok(Some(val(o)?)),
        }
    }
}

impl FromVal for MjdTime {
    fn from_val(s: &str) -> Result<Self> {
        let s = s.trim().trim_matches('"');
        if s == "undefined" {
            return Ok(MjdTime::UNDEFINED);
        }
        if s.starts_with("0x") {
            return Ok(MjdTime::from_u40(int(s)?));
        }
        let s = s.trim_end_matches(" JST");
        const FORM: &str = "time as YYYY-MM-DD HH:MM:SS";
        let (d, t) = s.split_once([' ', 'T']).ok_or_else(|| syntax(FORM, s))?;
        let dp: Vec<&str> = d.split('-').collect();
        let tp: Vec<&str> = t.split(':').collect();
        let (&[y, mo, da], &[h, mi, se]) = (&dp[..], &tp[..]) else {
            return Err(syntax(FORM, s));
        };
        let dt = DateTime {
            date: Date {
                year: num(y, FORM)?,
                month: num(mo, FORM)?,
                day: num(da, FORM)?,
            },
            hour: num(h, FORM)?,
            minute: num(mi, FORM)?,
            second: num(se, FORM)?,
        };
        MjdTime::from_jst(dt).map_err(codec(format!("JST time {s:?}")))
    }
}

impl FromVal for BcdDuration {
    fn from_val(s: &str) -> Result<Self> {
        let s = s.trim();
        if s == "undefined" {
            return Ok(BcdDuration::UNDEFINED);
        }
        if s.starts_with("0x") {
            return Ok(BcdDuration(field(s, "duration")?));
        }
        const FORM: &str = "duration as HH:MM:SS";
        let p: Vec<&str> = s.split(':').collect();
        let &[h, m, sec] = &p[..] else {
            return Err(syntax(FORM, s));
        };
        BcdDuration::from_hms(num(h, FORM)?, num(m, FORM)?, num(sec, FORM)?)
            .map_err(codec(format!("duration {s:?}")))
    }
}

impl FromVal for BcdOffset {
    fn from_val(s: &str) -> Result<Self> {
        let s = s.trim();
        if s.starts_with("0x") {
            return Ok(BcdOffset(field(s, "local_time_offset")?));
        }
        const FORM: &str = "offset as HH:MM";
        let (h, m) = s.split_once(':').ok_or_else(|| syntax(FORM, s))?;
        BcdOffset::from_hm(num(h, FORM)?, num(m, FORM)?).map_err(codec(format!("offset {s:?}")))
    }
}

// ---------------------------------------------------------------------------
// descriptors
// ---------------------------------------------------------------------------

/// Parse a complete descriptor (tag, length, body) from hex and decode it
/// with the typed B10 decoders.
pub fn descriptor_from_hex(s: &str) -> Result<Descriptor> {
    let b: Vec<u8> = val(s)?;
    Descriptor::decode(&b).map_err(codec(format!("descriptor {s}")))
}

fn desc_select(v: &[Descriptor], sel: &str) -> Result<usize> {
    let sel = sel.trim();
    if sel.starts_with("0x") || sel.starts_with("0X") {
        let (t, k) = match sel.split_once('#') {
            Some((t, k)) => (t, num::<usize>(k, "occurrence after # is not a number")?),
            None => (sel, 0),
        };
        let tag: u8 = field(t, "descriptor_tag")?;
        v.iter()
            .enumerate()
            .filter(|(_, d)| d.tag() == tag)
            .nth(k)
            .map(|(i, _)| i)
            .ok_or_else(|| MutateError::NoElement {
                what: "descriptor with tag",
                sel: sel.to_string(),
            })
    } else {
        let i: usize = num(sel, "bad index")?;
        if i >= v.len() {
            return Err(MutateError::Index {
                what: "descriptor index",
                index: i,
                len: v.len(),
            });
        }
        Ok(i)
    }
}

/// Mutate a B10 descriptor loop.
pub fn desc_loop(v: &mut Vec<Descriptor>, seg: &Seg, rest: &[Seg], op: &Op) -> Result<()> {
    match &seg.sel {
        Some(s) => {
            let i = desc_select(v, s)?;
            if rest.is_empty() {
                match op {
                    Op::Set(x) => v[i] = descriptor_from_hex(x)?,
                    Op::Del(_) => {
                        v.remove(i);
                    }
                    Op::Add(_) => return Err(path_err(&seg.name, "add on a selected descriptor")),
                }
                Ok(())
            } else {
                descriptor(&mut v[i], rest, op)
            }
        }
        None => {
            if !rest.is_empty() {
                return Err(path_err(
                    &seg.name,
                    "select a descriptor with [index] or [0xTAG]",
                ));
            }
            match op {
                Op::Add(x) => v.push(descriptor_from_hex(x)?),
                Op::Del(x) => {
                    let i = desc_select(v, x)?;
                    v.remove(i);
                }
                Op::Set(_) => {
                    return Err(path_err(&seg.name, "cannot set a whole descriptor loop"));
                }
            }
            Ok(())
        }
    }
}

/// Mutate an `isdb-ts` raw descriptor loop (PMT/CAT) through the typed B10
/// descriptors.
fn ts_desc_loop(
    v: &mut Vec<isdb_ts::descriptor::Descriptor>,
    seg: &Seg,
    rest: &[Seg],
    op: &Op,
) -> Result<()> {
    let mut typed: Vec<Descriptor> = v
        .iter()
        .map(|x| {
            Descriptor::decode_body(x.tag, &x.data)
                .map_err(codec(format!("descriptor {:#04x}", x.tag)))
        })
        .collect::<Result<_>>()?;
    desc_loop(&mut typed, seg, rest, op)?;
    *v = typed
        .iter()
        .map(|x| {
            let mut b = Vec::new();
            x.encode(&mut b)
                .map_err(codec(format!("descriptor {:#04x}", x.tag())))?;
            // encode wrote tag, length, body.
            match b.as_slice() {
                [tag, _, body @ ..] => {
                    Ok(isdb_ts::descriptor::Descriptor::new(*tag, body.to_vec()))
                }
                _ => Err(path_err(x.name(), "encoded without a tag and length")),
            }
        })
        .collect::<Result<_>>()?;
    Ok(())
}

macro_rules! fields {
    ($obj:expr, $p:expr, $op:expr; $($f:ident $(| $alias:literal)*),* $(,)?) => {{
        let v = leaf($p, $op)?;
        match $p[0].name.as_str() {
            $( stringify!($f) $(| $alias)* => { $obj.$f = val(v)?; Ok(()) } )*
            other => Err(no_field("element", other, &[$(stringify!($f)),*].join(", "))),
        }
    }};
}

/// Mutate fields of a typed descriptor.
fn descriptor(dsc: &mut Descriptor, p: &[Seg], op: &Op) -> Result<()> {
    let f = p[0].name.as_str();
    match dsc {
        Descriptor::TerrestrialDeliverySystem(x) => match f {
            "frequencies" | "frequency" if p[0].sel.is_some() || !matches!(op, Op::Set(_)) => list(
                &mut x.frequencies,
                &p[0],
                &p[1..],
                op,
                |&f| u64::from(f),
                val,
                scalar_element,
            ),
            "frequencies" | "frequency" => {
                x.frequencies = val(leaf(p, op)?)?;
                Ok(())
            }
            "frequency_mhz" => {
                let mhz: f64 = num(leaf(p, op)?, "frequency_mhz is not a number")?;
                let units = (mhz * 7.0).round();
                if !(0.0..=f64::from(u16::MAX)).contains(&units) {
                    return Err(MutateError::FrequencyMhz { mhz });
                }
                // Checked against the field's range just above.
                #[allow(clippy::cast_possible_truncation)]
                let units = units as u16;
                x.frequencies = vec![units];
                Ok(())
            }
            _ => fields!(x, p, op; area_code, guard_interval | "gi", transmission_mode | "mode"),
        },
        Descriptor::TsInformation(x) => match f {
            "transmission_types" => list(
                &mut x.transmission_types,
                &p[0],
                &p[1..],
                op,
                |t| u64::from(t.transmission_type_info),
                |s| {
                    Ok(d::TransmissionType {
                        transmission_type_info: val(s)?,
                        service_ids: vec![],
                    })
                },
                |t, r, op| match r[0].name.as_str() {
                    "service_ids" if r[0].sel.is_some() || !matches!(op, Op::Set(_)) => list(
                        &mut t.service_ids,
                        &r[0],
                        &r[1..],
                        op,
                        |&s| u64::from(s),
                        val,
                        scalar_element,
                    ),
                    _ => fields!(t, r, op; transmission_type_info, service_ids),
                },
            ),
            _ => fields!(x, p, op; remote_control_key_id, ts_name, reserved_future_use),
        },
        Descriptor::PartialReception(x) => match f {
            "service_ids" if p[0].sel.is_some() || !matches!(op, Op::Set(_)) => list(
                &mut x.service_ids,
                &p[0],
                &p[1..],
                op,
                |&s| u64::from(s),
                val,
                scalar_element,
            ),
            _ => fields!(x, p, op; service_ids),
        },
        Descriptor::ServiceList(x) => list(
            &mut x.services,
            &p[0],
            &p[1..],
            op,
            |s| u64::from(s.service_id),
            |s| {
                let (a, b) = s
                    .split_once(':')
                    .ok_or_else(|| syntax("service as SID:TYPE", s))?;
                Ok(d::ServiceListEntry {
                    service_id: val(a)?,
                    service_type: val(b)?,
                })
            },
            |e, r, op| fields!(e, r, op; service_id, service_type),
        ),
        Descriptor::NetworkName(x) => fields!(x, p, op; name),
        Descriptor::BouquetName(x) => fields!(x, p, op; name),
        Descriptor::BroadcasterName(x) => fields!(x, p, op; name),
        Descriptor::Service(x) => fields!(x, p, op; service_type, provider_name, service_name),
        Descriptor::SystemManagement(x) => {
            fields!(x, p, op; system_management_id, additional_identification_info)
        }
        Descriptor::ShortEvent(x) => fields!(x, p, op; language, event_name, text),
        Descriptor::Component(x) => {
            fields!(x, p, op; reserved_future_use, stream_content, component_type, component_tag, language, text)
        }
        Descriptor::StreamIdentifier(x) => fields!(x, p, op; component_tag),
        Descriptor::CaIdentifier(x) => fields!(x, p, op; ca_system_ids),
        Descriptor::ConditionalAccess(x) => {
            fields!(x, p, op; ca_system_id, reserved, ca_pid | "pid", private_data)
        }
        Descriptor::AccessControl(x) => {
            fields!(x, p, op; ca_system_id, transmission_type, pid, private_data)
        }
        Descriptor::DigitalCopyControl(x) => {
            fields!(x, p, op; digital_recording_control_data, user_defined, maximum_bitrate)
        }
        Descriptor::ContentAvailability(x) => {
            fields!(x, p, op; reserved_future_use, copy_restriction_mode, image_constraint_token, retention_mode, retention_state, encryption_mode, reserved_future_use_bytes)
        }
        Descriptor::VideoDecodeControl(x) => {
            fields!(x, p, op; still_picture_flag, sequence_end_code_flag, video_encode_format, transfer_characteristics)
        }
        Descriptor::HierarchicalTransmission(x) => {
            fields!(x, p, op; reserved_future_use_1, quality_level, reserved_future_use_2, reference_pid)
        }
        Descriptor::DataComponent(x) => {
            fields!(x, p, op; data_component_id, additional_data_component_info)
        }
        Descriptor::AudioComponent(x) => {
            fields!(x, p, op; reserved_future_use_1, stream_content, component_type, component_tag, stream_type, simulcast_group_tag, main_component_flag, quality_indicator, sampling_rate, reserved_future_use_2, language, text)
        }
        Descriptor::Series(x) => {
            fields!(x, p, op; series_id, repeat_label, program_pattern, expire_date_valid_flag, expire_date, episode_number, last_episode_number, series_name)
        }
        Descriptor::ParentalRating(x) => list(
            &mut x.ratings,
            &p[0],
            &p[1..],
            op,
            |r| u64::from(r.rating),
            |s| {
                let (c, r) = s
                    .split_once(':')
                    .ok_or_else(|| syntax("rating as CCC:RATING", s))?;
                Ok(d::ParentalRatingEntry {
                    country_code: val(c)?,
                    rating: val(r)?,
                })
            },
            |e, r, op| fields!(e, r, op; country_code, rating),
        ),
        Descriptor::LocalTimeOffset(x) => list(
            &mut x.entries,
            &p[0],
            &p[1..],
            op,
            |e| u64::from(e.country_region_id),
            |_| {
                Err(path_err(
                    "local_time_offset",
                    "add entries as a whole descriptor (desc.add=hex)",
                ))
            },
            |e, r, op| fields!(e, r, op; country_code, country_region_id, reserved, local_time_offset_polarity | "polarity", local_time_offset, time_of_change, next_time_offset),
        ),
        Descriptor::SiParameter(x) => match f {
            "tables" => list(
                &mut x.tables,
                &p[0],
                &p[1..],
                op,
                |t| u64::from(t.table_id),
                |s| {
                    let (t, h) = s.split_once(':').unwrap_or((s, ""));
                    Ok(d::TableDescription {
                        table_id: val(t)?,
                        description: val(h)?,
                    })
                },
                |t, r, op| fields!(t, r, op; table_id, description),
            ),
            _ => fields!(x, p, op; parameter_version, update_time),
        },
        Descriptor::ExtendedBroadcaster(x) => {
            if f == "reserved_future_use" {
                return fields!(x, p, op; reserved_future_use);
            }
            match &mut x.body {
                d::ExtendedBroadcasterBody::TerrestrialTv(t)
                | d::ExtendedBroadcasterBody::TerrestrialSound(t) => match f {
                    "affiliation_ids" => {
                        t.affiliation_ids = val(leaf(p, op)?)?;
                        Ok(())
                    }
                    "broadcasters" => list(
                        &mut t.broadcasters,
                        &p[0],
                        &p[1..],
                        op,
                        |b| u64::from(b.original_network_id),
                        |s| {
                            let (a, b) = s
                                .split_once(':')
                                .ok_or_else(|| syntax("broadcaster as ONID:BID", s))?;
                            Ok(d::NetworkBroadcaster {
                                original_network_id: val(a)?,
                                broadcaster_id: val(b)?,
                            })
                        },
                        |b, r, op| fields!(b, r, op; original_network_id, broadcaster_id),
                    ),
                    _ => fields!(t, p, op; terrestrial_broadcaster_id, private_data),
                },
                d::ExtendedBroadcasterBody::Other { .. } => Err(path_err(
                    f,
                    "broadcaster_type other than 0x1/0x2: replace the descriptor with hex",
                )),
            }
        }
        Descriptor::Unknown { data, .. } => {
            if f != "data" {
                return Err(path_err(
                    f,
                    "untyped descriptor: only `data` (hex) can be set",
                ));
            }
            *data = val(leaf(p, op)?)?;
            Ok(())
        }
        other => Err(path_err(
            format!("{} (tag {:#04x})", other.name(), other.tag()),
            "fields not editable by path; replace the descriptor with desc[0xTT]=<hex>",
        )),
    }
}

/// The element closure of a list of scalars: there is nothing below one.
fn scalar_element<T>(_: &mut T, r: &[Seg], _: &Op) -> Result<()> {
    Err(path_err(
        r.first().map_or("", |s| s.name.as_str()),
        "scalar element",
    ))
}

// ---------------------------------------------------------------------------
// SI tables
// ---------------------------------------------------------------------------

fn is_desc(name: &str) -> bool {
    matches!(name, "desc" | "descriptors" | "first" | "first_descriptors")
}

fn network_loops(n: &mut isdb_si::tables::NetworkLoops, p: &[Seg], op: &Op) -> Result<()> {
    match p[0].name.as_str() {
        x if is_desc(x) => desc_loop(&mut n.descriptors, &p[0], &p[1..], op),
        "ts" | "transport_streams" => list(
            &mut n.transport_streams,
            &p[0],
            &p[1..],
            op,
            |t| u64::from(t.transport_stream_id),
            |s| {
                let (a, b) = s
                    .split_once(':')
                    .ok_or_else(|| syntax("TS as TSID:ONID", s))?;
                Ok(isdb_si::tables::TransportStreamEntry {
                    transport_stream_id: val(a)?,
                    original_network_id: val(b)?,
                    reserved_future_use: 0xF,
                    descriptors: vec![],
                })
            },
            |t, r, op| {
                if is_desc(&r[0].name) {
                    return desc_loop(&mut t.descriptors, &r[0], &r[1..], op);
                }
                fields!(t, r, op; transport_stream_id | "tsid", original_network_id | "onid", reserved_future_use)
            },
        ),
        _ => fields!(n, p, op; reserved_future_use_1, reserved_future_use_2),
    }
}

fn si_table(a: &mut AnyTable, p: &[Seg], op: &Op) -> Result<()> {
    if p.is_empty() {
        return Err(path_err(a.name(), "path required"));
    }
    match a {
        AnyTable::Nit(t) => {
            if header(&mut t.header, p, op)? {
                return Ok(());
            }
            network_loops(&mut t.body.0, p, op)
        }
        AnyTable::Bat(t) => {
            if header(&mut t.header, p, op)? {
                return Ok(());
            }
            network_loops(&mut t.body.0, p, op)
        }
        AnyTable::Sdt(t) => {
            if header(&mut t.header, p, op)? {
                return Ok(());
            }
            match p[0].name.as_str() {
                "services" => list(
                    &mut t.body.services,
                    &p[0],
                    &p[1..],
                    op,
                    |s| u64::from(s.service_id),
                    |s| {
                        Ok(isdb_si::tables::SdtService {
                            service_id: val(s)?,
                            reserved_future_use: 0x7,
                            eit_user_defined_flags: 0x7,
                            eit_schedule_flag: false,
                            eit_present_following_flag: false,
                            running_status: 0,
                            free_ca_mode: false,
                            descriptors: vec![],
                        })
                    },
                    |s, r, op| {
                        if is_desc(&r[0].name) {
                            return desc_loop(&mut s.descriptors, &r[0], &r[1..], op);
                        }
                        fields!(s, r, op; service_id, reserved_future_use, eit_user_defined_flags, eit_schedule_flag, eit_present_following_flag | "eit_pf_flag", running_status, free_ca_mode)
                    },
                ),
                _ => fields!(t.body, p, op; original_network_id | "onid", reserved_future_use),
            }
        }
        AnyTable::Eit(t) => {
            if header(&mut t.header, p, op)? {
                return Ok(());
            }
            match p[0].name.as_str() {
                "events" => list(
                    &mut t.body.events,
                    &p[0],
                    &p[1..],
                    op,
                    |e| u64::from(e.event_id),
                    |s| {
                        Ok(isdb_si::tables::EitEvent {
                            event_id: val(s)?,
                            start_time: MjdTime::UNDEFINED,
                            duration: BcdDuration::UNDEFINED,
                            running_status: 0,
                            free_ca_mode: false,
                            descriptors: vec![],
                        })
                    },
                    |e, r, op| {
                        if is_desc(&r[0].name) {
                            return desc_loop(&mut e.descriptors, &r[0], &r[1..], op);
                        }
                        fields!(e, r, op; event_id, start_time, duration, running_status, free_ca_mode)
                    },
                ),
                _ => {
                    fields!(t.body, p, op; transport_stream_id | "tsid", original_network_id | "onid", segment_last_section_number, last_table_id)
                }
            }
        }
        AnyTable::Tot(t) => match p[0].name.as_str() {
            x if is_desc(x) => desc_loop(&mut t.body.descriptors, &p[0], &p[1..], op),
            _ => fields!(t.body, p, op; jst_time, reserved),
        },
        AnyTable::Tdt(t) => fields!(t.body, p, op; jst_time),
        AnyTable::Bit(t) => {
            if header(&mut t.header, p, op)? {
                return Ok(());
            }
            match p[0].name.as_str() {
                x if is_desc(x) => desc_loop(&mut t.body.first_descriptors, &p[0], &p[1..], op),
                "broadcasters" => list(
                    &mut t.body.broadcasters,
                    &p[0],
                    &p[1..],
                    op,
                    |b| u64::from(b.broadcaster_id),
                    |s| {
                        Ok(isdb_si::tables::BitBroadcaster {
                            broadcaster_id: val(s)?,
                            reserved_future_use: 0xF,
                            descriptors: vec![],
                        })
                    },
                    |b, r, op| {
                        if is_desc(&r[0].name) {
                            return desc_loop(&mut b.descriptors, &r[0], &r[1..], op);
                        }
                        fields!(b, r, op; broadcaster_id, reserved_future_use)
                    },
                ),
                _ => fields!(t.body, p, op; broadcast_view_propriety, reserved_future_use),
            }
        }
        other => Err(path_err(other.name(), "not editable by path")),
    }
}

/// Apply `expr` to one section (decode → mutate → encode).  Returns the
/// new section bytes.
pub fn apply_to_section(expr: &Expr, pid: Pid, sec: &[u8]) -> Result<Vec<u8>> {
    let mut t = crate::decode::decode(pid, sec).map_err(codec("decode"))?;
    expr.apply(&mut t)?;
    t.encode().map_err(codec("encode"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use isdb_si::tables::{NetworkLoops, Nit, NitBody, TransportStreamEntry};

    fn nit() -> Vec<u8> {
        let tds = Descriptor::TerrestrialDeliverySystem(d::TerrestrialDeliverySystem {
            area_code: 0x5A5,
            guard_interval: 2,
            transmission_mode: 2,
            frequencies: vec![3900],
        });
        let n = Nit {
            header: LongHeader {
                table_id: 0x40,
                private_indicator: true,
                ..LongHeader::new(0x40, 0x79EE, 3)
            },
            body: NitBody(NetworkLoops {
                reserved_future_use_1: 0xF,
                descriptors: vec![Descriptor::NetworkName(d::NetworkName {
                    name: AribString::encode("ABC").unwrap(),
                })],
                reserved_future_use_2: 0xF,
                transport_streams: vec![TransportStreamEntry {
                    transport_stream_id: 0x79EE,
                    original_network_id: 0x79EE,
                    reserved_future_use: 0xF,
                    descriptors: vec![tds],
                }],
            }),
        };
        n.encode().unwrap()
    }

    fn decoded(b: &[u8]) -> Nit {
        Nit::decode(b).unwrap()
    }

    #[test]
    fn parse_examples() {
        let e = Expr::parse("nit.ts[0].desc[0xFA].guard_interval=2").unwrap();
        assert_eq!(e.table, "nit");
        assert_eq!(e.path.len(), 3);
        assert_eq!(
            e.path[1],
            Seg {
                name: "desc".into(),
                sel: Some("0xFA".into())
            }
        );
        assert_eq!(e.op, Op::Set("2".into()));
        let e = Expr::parse("pat.add=0x0e28:0x1fff").unwrap();
        assert!(e.path.is_empty());
        assert_eq!(e.op, Op::Add("0x0e28:0x1fff".into()));
        let e = Expr::parse("eit@0x50[0x0408].events[0].duration=01:00:00").unwrap();
        assert_eq!((e.tid, e.ext), (Some(0x50), Some(0x0408)));
        assert_eq!(e.target().unwrap().table_id, 0x50);
    }

    #[test]
    fn nit_field_mutations_reencode() {
        let b = nit();
        let out = apply_to_section(
            &Expr::parse("nit.ts[0].desc[0xFA].guard_interval=1").unwrap(),
            Pid::new(0x10).unwrap(),
            &b,
        )
        .unwrap();
        let n = decoded(&out);
        let Descriptor::TerrestrialDeliverySystem(t) =
            &n.body.0.transport_streams[0].descriptors[0]
        else {
            panic!()
        };
        assert_eq!(t.guard_interval, 1);
        let out = apply_to_section(
            &Expr::parse("nit.version=24").unwrap(),
            Pid::new(0x10).unwrap(),
            &out,
        )
        .unwrap();
        assert_eq!(decoded(&out).header.version_number, 24);
        let out = apply_to_section(
            &Expr::parse("nit.desc[0x40].name=\"テスト\"").unwrap(),
            Pid::new(0x10).unwrap(),
            &out,
        )
        .unwrap();
        let Descriptor::NetworkName(nn) = &decoded(&out).body.0.descriptors[0] else {
            panic!()
        };
        assert_eq!(nn.name.to_string_lossy(), "テスト");
        let out = apply_to_section(
            &Expr::parse("nit.ts[0x79ee].desc.add=cd0a0940000f00000e08ffff").unwrap(),
            Pid::new(0x10).unwrap(),
            &out,
        );
        assert!(
            out.is_err(),
            "malformed TS info hex must be rejected by the typed decoder"
        );
        let out = apply_to_section(
            &Expr::parse("nit.ts[0x79ee].desc[0xFA].frequencies.add=3564").unwrap(),
            Pid::new(0x10).unwrap(),
            &nit(),
        )
        .unwrap();
        let Descriptor::TerrestrialDeliverySystem(t) =
            &decoded(&out).body.0.transport_streams[0].descriptors[0]
        else {
            panic!()
        };
        assert_eq!(t.frequencies, vec![3900, 3564]);
        // width overflow is caught by the encoder
        assert!(
            apply_to_section(
                &Expr::parse("nit.ts[0].desc[0xFA].guard_interval=4").unwrap(),
                Pid::new(0x10).unwrap(),
                &nit()
            )
            .is_err()
        );
        // unknown field lists alternatives
        let e = apply_to_section(
            &Expr::parse("nit.ts[0].desc[0xFA].gaurd=1").unwrap(),
            Pid::new(0x10).unwrap(),
            &nit(),
        )
        .unwrap_err();
        assert!(e.to_string().contains("guard_interval"), "{e}");
    }

    #[test]
    fn pat_add_del() {
        let p = isdb_ts::psi::Pat::section(
            0x79EE,
            0,
            vec![
                isdb_ts::psi::PatEntry::new(0, Pid::new(0x10).unwrap()),
                isdb_ts::psi::PatEntry::new(0x0408, Pid::new(0x1F0).unwrap()),
            ],
        )
        .encode()
        .unwrap();
        let out = apply_to_section(
            &Expr::parse("pat.add=0x0e28:0x1fff").unwrap(),
            Pid::new(0).unwrap(),
            &p,
        )
        .unwrap();
        let d = isdb_ts::section::Long::<isdb_ts::psi::Pat>::decode(&out).unwrap();
        assert_eq!(
            d.body.programs().collect::<Vec<_>>(),
            vec![(0x0408, Pid::new(0x1F0).unwrap()), (0x0E28, Pid::NULL)]
        );
        let out = apply_to_section(
            &Expr::parse("pat.del=0x0408").unwrap(),
            Pid::new(0).unwrap(),
            &out,
        )
        .unwrap();
        let d = isdb_ts::section::Long::<isdb_ts::psi::Pat>::decode(&out).unwrap();
        assert_eq!(
            d.body.programs().collect::<Vec<_>>(),
            vec![(0x0E28, Pid::NULL)]
        );
        let out = apply_to_section(
            &Expr::parse("pat.entries[0x0E28].pid=0x01f0").unwrap(),
            Pid::new(0).unwrap(),
            &out,
        )
        .unwrap();
        let d = isdb_ts::section::Long::<isdb_ts::psi::Pat>::decode(&out).unwrap();
        assert_eq!(
            d.body.programs().collect::<Vec<_>>(),
            vec![(0x0E28, Pid::new(0x1F0).unwrap())]
        );
    }
}
