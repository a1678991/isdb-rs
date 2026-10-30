//! Synthetic transport streams built with the isdb-ts / isdb-si encoders.
#![allow(dead_code)]

use isdb_si::descriptor::{self as d, Descriptor};
use isdb_si::tables::*;
use isdb_si::time::{BcdDuration, Date, DateTime, MjdTime};
use isdb_text::AribString;
pub use isdb_ts::Pid;
use isdb_ts::cc::Counter;
use isdb_ts::packet::{PACKET_SIZE, Packet};
use isdb_ts::packetize::{Packing, packetize_sections};
use isdb_ts::pcr::Pcr;
use isdb_ts::pes::{PesHeader, PesPacket, PtsDts};
use isdb_ts::psi::{Pat, PatEntry, Pmt, PmtStream};
use isdb_ts::section::{LongHeader, ShortHeader};
use std::collections::HashMap;

pub type Pkt = [u8; PACKET_SIZE];

/// A literal PID (panics above 0x1FFF).
pub fn pid(v: u16) -> Pid {
    Pid::new(v).expect("13-bit PID")
}

/// A PID argument of the builders: a `Pid` or a literal (checked).
pub struct PidArg(pub Pid);
impl From<Pid> for PidArg {
    fn from(p: Pid) -> Self {
        PidArg(p)
    }
}
impl From<u16> for PidArg {
    fn from(v: u16) -> Self {
        PidArg(Pid::new(v).expect("13-bit PID"))
    }
}
impl From<i32> for PidArg {
    fn from(v: i32) -> Self {
        PidArg(Pid::new(u16::try_from(v).expect("13-bit PID")).expect("13-bit PID"))
    }
}

pub const TSID: u16 = 0x79EE;
pub const ONID: u16 = 0x79EE;
pub const SID: u16 = 0x0408;
pub const PMT_PID: Pid = Pid::new(0x01F0).unwrap();
pub const PCR_PID: Pid = Pid::new(0x01FF).unwrap();
pub const AUDIO_PID: Pid = Pid::new(0x0110).unwrap();
/// Packets per second of the synthetic CBR stream.
pub const PPS: u64 = 2000;

pub fn si_header(tid: u8, ext: u16, version: u8) -> LongHeader {
    LongHeader {
        private_indicator: true,
        ..LongHeader::new(tid, ext, version)
    }
}

/// Identities of a synthetic stream.
#[derive(Debug, Clone, Copy)]
pub struct Ids {
    pub tsid: u16,
    pub onid: u16,
    pub sid: u16,
}

/// The terrestrial identities of [`default_spec`].
pub const TERRESTRIAL: Ids = Ids {
    tsid: TSID,
    onid: ONID,
    sid: SID,
};

/// BS-shaped identities (bs-si.md §6.1): network_id/onid 0x0004 is the BS
/// network's (B10 Annex N); TS_ID 0x4017 and service_id 0x03E7 are
/// placeholders that no BS broadcaster uses.
pub const BS: Ids = Ids {
    tsid: 0x4017,
    onid: 0x0004,
    sid: 0x03E7,
};

pub fn pat() -> Vec<u8> {
    pat_ids(TERRESTRIAL)
}

pub fn pat_ids(ids: Ids) -> Vec<u8> {
    Pat::section(
        ids.tsid,
        1,
        vec![
            PatEntry::new(0, Pid::new(0x0010).unwrap()),
            PatEntry::new(ids.sid, PMT_PID),
        ],
    )
    .encode()
    .unwrap()
}

pub fn pmt() -> Vec<u8> {
    pmt_sid(SID)
}

pub fn pmt_sid(sid: u16) -> Vec<u8> {
    Pmt::section(
        sid,
        1,
        Pmt::new(
            PCR_PID,
            vec![],
            vec![PmtStream::new(
                0x0F,
                AUDIO_PID,
                vec![isdb_ts::descriptor::Descriptor::new(0x52, [0x10])],
            )],
        ),
    )
    .encode()
    .unwrap()
}

pub fn cat() -> Vec<u8> {
    isdb_ts::psi::Cat::section(1, vec![]).encode().unwrap()
}

pub fn tds(gi: u8) -> Descriptor {
    Descriptor::TerrestrialDeliverySystem(d::TerrestrialDeliverySystem {
        area_code: 0x5A5,
        guard_interval: gi,
        transmission_mode: 2,
        frequencies: vec![3900],
    })
}

pub fn nit_with(version: u8, gi: u8) -> Vec<u8> {
    nit_terrestrial(version, 0x0301, vec![tds(gi)])
}

