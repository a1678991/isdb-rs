//! Experiment transforms on a transport stream, each producing a valid TS:
//! sections are (re)packetised per H.222.0 §2.4.4 (pointer_field,
//! PUSI, 0xFF stuffing only after a section end), continuity counters are
//! renumbered per PID (§2.4.3.3), CRC_32 is recomputed by the codecs, and
//! packet positions are kept (unused slots become null packets, §2.4.3.3),
//! so a CBR stream stays CBR.
//!
//! Section rewriting works per PID: the PID's sections are reassembled
//! with their original first/last packet positions; a transform maps each
//! section to zero or more output sections; the output is packed into the
//! PID's original packet slots, never earlier than the original section
//! start.  When the output needs more room than the original slots offer
//! (larger replacements, inserted sections), null packets are used as
//! extra slots according to [`NullSlots`].

use crate::mutate::{Expr, MutateError, Target};
use crate::scan;
use crate::timebase::Timebase;
use isdb_ts::Pid;
use isdb_ts::cc::Counter;
use isdb_ts::demux::{Assembled, SectionAssembler};
use isdb_ts::packet::{NULL_PACKET, PACKET_SIZE, PAYLOAD_CAPACITY, Packet, View};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::path::{Path, PathBuf};

/// Why an edit could not be made (the operations of [`EditOp`], the
/// section fixtures of [`sections_from_hex`] / [`load_sections`]).
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum EditError {
    /// A selector not of the form `PID:TID[:EXT[:SN]]`.
    #[error("selector as PID:TID[:EXT[:SN]]: {input:?}")]
    Selector {
        /// The text given.
        input: String,
    },
    /// An expression, or a value in one, that does not parse or apply
    /// ([`crate::mutate`]).
    #[error(transparent)]
    Mutate(#[from] MutateError),
    /// A mutation refused on one section.
    #[error("on pid {pid:#06x} packet {packet}")]
    Section {
        /// The PID carrying the section.
        pid: Pid,
        /// Index of the section's first packet.
        packet: u64,
        /// Why.
        #[source]
        source: MutateError,
    },
    /// Section rewriting on a PID that carries PCRs (H.222.0 §2.4.3.5) would
    /// remove them.
    #[error("PID {pid:#06x} carries PCRs; section rewriting would remove them")]
    CarriesPcr {
        /// The PID.
        pid: Pid,
    },
    /// A replacement that is not one complete section (H.222.0 §2.4.4.10
    /// section_length).
    #[error("replacement is not one complete section ({len} bytes)")]
    NotOneSection {
        /// Its length.
        len: usize,
    },
    /// `replace` with nothing to replace.
    #[error("no section matches {sel:?}; use insert to add a new table")]
    NoSection {
        /// The selector.
        sel: Selector,
    },
    /// A mutation whose table is on no PID of the stream.
    #[error("no section matches {table}")]
    NoTable {
        /// The table name of the expression.
        table: String,
    },
    /// An insertion interval that is zero or negative.
    #[error("interval must be positive (got {interval_s} s)")]
    Interval {
        /// The interval given, seconds.
        interval_s: f64,
    },
    /// Insertions need a time base: PCRs (H.222.0 §2.4.2.2) or a nominal
    /// rate.
    #[error("no PCR in the stream: give a nominal bit rate to time insertions")]
    NoTimebase,
    /// A line of a hex fixture that does not read.
    #[error("line {line}: {problem}")]
    Fixture {
        /// 1-based line number.
        line: usize,
        /// What is wrong with it.
        problem: &'static str,
    },
    /// A section file without a section matching the selector.
    #[error("no section matching {sel:?} in {}", path.display())]
    NoSectionIn {
        /// The selector.
        sel: Selector,
        /// The file.
        path: PathBuf,
    },
    /// Reading or writing a file.
    #[error("{op} {}", path.display())]
    Io {
        /// `"read"` or `"write"`.
        op: &'static str,
        /// The file.
        path: PathBuf,
        /// The OS error.
        #[source]
        source: std::io::Error,
    },
    /// A packet the TS codec would not build.
    #[error(transparent)]
    Codec(#[from] isdb_ts::Error),
}

/// Result alias using [`EditError`].
pub type Result<T, E = EditError> = std::result::Result<T, E>;

fn io_err(op: &'static str, path: &Path) -> impl FnOnce(std::io::Error) -> EditError {
    let path = path.to_path_buf();
    move |source| EditError::Io { op, path, source }
}

/// A TS packet.
pub type Pkt = [u8; PACKET_SIZE];

/// When null packets may carry rewritten section data.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
pub enum NullSlots {
    /// Only the PID's original packet slots.
    Never,
    /// Null slots only once the output falls behind the original schedule
    /// (the section being sent has passed its original end position), and
    /// for inserted sections.
    #[default]
    WhenNeeded,
    /// Any null slot as soon as data is pending.
    Always,
}

/// Which sections: PID, table_id, optional table_id_extension and
/// section_number.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Selector {
    /// PID.
    pub pid: Pid,
    /// table_id.
    pub table_id: u8,
    /// table_id_extension.
    pub ext: Option<u16>,
    /// section_number.
    pub sn: Option<u8>,
}

impl Selector {
    /// Parse `PID:TID[:EXT[:SN]]` (integers decimal or 0x hex).
    pub fn parse(s: &str) -> Result<Self> {
        let p: Vec<&str> = s.split(':').collect();
        if p.len() < 2 || p.len() > 4 {
            return Err(EditError::Selector {
                input: s.to_string(),
            });
        }
        use crate::mutate::field;
        Ok(Selector {
            pid: crate::mutate::pid(p[0])?,
            table_id: field(p[1], "table_id")?,
            ext: p
                .get(2)
                .map(|x| field(x, "table_id_extension"))
                .transpose()?,
            sn: p.get(3).map(|x| field(x, "section_number")).transpose()?,
        })
    }

    /// Whether section bytes `sec` (carried on `pid`) match.
    pub fn matches(&self, pid: Pid, sec: &[u8]) -> bool {
        if pid != self.pid || sec.first() != Some(&self.table_id) {
            return false;
        }
        let long = sec.len() >= 8 && sec[1] & 0x80 != 0 && self.table_id != 0x72;
        if let Some(e) = self.ext
            && !(long && u16::from_be_bytes([sec[3], sec[4]]) == e)
        {
            return false;
        }
        if let Some(n) = self.sn
            && !(long && sec[6] == n)
        {
            return false;
        }
        true
    }
}

/// One edit operation.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum EditOp {
    /// Replace every packet of these PIDs with a null packet.
    NullPids(Vec<Pid>),
    /// Remove matching sections.
    Remove(Selector),
    /// Replace matching sections with `sections` (matched by
    /// section_number; see [`replace_map`]).
    Replace {
        /// What to replace.
        sel: Selector,
        /// Replacement sections (complete, CRC included).
        sections: Vec<Vec<u8>>,
    },
    /// Insert `sections` on `pid` every `interval_s` seconds starting at
    /// `offset_s` (timed by PCR, or the nominal rate).
    Insert {
        /// PID.
        pid: Pid,
        /// Sections, sent together at each repetition.
        sections: Vec<Vec<u8>>,
        /// Repetition interval.
        interval_s: f64,
        /// First transmission.
        offset_s: f64,
    },
    /// Typed field mutation of every matching section.
    Mutate(Expr),
}

