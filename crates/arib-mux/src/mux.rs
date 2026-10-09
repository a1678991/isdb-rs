//! The CBR, OFDM-frame-paced multiplexer.
//!
//! Timing model: the TS a 13-segment receiver reproduces (ARIB STD-B31
//! §3.2.2, Fig. 3-4) reads one 204-byte transmission TSP every 408
//! clocks of the 512/63 MHz sample clock, in the slot order of the
//! multiplex frame ([`MultiplexPattern`]). Slot *j* of OFDM frame *k*
//! therefore starts at clock `k·samples_per_frame + 408·j`; converted to
//! the 27 MHz system clock (× 27·63/512 = × 1701/512) this is the arrival
//! time of the packet at the T-STD (H.222.0 §2.4.2.2), so PCR values and
//! the PTS/DTS schedule are consistent with the transmitted rate by
//! construction. PCR = time of the byte holding the last bit of
//! program_clock_reference_base (byte 10 of the packet, i.e. 20 clocks
//! after the slot start at 2 clocks per byte).
//!
//! Per slot of a layer the scheduler emits, in priority order: the PCR
//! packet of a service whose PCR position this is (B31 Part 2 §3.2.4: a
//! fixed number of PCR packets per frame at equal intervals), a pending
//! section packet (earliest deadline first, B10 §5.1.4 rate limit), a
//! PES packet whose T-STD buffers can take it (earliest DTS first), or a
//! null packet.

use std::collections::VecDeque;
use std::path::Path;

use arib_si::values::{CodedValue, DigitalRecordingControl};
use arib_ts::cc::Counter;
use arib_ts::packet::{AdaptationFields, PAYLOAD_CAPACITY, Packet as TsPacket};
use arib_ts::packetize::{Packing, packetize_sections};
use arib_ts::pcr::Pcr;
use arib_ts::pes::{PesHeader, PesPacket, PtsDts};
use isdbt_core::Params;
use isdbt_fec::MultiplexPattern;

use crate::audio::AdtsEs;
use crate::config::{ClockSource, Config, StreamKind};
use crate::error::{MuxError, Result};
use crate::jst::{self, JstSeconds};
use crate::relay::{self, Relay, RelayStats};
use crate::signalling::RecordingControlCfg;
use crate::si::{self, SiBuilder, StreamInfo, TableKind};
use crate::tstd::{Tstd, TstdParams, TstdViolations};
use crate::video::VideoEs;

/// A 188-byte transport packet.
pub type Packet = [u8; 188];

/// System clock ticks per second.
pub const HZ: u64 = 27_000_000;
/// Model-receiver clocks per TSP period (B31 §3.2.2.2).
const SLOT_CLOCKS: u64 = 408;
/// Byte holding the last bit of PCR base in a PCR-carrying packet × 2 clocks/byte.
const PCR_BYTE_CLOCKS: u64 = 20;

/// H.222.0 §2.4.3.2 null packet.
pub fn null_packet() -> Packet {
    let mut p = [0xFFu8; 188];
    p[..4].copy_from_slice(&[0x47, 0x1F, 0xFF, 0x10]);
    p
}

/// Convert model-receiver clocks (512/63 MHz) to 27 MHz ticks.
pub fn clocks_to_ticks(c: u64) -> u64 {
    ((c as u128 * 1701) / 512) as u64
}

struct SectionStream {
    kind: TableKind,
    pid: u16,
    layer: usize,
    interval: u64,
    sched: u64,
    next_due: u64,
    last_start: Option<u64>,
    cc: Counter,
    queue: VecDeque<(Packet, bool)>,
    version: u8,
    last: Option<Vec<Vec<u8>>>,
    window: VecDeque<u64>,
}

enum Media {
    Video {
        es: Box<VideoEs>,
        force_vbv: bool,
        broken_link: bool,
    },
    Audio {
        es: Box<AdtsEs>,
        per_pes: usize,
        sample_starts: Vec<u64>,
    },
}

struct PreparedPes {
    bytes: Vec<u8>,
    first_decode: u64,
    /// (decode time, bytes) per access unit, PES header attributed to the first.
    aus: Vec<(u64, usize)>,
    rai: bool,
}

struct EsStream {
    service: usize,
    pid: u16,
    layer: usize,
    media: Media,
    cc: Counter,
    tstd: Tstd,
    /// Next access unit (video AU / audio frame) within the pass.
    next: usize,
    pass: u64,
    period54: u64,
    base: u64,
    current: Option<(Vec<u8>, usize, bool, u64)>, // bytes, pos, rai pending, first decode
    ahead: Option<PreparedPes>,
    pes_sent: u64,
    late: u64,
    /// This PID is its service's PCR_PID.
    is_pcr_pid: bool,
}

struct PcrStream {
    pid: u16,
    layer: usize,
    /// Layer-local packet indices carrying PCR in each frame.
    positions: Vec<usize>,
    /// Index of the ES stream carrying the PCR, if PCR_PID is an ES PID.
    es: Option<usize>,
    cc: Counter,
    count: u64,
}

/// Running statistics.
#[derive(Debug, Clone, Default)]
pub struct MuxStats {
    /// Frames produced.
    pub frames: u64,
    /// Packets per layer (including nulls).
    pub layer_packets: [u64; 3],
    /// Null packets per layer.
    pub layer_nulls: [u64; 3],
    /// PES packets sent.
    pub pes: u64,
    /// PES packets whose last byte was not in the T-STD by its decoding time.
    pub late_pes: u64,
    /// Section (re)generations per table name.
    pub tables: std::collections::BTreeMap<String, u64>,
    /// Version increments.
    pub version_changes: u64,
    /// Internal errors turned into null packets.
    pub errors: Vec<String>,
}