/// A terrestrial NIT (network_id = onid = ONID, TS TSID) with
/// system_management_id `sm_id` and `delivery` between the service list
/// and the TS information descriptor.
pub fn nit_terrestrial(version: u8, sm_id: u16, delivery: Vec<Descriptor>) -> Vec<u8> {
    let mut ts_descriptors = vec![Descriptor::ServiceList(d::ServiceList {
        services: vec![d::ServiceListEntry {
            service_id: SID,
            service_type: 0x01,
        }],
    })];
    ts_descriptors.extend(delivery);
    ts_descriptors.push(Descriptor::TsInformation(d::TsInformation {
        remote_control_key_id: 9,
        ts_name: AribString::encode("テスト").unwrap(),
        transmission_types: vec![d::TransmissionType {
            transmission_type_info: 0x0F,
            service_ids: vec![SID],
        }],
        reserved_future_use: vec![],
    }));
    Nit {
        header: si_header(0x40, ONID, version),
        body: NitBody(NetworkLoops {
            reserved_future_use_1: 0xF,
            descriptors: vec![
                Descriptor::NetworkName(d::NetworkName {
                    name: AribString::encode("テスト").unwrap(),
                }),
                Descriptor::SystemManagement(d::SystemManagement {
                    system_management_id: sm_id,
                    additional_identification_info: vec![],
                }),
            ],
            reserved_future_use_2: 0xF,
            transport_streams: vec![TransportStreamEntry {
                transport_stream_id: TSID,
                original_network_id: ONID,
                reserved_future_use: 0xF,
                descriptors: ts_descriptors,
            }],
        }),
    }
    .encode()
    .unwrap()
}

/// satellite_delivery_system_descriptor of BS-1 laid out per B10 Table
/// 6-8 (bs-si.md §6.2): frequency 011.72748 GHz (8 BCD), orbital 110.0
/// (4 BCD), east, circular right ('11', Table 6-9), modulation '01000'
/// (Table 6-10), symbol rate 028.8600 Msym/s (7 BCD), FEC '1000' (Table
/// 6-11).
pub fn bs1_delivery() -> Descriptor {
    Descriptor::SatelliteDeliverySystem(d::SatelliteDeliverySystem {
        frequency: 0x0117_2748,
        orbital_position: 0x1100,
        west_east_flag: true,
        polarization: 0b11,
        modulation: 0b0_1000,
        symbol_rate: 0x028_8600,
        fec_inner: 0b1000,
    })
}

/// A BS-shaped NIT actual (bs-si.md §6.2): network_id 0x0004, network
/// loop 0x40 "ベンチ" + 0xFE `sm_id`, one TS with the [`BS`] identities
/// and 0x41 then `delivery`.
pub fn nit_bs(sm_id: u16, delivery: Vec<Descriptor>) -> Vec<u8> {
    let mut ts_descriptors = vec![Descriptor::ServiceList(d::ServiceList {
        services: vec![d::ServiceListEntry {
            service_id: BS.sid,
            service_type: 0x01,
        }],
    })];
    ts_descriptors.extend(delivery);
    Nit {
        header: si_header(0x40, BS.onid, 0),
        body: NitBody(NetworkLoops {
            reserved_future_use_1: 0xF,
            descriptors: vec![
                Descriptor::NetworkName(d::NetworkName {
                    name: AribString::encode("ベンチ").unwrap(),
                }),
                Descriptor::SystemManagement(d::SystemManagement {
                    system_management_id: sm_id,
                    additional_identification_info: vec![],
                }),
            ],
            reserved_future_use_2: 0xF,
            transport_streams: vec![TransportStreamEntry {
                transport_stream_id: BS.tsid,
                original_network_id: BS.onid,
                reserved_future_use: 0xF,
                descriptors: ts_descriptors,
            }],
        }),
    }
    .encode()
    .unwrap()
}

pub fn nit() -> Vec<u8> {
    nit_with(1, 2)
}

pub fn sdt_with(tsid: u16, onid: u16, sids: &[u16]) -> Vec<u8> {
    Sdt {
        header: si_header(0x42, tsid, 1),
        body: SdtBody {
            original_network_id: onid,
            reserved_future_use: 0xFF,
            services: sids
                .iter()
                .map(|&s| SdtService {
                    service_id: s,
                    reserved_future_use: 0x7,
                    eit_user_defined_flags: 0x7,
                    eit_schedule_flag: false,
                    eit_present_following_flag: true,
                    running_status: 4,
                    free_ca_mode: false,
                    descriptors: vec![Descriptor::Service(d::Service {
                        service_type: 0x01,
                        provider_name: AribString::encode("P").unwrap(),
                        service_name: AribString::encode("S").unwrap(),
                    })],
                })
                .collect(),
        },
    }
    .encode()
    .unwrap()
}