/// Options.
#[derive(Debug, Clone, Default)]
pub struct EditOptions {
    /// Null-slot policy.
    pub null_slots: NullSlots,
    /// Nominal bit rate for timing when there are no PCRs.
    pub nominal_bps: Option<f64>,
}

/// What one PID rewrite did.
#[derive(Debug, Clone, Serialize)]
pub struct PidRewrite {
    /// PID.
    pub pid: Pid,
    /// Sections reassembled from the input.
    pub sections_in: usize,
    /// Sections changed (replaced/mutated) — input occurrences.
    pub changed: usize,
    /// Sections removed — input occurrences.
    pub removed: usize,
    /// Sections inserted (extra requests).
    pub inserted: usize,
    /// Output sections fully sent.
    pub sections_out: usize,
    /// Output sections not (completely) sent before the end of the stream.
    pub dropped: usize,
    /// Original packet slots of the PID.
    pub original_slots: usize,
    /// Original slots left empty (now null packets).
    pub slots_nulled: usize,
    /// Null packets used for this PID.
    pub null_slots_used: usize,
    /// Largest delay (packets) between a section's earliest position and
    /// its first packet.
    pub max_delay_packets: u64,
}

/// Report of one operation.
#[derive(Debug, Clone, Default, Serialize)]
pub struct StepReport {
    /// Operation text.
    pub op: String,
    /// Packets replaced by null packets (NullPids).
    pub nulled_packets: usize,
    /// PID rewrites.
    pub rewrites: Vec<PidRewrite>,
}