/// The multiplexer.
pub struct Multiplexer {
    params: Params,
    pattern: MultiplexPattern,
    si: SiBuilder,
    sections: Vec<SectionStream>,
    es: Vec<EsStream>,
    pcr: Vec<PcrStream>,
    frame: u64,
    start_pcr: u64,
    clock_start: JstSeconds,
    lead: u64,
    si_window: u64,
    si_max_packets: usize,
    stats: MuxStats,
    warnings: Vec<String>,
    notes: Vec<String>,
    relays: Vec<Relay>,
}

/// Delay from the first multiplex slot to the first relayed packet: the
/// PAT/PMT/PCR of the first frame go out before it.
const RELAY_START_MS: u32 = 100;

fn ms(x: u32) -> u64 {
    x as u64 * HZ / 1000
}

fn to90k(t: u64) -> u64 {
    (t / 300) % (1u64 << 33)
}

impl Multiplexer {
    /// Build a multiplexer. Relative `source` paths are resolved against
    /// `base_dir` (normally the config file's directory).
    pub fn new(cfg: &Config, params: &Params, base_dir: &Path) -> Result<Self> {
        cfg.validate()?;
        let mut warnings = Vec::new();
        let pattern = MultiplexPattern::new(params);
        let start_pcr = cfg.clock.start_pcr;
        let clock_start = match cfg.clock.source {
            ClockSource::System => match &cfg.clock.start {
                Some(s) => jst::parse(s)?,
                None => jst::now(),
            },
            ClockSource::Fixed => jst::parse(cfg.clock.start.as_deref().ok_or_else(|| {
                MuxError::Config("clock.source = fixed needs clock.start".into())
            })?)?,
        };
        if !(0x7880..=0x7FE8).contains(&cfg.network.network_id) {
            warnings.push(format!(
                "network_id {:#06x} is outside the terrestrial TV range 0x7880–0x7FE8 (B10 Annex N)",
                cfg.network.network_id
            ));
        }
        if cfg.network.channels.is_empty() && cfg.network.frequencies.is_empty() {
            return Err(MuxError::Config(
                "network: give at least one channel or frequency".into(),
            ));
        }
        for s in &cfg.services {
            if params.layers[s.layer.index()].is_none() {
                return Err(MuxError::Config(format!(
                    "service {:#06x} is on layer {:?}, which is not active",
                    s.service_id, s.layer
                )));
            }
        }
        let t0 = start_pcr;
        let media_base = t0 + ms(cfg.timing.av_delay_ms);
        let lead = ms(cfg.timing.max_std_delay_ms).saturating_sub(ms(50));

        // ---- ts-program sources: probe each file and expand the entry into
        // one stream per relayed elementary stream (crate::relay).
        let mut notes = Vec::new();
        let mut plans: Vec<(relay::Probe, usize, crate::config::StreamCfg)> = Vec::new();
        let mut expanded = cfg.clone();
        for s in expanded.services.iter_mut() {
            let mut streams = Vec::new();
            let mut events = None;
            // copy restriction of the relayed source(s), for
            // recording_control = "source"
            let mut source_cc: Vec<Option<relay::SourceCopyControl>> = Vec::new();
            for st in std::mem::take(&mut s.streams) {
                if st.kind != StreamKind::TsProgram || st.relay_es.is_some() {
                    streams.push(st);
                    continue;
                }
                let src = st.source.as_ref().ok_or_else(|| {
                    MuxError::Config(format!(
                        "service {:#06x}: ts-program needs a source file",
                        s.service_id
                    ))
                })?;
                let probe = relay::probe(&base_dir.join(src), &st)?;
                notes.push(probe.report());
                source_cc.push(probe.source_copy_control);
                let ri = plans.len();
                for (ei, e) in probe.es.iter().enumerate() {
                    streams.push(relay::expanded_stream(&st, e, ri, ei)?);
                }
                if st.eit_from_source {
                    let ev = relay::source_events(&probe);
                    if ev.is_empty() {
                        warnings.push(format!(
                            "service {:#06x}: eit_from_source: the probed part of {} has no EIT p/f of program {:#06x}; the configured events are used",
                            s.service_id,
                            probe.path.display(),
                            probe.program
                        ));
                    } else {
                        notes.push(format!(
                            "service {:#06x}: EIT p/f from the source: {}",
                            s.service_id,
                            ev.iter()
                                .map(|e| format!(
                                    "{:#06x} \"{}\" {} at {:+} s",
                                    e.event_id,
                                    e.name,
                                    e.duration,
                                    e.start_offset_s.unwrap_or(0)
                                ))
                                .collect::<Vec<_>>()
                                .join(", ")
                        ));
                        events = Some(ev);
                    }
                }
                let layer = st.layer.unwrap_or(s.layer).index();
                plans.push((probe, layer, st));
            }
            s.streams = streams;
            if let Some(ev) = events {
                s.events = ev;
            }
            if let Some(cc) = s.copy_control.as_mut()
                && cc.recording_control == RecordingControlCfg::Source
                && !source_cc.is_empty()
            {
                // the most restrictive of the relayed sources; a source
                // without a digital_copy_control_descriptor signals no
                // restriction (copy-free)
                let v = source_cc
                    .iter()
                    .map(|c| {
                        c.map_or(DigitalRecordingControl::CopyFree, |c| c.recording_control)
                    })
                    .max_by_key(|v| v.code())
                    .unwrap_or(DigitalRecordingControl::CopyFree);
                cc.recording_control = RecordingControlCfg::Value(v);
                notes.push(format!(
                    "service {:#06x}: copy_control recording_control = \"source\" -> \"{}\" ({})",
                    s.service_id,
                    v.name(),
                    source_cc
                        .iter()
                        .map(|c| c.map_or_else(
                            || "the source signals no digital_copy_control_descriptor".to_string(),
                            |c| c.describe()
                        ))
                        .collect::<Vec<_>>()
                        .join("; ")
                ));
            }
            if s.streams
                .iter()
                .any(|st| st.relay_es.is_some() && st.pid == s.pcr_pid)
            {
                return Err(MuxError::Config(format!(
                    "service {:#06x}: pcr_pid {:#06x} is a relayed elementary stream; the relayed packets keep their payload, so the PCR needs its own PID (e.g. pcr_pid = 0x01FF)",
                    s.service_id, s.pcr_pid
                )));
            }
        }
        if !plans.is_empty() {
            expanded.validate()?;
        }
        let cfg = &expanded;

        // ---- media
        let mut media_info: Vec<Vec<StreamInfo>> = Vec::new();
        let mut es: Vec<EsStream> = Vec::new();
        let mut pid_layer: std::collections::HashMap<u16, (usize, String, bool)> =
            Default::default();
        // `section`: section streams may share a PID with other section
        // streams (one continuity counter); nothing else may share.
        let mut claim = |pid: u16, layer: usize, what: String, section: bool| -> Result<()> {
            if pid > 0x1FFE {
                return Err(MuxError::Config(format!(
                    "{what}: PID {pid:#06x} out of range"
                )));
            }
            if let Some((l, w, sec)) = pid_layer.get(&pid) {
                if *l != layer {
                    return Err(MuxError::Config(format!(
                        "{what}: PID {pid:#06x} already used by {w} on another layer (would need B31 Part 2 §3.2.2 duplicate packets)"
                    )));
                }
                if !(section && *sec) {
                    return Err(MuxError::Config(format!(
                        "{what}: PID {pid:#06x} already used by {w}"
                    )));
                }
            }
            pid_layer.insert(pid, (layer, what, section));
            Ok(())
        };
        for (si_idx, s) in cfg.services.iter().enumerate() {
            let mut infos = Vec::new();
            let mut svc_streams: Vec<(usize, Media, u64, usize)> = Vec::new();
            for st in &s.streams {
                let layer = st.layer.unwrap_or(s.layer).index();
                if params.layers[layer].is_none() {
                    return Err(MuxError::Config(format!(
                        "stream {:#06x} on inactive layer {}",
                        st.pid,
                        ["A", "B", "C"][layer]
                    )));
                }
                if !(0x0010..=0x1FFE).contains(&st.pid) {
                    return Err(MuxError::Config(format!(
                        "stream PID {:#06x} outside 0x0010–0x1FFE (H.222.0 Table 2-3)",
                        st.pid
                    )));
                }
                claim(
                    st.pid,
                    layer,
                    format!("service {:#06x} stream", s.service_id),
                    false,
                )?;
                match st.kind {
                    StreamKind::TsProgram => {
                        let (ri, ei) = st.relay_es.ok_or_else(|| {
                            MuxError::Config("ts-program stream not expanded".into())
                        })?;
                        infos.push(plans[ri].0.es[ei].info.clone());
                    }
                    StreamKind::Declared => {
                        if st.stream_type.is_none() {
                            return Err(MuxError::Config(format!(
                                "declared stream {:#06x} needs stream_type",
                                st.pid
                            )));
                        }
                        infos.push(StreamInfo::None);
                    }
                    StreamKind::Mpeg2Video | StreamKind::AacAdts => {
                        let src = st.source.as_ref().ok_or_else(|| {
                            MuxError::Config(format!("stream {:#06x} needs a source file", st.pid))
                        })?;
                        let path = base_dir.join(src);
                        let p = path.display().to_string();
                        let data = std::fs::read(&path).map_err(|e| MuxError::Media {
                            path: p.clone(),
                            msg: e.to_string(),
                        })?;
                        if st.kind == StreamKind::Mpeg2Video {
                            let v = VideoEs::parse(data, &p)?;
                            infos.push(StreamInfo::Video {
                                seq: v.seq,
                                has_sequence_end: v.has_sequence_end,
                            });
                            let dur = v.duration54;
                            svc_streams.push((
                                st.pid as usize,
                                Media::Video {
                                    es: Box::new(v),
                                    force_vbv: st.force_vbv_delay_ffff,
                                    broken_link: st.mark_broken_link,
                                },
                                dur,
                                layer,
                            ));
                        } else {
                            let (a, w) = AdtsEs::load(data, &p, st.adts_compliance)?;
                            warnings.extend(w);
                            infos.push(StreamInfo::Audio {
                                sampling_rate: a.sampling_rate_code(),
                                audio_mode: a.audio_mode(),
                                quality: a.quality_mode(),
                            });
                            let fs = a.sampling_frequency as u64;
                            let mut acc = 0u64;
                            let mut starts = Vec::with_capacity(a.frames.len());
                            for f in &a.frames {
                                starts.push(acc);
                                acc += f.samples as u64;
                            }
                            let dur = (a.total_samples * 54_000_000).div_ceil(fs);
                            svc_streams.push((
                                st.pid as usize,
                                Media::Audio {
                                    es: Box::new(a),
                                    per_pes: st.frames_per_pes.max(1) as usize,
                                    sample_starts: starts,
                                },
                                dur,
                                layer,
                            ));
                        }
                    }
                }
            }
            let period54 = svc_streams.iter().map(|s| s.2).max().unwrap_or(0);
            // Video PTS include one frame of reorder delay; start audio at the
            // first displayed picture so that A/V stay aligned.
            let av_offset54 = svc_streams
                .iter()
                .filter_map(|s| match &s.1 {
                    Media::Video { es, .. } => Some(es.reorder54),
                    _ => None,
                })
                .max()
                .unwrap_or(0);
            for (pid, m, _, layer) in svc_streams {
                let base = match &m {
                    Media::Audio { .. } => media_base + av_offset54 / 2,
                    Media::Video { .. } => media_base,
                };
                let tp = match &m {
                    Media::Video { es, .. } => {
                        let seq = es.seq;
                        let rmax = seq.rmax() as f64;
                        let mbs = if seq.high_level() {
                            0.004 * rmax / 8.0 + rmax / 750.0 / 8.0
                        } else {
                            // MBSn = BSmux + BSoh + VBVmax − vbv_buffer_size; VBVmax
                            // approximated by vbv_buffer_size (conservative).
                            0.004 * rmax / 8.0 + rmax / 750.0 / 8.0
                        };
                        let rbx = if seq.high_level() {
                            (1.05 * seq.bit_rate as f64).min(rmax)
                        } else {
                            rmax
                        };
                        TstdParams {
                            rx: 1.2 * rmax,
                            mb: Some((mbs, rbx)),
                            main: seq.vbv_buffer_bits as f64 / 8.0,
                        }
                    }
                    Media::Audio { es, .. } => TstdParams {
                        rx: es.rx() as f64,
                        mb: None,
                        main: es.bs() as f64,
                    },
                };
                es.push(EsStream {
                    service: si_idx,
                    pid: pid as u16,
                    layer,
                    media: m,
                    cc: Counter::new(0),
                    tstd: Tstd::new(tp, t0),
                    next: 0,
                    pass: 0,
                    period54,
                    base,
                    current: None,
                    ahead: None,
                    pes_sent: 0,
                    late: 0,
                    is_pcr_pid: false,
                });
            }
            media_info.push(infos);
        }

        // ---- SI
        let events = si::resolve_events(cfg, clock_start)?;
        let si = SiBuilder {
            cfg: cfg.clone(),
            params: params.clone(),
            media: media_info,
            events,
        };
        let mut kinds = vec![
            TableKind::Pat,
            TableKind::Cat,
            TableKind::Nit,
            TableKind::Sdt,
            TableKind::EitPf,
            TableKind::EitSchedule,
            TableKind::Tot,
            TableKind::Tdt,
            TableKind::Bit,
        ];
        for i in 0..cfg.services.len() {
            kinds.push(TableKind::Pmt(i));
        }
        for i in 0..cfg.raw_tables.len() {
            kinds.push(TableKind::Raw(i));
        }
        let margin = cfg.timing.interval_margin.clamp(0.1, 1.0);
        let frame_ticks = params.frame_ticks_27mhz();
        let mut sections = Vec::new();
        let mut per_layer_sections = [0u64; 3];
        // Estimated section rate per layer (bit/s) for the relay capacity check.
        let mut section_bps = [0f64; 3];
        for kind in kinds {
            let o = si.opts(kind);
            if !o.enabled {
                continue;
            }
            si::validate_targets(kind, &o)?;
            let pid = o.pid.unwrap_or_else(|| si.default_pid(kind));
            let layer = o
                .layer
                .map(|l| l.index())
                .unwrap_or_else(|| si.default_layer(kind));
            if params.layers[layer].is_none() {
                return Err(MuxError::Config(format!(
                    "{} on inactive layer {}",
                    kind.name(),
                    layer
                )));
            }
            // EIT p/f and schedule (and TOT/TDT) may share a PID: one CC sequence.
            claim(pid, layer, kind.name(), true)?;
            let interval = ms(o
                .interval_ms
                .unwrap_or_else(|| SiBuilder::default_interval_ms(kind)));
            // Build once now so configuration errors surface at start-up.
            let secs = si.build(kind, clock_start, o.version)?;
            let pkts: usize = secs.iter().map(|x| (x.len() + 1).div_ceil(184)).sum();
            section_bps[layer] += pkts as f64 * 1504.0 * HZ as f64 / (interval as f64 * margin);
            // Stagger the first transmissions two layer slots apart.
            let layer_slot =
                frame_ticks / pattern.count(isdbt_core::Layer::ALL[layer]).max(1) as u64;
            let n = per_layer_sections[layer];
            per_layer_sections[layer] += 1;
            sections.push(SectionStream {
                kind,
                pid,
                layer,
                interval,
                sched: ((interval as f64) * margin) as u64,
                next_due: t0 + n * 2 * layer_slot,
                last_start: None,
                cc: Counter::new(0),
                queue: VecDeque::new(),
                version: o.version & 0x1F,
                last: None,
                window: VecDeque::new(),
            });
        }

        // ---- PCR
        let scale = params.mode.scale() as u32;
        let per_frame = cfg.timing.pcr_per_frame.unwrap_or(scale) as usize;
        let mut pcr = Vec::new();
        let mut per_layer_count = [0usize; 3];
        for (i, s) in cfg.services.iter().enumerate() {
            if s.pcr_pid == 0x1FFF {
                continue;
            }
            let es_idx = es.iter().position(|e| e.service == i && e.pid == s.pcr_pid);
            // PCR packets go on the layer of the PCR_PID's stream (declared or
            // media), else on the service's layer.
            let layer = match es_idx {
                Some(ei) => es[ei].layer,
                None => s
                    .streams
                    .iter()
                    .find(|st| st.pid == s.pcr_pid)
                    .and_then(|st| st.layer)
                    .unwrap_or(s.layer)
                    .index(),
            };
            let n = pattern.count(isdbt_core::Layer::ALL[layer]);
            if per_frame == 0 || !n.is_multiple_of(per_frame) {
                return Err(MuxError::Config(format!(
                    "pcr_per_frame {per_frame} does not divide the {n} TSPs per frame of layer {}",
                    ["A", "B", "C"][layer]
                )));
            }
            if frame_ticks / per_frame as u64 > HZ / 10 {
                return Err(MuxError::Config(format!(
                    "PCR interval {} ms exceeds 100 ms (H.222.0 §2.7.2); raise pcr_per_frame",
                    frame_ticks * 1000 / per_frame as u64 / HZ
                )));
            }
            let offset = per_layer_count[layer];
            per_layer_count[layer] += 1;
            let step = n / per_frame;
            if offset >= step {
                return Err(MuxError::Config(
                    "too many PCR services on one layer".into(),
                ));
            }
            let positions = (0..per_frame).map(|q| offset + q * step).collect();
            // PCR_PID may be an elementary PID of the service: a media stream
            // (PCR in its packets) or a declared one (PCR-only packets on it).
            let declared = s
                .streams
                .iter()
                .any(|st| st.pid == s.pcr_pid && st.kind == StreamKind::Declared);
            if es_idx.is_none() && !declared {
                claim(
                    s.pcr_pid,
                    layer,
                    format!("service {:#06x} PCR", s.service_id),
                    false,
                )?;
            }
            if let Some(ei) = es_idx {
                es[ei].is_pcr_pid = true;
            }
            pcr.push(PcrStream {
                pid: s.pcr_pid,
                layer,
                positions,
                es: es_idx,
                cc: Counter::new(0),
                count: 0,
            });
        }

        // ---- relays: capacity check (B31 Table 3-3 rates, isdbt-core) and start
        let frame_s = params.frame_seconds();
        let mut relays = Vec::new();
        for (probe, layer, st) in &plans {
            let id = isdbt_core::Layer::ALL[*layer];
            let pcr_bps: f64 =
                pcr.iter().filter(|p| p.layer == *layer).count() as f64 * per_frame as f64 * 1504.0
                    / frame_s;
            let overhead = pcr_bps + section_bps[*layer];
            let cap = crate::capacity::layer_bps(params, id);
            let need = probe.mean_bps + overhead;
            if need > cap {
                let msg = crate::capacity::over_capacity_message(
                    params,
                    id,
                    need,
                    &format!(
                        "ts-program {} ({:.2} Mbit/s mean over {:.1} s + {:.2} Mbit/s PCR/PSI/SI on layer {id})",
                        probe.path.display(),
                        probe.mean_bps / 1e6,
                        probe.span_s,
                        overhead / 1e6
                    ),
                );
                match st.rate_check {
                    crate::config::RateCheck::Error => {
                        return Err(MuxError::OverCapacity {
                            layer: id,
                            need_bps: need,
                            message: format!(
                                "{msg}\n  (rate_check = \"warn\" relays it anyway; it then falls behind its PCR schedule)"
                            ),
                        });
                    }
                    crate::config::RateCheck::Warn => warnings.push(msg),
                }
            } else if probe.peak_1s_bps + overhead > cap {
                warnings.push(format!(
                    "ts-program {}: 1-s peaks {:.2} Mbit/s (+ {:.2} PCR/SI) exceed layer {id}'s {:.2} Mbit/s; packets are delayed during peaks (lag in the relay status)",
                    probe.path.display(),
                    probe.peak_1s_bps / 1e6,
                    overhead / 1e6,
                    cap / 1e6
                ));
            }
            notes.push(format!(
                "ts-program {}: layer {id} {:.2} Mbit/s = {:.1} % used by relay + PCR/SI ({:.2} + {:.2} Mbit/s)",
                probe.path.display(),
                cap / 1e6,
                100.0 * need / cap,
                probe.mean_bps / 1e6,
                overhead / 1e6
            ));
            relays.push(Relay::start(
                probe,
                &vec![*layer; probe.es.len()],
                st.at_eof,
                t0 + ms(RELAY_START_MS),
                ms(st.max_early_ms),
                lead,
            )?);
        }

        // ---- rough capacity check
        for e in &es {
            if let Media::Video { es: v, .. } = &e.media {
                let cap = params.layers[e.layer]
                    .map(|l| l.tsp_per_frame(params.mode))
                    .unwrap_or(0) as f64
                    * 184.0
                    * 8.0
                    / params.frame_seconds();
                if v.seq.bit_rate as f64 > 0.95 * cap {
                    warnings.push(format!(
                        "video PID {:#06x}: bit_rate {} bit/s close to/above layer payload capacity {:.0} bit/s",
                        e.pid, v.seq.bit_rate, cap
                    ));
                }
            }
        }

        Ok(Self {
            params: params.clone(),
            pattern,
            si,
            sections,
            es,
            pcr,
            frame: 0,
            start_pcr,
            clock_start,
            lead,
            si_window: ms(cfg.timing.si_window_ms),
            si_max_packets: (cfg.timing.si_max_bytes as usize / 188).max(1),
            stats: MuxStats::default(),
            warnings,
            notes,
            relays,
        })
    }

