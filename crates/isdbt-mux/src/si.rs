//! PSI/SI section generation (H.222.0 §2.4.4, ARIB STD-B10 §5.2) with the
//! typed codecs of `isdb-ts` and `isdb-si`, plus the experiment overrides
//! of [`crate::config::TableOpts`].

use std::sync::Arc;

use isdb_si::Descriptor;
use isdb_si::bxml::{
    AdditionalAribBxmlInfo, AdditionalAribCarouselInfo, AribBxmlInfo, BxmlEntry, BxmlEntryPoint,
    CarouselInfo, TRANSMISSION_FORMAT_CAROUSEL,
};
use isdb_si::descriptor::{
    AudioComponent, BroadcasterName, Component, Content, ContentAvailability, ContentItem,
    DataComponent, DataContent, ExtendedBroadcaster, ExtendedBroadcasterBody,
    HierarchicalTransmission, LocalTimeOffset, LocalTimeOffsetEntry, NetworkBroadcaster,
    NetworkName, PartialReception, Service as ServiceDescriptor, ServiceList, ServiceListEntry,
    ShortEvent, StreamIdentifier, SystemManagement, TerrestrialBroadcaster,
    TerrestrialDeliverySystem, TransmissionType, TsInformation,
};
use isdb_si::tables::{
    BitBody, BitBroadcaster, EitBody, EitEvent, NetworkLoops, NitBody, SdtBody, SdtService,
    TdtBody, TotBody, TransportStreamEntry,
};
use isdb_si::time::BcdOffset;
use isdb_si::values::{AudioMode, CodedValue};
use isdb_ts::Pid;

use crate::adts::b32::QualityMode;
use crate::carousel::{self, Carousel};
use isdb_text::AribString;
use isdb_ts::descriptor::Descriptor as RawDesc;
use isdb_ts::psi::{Cat, Pat, PatEntry, Pmt, PmtStream};
use isdb_ts::section::{Long, LongBody, LongHeader, Short, ShortBody, ShortHeader, total_len};
use isdbt_core::{CodeRate, GuardInterval, Mode, Modulation, Params};

use crate::config::{
    AddDescriptor, AitCarriage, BmlCfg, Config, LayerName, RawDescriptor, StreamCfg, StreamKind,
    TableOpts, parse_hex,
};
use crate::error::{MuxError, Result};
use crate::jst::{self, JstSeconds};
use crate::signalling::VdcDerived;
use crate::video::SequenceInfo;

/// PIDs of B10 Table 5-1.
pub mod pid {
    use isdb_ts::Pid;

    /// PAT.
    pub const PAT: Pid = Pid::PAT;
    /// CAT.
    pub const CAT: Pid = Pid::CAT;
    /// NIT.
    pub const NIT: Pid = Pid::new(0x0010).unwrap();
    /// SDT/BAT.
    pub const SDT: Pid = Pid::new(0x0011).unwrap();
    /// EIT.
    pub const EIT: Pid = Pid::new(0x0012).unwrap();
    /// TDT/TOT.
    pub const TOT: Pid = Pid::new(0x0014).unwrap();
    /// BIT.
    pub const BIT: Pid = Pid::new(0x0024).unwrap();
}

/// broadcasting_flag '00' = broadcasting (B10 Table 6-35).
const BROADCASTING_FLAG: u16 = 0b00;
/// broadcasting_identifier '000011' = digital terrestrial television (B10 Table 6-36).
const BROADCASTING_IDENTIFIER: u16 = 0b000011;

/// Which table a section stream carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TableKind {
    /// PAT.
    Pat,
    /// CAT.
    Cat,
    /// PMT of service index.
    Pmt(usize),
    /// NIT actual.
    Nit,
    /// SDT actual.
    Sdt,
    /// EIT present/following actual.
    EitPf,
    /// EIT schedule actual.
    EitSchedule,
    /// TOT.
    Tot,
    /// TDT.
    Tdt,
    /// BIT.
    Bit,
    /// Raw table stream index.
    Raw(usize),
    /// BML data carousel (DII + DDBs) of stream `k` of service `i`.
    Carousel(usize, usize),
    /// Hybridcast AIT of service `i` as its own component
    /// (`carriage = "es"`).
    Ait(usize),
}

impl TableKind {
    /// Short name.
    pub fn name(&self) -> String {
        match self {
            TableKind::Pat => "PAT".into(),
            TableKind::Cat => "CAT".into(),
            TableKind::Pmt(i) => format!("PMT[{i}]"),
            TableKind::Nit => "NIT".into(),
            TableKind::Sdt => "SDT".into(),
            TableKind::EitPf => "EIT p/f".into(),
            TableKind::EitSchedule => "EIT schedule".into(),
            TableKind::Tot => "TOT".into(),
            TableKind::Tdt => "TDT".into(),
            TableKind::Bit => "BIT".into(),
            TableKind::Raw(i) => format!("raw[{i}]"),
            TableKind::Carousel(i, k) => format!("carousel[{i}.{k}]"),
            TableKind::Ait(i) => format!("AIT[{i}]"),
        }
    }
}

/// Elementary-stream facts the SI derives descriptors from.
#[derive(Debug, Clone)]
pub enum StreamInfo {
    /// MPEG-2 video with its sequence parameters.
    Video {
        /// Sequence parameters.
        seq: SequenceInfo,
        /// The ES contains sequence_end_code.
        has_sequence_end: bool,
    },
    /// AAC ADTS audio.
    Audio {
        /// B10 Table 6-45 code.
        sampling_rate: Option<u8>,
        /// B10 Table 6-43 audio mode derived from the ES.
        audio_mode: Option<AudioMode>,
        /// B32 Part 2 Attachment Ch. 3 Table 1 mode derived from the ES.
        quality: QualityMode,
    },
    /// A BML data carousel (`bml-carousel`), loaded from its files.
    Carousel(Arc<Carousel>),
    /// No media.
    None,
}

/// An event with absolute times.
#[derive(Debug, Clone)]
pub struct Event {
    /// Index into the service's `events`.
    pub index: usize,
    /// event_id.
    pub event_id: u16,
    /// Start (JST).
    pub start: JstSeconds,
    /// Duration (s).
    pub duration: u32,
}

/// Resolve event start times (config order is kept; events must not
/// overlap and are sorted chronologically).
pub fn resolve_events(cfg: &Config, clock_start: JstSeconds) -> Result<Vec<Vec<Event>>> {
    let mut all = Vec::new();
    for s in &cfg.services {
        let mut v: Vec<Event> = Vec::new();
        let mut prev_end: Option<JstSeconds> = None;
        for (i, e) in s.events.iter().enumerate() {
            let start = match (&e.start, e.start_offset_s) {
                (Some(t), None) => jst::parse(t)?,
                (None, Some(o)) => clock_start + o,
                (None, None) => prev_end.unwrap_or(clock_start - clock_start.rem_euclid(3600)),
                (Some(_), Some(_)) => {
                    return Err(MuxError::Config(format!(
                        "event {:#06x}: give start or start_offset_s, not both",
                        e.event_id
                    )));
                }
            };
            let duration = jst::parse_hms(&e.duration)?;
            prev_end = Some(start + i64::from(duration));
            v.push(Event {
                index: i,
                event_id: e.event_id,
                start,
                duration,
            });
        }
        v.sort_by_key(|e| e.start);
        for w in v.windows(2) {
            if w[0].start + i64::from(w[0].duration) > w[1].start {
                return Err(MuxError::Config(format!(
                    "service {:#06x}: events {:#06x} and {:#06x} overlap",
                    s.service_id, w[0].event_id, w[1].event_id
                )));
            }
        }
        all.push(v);
    }
    Ok(all)
}