pub fn sdt() -> Vec<u8> {
    sdt_with(TSID, ONID, &[SID])
}

pub fn jst(h: u8, m: u8, s: u8) -> MjdTime {
    MjdTime::from_jst(DateTime {
        date: Date {
            year: 2026,
            month: 10,
            day: 7,
        },
        hour: h,
        minute: m,
        second: s,
    })
    .unwrap()
}

pub fn event(id: u16, h: u8) -> EitEvent {
    EitEvent {
        event_id: id,
        start_time: jst(h, 0, 0),
        duration: BcdDuration::from_hms(1, 0, 0).unwrap(),
        running_status: 0,
        free_ca_mode: false,
        descriptors: vec![Descriptor::ShortEvent(d::ShortEvent {
            language: *b"jpn",
            event_name: AribString::encode("番組").unwrap(),
            text: AribString::encode("").unwrap(),
        })],
    }
}

pub fn eit_pf(sn: u8, events: Vec<EitEvent>) -> Vec<u8> {
    eit_pf_ids(TERRESTRIAL, sn, events)
}

pub fn eit_pf_ids(ids: Ids, sn: u8, events: Vec<EitEvent>) -> Vec<u8> {
    Eit {
        header: LongHeader {
            section_number: sn,
            last_section_number: 1,
            ..si_header(0x4E, ids.sid, 1)
        },
        body: EitBody {
            transport_stream_id: ids.tsid,
            original_network_id: ids.onid,
            segment_last_section_number: 1,
            last_table_id: 0x4E,
            events,
        },
    }
    .encode()
    .unwrap()
}

pub fn tot(sec: u32) -> Vec<u8> {
    let t = jst(
        12,
        u8::try_from(sec / 60).unwrap(),
        u8::try_from(sec % 60).unwrap(),
    );
    isdb_si::tables::Tot {
        header: ShortHeader {
            table_id: 0x73,
            private_indicator: true,
            reserved1: 3,
        },
        body: TotBody {
            jst_time: t,
            reserved: 0xF,
            descriptors: vec![],
        },
    }
    .encode()
    .unwrap()
}

pub fn bit() -> Vec<u8> {
    Bit {
        header: si_header(0xC4, ONID, 1),
        body: BitBody {
            reserved_future_use: 0x7,
            broadcast_view_propriety: true,
            first_descriptors: vec![],
            broadcasters: vec![BitBroadcaster {
                broadcaster_id: 1,
                reserved_future_use: 0xF,
                descriptors: vec![],
            }],
        },
    }
    .encode()
    .unwrap()
}

/// A scheduled item of the synthetic stream.
pub struct Item {
    /// PID.
    pub pid: Pid,
    /// Period in ms (0 = once at start).
    pub period_ms: u64,
    /// Phase in ms.
    pub phase_ms: u64,
    /// Sections (re-generated at each repetition by `make(n)`).
    pub make: Box<dyn Fn(u64) -> Vec<Vec<u8>>>,
}

pub struct Spec {
    pub seconds: u64,
    pub pcr_period_ms: u64,
    pub items: Vec<Item>,
    /// Audio PES period in ms (0 = none).
    pub audio_period_ms: u64,
    /// Payload of every audio PES (one ADTS frame; default
    /// [`adts_frame`] with no violation).
    pub audio_payload: Vec<u8>,
    pub packing: Packing,
}

pub fn item(pid: impl Into<PidArg>, period_ms: u64, phase_ms: u64, s: Vec<u8>) -> Item {
    Item {
        pid: pid.into().0,
        period_ms,
        phase_ms,
        make: Box::new(move |_| vec![s.clone()]),
    }
}

/// The default valid stream: PAT/PMT 100 ms (well inside), CAT 500 ms,
/// NIT 1 s, SDT 1 s, EIT p/f 1 s, TOT 5 s, PCR 40 ms, audio 64 ms.
pub fn default_spec(seconds: u64) -> Spec {
    spec_with(seconds, TERRESTRIAL, nit())
}

