//! Inspect report: transport inventory, continuity, PCR timing, programs
//! and PES timestamps, every distinct section with repetition statistics,
//! and the decoded SI summary.

use crate::decode;
use crate::names;
use crate::scan::{Capture, SectionError, SlotKey, Tally};
use crate::si::{self, SiSummary};
use crate::timebase::Timebase;
use crate::tree::{self, Node};
use arib_ts::cc::CcEventKind;
use arib_ts::pcr::{BASE_MODULUS, ts90k_wrap_diff};
use serde::Serialize;
use std::collections::BTreeMap;
use std::fmt::Write as _;

/// Min/mean/max of a set of intervals (seconds).
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Default)]
pub struct Intervals {
    /// Number of intervals.
    pub n: usize,
    /// Minimum.
    pub min: f64,
    /// Mean.
    pub mean: f64,
    /// Maximum.
    pub max: f64,
}

impl Intervals {
    /// Statistics of `v` (`None` if empty).
    pub fn of(v: &[f64]) -> Option<Self> {
        if v.is_empty() {
            return None;
        }
        let min = v.iter().copied().fold(f64::INFINITY, f64::min);
        let max = v.iter().copied().fold(f64::NEG_INFINITY, f64::max);
        Some(Intervals {
            n: v.len(),
            min,
            mean: v.iter().sum::<f64>() / v.len() as f64,
            max,
        })
    }
}

/// Per-PID line.
#[derive(Debug, Clone, Serialize)]
pub struct PidReport {
    /// PID.
    pub pid: u16,
    /// What it carries (from PSI/SI and allocation tables).
    pub kind: String,
    /// Packets.
    pub packets: u64,
    /// Share of all packets (%).
    pub share_pct: f64,
    /// Rate (kbit/s) over the capture duration.
    pub rate_kbps: Option<f64>,
    /// transport_error_indicator packets.
    pub tei: u64,
    /// Scrambled packets.
    pub scrambled: u64,
    /// PUSI packets.
    pub pusi: u64,
    /// Continuity events.
    pub cc_events: u64,
    /// PCRs.
    pub pcrs: usize,
    /// discontinuity_indicator packets.
    pub discontinuity: u64,
}

/// PCR timing of one PID (H.222.0 §2.7.2, §2.4.2.2).
#[derive(Debug, Clone, Serialize)]
pub struct PcrReport {
    /// PID.
    pub pid: u16,
    /// PCRs.
    pub count: usize,
    /// Intervals between successive PCR values (seconds of PCR time).
    pub intervals: Option<Intervals>,
    /// Intervals > 0.1 s (§2.7.2).
    pub over_100ms: usize,
    /// PCR jumps > 5 s or backwards (discontinuities).
    pub discontinuities: u64,
    /// Rate implied by packets/PCR time (bit/s).
    pub rate_bps: Option<f64>,
    /// Max |PCR − constant-rate fit| in ns (meaningful only for CBR
    /// streams; PCR tolerance is ±500 ns, §2.4.2.2).
    pub fit_max_ns: Option<f64>,
    /// RMS of the same residual (ns).
    pub fit_rms_ns: Option<f64>,
}

/// One elementary stream.
#[derive(Debug, Clone, Serialize)]
pub struct EsReport {
    /// elementary_PID.
    pub pid: u16,
    /// stream_type.
    pub stream_type: u8,
    /// Table 2-34 description.
    pub stream_type_name: &'static str,
    /// Packets.
    pub packets: u64,
    /// Rate (kbit/s).
    pub rate_kbps: Option<f64>,
    /// PES packets started.
    pub pes_count: u64,
    /// stream_id values.
    pub stream_ids: Vec<u8>,
    /// First/last PTS (90 kHz).
    pub pts_first_last: Option<(u64, u64)>,
    /// PTS span in seconds (wrap-aware).
    pub pts_span_s: Option<f64>,
    /// First/last DTS.
    pub dts_first_last: Option<(u64, u64)>,
    /// Optional-header flags of the first PES header.
    pub pes_header_flags: Option<String>,
    /// ES_info descriptors.
    pub descriptors: Vec<Node>,
}