/// Robustness rank of B31 Part 2 Table 3-2 (1 = most robust).
pub fn robustness_rank(m: Modulation, r: CodeRate) -> u32 {
    let ri = match r {
        CodeRate::R1_2 => 0,
        CodeRate::R2_3 => 1,
        CodeRate::R3_4 => 2,
        CodeRate::R5_6 => 3,
        CodeRate::R7_8 => 4,
    };
    match m {
        Modulation::Dqpsk => 1 + 2 * ri,
        Modulation::Qpsk => 2 + 2 * ri,
        Modulation::Qam16 => 11 + ri,
        Modulation::Qam64 => 16 + ri,
    }
}

/// Layer for PAT/NIT/CAT per B31 Part 2 Table 3-1: the partial-reception
/// layer with partial reception, else the robustest layer.
pub fn control_layer(params: &Params) -> usize {
    if params.partial_reception() {
        return 0;
    }
    params
        .active_layers()
        .min_by_key(|(id, l)| (robustness_rank(l.modulation, l.code_rate), id.index()))
        .map(|(id, _)| id.index())
        .unwrap_or(0)
}

/// guard_interval code of B10 Table 6-62.
pub fn guard_interval_code(g: GuardInterval) -> u8 {
    match g {
        GuardInterval::G1_32 => 0b00,
        GuardInterval::G1_16 => 0b01,
        GuardInterval::G1_8 => 0b10,
        GuardInterval::G1_4 => 0b11,
    }
}

/// transmission_mode code of B10 Table 6-63.
pub fn transmission_mode_code(m: Mode) -> u8 {
    match m {
        Mode::M1 => 0b00,
        Mode::M2 => 0b01,
        Mode::M3 => 0b10,
    }
}

/// Frequency code (1/7 MHz units, B10 §6.2.31) of UHF channel `ch`:
/// centre 473 + 6·(ch − 13) + 1/7 MHz.
pub fn channel_frequency_code(ch: u32) -> Result<u16> {
    let ch = u16::try_from(ch)
        .ok()
        .filter(|ch| (13..=62).contains(ch))
        .ok_or_else(|| MuxError::Config(format!("UHF channel {ch} outside 13..62")))?;
    Ok((473 + 6 * (ch - 13)) * 7 + 1) // at most 767 * 7 + 1 = 5370
}

fn text(s: &str) -> Result<AribString> {
    isdb_text::encode(s).map_err(|e| MuxError::Config(format!("text {s:?}: {e}")))
}

fn raw_si(d: &RawDescriptor) -> Result<Descriptor> {
    Ok(Descriptor::Unknown {
        tag: d.tag,
        data: parse_hex(&d.hex)?,
    })
}

fn raw_list(ds: &[RawDescriptor]) -> Result<Vec<Descriptor>> {
    ds.iter().map(raw_si).collect()
}

fn to_raw(d: &Descriptor) -> Result<RawDesc> {
    let mut v = Vec::new();
    d.encode(&mut v)?;
    Ok(RawDesc::new(v[0], v[2..].to_vec()))
}

fn parse_num(s: &str) -> Option<u32> {
    let s = s.trim();
    if let Some(h) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        u32::from_str_radix(h, 16).ok()
    } else {
        s.parse().ok()
    }
}

/// [`parse_num`] into the width of the selected identifier (`None` when
/// the number does not fit: it would select nothing).
fn parse_id<T: TryFrom<u32>>(s: &str) -> Option<T> {
    T::try_from(parse_num(s)?).ok()
}

/// Loop selector of an [`AddDescriptor`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Target {
    Main,
    Program,
    Es(Pid),
    Network,
    Ts,
    Service(Option<u16>),
    Event(Option<u16>, Option<u16>),
    First,
    Broadcaster(Option<u8>),
}

pub(crate) fn parse_target(s: &str) -> Option<Target> {
    let f: Vec<&str> = s.split(':').collect();
    Some(match f.as_slice() {
        ["main"] => Target::Main,
        ["program"] => Target::Program,
        ["es", p] => Target::Es(Pid::new(parse_id(p)?)?),
        ["network"] => Target::Network,
        ["ts"] => Target::Ts,
        ["service"] => Target::Service(None),
        ["service", s] => Target::Service(Some(parse_id(s)?)),
        ["event"] => Target::Event(None, None),
        ["event", s] => Target::Event(Some(parse_id(s)?), None),
        ["event", s, e] => Target::Event(Some(parse_id(s)?), Some(parse_id(e)?)),
        ["first"] => Target::First,
        ["broadcaster"] => Target::Broadcaster(None),
        ["broadcaster", b] => Target::Broadcaster(Some(parse_id(b)?)),
        _ => return None,
    })
}

/// Validate the loop selectors of `opts` for `kind`.
pub fn validate_targets(kind: TableKind, opts: &TableOpts) -> Result<()> {
    for a in &opts.add_descriptors {
        let t = parse_target(&a.target).ok_or_else(|| {
            MuxError::Config(format!("{}: bad loop selector {:?}", kind.name(), a.target))
        })?;
        let ok = match kind {
            TableKind::Cat | TableKind::Tot => t == Target::Main,
            TableKind::Pmt(_) => matches!(t, Target::Program | Target::Es(_)),
            TableKind::Nit => matches!(t, Target::Network | Target::Ts),
            TableKind::Sdt => matches!(t, Target::Service(_)),
            TableKind::EitPf | TableKind::EitSchedule => matches!(t, Target::Event(..)),
            TableKind::Bit => matches!(t, Target::First | Target::Broadcaster(_)),
            TableKind::Pat
            | TableKind::Tdt
            | TableKind::Raw(_)
            | TableKind::Carousel(..)
            | TableKind::Ait(_) => false,
        };
        if !ok {
            return Err(MuxError::Config(format!(
                "{}: loop selector {:?} does not apply",
                kind.name(),
                a.target
            )));
        }
        parse_hex(&a.hex)?;
    }
    for s in opts.replace_sections.iter().chain(&opts.extra_sections) {
        let b = parse_hex(s)?;
        if total_len(&b) != Some(b.len()) {
            return Err(MuxError::Config(format!(
                "{}: section hex is not one complete section ({} bytes)",
                kind.name(),
                b.len()
            )));
        }
    }
    for p in &opts.byte_patches {
        parse_hex(&p.hex)?;
    }
    Ok(())
}

/// Insert the matching added descriptors into `loop_`.
fn add_into(
    loop_: &mut Vec<Descriptor>,
    adds: &[AddDescriptor],
    pred: impl Fn(&Target) -> bool,
) -> Result<()> {
    let mut front = Vec::new();
    for a in adds {
        if let Some(t) = parse_target(&a.target)
            && pred(&t)
        {
            let d = Descriptor::Unknown {
                tag: a.tag,
                data: parse_hex(&a.hex)?,
            };
            if a.first {
                front.push(d);
            } else {
                loop_.push(d);
            }
        }
    }
    if !front.is_empty() {
        front.append(loop_);
        *loop_ = front;
    }
    Ok(())
}

fn add_into_raw(
    loop_: &mut Vec<RawDesc>,
    adds: &[AddDescriptor],
    pred: impl Fn(&Target) -> bool,
) -> Result<()> {
    let mut front = Vec::new();
    for a in adds {
        if let Some(t) = parse_target(&a.target)
            && pred(&t)
        {
            let d = RawDesc::new(a.tag, parse_hex(&a.hex)?);
            if a.first {
                front.push(d);
            } else {
                loop_.push(d);
            }
        }
    }
    if !front.is_empty() {
        front.append(loop_);
        *loop_ = front;
    }
    Ok(())
}