    /// Informational messages from start-up (ts-program probe reports,
    /// layer usage).
    pub fn notes(&self) -> &[String] {
        &self.notes
    }

    /// True when the configuration relays `ts-program` sources and every
    /// one of them has ended (`at_eof = "end"`) and been sent.
    pub fn relays_finished(&self) -> bool {
        !self.relays.is_empty() && self.relays.iter().all(|r| r.finished())
    }

    /// One status line per `ts-program` relay.
    pub fn relay_status(&self) -> Vec<String> {
        self.relays.iter().map(|r| r.status()).collect()
    }

    /// Counters per `ts-program` relay.
    pub fn relay_stats(&self) -> Vec<RelayStats> {
        self.relays.iter().map(|r| r.stats).collect()
    }

    /// Transmission parameters.
    pub fn params(&self) -> &Params {
        &self.params
    }

    /// The multiplex-frame slot pattern.
    pub fn pattern(&self) -> &MultiplexPattern {
        &self.pattern
    }

    /// Non-fatal configuration findings.
    pub fn warnings(&self) -> &[String] {
        &self.warnings
    }

    /// Statistics so far.
    pub fn stats(&self) -> MuxStats {
        let mut s = self.stats.clone();
        s.late_pes = self.es.iter().map(|e| e.late).sum::<u64>()
            + self.relays.iter().map(|r| r.stats.late_pes).sum::<u64>();
        s.pes = self.es.iter().map(|e| e.pes_sent).sum();
        s
    }