/// One program.
#[derive(Debug, Clone, Serialize)]
pub struct ProgramReport {
    /// program_number.
    pub program_number: u16,
    /// program_map_PID.
    pub pmt_pid: u16,
    /// PMT version.
    pub version: u8,
    /// PCR_PID.
    pub pcr_pid: u16,
    /// program_info descriptors.
    pub descriptors: Vec<Node>,
    /// Streams.
    pub streams: Vec<EsReport>,
}

/// Per version of a section slot.
#[derive(Debug, Clone, Serialize)]
pub struct VersionReport {
    /// version_number.
    pub version: u8,
    /// current_next_indicator.
    pub cni: bool,
    /// last_section_number values.
    pub lsn: Vec<u8>,
    /// Distinct bodies.
    pub bodies: usize,
    /// Occurrences.
    pub count: u64,
    /// Section length (bytes, first body).
    pub length: usize,
}

/// One repeatedly transmitted section.
#[derive(Debug, Clone, Serialize)]
pub struct SectionReport {
    /// Identity.
    pub key: SlotKey,
    /// Table name.
    pub table: &'static str,
    /// Occurrences.
    pub count: usize,
    /// Versions (long form).
    pub versions: Vec<VersionReport>,
    /// Intervals between occurrences (seconds).
    pub interval: Option<Intervals>,
    /// Gap from capture start to first occurrence and from last to end.
    pub edge_gaps_s: Option<(f64, f64)>,
    /// Latest section length.
    pub latest_length: usize,
    /// Latest decoded content.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub decoded: Option<Node>,
    /// Decode error of the latest body.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub decode_error: Option<String>,
}

/// Sections on non-SI PIDs.
#[derive(Debug, Clone, Serialize)]
pub struct OtherSectionReport {
    /// PID.
    pub pid: u16,
    /// table_id.
    pub table_id: u8,
    /// Table name.
    pub table: &'static str,
    /// Sections.
    pub count: u64,
    /// Distinct (ext, version, sn).
    pub distinct: u64,
    /// CRC failures.
    pub crc_errors: u64,
}

/// Continuity event summary.
#[derive(Debug, Clone, Serialize)]
pub struct CcSummary {
    /// Kind.
    pub kind: String,
    /// PID.
    pub pid: u16,
    /// Count.
    pub count: u64,
    /// First packet.
    pub first_packet: u64,
}

/// Full inspect report.
#[derive(Debug, Clone, Serialize)]
pub struct InspectReport {
    /// Source.
    pub source: String,
    /// Framing (188/192/204).
    pub packet_size: Option<usize>,
    /// Packets.
    pub packets: u64,
    /// Reader sync counters.
    pub sync_losses: u64,
    /// Bytes skipped while (re)synchronising.
    pub skipped_bytes: u64,
    /// Trailing bytes.
    pub trailing_bytes: u64,
    /// Duration (s) from the time base.
    pub duration_s: Option<f64>,
    /// PID whose PCRs define times.
    pub timebase_pcr_pid: Option<u16>,
    /// Packet-rate estimate (bit/s) from PCRs.
    pub rate_bps: Option<f64>,
    /// Null packets present (0 ⇒ nulls were stripped or the TS is VBR).
    pub null_packets: u64,
    /// PIDs.
    pub pids: Vec<PidReport>,
    /// Continuity events by kind/PID.
    pub cc: Vec<CcSummary>,
    /// PCR timing.
    pub pcr: Vec<PcrReport>,
    /// Programs.
    pub programs: Vec<ProgramReport>,
    /// Sections.
    pub sections: Vec<SectionReport>,
    /// Sections on other PIDs.
    pub other_sections: Vec<OtherSectionReport>,
    /// Section errors.
    pub section_errors: Vec<SectionError>,
    /// Total section errors.
    pub section_error_count: u64,
    /// Section assembler events.
    pub demux_events: Vec<Tally>,
    /// Packet rule violations.
    pub packet_violations: Vec<Tally>,
    /// Decoded SI.
    pub si: SiSummary,
}

/// Options for [`inspect`].
#[derive(Debug, Clone, Default)]
pub struct InspectOptions {
    /// Include the decoded tree of every section.
    pub decode_sections: bool,
    /// Nominal bit rate when there are no PCRs.
    pub nominal_bps: Option<f64>,
}