/// [`default_spec`] with the identities `ids` in PAT, PMT, SDT and EIT and
/// `nit` as the NIT section.
pub fn spec_with(seconds: u64, ids: Ids, nit: Vec<u8>) -> Spec {
    Spec {
        seconds,
        pcr_period_ms: 40,
        items: vec![
            item(0x0000, 80, 0, pat_ids(ids)),
            item(PMT_PID, 80, 5, pmt_sid(ids.sid)),
            item(0x0001, 500, 7, cat()),
            item(0x0010, 1000, 11, nit),
            item(0x0011, 1000, 13, sdt_with(ids.tsid, ids.onid, &[ids.sid])),
            Item {
                pid: Pid::new(0x0012).unwrap(),
                period_ms: 1000,
                phase_ms: 17,
                make: Box::new(move |_| {
                    vec![
                        eit_pf_ids(ids, 0, vec![event(1, 12)]),
                        eit_pf_ids(ids, 1, vec![event(2, 13)]),
                    ]
                }),
            },
            Item {
                pid: Pid::new(0x0014).unwrap(),
                period_ms: 5000,
                phase_ms: 19,
                make: Box::new(|n| vec![tot(u32::try_from(n).unwrap() * 5)]),
            },
        ],
        audio_period_ms: 64,
        audio_payload: adts_frame(|_| {}),
        packing: Packing::OnePerPacket,
    }
}

/// One silent AAC LC 48 kHz stereo ADTS frame of 200 bytes that satisfies
/// the ARIB STD-B32 Part 2 §5.2 header rules (crc_check present,
/// adts_buffer_fullness 0x40), after `edit` of its header template
/// (`SilentFrame` computes crc_check from its own element layout).
pub fn adts_frame(edit: impl FnOnce(&mut isdbt_mux::adts::AdtsHeader)) -> Vec<u8> {
    let mut f = isdbt_mux::audio::SilentFrame::stereo_48k(200);
    f.header.protection_absent = false;
    f.header.adts_buffer_fullness = 0x40;
    edit(&mut f.header);
    f.build()
}

/// Build a CBR stream: PPS packets/s, null packets where nothing is due.
pub fn build(spec: &Spec) -> Vec<Pkt> {
    let total = spec.seconds * PPS;
    let mut cc: HashMap<Pid, Counter> = HashMap::new();
    let mut queue: Vec<Pkt> = Vec::new(); // FIFO of pending packets
    let mut out = Vec::with_capacity(usize::try_from(total).unwrap());
    let mut next: Vec<u64> = spec.items.iter().map(|i| i.phase_ms).collect();
    let mut reps: Vec<u64> = vec![0; spec.items.len()];
    let mut next_pcr = 0u64;
    let mut next_audio = 3u64;
    let null = Packet::null().encode().unwrap();
    for i in 0..total {
        let t_ms = i * 1000 / PPS;
        if t_ms >= next_pcr && spec.pcr_period_ms > 0 {
            let ticks = i * 27_000_000 / PPS;
            let c = cc.entry(PCR_PID).or_default();
            out.push(
                Packet::pcr_only(PCR_PID, c.next(false), Pcr::from_ticks(ticks))
                    .encode()
                    .unwrap(),
            );
            next_pcr += spec.pcr_period_ms;
            continue;
        }
        for (k, it) in spec.items.iter().enumerate() {
            if next[k] != u64::MAX && t_ms >= next[k] {
                let secs = (it.make)(reps[k]);
                reps[k] += 1;
                let c = cc.entry(it.pid).or_default();
                queue.extend(packetize_sections(it.pid, &secs, spec.packing, c).unwrap());
                next[k] = if it.period_ms == 0 {
                    u64::MAX
                } else {
                    next[k] + it.period_ms
                };
            }
        }
        if spec.audio_period_ms > 0 && t_ms >= next_audio {
            // PTS 0.5 s ahead of the PCR time at this packet.
            let pts = (i * 90_000 / PPS + 45_000) & ((1 << 33) - 1);
            let h = PesHeader {
                pts_dts: PtsDts::pts_only(pts),
                ..Default::default()
            };
            let pes = PesPacket::new(0xC0, h, spec.audio_payload.clone())
                .encode()
                .unwrap();
            let c = cc.entry(AUDIO_PID).or_default();
            queue.extend(isdb_ts::pes::packetize(AUDIO_PID, &pes, None, c).unwrap());
            next_audio += spec.audio_period_ms;
        }
        if !queue.is_empty() {
            out.push(queue.remove(0));
        } else {
            out.push(null);
        }
    }
    out
}

pub fn scan(p: &[Pkt]) -> isdb_analyze::Capture {
    isdb_analyze::scan_packets(p, &Default::default())
}

pub fn validate(p: &[Pkt]) -> isdb_analyze::validate::ValidationReport {
    isdb_analyze::validate::validate(&scan(p), &Default::default())
}

pub fn has(
    r: &isdb_analyze::validate::ValidationReport,
    sev: isdb_analyze::validate::Severity,
    check: &str,
    clause_part: &str,
) -> bool {
    r.findings
        .iter()
        .any(|f| f.severity == sev && f.check == check && f.clause.contains(clause_part))
}