    /// T-STD violation counters per elementary PID.
    pub fn tstd_violations(&self) -> Vec<(u16, TstdViolations)> {
        let mut v: Vec<_> = self.es.iter().map(|e| (e.pid, e.tstd.violations)).collect();
        for r in &self.relays {
            for e in &r.es {
                if let Some(t) = &e.tstd {
                    v.push((e.pid, t.violations));
                }
            }
        }
        v
    }

    /// Wall clock (JST) of the stream start.
    pub fn clock_start(&self) -> JstSeconds {
        self.clock_start
    }

    /// The SI builder (tables, events, media facts).
    pub fn si(&self) -> &SiBuilder {
        &self.si
    }

    /// Frames produced so far.
    pub fn frames(&self) -> u64 {
        self.frame
    }

    /// 27 MHz time of slot `j` of frame `k` (arrival of its first byte).
    pub fn slot_time(&self, k: u64, j: usize) -> u64 {
        self.start_pcr
            + clocks_to_ticks(k * self.params.samples_per_frame() as u64 + j as u64 * SLOT_CLOCKS)
    }

    fn jst_at(&self, t: u64) -> JstSeconds {
        self.clock_start + ((t - self.start_pcr) / HZ) as i64
    }

    /// Next OFDM frame: exactly `tsp_per_frame` packets for each active
    /// layer (empty for unused layers).
    pub fn next_frame(&mut self) -> [Vec<Packet>; 3] {
        let slots = self.step();
        let mut out: [Vec<Packet>; 3] = Default::default();
        for (l, p) in slots {
            if let Some(l) = l {
                out[l].push(p);
            }
        }
        out
    }

