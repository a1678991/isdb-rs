//! Synthetic transport streams built with the isdb-ts / isdb-si encoders.
#![allow(dead_code)]

use isdb_si::descriptor::{self as d, Descriptor};
use isdb_si::tables::*;
use isdb_si::time::{BcdDuration, Date, DateTime, MjdTime};
use isdb_text::AribString;
use isdb_ts::cc::Counter;
use isdb_ts::packet::{PACKET_SIZE, Packet};
use isdb_ts::packetize::{Packing, packetize_sections};
use isdb_ts::pcr::Pcr;
use isdb_ts::pes::{PesHeader, PesPacket, PtsDts};
use isdb_ts::psi::{Pat, PatEntry, Pmt, PmtStream};
use isdb_ts::section::{LongHeader, ShortHeader};
use std::collections::HashMap;

pub type Pkt = [u8; PACKET_SIZE];

pub const TSID: u16 = 0x79EE;
pub const ONID: u16 = 0x79EE;
pub const SID: u16 = 0x0408;
pub const PMT_PID: u16 = 0x01F0;
pub const PCR_PID: u16 = 0x01FF;
pub const AUDIO_PID: u16 = 0x0110;
/// Packets per second of the synthetic CBR stream.
pub const PPS: u64 = 2000;

pub fn si_header(tid: u8, ext: u16, version: u8) -> LongHeader {
    LongHeader {
        private_indicator: true,
        ..LongHeader::new(tid, ext, version)
    }
}

pub fn pat() -> Vec<u8> {
    Pat::section(
        TSID,
        1,
        vec![PatEntry::new(0, 0x0010), PatEntry::new(SID, PMT_PID)],
    )
    .encode()
    .unwrap()
}

pub fn pmt() -> Vec<u8> {
    Pmt::section(
        SID,
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
    Nit {
        header: si_header(0x40, ONID, version),
        body: NitBody(NetworkLoops {
            reserved_future_use_1: 0xF,
            descriptors: vec![
                Descriptor::NetworkName(d::NetworkName {
                    name: AribString::encode("テスト").unwrap(),
                }),
                Descriptor::SystemManagement(d::SystemManagement {
                    system_management_id: 0x0301,
                    additional_identification_info: vec![],
                }),
            ],
            reserved_future_use_2: 0xF,
            transport_streams: vec![TransportStreamEntry {
                transport_stream_id: TSID,
                original_network_id: ONID,
                reserved_future_use: 0xF,
                descriptors: vec![
                    Descriptor::ServiceList(d::ServiceList {
                        services: vec![d::ServiceListEntry {
                            service_id: SID,
                            service_type: 0x01,
                        }],
                    }),
                    tds(gi),
                    Descriptor::TsInformation(d::TsInformation {
                        remote_control_key_id: 9,
                        ts_name: AribString::encode("テスト").unwrap(),
                        transmission_types: vec![d::TransmissionType {
                            transmission_type_info: 0x0F,
                            service_ids: vec![SID],
                        }],
                        reserved_future_use: vec![],
                    }),
                ],
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
    Eit {
        header: LongHeader {
            section_number: sn,
            last_section_number: 1,
            ..si_header(0x4E, SID, 1)
        },
        body: EitBody {
            transport_stream_id: TSID,
            original_network_id: ONID,
            segment_last_section_number: 1,
            last_table_id: 0x4E,
            events,
        },
    }
    .encode()
    .unwrap()
}

pub fn tot(sec: u32) -> Vec<u8> {
    let t = jst(12, (sec / 60) as u8, (sec % 60) as u8);
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
    pub pid: u16,
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

pub fn item(pid: u16, period_ms: u64, phase_ms: u64, s: Vec<u8>) -> Item {
    Item {
        pid,
        period_ms,
        phase_ms,
        make: Box::new(move |_| vec![s.clone()]),
    }
}

/// The default valid stream: PAT/PMT 100 ms (well inside), CAT 500 ms,
/// NIT 1 s, SDT 1 s, EIT p/f 1 s, TOT 5 s, PCR 40 ms, audio 64 ms.
pub fn default_spec(seconds: u64) -> Spec {
    Spec {
        seconds,
        pcr_period_ms: 40,
        items: vec![
            item(0x0000, 80, 0, pat()),
            item(PMT_PID, 80, 5, pmt()),
            item(0x0001, 500, 7, cat()),
            item(0x0010, 1000, 11, nit()),
            item(0x0011, 1000, 13, sdt()),
            Item {
                pid: 0x0012,
                period_ms: 1000,
                phase_ms: 17,
                make: Box::new(|_| {
                    vec![eit_pf(0, vec![event(1, 12)]), eit_pf(1, vec![event(2, 13)])]
                }),
            },
            Item {
                pid: 0x0014,
                period_ms: 5000,
                phase_ms: 19,
                make: Box::new(|n| vec![tot(n as u32 * 5)]),
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
    let mut cc: HashMap<u16, Counter> = HashMap::new();
    let mut queue: Vec<Pkt> = Vec::new(); // FIFO of pending packets
    let mut out = Vec::with_capacity(total as usize);
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