/// Choose the time base: PCR_PID of the first program, else the PID with
/// most PCRs.
pub fn timebase(cap: &Capture, nominal_bps: Option<f64>) -> Timebase {
    let pcr_pid = cap
        .pmts()
        .values()
        .map(|(_, p)| p.body.pcr_pid)
        .find(|&p| p != 0x1FFF);
    Timebase::from_stats(&cap.stats, pcr_pid, nominal_bps)
}

/// Intervals between `occurrences` (packet indices) in seconds, and the
/// edge gaps to the capture start/end.
pub fn occurrence_intervals(
    tb: &Timebase,
    occ: &[u64],
    packets: u64,
) -> (Option<Intervals>, Option<(f64, f64)>) {
    if !tb.known() || occ.is_empty() {
        return (None, None);
    }
    let t: Vec<f64> = occ.iter().filter_map(|&i| tb.time(i)).collect();
    let d: Vec<f64> = t.windows(2).map(|w| w[1] - w[0]).collect();
    let start = tb.time(0).unwrap_or(0.0);
    let end = tb.time(packets.saturating_sub(1)).unwrap_or(0.0);
    (
        Intervals::of(&d),
        Some((t[0] - start, end - t[t.len() - 1])),
    )
}

fn pcr_report(cap: &Capture, pid: u16) -> PcrReport {
    let pcrs = &cap.stats.pids[&pid].pcrs;
    let tb = Timebase::from_stats(&cap.stats, Some(pid), None);
    let samples: Vec<(u64, f64)> = pcrs
        .iter()
        .filter_map(|&(i, _)| tb.time(i).map(|t| (i, t)))
        .collect();
    let d: Vec<f64> = samples.windows(2).map(|w| w[1].1 - w[0].1).collect();
    let over = d.iter().filter(|&&x| x > 0.1).count();
    // Least-squares fit t = a + b·idx; residuals in ns.
    let (fit_max, fit_rms) = if samples.len() >= 3 {
        let n = samples.len() as f64;
        let mx = samples.iter().map(|s| s.0 as f64).sum::<f64>() / n;
        let my = samples.iter().map(|s| s.1).sum::<f64>() / n;
        let sxx: f64 = samples.iter().map(|s| (s.0 as f64 - mx).powi(2)).sum();
        let sxy: f64 = samples.iter().map(|s| (s.0 as f64 - mx) * (s.1 - my)).sum();
        let b = if sxx > 0.0 { sxy / sxx } else { 0.0 };
        let r: Vec<f64> = samples
            .iter()
            .map(|s| (s.1 - (my + b * (s.0 as f64 - mx))) * 1e9)
            .collect();
        let max = r.iter().fold(0f64, |m, &x| m.max(x.abs()));
        let rms = (r.iter().map(|x| x * x).sum::<f64>() / n).sqrt();
        (Some(max), Some(rms))
    } else {
        (None, None)
    };
    PcrReport {
        pid,
        count: pcrs.len(),
        intervals: Intervals::of(&d),
        over_100ms: over,
        discontinuities: tb.discontinuities,
        rate_bps: tb.rate_bps(),
        fit_max_ns: fit_max,
        fit_rms_ns: fit_rms,
    }
}