fn apply_long_header(h: &mut LongHeader, o: &TableOpts) {
    let x = &o.header;
    if let Some(v) = x.table_id {
        h.table_id = v;
    }
    if let Some(v) = x.private_indicator {
        h.private_indicator = v;
    }
    if let Some(v) = x.reserved1 {
        h.reserved1 = v & 3;
    }
    if let Some(v) = x.table_id_extension {
        h.table_id_extension = v;
    }
    if let Some(v) = x.reserved2 {
        h.reserved2 = v & 3;
    }
    if let Some(v) = x.version_number {
        h.version_number = v & 0x1F;
    }
    if let Some(v) = x.current_next_indicator {
        h.current_next_indicator = v;
    }
    if let Some(v) = x.section_number {
        h.section_number = v;
    }
    if let Some(v) = x.last_section_number {
        h.last_section_number = v;
    }
}

fn apply_short_header(h: &mut ShortHeader, o: &TableOpts) {
    let x = &o.header;
    if let Some(v) = x.table_id {
        h.table_id = v;
    }
    if let Some(v) = x.private_indicator {
        h.private_indicator = v;
    }
    if let Some(v) = x.reserved1 {
        h.reserved1 = v & 3;
    }
}

fn fix_crc(sec: &mut [u8]) {
    let n = sec.len();
    if n >= 4 {
        let c = isdb_ts::crc::crc32(&sec[..n - 4]);
        sec[n - 4..].copy_from_slice(&c.to_be_bytes());
    }
}

/// Post-encode overrides: section_syntax_indicator, byte patches, extra
/// sections. `has_crc` tells whether the generated form carries CRC_32.
fn post(mut secs: Vec<Vec<u8>>, o: &TableOpts, has_crc: bool) -> Result<Vec<Vec<u8>>> {
    if let Some(ssi) = o.header.section_syntax_indicator {
        for s in &mut secs {
            s[1] = (s[1] & 0x7F) | if ssi { 0x80 } else { 0 };
            if has_crc {
                fix_crc(s);
            }
        }
    }
    for p in &o.byte_patches {
        let bytes = parse_hex(&p.hex)?;
        for (i, s) in secs.iter_mut().enumerate() {
            if p.section.is_some_and(|k| k != i) {
                continue;
            }
            if p.offset + bytes.len() > s.len() {
                return Err(MuxError::Config(format!(
                    "byte patch at {} runs past the {}-byte section",
                    p.offset,
                    s.len()
                )));
            }
            s[p.offset..p.offset + bytes.len()].copy_from_slice(&bytes);
            if p.fix_crc && has_crc {
                fix_crc(s);
            }
        }
    }
    for x in &o.extra_sections {
        secs.push(parse_hex(x)?);
    }
    Ok(secs)
}

fn encode_long<B: LongBody>(mut secs: Vec<Long<B>>, o: &TableOpts) -> Result<Vec<Vec<u8>>> {
    let mut out = Vec::new();
    for s in &mut secs {
        apply_long_header(&mut s.header, o);
        out.push(s.encode()?);
    }
    post(out, o, true)
}

fn encode_short<B: ShortBody>(mut s: Short<B>, o: &TableOpts) -> Result<Vec<Vec<u8>>> {
    apply_short_header(&mut s.header, o);
    let v = vec![s.encode()?];
    post(v, o, B::HAS_CRC)
}

fn si_header(table_id: u8, ext: u16, version: u8) -> LongHeader {
    let mut h = LongHeader::new(table_id, ext, version);
    h.private_indicator = true; // B10: reserved_future_use = '1'
    h
}

/// Builds the sections of every table from the configuration.
#[derive(Debug, Clone)]
pub struct SiBuilder {
    /// Configuration.
    pub cfg: Config,
    /// Transmission parameters.
    pub params: Params,
    /// Per service, per stream media facts.
    pub media: Vec<Vec<StreamInfo>>,
    /// Per service, resolved events.
    pub events: Vec<Vec<Event>>,
}

impl SiBuilder {
    /// The options of `kind`.
    pub fn opts(&self, kind: TableKind) -> TableOpts {
        let c = &self.cfg;
        match kind {
            TableKind::Pat => c.pat.opts(),
            TableKind::Cat => c.cat.opts(),
            TableKind::Pmt(i) => c.services[i].pmt.opts(),
            TableKind::Nit => c.nit.opts(),
            TableKind::Sdt => c.sdt.opts(),
            TableKind::EitPf => c.eit.opts(),
            TableKind::EitSchedule => c.eit_schedule.opts(),
            TableKind::Tot => c.tot.opts(),
            TableKind::Tdt => c.tdt.opts(),
            TableKind::Bit => c.bit.opts(),
            TableKind::Raw(i) => TableOpts {
                enabled: true,
                interval_ms: Some(c.raw_tables[i].interval_ms),
                layer: Some(c.raw_tables[i].layer),
                pid: Some(c.raw_tables[i].pid),
                replace_sections: c.raw_tables[i].sections.clone(),
                ..Default::default()
            },
            TableKind::Carousel(i, k) => {
                let s = &c.services[i];
                let st = &s.streams[k];
                TableOpts {
                    enabled: true,
                    interval_ms: Some(st.bml.as_ref().map_or(500, |b| b.cycle_ms)),
                    layer: Some(st.layer.unwrap_or(s.layer)),
                    pid: Some(st.pid),
                    ..Default::default()
                }
            }
            TableKind::Ait(i) => {
                let s = &c.services[i];
                let h = s.hybridcast.as_ref();
                TableOpts {
                    enabled: h.is_some_and(|h| h.carriage == AitCarriage::Es),
                    interval_ms: h.map(|h| h.interval_ms),
                    layer: Some(h.and_then(|h| h.layer).unwrap_or(s.layer)),
                    pid: h.and_then(|h| h.pid),
                    ..Default::default()
                }
            }
        }
    }

    /// The carousel stream `k` of service `i` carries, if loaded.
    pub fn carousel(&self, i: usize, k: usize) -> Option<&Carousel> {
        match self.media.get(i).and_then(|m| m.get(k)) {
            Some(StreamInfo::Carousel(c)) => Some(c),
            _ => None,
        }
    }

    /// Default PID of `kind` (B10 Table 5-1; PMT from the service).
    pub fn default_pid(&self, kind: TableKind) -> Pid {
        match kind {
            TableKind::Pat => pid::PAT,
            TableKind::Cat => pid::CAT,
            TableKind::Pmt(i) => self.cfg.services[i].pmt_pid,
            TableKind::Nit => pid::NIT,
            TableKind::Sdt => pid::SDT,
            TableKind::EitPf | TableKind::EitSchedule => pid::EIT,
            TableKind::Tot | TableKind::Tdt => pid::TOT,
            TableKind::Bit => pid::BIT,
            TableKind::Raw(i) => self.cfg.raw_tables[i].pid,
            TableKind::Carousel(i, k) => self.cfg.services[i].streams[k].pid,
            TableKind::Ait(i) => self.cfg.services[i]
                .hybridcast
                .as_ref()
                .and_then(|h| h.pid)
                .unwrap_or(Pid::NULL),
        }
    }