/// A section as reassembled from packets.
#[derive(Debug, Clone)]
pub struct Occurrence {
    /// First packet.
    pub start: u64,
    /// Last packet.
    pub end: u64,
    /// Bytes.
    pub bytes: Vec<u8>,
}

/// Reassemble the sections of `pid` (H.222.0 §2.4.4).
pub fn assemble_pid(pkts: &[Pkt], pid: Pid) -> Vec<Occurrence> {
    let mut a = SectionAssembler::new(pid);
    let mut v = Vec::new();
    for (i, p) in pkts.iter().enumerate() {
        if pid_of(p) != pid {
            continue;
        }
        for o in a.push(i as u64, p) {
            if let Assembled::Section(s) = o {
                v.push(Occurrence {
                    start: s.start_packet,
                    end: s.end_packet,
                    bytes: s.data,
                });
            }
        }
    }
    v
}

fn pid_of(p: &Pkt) -> Pid {
    Pid::of_packet(p)
}

/// Replace every packet of `pids` with a null packet; returns the count.
pub fn null_pids(pkts: &mut [Pkt], pids: &[Pid]) -> usize {
    let null = NULL_PACKET;
    let set: BTreeSet<Pid> = pids.iter().copied().collect();
    let mut n = 0;
    for p in pkts.iter_mut() {
        if set.contains(&pid_of(p)) {
            *p = null;
            n += 1;
        }
    }
    n
}

/// One output section with its scheduling constraints.
#[derive(Debug, Clone)]
pub struct Request {
    /// Earliest packet index for its first byte.
    pub earliest: u64,
    /// Original end position (null slots may be used once past it).
    pub deadline: u64,
    /// Section bytes.
    pub bytes: Vec<u8>,
}

