//! One pass over a transport stream collecting everything the reports
//! need: per-PID statistics and continuity (H.222.0 §2.4.3.3), PCRs
//! (§2.4.3.4), packet-level rule violations (`isdb_ts::packet::check`),
//! every PSI/SI section with its packet position (§2.4.4), and PES header
//! timestamps (§2.4.3.6/§2.4.3.7).
//!
//! Nothing here interprets packing: sections come from
//! `isdb_ts::demux::SectionAssembler`, PES headers from
//! `isdb_ts::pes::PesHeader::decode`.

use crate::names;
use isdb_ts::demux::{Assembled, DemuxEventKind, SectionDemux};
use isdb_ts::packet::{PACKET_SIZE, Packet, View};
use isdb_ts::psi::{Cat, Pat, Pmt};
use isdb_ts::reader::{PacketFormat, ReaderStats, SliceReader, StreamReader};
use isdb_ts::section::{Long, split_long_unchecked};
use isdb_ts::stats::Stats;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::Path;

/// Scan options.
#[derive(Debug, Clone, Default)]
pub struct ScanOptions {
    /// Stop after this many packets.
    pub max_packets: Option<u64>,
    /// Skip `isdb_ts::packet::check` on every packet (faster).
    pub skip_packet_checks: bool,
}

/// Identity of a repeatedly transmitted section: (PID, table_id,
/// table_id_extension, section_number); the last two are `None` for
/// short-form sections (B10 Part 2 §5.1.1 a–c).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize)]
pub struct SlotKey {
    /// PID.
    pub pid: u16,
    /// table_id.
    pub table_id: u8,
    /// table_id_extension (long form).
    pub ext: Option<u16>,
    /// section_number (long form).
    pub sn: Option<u8>,
}

impl std::fmt::Display for SlotKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "pid={:#06x} tid={:#04x}", self.pid, self.table_id)?;
        if let Some(e) = self.ext {
            write!(f, " ext={e:#06x}")?;
        }
        if let Some(s) = self.sn {
            write!(f, " sn={s}")?;
        }
        Ok(())
    }
}

/// One distinct section body seen under a version.
#[derive(Debug, Clone)]
pub struct BodyRec {
    /// Complete section bytes.
    pub bytes: Vec<u8>,
    /// Occurrences.
    pub count: u64,
    /// First packet index.
    pub first: u64,
    /// Last packet index.
    pub last: u64,
}

/// Sections of one (version_number, current_next_indicator) of a slot.
#[derive(Debug, Clone)]
pub struct VersionRec {
    /// version_number.
    pub version: u8,
    /// current_next_indicator.
    pub cni: bool,
    /// last_section_number values seen.
    pub lsns: BTreeSet<u8>,
    /// Distinct bodies (more than one ⇒ content changed without a version
    /// change).
    pub bodies: Vec<BodyRec>,
}

/// Everything seen for one [`SlotKey`].
#[derive(Debug, Clone, Default)]
pub struct Slot {
    /// Start packet index of every occurrence (stream order).
    pub occurrences: Vec<u64>,
    /// Long-form sections by version, in order of first appearance.
    pub versions: Vec<VersionRec>,
    /// Latest section bytes (any form).
    pub latest: Vec<u8>,
    /// Packet index of the latest occurrence.
    pub latest_packet: u64,
    /// Distinct consecutive bodies of a short-form slot (TDT/TOT change on
    /// every transmission).
    pub short_changes: u64,
}

/// A section that failed CRC or envelope decoding.
#[derive(Debug, Clone, serde::Serialize)]
pub struct SectionError {
    /// PID.
    pub pid: u16,
    /// Start packet.
    pub packet: u64,
    /// table_id byte.
    pub table_id: u8,
    /// Error text.
    pub message: String,
}

/// Count + first occurrence of a repeated finding.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct Tally {
    /// Occurrences.
    pub count: u64,
    /// First packet index.
    pub first_packet: u64,
    /// First PID.
    pub pid: u16,
    /// Example message.
    pub example: String,
    /// Clause.
    pub clause: String,
}

impl Tally {
    fn add(
        m: &mut BTreeMap<String, Tally>,
        key: String,
        clause: &str,
        pid: u16,
        packet: u64,
        example: impl FnOnce() -> String,
    ) {
        let t = m.entry(key).or_insert_with(|| Tally {
            count: 0,
            first_packet: packet,
            pid,
            example: example(),
            clause: clause.into(),
        });
        t.count += 1;
    }
}