/// What a PID carries, for the inventory.
pub fn pid_kinds(cap: &Capture) -> BTreeMap<u16, String> {
    let mut m = BTreeMap::new();
    for &pid in cap.stats.pids.keys() {
        if let Some(n) = names::pid_allocation(pid) {
            m.insert(pid, n.to_string());
        }
    }
    if let Some(p) = cap.pat() {
        for e in &p.body.entries {
            if e.program_number == 0 {
                m.insert(e.pid, "NIT (network_PID)".into());
            } else {
                m.insert(e.pid, format!("PMT program {:#06x}", e.program_number));
            }
        }
    }
    for (prog, (_, pmt)) in cap.pmts() {
        if pmt.body.pcr_pid != 0x1FFF {
            m.entry(pmt.body.pcr_pid)
                .or_insert_with(|| format!("PCR (program {prog:#06x})"));
        }
        for s in &pmt.body.streams {
            m.insert(
                s.elementary_pid,
                format!(
                    "ES {:#04x} {} (program {prog:#06x})",
                    s.stream_type,
                    names::stream_type_name(s.stream_type)
                ),
            );
        }
        let ca = |ds: &[arib_ts::descriptor::Descriptor], m: &mut BTreeMap<u16, String>| {
            // conditional_access_descriptor (H.222.0 §2.6.16) / access_control_descriptor (B10 §6.2.54)
            for d in ds
                .iter()
                .filter(|d| matches!(d.tag, 0x09 | 0xF6) && d.data.len() >= 4)
            {
                let pid = u16::from_be_bytes([d.data[2], d.data[3]]) & 0x1FFF;
                if pid != 0x1FFF {
                    m.insert(
                        pid,
                        format!(
                            "{} (program {prog:#06x})",
                            if d.tag == 0x09 {
                                "ECM"
                            } else {
                                "access control"
                            }
                        ),
                    );
                }
            }
        };
        ca(&pmt.body.program_info, &mut m);
        for s in &pmt.body.streams {
            ca(&s.descriptors, &mut m);
        }
    }
    if let Some(c) = cap
        .slots
        .iter()
        .find(|(k, _)| k.pid == 1 && k.table_id == 1)
        .and_then(|(_, s)| arib_ts::section::Long::<arib_ts::psi::Cat>::decode(&s.latest).ok())
    {
        for d in c
            .body
            .descriptors
            .iter()
            .filter(|d| matches!(d.tag, 0x09 | 0xF6) && d.data.len() >= 4)
        {
            let pid = u16::from_be_bytes([d.data[2], d.data[3]]) & 0x1FFF;
            if pid != 0x1FFF {
                m.insert(
                    pid,
                    format!(
                        "EMM ({} in CAT)",
                        if d.tag == 0x09 {
                            "CA_descriptor"
                        } else {
                            "access_control_descriptor"
                        }
                    ),
                );
            }
        }
    }
    for &(pid, tid) in cap.other_sections.keys() {
        m.entry(pid)
            .or_insert_with(|| format!("sections {}", names::table_name(tid)));
    }
    m
}

fn rate_kbps(packets: u64, duration: Option<f64>) -> Option<f64> {
    duration
        .filter(|&d| d > 0.0)
        .map(|d| packets as f64 * 1504.0 / d / 1000.0)
}

fn pts_span(v: &[(u64, u64)]) -> Option<f64> {
    if v.len() < 2 {
        return None;
    }
    let mut acc = 0u64;
    let (mut lo, mut hi) = (0i64, 0i64);
    let mut cur = 0i64;
    for w in v.windows(2) {
        let d = ts90k_wrap_diff(w[1].1, w[0].1);
        let s = if d > BASE_MODULUS / 2 {
            d as i64 - BASE_MODULUS as i64
        } else {
            d as i64
        };
        cur += s;
        lo = lo.min(cur);
        hi = hi.max(cur);
        acc += 1;
    }
    let _ = acc;
    Some((hi - lo) as f64 / 90_000.0)
}