/// Rewrite the sections of `pid`: `f(occurrence)` returns the output
/// sections replacing it (`None` = keep unchanged); `extra` are inserted.
pub fn rewrite_pid(
    pkts: &mut [Pkt],
    pid: Pid,
    mut f: impl FnMut(&Occurrence) -> Result<Option<Vec<Vec<u8>>>>,
    extra: Vec<Request>,
    policy: NullSlots,
) -> Result<PidRewrite> {
    let mut rep = PidRewrite {
        pid,
        sections_in: 0,
        changed: 0,
        removed: 0,
        inserted: 0,
        sections_out: 0,
        dropped: 0,
        original_slots: 0,
        slots_nulled: 0,
        null_slots_used: 0,
        max_delay_packets: 0,
    };
    let mut originals = Vec::new();
    let mut first_cc = None;
    for (i, p) in pkts.iter().enumerate() {
        if pid_of(p) != pid {
            continue;
        }
        if let Ok(v) = View::parse(p) {
            if v.pcr().is_some() {
                return Err(EditError::CarriesPcr { pid });
            }
            if p[1] & 0x80 == 0 && first_cc.is_none() {
                first_cc = Some(v.header.continuity_counter);
            }
        }
        originals.push(i);
    }
    rep.original_slots = originals.len();
    let occ = assemble_pid(pkts, pid);
    rep.sections_in = occ.len();
    let mut reqs: Vec<Request> = Vec::new();
    for o in &occ {
        match f(o)? {
            None => reqs.push(Request {
                earliest: o.start,
                deadline: o.end,
                bytes: o.bytes.clone(),
            }),
            Some(v) => {
                if v.is_empty() {
                    rep.removed += 1;
                } else {
                    rep.changed += 1;
                }
                for b in v {
                    if isdb_ts::section::total_len(&b) != Some(b.len()) {
                        return Err(EditError::NotOneSection { len: b.len() });
                    }
                    reqs.push(Request {
                        earliest: o.start,
                        deadline: o.end,
                        bytes: b,
                    });
                }
            }
        }
    }
    rep.inserted = extra.len();
    reqs.extend(extra);
    reqs.sort_by_key(|r| r.earliest);
    let total_reqs = reqs.len();
    let mut q: VecDeque<Request> = reqs.into();
    let orig_set: BTreeSet<usize> = originals.iter().copied().collect();
    let nulls: Vec<usize> = if policy == NullSlots::Never {
        Vec::new()
    } else {
        pkts.iter()
            .enumerate()
            .filter(|(_, p)| pid_of(p) == Pid::NULL)
            .map(|(i, _)| i)
            .collect()
    };
    // merged slot positions
    let mut slots: Vec<(usize, bool)> = originals
        .iter()
        .map(|&i| (i, true))
        .chain(nulls.iter().map(|&i| (i, false)))
        .collect();
    slots.sort_unstable();
    let mut cc = Counter::new(first_cc.unwrap_or_default());
    let mut cur: Option<(Vec<u8>, u64)> = None;
    let mut out: Vec<(usize, Pkt)> = Vec::new();
    let mut used: BTreeSet<usize> = BTreeSet::new();
    let mut sent = 0usize;
    for (slot, orig) in slots {
        let idx = slot as u64;
        let eligible = cur.is_some() || q.front().is_some_and(|r| r.earliest <= idx);
        if !eligible {
            continue;
        }
        if !orig {
            match policy {
                NullSlots::Never => continue,
                NullSlots::Always => {}
                NullSlots::WhenNeeded => {
                    let dl = cur
                        .as_ref()
                        .map(|c| c.1)
                        .or_else(|| q.front().map(|r| r.deadline))
                        .unwrap_or(0);
                    if dl > idx {
                        continue;
                    }
                }
            }
        }
        let mut payload: Vec<u8> = Vec::with_capacity(PAYLOAD_CAPACITY);
        let mut pusi = false;
        let mut fill = true;
        if let Some((rem, dl)) = cur.take() {
            if rem.len() >= PAYLOAD_CAPACITY {
                payload.extend_from_slice(&rem[..PAYLOAD_CAPACITY]);
                let rest = rem[PAYLOAD_CAPACITY..].to_vec();
                if rest.is_empty() {
                    sent += 1;
                } else {
                    cur = Some((rest, dl));
                }
                fill = false;
            } else {
                // A section due now starts after the remainder when the
                // pointer_field (§2.4.4.2) and at least one byte of it fit.
                let pointer_field = u8::try_from(rem.len())
                    .ok()
                    .filter(|&n| usize::from(n) <= PAYLOAD_CAPACITY - 2)
                    .filter(|_| q.front().is_some_and(|r| r.earliest <= idx));
                if let Some(pf) = pointer_field {
                    pusi = true;
                    payload.push(pf);
                }
                payload.extend_from_slice(&rem);
                sent += 1;
                fill = pointer_field.is_some();
            }
        } else {
            pusi = true;
            payload.push(0);
        }
        while fill && payload.len() < PAYLOAD_CAPACITY {
            if !q.front().is_some_and(|r| r.earliest <= idx) {
                break;
            }
            let Some(r) = q.pop_front() else {
                break;
            };
            rep.max_delay_packets = rep.max_delay_packets.max(idx - r.earliest);
            let room = PAYLOAD_CAPACITY - payload.len();
            if r.bytes.len() <= room {
                payload.extend_from_slice(&r.bytes);
                sent += 1;
            } else {
                payload.extend_from_slice(&r.bytes[..room]);
                cur = Some((r.bytes[room..].to_vec(), r.deadline));
                break;
            }
        }
        payload.resize(PAYLOAD_CAPACITY, 0xFF); // §2.4.4 stuffing after a section end
        let p = Packet::with_payload(pid, pusi, cc.next(true), &payload)?.encode()?;
        out.push((slot, p));
        used.insert(slot);
        if !orig {
            rep.null_slots_used += 1;
        }
    }
    rep.sections_out = sent;
    rep.dropped = total_reqs - sent;
    let null = NULL_PACKET;
    for &i in &orig_set {
        if !used.contains(&i) {
            pkts[i] = null;
            rep.slots_nulled += 1;
        }
    }
    for (i, p) in out {
        pkts[i] = p;
    }
    Ok(rep)
}

