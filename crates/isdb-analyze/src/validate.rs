//! Conformance validation with clause citations.
//!
//! Combines the `isdb-ts` `check_*` functions (H.222.0), `isdb-si`
//! `conformance::check` (B10 value rules), and the cross-table / transport
//! rules stated in the specs (see `SPEC-NOTES.md` for every
//! interpretation).  Uses only decoders: no packing assumption of any
//! multiplexer is shared.
//!
//! Severities:
//! * **error** — a "shall" of H.222.0 / B10 Part 1–2 / B32 Part 2 §5.2 is
//!   violated;
//! * **warning** — reserved-bit values (decoders ignore them), rules whose
//!   text admits substitution, operational criteria (B10 Part 2 Table 5-2
//!   frequencies: "only a criterion of operation and is not the standard
//!   value"), B10 Appendix guidelines ("not a part of the standard"), and
//!   estimates (timing derived from interpolated PCRs);
//! * **info** — observations (unreferenced PIDs, next-version sections,
//!   frequency rows that meet the criterion, …).

use crate::decode::{self, Table};
use crate::inspect::{self, Intervals};
use crate::names;
use crate::scan::{Capture, SlotKey};
use crate::timebase::Timebase;
use isdb_si::AnyTable;
use isdb_si::conformance::{self, Kind};
use isdb_si::descriptor::Descriptor;
use isdb_ts::cc::CcEventKind;
use isdb_ts::pcr::{pcr_signed_diff, ts90k_signed_diff};
use isdb_ts::psi::{self, Pmt};
use isdb_ts::section::Long;
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

/// Finding severity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    /// Observation.
    Info,
    /// Reserved bits, operational criteria, guidelines, estimates.
    Warning,
    /// Violated "shall".
    Error,
}

impl std::fmt::Display for Severity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Severity::Info => "info",
            Severity::Warning => "warning",
            Severity::Error => "error",
        })
    }
}

/// One finding.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Finding {
    /// Severity.
    pub severity: Severity,
    /// Check identifier (e.g. `"pcr.interval"`).
    pub check: &'static str,
    /// Clause stating the rule.
    pub clause: String,
    /// Where.
    pub location: String,
    /// What.
    pub message: String,
    /// Occurrences merged into this finding.
    pub count: u64,
}

/// Measured vs criterion row of B10 Part 2 Table 5-2.
#[derive(Debug, Clone, Serialize)]
pub struct FrequencyRow {
    /// Section identity.
    pub key: SlotKey,
    /// Table name.
    pub table: &'static str,
    /// Criterion text.
    pub criterion: &'static str,
    /// Criterion seconds.
    pub criterion_s: f64,
    /// Occurrences.
    pub count: usize,
    /// Measured intervals.
    pub interval: Option<Intervals>,
    /// Largest gap including capture edges (lower bound of the true
    /// maximum interval).
    pub max_gap_s: Option<f64>,
    /// "ok", "exceeded", "insufficient capture".
    pub status: &'static str,
}

/// Bench expectations (all optional).
#[derive(Debug, Clone, Default, Serialize)]
pub struct Expectations {
    /// PAT transport_stream_id (also SDT/NIT/EIT actual).
    pub transport_stream_id: Option<u16>,
    /// original_network_id (SDT, NIT TS loop, EIT, BIT).
    pub original_network_id: Option<u16>,
    /// NIT actual network_id.
    pub network_id: Option<u16>,
    /// Exact set of PAT program numbers / SDT service ids.
    pub service_ids: Option<Vec<u16>>,
    /// Frequency in MHz that NIT terrestrial_delivery_system_descriptor
    /// must list (tolerance ±1/14 MHz, half a 1/7 MHz step).
    pub frequency_mhz: Option<f64>,
    /// guard_interval code (B10 Table 6-62).
    pub guard_interval: Option<u8>,
    /// transmission_mode code (B10 Table 6-63).
    pub transmission_mode: Option<u8>,
    /// area_code.
    pub area_code: Option<u16>,
    /// remote_control_key_id (TS_information_descriptor).
    pub remote_control_key_id: Option<u8>,
    /// ts_name (TS_information_descriptor).
    pub ts_name: Option<String>,
    /// network_name_descriptor of NIT actual.
    pub network_name: Option<String>,
}

/// Options.
#[derive(Debug, Clone, Default)]
pub struct ValidateOptions {
    /// Expectations.
    pub expect: Expectations,
    /// Nominal bit rate when there are no PCRs.
    pub nominal_bps: Option<f64>,
}

/// Validation report.
#[derive(Debug, Clone, Serialize)]
pub struct ValidationReport {
    /// Source.
    pub source: String,
    /// Capture duration (s).
    pub duration_s: Option<f64>,
    /// Findings, most severe first.
    pub findings: Vec<Finding>,
    /// B10 Table 5-2 measured vs criterion.
    pub frequencies: Vec<FrequencyRow>,
    /// Error findings.
    pub errors: usize,
    /// Warning findings.
    pub warnings: usize,
    /// Info findings.
    pub infos: usize,
}

impl ValidationReport {
    /// True if any error finding.
    pub fn has_errors(&self) -> bool {
        self.errors > 0
    }

    /// Text rendering, findings at or above `min`.
    pub fn to_text(&self, min: Severity) -> String {
        let mut o = String::new();
        let _ = writeln!(
            o,
            "== validate {} ({} s) ==",
            self.source,
            self.duration_s.map_or("?".into(), |d| format!("{d:.3}"))
        );
        for f in self.findings.iter().filter(|f| f.severity >= min) {
            let cnt = if f.count > 1 {
                format!(" (x{})", f.count)
            } else {
                String::new()
            };
            let _ = writeln!(
                o,
                "{:<7} [{}] {} — {}: {}{}",
                f.severity.to_string().to_uppercase(),
                f.check,
                f.clause,
                f.location,
                f.message,
                cnt
            );
        }
        let _ = writeln!(
            o,
            "\n-- B10 Part 2 Table 5-2 transmission frequency (operational criterion) --"
        );
        for r in &self.frequencies {
            let shown = match min {
                Severity::Info => true,
                Severity::Warning => r.status.starts_with("exceeded"),
                Severity::Error => false,
            };
            if !shown {
                continue;
            }
            let _ = writeln!(
                o,
                "{} {:<20} crit {:<38} x{:<4} max interval {} max gap incl. edges {} → {}",
                r.key,
                r.table,
                r.criterion,
                r.count,
                r.interval.map_or("-".into(), |i| format!("{:.3} s", i.max)),
                r.max_gap_s.map_or("-".into(), |g| format!("{g:.3} s")),
                r.status
            );
        }
        let _ = writeln!(
            o,
            "\n{} errors, {} warnings, {} info",
            self.errors, self.warnings, self.infos
        );
        o
    }
}

struct Out {
    v: Vec<Finding>,
}

impl Out {
    fn push(
        &mut self,
        severity: Severity,
        check: &'static str,
        clause: impl Into<String>,
        location: impl Into<String>,
        message: impl Into<String>,
    ) {
        self.add(severity, check, clause, location, message, 1);
    }
    fn add(
        &mut self,
        severity: Severity,
        check: &'static str,
        clause: impl Into<String>,
        location: impl Into<String>,
        message: impl Into<String>,
        count: u64,
    ) {
        let (clause, location, message) = (clause.into(), location.into(), message.into());
        if let Some(f) = self.v.iter_mut().find(|f| {
            f.check == check && f.clause == clause && f.location == location && f.message == message
        }) {
            f.count += count;
            return;
        }
        self.v.push(Finding {
            severity,
            check,
            clause,
            location,
            message,
            count,
        });
    }
}

fn pid_loc(pid: u16) -> String {
    format!("pid {pid:#06x}")
}

/// Severity of an H.222.0 violation: reserved bits (§2.1.64) are warnings.
fn h222_sev(clause: &str) -> Severity {
    if clause == "§2.1.64" {
        Severity::Warning
    } else {
        Severity::Error
    }
}

/// Run every check.
pub fn validate(cap: &Capture, opts: &ValidateOptions) -> ValidationReport {
    let mut o = Out { v: Vec::new() };
    let tb = inspect::timebase(cap, opts.nominal_bps);
    let duration = tb.duration(cap.packets);
    transport(cap, &tb, &mut o);
    let pmts = cap.pmts();
    psi_checks(cap, &pmts, duration, &mut o);
    pid_checks(cap, &pmts, &mut o);
    section_checks(cap, &pmts, &mut o);
    let si = si_checks(cap, &mut o);
    mandatory_tables(cap, &pmts, duration, &mut o);
    let freqs = frequency(cap, &tb, duration, &mut o);
    window_4kb(cap, &tb, &mut o);
    cross_checks(cap, &si, &mut o);
    pes_checks(cap, &pmts, &mut o);
    copy_control_checks(cap, &mut o);
    aac_checks(cap, &si, &pmts, &mut o);
    expectations(cap, &si, &opts.expect, &mut o);
    let mut findings = o.v;
    findings.sort_by(|a, b| {
        b.severity
            .cmp(&a.severity)
            .then(a.check.cmp(b.check))
            .then(a.location.cmp(&b.location))
    });
    let n = |s| findings.iter().filter(|f| f.severity == s).count();
    ValidationReport {
        source: cap.source.clone(),
        duration_s: duration,
        errors: n(Severity::Error),
        warnings: n(Severity::Warning),
        infos: n(Severity::Info),
        findings,
        frequencies: freqs,
    }
}

// ---------------------------------------------------------------------------
// transport
// ---------------------------------------------------------------------------