    /// Next OFDM frame as the multiplex frame (the TS a 13-segment receiver
    /// reproduces, null slots included) and as per-layer packets.
    pub fn next_frame_full(&mut self) -> (Vec<Packet>, [Vec<Packet>; 3]) {
        let slots = self.step();
        let mut out: [Vec<Packet>; 3] = Default::default();
        let mut full = Vec::with_capacity(slots.len());
        for (l, p) in slots {
            if let Some(l) = l {
                out[l].push(p);
            }
            full.push(p);
        }
        (full, out)
    }

    fn step(&mut self) -> Vec<(Option<usize>, Packet)> {
        let k = self.frame;
        let n = self.pattern.len();
        let mut idx = [0usize; 3];
        let mut out = Vec::with_capacity(n);
        let slots: Vec<Option<usize>> = self
            .pattern
            .slots()
            .iter()
            .map(|s| s.map(|l| l.index()))
            .collect();
        for (j, slot) in slots.into_iter().enumerate() {
            match slot {
                None => out.push((None, null_packet())),
                Some(l) => {
                    let t = self.slot_time(k, j);
                    let p = match self.decide(l, idx[l], k, j, t) {
                        Ok(p) => p,
                        Err(e) => {
                            if self.stats.errors.len() < 100 {
                                self.stats.errors.push(e.to_string());
                            }
                            None
                        }
                    };
                    idx[l] += 1;
                    self.stats.layer_packets[l] += 1;
                    let p = p.unwrap_or_else(|| {
                        self.stats.layer_nulls[l] += 1;
                        null_packet()
                    });
                    out.push((Some(l), p));
                }
            }
        }
        self.frame += 1;
        self.stats.frames = self.frame;
        out
    }