/// Build the inspect report.
pub fn inspect(cap: &Capture, opts: &InspectOptions) -> InspectReport {
    let tb = timebase(cap, opts.nominal_bps);
    let duration = tb.duration(cap.packets);
    let kinds = pid_kinds(cap);
    let pids = cap
        .stats
        .pids
        .iter()
        .map(|(&pid, s)| PidReport {
            pid,
            kind: kinds
                .get(&pid)
                .cloned()
                .unwrap_or_else(|| "unreferenced".into()),
            packets: s.packets,
            share_pct: 100.0 * s.packets as f64 / cap.packets.max(1) as f64,
            rate_kbps: rate_kbps(s.packets, duration),
            tei: s.tei,
            scrambled: s.scrambled,
            pusi: s.pusi,
            cc_events: s.cc_events,
            pcrs: s.pcrs.len(),
            discontinuity: s.discontinuity,
        })
        .collect();
    let mut cc: BTreeMap<(String, u16), CcSummary> = BTreeMap::new();
    for e in &cap.stats.cc_events {
        let kind = match e.kind {
            CcEventKind::Break { .. } => "break",
            CcEventKind::Discontinuity { .. } => "signalled discontinuity",
            CcEventKind::Duplicate { identical: true } => "duplicate",
            CcEventKind::Duplicate { identical: false } => "duplicate (content differs)",
            CcEventKind::ExcessDuplicate => "excess duplicate",
            CcEventKind::NoPayloadChanged { .. } => "counter changed without payload",
            CcEventKind::TransportError => "transport_error_indicator",
        };
        cc.entry((kind.into(), e.pid))
            .or_insert(CcSummary {
                kind: kind.into(),
                pid: e.pid,
                count: 0,
                first_packet: e.packet_index,
            })
            .count += 1;
    }
    let pcr = cap
        .stats
        .pids
        .iter()
        .filter(|(_, s)| !s.pcrs.is_empty())
        .map(|(&p, _)| pcr_report(cap, p))
        .collect();
    let mut programs = Vec::new();
    for (prog, (pmt_pid, pmt)) in cap.pmts() {
        let streams = pmt
            .body
            .streams
            .iter()
            .map(|s| {
                let pk = cap
                    .stats
                    .pids
                    .get(&s.elementary_pid)
                    .map_or(0, |x| x.packets);
                let t = cap.pes.get(&s.elementary_pid);
                EsReport {
                    pid: s.elementary_pid,
                    stream_type: s.stream_type,
                    stream_type_name: names::stream_type_name(s.stream_type),
                    packets: pk,
                    rate_kbps: rate_kbps(pk, duration),
                    pes_count: t.map_or(0, |t| t.pes_count),
                    stream_ids: t
                        .map(|t| t.stream_ids.iter().copied().collect())
                        .unwrap_or_default(),
                    pts_first_last: t.and_then(|t| Some((t.pts.first()?.1, t.pts.last()?.1))),
                    pts_span_s: t.and_then(|t| pts_span(&t.pts)),
                    dts_first_last: t.and_then(|t| Some((t.dts.first()?.1, t.dts.last()?.1))),
                    pes_header_flags: t.and_then(|t| t.first_flags.clone()),
                    descriptors: s.descriptors.iter().map(tree::of).collect(),
                }
            })
            .collect();
        programs.push(ProgramReport {
            program_number: prog,
            pmt_pid,
            version: pmt.header.version_number,
            pcr_pid: pmt.body.pcr_pid,
            descriptors: pmt.body.program_info.iter().map(tree::of).collect(),
            streams,
        });
    }
    let sections = cap
        .slots
        .iter()
        .map(|(k, s)| {
            let (interval, edges) = occurrence_intervals(&tb, &s.occurrences, cap.packets);
            let (decoded, decode_error) = match decode::decode(k.pid, &s.latest) {
                Ok(t) => (opts.decode_sections.then(|| t.tree()), None),
                Err(e) => (None, Some(e.to_string())),
            };
            SectionReport {
                key: *k,
                table: names::table_name(k.table_id),
                count: s.occurrences.len(),
                versions: s
                    .versions
                    .iter()
                    .map(|v| VersionReport {
                        version: v.version,
                        cni: v.cni,
                        lsn: v.lsns.iter().copied().collect(),
                        bodies: v.bodies.len(),
                        count: v.bodies.iter().map(|b| b.count).sum(),
                        length: v.bodies.first().map_or(0, |b| b.bytes.len()),
                    })
                    .collect(),
                interval,
                edge_gaps_s: edges,
                latest_length: s.latest.len(),
                decoded,
                decode_error,
            }
        })
        .collect();
    let other_sections = cap
        .other_sections
        .iter()
        .map(|(&(pid, tid), o)| OtherSectionReport {
            pid,
            table_id: tid,
            table: names::table_name(tid),
            count: o.count,
            distinct: o.distinct,
            crc_errors: o.crc_errors,
        })
        .collect();
    InspectReport {
        source: cap.source.clone(),
        packet_size: cap.format.map(|f| f.size()),
        packets: cap.packets,
        sync_losses: cap.reader.sync_losses,
        skipped_bytes: cap.reader.skipped_bytes,
        trailing_bytes: cap.reader.trailing_bytes,
        duration_s: duration,
        timebase_pcr_pid: tb.pcr_pid,
        rate_bps: tb.rate_bps(),
        null_packets: cap.stats.pids.get(&0x1FFF).map_or(0, |s| s.packets),
        pids,
        cc: cc.into_values().collect(),
        pcr,
        programs,
        sections,
        other_sections,
        section_errors: cap.section_errors.clone(),
        section_error_count: cap.section_error_count,
        demux_events: cap.demux_events.values().cloned().collect(),
        packet_violations: cap.packet_violations.values().cloned().collect(),
        si: si::summarize(cap),
    }
}