    /// Default layer of `kind`: PAT/NIT/CAT per B31 Part 2 Table 3-1, PMT on
    /// its service's layer (Table 3-3), the other SI with PAT (operational
    /// choice, SPEC-NOTES).
    pub fn default_layer(&self, kind: TableKind) -> usize {
        match kind {
            TableKind::Pmt(i) => {
                // Table 3-3: a partial-reception service's PMT on the
                // partial-reception layer; otherwise the robustest layer
                // carrying the service's elementary streams (condition 2
                // requires it, condition 3 allows it).
                let s = &self.cfg.services[i];
                if s.layer == LayerName::A && self.params.partial_reception() {
                    0
                } else {
                    (0..s.streams.len())
                        .map(|k| self.stream_layer(i, k))
                        .min_by_key(|&l| (self.layer_rank(l), l))
                        .unwrap_or(s.layer.index())
                }
            }
            TableKind::Raw(i) => self.cfg.raw_tables[i].layer.index(),
            TableKind::Carousel(i, k) => self.stream_layer(i, k),
            TableKind::Ait(i) => self.cfg.services[i].layer.index(),
            _ => control_layer(&self.params),
        }
    }

    /// Default maximum repetition interval (ms) of B10 Table 5-2.
    pub fn default_interval_ms(kind: TableKind) -> u32 {
        match kind {
            TableKind::Pat | TableKind::Pmt(_) => 100,
            TableKind::Cat => 1_000,
            TableKind::Nit => 10_000,
            TableKind::Sdt | TableKind::EitPf => 2_000,
            TableKind::EitSchedule => 10_000,
            TableKind::Tot | TableKind::Tdt => 30_000,
            TableKind::Bit => 20_000,
            TableKind::Raw(_) | TableKind::Carousel(..) | TableKind::Ait(_) => 1_000,
        }
    }

    /// Generate the sections of `kind` at wall-clock `now` with `version`.
    pub fn build(&self, kind: TableKind, now: JstSeconds, version: u8) -> Result<Vec<Vec<u8>>> {
        let o = self.opts(kind);
        if !o.replace_sections.is_empty() {
            return o.replace_sections.iter().map(|h| parse_hex(h)).collect();
        }
        let v = version & 0x1F;
        match kind {
            TableKind::Pat => self.pat(&o, v),
            TableKind::Cat => self.cat(&o, v),
            TableKind::Pmt(i) => self.pmt(i, &o, v),
            TableKind::Nit => self.nit(&o, v),
            TableKind::Sdt => self.sdt(&o, v),
            TableKind::EitPf => self.eit_pf(&o, v, now),
            TableKind::EitSchedule => self.eit_schedule(&o, v, now),
            TableKind::Tot => self.tot(&o, now),
            TableKind::Tdt => encode_short(
                Short {
                    header: ShortHeader {
                        table_id: 0x70,
                        private_indicator: true,
                        reserved1: 0b11,
                    },
                    body: TdtBody {
                        jst_time: jst::to_mjd_time(now)?,
                    },
                },
                &o,
            ),
            TableKind::Bit => self.bit(&o, v),
            TableKind::Raw(_) => unreachable!("raw tables always have replace_sections"),
            // The carousel's own versions (transaction_id, moduleVersion)
            // are configured; the stream version is not applied.
            TableKind::Carousel(i, k) => post(
                self.carousel(i, k)
                    .ok_or_else(|| MuxError::Config(format!("{} not loaded", kind.name())))?
                    .sections
                    .clone(),
                &o,
                true,
            ),
            TableKind::Ait(i) => {
                let h = self.cfg.services[i].hybridcast.as_ref().ok_or_else(|| {
                    MuxError::Config(format!("{} without [service.hybridcast]", kind.name()))
                })?;
                post(vec![crate::hybridcast::ait_section(h)?], &o, true)
            }
        }
    }

    fn pat(&self, o: &TableOpts, v: u8) -> Result<Vec<Vec<u8>>> {
        let nit_pid = self.cfg.nit.pid.unwrap_or(pid::NIT);
        let mut entries = vec![PatEntry::new(0, nit_pid)];
        let mut svcs: Vec<_> = self.cfg.services.iter().collect();
        svcs.sort_by_key(|s| s.service_id);
        for s in svcs {
            entries.push(PatEntry::new(s.service_id, s.pmt.pid.unwrap_or(s.pmt_pid)));
        }
        encode_long(
            vec![Pat::section(
                self.cfg.transport.transport_stream_id,
                v,
                entries,
            )],
            o,
        )
    }

    fn cat(&self, o: &TableOpts, v: u8) -> Result<Vec<Vec<u8>>> {
        let mut ds: Vec<RawDesc> = self
            .cfg
            .cat
            .descriptors
            .iter()
            .map(|d| Ok(RawDesc::new(d.tag, parse_hex(&d.hex)?)))
            .collect::<Result<_>>()?;
        add_into_raw(&mut ds, &o.add_descriptors, |t| *t == Target::Main)?;
        encode_long(vec![Cat::section(v, ds)], o)
    }

    /// Digital copy control descriptor of service `sid` (B10 §6.2.23); the
    /// component loop only outside the PMT (§6.2.23 "When this descriptor
    /// is transmitted by PMT, component control flag should always be '0'").
    fn copy_control(
        cc: &crate::config::CopyControlCfg,
        in_pmt: bool,
        sid: u16,
    ) -> Result<Descriptor> {
        Ok(Descriptor::DigitalCopyControl(cc.descriptor(
            !in_pmt,
            &format!("service {sid:#06x} copy_control"),
        )?))
    }

    fn content_availability(c: &crate::config::ContentAvailabilityCfg) -> Descriptor {
        Descriptor::ContentAvailability(ContentAvailability {
            reserved_future_use: true,
            copy_restriction_mode: c.copy_restriction_mode,
            image_constraint_token: c.image_constraint_token,
            retention_mode: c.retention_mode,
            retention_state: c.retention_state & 7,
            encryption_mode: c.encryption_mode,
            reserved_future_use_bytes: Vec::new(),
        })
    }

