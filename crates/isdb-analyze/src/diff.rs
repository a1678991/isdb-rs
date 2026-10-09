//! Table-level and field-level comparison of two captures: for every
//! (PID, table_id, table_id_extension, section_number) presence in A/B,
//! repetition, and a field diff of the decoded tables/descriptors; plus the
//! PID inventory with rates.  This answers "what does the accepted station
//! mux have that ours lacks".

use crate::decode;
use crate::inspect::{self, Intervals};
use crate::names;
use crate::scan::{Capture, SlotKey};
use crate::tree::{self, Node};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

/// One changed field.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct FieldChange {
    /// Path, e.g. `body.transport_streams[transport_stream_id=0x79ee].descriptors[TerrestrialDeliverySystem].guard_interval`.
    pub path: String,
    /// Value in A (`None` = absent).
    pub a: Option<String>,
    /// Value in B.
    pub b: Option<String>,
}

/// Comparison of one section slot.
#[derive(Debug, Clone, Serialize)]
pub struct SectionDiff {
    /// Key in A.
    pub key_a: Option<SlotKey>,
    /// Key in B.
    pub key_b: Option<SlotKey>,
    /// How A and B were paired: "exact", "pid differs", "table_id_extension
    /// differs", "loose", or "only A"/"only B".
    pub pairing: &'static str,
    /// Table name.
    pub table: &'static str,
    /// Occurrences in A / B.
    pub count: (usize, usize),
    /// Interval statistics A / B.
    pub interval: (Option<Intervals>, Option<Intervals>),
    /// Latest section length A / B.
    pub length: (usize, usize),
    /// Latest bytes identical.
    pub identical: bool,
    /// Field changes (latest versions).
    pub changes: Vec<FieldChange>,
}

/// PID inventory line.
#[derive(Debug, Clone, Serialize)]
pub struct PidDiff {
    /// PID.
    pub pid: u16,
    /// Kind in A / B.
    pub kind: (Option<String>, Option<String>),
    /// Packets A / B.
    pub packets: (u64, u64),
    /// kbit/s A / B.
    pub kbps: (Option<f64>, Option<f64>),
}

/// Capture summary.
#[derive(Debug, Clone, Serialize)]
pub struct Side {
    /// Source.
    pub source: String,
    /// Packets.
    pub packets: u64,
    /// Duration.
    pub duration_s: Option<f64>,
    /// Packet rate (bit/s).
    pub rate_bps: Option<f64>,
    /// Null packets.
    pub null_packets: u64,
}

/// Diff report.
#[derive(Debug, Clone, Serialize)]
pub struct DiffReport {
    /// A.
    pub a: Side,
    /// B.
    pub b: Side,
    /// PIDs.
    pub pids: Vec<PidDiff>,
    /// Sections.
    pub sections: Vec<SectionDiff>,
}

/// Options.
#[derive(Debug, Clone, Default)]
pub struct DiffOptions {
    /// Nominal bit rate when there are no PCRs (applies to both).
    pub nominal_bps: Option<f64>,
}

// ---------------------------------------------------------------------------
// tree diff
// ---------------------------------------------------------------------------

/// Fields that identify list elements (matched by value, not index).
const ID_FIELDS: &[&str] = &[
    "transport_stream_id",
    "service_id",
    "event_id",
    "broadcaster_id",
    "program_number",
    "elementary_pid",
    "information_id",
    "description_id",
    "content_version",
    "component_group_id",
    "country_code",
];

fn elem_key(n: &Node, field: &str) -> Option<String> {
    tree::get(n, field).map(|v| tree::scalar(v, field))
}