fn opt_f(v: Option<f64>, scale: f64, prec: usize) -> String {
    v.map(|x| format!("{:.*}", prec, x * scale))
        .unwrap_or_else(|| "-".into())
}

fn ivl(i: &Option<Intervals>) -> String {
    match i {
        Some(i) => format!("{:.3}/{:.3}/{:.3} s", i.min, i.mean, i.max),
        None => "-".into(),
    }
}

impl InspectReport {
    /// Text rendering.
    pub fn to_text(&self) -> String {
        let mut o = String::new();
        let _ = writeln!(o, "== {} ==", self.source);
        let _ = writeln!(
            o,
            "packets {} ({}-byte units), sync losses {}, skipped {} B, trailing {} B",
            self.packets,
            self.packet_size.map_or("?".into(), |s| s.to_string()),
            self.sync_losses,
            self.skipped_bytes,
            self.trailing_bytes
        );
        let _ = writeln!(
            o,
            "duration {} s (time base: PCR of {}), packet rate {} Mbit/s, null packets {}{}",
            opt_f(self.duration_s, 1.0, 3),
            self.timebase_pcr_pid
                .map_or("none".into(), |p| format!("{p:#06x}")),
            opt_f(self.rate_bps, 1e-6, 6),
            self.null_packets,
            if self.null_packets == 0 {
                " (stripped or VBR: packet positions are not CBR time)"
            } else {
                ""
            }
        );
        let _ = writeln!(o, "\n-- PIDs --");
        let _ = writeln!(
            o,
            "{:>6} {:>9} {:>6} {:>10} {:>5} {:>5} {:>5} {:>5}  kind",
            "pid", "packets", "%", "kbit/s", "tei", "scr", "cc", "pcr"
        );
        for p in &self.pids {
            let _ = writeln!(
                o,
                "{:#06x} {:>9} {:>6.2} {:>10} {:>5} {:>5} {:>5} {:>5}  {}",
                p.pid,
                p.packets,
                p.share_pct,
                opt_f(p.rate_kbps, 1.0, 1),
                p.tei,
                p.scrambled,
                p.cc_events,
                p.pcrs,
                p.kind
            );
        }
        if !self.cc.is_empty() {
            let _ = writeln!(o, "\n-- continuity events (H.222.0 §2.4.3.3) --");
            for c in &self.cc {
                let _ = writeln!(
                    o,
                    "pid {:#06x}: {} x{} (first at packet {})",
                    c.pid, c.kind, c.count, c.first_packet
                );
            }
        }
        let _ = writeln!(o, "\n-- PCR (H.222.0 §2.7.2: interval ≤ 0.1 s) --");
        for p in &self.pcr {
            let _ = writeln!(
                o,
                "pid {:#06x}: {} PCRs, interval min/mean/max {}, >0.1 s: {}, discontinuities {}, rate {} Mbit/s, CBR-fit residual max {} ns rms {} ns",
                p.pid,
                p.count,
                ivl(&p.intervals),
                p.over_100ms,
                p.discontinuities,
                opt_f(p.rate_bps, 1e-6, 6),
                opt_f(p.fit_max_ns, 1.0, 0),
                opt_f(p.fit_rms_ns, 1.0, 0)
            );
        }
        let _ = writeln!(o, "\n-- programs --");
        for p in &self.programs {
            let _ = writeln!(
                o,
                "program {:#06x}: PMT pid {:#06x} v{}, PCR_PID {:#06x}",
                p.program_number, p.pmt_pid, p.version, p.pcr_pid
            );
            for d in &p.descriptors {
                let _ = writeln!(o, "    {}", tree::scalar(d, ""));
            }
            for s in &p.streams {
                let _ = writeln!(
                    o,
                    "  es {:#06x} type {:#04x} ({}) {} pkts {} kbit/s, {} PES, stream_id {:02x?}, PTS span {} s, PTS {:?} DTS {:?}",
                    s.pid,
                    s.stream_type,
                    s.stream_type_name,
                    s.packets,
                    opt_f(s.rate_kbps, 1.0, 1),
                    s.pes_count,
                    s.stream_ids,
                    opt_f(s.pts_span_s, 1.0, 3),
                    s.pts_first_last,
                    s.dts_first_last
                );
                if let Some(f) = &s.pes_header_flags {
                    let _ = writeln!(o, "      first PES header: {f}");
                }
                for d in &s.descriptors {
                    let _ = writeln!(o, "      {}", tree::scalar(d, ""));
                }
            }
        }
        self.si_text(&mut o);
        let _ = writeln!(
            o,
            "\n-- sections (interval min/mean/max; edge gaps start/end) --"
        );
        for s in &self.sections {
            let vers: Vec<String> = s
                .versions
                .iter()
                .map(|v| {
                    format!(
                        "v{}{} lsn{:?} x{}{}",
                        v.version,
                        if v.cni { "" } else { "(next)" },
                        v.lsn,
                        v.count,
                        if v.bodies > 1 {
                            format!(" BODIES={}", v.bodies)
                        } else {
                            String::new()
                        }
                    )
                })
                .collect();
            let _ = writeln!(
                o,
                "{} {:<20} x{:<5} len {:<5} {} edges {} {}{}",
                s.key,
                s.table,
                s.count,
                s.latest_length,
                ivl(&s.interval),
                s.edge_gaps_s
                    .map_or("-".into(), |(a, b)| format!("{a:.2}/{b:.2}")),
                vers.join(" "),
                s.decode_error
                    .as_ref()
                    .map_or(String::new(), |e| format!(" DECODE ERROR: {e}"))
            );
            if let Some(d) = &s.decoded {
                let mut t = String::new();
                tree::render(d, 2, &mut t);
                o.push_str(&t);
            }
        }
        if !self.other_sections.is_empty() {
            let _ = writeln!(o, "\n-- sections on other PIDs (counted only) --");
            for s in &self.other_sections {
                let _ = writeln!(
                    o,
                    "pid {:#06x} tid {:#04x} {}: {} sections, {} distinct, {} CRC errors",
                    s.pid, s.table_id, s.table, s.count, s.distinct, s.crc_errors
                );
            }
        }
        if self.section_error_count > 0 {
            let _ = writeln!(o, "\n-- section errors: {} --", self.section_error_count);
            for e in self.section_errors.iter().take(20) {
                let _ = writeln!(
                    o,
                    "pid {:#06x} tid {:#04x} packet {}: {}",
                    e.pid, e.table_id, e.packet, e.message
                );
            }
        }
        if !self.demux_events.is_empty() || !self.packet_violations.is_empty() {
            let _ = writeln!(o, "\n-- assembler events / packet rule violations --");
            for t in self.demux_events.iter().chain(&self.packet_violations) {
                let _ = writeln!(
                    o,
                    "H.222.0 {} x{} (first pid {:#06x} packet {}): {}",
                    t.clause, t.count, t.pid, t.first_packet, t.example
                );
            }
        }
        o
    }