    fn pmt(&self, i: usize, o: &TableOpts, v: u8) -> Result<Vec<Vec<u8>>> {
        let s = &self.cfg.services[i];
        let mut program = Vec::new();
        if let Some(cc) = &s.copy_control
            && cc.in_pmt
        {
            program.push(to_raw(&Self::copy_control(cc, true, s.service_id)?)?);
        }
        if let Some(ca) = &s.content_availability
            && ca.in_pmt
        {
            program.push(to_raw(&Self::content_availability(ca))?);
        }
        for d in &s.program_descriptors {
            program.push(RawDesc::new(d.tag, parse_hex(&d.hex)?));
        }
        add_into_raw(&mut program, &o.add_descriptors, |t| *t == Target::Program)?;
        let mut streams = Vec::new();
        for (k, st) in s.streams.iter().enumerate() {
            let mut ds = Vec::new();
            if let Some(tag) = st.component_tag {
                ds.push(to_raw(&Descriptor::StreamIdentifier(StreamIdentifier {
                    component_tag: tag,
                }))?);
            }
            // B10 §6.2.30; derivation rules in `VideoDecodeControlCfg`.
            let derived = match (&self.media[i][k], st.kind) {
                (
                    StreamInfo::Video {
                        seq,
                        has_sequence_end,
                    },
                    StreamKind::Mpeg2Video | StreamKind::TsProgram,
                ) => Some(VdcDerived {
                    sequence_end_code: *has_sequence_end,
                    video_encode_format: seq.video_encode_format(),
                }),
                _ => None,
            };
            if st.kind != StreamKind::AacAdts
                && let Some(vdc) = st.video_decode_control.resolve(derived)
            {
                ds.push(to_raw(&Descriptor::VideoDecodeControl(vdc))?);
            }
            if let Some(h) = &st.hierarchical {
                ds.push(to_raw(&Descriptor::HierarchicalTransmission(
                    HierarchicalTransmission {
                        reserved_future_use_1: 0x7F,
                        quality_level: self.hierarchical_quality(i, k, h)?,
                        reserved_future_use_2: 0b111,
                        reference_pid: h.reference_pid,
                    },
                ))?);
            }
            if st.kind == StreamKind::BmlCarousel {
                // B24 Vol. 2 §9.3.2: data component descriptor with
                // additional_arib_bxml_info; the component_tag of the
                // entry component (stream identifier first, above)
                if st.component_tag.is_none() {
                    ds.insert(
                        0,
                        to_raw(&Descriptor::StreamIdentifier(StreamIdentifier {
                            component_tag: carousel::DEFAULT_COMPONENT_TAG,
                        }))?,
                    );
                }
                let bml = st.bml.clone().unwrap_or_default();
                ds.push(to_raw(&Descriptor::DataComponent(DataComponent {
                    data_component_id: bml.data_component_id,
                    additional_data_component_info: additional_bxml_info(&bml, s.service_id)?,
                }))?);
            }
            for d in &st.descriptors {
                ds.push(RawDesc::new(d.tag, parse_hex(&d.hex)?));
            }
            add_into_raw(&mut ds, &o.add_descriptors, |t| *t == Target::Es(st.pid))?;
            streams.push(PmtStream::new(stream_type(st), st.pid, ds));
        }
        if let Some(h) = &s.hybridcast
            && h.carriage == AitCarriage::Es
        {
            // B24 Vol. 4 §7.3: the AIT component with ait_identifier_info
            let pid = h.pid.ok_or_else(|| {
                MuxError::Config(format!(
                    "service {:#06x} hybridcast: carriage = \"es\" needs pid",
                    s.service_id
                ))
            })?;
            let dcid = h.data_component_id.ok_or_else(|| {
                MuxError::Config(format!(
                    "service {:#06x} hybridcast: carriage = \"es\" needs data_component_id",
                    s.service_id
                ))
            })?;
            let mut ds = vec![to_raw(&Descriptor::DataComponent(DataComponent {
                data_component_id: dcid,
                additional_data_component_info: crate::hybridcast::ait_identifier_info(h)?,
            }))?];
            add_into_raw(&mut ds, &o.add_descriptors, |t| *t == Target::Es(pid))?;
            streams.push(PmtStream::new(AIT_STREAM_TYPE, pid, ds));
        }
        encode_long(
            vec![Pmt::section(
                s.service_id,
                v,
                Pmt::new(s.pcr_pid, program, streams),
            )],
            o,
        )
    }

    fn stream_layer(&self, i: usize, k: usize) -> usize {
        let s = &self.cfg.services[i];
        s.streams[k].layer.unwrap_or(s.layer).index()
    }

    fn layer_rank(&self, layer: usize) -> u32 {
        self.params.layers()[layer].map_or(u32::MAX, |l| robustness_rank(l.modulation, l.code_rate))
    }

    /// quality_level: explicit, or '1' for the stream on the less robust
    /// layer of the pair (B31 Part 2 Table 3-2).
    fn hierarchical_quality(
        &self,
        i: usize,
        k: usize,
        h: &crate::config::HierarchicalCfg,
    ) -> Result<bool> {
        if let Some(q) = h.high_quality {
            return Ok(q);
        }
        let s = &self.cfg.services[i];
        let r = s
            .streams
            .iter()
            .position(|x| x.pid == h.reference_pid)
            .ok_or_else(|| {
                MuxError::Config(format!(
                    "hierarchical reference_pid {:#06x} is not a stream of service {:#06x}",
                    h.reference_pid, s.service_id
                ))
            })?;
        let (a, b) = (
            self.layer_rank(self.stream_layer(i, k)),
            self.layer_rank(self.stream_layer(i, r)),
        );
        if a == b {
            return Err(MuxError::Config(format!(
                "stream {:#06x}: both hierarchical streams have the same robustness; set high_quality",
                s.streams[k].pid
            )));
        }
        Ok(a > b)
    }

    fn nit(&self, o: &TableOpts, v: u8) -> Result<Vec<Vec<u8>>> {
        let c = &self.cfg;
        let n = &c.network;
        let mut net = vec![Descriptor::NetworkName(NetworkName {
            name: text(&n.name)?,
        })];
        if n.system_management {
            // B10 Table 6-34/6-35/6-36: broadcasting_flag '00' (broadcasting),
            // broadcasting_identifier '000011' (digital terrestrial TV).
            net.push(Descriptor::SystemManagement(SystemManagement {
                system_management_id: (BROADCASTING_FLAG << 14)
                    | (BROADCASTING_IDENTIFIER << 8)
                    | u16::from(n.additional_broadcasting_identification),
                additional_identification_info: Vec::new(),
            }));
        }
        add_into(&mut net, &o.add_descriptors, |t| *t == Target::Network)?;

        let mut freqs = Vec::new();
        for &ch in &n.channels {
            freqs.push(channel_frequency_code(ch)?);
        }
        freqs.extend_from_slice(&n.frequencies);
        let mut ts = vec![Descriptor::TerrestrialDeliverySystem(
            TerrestrialDeliverySystem {
                area_code: n.area_code & 0x0FFF,
                guard_interval: guard_interval_code(self.params.guard()),
                transmission_mode: transmission_mode_code(self.params.mode()),
                frequencies: freqs,
            },
        )];
        let services_on = |layer: LayerName| {
            let mut v: Vec<u16> = c
                .services
                .iter()
                .filter(|s| s.layer == layer)
                .map(|s| s.service_id)
                .collect();
            v.sort();
            v
        };
        ts.push(Descriptor::TsInformation(TsInformation {
            remote_control_key_id: n.remote_control_key_id.get(),
            ts_name: text(n.ts_name.as_deref().unwrap_or(&n.name))?,
            transmission_types: n
                .transmission_types
                .iter()
                .map(|t| TransmissionType {
                    transmission_type_info: t.info,
                    service_ids: services_on(t.layer),
                })
                .collect(),
            reserved_future_use: Vec::new(),
        }));
        let mut list: Vec<ServiceListEntry> = c
            .services
            .iter()
            .map(|s| ServiceListEntry {
                service_id: s.service_id,
                service_type: s.service_type,
            })
            .collect();
        list.sort_by_key(|e| e.service_id);
        ts.push(Descriptor::ServiceList(ServiceList { services: list }));
        let partial = services_on(LayerName::A);
        if self.params.partial_reception() && n.partial_reception_descriptor && !partial.is_empty()
        {
            ts.push(Descriptor::PartialReception(PartialReception {
                service_ids: partial,
            }));
        }
        add_into(&mut ts, &o.add_descriptors, |t| *t == Target::Ts)?;
        let body = NitBody(NetworkLoops {
            reserved_future_use_1: 0xF,
            descriptors: net,
            reserved_future_use_2: 0xF,
            transport_streams: vec![TransportStreamEntry {
                transport_stream_id: c.transport.transport_stream_id,
                original_network_id: c.original_network_id(),
                reserved_future_use: 0xF,
                descriptors: ts,
            }],
        });
        encode_long(
            vec![Long {
                header: si_header(0x40, n.network_id, v),
                body,
            }],
            o,
        )
    }

    fn has_schedule(&self, i: usize) -> bool {
        self.cfg.eit_schedule.enabled
            && self.cfg.services[i].eit_schedule
            && !self.events[i].is_empty()
    }