fn list_keys(items: &[Node], leaf: &str) -> Vec<String> {
    // Descriptor loops (field named `*descriptors` / `program_info`, or
    // structs with a `tag` field): key by type name + occurrence.
    let all_struct = !items.is_empty() && items.iter().all(|x| matches!(x, Node::Struct(..)));
    let desc_loop = leaf.ends_with("descriptors") || leaf == "program_info";
    if all_struct && (desc_loop || items.iter().all(|x| tree::get(x, "tag").is_some())) {
        let mut seen: BTreeMap<String, usize> = BTreeMap::new();
        return items
            .iter()
            .map(|x| {
                let Node::Struct(name, _) = x else {
                    unreachable!()
                };
                let base = if name == "Unknown" || name == "Undecodable" {
                    format!("tag={}", elem_key(x, "tag").unwrap_or_default())
                } else {
                    name.clone()
                };
                let n = seen.entry(base.clone()).or_default();
                *n += 1;
                if *n == 1 { base } else { format!("{base}#{n}") }
            })
            .collect();
    }
    if all_struct {
        for f in ID_FIELDS {
            let keys: Vec<Option<String>> = items.iter().map(|x| elem_key(x, f)).collect();
            if keys.iter().all(Option::is_some) {
                let ks: Vec<String> = keys
                    .into_iter()
                    .map(|k| format!("{f}={}", k.unwrap()))
                    .collect();
                let uniq: BTreeSet<&String> = ks.iter().collect();
                if uniq.len() == ks.len() {
                    return ks;
                }
            }
        }
    }
    (0..items.len()).map(|i| i.to_string()).collect()
}

fn short(n: &Node, k: &str) -> String {
    let s = tree::scalar(n, k);
    if s.chars().count() > 400 {
        format!("{}…", s.chars().take(400).collect::<String>())
    } else {
        s
    }
}

/// Field-level diff of two trees.
pub fn diff_nodes(path: &str, a: &Node, b: &Node, out: &mut Vec<FieldChange>) {
    let leaf = path.rsplit(['.', '[']).next().unwrap_or("");
    match (a, b) {
        (Node::Struct(na, fa), Node::Struct(nb, fb)) if na == nb => {
            let mut names: Vec<&String> = fa.iter().map(|x| &x.0).collect();
            for (k, _) in fb {
                if !names.contains(&k) {
                    names.push(k);
                }
            }
            for k in names {
                let p = if path.is_empty() {
                    k.clone()
                } else {
                    format!("{path}.{k}")
                };
                match (tree::get(a, k), tree::get(b, k)) {
                    (Some(x), Some(y)) => diff_nodes(&p, x, y, out),
                    (Some(x), None) => out.push(FieldChange {
                        path: p,
                        a: Some(short(x, k)),
                        b: None,
                    }),
                    (None, Some(y)) => out.push(FieldChange {
                        path: p,
                        a: None,
                        b: Some(short(y, k)),
                    }),
                    (None, None) => {}
                }
            }
        }
        (Node::List(la), Node::List(lb))
            if la.iter().chain(lb).any(|x| matches!(x, Node::Struct(..))) =>
        {
            let ka = list_keys(la, leaf);
            let kb = list_keys(lb, leaf);
            let mut order: Vec<&String> = ka.iter().collect();
            for k in &kb {
                if !order.contains(&k) {
                    order.push(k);
                }
            }
            for k in order {
                let p = format!("{path}[{k}]");
                let x = ka.iter().position(|q| q == k).map(|i| &la[i]);
                let y = kb.iter().position(|q| q == k).map(|i| &lb[i]);
                match (x, y) {
                    (Some(x), Some(y)) => diff_nodes(&p, x, y, out),
                    (Some(x), None) => out.push(FieldChange {
                        path: p,
                        a: Some(short(x, "")),
                        b: None,
                    }),
                    (None, Some(y)) => out.push(FieldChange {
                        path: p,
                        a: None,
                        b: Some(short(y, "")),
                    }),
                    _ => {}
                }
            }
        }
        (Node::Text(_, ra), Node::Text(_, rb)) => {
            if ra != rb {
                out.push(FieldChange {
                    path: path.into(),
                    a: Some(short(a, leaf)),
                    b: Some(short(b, leaf)),
                });
            }
        }
        _ => {
            if a != b {
                out.push(FieldChange {
                    path: path.into(),
                    a: Some(short(a, leaf)),
                    b: Some(short(b, leaf)),
                });
            }
        }
    }
}