/// Map an original section to its replacement: replacement sections are
/// keyed by section_number; an original with section_number `k` becomes the
/// replacement with the same number (or nothing), and the first original
/// section number of the sub_table also carries the replacement sections
/// whose numbers the original lacks.  Short sections: all replacements.
pub fn replace_map(orig_sns: &BTreeSet<u8>, repl: &[Vec<u8>], sec: &[u8]) -> Vec<Vec<u8>> {
    let sn_of = |b: &[u8]| (b.len() >= 8 && b[1] & 0x80 != 0 && b[0] != 0x72).then(|| b[6]);
    match sn_of(sec) {
        None => repl.to_vec(),
        Some(k) => {
            let mut v: Vec<Vec<u8>> = repl
                .iter()
                .filter(|r| sn_of(r) == Some(k))
                .cloned()
                .collect();
            if orig_sns.iter().next() == Some(&k) {
                v.extend(
                    repl.iter()
                        .filter(|r| sn_of(r).is_none_or(|s| !orig_sns.contains(&s)))
                        .cloned(),
                );
            }
            v
        }
    }
}

/// PIDs carrying sections that match `t`.
fn target_pids(pkts: &[Pkt], t: &Target) -> Vec<Pid> {
    if !t.pids.is_empty() {
        return t.pids.clone();
    }
    let cap = scan::scan_packets(
        pkts,
        &scan::ScanOptions {
            skip_packet_checks: true,
            ..Default::default()
        },
    );
    let set: BTreeSet<Pid> = cap
        .slots
        .keys()
        .filter(|k| k.table_id == t.table_id && (t.ext.is_none() || k.ext == t.ext))
        .map(|k| k.pid)
        .collect();
    set.into_iter().collect()
}

/// One-line description of an operation.
pub fn describe(op: &EditOp) -> String {
    let sel = |s: &Selector| {
        let mut t = format!("pid {:#06x} tid {:#04x}", s.pid, s.table_id);
        if let Some(e) = s.ext {
            t += &format!(" ext {e:#06x}");
        }
        if let Some(n) = s.sn {
            t += &format!(" sn {n}");
        }
        t
    };
    match op {
        EditOp::NullPids(p) => format!("null PIDs {p:04x?}"),
        EditOp::Remove(s) => format!("remove sections {}", sel(s)),
        EditOp::Replace { sel: s, sections } => format!(
            "replace sections {} with {} section(s)",
            sel(s),
            sections.len()
        ),
        EditOp::Insert {
            pid,
            sections,
            interval_s,
            offset_s,
        } => format!(
            "insert {} section(s) on pid {pid:#06x} every {interval_s} s from {offset_s} s",
            sections.len()
        ),
        EditOp::Mutate(e) => {
            let path: Vec<String> = e
                .path
                .iter()
                .map(|s| match &s.sel {
                    Some(x) => format!("{}[{x}]", s.name),
                    None => s.name.clone(),
                })
                .collect();
            let (op, v) = match &e.op {
                crate::mutate::Op::Set(v) => ("=", v),
                crate::mutate::Op::Add(v) => (".add=", v),
                crate::mutate::Op::Del(v) => (".del=", v),
            };
            format!(
                "set {}{}{}{}{op}{v}",
                e.table,
                e.tid.map(|t| format!("@{t:#04x}")).unwrap_or_default(),
                e.ext.map(|x| format!("[{x:#06x}]")).unwrap_or_default(),
                if path.is_empty() {
                    String::new()
                } else {
                    format!(".{}", path.join("."))
                }
            )
        }
    }
}