    fn sdt(&self, o: &TableOpts, v: u8) -> Result<Vec<Vec<u8>>> {
        let c = &self.cfg;
        let mut idx: Vec<usize> = (0..c.services.len()).collect();
        idx.sort_by_key(|&i| c.services[i].service_id);
        let mut services = Vec::new();
        for i in idx {
            let s = &c.services[i];
            let mut ds = vec![Descriptor::Service(ServiceDescriptor {
                service_type: s.service_type,
                provider_name: text(&s.provider)?,
                service_name: text(&s.name)?,
            })];
            if let Some(cc) = &s.copy_control
                && cc.in_sdt
            {
                ds.push(Self::copy_control(cc, false, s.service_id)?);
            }
            ds.extend(raw_list(&s.sdt_descriptors)?);
            add_into(&mut ds, &o.add_descriptors, |t| {
                matches!(t, Target::Service(None)) || *t == Target::Service(Some(s.service_id))
            })?;
            services.push(SdtService {
                service_id: s.service_id,
                reserved_future_use: 0b111,
                eit_user_defined_flags: s.eit_user_defined_flags & 7,
                eit_schedule_flag: self.has_schedule(i),
                eit_present_following_flag: c.eit.enabled && s.eit_pf,
                running_status: s.running_status & 7,
                free_ca_mode: s.free_ca_mode,
                descriptors: ds,
            });
        }
        let body = SdtBody {
            original_network_id: c.original_network_id(),
            reserved_future_use: 0xFF,
            services,
        };
        encode_long(
            vec![Long {
                header: si_header(0x42, c.transport.transport_stream_id, v),
                body,
            }],
            o,
        )
    }

    fn event_descriptors(&self, i: usize, e: &Event, o: &TableOpts) -> Result<Vec<Descriptor>> {
        let s = &self.cfg.services[i];
        let ec = &s.events[e.index];
        let mut ds = vec![Descriptor::ShortEvent(ShortEvent {
            language: *b"jpn",
            event_name: text(&ec.name)?,
            text: text(&ec.text)?,
        })];
        for (k, st) in s.streams.iter().enumerate() {
            if !st.eit_component {
                continue;
            }
            let tag = st.component_tag.unwrap_or(0);
            match &self.media[i][k] {
                StreamInfo::Video { seq, .. } => {
                    // B10 Table 6-5, stream_content 0x1 (MPEG-2 video)
                    if let Some(ct) = st.component_type.video(seq.component_type()) {
                        ds.push(Descriptor::Component(Component {
                            reserved_future_use: 0xF,
                            stream_content: 0x1,
                            component_type: ct.code(),
                            component_tag: tag,
                            language: *b"jpn",
                            text: AribString(Vec::new()),
                        }));
                    }
                }
                StreamInfo::Audio {
                    sampling_rate,
                    audio_mode,
                    quality,
                } => {
                    // B10 Table 6-43 (dialog control | disabilities | mode)
                    if let (Some(sr), Some(ct)) =
                        (sampling_rate, st.component_type.audio(*audio_mode))
                    {
                        ds.push(Descriptor::AudioComponent(AudioComponent {
                            reserved_future_use_1: 0xF,
                            stream_content: 0x2,
                            component_type: ct.code(),
                            component_tag: tag,
                            stream_type: stream_type(st),
                            simulcast_group_tag: 0xFF,
                            main_component_flag: true,
                            quality_indicator: st.audio_quality_indicator.resolve(*quality).code(),
                            sampling_rate: *sr,
                            reserved_future_use_2: true,
                            language: *b"jpn",
                            language_2: None,
                            text: AribString(Vec::new()),
                        }));
                    }
                }
                StreamInfo::None | StreamInfo::Carousel(_) => {}
            }
        }
        for (k, st) in s.streams.iter().enumerate() {
            if st.kind != StreamKind::BmlCarousel {
                continue;
            }
            let bml = st.bml.clone().unwrap_or_default();
            if !(bml.entry_point && bml.eit_data_content) {
                continue;
            }
            // B10 §6.2.28 / B24 Vol. 2 §9.3.3: the entry component, every
            // carousel of the service's BML content, and component_ref = the
            // other components the content uses (live broadcasts list the
            // video, audio and other carousel tags)
            let mut carousels = Vec::new();
            let mut refs = Vec::new();
            for (k2, other) in s.streams.iter().enumerate() {
                let is_bml = other.kind == StreamKind::BmlCarousel;
                let tag = other.component_tag.unwrap_or(if is_bml {
                    carousel::DEFAULT_COMPONENT_TAG
                } else {
                    0
                });
                if k2 != k && (other.component_tag.is_some() || is_bml) {
                    refs.push(tag);
                }
                if !is_bml {
                    continue;
                }
                let other_bml = other.bml.clone().unwrap_or_default();
                carousels.push(CarouselInfo {
                    component_tag: tag,
                    event_section: other_bml.event_section,
                    reserved: 0b111,
                    component_size: self.carousel(i, k2).map(|c| c.component_size),
                    transaction_id: None,
                    timeout_dii: None,
                    leak_rate: None,
                });
            }
            ds.push(Descriptor::DataContent(DataContent {
                data_component_id: bml.data_component_id,
                entry_component: st.component_tag.unwrap_or(carousel::DEFAULT_COMPONENT_TAG),
                selector: bxml_info(&bml, carousels, s.service_id)?,
                component_refs: refs,
                language: *b"jpn",
                text: AribString(Vec::new()),
            }));
        }
        if !ec.content.is_empty() {
            ds.push(Descriptor::Content(Content {
                items: ec
                    .content
                    .iter()
                    .map(|&b| ContentItem {
                        content_nibble_level_1: b >> 4,
                        content_nibble_level_2: b & 0xF,
                        user_nibble_1: 0xF,
                        user_nibble_2: 0xF,
                    })
                    .collect(),
            }));
        }
        if let Some(cc) = &s.copy_control
            && cc.in_eit
        {
            ds.push(Self::copy_control(cc, false, s.service_id)?);
        }
        if let Some(ca) = &s.content_availability
            && ca.in_eit
        {
            ds.push(Self::content_availability(ca));
        }
        ds.extend(raw_list(&ec.descriptors)?);
        let sid = s.service_id;
        add_into(&mut ds, &o.add_descriptors, |t| match t {
            Target::Event(None, None) => true,
            Target::Event(Some(x), None) => *x == sid,
            Target::Event(Some(x), Some(y)) => *x == sid && *y == e.event_id,
            _ => false,
        })?;
        Ok(ds)
    }

    fn eit_event(
        &self,
        i: usize,
        e: &Event,
        running_status: u8,
        o: &TableOpts,
    ) -> Result<EitEvent> {
        Ok(EitEvent {
            event_id: e.event_id,
            start_time: jst::to_mjd_time(e.start)?,
            duration: jst::bcd_duration(e.duration)?,
            running_status: running_status & 7,
            free_ca_mode: self.cfg.services[i].free_ca_mode,
            descriptors: self.event_descriptors(i, e, o)?,
        })
    }

    /// Present and following events of service `i` at `now` (B10 Part 2
    /// Annex 1.4.1 rules a–h).
    pub fn present_following(&self, i: usize, now: JstSeconds) -> (Option<&Event>, Option<&Event>) {
        let ev = &self.events[i];
        let present = ev
            .iter()
            .find(|e| e.start <= now && now < e.start + i64::from(e.duration));
        let after = present
            .map(|p| p.start + i64::from(p.duration))
            .unwrap_or(now);
        let following = ev
            .iter()
            .find(|e| e.start >= after && present.is_none_or(|p| p.event_id != e.event_id));
        (present, following)
    }