    fn decide(
        &mut self,
        layer: usize,
        li: usize,
        k: u64,
        j: usize,
        t: u64,
    ) -> Result<Option<Packet>> {
        // 1. PCR
        if let Some(pi) = self
            .pcr
            .iter()
            .position(|p| p.layer == layer && p.positions.contains(&li))
        {
            let pcr_val = self.start_pcr
                + clocks_to_ticks(
                    k * self.params.samples_per_frame() as u64
                        + j as u64 * SLOT_CLOCKS
                        + PCR_BYTE_CLOCKS,
                );
            return self.emit_pcr(pi, t, pcr_val).map(Some);
        }
        // 2. sections
        self.refresh_sections(layer, t)?;
        if let Some(p) = self.emit_section(layer, t) {
            return Ok(Some(p));
        }
        // 3. PES
        if let Some(p) = self.emit_es(layer, t, None)? {
            return Ok(Some(p));
        }
        // 4. relayed ts-program packets
        self.emit_relay(layer, t)
    }

    /// Emit the relayed packet due earliest on `layer`: a packet is sent at
    /// or after its PCR-derived time (TBn permitting), or up to the relay's
    /// `max_early_ms` before it when its stream's T-STD model accepts it.
    fn emit_relay(&mut self, layer: usize, t: u64) -> Result<Option<Packet>> {
        let mut best: Option<(u64, usize, usize)> = None;
        let layer_bps =
            crate::capacity::layer_bps(&self.params, isdbt_core::Layer::ALL[layer]).max(1.0);
        for ri in 0..self.relays.len() {
            let r = &mut self.relays[ri];
            if !r.es.iter().any(|e| e.layer == layer) {
                continue;
            }
            r.fill(t + r.early);
            if let Some(f) = r.failure.take()
                && self.stats.errors.len() < 100
            {
                self.stats.errors.push(f);
            }
            let r = &mut self.relays[ri];
            let (early, lead) = (r.early, r.lead);
            let mut lag = 0;
            for ei in 0..r.es.len() {
                if r.es[ei].layer != layer {
                    continue;
                }
                let Some(h) = r.head(ei) else { continue };
                let due = h.at <= t;
                if due {
                    lag = lag.max(t - h.at);
                }
                // A due packet goes out (TBn permitting) at the source's
                // pace; only at the start of a pass after a loop seam does it
                // wait while its T-STD buffer would overflow, released once
                // its PES must leave to complete by its decoding time. An
                // early packet needs the full model's consent.
                let ok = match r.es[ei].tstd.as_mut() {
                    Some(ts) => {
                        ts.tb_can_accept(t)
                            && if due {
                                let send_time =
                                    (h.rest as f64 * 8.0 * HZ as f64 / layer_bps) as u64 * 2;
                                let fits = !h.seam
                                    || ts.can_accept(t, h.len)
                                    || h.decode
                                        .is_none_or(|d| d <= t + relay::HOLD_MARGIN + send_time);
                                if !fits {
                                    r.stats.tstd_holds += 1;
                                }
                                fits
                            } else {
                                h.at <= t + early
                                    && h.decode.is_none_or(|d| d <= t + lead)
                                    && ts.can_accept(t, h.len)
                            }
                    }
                    None => due,
                };
                if ok && best.is_none_or(|b| h.at < b.0) {
                    best = Some((h.at, ri, ei));
                }
            }
            r.stats.lag = lag;
        }
        let Some((_, ri, ei)) = best else {
            return Ok(None);
        };
        self.relays[ri].emit(ei, t).map(Some)
    }