/// Apply operations in order.
pub fn apply(pkts: &mut [Pkt], ops: &[EditOp], opts: &EditOptions) -> Result<Vec<StepReport>> {
    let mut reports = Vec::new();
    for op in ops {
        let mut r = StepReport {
            op: describe(op),
            ..Default::default()
        };
        match op {
            EditOp::NullPids(p) => r.nulled_packets = null_pids(pkts, p),
            EditOp::Remove(sel) => {
                let s = *sel;
                r.rewrites.push(rewrite_pid(
                    pkts,
                    sel.pid,
                    |o| Ok(s.matches(s.pid, &o.bytes).then(Vec::new)),
                    vec![],
                    opts.null_slots,
                )?);
            }
            EditOp::Replace { sel, sections } => {
                let occ = assemble_pid(pkts, sel.pid);
                let sns: BTreeSet<u8> = occ
                    .iter()
                    .filter(|o| sel.matches(sel.pid, &o.bytes))
                    .filter(|o| o.bytes[1] & 0x80 != 0)
                    .map(|o| o.bytes[6])
                    .collect();
                if !occ.iter().any(|o| sel.matches(sel.pid, &o.bytes)) {
                    return Err(EditError::NoSection { sel: *sel });
                }
                let s = *sel;
                r.rewrites.push(rewrite_pid(
                    pkts,
                    sel.pid,
                    |o| {
                        Ok(s.matches(s.pid, &o.bytes)
                            .then(|| replace_map(&sns, sections, &o.bytes)))
                    },
                    vec![],
                    opts.null_slots,
                )?);
            }
            EditOp::Insert {
                pid,
                sections,
                interval_s,
                offset_s,
            } => {
                if *interval_s <= 0.0 {
                    return Err(EditError::Interval {
                        interval_s: *interval_s,
                    });
                }
                let cap = scan::scan_packets(
                    pkts,
                    &scan::ScanOptions {
                        skip_packet_checks: true,
                        ..Default::default()
                    },
                );
                let tb: Timebase = crate::inspect::timebase(&cap, opts.nominal_bps);
                if !tb.known() {
                    return Err(EditError::NoTimebase);
                }
                let n = pkts.len() as u64;
                let end = tb.time(n.saturating_sub(1)).unwrap_or(0.0);
                let mut t = tb.time(0).unwrap_or(0.0) + offset_s;
                let mut extra = Vec::new();
                while t <= end {
                    let at = tb.index_at(t).unwrap_or(n).min(n);
                    for s in sections {
                        extra.push(Request {
                            earliest: at,
                            deadline: at,
                            bytes: s.clone(),
                        });
                    }
                    t += interval_s;
                }
                r.rewrites.push(rewrite_pid(
                    pkts,
                    *pid,
                    |_| Ok(None),
                    extra,
                    opts.null_slots,
                )?);
            }
            EditOp::Mutate(expr) => {
                let t = expr.target()?;
                let pids = target_pids(pkts, &t);
                if pids.is_empty() {
                    return Err(EditError::NoTable {
                        table: expr.table.clone(),
                    });
                }
                for pid in pids {
                    let mut cache: HashMap<Vec<u8>, Vec<u8>> = HashMap::new();
                    let rw = rewrite_pid(
                        pkts,
                        pid,
                        |o| {
                            if !t.matches(pid, &o.bytes) {
                                return Ok(None);
                            }
                            if let Some(v) = cache.get(&o.bytes) {
                                return Ok(Some(vec![v.clone()]));
                            }
                            let v = crate::mutate::apply_to_section(expr, pid, &o.bytes).map_err(
                                |source| EditError::Section {
                                    pid,
                                    packet: o.start,
                                    source,
                                },
                            )?;
                            cache.insert(o.bytes.clone(), v.clone());
                            Ok(Some(vec![v]))
                        },
                        vec![],
                        opts.null_slots,
                    )?;
                    r.rewrites.push(rw);
                }
            }
        }
        reports.push(r);
    }
    Ok(reports)
}

/// Distinct sections of a capture matching `sel` (latest version per
/// table_id_extension/section_number, ordered by section key).
pub fn sections_from_capture(cap: &scan::Capture, sel: &Selector) -> Vec<Vec<u8>> {
    cap.slots
        .iter()
        .filter(|(k, s)| {
            k.pid == sel.pid
                && k.table_id == sel.table_id
                && sel.ext.is_none_or(|e| k.ext == Some(e))
                && sel.sn.is_none_or(|n| k.sn == Some(n))
                && !s.latest.is_empty()
        })
        .map(|(_, s)| s.latest.clone())
        .collect()
}