    fn si_text(&self, o: &mut String) {
        let s = &self.si;
        let _ = writeln!(o, "\n-- SI --");
        let _ = writeln!(
            o,
            "PAT: transport_stream_id {} network_PID {} programs {}",
            s.transport_stream_id
                .map_or("-".into(), |t| format!("{t:#06x}")),
            s.network_pid.map_or("-".into(), |t| format!("{t:#06x}")),
            s.programs
                .iter()
                .map(|(p, pid)| format!("{p:#06x}→{pid:#06x}"))
                .collect::<Vec<_>>()
                .join(" ")
        );
        for n in &s.networks {
            let _ = writeln!(
                o,
                "NIT tid {:#04x} network_id {:#06x} v{} name {:?} system_management_id {} {}",
                n.table_id,
                n.network_id,
                n.version,
                n.name.as_deref().unwrap_or("-"),
                n.system_management_id
                    .map_or("-".into(), |x| format!("{x:#06x}")),
                n.other_descriptors.join(", ")
            );
            for t in &n.transport_streams {
                let _ = writeln!(
                    o,
                    "  TS {:#06x} onid {:#06x}",
                    t.transport_stream_id, t.original_network_id
                );
                if let Some(d) = &t.delivery {
                    let _ = writeln!(
                        o,
                        "    delivery: area {:#05x} GI {} ({}) {} ({}) freq {:?} = {:?} MHz",
                        d.area_code,
                        d.guard_interval_name,
                        d.guard_interval,
                        d.transmission_mode_name,
                        d.transmission_mode,
                        d.frequencies,
                        d.frequencies_mhz
                            .iter()
                            .map(|f| format!("{f:.6}"))
                            .collect::<Vec<_>>()
                    );
                }
                if let Some(i) = &t.ts_info {
                    let _ = writeln!(
                        o,
                        "    TS info: remote key {} name {:?} types {}",
                        i.remote_control_key_id,
                        i.ts_name,
                        i.transmission_types
                            .iter()
                            .map(|(t, v)| format!("{t:#04x}:{v:04x?}"))
                            .collect::<Vec<_>>()
                            .join(" ")
                    );
                }
                if let Some(p) = &t.partial_reception {
                    let _ = writeln!(o, "    partial reception: {p:04x?}");
                }
                if let Some(l) = &t.service_list {
                    let _ = writeln!(
                        o,
                        "    service list: {}",
                        l.iter()
                            .map(|(a, b)| format!("{a:#06x}/{b:#04x}"))
                            .collect::<Vec<_>>()
                            .join(" ")
                    );
                }
                if !t.other_descriptors.is_empty() {
                    let _ = writeln!(o, "    other: {}", t.other_descriptors.join(", "));
                }
            }
        }
        for v in &s.services {
            let _ = writeln!(
                o,
                "SDT tid {:#04x} ts {:#06x} onid {:#06x} service {:#06x} type {} {:?} / {:?} sched {} p/f {} {} free_CA {} {}",
                v.table_id,
                v.transport_stream_id,
                v.original_network_id,
                v.service_id,
                v.service_type.map_or("-".into(), |t| format!("{t:#04x}")),
                v.provider.as_deref().unwrap_or("-"),
                v.name.as_deref().unwrap_or("-"),
                v.eit_schedule,
                v.eit_present_following,
                v.running_status,
                v.free_ca_mode,
                v.other_descriptors.join(", ")
            );
        }
        for b in &s.bits {
            let _ = writeln!(
                o,
                "BIT onid {:#06x} v{} broadcast_view_propriety {} first: {}",
                b.original_network_id,
                b.version,
                b.broadcast_view_propriety,
                b.first_descriptors.join(", ")
            );
            for p in &b.si_parameter {
                let _ = writeln!(o, "    SI param tid {:#04x}: {}", p.0, p.1);
            }
            for br in &b.broadcasters {
                let _ = writeln!(
                    o,
                    "  broadcaster {:#04x} {:?} {} {}",
                    br.broadcaster_id,
                    br.name.as_deref().unwrap_or("-"),
                    br.extended.as_deref().unwrap_or(""),
                    br.other_descriptors.join(", ")
                );
                for p in &br.si_parameter {
                    let _ = writeln!(o, "    SI param tid {:#04x}: {}", p.0, p.1);
                }
            }
        }
        for e in &s.present_following {
            let _ = writeln!(
                o,
                "EIT {:#04x} service {:#06x} sec {} event {:#06x} {} +{} {} {:?}",
                e.table_id,
                e.service_id,
                e.section_number,
                e.event_id,
                e.start,
                e.duration,
                e.running_status,
                e.title.as_deref().unwrap_or("-")
            );
        }
        for sc in &s.schedules {
            let _ = writeln!(
                o,
                "EIT schedule {:#04x} service {:#06x}: {} sections, {} events, {} .. {}",
                sc.table_id,
                sc.service_id,
                sc.sections,
                sc.events,
                sc.first_start.as_deref().unwrap_or("-"),
                sc.last_start.as_deref().unwrap_or("-")
            );
        }
        if let Some(t) = &s.time {
            let _ = writeln!(
                o,
                "TOT {} TDT {} {}",
                t.tot.as_deref().unwrap_or("-"),
                t.tdt.as_deref().unwrap_or("-"),
                t.local_time_offsets.join("; ")
            );
        }
    }
}