    fn emit_pcr(&mut self, pi: usize, t: u64, pcr_val: u64) -> Result<Packet> {
        let pcr = Pcr::from_ticks(pcr_val);
        self.pcr[pi].count += 1;
        if let Some(ei) = self.pcr[pi].es {
            let layer = self.pcr[pi].layer;
            if let Some(p) = self.emit_es(layer, t, Some((ei, pcr)))? {
                return Ok(p);
            }
            // nothing to send on the ES: adaptation-field-only packet on its PID
            let e = &mut self.es[ei];
            let cc = e.cc.next(false);
            e.tstd.push(t, 0);
            return Ok(TsPacket::pcr_only(e.pid, cc, pcr).encode()?);
        }
        let p = &mut self.pcr[pi];
        let cc = p.cc.next(false);
        Ok(TsPacket::pcr_only(p.pid, cc, pcr).encode()?)
    }

    fn refresh_sections(&mut self, layer: usize, t: u64) -> Result<()> {
        for i in 0..self.sections.len() {
            let s = &self.sections[i];
            if s.layer != layer || !s.queue.is_empty() || t < s.next_due {
                continue;
            }
            // Streams sharing a PID must not interleave sections (B10 §5.1.2).
            if self
                .sections
                .iter()
                .any(|x| x.pid == s.pid && !x.queue.is_empty())
            {
                continue;
            }
            let now = self.jst_at(t);
            let kind = s.kind;
            let mut version = s.version;
            let mut secs = self.si.build(kind, now, version)?;
            if let Some(last) = &self.sections[i].last
                && *last != secs
                && !matches!(kind, TableKind::Tot | TableKind::Tdt)
            {
                version = (version + 1) & 0x1F;
                secs = self.si.build(kind, now, version)?;
                self.stats.version_changes += 1;
            }
            *self.stats.tables.entry(kind.name()).or_default() += 1;
            let pid = self.sections[i].pid;
            // CC continuity across streams sharing the PID: use the first stream's counter.
            let owner = self.sections.iter().position(|x| x.pid == pid).unwrap_or(i);
            let mut cc = self.sections[owner].cc;
            let pkts = packetize_sections(pid, &secs, Packing::OnePerPacket, &mut cc)?;
            self.sections[owner].cc = cc;
            let s = &mut self.sections[i];
            s.version = version;
            s.last = Some(secs);
            if pkts.is_empty() {
                s.next_due = t + s.sched;
                continue;
            }
            for (n, p) in pkts.into_iter().enumerate() {
                s.queue.push_back((p, n == 0));
            }
            // next_due is set when the first packet actually goes out
            s.next_due = u64::MAX;
        }
        Ok(())
    }

    fn emit_section(&mut self, layer: usize, t: u64) -> Option<Packet> {
        let window = self.si_window;
        let maxp = self.si_max_packets;
        let mut cands: Vec<(u64, usize)> = Vec::new();
        for (i, s) in self.sections.iter_mut().enumerate() {
            while s.window.front().is_some_and(|&x| x + window <= t) {
                s.window.pop_front();
            }
            if s.layer != layer || s.queue.is_empty() {
                continue;
            }
            // earliest deadline first: the latest start that keeps the interval
            cands.push((s.last_start.map(|l| l + s.interval).unwrap_or(0), i));
        }
        cands.sort();
        for (_, i) in cands {
            let pid = self.sections[i].pid;
            // B10 §5.1.4 "4KB in 32 ms" per PID
            let in_window: usize = self
                .sections
                .iter()
                .filter(|s| s.pid == pid)
                .map(|s| s.window.len())
                .sum();
            if in_window >= maxp {
                continue;
            }
            let s = &mut self.sections[i];
            let (p, first) = s.queue.pop_front()?;
            s.window.push_back(t);
            if first {
                s.last_start = Some(t);
                s.next_due = t + s.sched;
            }
            return Some(p);
        }
        None
    }