/// Sections on PIDs that are not PSI/SI (ECM/EMM, DSM-CC carousels, …):
/// only counted.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct OtherSections {
    /// Sections.
    pub count: u64,
    /// Distinct (table_id_extension, version, section_number).
    pub distinct: u64,
    /// CRC failures.
    pub crc_errors: u64,
    #[serde(skip)]
    seen: BTreeSet<(u16, u8, u8)>,
}

/// PES headers seen on one elementary PID.
#[derive(Debug, Clone, Default)]
pub struct PesTrack {
    /// program_number(s) listing it.
    pub programs: BTreeSet<u16>,
    /// stream_type from the PMT.
    pub stream_type: u8,
    /// PES packets started (PUSI with start code).
    pub pes_count: u64,
    /// PUSI payloads not starting with packet_start_code_prefix.
    pub no_start_code: u64,
    /// Optional PES headers extending beyond the first packet (not decoded).
    pub header_truncated: u64,
    /// Optional PES headers that fit in the first packet but do not decode
    /// (syntax error), with the first error text.
    pub header_errors: u64,
    /// First header decode error.
    pub first_error: Option<String>,
    /// stream_id values.
    pub stream_ids: BTreeSet<u8>,
    /// (packet index, PTS).
    pub pts: Vec<(u64, u64)>,
    /// (packet index, DTS).
    pub dts: Vec<(u64, u64)>,
    /// PES header rule violations (`isdb_ts::pes::check`).
    pub violations: BTreeMap<String, Tally>,
    /// Optional-header flags of the first decoded PES header.
    pub first_flags: Option<String>,
    /// ADTS reassembly and ARIB STD-B32 Part 2 §5.2 counters
    /// (stream_type 0x0F only).
    pub aac: Option<crate::aac::AdtsTrack>,
}

/// Result of a scan.
#[derive(Debug, Clone, Default)]
pub struct Capture {
    /// Source name.
    pub source: String,
    /// Packet framing.
    pub format: Option<PacketFormat>,
    /// Reader counters.
    pub reader: ReaderStats,
    /// Packets read.
    pub packets: u64,
    /// Per-PID counters, CC events, PCRs.
    pub stats: Stats,
    /// Packet-level violations (H.222.0 `packet::check`), by clause+kind.
    pub packet_violations: BTreeMap<String, Tally>,
    /// PIDs whose sections are fully recorded (PSI/SI).
    pub section_pids: BTreeSet<u16>,
    /// Recorded sections.
    pub slots: BTreeMap<SlotKey, Slot>,
    /// Sections failing CRC / envelope (first 1000).
    pub section_errors: Vec<SectionError>,
    /// Total section errors.
    pub section_error_count: u64,
    /// Section assembler events, by kind.
    pub demux_events: BTreeMap<String, Tally>,
    /// Sections on non-SI PIDs.
    pub other_sections: BTreeMap<(u16, u8), OtherSections>,
    /// PES tracks by PID.
    pub pes: BTreeMap<u16, PesTrack>,
    /// Packet indices of every packet on section PIDs.
    pub section_pid_packets: BTreeMap<u16, Vec<u64>>,
    /// Packets whose payload was not parsed because it is scrambled
    /// (transport_scrambling_control ≠ '00').
    pub scrambled_payloads: u64,
}

/// Whether `pid` is in the PSI/SI range fully recorded by default
/// (H.222.0 Table 2-3 0x0000–0x000F, B10 Part 1 Table 5-1 0x0010–0x002F).
pub fn si_range(pid: u16) -> bool {
    pid <= 0x002F
}

struct Scanner {
    cap: Capture,
    opts: ScanOptions,
    demux: SectionDemux,
    /// PIDs with other (non-SI) sections.
    other_pids: BTreeSet<u16>,
    pmt_pids: BTreeSet<u16>,
    decoded: HashMap<Vec<u8>, ()>,
}

impl Scanner {
    fn new(opts: ScanOptions, source: String) -> Self {
        let mut s = Scanner {
            cap: Capture {
                source,
                ..Default::default()
            },
            opts,
            demux: SectionDemux::new(),
            other_pids: BTreeSet::new(),
            pmt_pids: BTreeSet::new(),
            decoded: HashMap::new(),
        };
        for pid in 0..=0x002F {
            s.add_si_pid(pid);
        }
        s
    }