    fn eit_pf(&self, o: &TableOpts, v: u8, now: JstSeconds) -> Result<Vec<Vec<u8>>> {
        let c = &self.cfg;
        let mut idx: Vec<usize> = (0..c.services.len())
            .filter(|&i| c.services[i].eit_pf)
            .collect();
        idx.sort_by_key(|&i| c.services[i].service_id);
        let mut secs = Vec::new();
        for i in idx {
            let (p, f) = self.present_following(i, now);
            for (sn, ev, rs) in [
                (0u8, p, c.eit.present_running_status),
                (1u8, f, c.eit.following_running_status),
            ] {
                let mut h = si_header(0x4E, c.services[i].service_id, v);
                h.section_number = sn;
                h.last_section_number = 1;
                let events = match ev {
                    Some(e) => vec![self.eit_event(i, e, rs, o)?],
                    None => Vec::new(),
                };
                secs.push(Long {
                    header: h,
                    body: EitBody {
                        transport_stream_id: c.transport.transport_stream_id,
                        original_network_id: c.original_network_id(),
                        segment_last_section_number: 1,
                        last_table_id: 0x4E,
                        events,
                    },
                });
            }
        }
        encode_long(secs, o)
    }

    /// EIT schedule per B10 Part 2 Annex 1.4.2.1: 0x50 + 4-day blocks,
    /// 32 segments of 3 h × 8 sections, empty segments as one empty section.
    fn eit_schedule(&self, o: &TableOpts, v: u8, now: JstSeconds) -> Result<Vec<Vec<u8>>> {
        let c = &self.cfg;
        let t0 = if c.eit_schedule.reference == crate::config::EitScheduleReference::Month {
            let d = jst::to_datetime(now)
                .ok_or_else(|| MuxError::Config("clock out of range".into()))?;
            let first = isdb_si::time::DateTime {
                date: isdb_si::time::Date { day: 1, ..d.date },
                hour: 0,
                minute: 0,
                second: 0,
            };
            jst::from_datetime(first)
                .ok_or_else(|| MuxError::Config("clock out of range".into()))?
        } else {
            now - now.rem_euclid(86_400)
        };
        let mut idx: Vec<usize> = (0..c.services.len())
            .filter(|&i| self.has_schedule(i))
            .collect();
        idx.sort_by_key(|&i| c.services[i].service_id);
        let mut out = Vec::new();
        for i in idx {
            // (table index, segment) → events
            let mut segs: std::collections::BTreeMap<(u8, u8), Vec<&Event>> = Default::default();
            for e in &self.events[i] {
                if e.start + i64::from(e.duration) <= now || e.start < t0 {
                    continue; // past events may be omitted (rule m)
                }
                let h3 = (e.start - t0) / (3 * 3600);
                // 16 tables of 32 three-hour segments; later events are dropped
                let Some(table) = u8::try_from(h3 / 32).ok().filter(|&t| t <= 15) else {
                    continue;
                };
                segs.entry((table, (h3 % 32) as u8)).or_default().push(e);
            }
            let Some(&(last_table, _)) = segs.keys().next_back() else {
                continue;
            };
            for table in 0..=last_table {
                let tid = 0x50 + table;
                let last_seg = segs.keys().filter(|k| k.0 == table).map(|k| k.1).max();
                let Some(last_seg) = last_seg else { continue };
                // sections per segment (one section per segment unless it overflows 4096 bytes)
                let mut per_seg: Vec<Vec<Vec<EitEvent>>> = Vec::new();
                for seg in 0..=last_seg {
                    let mut sections: Vec<Vec<EitEvent>> = vec![Vec::new()];
                    let mut size = 0usize;
                    for e in segs.get(&(table, seg)).map(|v| v.as_slice()).unwrap_or(&[]) {
                        let ev = self.eit_event(i, e, 0, o)?;
                        let sz = 12 + isdb_si::descriptor::loop_bytes(&ev.descriptors)?.len();
                        if size + sz > 4000 && !sections.last().unwrap().is_empty() {
                            if sections.len() == 8 {
                                return Err(MuxError::Config(
                                    "EIT schedule segment needs more than 8 sections".into(),
                                ));
                            }
                            sections.push(Vec::new());
                            size = 0;
                        }
                        size += sz;
                        sections.last_mut().unwrap().push(ev);
                    }
                    per_seg.push(sections);
                }
                // segment (<= 31) * 8 + section within it (< 8) fits a byte
                let section_number = |n: usize| {
                    u8::try_from(n).map_err(|_| {
                        MuxError::Config(format!("EIT schedule section number {n} exceeds 255"))
                    })
                };
                let last_section = section_number(
                    usize::from(last_seg) * 8 + per_seg[usize::from(last_seg)].len() - 1,
                )?;
                for (seg, sections) in per_seg.into_iter().enumerate() {
                    let n = sections.len();
                    for (k, events) in sections.into_iter().enumerate() {
                        let sn = section_number(seg * 8 + k)?;
                        let mut h = si_header(tid, c.services[i].service_id, v);
                        h.section_number = sn;
                        h.last_section_number = last_section;
                        out.push(Long {
                            header: h,
                            body: EitBody {
                                transport_stream_id: c.transport.transport_stream_id,
                                original_network_id: c.original_network_id(),
                                segment_last_section_number: section_number(seg * 8 + n - 1)?,
                                last_table_id: 0x50 + last_table,
                                events,
                            },
                        });
                    }
                }
            }
        }
        encode_long(out, o)
    }

    fn tot(&self, o: &TableOpts, now: JstSeconds) -> Result<Vec<Vec<u8>>> {
        let mut ds = Vec::new();
        if !self.cfg.tot.local_time_offset.is_empty() {
            let mut entries = Vec::new();
            for l in &self.cfg.tot.local_time_offset {
                let cc = l.country_code.as_bytes();
                if cc.len() != 3 {
                    return Err(MuxError::Config(format!(
                        "country_code {:?}",
                        l.country_code
                    )));
                }
                let (h1, m1) = jst::parse_hm(&l.offset)?;
                let (h2, m2) = jst::parse_hm(&l.next_offset)?;
                entries.push(LocalTimeOffsetEntry {
                    country_code: [cc[0], cc[1], cc[2]],
                    country_region_id: l.country_region_id & 0x3F,
                    reserved: true,
                    local_time_offset_polarity: l.polarity,
                    local_time_offset: BcdOffset::from_hm(h1, m1)?,
                    time_of_change: jst::to_mjd_time(jst::parse(&l.time_of_change)?)?,
                    next_time_offset: BcdOffset::from_hm(h2, m2)?,
                });
            }
            ds.push(Descriptor::LocalTimeOffset(LocalTimeOffset { entries }));
        }
        add_into(&mut ds, &o.add_descriptors, |t| *t == Target::Main)?;
        encode_short(
            Short {
                header: ShortHeader {
                    table_id: 0x73,
                    private_indicator: true,
                    reserved1: 0b11,
                },
                body: TotBody {
                    jst_time: jst::to_mjd_time(now)?,
                    reserved: 0xF,
                    descriptors: ds,
                },
            },
            o,
        )
    }