    fn prepare(&mut self, ei: usize) -> Result<()> {
        let e = &mut self.es[ei];
        if e.ahead.is_some() {
            return Ok(());
        }
        let to27 =
            |base: u64, pass: u64, period54: u64, v54: u64| base + (pass * period54 + v54) / 2;
        let p = match &e.media {
            Media::Video {
                es,
                force_vbv,
                broken_link,
            } => {
                let i = e.next;
                let pts = to27(e.base, e.pass, e.period54, es.pts54[i]);
                let dts = to27(e.base, e.pass, e.period54, es.dts54[i]);
                let data = es.au_bytes(i, *force_vbv, *broken_link && i == first_gop(es));
                let header = PesHeader {
                    data_alignment_indicator: true,
                    pts_dts: if pts == dts {
                        PtsDts::pts_only(to90k(pts))
                    } else {
                        PtsDts::pts_and_dts(to90k(pts), to90k(dts))
                    },
                    ..PesHeader::default()
                };
                let mut pes = PesPacket::new(0xE0, header, data);
                if 6 + 3 + 10 + es.aus[i].len > 0xFFFF {
                    pes.unbounded = true; // H.222.0 §2.4.3.7: allowed for video in TS
                }
                let bytes = pes.encode()?;
                let rai = es.aus[i].has_sequence_header;
                e.next += 1;
                if e.next == es.aus.len() {
                    e.next = 0;
                    e.pass += 1;
                }
                PreparedPes {
                    first_decode: dts,
                    aus: vec![(dts, bytes.len())],
                    bytes,
                    rai,
                }
            }
            Media::Audio {
                es,
                per_pes,
                sample_starts,
            } => {
                let fs = es.sampling_frequency as u64;
                let i0 = e.next;
                let n = (*per_pes).min(es.frames.len() - i0);
                let mut data = Vec::new();
                let mut times = Vec::new();
                for f in &es.frames[i0..i0 + n] {
                    data.extend_from_slice(&es.data[f.offset..f.offset + f.len]);
                }
                for (q, f) in es.frames[i0..i0 + n].iter().enumerate() {
                    let s54 = sample_starts[i0 + q] * 54_000_000 / fs;
                    times.push((to27(e.base, e.pass, e.period54, s54), f.len));
                }
                let pts = times[0].0;
                let header = PesHeader {
                    data_alignment_indicator: true,
                    pts_dts: PtsDts::pts_only(to90k(pts)),
                    ..PesHeader::default()
                };
                let bytes = PesPacket::new(0xC0, header, data).encode()?;
                let hdr = bytes.len() - times.iter().map(|x| x.1).sum::<usize>();
                times[0].1 += hdr;
                e.next += n;
                if e.next == es.frames.len() {
                    e.next = 0;
                    e.pass += 1;
                }
                PreparedPes {
                    first_decode: pts,
                    aus: times,
                    bytes,
                    rai: true,
                }
            }
        };
        e.ahead = Some(p);
        Ok(())
    }

    /// Emit one PES packet on `layer` if any stream can; with `force`, only
    /// that stream, carrying the PCR.
    fn emit_es(
        &mut self,
        layer: usize,
        t: u64,
        force: Option<(usize, Pcr)>,
    ) -> Result<Option<Packet>> {
        let mut best: Option<(u64, usize)> = None;
        for ei in 0..self.es.len() {
            if self.es[ei].layer != layer || force.is_some_and(|f| f.0 != ei) {
                continue;
            }
            if self.es[ei].current.is_none() {
                self.prepare(ei)?;
            }
            let lead = self.lead;
            let e = &mut self.es[ei];
            let (decode, remaining) = match &e.current {
                Some((b, pos, _, d)) => (*d, b.len() - pos),
                None => {
                    let a = e.ahead.as_ref().expect("prepared");
                    if t + lead < a.first_decode {
                        continue; // H.222.0 §2.4.2.6: ≤ 1 s in the T-STD
                    }
                    (a.first_decode, a.bytes.len())
                }
            };
            let af = if force.is_some() { 8 } else { 0 };
            let payload = remaining.min(PAYLOAD_CAPACITY - af);
            if !e.tstd.can_accept(t, payload) {
                continue;
            }
            if best.is_none_or(|b| decode < b.0) {
                best = Some((decode, ei));
            }
        }
        let Some((_, ei)) = best else { return Ok(None) };
        let e = &mut self.es[ei];
        if e.current.is_none() {
            let a = e.ahead.take().expect("prepared");
            for &(d, b) in &a.aus {
                e.tstd.add_au(d, b);
            }
            e.current = Some((a.bytes, 0, a.rai, a.first_decode));
            e.pes_sent += 1;
        }
        let (bytes, pos, rai, decode) = e.current.as_mut().expect("current");
        let pusi = *pos == 0;
        let mut fields = AdaptationFields::default();
        let mut has_af = false;
        // H.222.0 §2.4.3.5: "In the PCR_PID the random_access_indicator may
        // only be set to '1' in Transport Stream packet containing the PCR
        // fields."
        if pusi && *rai && (!e.is_pcr_pid || force.is_some()) {
            fields.random_access_indicator = true;
            has_af = true;
        }
        if pusi {
            *rai = false;
        }
        if let Some((_, pcr)) = force {
            fields.pcr = Some(pcr);
            has_af = true;
        }
        let af_len = if has_af { 1 + fields.length() } else { 0 };
        let n = (bytes.len() - *pos).min(PAYLOAD_CAPACITY - af_len);
        let chunk = &bytes[*pos..*pos + n];
        let cc = e.cc.next(true);
        let pkt = if has_af {
            TsPacket::with_adaptation(e.pid, pusi, cc, Some(fields), chunk)?
        } else {
            TsPacket::with_payload(e.pid, pusi, cc, chunk)?
        };
        *pos += n;
        e.tstd.push(t, n);
        if *pos == bytes.len() {
            if t > *decode {
                e.late += 1;
            }
            e.current = None;
        }
        Ok(Some(pkt.encode()?))
    }
}

fn first_gop(es: &VideoEs) -> usize {
    es.aus
        .iter()
        .position(|a| a.gop_flags_at.is_some())
        .unwrap_or(usize::MAX)
}

impl Iterator for Multiplexer {
    type Item = [Vec<Packet>; 3];
    /// Endless: media loops.
    fn next(&mut self) -> Option<Self::Item> {
        Some(self.next_frame())
    }
}

/// Borrow a frame in the form `isdbt_phy::Transmitter::transmit_frame` takes.
pub fn layer_slices(f: &[Vec<Packet>; 3]) -> [&[Packet]; 3] {
    [&f[0], &f[1], &f[2]]
}