    fn add_si_pid(&mut self, pid: u16) {
        self.demux.add_pid(pid);
        self.cap.section_pids.insert(pid);
    }

    fn add_other_pid(&mut self, pid: u16) {
        if pid != 0x1FFF && !self.cap.section_pids.contains(&pid) {
            self.demux.add_pid(pid);
            self.other_pids.insert(pid);
        }
    }

    /// Learn PIDs from PSI (PAT → PMT/NIT PIDs, PMT → ES/ECM PIDs, CAT →
    /// EMM PIDs).  Called for each distinct PSI body.
    fn learn(&mut self, pid: u16, sec: &[u8]) {
        match sec.first() {
            Some(0x00) if pid == 0 => {
                if let Ok(p) = Long::<Pat>::decode(sec) {
                    for e in &p.body.entries {
                        if e.pid != 0x1FFF {
                            self.pmt_pids.insert(e.pid);
                            self.add_si_pid(e.pid);
                        }
                    }
                }
            }
            Some(0x01) if pid == 1 => {
                if let Ok(c) = Long::<Cat>::decode(sec) {
                    for d in &c.body.descriptors {
                        if matches!(d.tag, 0x09 | 0xF6) && d.data.len() >= 4 {
                            self.add_other_pid(u16::from_be_bytes([d.data[2], d.data[3]]) & 0x1FFF);
                        }
                    }
                }
            }
            Some(0x02) => {
                if let Ok(p) = Long::<Pmt>::decode(sec) {
                    let prog = p.header.table_id_extension;
                    let ca = |ds: &[isdb_ts::descriptor::Descriptor]| -> Vec<u16> {
                        ds.iter()
                            .filter(|d| matches!(d.tag, 0x09 | 0xF6) && d.data.len() >= 4)
                            .map(|d| u16::from_be_bytes([d.data[2], d.data[3]]) & 0x1FFF)
                            .collect()
                    };
                    for c in ca(&p.body.program_info) {
                        self.add_other_pid(c);
                    }
                    for s in &p.body.streams {
                        for c in ca(&s.descriptors) {
                            self.add_other_pid(c);
                        }
                        if names::stream_type_is_sections(s.stream_type) {
                            self.add_other_pid(s.elementary_pid);
                        } else {
                            let t = self.cap.pes.entry(s.elementary_pid).or_default();
                            t.stream_type = s.stream_type;
                            t.programs.insert(prog);
                            if s.stream_type == 0x0F && t.aac.is_none() {
                                t.aac = Some(Default::default());
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }

    fn push(&mut self, index: u64, pkt: &[u8; PACKET_SIZE]) {
        self.cap.packets += 1;
        self.cap.stats.push(index, pkt);
        let pid = (u16::from(pkt[1] & 0x1F) << 8) | u16::from(pkt[2]);
        // Packets flagged by transport_error_indicator carry uncorrectable
        // bit errors (§2.4.3.3); their contents are not checked.
        if !self.opts.skip_packet_checks && pkt[1] & 0x80 == 0 {
            match Packet::decode(pkt) {
                Ok(p) => {
                    for v in isdb_ts::packet::check(&p) {
                        let kind = v
                            .message
                            .split(|c: char| c.is_ascii_digit())
                            .next()
                            .unwrap_or("")
                            .to_string();
                        Tally::add(
                            &mut self.cap.packet_violations,
                            format!("{} {kind}", v.clause),
                            v.clause,
                            pid,
                            index,
                            || v.message.clone(),
                        );
                    }
                }
                Err(e) => {
                    Tally::add(
                        &mut self.cap.packet_violations,
                        "decode".into(),
                        "§2.4.3.2 Table 2-2",
                        pid,
                        index,
                        || e.to_string(),
                    );
                }
            }
        }
        // transport_scrambling_control ≠ '00': the payload is encrypted
        // (H.222.0 §2.4.3.3, Table 2-4; the header and adaptation field
        // stay in the clear). Counted in `stats.scrambled`, not parsed as
        // sections, PES or ADTS: descrambling is out of scope.
        let scrambled = pkt[3] & 0xC0 != 0;
        if scrambled {
            self.cap.scrambled_payloads += 1;
        }
        if self.demux.has_pid(pid) && !scrambled {
            if self.cap.section_pids.contains(&pid) {
                self.cap
                    .section_pid_packets
                    .entry(pid)
                    .or_default()
                    .push(index);
            }
            for a in self.demux.push(index, pkt) {
                match a {
                    Assembled::Section(s) => self.section(pid, s.start_packet, s.data),
                    Assembled::Event(e) => {
                        let (key, clause) = match &e.kind {
                            DemuxEventKind::Discarded { reason, .. } => {
                                (format!("discarded partial section ({reason:?})"), "§2.4.4")
                            }
                            DemuxEventKind::PointerOutOfRange { .. } => {
                                ("pointer_field beyond payload".to_string(), "§2.4.4.2")
                            }
                            DemuxEventKind::UnexpectedBytes { .. } => (
                                "bytes after section end are not 0xFF stuffing".to_string(),
                                "§2.4.4",
                            ),
                            DemuxEventKind::BadStuffing => {
                                ("non-0xFF byte after stuffing started".to_string(), "§2.4.4")
                            }
                        };
                        Tally::add(
                            &mut self.cap.demux_events,
                            format!("{key} pid={pid:#06x}"),
                            clause,
                            pid,
                            e.packet_index,
                            || format!("{:?}", e.kind),
                        );
                    }
                }
            }
        }
        if let Some(t) = self.cap.pes.get_mut(&pid)
            && !scrambled
            && pkt[1] & 0xC0 == 0x40
            && let Ok(v) = View::parse(pkt)
            && let Some(pl) = v.payload
        {
            pes_header(t, index, pid, pl);
        }
        // ADTS payload bytes (stream_type 0x0F); packets flagged by
        // transport_error_indicator are not fed (the next frame resyncs)
        if let Some(a) = self.cap.pes.get_mut(&pid).and_then(|t| t.aac.as_mut())
            && !scrambled
            && pkt[1] & 0x80 == 0
            && let Ok(v) = View::parse(pkt)
            && let Some(pl) = v.payload
        {
            if pkt[1] & 0x40 != 0 {
                if pl.len() >= 9
                    && pl[..3] == isdb_ts::pes::START_CODE_PREFIX
                    && 9 + pl[8] as usize <= pl.len()
                {
                    a.push_start(&pl[9 + pl[8] as usize..]);
                }
            } else {
                a.push(pl);
            }
        }
    }

    fn section(&mut self, pid: u16, start: u64, data: Vec<u8>) {
        let tid = data[0];
        if !self.cap.section_pids.contains(&pid) {
            let o = self.cap.other_sections.entry((pid, tid)).or_default();
            o.count += 1;
            if data.len() > 3 && data[1] & 0x80 != 0 {
                match split_long_unchecked(&data) {
                    Ok((h, _, 0)) => {
                        if o.seen
                            .insert((h.table_id_extension, h.version_number, h.section_number))
                        {
                            o.distinct += 1;
                        }
                    }
                    _ => o.crc_errors += 1,
                }
            }
            return;
        }
        let err = |cap: &mut Capture, msg: String| {
            cap.section_error_count += 1;
            if cap.section_errors.len() < 1000 {
                cap.section_errors.push(SectionError {
                    pid,
                    packet: start,
                    table_id: tid,
                    message: msg,
                });
            }
        };
        // ST (B10 §5.2.11) has no fixed form: always short, no CRC.
        let long = tid != 0x72 && data[1] & 0x80 != 0;
        let key;
        let mut header = None;
        if long {
            match split_long_unchecked(&data) {
                Ok((h, _, residue)) => {
                    if residue != 0 {
                        err(
                            &mut self.cap,
                            format!("CRC_32 mismatch (residue {residue:#010x})"),
                        );
                        return;
                    }
                    key = SlotKey {
                        pid,
                        table_id: tid,
                        ext: Some(h.table_id_extension),
                        sn: Some(h.section_number),
                    };
                    header = Some(h);
                }
                Err(e) => {
                    err(&mut self.cap, e.to_string());
                    return;
                }
            }
        } else {
            if tid == 0x73 && isdb_ts::crc::crc32(&data) != 0 {
                err(&mut self.cap, "CRC_32 mismatch (TOT)".into());
                return;
            }
            key = SlotKey {
                pid,
                table_id: tid,
                ext: None,
                sn: None,
            };
        }
        let slot = self.cap.slots.entry(key).or_default();
        slot.occurrences.push(start);
        if let Some(h) = header {
            let vi =
                match slot.versions.iter().position(|v| {
                    v.version == h.version_number && v.cni == h.current_next_indicator
                }) {
                    Some(i) => i,
                    None => {
                        slot.versions.push(VersionRec {
                            version: h.version_number,
                            cni: h.current_next_indicator,
                            lsns: BTreeSet::new(),
                            bodies: Vec::new(),
                        });
                        slot.versions.len() - 1
                    }
                };
            let v = &mut slot.versions[vi];
            v.lsns.insert(h.last_section_number);
            match v.bodies.iter_mut().find(|b| b.bytes == data) {
                Some(b) => {
                    b.count += 1;
                    b.last = start;
                }
                None => v.bodies.push(BodyRec {
                    bytes: data.clone(),
                    count: 1,
                    first: start,
                    last: start,
                }),
            }
        } else if slot.latest != data {
            slot.short_changes += 1;
        }
        slot.latest_packet = start;
        if slot.latest != data {
            slot.latest = data.clone();
        }
        if matches!(tid, 0x00..=0x02) && !self.decoded.contains_key(&data) {
            self.decoded.insert(data.clone(), ());
            self.learn(pid, &data);
        }
    }
}

fn pes_header(t: &mut PesTrack, index: u64, pid: u16, pl: &[u8]) {
    if pl.len() < 6 || pl[..3] != isdb_ts::pes::START_CODE_PREFIX {
        t.no_start_code += 1;
        return;
    }
    t.pes_count += 1;
    let sid = pl[3];
    t.stream_ids.insert(sid);
    if !isdb_ts::pes::has_optional_header(sid) {
        return;
    }
    match isdb_ts::pes::PesHeader::decode(&pl[6..]) {
        Ok((h, _)) => {
            if t.first_flags.is_none() {
                t.first_flags = Some(format!(
                    "flags byte {:#04x}: PTS_DTS_flags {}, ESCR {}, ES_rate {}, DSM_trick_mode {}, additional_copy_info {}, PES_CRC {}, PES_extension {}",
                    pl.get(7).copied().unwrap_or(0),
                    match h.pts_dts {
                        isdb_ts::pes::PtsDts::None => "'00'",
                        isdb_ts::pes::PtsDts::Forbidden => "'01'",
                        isdb_ts::pes::PtsDts::Pts(_) => "'10'",
                        isdb_ts::pes::PtsDts::PtsDts(..) => "'11'",
                    },
                    h.escr.is_some(),
                    h.es_rate.is_some(),
                    h.trick_mode.is_some(),
                    h.additional_copy_info.is_some(),
                    h.previous_pes_packet_crc.is_some(),
                    h.extension.is_some()
                ));
            }
            if let Some(p) = h.pts_dts.pts() {
                t.pts.push((index, p));
            }
            if let Some(d) = h.pts_dts.dts() {
                t.dts.push((index, d));
            }
            let len = u16::from_be_bytes([pl[4], pl[5]]);
            let pkt = isdb_ts::pes::PesPacket {
                stream_id: sid,
                unbounded: len == 0,
                body: isdb_ts::pes::PesBody::Header {
                    header: h,
                    data: Vec::new(),
                },
            };
            for v in isdb_ts::pes::check(&pkt) {
                let kind = v
                    .message
                    .split(|c: char| c.is_ascii_digit())
                    .next()
                    .unwrap_or("")
                    .to_string();
                Tally::add(
                    &mut t.violations,
                    format!("{} {kind}", v.clause),
                    v.clause,
                    pid,
                    index,
                    || v.message.clone(),
                );
            }
        }
        Err(e) => {
            let fits = pl.len() >= 9 && pl.len() >= 9 + pl[8] as usize;
            if fits {
                t.header_errors += 1;
                if t.first_error.is_none() {
                    t.first_error = Some(format!(
                        "flags byte {:#04x}, PES_header_data_length {}: {e}",
                        pl[7], pl[8]
                    ));
                }
            } else {
                t.header_truncated += 1;
            }
        }
    }
}

/// Scan a file.
pub fn scan_path(path: &Path, opts: &ScanOptions) -> std::io::Result<Capture> {
    let mut sc = Scanner::new(opts.clone(), path.display().to_string());
    // Pre-pass: learn PSI-designated PIDs (PMT/ES/ECM/EMM) from the first
    // packets so sections before the first PAT/PMT are not missed.
    {
        let f = std::fs::File::open(path)?;
        let mut r = StreamReader::new(std::io::BufReader::with_capacity(1 << 20, f));
        let mut d = SectionDemux::new();
        d.add_pid(0);
        d.add_pid(1);
        let mut n = 0u64;
        while let Some(p) = r.next_packet()? {
            n += 1;
            if n > 400_000 {
                break;
            }
            let pk = p.packet();
            let pid = (u16::from(pk[1] & 0x1F) << 8) | u16::from(pk[2]);
            if !d.has_pid(pid) {
                continue;
            }
            for a in d.push(p.index, pk) {
                if let Assembled::Section(s) = a {
                    sc.learn(pid, &s.data);
                    for &pm in &sc.pmt_pids.clone() {
                        d.add_pid(pm);
                    }
                }
            }
        }
    }
    let f = std::fs::File::open(path)?;
    let mut r = StreamReader::new(std::io::BufReader::with_capacity(1 << 20, f));
    while let Some(p) = r.next_packet()? {
        if opts.max_packets.is_some_and(|m| p.index >= m) {
            break;
        }
        sc.push(p.index, p.packet());
    }
    sc.cap.format = r.sync.format();
    sc.cap.reader = r.sync.stats;
    sc.decoded.clear();
    Ok(sc.cap)
}

/// Scan an in-memory stream (188/192/204-byte framing autodetected).
pub fn scan_bytes(buf: &[u8], opts: &ScanOptions) -> Capture {
    let mut sc = Scanner::new(opts.clone(), "<memory>".into());
    {
        let mut d = SectionDemux::new();
        d.add_pid(0);
        d.add_pid(1);
        for p in SliceReader::new(buf).take(400_000) {
            let pid = (u16::from(p.packet[1] & 0x1F) << 8) | u16::from(p.packet[2]);
            if !d.has_pid(pid) {
                continue;
            }
            for a in d.push(p.index, p.packet) {
                if let Assembled::Section(s) = a {
                    sc.learn(pid, &s.data);
                    for &pm in &sc.pmt_pids.clone() {
                        d.add_pid(pm);
                    }
                }
            }
        }
    }
    let mut r = SliceReader::new(buf);
    for p in r.by_ref() {
        if opts.max_packets.is_some_and(|m| p.index >= m) {
            break;
        }
        sc.push(p.index, p.packet);
    }
    sc.cap.format = r.sync.format();
    sc.cap.reader = r.sync.stats;
    sc.cap
}

/// Scan 188-byte packets.
pub fn scan_packets(pkts: &[[u8; PACKET_SIZE]], opts: &ScanOptions) -> Capture {
    let buf: Vec<u8> = pkts.iter().flatten().copied().collect();
    scan_bytes(&buf, opts)
}

/// Read a whole file as 188-byte packets (framing autodetected; 192/204
/// prefixes/trailers dropped).
pub fn read_packets(
    path: &Path,
) -> std::io::Result<(Vec<[u8; PACKET_SIZE]>, Option<PacketFormat>)> {
    let buf = std::fs::read(path)?;
    let mut r = SliceReader::new(&buf);
    let v: Vec<[u8; PACKET_SIZE]> = r.by_ref().map(|p| *p.packet).collect();
    Ok((v, r.sync.format()))
}

impl Capture {
    /// Latest decodable body of each long-form slot / latest short section.
    pub fn latest_sections(&self) -> impl Iterator<Item = (&SlotKey, &[u8])> {
        self.slots.iter().map(|(k, s)| (k, s.latest.as_slice()))
    }

    /// Latest PAT (PID 0x0000, section 0).
    pub fn pat(&self) -> Option<Long<Pat>> {
        self.slots
            .iter()
            .filter(|(k, _)| k.pid == 0 && k.table_id == 0)
            .max_by_key(|(_, s)| s.latest_packet)
            .and_then(|(_, s)| Long::<Pat>::decode(&s.latest).ok())
    }

    /// Latest PMT of every program, keyed by program_number, with its PID.
    pub fn pmts(&self) -> BTreeMap<u16, (u16, Long<Pmt>)> {
        let mut m: BTreeMap<u16, (u64, u16, Long<Pmt>)> = BTreeMap::new();
        for (k, s) in &self.slots {
            if k.table_id == 2
                && let Ok(p) = Long::<Pmt>::decode(&s.latest)
            {
                let prog = p.header.table_id_extension;
                if m.get(&prog).is_none_or(|(at, _, _)| s.latest_packet > *at) {
                    m.insert(prog, (s.latest_packet, k.pid, p));
                }
            }
        }
        m.into_iter()
            .map(|(k, (_, pid, p))| (k, (pid, p)))
            .collect()
    }
}