    fn bit(&self, o: &TableOpts, v: u8) -> Result<Vec<Vec<u8>>> {
        let b = &self.cfg.bit;
        let mut first = raw_list(&b.first_descriptors)?;
        add_into(&mut first, &o.add_descriptors, |t| *t == Target::First)?;
        let mut broadcasters = Vec::new();
        for bc in &b.broadcasters {
            let mut ds = Vec::new();
            if let Some(n) = &bc.name {
                ds.push(Descriptor::BroadcasterName(BroadcasterName {
                    name: text(n)?,
                }));
            }
            if let Some(t) = &bc.terrestrial {
                ds.push(Descriptor::ExtendedBroadcaster(ExtendedBroadcaster {
                    reserved_future_use: 0xF,
                    body: ExtendedBroadcasterBody::TerrestrialTv(TerrestrialBroadcaster {
                        terrestrial_broadcaster_id: t.terrestrial_broadcaster_id,
                        affiliation_ids: t.affiliation_ids.clone(),
                        broadcasters: t
                            .broadcasters
                            .iter()
                            .map(|&(onid, id)| NetworkBroadcaster {
                                original_network_id: onid,
                                broadcaster_id: id,
                            })
                            .collect(),
                        private_data: Vec::new(),
                    }),
                }));
            }
            ds.extend(raw_list(&bc.descriptors)?);
            let id = bc.broadcaster_id;
            add_into(&mut ds, &o.add_descriptors, |t| {
                matches!(t, Target::Broadcaster(None)) || *t == Target::Broadcaster(Some(id))
            })?;
            broadcasters.push(BitBroadcaster {
                broadcaster_id: id,
                reserved_future_use: 0xF,
                descriptors: ds,
            });
        }
        let body = BitBody {
            reserved_future_use: 0b111,
            broadcast_view_propriety: b.broadcast_view_propriety,
            first_descriptors: first,
            broadcasters,
        };
        encode_long(
            vec![Long {
                header: si_header(0xC4, self.cfg.original_network_id(), v),
                body,
            }],
            o,
        )
    }
}

/// stream_type of a configured stream (H.222.0 Table 2-34: 0x02 = ITU-T
/// H.262 video, 0x0F = ISO/IEC 13818-7 audio with ADTS transport syntax).
pub fn stream_type(s: &StreamCfg) -> u8 {
    s.stream_type.unwrap_or(match s.kind {
        StreamKind::Mpeg2Video => 0x02,
        StreamKind::AacAdts => 0x0F,
        StreamKind::Declared | StreamKind::TsProgram => 0x06,
        // B24 Vol. 3 Table 4-1: data carousel "0x0B, 0x0D" (0x0D when the
        // stream also carries other DSM-CC data such as event messages)
        StreamKind::BmlCarousel => 0x0D,
    })
}

/// stream_type of a dedicated AIT component: ISO/IEC 13818-1
/// private_sections (H.222.0 Table 2-34). B24 Vol. 4 does not name one;
/// 0x05 is the H.222.0 value for a private section stream.
pub const AIT_STREAM_TYPE: u8 = 0x05;

/// additional_arib_bxml_info() of a `bml-carousel` (B24 Vol. 2 Table 9-12
/// with Vol. 3 Annex C.1).
pub fn additional_bxml_info(bml: &BmlCfg, sid: u16) -> Result<Vec<u8>> {
    let what = format!("service {sid:#06x} bml");
    let versions = carousel::bml_versions(bml, &what)?;
    let entry = if bml.entry_point {
        BxmlEntry::Entry(BxmlEntryPoint {
            auto_start: bml.auto_start,
            document_resolution: bml.document_resolution.code(),
            use_xml: false,
            independent: bml.independent,
            style_for_tv: bml.style_for_tv,
            reserved: bml.reserved_future_use & 0xF,
            versions,
        })
    } else {
        BxmlEntry::NotEntry { reserved: 0x1F }
    };
    Ok(AdditionalAribBxmlInfo {
        transmission_format: TRANSMISSION_FORMAT_CAROUSEL,
        entry,
        carousel: Some(AdditionalAribCarouselInfo {
            data_event_id: bml.pmt_data_event_id & 0xF,
            event_section: bml.event_section,
            reserved: 0b111,
        }),
        ondemand_retrieval: bml.ondemand_retrieval,
        file_storable: false,
        reserved: 0x3F,
    }
    .encode()?)
}

/// arib_bxml_info() selector of the EIT data content descriptor (B24
/// Vol. 2 Table 9-13 with Vol. 3 Annex C.2.1).
pub fn bxml_info(bml: &BmlCfg, carousels: Vec<CarouselInfo>, sid: u16) -> Result<Vec<u8>> {
    let what = format!("service {sid:#06x} bml");
    Ok(AribBxmlInfo {
        transmission_format: TRANSMISSION_FORMAT_CAROUSEL,
        reserved_1: true,
        auto_start: bml.auto_start,
        document_resolution: bml.document_resolution.code(),
        use_xml: false,
        independent: bml.independent,
        content_id: None,
        associated_contents: false,
        reserved_2: true,
        style_for_tv: bml.style_for_tv,
        update: false,
        language: *b"jpn",
        versions: carousel::bml_versions(bml, &what)?,
        carousels,
        ondemand_retrieval: bml.ondemand_retrieval,
        file_storable: false,
        reserved_3: 0x3F,
    }
    .encode()?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spec_b10_table_6_62_6_63() {
        assert_eq!(guard_interval_code(GuardInterval::G1_32), 0b00);
        assert_eq!(guard_interval_code(GuardInterval::G1_16), 0b01);
        assert_eq!(guard_interval_code(GuardInterval::G1_8), 0b10);
        assert_eq!(guard_interval_code(GuardInterval::G1_4), 0b11);
        assert_eq!(transmission_mode_code(Mode::M1), 0b00);
        assert_eq!(transmission_mode_code(Mode::M2), 0b01);
        assert_eq!(transmission_mode_code(Mode::M3), 0b10);
    }

    #[test]
    fn frequency_codes() {
        // ch 13: 473 + 1/7 MHz = 3312/7 MHz; ch 62: 767 + 1/7 MHz
        assert_eq!(channel_frequency_code(13).unwrap(), 3312);
        assert_eq!(channel_frequency_code(62).unwrap(), 767 * 7 + 1);
        assert!(channel_frequency_code(12).is_err());
    }

    #[test]
    fn spec_b31_part2_table_3_2() {
        use CodeRate::*;
        use Modulation::*;
        let order = [
            (Dqpsk, R1_2),
            (Qpsk, R1_2),
            (Dqpsk, R2_3),
            (Qpsk, R2_3),
            (Dqpsk, R3_4),
            (Qpsk, R3_4),
            (Dqpsk, R5_6),
            (Qpsk, R5_6),
            (Dqpsk, R7_8),
            (Qpsk, R7_8),
            (Qam16, R1_2),
            (Qam16, R2_3),
            (Qam16, R3_4),
            (Qam16, R5_6),
            (Qam16, R7_8),
            (Qam64, R1_2),
            (Qam64, R2_3),
            (Qam64, R3_4),
            (Qam64, R5_6),
            (Qam64, R7_8),
        ];
        for (i, (m, r)) in order.into_iter().enumerate() {
            assert_eq!(robustness_rank(m, r), u32::try_from(i).unwrap() + 1);
        }
    }

    #[test]
    fn hex_and_targets() {
        assert_eq!(
            parse_hex("02 b0:0d_ff").unwrap(),
            vec![0x02, 0xB0, 0x0D, 0xFF]
        );
        assert!(parse_hex("abc").is_err());
        assert_eq!(
            parse_target("es:0x0111"),
            Some(Target::Es(Pid::new(0x111).unwrap()))
        );
        assert_eq!(parse_target("es:0x2000"), None);
        assert_eq!(
            parse_target("event:1024:2"),
            Some(Target::Event(Some(1024), Some(2)))
        );
        assert_eq!(parse_target("bogus"), None);
    }
}