/// Sections from a hex fixture (`pid tid hex` per line, or one hex
/// section per line), filtered by `sel` (PID ignored for bare-hex lines).
pub fn sections_from_hex(text: &str, sel: Option<&Selector>) -> Result<Vec<(Pid, Vec<u8>)>> {
    let mut v = Vec::new();
    for (n, l) in text.lines().enumerate() {
        let l = l.trim();
        if l.is_empty() || l.starts_with('#') {
            continue;
        }
        let t: Vec<&str> = l.split_whitespace().collect();
        let line = |problem| EditError::Fixture {
            line: n + 1,
            problem,
        };
        let (pid, hex) = match t[..] {
            [hex] => (sel.map_or(Pid::PAT, |s| s.pid), hex),
            [pid, _, hex] => (crate::mutate::pid(pid)?, hex),
            _ => return Err(line("expected `pid tid hex` or `hex`")),
        };
        let b = crate::tree::unhex(hex).ok_or_else(|| line("bad hex"))?;
        if isdb_ts::section::total_len(&b) != Some(b.len()) {
            return Err(line("not one complete section"));
        }
        if sel.is_none_or(|s| s.matches(if t.len() == 1 { s.pid } else { pid }, &b)) {
            v.push((pid, b));
        }
    }
    Ok(v)
}

/// Load replacement sections from a `.ts` capture or a hex fixture.
pub fn load_sections(path: &Path, sel: &Selector) -> Result<Vec<Vec<u8>>> {
    let is_ts = path.extension().is_some_and(|e| {
        e.eq_ignore_ascii_case("ts")
            || e.eq_ignore_ascii_case("m2ts")
            || e.eq_ignore_ascii_case("mts")
    });
    let v = if is_ts {
        let cap = scan::scan_path(
            path,
            &scan::ScanOptions {
                skip_packet_checks: true,
                ..Default::default()
            },
        )
        .map_err(io_err("read", path))?;
        sections_from_capture(&cap, sel)
    } else {
        let text = std::fs::read_to_string(path).map_err(io_err("read", path))?;
        sections_from_hex(&text, Some(sel))?
            .into_iter()
            .map(|x| x.1)
            .collect()
    };
    if v.is_empty() {
        return Err(EditError::NoSectionIn {
            sel: *sel,
            path: path.to_path_buf(),
        });
    }
    Ok(v)
}

/// Extract sections to the hex fixture format (`0xPPPP 0xTT hex` per
/// line).  `all_bodies`: every distinct body of every version; otherwise
/// the latest section of each slot.
pub fn extract(cap: &scan::Capture, pid: Option<Pid>, tid: Option<u8>, all_bodies: bool) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "# sections of {} (pid table_id hex)\n",
        cap.source
    ));
    let mut seen: BTreeMap<(Pid, Vec<u8>), ()> = BTreeMap::new();
    for (k, s) in &cap.slots {
        if pid.is_some_and(|p| p != k.pid) || tid.is_some_and(|t| t != k.table_id) {
            continue;
        }
        let bodies: Vec<&Vec<u8>> = if all_bodies && !s.versions.is_empty() {
            s.versions
                .iter()
                .flat_map(|v| v.bodies.iter().map(|b| &b.bytes))
                .collect()
        } else {
            vec![&s.latest]
        };
        for b in bodies {
            if seen.insert((k.pid, b.clone()), ()).is_none() {
                out.push_str(&format!(
                    "{:#06x} {:#04x} {}\n",
                    k.pid,
                    k.table_id,
                    crate::tree::hex(b)
                ));
            }
        }
    }
    out
}

/// Write 188-byte packets.
pub fn write_packets(path: &Path, pkts: &[Pkt]) -> Result<()> {
    let mut buf = Vec::with_capacity(pkts.len() * PACKET_SIZE);
    for p in pkts {
        buf.extend_from_slice(p);
    }
    std::fs::write(path, buf).map_err(io_err("write", path))
}