fn transport(cap: &Capture, tb: &Timebase, o: &mut Out) {
    use Severity::*;
    if cap.reader.sync_losses > 0 {
        o.add(
            Error,
            "transport.sync",
            "H.222.0 §2.4.3.3 sync_byte, Table 2-1",
            "stream",
            format!(
                "{} sync losses, {} bytes skipped",
                cap.reader.sync_losses, cap.reader.skipped_bytes
            ),
            cap.reader.sync_losses,
        );
    }
    if cap.reader.trailing_bytes > 0 {
        o.push(
            Info,
            "transport.sync",
            "H.222.0 §2.4.3",
            "stream",
            format!(
                "{} trailing bytes do not form a packet",
                cap.reader.trailing_bytes
            ),
        );
    }
    for (&pid, s) in &cap.stats.pids {
        if s.tei > 0 {
            o.add(Warning, "transport.tei", "H.222.0 §2.4.3.3 transport_error_indicator", pid_loc(pid), "packets flagged with an uncorrectable bit error (input damage, contents not checked)", s.tei);
        }
        if s.scrambled > 0 && s.tei < s.packets {
            let si = matches!(
                pid,
                0x0000 | 0x0001 | 0x0010 | 0x0011 | 0x0013 | 0x0014 | 0x0024 | 0x0025
            ) || cap.slots.keys().any(|k| k.pid == pid && k.table_id == 2);
            if si {
                o.add(
                    Error,
                    "transport.scrambled",
                    "B10 Part 2 §5.1 / §5.1.5",
                    pid_loc(pid),
                    "PSI/SI packets scrambled (only EIT schedule may be)",
                    s.scrambled,
                );
            } else {
                o.add(
                    Info,
                    "transport.scrambled",
                    "H.222.0 Table 2-4",
                    pid_loc(pid),
                    "scrambled packets",
                    s.scrambled,
                );
            }
        }
    }
    for t in cap.packet_violations.values() {
        let sev = h222_sev(&t.clause);
        o.add(
            sev,
            "transport.packet",
            format!("H.222.0 {}", t.clause),
            format!("first pid {:#06x} packet {}", t.pid, t.first_packet),
            t.example.clone(),
            t.count,
        );
    }
    // continuity (§2.4.3.3)
    let mut agg: BTreeMap<(u16, &'static str, Severity), u64> = BTreeMap::new();
    for e in &cap.stats.cc_events {
        let (k, s) = match e.kind {
            CcEventKind::Break { .. } => {
                ("continuity_counter break (packet loss or mux error)", Error)
            }
            CcEventKind::Discontinuity { .. } => {
                ("continuity jump signalled by discontinuity_indicator", Info)
            }
            CcEventKind::Duplicate { identical: true } => ("duplicate packet", Info),
            CcEventKind::Duplicate { identical: false } => (
                "duplicate packet whose bytes differ (other than PCR)",
                Error,
            ),
            CcEventKind::ExcessDuplicate => (
                "more than two consecutive packets with the same counter",
                Error,
            ),
            CcEventKind::NoPayloadChanged { .. } => {
                ("counter changed in a packet without payload", Error)
            }
            CcEventKind::TransportError => continue,
        };
        *agg.entry((e.pid, k, s)).or_default() += 1;
    }
    for ((pid, k, s), n) in agg {
        o.add(
            s,
            "transport.cc",
            "H.222.0 §2.4.3.3 continuity_counter",
            pid_loc(pid),
            k,
            n,
        );
    }
    // PCR (§2.7.2) on each program's PCR_PID
    let pmts = cap.pmts();
    let pcr_pids: BTreeSet<u16> = pmts
        .values()
        .map(|(_, p)| p.body.pcr_pid)
        .filter(|&p| p != 0x1FFF)
        .collect();
    for &pid in &pcr_pids {
        let n = cap.stats.pids.get(&pid).map_or(0, |s| s.pcrs.len());
        if n == 0 {
            o.push(
                Error,
                "pcr.missing",
                "H.222.0 §2.4.4.9 PCR_PID, §2.7.2",
                pid_loc(pid),
                "PCR_PID of a program carries no PCR",
            );
            continue;
        }
        let ptb = Timebase::from_stats(&cap.stats, Some(pid), None);
        let s = ptb.samples();
        let d: Vec<f64> = s.windows(2).map(|w| w[1].1 - w[0].1).collect();
        let over = d.iter().filter(|&&x| x > 0.1 + 1e-9).count();
        let max = d.iter().copied().fold(0.0, f64::max);
        if over > 0 {
            o.add(
                Error,
                "pcr.interval",
                "H.222.0 §2.7.2",
                pid_loc(pid),
                format!(
                    "PCR interval exceeds 0.1 s ({over} of {} intervals; max {:.3} s)",
                    d.len(),
                    max
                ),
                over as u64,
            );
        } else if !d.is_empty() {
            o.push(
                Info,
                "pcr.interval",
                "H.222.0 §2.7.2",
                pid_loc(pid),
                format!("PCR interval max {max:.4} s ≤ 0.1 s ({} PCRs)", n),
            );
        }
        if n == 1 {
            o.push(
                Error,
                "pcr.interval",
                "H.222.0 §2.7.2",
                pid_loc(pid),
                "only one PCR (at least two required between discontinuities)",
            );
        }
        if ptb.discontinuities > 0 {
            o.add(
                Warning,
                "pcr.discontinuity",
                "H.222.0 §2.4.3.4/§2.4.3.5",
                pid_loc(pid),
                "PCR jump > 5 s or backwards",
                ptb.discontinuities,
            );
        }
        // Accuracy is only measurable when packet positions are CBR time.
        let nulls = cap.stats.pids.get(&0x1FFF).map_or(0, |s| s.packets);
        if s.len() >= 3 && nulls > 0 {
            let n = s.len() as f64;
            let mx = s.iter().map(|x| x.0 as f64).sum::<f64>() / n;
            let my = s.iter().map(|x| x.1).sum::<f64>() / n;
            let sxx: f64 = s.iter().map(|x| (x.0 as f64 - mx).powi(2)).sum();
            let b = if sxx > 0.0 {
                s.iter()
                    .map(|x| (x.0 as f64 - mx) * (x.1 - my))
                    .sum::<f64>()
                    / sxx
            } else {
                0.0
            };
            let worst = s
                .iter()
                .map(|x| ((x.1 - (my + b * (x.0 as f64 - mx))) * 1e9).abs())
                .fold(0.0, f64::max);
            let sev = if worst > 500.0 { Warning } else { Info };
            o.push(sev, "pcr.accuracy", "H.222.0 §2.4.2.2 (PCR tolerance ±500 ns)", pid_loc(pid), format!("max deviation from a constant-rate fit {worst:.0} ns (estimate assuming CBR packet timing)"));
        }
    }
    for (&pid, s) in &cap.stats.pids {
        if !s.pcrs.is_empty() && !pcr_pids.contains(&pid) && s.tei < s.packets {
            o.add(
                Info,
                "pcr.unreferenced",
                "H.222.0 §2.4.4.9",
                pid_loc(pid),
                "PCRs on a PID that is no program's PCR_PID",
                s.pcrs.len() as u64,
            );
        }
    }
    let _ = tb;
}

// ---------------------------------------------------------------------------
// PSI
// ---------------------------------------------------------------------------

fn psi_checks(
    cap: &Capture,
    pmts: &BTreeMap<u16, (u16, Long<Pmt>)>,
    duration: Option<f64>,
    o: &mut Out,
) {
    use Severity::*;
    // H.222.0 check_* on every distinct body.
    for (k, s) in &cap.slots {
        for v in &s.versions {
            for b in &v.bodies {
                let viol = match k.table_id {
                    0x00 => Long::<psi::Pat>::decode(&b.bytes).map(|t| psi::check_pat(&t)),
                    0x01 => Long::<psi::Cat>::decode(&b.bytes).map(|t| psi::check_cat(&t)),
                    0x02 => Long::<Pmt>::decode(&b.bytes).map(|t| psi::check_pmt(&t)),
                    _ => continue,
                };
                match viol {
                    Ok(vs) => {
                        for x in vs {
                            o.push(
                                h222_sev(x.clause),
                                "psi.syntax",
                                format!("H.222.0 {}", x.clause),
                                format!("{k} v{}", v.version),
                                x.message,
                            );
                        }
                    }
                    Err(e) => o.push(
                        Error,
                        "psi.decode",
                        "H.222.0 §2.4.4",
                        format!("{k} v{}", v.version),
                        e.to_string(),
                    ),
                }
            }
        }
    }
    let Some(pat) = cap.pat() else {
        o.push(Error, "psi.pat", "H.222.0 §2.4.4 (\"Each Transport Stream shall contain one or more Transport Stream packets with PID value 0x0000\")", "pid 0x0000", "no PAT");
        return;
    };
    let tsid = pat.header.table_id_extension;
    for (prog, pid) in pat.body.programs() {
        match pmts.get(&prog) {
            None => o.push(
                // B10 Table 5-2: PMT ≥ once per 100 ms is the criterion,
                // but data-service PMTs are seen at longer intervals: in a
                // capture under 2 s, a missing one is not yet an error.
                if duration.is_some_and(|d| d < 2.0) { Severity::Info } else { Error },
                "psi.pmt",
                "H.222.0 §2.4.4 (\"Each program listed in the Program Association Table shall be described in a unique TS_program_map_section\")",
                format!("program {prog:#06x}"),
                format!("no PMT for program listed in PAT (tsid {tsid:#06x}) on PID {pid:#06x}"),
            ),
            Some((ppid, _)) if *ppid != pid => o.push(
                Error,
                "psi.pmt",
                "H.222.0 §2.4.4 (program_map_PID designated by the PAT)",
                format!("program {prog:#06x}"),
                format!("PMT carried on PID {ppid:#06x}, PAT designates {pid:#06x}"),
            ),
            _ => {}
        }
    }
    let progs: BTreeSet<u16> = pat.body.programs().map(|p| p.0).collect();
    for (prog, (ppid, _)) in pmts {
        if !progs.contains(prog) {
            o.push(
                Info,
                "psi.pmt",
                "H.222.0 §2.4.4",
                format!("program {prog:#06x}"),
                format!("PMT on PID {ppid:#06x} for a program not in the PAT"),
            );
        }
    }
    if let Some(np) = pat.body.network_pid()
        && np != 0x0010
    {
        o.push(
            Error,
            "psi.pat",
            "B10 Part 1 §5.1 Table 5-1 (NIT 0x0010)",
            "pid 0x0000",
            format!("network_PID {np:#06x}, NIT is allocated PID 0x0010"),
        );
    }
    // scrambling ⇒ CAT (H.222.0 §2.4.4)
    let scrambled = cap.stats.pids.iter().any(|(_, s)| s.scrambled > s.tei);
    let has_cat = cap.slots.keys().any(|k| k.pid == 1 && k.table_id == 1);
    if scrambled && !has_cat {
        let sev = absent_severity(duration, 0x01);
        let msg = if sev == Severity::Info {
            "scrambled packets, no CAT seen; the capture is shorter than its repetition interval (Once or more/1 sec.)"
        } else {
            "scrambled packets but no CAT"
        };
        o.push(sev, "psi.cat", "H.222.0 §2.4.4 (\"Whenever one or more elementary streams … are scrambled … PID value 0x0001 shall be transmitted\")", "pid 0x0001", msg);
    }
}

// ---------------------------------------------------------------------------
// PID allocation
// ---------------------------------------------------------------------------

fn referenced_pids(cap: &Capture, pmts: &BTreeMap<u16, (u16, Long<Pmt>)>) -> BTreeMap<u16, String> {
    let mut m = inspect::pid_kinds(cap);
    for (ppid, p) in pmts.values() {
        m.entry(*ppid).or_insert_with(|| "PMT".into());
        for s in &p.body.streams {
            m.entry(s.elementary_pid).or_insert_with(|| "ES".into());
        }
    }
    m
}

fn pid_checks(cap: &Capture, pmts: &BTreeMap<u16, (u16, Long<Pmt>)>, o: &mut Out) {
    use Severity::*;
    let referenced = referenced_pids(cap, pmts);
    for (&pid, s) in &cap.stats.pids {
        if s.tei == s.packets {
            continue; // only corrupt packets: the PID itself may be a bit error
        }
        if (0x0004..=0x000F).contains(&pid) {
            o.add(
                Error,
                "pid.allocation",
                "H.222.0 Table 2-3 (0x0004–0x000F reserved)",
                pid_loc(pid),
                "packets on a reserved PID",
                s.packets,
            );
        }
        if !referenced.contains_key(&pid) && names::pid_allocation(pid).is_none() {
            o.add(
                Info,
                "pid.unreferenced",
                "H.222.0 §2.4.4 (private data may exist without being listed)",
                pid_loc(pid),
                "PID not designated by PAT/PMT/CAT nor allocated by B10 Table 5-1",
                s.packets,
            );
        }
    }
    // PMT / ES PIDs colliding with allocated PIDs
    for (prog, (ppid, p)) in pmts {
        let chk = |o: &mut Out, pid: u16, what: &str| {
            if pid <= 0x000F {
                o.push(
                    Error,
                    "pid.allocation",
                    "H.222.0 Table 2-3 / §2.4.4.9",
                    format!("program {prog:#06x}"),
                    format!("{what} PID {pid:#06x} in 0x0000–0x000F"),
                );
            } else if let Some(n) = names::pid_allocation(pid).filter(|_| pid != 0x1FFF) {
                o.push(
                    Error,
                    "pid.allocation",
                    "B10 Part 1 §5.1 Table 5-1",
                    format!("program {prog:#06x}"),
                    format!("{what} PID {pid:#06x} is allocated to {n}"),
                );
            } else if pid <= 0x002F {
                o.push(Info, "pid.allocation", "B10 Part 1 §5.1 Table 5-1", format!("program {prog:#06x}"), format!("{what} PID {pid:#06x} inside the SI PID range 0x0010–0x002F (not allocated by Table 5-1)"));
            }
        };
        chk(o, *ppid, "program_map");
        for s in &p.body.streams {
            chk(o, s.elementary_pid, "elementary");
            if !cap.stats.pids.contains_key(&s.elementary_pid) {
                o.push(Warning, "pid.missing", "H.222.0 §2.4.4 (\"Every program shall be fully defined within the Transport Stream itself\")", format!("program {prog:#06x}"), format!("elementary PID {:#06x} (stream_type {:#04x}) listed in PMT has no packets", s.elementary_pid, s.stream_type));
            }
        }
        let mut seen = BTreeSet::new();
        for s in &p.body.streams {
            if !seen.insert(s.elementary_pid) {
                o.push(
                    Error,
                    "psi.pmt",
                    "H.222.0 §2.4.4.9",
                    format!("program {prog:#06x}"),
                    format!("elementary_PID {:#06x} listed twice", s.elementary_pid),
                );
            }
        }
    }
}

// ---------------------------------------------------------------------------
// sections: placement, CRC, assembly, version coherence
// ---------------------------------------------------------------------------

fn section_checks(cap: &Capture, pmts: &BTreeMap<u16, (u16, Long<Pmt>)>, o: &mut Out) {
    use Severity::*;
    let pmt_pids: BTreeSet<u16> = cap
        .pat()
        .map(|p| p.body.programs().map(|x| x.1).collect())
        .unwrap_or_default();
    let _ = pmts;
    for (k, slot) in &cap.slots {
        let tid = k.table_id;
        let loc = k.to_string();
        // table_id per PID
        if k.pid == 0x0000 && tid != 0x00 {
            o.push(Error, "section.placement", "H.222.0 §2.4.4 (\"Only sections with this value of table_id are permitted\" on PID 0x0000)", &loc, format!("table_id {tid:#04x} on the PAT PID"));
        } else if k.pid == 0x0001 && tid != 0x01 {
            o.push(
                Error,
                "section.placement",
                "H.222.0 §2.4.4 (only table_id 0x01 on PID 0x0001)",
                &loc,
                format!("table_id {tid:#04x} on the CAT PID"),
            );
        } else if tid == 0x72 {
            if matches!(k.pid, 0x0000 | 0x0001 | 0x0014) {
                o.push(
                    Error,
                    "section.placement",
                    "B10 Part 1 §5.1 Table 5-1 (ST: \"Exclude 0x0000, 0x0001, 0x0014\")",
                    &loc,
                    "stuffing table on an excluded PID",
                );
            }
        } else if let Some(p) = names::fixed_pids(tid) {
            if !p.contains(&k.pid) {
                o.push(
                    Error,
                    "section.placement",
                    "B10 Part 1 §5.1 Table 5-1 / Part 2 §5.1.3 Table 5-1",
                    &loc,
                    format!(
                        "{} (table_id {tid:#04x}) shall be on PID {:04x?}",
                        names::table_name(tid),
                        p
                    ),
                );
            }
        } else if tid == 0x02 {
            if !pmt_pids.contains(&k.pid) {
                o.push(
                    Info,
                    "section.placement",
                    "H.222.0 §2.4.4 (PMT sections may be carried on other PIDs)",
                    &loc,
                    "PMT section on a PID the PAT does not designate",
                );
            }
        } else if (0x0010..=0x002F).contains(&k.pid) && !(0x90..=0xBF).contains(&tid) {
            o.push(
                Warning,
                "section.placement",
                "B10 Part 1 §5.1 Table 5-1 / §5.2 Table 5-2",
                &loc,
                format!(
                    "table_id {tid:#04x} ({}) on SI PID {:#06x} allocated to {}",
                    names::table_name(tid),
                    k.pid,
                    names::pid_allocation(k.pid).unwrap_or("nothing")
                ),
            );
        }
        if tid == 0xFF {
            o.push(
                Error,
                "section.placement",
                "H.222.0 Table 2-31 (0xFF forbidden)",
                &loc,
                "table_id 0xFF",
            );
        }
        // version coherence
        for v in &slot.versions {
            if v.bodies.len() > 1 {
                o.push(
                    Error,
                    "section.version",
                    "H.222.0 §2.4.4.5/§2.4.4.9 version_number; B10 Part 2 §5.1.1 d)",
                    format!("{loc} v{}", v.version),
                    format!("{} different contents under one version_number (content changed without a version change)", v.bodies.len()),
                );
            }
            if v.lsns.len() > 1 {
                o.push(
                    Error,
                    "section.version",
                    "B10 Part 2 §5.2 last_section_number",
                    format!("{loc} v{}", v.version),
                    format!(
                        "last_section_number differs between transmissions: {:?}",
                        v.lsns
                    ),
                );
            }
            if !v.cni {
                o.push(
                    Info,
                    "section.version",
                    "B10 Part 2 §5.1.1 e) current_next_indicator",
                    format!("{loc} v{}", v.version),
                    "next-version section transmitted",
                );
            }
        }
        if slot.versions.len() > 1 {
            let vs: Vec<u8> = slot.versions.iter().map(|v| v.version).collect();
            o.push(
                Info,
                "section.version",
                "B10 Part 2 §5.1.1 d)",
                &loc,
                format!("version changes during capture: {vs:?}"),
            );
        }
    }
    // sub_table consistency: lsn equal across sections, sections 0..=lsn present
    // (pid, tid, ext, version) → (section numbers, last_section_numbers)
    type Sub = (BTreeSet<u8>, BTreeSet<u8>);
    let mut subs: BTreeMap<(u16, u8, u16, u8), Sub> = BTreeMap::new();
    for (k, slot) in &cap.slots {
        let (Some(ext), Some(sn)) = (k.ext, k.sn) else {
            continue;
        };
        for v in slot.versions.iter().filter(|v| v.cni) {
            let e = subs.entry((k.pid, k.table_id, ext, v.version)).or_default();
            e.0.insert(sn);
            e.1.extend(v.lsns.iter().copied());
        }
    }
    for ((pid, tid, ext, ver), (sns, lsns)) in &subs {
        let loc = format!("pid={pid:#06x} tid={tid:#04x} ext={ext:#06x} v{ver}");
        if lsns.len() > 1 {
            o.push(Error, "section.subtable", "B10 Part 2 §5.2 last_section_number (\"the section with the highest section_number of the sub_table\")", &loc, format!("sections disagree on last_section_number: {lsns:?}"));
        }
        let lsn = *lsns.iter().max().unwrap_or(&0);
        if let Some(&mx) = sns.iter().max()
            && mx > lsn
        {
            o.push(
                Error,
                "section.subtable",
                "B10 Part 2 §5.2 (section_number ≤ last_section_number)",
                &loc,
                format!("section_number {mx} > last_section_number {lsn}"),
            );
        }
        if (0x50..=0x6F).contains(tid) {
            continue; // segmented: checked in EIT rules; often incomplete in short captures
        }
        let missing: Vec<u8> = (0..=lsn).filter(|s| !sns.contains(s)).collect();
        if !missing.is_empty() {
            o.push(
                Warning,
                "section.subtable",
                "B10 Part 2 §5.1.1 c) / H.222.0 §2.4.4 (sub_table sections 0..last_section_number)",
                &loc,
                format!("sections not seen in capture: {missing:?}"),
            );
        }
    }
    for e in &cap.section_errors {
        o.push(
            Error,
            "section.crc",
            "H.222.0 Annex A / §2.4.4 (CRC_32, section_length)",
            format!("pid {:#06x} tid {:#04x}", e.pid, e.table_id),
            e.message.clone(),
        );
    }
    if cap.section_error_count > cap.section_errors.len() as u64 {
        o.add(
            Error,
            "section.crc",
            "H.222.0 Annex A",
            "stream",
            "further section errors not listed",
            cap.section_error_count - cap.section_errors.len() as u64,
        );
    }
    for t in cap.demux_events.values() {
        let sev = if t.example.contains("ContinuityBreak") || t.example.contains("TransportError") {
            Warning
        } else {
            Error
        };
        o.add(
            sev,
            "section.assembly",
            format!("H.222.0 {}", t.clause),
            format!("pid {:#06x}", t.pid),
            t.example.clone(),
            t.count,
        );
    }
    for (&(pid, tid), s) in &cap.other_sections {
        if s.crc_errors > 0 {
            o.add(
                Error,
                "section.crc",
                "H.222.0 Annex A",
                format!("pid {pid:#06x} tid {tid:#04x}"),
                "CRC_32 mismatch",
                s.crc_errors,
            );
        }
    }
}

// ---------------------------------------------------------------------------
// SI value conformance (isdb-si)
// ---------------------------------------------------------------------------

/// Latest decoded SI tables by slot (for cross checks).
pub struct SiView {
    /// (slot, table) of every decodable latest section.
    pub tables: Vec<(SlotKey, Table)>,
}

impl SiView {
    fn iter_si(&self) -> impl Iterator<Item = (&SlotKey, &AnyTable)> {
        self.tables.iter().filter_map(|(k, t)| {
            if let Table::Si(a) = t {
                Some((k, a))
            } else {
                None
            }
        })
    }
}

fn si_checks(cap: &Capture, o: &mut Out) -> SiView {
    use Severity::*;
    let mut view = SiView { tables: Vec::new() };
    let mut seen_dev: BTreeSet<(String, String, String)> = BTreeSet::new();
    for (k, slot) in &cap.slots {
        let mut bodies: Vec<&[u8]> = slot
            .versions
            .iter()
            .flat_map(|v| v.bodies.iter().map(|b| b.bytes.as_slice()))
            .collect();
        if bodies.is_empty() {
            bodies.push(&slot.latest);
        }
        for b in bodies {
            if matches!(k.table_id, 0x00..=0x02) {
                continue;
            }
            match AnyTable::decode(k.pid, b) {
                Ok(t) => {
                    for d in conformance::check(k.pid, &t) {
                        if d.kind == Kind::Placement {
                            continue; // covered by section.placement
                        }
                        if !seen_dev.insert((
                            d.clause.to_string(),
                            d.location.clone(),
                            d.message.clone(),
                        )) {
                            continue;
                        }
                        let sev = match d.kind {
                            Kind::ReservedBits | Kind::ReservedValue => Warning,
                            Kind::MandatoryDescriptor => {
                                // Table 6-1 *2: "Can be substituted with the descriptor defined by service provider"
                                if d.message.contains("service_list")
                                    || d.message.contains("bouquet_name")
                                {
                                    Error
                                } else {
                                    Warning
                                }
                            }
                            _ => Error,
                        };
                        // Group per table: the location's first component.
                        let loc = d
                            .location
                            .split(" / ")
                            .next()
                            .unwrap_or(&d.location)
                            .to_string();
                        o.push(sev, "si.conformance", d.clause, loc, d.message);
                    }
                }
                Err(e) => o.push(
                    Error,
                    "si.decode",
                    "B10 Part 2 §5.2 / §6.2 (syntax)",
                    k.to_string(),
                    e.to_string(),
                ),
            }
        }
        if let Ok(t) = decode::decode(k.pid, &slot.latest) {
            view.tables.push((*k, t));
        }
    }
    view
}

// ---------------------------------------------------------------------------
// mandatory tables and transmission frequency (B10 Part 2 Table 5-2)
// ---------------------------------------------------------------------------

/// A table absent from a capture shorter than its Table 5-2 repetition
/// interval may simply not have come round: Info, not Error.
fn absent_severity(duration: Option<f64>, tid: u8) -> Severity {
    let crit = names::criterion(tid).and_then(|c| c.max_interval_s);
    match (duration, crit) {
        (Some(d), Some(c)) if d < c => Severity::Info,
        _ => Severity::Error,
    }
}

fn mandatory_tables(
    cap: &Capture,
    pmts: &BTreeMap<u16, (u16, Long<Pmt>)>,
    duration: Option<f64>,
    o: &mut Out,
) {
    use Severity::*;
    let has = |tid: u8| cap.slots.keys().any(|k| k.table_id == tid);
    let c = "B10 Part 2 §5.1.3 Table 5-2 (transmission level Mandatory)";
    for (tid, name) in [
        (0x00, "PAT"),
        (0x01, "CAT"),
        (0x40, "NIT (actual network)"),
        (0x42, "SDT (actual stream)"),
        (0x4E, "EIT p/f (actual stream)"),
    ] {
        if !has(tid) {
            let sev = absent_severity(duration, tid);
            o.push(
                sev,
                "tables.mandatory",
                c,
                format!("table_id {tid:#04x}"),
                if sev == Info {
                    format!(
                        "mandatory {name} not seen; the capture is shorter than its repetition interval ({})",
                        names::criterion(tid).map_or("", |c| c.text)
                    )
                } else {
                    format!("mandatory {name} not transmitted")
                },
            );
        }
    }
    if pmts.is_empty()
        && cap
            .pat()
            .is_some_and(|p| p.body.programs().next().is_some())
    {
        o.push(
            Error,
            "tables.mandatory",
            c,
            "table_id 0x02",
            "mandatory PMT not transmitted",
        );
    }
    if !has(0x70) && !has(0x73) {
        o.push(
            absent_severity(duration, 0x73),
            "tables.mandatory",
            "B10 Part 2 Table 5-2 *2 (\"Transmitting either TDT or TOT is mandatory\")",
            "table_id 0x70/0x73",
            "neither TDT nor TOT transmitted",
        );
    }
}

fn frequency(
    cap: &Capture,
    tb: &Timebase,
    duration: Option<f64>,
    o: &mut Out,
) -> Vec<FrequencyRow> {
    use Severity::*;
    let mut rows = Vec::new();
    for (k, slot) in &cap.slots {
        let Some(c) = names::criterion(k.table_id) else {
            continue;
        };
        let Some(crit) = c.max_interval_s else {
            continue;
        };
        if k.table_id == 0x02
            && !cap
                .pat()
                .is_some_and(|p| p.body.programs().any(|x| Some(x.0) == k.ext))
        {
            continue;
        }
        let (iv, edges) = inspect::occurrence_intervals(tb, &slot.occurrences, cap.packets);
        let max_gap = match (iv, edges) {
            (Some(i), Some((a, b))) => Some(i.max.max(a).max(b)),
            (None, Some((a, b))) => Some(a.max(b)),
            _ => None,
        };
        let status = match (duration, max_gap) {
            (_, Some(g)) if g > crit * 1.0001 && iv.is_some_and(|i| i.max > crit * 1.0001) => {
                "exceeded"
            }
            (Some(d), Some(g)) if g > crit * 1.0001 && d > crit => "exceeded (edge gap)",
            (Some(d), _) if d < crit => "insufficient capture",
            (None, _) => "no time base",
            _ => "ok",
        };
        if status.starts_with("exceeded") {
            o.push(
                Warning,
                "tables.frequency",
                "B10 Part 2 §5.1.3 Table 5-2 (operational criterion, \"not the standard value\")",
                k.to_string(),
                format!("{}: criterion {} — measured max interval {}, max gap incl. capture edges {:.3} s", names::table_name(k.table_id), c.text, iv.map_or("-".into(), |i| format!("{:.3} s", i.max)), max_gap.unwrap_or(0.0)),
            );
        }
        rows.push(FrequencyRow {
            key: *k,
            table: names::table_name(k.table_id),
            criterion: c.text,
            criterion_s: crit,
            count: slot.occurrences.len(),
            interval: iv,
            max_gap_s: max_gap,
            status,
        });
    }
    rows
}

/// B10 Part 2 §5.1.4: "TS packets of the same PID is transmitted within the
/// range of 4KB±100% in 32msec each" — checked as at most 8 KiB in any
/// 32 ms window on SI PIDs (interpretation in SPEC-NOTES.md).
fn window_4kb(cap: &Capture, tb: &Timebase, o: &mut Out) {
    if !tb.known() {
        return;
    }
    for (&pid, idx) in &cap.section_pid_packets {
        if !(0x0010..=0x002F).contains(&pid) {
            continue;
        }
        let t: Vec<f64> = idx.iter().filter_map(|&i| tb.time(i)).collect();
        let mut j = 0;
        let mut worst = 0usize;
        for i in 0..t.len() {
            while t[i] - t[j] >= 0.032 {
                j += 1;
            }
            worst = worst.max(i - j + 1);
        }
        let bytes = worst * 188;
        if bytes > 8192 {
            o.push(
                Severity::Warning,
                "si.rate",
                "B10 Part 2 §5.1.4 (\"4KB±100% in 32msec\" per PID)",
                pid_loc(pid),
                format!("{bytes} bytes of TS packets within 32 ms (> 8 KiB)"),
            );
        }
    }
}

// ---------------------------------------------------------------------------
// cross-table rules
// ---------------------------------------------------------------------------

fn cross_checks(cap: &Capture, si: &SiView, o: &mut Out) {
    use Severity::*;
    let pat = cap.pat();
    let tsid = pat.as_ref().map(|p| p.header.table_id_extension);
    let progs: BTreeSet<u16> = pat
        .as_ref()
        .map(|p| p.body.programs().map(|x| x.0).collect())
        .unwrap_or_default();
    let mut sdt_actual: BTreeMap<u16, (u16, &isdb_si::tables::SdtService)> = BTreeMap::new();
    let mut onids: BTreeSet<(String, u16)> = BTreeSet::new();
    let mut nit_actual = Vec::new();
    let mut eit_pf: BTreeMap<u16, BTreeSet<u8>> = BTreeMap::new();
    let mut eit_sched: BTreeSet<u16> = BTreeSet::new();
    for (k, t) in si.iter_si() {
        match t {
            AnyTable::Sdt(s) if s.header.table_id == 0x42 => {
                if let Some(ts) = tsid
                    && s.header.table_id_extension != ts
                {
                    o.push(
                        Error,
                        "cross.tsid",
                        "B10 Part 2 §5.2.6 (SDT actual describes the actual TS) / H.222.0 §2.4.4.5",
                        k.to_string(),
                        format!(
                            "SDT actual transport_stream_id {:#06x} ≠ PAT {ts:#06x}",
                            s.header.table_id_extension
                        ),
                    );
                }
                onids.insert(("SDT actual".into(), s.body.original_network_id));
                for sv in &s.body.services {
                    sdt_actual.insert(sv.service_id, (s.body.original_network_id, sv));
                }
            }
            AnyTable::Nit(n) if n.header.table_id == 0x40 => nit_actual.push(n),
            AnyTable::Eit(e) => {
                let tid = e.header.table_id;
                if tid == 0x4E || (0x50..=0x5F).contains(&tid) {
                    if let Some(ts) = tsid
                        && e.body.transport_stream_id != ts
                    {
                        o.push(
                            Error,
                            "cross.tsid",
                            "B10 Part 2 §5.2.7 (EIT actual stream)",
                            k.to_string(),
                            format!(
                                "EIT actual transport_stream_id {:#06x} ≠ PAT {ts:#06x}",
                                e.body.transport_stream_id
                            ),
                        );
                    }
                    onids.insert(("EIT actual".into(), e.body.original_network_id));
                }
                if tid == 0x4E {
                    eit_pf
                        .entry(e.header.table_id_extension)
                        .or_default()
                        .insert(e.header.section_number);
                    eit_rules_pf(k, e, o);
                }
                if (0x50..=0x5F).contains(&tid) {
                    eit_sched.insert(e.header.table_id_extension);
                }
                if (0x50..=0x6F).contains(&tid) {
                    eit_rules_schedule(k, e, o);
                }
                chronological(k, e, o);
            }
            AnyTable::Bit(b) => {
                onids.insert(("BIT".into(), b.header.table_id_extension));
            }
            _ => {}
        }
    }
    // NIT actual
    for n in &nit_actual {
        let nid = n.header.table_id_extension;
        let own = tsid.and_then(|ts| {
            n.body
                .0
                .transport_streams
                .iter()
                .find(|t| t.transport_stream_id == ts)
        });
        for t in &n.body.0.transport_streams {
            onids.insert((
                format!("NIT TS {:#06x}", t.transport_stream_id),
                t.original_network_id,
            ));
        }
        match own {
            None => o.push(
                Warning,
                "cross.nit",
                "B10 Appendix §1.1 c) (guideline: NIT actual \"shall l[i]st all TSs of the actual delivery system\")",
                format!("NIT network_id {nid:#06x}"),
                format!("NIT actual does not list the actual TS {}", tsid.map_or("?".into(), |t| format!("{t:#06x}"))),
            ),
            Some(t) => {
                if t.original_network_id != nid {
                    o.push(Warning, "cross.nit", "B10 Part 2 §5.2.4 (network_id and original_network_id shall take the same value on the originating network)", format!("NIT network_id {nid:#06x}"), format!("own TS original_network_id {:#06x} ≠ network_id", t.original_network_id));
                }
                let has_delivery = t.descriptors.iter().any(|d| matches!(d, Descriptor::TerrestrialDeliverySystem(_)));
                if !has_delivery {
                    o.push(Warning, "cross.nit", "B10 Part 2 Table 6-1 (terrestrial_delivery_system_descriptor: \"Mandatory for digital terrestrial broadcasting\")", format!("NIT TS {:#06x}", t.transport_stream_id), "no terrestrial_delivery_system_descriptor for the actual TS");
                }
                for d in &t.descriptors {
                    match d {
                        Descriptor::ServiceList(sl) => {
                            let listed: BTreeSet<u16> = sl.services.iter().map(|s| s.service_id).collect();
                            let missing: Vec<u16> = progs.difference(&listed).copied().collect();
                            if !missing.is_empty() {
                                o.push(Warning, "cross.services", "B10 Part 2 §6.2.14 service_list_descriptor (lists services of the TS)", format!("NIT TS {:#06x}", t.transport_stream_id), format!("PAT programs not in NIT service_list: {missing:04x?}"));
                            }
                            let extra: Vec<u16> = listed.difference(&progs).copied().collect();
                            if !extra.is_empty() {
                                o.push(Info, "cross.services", "B10 Part 2 §6.2.14", format!("NIT TS {:#06x}", t.transport_stream_id), format!("service_list entries without a PAT program: {extra:04x?}"));
                            }
                        }
                        Descriptor::PartialReception(p) => {
                            let bad: Vec<u16> = p.service_ids.iter().copied().filter(|s| !progs.contains(s)).collect();
                            if !bad.is_empty() {
                                o.push(Warning, "cross.services", "B10 Part 2 §6.2.32 partial_reception_descriptor", format!("NIT TS {:#06x}", t.transport_stream_id), format!("partial reception service_ids not in PAT: {bad:04x?}"));
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
        if !n
            .body
            .0
            .descriptors
            .iter()
            .chain(
                n.body
                    .0
                    .transport_streams
                    .iter()
                    .flat_map(|t| t.descriptors.iter()),
            )
            .any(|d| matches!(d, Descriptor::SystemManagement(_)))
            && !cap
                .pmts()
                .values()
                .any(|(_, p)| p.body.program_info.iter().any(|d| d.tag == 0xFE))
        {
            o.push(Warning, "cross.nit", "B10 Part 2 Table 6-1 (system_management_descriptor: \"Mandatory either in PMT or NIT\")", format!("NIT network_id {nid:#06x}"), "system_management_descriptor in neither NIT nor PMT");
        }
    }
    // onid agreement
    let vals: BTreeSet<u16> = onids.iter().map(|x| x.1).collect();
    if vals.len() > 1 {
        let desc: Vec<String> = onids.iter().map(|(w, v)| format!("{w}={v:#06x}")).collect();
        o.push(Error, "cross.onid", "B10 Part 2 §5.2.4/§5.2.6/§5.2.7/§5.2.13 original_network_id (\"label identifying the network_id of the originating delivery system\")", "SI", format!("original_network_id differs: {}", desc.join(", ")));
    }
    // services: PAT vs SDT actual (Appendix §1.3)
    if !sdt_actual.is_empty() || !progs.is_empty() {
        let sids: BTreeSet<u16> = sdt_actual.keys().copied().collect();
        let missing: Vec<u16> = progs.difference(&sids).copied().collect();
        if !missing.is_empty() {
            o.push(Warning, "cross.services", "B10 Appendix §1.3 (guideline: \"the SI bit stream shall list in the SDT of a particular TS at least all the services of that TS\")", "SDT actual", format!("PAT programs absent from SDT actual: {missing:04x?}"));
        }
        let extra: Vec<u16> = sids.difference(&progs).copied().collect();
        if !extra.is_empty() {
            o.push(
                Info,
                "cross.services",
                "B10 Part 2 §5.2.6",
                "SDT actual",
                format!("SDT services without a PAT program: {extra:04x?}"),
            );
        }
    }
    // SDT EIT flags vs EIT presence (§5.2.6)
    for (sid, (_, sv)) in &sdt_actual {
        let pf = eit_pf.contains_key(sid);
        if sv.eit_present_following_flag && !pf {
            o.push(
                Warning,
                "cross.eit_flags",
                "B10 Part 2 §5.2.6 EIT_present_following_flag",
                format!("service {sid:#06x}"),
                "flag is 1 but no EIT p/f actual seen",
            );
        }
        if !sv.eit_present_following_flag && pf {
            o.push(
                Warning,
                "cross.eit_flags",
                "B10 Part 2 §5.2.6 EIT_present_following_flag (\"should not be present\")",
                format!("service {sid:#06x}"),
                "flag is 0 but EIT p/f actual present",
            );
        }
        let sc = eit_sched.contains(sid);
        if sv.eit_schedule_flag && !sc {
            o.push(
                Info,
                "cross.eit_flags",
                "B10 Part 2 §5.2.6 EIT_schedule_flag",
                format!("service {sid:#06x}"),
                "flag is 1 but no EIT schedule actual seen in capture",
            );
        }
        if !sv.eit_schedule_flag && sc {
            o.push(
                Warning,
                "cross.eit_flags",
                "B10 Part 2 §5.2.6 EIT_schedule_flag (\"should not be present\")",
                format!("service {sid:#06x}"),
                "flag is 0 but EIT schedule actual present",
            );
        }
    }
    // EIT p/f: sections 0 and 1 (Appendix §1.4.1)
    for (sid, sns) in &eit_pf {
        if !sns.contains(&0) || !sns.contains(&1) {
            o.push(Warning, "eit.pf", "B10 Appendix §1.4.1 (guideline: two sections per service, 0x00 present, 0x01 following)", format!("service {sid:#06x}"), format!("EIT p/f sections seen: {sns:?}"));
        }
    }
}

fn eit_rules_pf(k: &SlotKey, e: &isdb_si::tables::Eit, o: &mut Out) {
    let sn = e.header.section_number;
    if sn <= 1 && e.body.events.len() > 1 {
        o.push(
            Severity::Error,
            "eit.pf",
            "B10 Part 2 §5.2.7 (\"shall contain only information pertaining to the present event and the chronologically following event\"), Appendix §1.4.1",
            k.to_string(),
            format!("{} events in section {sn}", e.body.events.len()),
        );
    }
}

fn eit_rules_schedule(k: &SlotKey, e: &isdb_si::tables::Eit, o: &mut Out) {
    use Severity::Warning;
    let sn = e.header.section_number;
    let s0 = sn / 8 * 8;
    let slsn = e.body.segment_last_section_number;
    if slsn < sn || slsn > s0 + 7 {
        o.push(
            Warning,
            "eit.schedule",
            "B10 Appendix §1.4.2.1 e)–g) (guideline: segment_last_section_number = s0 + n − 1 within the segment)",
            k.to_string(),
            format!("segment_last_section_number {slsn} for section {sn} (segment {s0}..{})", s0 + 7),
        );
    }
    if slsn > e.header.last_section_number {
        o.push(
            Severity::Error,
            "eit.schedule",
            "B10 Part 2 §5.2.7 segment_last_section_number",
            k.to_string(),
            format!(
                "segment_last_section_number {slsn} > last_section_number {}",
                e.header.last_section_number
            ),
        );
    }
    let tid = e.header.table_id;
    let lt = e.body.last_table_id;
    let same_range = if tid <= 0x5F {
        (0x50..=0x5F).contains(&lt)
    } else {
        (0x60..=0x6F).contains(&lt)
    };
    if lt < tid || !same_range {
        o.push(
            Severity::Error,
            "eit.schedule",
            "B10 Part 2 §5.2.7 last_table_id",
            k.to_string(),
            format!("last_table_id {lt:#04x} with table_id {tid:#04x}"),
        );
    }
}

fn chronological(k: &SlotKey, e: &isdb_si::tables::Eit, o: &mut Out) {
    let mut prev: Option<u64> = None;
    for ev in &e.body.events {
        if ev.start_time.is_undefined() {
            continue;
        }
        let Some(dt) = ev.start_time.to_jst() else {
            continue;
        };
        let key = ev.start_time.mjd as u64 * 86400
            + dt.hour as u64 * 3600
            + dt.minute as u64 * 60
            + dt.second as u64;
        if prev.is_some_and(|p| key < p) {
            o.push(
                Severity::Error,
                "eit.order",
                "B10 Part 2 §5.2.7 (\"The event information shall be chronologically ordered\")",
                k.to_string(),
                format!(
                    "event {:#06x} starts before the previous event",
                    ev.event_id
                ),
            );
        }
        prev = Some(key);
    }
}

// ---------------------------------------------------------------------------
// PES / timestamps
// ---------------------------------------------------------------------------

fn pes_checks(cap: &Capture, pmts: &BTreeMap<u16, (u16, Long<Pmt>)>, o: &mut Out) {
    use Severity::*;
    for (prog, (_, pmt)) in pmts {
        let ptb = Timebase::from_stats(&cap.stats, Some(pmt.body.pcr_pid), None);
        let have_pcr = ptb.pcr_pid == Some(pmt.body.pcr_pid);
        for s in &pmt.body.streams {
            let Some(t) = cap.pes.get(&s.elementary_pid) else {
                continue;
            };
            let loc = format!("program {prog:#06x} es {:#06x}", s.elementary_pid);
            let pk = cap
                .stats
                .pids
                .get(&s.elementary_pid)
                .map_or(0, |x| x.packets);
            for v in t.violations.values() {
                o.add(
                    h222_sev(&v.clause),
                    "pes.header",
                    format!("H.222.0 {}", v.clause),
                    &loc,
                    v.example.clone(),
                    v.count,
                );
            }
            if t.header_errors > 0 {
                o.add(
                    Error,
                    "pes.header",
                    "H.222.0 §2.4.3.6 Table 2-21 (PES_header_data_length vs optional fields)",
                    &loc,
                    format!(
                        "optional PES header does not decode: {}",
                        t.first_error.as_deref().unwrap_or("?")
                    ),
                    t.header_errors,
                );
            }
            if t.no_start_code > 0 {
                o.add(
                    Error,
                    "pes.header",
                    "H.222.0 §2.4.3.6 packet_start_code_prefix / §2.4.3.3 PUSI",
                    &loc,
                    "payload_unit_start_indicator packet without PES start code",
                    t.no_start_code,
                );
            }
            if t.pes_count == 0 {
                let scr = cap
                    .stats
                    .pids
                    .get(&s.elementary_pid)
                    .map_or(0, |x| x.scrambled);
                if pk > 0 && scr >= pk {
                    o.push(
                        Info,
                        "pes.scrambled",
                        "H.222.0 §2.4.3.3 (transport_scrambling_control)",
                        &loc,
                        format!("{pk} packets, all scrambled: PES not checked"),
                    );
                } else if pk > 0 {
                    o.push(
                        Warning,
                        "pes.missing",
                        "H.222.0 §2.4.3.3",
                        &loc,
                        format!("{pk} packets but no PES packet start"),
                    );
                }
                continue;
            }
            let av = names::stream_type_is_video(s.stream_type)
                || names::stream_type_is_audio(s.stream_type);
            // stream_id class (Table 2-22)
            for &sid in &t.stream_ids {
                let ok = if names::stream_type_is_video(s.stream_type) {
                    (0xE0..=0xEF).contains(&sid)
                } else if names::stream_type_is_audio(s.stream_type) {
                    (0xC0..=0xDF).contains(&sid)
                } else {
                    true
                };
                if !ok {
                    o.push(
                        Warning,
                        "pes.stream_id",
                        "H.222.0 Table 2-22 stream_id assignments",
                        &loc,
                        format!(
                            "stream_id {sid:#04x} for stream_type {:#04x}",
                            s.stream_type
                        ),
                    );
                }
            }
            if !av {
                continue;
            }
            if t.pts.is_empty() {
                o.push(
                    Error,
                    "pes.pts",
                    "H.222.0 §2.7.4 / §2.4.3.7 PTS_DTS_flags",
                    &loc,
                    format!(
                        "{} PES packets but no PTS ({}; {} undecodable, {} spanning packets)",
                        t.pes_count,
                        t.first_flags.as_deref().unwrap_or("no header decoded"),
                        t.header_errors,
                        t.header_truncated
                    ),
                );
                continue;
            }
            // §2.7.4: presentation order (sorted, wrap-unwrapped)
            let mut u: Vec<i64> = Vec::with_capacity(t.pts.len());
            let mut acc = 0i64;
            for (i, &(_, p)) in t.pts.iter().enumerate() {
                if i > 0 {
                    acc += ts90k_signed_diff(p, t.pts[i - 1].1);
                }
                u.push(acc);
            }
            u.sort_unstable();
            let gaps: Vec<i64> = u.windows(2).map(|w| w[1] - w[0]).collect();
            let over = gaps.iter().filter(|&&g| g > 63_000).count();
            if over > 0 {
                let mx = *gaps.iter().max().unwrap();
                o.add(
                    Error,
                    "pes.pts",
                    "H.222.0 §2.7.4 (PTS at most 0.7 s apart)",
                    &loc,
                    format!("PTS gaps over 0.7 s (max {:.3} s)", mx as f64 / 90_000.0),
                    over as u64,
                );
            }
            // decode order: DTS (or PTS when no DTS) increasing in stream order
            let dec: Vec<(u64, u64)> = decode_times(t);
            let back = dec
                .windows(2)
                .filter(|w| ts90k_signed_diff(w[1].1, w[0].1) <= 0)
                .count();
            if back > 0 {
                o.add(
                    Warning,
                    "pes.order",
                    "H.222.0 §2.4.2 (T-STD: access units decoded in stream order)",
                    &loc,
                    "decoding time (DTS, else PTS) not increasing in stream order",
                    back as u64,
                );
            }
            // PTS ≥ DTS
            let mut lt = 0;
            for &(i, d) in &t.dts {
                if let Some(&(_, p)) = t.pts.iter().find(|x| x.0 == i)
                    && ts90k_signed_diff(p, d) < 0
                {
                    lt += 1;
                }
            }
            if lt > 0 {
                o.add(
                    Error,
                    "pes.order",
                    "H.222.0 §2.4.3.7 (PTS/DTS: decoding precedes presentation)",
                    &loc,
                    "PTS earlier than DTS",
                    lt,
                );
            }
            // T-STD delay: decode time − arrival ∈ [0, 1 s] (§2.4.2.6)
            if have_pcr {
                // §2.4.2.6: ≤ 1 s, except ISO/IEC 14496 streams (≤ 10 s).
                let limit = if matches!(s.stream_type, 0x10..=0x13 | 0x1B) {
                    10.0
                } else {
                    1.0
                };
                let (mut late, mut early, mut worst_late, mut worst_early) =
                    (0u64, 0u64, 0f64, 0f64);
                for &(idx, d) in &dec {
                    let Some(pcr) = ptb.pcr_at(idx) else { continue };
                    let delta =
                        pcr_signed_diff(d * 300 % isdb_ts::pcr::PCR_MODULUS, pcr) as f64 / 27e6;
                    if delta < 0.0 {
                        late += 1;
                        worst_late = worst_late.min(delta);
                    } else if delta > limit {
                        early += 1;
                        worst_early = worst_early.max(delta);
                    }
                }
                if late > 0 {
                    o.add(
                        Warning,
                        "pes.delay",
                        "H.222.0 §2.4.2.6 (estimate from interpolated PCR)",
                        &loc,
                        format!(
                            "PES header arrives after its decoding time (worst {worst_late:.3} s)"
                        ),
                        late,
                    );
                }
                if early > 0 {
                    o.add(Warning, "pes.delay", "H.222.0 §2.4.2.6 (\"delay … shall be less than or equal to one second\", 10 s for ISO/IEC 14496 streams; estimate)", &loc, format!("decoding time more than {limit} s after arrival (worst {worst_early:.3} s)"), early);
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// digital copy control user_defined (receiver compatibility)
// ---------------------------------------------------------------------------

/// Warn on a digital_copy_control_descriptor (0xC1) whose user_defined bits
/// carry copy_control_type '00' in the B10 Annex F (informative) Table F-1
/// layout (copy_control_type (2) + APS_control_data (2)); the ARIB
/// operational guidelines (TR-B14/TR-B15, not available locally) are
/// understood to make that layout normative.  Receiver evidence (observed
/// on one Sony BDZ-series recorder): a service whose 0xC1 has user_defined
/// 0b0000 is refused ("Unsupported signal") and plays with 0b0100; live
/// broadcasters send 0b0100.
fn copy_control_checks(cap: &Capture, o: &mut Out) {
    use isdb_si::descriptor::Descriptor;
    const CLAUSE: &str = "B10 §6.2.23 user_defined / Annex F Table F-2 (copy_control_type '00' \"Undefined\"; observed to be refused by a Sony BDZ-series recorder)";
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut check = |loc: String, d: &Descriptor, o: &mut Out| {
        let Descriptor::DigitalCopyControl(c) = d else {
            return;
        };
        let mut bad = Vec::new();
        if c.user_defined >> 2 == 0b00 {
            bad.push(format!("user_defined {:#06b}", c.user_defined));
        }
        for cc in c.component_control.iter().flatten() {
            if cc.user_defined >> 2 == 0b00 {
                bad.push(format!(
                    "component {:#04x} user_defined {:#06b}",
                    cc.component_tag, cc.user_defined
                ));
            }
        }
        if !bad.is_empty() && seen.insert(loc.clone()) {
            o.push(
                Severity::Warning,
                "si.copy_control",
                CLAUSE,
                loc,
                format!("digital_copy_control_descriptor with copy_control_type '00' ({}); live broadcasters send '01' (user_defined 0b0100)", bad.join(", ")),
            );
        }
    };
    for (k, slot) in &cap.slots {
        if k.table_id == 0x02 {
            if let Ok(t) = Long::<Pmt>::decode(&slot.latest) {
                for d in t
                    .body
                    .program_info
                    .iter()
                    .chain(t.body.streams.iter().flat_map(|s| s.descriptors.iter()))
                {
                    if d.tag == 0xC1
                        && let Ok(x) = Descriptor::decode_body(d.tag, &d.data)
                    {
                        check(k.to_string(), &x, o);
                    }
                }
            }
            continue;
        }
        if matches!(k.table_id, 0x00 | 0x01) {
            continue;
        }
        if let Ok(t) = AnyTable::decode(k.pid, &slot.latest) {
            for (_, d) in t.descriptors() {
                check(k.to_string(), d, o);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// MPEG-2 AAC ADTS (ARIB STD-B32 Part 2 §5.2, Attachment Ch. 3)
// ---------------------------------------------------------------------------

/// ADTS findings of every stream_type 0x0F PID (counted during the scan
/// by [`crate::aac::AdtsTrack`]) and the consistency of the audio component
/// descriptors' quality_indicator with the measured stream.
fn aac_checks(cap: &Capture, si: &SiView, pmts: &BTreeMap<u16, (u16, Long<Pmt>)>, o: &mut Out) {
    use Severity::*;
    use isdbt_mux::adts::b32::{self, QualityMode};
    for (&pid, t) in &cap.pes {
        let Some(a) = &t.aac else { continue };
        let loc = pid_loc(pid);
        if a.frames == 0 {
            if t.pes_count == 0 {
                continue; // no packets at all: reported by the PID checks
            }
            o.push(
                Warning,
                "aac.frames",
                "B32 Part 2 §4.1 (ADTS)",
                loc,
                format!(
                    "stream_type 0x0F but no complete ADTS frame in {} PES packets",
                    t.pes_count
                ),
            );
            continue;
        }
        for (rule, c) in &a.rules {
            let sev = match rule.severity() {
                b32::Severity::Error => Error,
                b32::Severity::Warning => Warning,
            };
            o.add(
                sev,
                crate::aac::check_name(*rule),
                rule.clause(),
                loc.clone(),
                format!(
                    "{}/{} frames, first #{}: {} (\"{}\")",
                    c.frames,
                    a.frames,
                    c.first_frame,
                    c.example,
                    rule.quote()
                ),
                c.frames,
            );
        }
        if a.resyncs > 0 {
            o.push(
                Warning,
                "aac.sync",
                "B32 Part 2 §4.1 (ADTS)",
                loc.clone(),
                format!(
                    "{} ADTS resynchronisations (frames lost or cut; see continuity findings)",
                    a.resyncs
                ),
            );
        }
        if a.parameter_changes > 0 {
            o.push(
                Info,
                "aac.parameters",
                "B32 Part 2 §5.2.3 (3)(a)",
                loc.clone(),
                format!(
                    "{} frames change sampling_frequency_index / channel_configuration",
                    a.parameter_changes
                ),
            );
        }
        let fs = a.sampling_frequency().unwrap_or(0);
        let ch = a.coded_channels.unwrap_or(0);
        let bps = a.bit_rate().unwrap_or(0.0);
        let h = a.first.expect("frames > 0");
        o.push(
            Info,
            "aac.summary",
            "B32 Part 2 §5.2, Attachment Ch. 3 Table 1",
            loc,
            format!(
                "{} frames: ID {}, profile {}, {} Hz{}, channel_configuration {} ({}), {:.1} kbit/s ({:.1} per stereo pair), crc_check in {}/{} frames (present, value not verified: header-level checks only), adts_buffer_fullness {}; Table 1 derives {}",
                a.frames,
                h.id,
                h.profile,
                fs,
                if b32::SBR_SFI.contains(&h.sampling_frequency_index) { format!(" (output {} Hz if SBR is used; not detectable from the header)", 2 * fs) } else { String::new() },
                h.channel_configuration,
                a.coded_channels.map_or("coded channels not determined from the header".to_string(), |n| format!("{n} coded channels")),
                bps / 1000.0,
                b32::per_stereo_pair(bps, ch) / 1000.0,
                a.crc_frames,
                a.frames,
                a.fullness.map_or("always 0x7FF".to_string(), |(lo, hi)| format!("{lo}..={hi}")),
                QualityMode::derive(fs, bps, ch).name()
            ),
        );
    }
    // component_tag → elementary PID per program (stream_identifier_descriptor)
    let mut tag_pid: BTreeMap<(u16, u8), u16> = BTreeMap::new();
    for (&prog, (_, pmt)) in pmts {
        for st in &pmt.body.streams {
            for d in &st.descriptors {
                if d.tag == 0x52 && !d.data.is_empty() {
                    tag_pid.insert((prog, d.data[0]), st.elementary_pid);
                }
            }
        }
    }
    let check = |sid: u16, where_: &str, ac: &isdb_si::descriptor::AudioComponent, o: &mut Out| {
        let Some(&pid) = tag_pid.get(&(sid, ac.component_tag)) else {
            return;
        };
        let Some(a) = cap.pes.get(&pid).and_then(|t| t.aac.as_ref()) else {
            return;
        };
        let (Some(mode), Some(bps), Some(fs)) = (
            QualityMode::from_code(ac.quality_indicator),
            a.bit_rate(),
            a.sampling_frequency(),
        ) else {
            return;
        };
        // implicit SBR (B32 Part 2 §5.2.3 (4)(a): permitted at 0x6–0x8)
        // doubles the output rate but is invisible in the header: a
        // contradiction that holds only at the core rate is not reported
        // as one
        let sbr_possible = a
            .first
            .is_some_and(|h| b32::SBR_SFI.contains(&h.sampling_frequency_index));
        let ch = a.coded_channels.unwrap_or(2);
        let loc = format!(
            "{where_} service {sid:#06x} component_tag {:#04x} → pid {pid:#06x}",
            ac.component_tag
        );
        const CLAUSE: &str =
            "B32 Part 2 Attachment Ch. 3 Table 1 (B10 Table 6-44 quality_indicator)";
        let at_core = mode.contradiction(fs, bps, ch);
        let with_sbr = if sbr_possible {
            mode.contradiction(2 * fs, bps, ch)
        } else {
            at_core.clone()
        };
        match (at_core, with_sbr) {
            (Some(why), Some(_)) => o.push(Warning, "aac.quality_indicator", CLAUSE, loc, format!("quality_indicator '{:02b}': {why}; \"The quality indicator shall be transmitted in accordance with the quality of an AAC coding stream\"", ac.quality_indicator)),
            (Some(why), None) => o.push(Info, "aac.quality_indicator", CLAUSE, loc, format!("quality_indicator '{:02b}' ({}): {why} — but consistent at {} Hz if the stream uses SBR (permitted at sampling_frequency_index 0x6–0x8, not detectable from the ADTS header)", ac.quality_indicator, mode.name(), 2 * fs)),
            (None, _) => o.push(Info, "aac.quality_indicator", CLAUSE, loc, format!("quality_indicator '{:02b}' ({}) consistent with {:.1} kbit/s per stereo pair at {fs} Hz (Table 1 derives {})", ac.quality_indicator, mode.name(), b32::per_stereo_pair(bps, ch) / 1000.0, QualityMode::derive(fs, bps, ch).name())),
        }
    };
    for (_, t) in si.iter_si() {
        if let AnyTable::Eit(e) = t {
            for ev in &e.body.events {
                for d in &ev.descriptors {
                    if let Descriptor::AudioComponent(ac) = d {
                        check(e.header.table_id_extension, "EIT", ac, o);
                    }
                }
            }
        }
    }
    for (&prog, (_, pmt)) in pmts {
        for d in pmt
            .body
            .program_info
            .iter()
            .chain(pmt.body.streams.iter().flat_map(|s| s.descriptors.iter()))
        {
            if d.tag == 0xC4
                && let Ok(Descriptor::AudioComponent(ac)) = Descriptor::decode_body(d.tag, &d.data)
            {
                check(prog, "PMT", &ac, o);
            }
        }
    }
}

fn decode_times(t: &crate::scan::PesTrack) -> Vec<(u64, u64)> {
    let mut v: Vec<(u64, u64)> = Vec::with_capacity(t.pts.len());
    let mut di = 0;
    for &(i, p) in &t.pts {
        while di < t.dts.len() && t.dts[di].0 < i {
            di += 1;
        }
        if di < t.dts.len() && t.dts[di].0 == i {
            v.push((i, t.dts[di].1));
        } else {
            v.push((i, p));
        }
    }
    v
}

// ---------------------------------------------------------------------------
// expectations
// ---------------------------------------------------------------------------

fn expectations(cap: &Capture, si: &SiView, e: &Expectations, o: &mut Out) {
    use Severity::Error;
    let c = "bench expectation";
    let mut bad = |what: &str, msg: String| o.push(Error, "expect", c, what, msg);
    let pat = cap.pat();
    if let Some(ts) = e.transport_stream_id {
        match &pat {
            Some(p) if p.header.table_id_extension == ts => {}
            Some(p) => bad(
                "PAT",
                format!(
                    "transport_stream_id {:#06x}, expected {ts:#06x}",
                    p.header.table_id_extension
                ),
            ),
            None => bad("PAT", "no PAT".into()),
        }
        for (k, t) in si.iter_si() {
            match t {
                AnyTable::Sdt(s)
                    if s.header.table_id == 0x42 && s.header.table_id_extension != ts =>
                {
                    bad(
                        &k.to_string(),
                        format!(
                            "SDT tsid {:#06x}, expected {ts:#06x}",
                            s.header.table_id_extension
                        ),
                    )
                }
                AnyTable::Eit(x)
                    if x.header.table_id == 0x4E && x.body.transport_stream_id != ts =>
                {
                    bad(
                        &k.to_string(),
                        format!(
                            "EIT tsid {:#06x}, expected {ts:#06x}",
                            x.body.transport_stream_id
                        ),
                    )
                }
                _ => {}
            }
        }
    }
    let nits: Vec<&isdb_si::tables::Nit> = si
        .iter_si()
        .filter_map(|(_, t)| {
            if let AnyTable::Nit(n) = t {
                (n.header.table_id == 0x40).then_some(n)
            } else {
                None
            }
        })
        .collect();
    if let Some(onid) = e.original_network_id {
        for (k, t) in si.iter_si() {
            let got = match t {
                AnyTable::Sdt(s) if s.header.table_id == 0x42 => Some(s.body.original_network_id),
                AnyTable::Eit(x) if x.header.table_id == 0x4E => Some(x.body.original_network_id),
                AnyTable::Bit(b) => Some(b.header.table_id_extension),
                _ => None,
            };
            if let Some(g) = got
                && g != onid
            {
                bad(
                    &k.to_string(),
                    format!("original_network_id {g:#06x}, expected {onid:#06x}"),
                );
            }
        }
        for n in &nits {
            for t in &n.body.0.transport_streams {
                if t.original_network_id != onid {
                    bad(
                        "NIT",
                        format!(
                            "TS {:#06x} original_network_id {:#06x}, expected {onid:#06x}",
                            t.transport_stream_id, t.original_network_id
                        ),
                    );
                }
            }
        }
    }
    if let Some(nid) = e.network_id {
        if nits.is_empty() {
            bad("NIT", "no NIT actual".into());
        }
        for n in &nits {
            if n.header.table_id_extension != nid {
                bad(
                    "NIT",
                    format!(
                        "network_id {:#06x}, expected {nid:#06x}",
                        n.header.table_id_extension
                    ),
                );
            }
        }
    }
    if let Some(want) = &e.service_ids {
        let want: BTreeSet<u16> = want.iter().copied().collect();
        let progs: BTreeSet<u16> = pat
            .as_ref()
            .map(|p| p.body.programs().map(|x| x.0).collect())
            .unwrap_or_default();
        if progs != want {
            bad(
                "PAT",
                format!("programs {progs:04x?}, expected {want:04x?}"),
            );
        }
        let sdt: BTreeSet<u16> = si
            .iter_si()
            .filter_map(|(_, t)| {
                if let AnyTable::Sdt(s) = t {
                    (s.header.table_id == 0x42).then_some(s)
                } else {
                    None
                }
            })
            .flat_map(|s| s.body.services.iter().map(|x| x.service_id))
            .collect();
        if sdt != want {
            bad("SDT", format!("services {sdt:04x?}, expected {want:04x?}"));
        }
    }
    let own_ts = |n: &&isdb_si::tables::Nit| -> Vec<isdb_si::tables::TransportStreamEntry> {
        let ts = pat.as_ref().map(|p| p.header.table_id_extension);
        n.body
            .0
            .transport_streams
            .iter()
            .filter(|t| ts.is_none_or(|x| x == t.transport_stream_id))
            .cloned()
            .collect()
    };
    let deliveries: Vec<isdb_si::descriptor::TerrestrialDeliverySystem> = nits
        .iter()
        .flat_map(own_ts)
        .flat_map(|t| t.descriptors.into_iter())
        .filter_map(|d| {
            if let Descriptor::TerrestrialDeliverySystem(x) = d {
                Some(x)
            } else {
                None
            }
        })
        .collect();
    let need_delivery = e.frequency_mhz.is_some()
        || e.guard_interval.is_some()
        || e.transmission_mode.is_some()
        || e.area_code.is_some();
    if need_delivery && deliveries.is_empty() {
        bad(
            "NIT",
            "no terrestrial_delivery_system_descriptor for the actual TS".into(),
        );
    }
    for d in &deliveries {
        if let Some(f) = e.frequency_mhz
            && !d
                .frequencies
                .iter()
                .any(|&x| (x as f64 / 7.0 - f).abs() <= 0.5 / 7.0)
        {
            bad(
                "NIT delivery",
                format!(
                    "frequencies {:?} MHz do not include {f} MHz",
                    d.frequencies
                        .iter()
                        .map(|&x| format!("{:.6}", x as f64 / 7.0))
                        .collect::<Vec<_>>()
                ),
            );
        }
        if let Some(g) = e.guard_interval
            && d.guard_interval != g
        {
            bad(
                "NIT delivery",
                format!(
                    "guard_interval {} ({}), expected {g} ({})",
                    d.guard_interval,
                    names::guard_interval_name(d.guard_interval),
                    names::guard_interval_name(g)
                ),
            );
        }
        if let Some(m) = e.transmission_mode
            && d.transmission_mode != m
        {
            bad(
                "NIT delivery",
                format!(
                    "transmission_mode {} ({}), expected {m}",
                    d.transmission_mode,
                    names::transmission_mode_name(d.transmission_mode)
                ),
            );
        }
        if let Some(a) = e.area_code
            && d.area_code != a
        {
            bad(
                "NIT delivery",
                format!("area_code {:#05x}, expected {a:#05x}", d.area_code),
            );
        }
    }
    let tsinfo: Vec<isdb_si::descriptor::TsInformation> = nits
        .iter()
        .flat_map(own_ts)
        .flat_map(|t| t.descriptors.into_iter())
        .filter_map(|d| {
            if let Descriptor::TsInformation(x) = d {
                Some(x)
            } else {
                None
            }
        })
        .collect();
    if (e.remote_control_key_id.is_some() || e.ts_name.is_some()) && tsinfo.is_empty() {
        bad(
            "NIT",
            "no TS_information_descriptor for the actual TS".into(),
        );
    }
    for t in &tsinfo {
        if let Some(r) = e.remote_control_key_id
            && t.remote_control_key_id != r
        {
            bad(
                "NIT TS info",
                format!(
                    "remote_control_key_id {}, expected {r}",
                    t.remote_control_key_id
                ),
            );
        }
        if let Some(n) = &e.ts_name {
            let got = t.ts_name.to_string_lossy();
            if &got != n {
                bad("NIT TS info", format!("ts_name {got:?}, expected {n:?}"));
            }
        }
    }
    if let Some(name) = &e.network_name {
        let got: Vec<String> = nits
            .iter()
            .flat_map(|n| n.body.0.descriptors.iter())
            .filter_map(|d| {
                if let Descriptor::NetworkName(x) = d {
                    Some(x.name.to_string_lossy())
                } else {
                    None
                }
            })
            .collect();
        if !got.iter().any(|g| g == name) {
            bad("NIT", format!("network_name {got:?}, expected {name:?}"));
        }
    }
}