// ---------------------------------------------------------------------------
// capture diff
// ---------------------------------------------------------------------------

fn side(cap: &Capture, nominal: Option<f64>) -> (Side, crate::timebase::Timebase) {
    let tb = inspect::timebase(cap, nominal);
    (
        Side {
            source: cap.source.clone(),
            packets: cap.packets,
            duration_s: tb.duration(cap.packets),
            rate_bps: tb.rate_bps(),
            null_packets: cap.stats.pids.get(&0x1FFF).map_or(0, |s| s.packets),
        },
        tb,
    )
}

fn pair_keys(
    a: &[SlotKey],
    b: &[SlotKey],
) -> Vec<(Option<SlotKey>, Option<SlotKey>, &'static str)> {
    let mut out = Vec::new();
    let mut ua: Vec<SlotKey> = Vec::new();
    let bset: BTreeSet<SlotKey> = b.iter().copied().collect();
    let mut used_b: BTreeSet<SlotKey> = BTreeSet::new();
    for k in a {
        if bset.contains(k) {
            out.push((Some(*k), Some(*k), "exact"));
            used_b.insert(*k);
        } else {
            ua.push(*k);
        }
    }
    let mut ub: Vec<SlotKey> = b.iter().copied().filter(|k| !used_b.contains(k)).collect();
    type Proj = fn(&SlotKey) -> (Option<u16>, u8, Option<u16>, Option<u8>);
    let passes: [(Proj, &'static str); 3] = [
        (|k| (None, k.table_id, k.ext, k.sn), "pid differs"),
        (
            |k| (Some(k.pid), k.table_id, None, k.sn),
            "table_id_extension differs",
        ),
        (|k| (None, k.table_id, None, k.sn), "loose"),
    ];
    for (proj, label) in passes {
        let mut next_a = Vec::new();
        for k in &ua {
            let pk = proj(k);
            let ca: Vec<&SlotKey> = ua.iter().filter(|x| proj(x) == pk).collect();
            let cb: Vec<usize> = ub
                .iter()
                .enumerate()
                .filter(|(_, x)| proj(x) == pk)
                .map(|(i, _)| i)
                .collect();
            if ca.len() == 1 && cb.len() == 1 {
                let kb = ub.remove(cb[0]);
                out.push((Some(*k), Some(kb), label));
            } else {
                next_a.push(*k);
            }
        }
        ua = next_a;
    }
    for k in ua {
        out.push((Some(k), None, "only A"));
    }
    for k in ub {
        out.push((None, Some(k), "only B"));
    }
    out.sort_by_key(|(a, b, _)| a.or(*b));
    out
}

/// Compare two captures.
pub fn diff(a: &Capture, b: &Capture, opts: &DiffOptions) -> DiffReport {
    let (sa, ta) = side(a, opts.nominal_bps);
    let (sb, tb) = side(b, opts.nominal_bps);
    let ka = inspect::pid_kinds(a);
    let kb = inspect::pid_kinds(b);
    let pids: BTreeSet<u16> = a
        .stats
        .pids
        .keys()
        .chain(b.stats.pids.keys())
        .copied()
        .collect();
    let rate = |n: u64, d: Option<f64>| {
        d.filter(|&d| d > 0.0)
            .map(|d| n as f64 * 1504.0 / d / 1000.0)
    };
    let pids = pids
        .into_iter()
        .map(|pid| {
            let pa = a.stats.pids.get(&pid).map_or(0, |s| s.packets);
            let pb = b.stats.pids.get(&pid).map_or(0, |s| s.packets);
            PidDiff {
                pid,
                kind: (
                    ka.get(&pid)
                        .cloned()
                        .or_else(|| (pa > 0).then(|| "unreferenced".into())),
                    kb.get(&pid)
                        .cloned()
                        .or_else(|| (pb > 0).then(|| "unreferenced".into())),
                ),
                packets: (pa, pb),
                kbps: (rate(pa, sa.duration_s), rate(pb, sb.duration_s)),
            }
        })
        .collect();
    let keys_a: Vec<SlotKey> = a.slots.keys().copied().collect();
    let keys_b: Vec<SlotKey> = b.slots.keys().copied().collect();
    let mut sections = Vec::new();
    for (x, y, pairing) in pair_keys(&keys_a, &keys_b) {
        let s_a = x.map(|k| &a.slots[&k]);
        let s_b = y.map(|k| &b.slots[&k]);
        let tid = x.or(y).unwrap().table_id;
        let ia = s_a.and_then(|s| inspect::occurrence_intervals(&ta, &s.occurrences, a.packets).0);
        let ib = s_b.and_then(|s| inspect::occurrence_intervals(&tb, &s.occurrences, b.packets).0);
        let mut changes = Vec::new();
        let identical = matches!((s_a, s_b), (Some(p), Some(q)) if p.latest == q.latest);
        if let (Some(p), Some(q), false) = (s_a, s_b, identical) {
            let na = decode::decode(x.unwrap().pid, &p.latest)
                .map(|t| t.tree())
                .unwrap_or_else(|e| Node::Str(format!("decode error: {e}")));
            let nb = decode::decode(y.unwrap().pid, &q.latest)
                .map(|t| t.tree())
                .unwrap_or_else(|e| Node::Str(format!("decode error: {e}")));
            diff_nodes("", &na, &nb, &mut changes);
        }
        sections.push(SectionDiff {
            key_a: x,
            key_b: y,
            pairing,
            table: names::table_name(tid),
            count: (
                s_a.map_or(0, |s| s.occurrences.len()),
                s_b.map_or(0, |s| s.occurrences.len()),
            ),
            interval: (ia, ib),
            length: (
                s_a.map_or(0, |s| s.latest.len()),
                s_b.map_or(0, |s| s.latest.len()),
            ),
            identical,
            changes,
        });
    }
    DiffReport {
        a: sa,
        b: sb,
        pids,
        sections,
    }
}

fn f(v: Option<f64>, p: usize) -> String {
    v.map(|x| format!("{x:.p$}")).unwrap_or_else(|| "-".into())
}

impl DiffReport {
    /// Text rendering.  `all` lists every EIT schedule section; otherwise
    /// EIT schedule sub_tables are summarised.
    pub fn to_text(&self, all: bool) -> String {
        let mut o = String::new();
        let _ = writeln!(
            o,
            "A: {} — {} packets, {} s, {} Mbit/s, {} null",
            self.a.source,
            self.a.packets,
            f(self.a.duration_s, 3),
            f(self.a.rate_bps.map(|r| r / 1e6), 6),
            self.a.null_packets
        );
        let _ = writeln!(
            o,
            "B: {} — {} packets, {} s, {} Mbit/s, {} null",
            self.b.source,
            self.b.packets,
            f(self.b.duration_s, 3),
            f(self.b.rate_bps.map(|r| r / 1e6), 6),
            self.b.null_packets
        );
        let _ = writeln!(o, "\n-- PIDs (kbit/s A | B) --");
        for p in &self.pids {
            let _ = writeln!(
                o,
                "{:#06x} {:>10} | {:<10} {} | {}",
                p.pid,
                f(p.kbps.0, 1),
                f(p.kbps.1, 1),
                p.kind.0.as_deref().unwrap_or("—"),
                p.kind.1.as_deref().unwrap_or("—")
            );
        }
        let _ = writeln!(o, "\n-- sections --");
        // (pid, tid, ext) → (only A, only B, identical, differ)
        type Counts = (usize, usize, usize, usize);
        let mut sched: BTreeMap<(u16, u8, u16), Counts> = BTreeMap::new();
        for s in &self.sections {
            let k = s.key_a.or(s.key_b).unwrap();
            if !all && (0x50..=0x6F).contains(&k.table_id) {
                let e = sched
                    .entry((k.pid, k.table_id, k.ext.unwrap_or(0)))
                    .or_default();
                match s.pairing {
                    "only A" => e.0 += 1,
                    "only B" => e.1 += 1,
                    _ if s.identical => e.2 += 1,
                    _ => e.3 += 1,
                }
                continue;
            }
            let label = match (s.key_a, s.key_b) {
                (Some(x), Some(y)) if x == y => format!("{x}"),
                (Some(x), Some(y)) => format!("A {x} ~ B {y} ({})", s.pairing),
                (Some(x), None) => format!("{x} ONLY IN A"),
                (None, Some(y)) => format!("{y} ONLY IN B"),
                _ => unreachable!(),
            };
            let st = if s.identical {
                "identical".to_string()
            } else if s.key_a.is_some() && s.key_b.is_some() {
                format!("{} field changes", s.changes.len())
            } else {
                String::new()
            };
            let _ = writeln!(
                o,
                "{label} {} — x{}|x{} max interval {}|{} s len {}|{} {st}",
                s.table,
                s.count.0,
                s.count.1,
                f(s.interval.0.map(|i| i.max), 3),
                f(s.interval.1.map(|i| i.max), 3),
                s.length.0,
                s.length.1
            );
            for c in &s.changes {
                let _ = writeln!(
                    o,
                    "    {}: {} → {}",
                    c.path,
                    c.a.as_deref().unwrap_or("(absent)"),
                    c.b.as_deref().unwrap_or("(absent)")
                );
            }
        }
        for ((pid, tid, ext), (oa, ob, same, diff)) in sched {
            let _ = writeln!(
                o,
                "pid={pid:#06x} tid={tid:#04x} ext={ext:#06x} EIT schedule: {oa} sections only in A, {ob} only in B, {same} identical, {diff} differ"
            );
        }
        o
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(name: &str, f: Vec<(&str, Node)>) -> Node {
        Node::Struct(
            name.into(),
            f.into_iter().map(|(k, v)| (k.to_string(), v)).collect(),
        )
    }

    #[test]
    fn descriptor_lists_match_by_type_and_ids_by_value() {
        let d = |gi: i128| {
            s(
                "TerrestrialDeliverySystem",
                vec![("tag", Node::Int(0xFA)), ("guard_interval", Node::Int(gi))],
            )
        };
        let n = s(
            "NetworkName",
            vec![("tag", Node::Int(0x40)), ("name", Node::Str("x".into()))],
        );
        let ts = |id: i128, ds: Vec<Node>| {
            s(
                "TransportStreamEntry",
                vec![
                    ("transport_stream_id", Node::Int(id)),
                    ("descriptors", Node::List(ds)),
                ],
            )
        };
        let a = s(
            "T",
            vec![("ts", Node::List(vec![ts(1, vec![d(2)]), ts(2, vec![])]))],
        );
        let b = s(
            "T",
            vec![(
                "ts",
                Node::List(vec![ts(2, vec![]), ts(1, vec![n.clone(), d(1)])]),
            )],
        );
        let mut out = Vec::new();
        diff_nodes("", &a, &b, &mut out);
        assert_eq!(out.len(), 2, "{out:?}");
        assert_eq!(
            out[0].path,
            "ts[transport_stream_id=0x01].descriptors[TerrestrialDeliverySystem].guard_interval"
        );
        assert_eq!(
            (out[0].a.as_deref(), out[0].b.as_deref()),
            (Some("2"), Some("1"))
        );
        assert_eq!(
            out[1].path,
            "ts[transport_stream_id=0x01].descriptors[NetworkName]"
        );
        assert!(out[1].a.is_none());
    }
}
