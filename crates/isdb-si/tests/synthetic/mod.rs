//! Synthetic SI section set of a fictional terrestrial multiplex, built
//! with this crate's typed encoders (strings with `isdb-text`), so every
//! section carries a valid CRC_32 and re-encodes byte-identically.
//!
//! It mirrors the table and descriptor variety of a full-segment ISDB-T
//! multiplex (ARIB STD-B10 Part 2 §5.2, TR-B14 operation): NIT with
//! terrestrial delivery system (several frequencies), partial reception,
//! service list, TS information (two transmission types), system
//! management and emergency information; BIT with SI parameter and
//! extended broadcaster descriptors; SDT with service, digital copy
//! control and the three logo transmission schemes; EIT p/f actual on
//! 0x0012 and the one-seg L-EIT on 0x0027; EIT schedule basic (0x50/0x51)
//! and extended (0x58/0x59) with extended event descriptors whose item
//! spans several descriptors; event group descriptors; empty EIT
//! sections; TOT with and without local time offset; TDT.
//!
//! Every name and text is fictional.

#![allow(dead_code)]

use isdb_si::descriptor::*;
use isdb_si::tables::*;
use isdb_si::time::{BcdDuration, BcdOffset, Date, DateTime, MjdTime};
use isdb_text::AribString;
use isdb_ts::Pid;
use isdb_ts::section::{Long, LongHeader, Short, ShortHeader};

/// network_id = original_network_id = transport_stream_id of the
/// fictional multiplex.
pub const NETWORK_ID: u16 = 0x79EE;
/// HD service (service_type 0x01) whose EIT carries every event descriptor.
pub const HD: u16 = 0x0400;
/// Second and third services of the multi-channel group (service_type 0x01).
pub const SUB: [u16; 2] = [0x0401, 0x0402];
/// Temporary video service (service_type 0xA1, no EIT schedule).
pub const TEMP: u16 = 0x0404;
/// One-seg partial-reception service (service_type 0xC0, L-EIT on 0x0027).
pub const ONESEG: u16 = 0x0588;

/// `(pid, table_id, section bytes)` of every synthetic section.
pub fn sections() -> Vec<(Pid, u8, Vec<u8>)> {
    let mut out = Vec::new();
    let mut push = |pid: u16, sec: Vec<u8>| out.push((Pid::new(pid).unwrap(), sec[0], sec));
    push(0x0010, nit());
    push(0x0024, bit());
    push(0x0011, sdt());
    for sec in eit_pf() {
        push(0x0012, sec);
    }
    for sec in l_eit() {
        push(0x0027, sec);
    }
    for sec in eit_schedule() {
        push(0x0012, sec);
    }
    for sec in tot_tdt() {
        push(0x0014, sec);
    }
    out
}

/// ARIB 8-unit string of fictional text.
fn s(text: &str) -> AribString {
    isdb_text::encode(text).expect("encodable synthetic text")
}

/// SI long header: reserved_future_use bit '1' (B10 Part 2 §5.2).
fn long_header(table_id: u8, ext: u16, version: u8, sec: u8, last: u8) -> LongHeader {
    LongHeader {
        private_indicator: true,
        section_number: sec,
        last_section_number: last,
        ..LongHeader::new(table_id, ext, version)
    }
}

fn jst(day: u8, hour: u8, minute: u8) -> MjdTime {
    MjdTime::from_jst(DateTime {
        date: Date {
            year: 2026,
            month: 1,
            day,
        },
        hour,
        minute,
        second: 0,
    })
    .unwrap()
}

/// Channel → centre frequency in units of 1/7 MHz (B10 §6.2.31):
/// UHF channel N is centred on 473 + 6·(N − 13) + 1/7 MHz.
fn uhf(ch: u16) -> u16 {
    7 * (473 + 6 * (ch - 13)) + 1
}

fn nit() -> Vec<u8> {
    let ts_descriptors = vec![
        Descriptor::ServiceList(ServiceList {
            services: [
                (HD, 0x01),
                (SUB[0], 0x01),
                (SUB[1], 0x01),
                (ONESEG, 0xC0),
                (TEMP, 0xA1),
            ]
            .into_iter()
            .map(|(service_id, service_type)| ServiceListEntry {
                service_id,
                service_type,
            })
            .collect(),
        }),
        Descriptor::TerrestrialDeliverySystem(TerrestrialDeliverySystem {
            area_code: 0x5A5,
            guard_interval: 0b10,    // 1/8 (Table 6-62)
            transmission_mode: 0b10, // Mode 3 (Table 6-63)
            frequencies: [20, 23, 27, 31, 36, 40, 44, 49, 52].map(uhf).to_vec(),
        }),
        Descriptor::PartialReception(PartialReception {
            service_ids: vec![ONESEG],
        }),
        Descriptor::TsInformation(TsInformation {
            remote_control_key_id: 12,
            ts_name: s("テストテレビ"),
            transmission_types: vec![
                // 0x0F: layer B (full segment) services
                TransmissionType {
                    transmission_type_info: 0x0F,
                    service_ids: vec![HD, SUB[0], SUB[1], TEMP],
                },
                // 0xAF: layer A (partial reception) service
                TransmissionType {
                    transmission_type_info: 0xAF,
                    service_ids: vec![ONESEG],
                },
            ],
            reserved_future_use: vec![],
        }),
    ];
    Long {
        header: long_header(0x40, NETWORK_ID, 3, 0, 0),
        body: NitBody(NetworkLoops {
            reserved_future_use_1: 0xF,
            descriptors: vec![
                Descriptor::NetworkName(NetworkName {
                    name: s("テストネット"),
                }),
                Descriptor::SystemManagement(SystemManagement {
                    system_management_id: 0x0301,
                    additional_identification_info: vec![],
                }),
                Descriptor::EmergencyInformation(EmergencyInformation {
                    services: vec![EmergencyService {
                        service_id: HD,
                        start_end_flag: false,
                        signal_level: false,
                        reserved_future_use: 0x3F,
                        area_codes: vec![AreaCode {
                            area_code: 0x5A5,
                            reserved_future_use: 0xF,
                        }],
                    }],
                }),
            ],
            reserved_future_use_2: 0xF,
            transport_streams: vec![TransportStreamEntry {
                transport_stream_id: NETWORK_ID,
                original_network_id: NETWORK_ID,
                reserved_future_use: 0xF,
                descriptors: ts_descriptors,
            }],
        }),
    }
    .encode()
    .unwrap()
}

fn si_parameter(tables: &[(u8, &[u8])]) -> Descriptor {
    Descriptor::SiParameter(SiParameter {
        parameter_version: 0x01,
        update_time: 0xE000,
        tables: tables
            .iter()
            .map(|&(table_id, d)| TableDescription {
                table_id,
                description: d.to_vec(),
            })
            .collect(),
    })
}

fn bit() -> Vec<u8> {
    Long {
        header: long_header(0xC4, NETWORK_ID, 1, 0, 0),
        body: BitBody {
            reserved_future_use: 0x7,
            broadcast_view_propriety: true,
            first_descriptors: vec![si_parameter(&[
                (0x40, &[0x0A]),
                (0xC4, &[0x0A]),
                (0x42, &[0x0A]),
                (0x4E, &[0x02, 0x03, 0x04, 0x05]),
                (0x50, &[0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08]),
            ])],
            broadcasters: vec![BitBroadcaster {
                broadcaster_id: 0x01,
                reserved_future_use: 0xF,
                descriptors: vec![
                    Descriptor::BroadcasterName(BroadcasterName {
                        name: s("テスト放送"),
                    }),
                    Descriptor::ExtendedBroadcaster(ExtendedBroadcaster {
                        reserved_future_use: 0xF,
                        body: ExtendedBroadcasterBody::TerrestrialTv(TerrestrialBroadcaster {
                            terrestrial_broadcaster_id: 0x0001,
                            affiliation_ids: vec![0x01],
                            broadcasters: vec![],
                            private_data: vec![],
                        }),
                    }),
                    si_parameter(&[
                        (0x4E, &[0x02, 0x03, 0x04, 0x05]),
                        (0x58, &[0x01, 0x02, 0x03, 0x04, 0x05, 0x06]),
                        (0xC8, &[0x0A, 0x0B]),
                    ]),
                ],
            }],
        },
    }
    .encode()
    .unwrap()
}

/// digital_copy_control_descriptor: copy-one-generation ('10'),
/// user_defined, optional maximum_bitrate (1/4 Mbit/s units).
fn copy_control(user_defined: u8, maximum_bitrate: Option<u8>) -> Descriptor {
    Descriptor::DigitalCopyControl(DigitalCopyControl {
        digital_recording_control_data: 0b10,
        user_defined,
        maximum_bitrate,
        component_control: None,
    })
}

fn sdt() -> Vec<u8> {
    let cdt1 = LogoTransmissionBody::Cdt1 {
        reserved_future_use_1: 0x7F,
        logo_id: 0x011,
        reserved_future_use_2: 0xF,
        logo_version: 0x002,
        download_data_id: 0x0002,
    };
    let cdt2 = LogoTransmissionBody::Cdt2 {
        reserved_future_use: 0x7F,
        logo_id: 0x011,
    };
    let service = |service_id: u16,
                   service_type: u8,
                   name: &str,
                   user_flags: u8,
                   schedule: bool,
                   logo: LogoTransmissionBody| SdtService {
        service_id,
        reserved_future_use: 0x7,
        eit_user_defined_flags: user_flags,
        eit_schedule_flag: schedule,
        eit_present_following_flag: true,
        running_status: 0,
        free_ca_mode: false,
        descriptors: vec![
            Descriptor::Service(Service {
                service_type,
                provider_name: AribString::default(),
                service_name: s(name),
            }),
            copy_control(0b0100, None),
            Descriptor::LogoTransmission(LogoTransmission { body: logo }),
        ],
    };
    Long {
        header: long_header(0x42, NETWORK_ID, 1, 0, 0),
        body: SdtBody {
            original_network_id: NETWORK_ID,
            reserved_future_use: 0xFF,
            services: vec![
                service(HD, 0x01, "テストテレビ１", 0b100, true, cdt1.clone()),
                service(SUB[0], 0x01, "テストテレビ２", 0b100, true, cdt2.clone()),
                service(SUB[1], 0x01, "テストテレビ３", 0b100, true, cdt2),
                service(TEMP, 0xA1, "テスト臨時放送", 0b100, false, cdt1),
                service(
                    ONESEG,
                    0xC0,
                    "テストワンセグ",
                    0b001,
                    false,
                    LogoTransmissionBody::Simple(s("ﾃｽﾄ")),
                ),
            ],
        },
    }
    .encode()
    .unwrap()
}

/// One fictional programme of the HD service.
struct Programme {
    event_id: u16,
    day: u8,
    hour: u8,
    minute: u8,
    minutes: u8,
    name: &'static str,
    text: &'static str,
    /// content_nibble_level_1/2 pairs (B10 Annex H).
    genres: &'static [(u8, u8)],
    /// Extended event items: (item_description, item).
    items: &'static [(&'static str, &'static str)],
    /// Second (non-main) audio component.
    dual_audio: bool,
}

const PROGRAMMES: &[Programme] = &[
    Programme {
        event_id: 0x1001,
        day: 15,
        hour: 5,
        minute: 0,
        minutes: 60,
        name: "テスト・ニュース",
        text: "架空の放送局による試験用のニュース番組です。",
        genres: &[(0x0, 0x0), (0x0, 0x1)],
        items: &[(
            "番組内容",
            "試験用の番組説明です。実在の番組とは関係ありません。",
        )],
        dual_audio: false,
    },
    Programme {
        event_id: 0x1002,
        day: 15,
        hour: 6,
        minute: 0,
        minutes: 30,
        name: "サンプル天気予報",
        text: "全国の天気（サンプル）",
        genres: &[(0x0, 0x1)],
        items: &[],
        dual_audio: false,
    },
    Programme {
        event_id: 0x1003,
        day: 15,
        hour: 6,
        minute: 30,
        minutes: 90,
        name: "サンプル番組　第１回「はじまり」",
        text: "テスト太郎とサンプル花子が架空の町を旅する番組。字幕放送。",
        genres: &[(0x2, 0x2), (0x0, 0x4), (0x5, 0x2)],
        items: &[
            (
                "番組内容",
                "架空の町を舞台にした試験用の紀行番組です。各地の名所（すべて架空）を紹介します。",
            ),
            ("出演者", "テスト太郎、サンプル花子、ダミー次郎"),
            ("音楽", "サンプル楽団"),
        ],
        dual_audio: true,
    },
    Programme {
        event_id: 0x1004,
        day: 15,
        hour: 8,
        minute: 0,
        minutes: 120,
        name: "Sample Movie Theater",
        text: "ABC 123 ｱｲｳ ＡＢＣ　かなカナ漢字 ★記号",
        genres: &[(0x6, 0x0)],
        items: &[
            (
                "あらすじ",
                "試験信号の世界で暮らす主人公が、失われた同期ワードを探す物語。途中で多くの架空の人物と出会う。",
            ),
            ("出演", "ダミー次郎、テスト三郎"),
        ],
        dual_audio: false,
    },
];

/// Extended event descriptors (B10 §6.2.7) for `items`, every item_char
/// loop split into chunks so that an item spans several descriptors; a
/// continuation descriptor carries an empty item_description.
fn extended_event(items: &[(&str, &str)], chunk_chars: usize) -> Vec<Descriptor> {
    let mut parts: Vec<ExtendedEventItem> = Vec::new();
    for (desc, item) in items {
        let chars: Vec<char> = item.chars().collect();
        for (k, c) in chars.chunks(chunk_chars).enumerate() {
            parts.push(ExtendedEventItem {
                description: if k == 0 {
                    s(desc)
                } else {
                    AribString::default()
                },
                item: s(&c.iter().collect::<String>()),
            });
        }
    }
    let last = u8::try_from(parts.len() - 1).unwrap();
    (0u8..)
        .zip(parts)
        .map(|(n, it)| {
            Descriptor::ExtendedEvent(ExtendedEvent {
                descriptor_number: n,
                last_descriptor_number: last,
                language: *b"jpn",
                items: vec![it],
                text: AribString::default(),
            })
        })
        .collect()
}

/// Event group descriptor (B10 §6.2.34), group_type 1 (common event):
/// the same programme on the services of the multi-channel group.
fn common_event(event_id: u16, services: &[u16]) -> Descriptor {
    Descriptor::EventGroup(EventGroup {
        group_type: 1,
        events: services
            .iter()
            .map(|&service_id| EventRef {
                service_id,
                event_id,
            })
            .collect(),
        other_network_events: vec![],
        private_data: vec![],
    })
}

/// Full descriptor loop of an HD event (EIT p/f and schedule basic):
/// short event, [extended event], component, content, copy control,
/// audio component(s), data content(s), event group.
fn hd_event(p: &Programme, with_extended: bool) -> EitEvent {
    let mut d = vec![Descriptor::ShortEvent(ShortEvent {
        language: *b"jpn",
        event_name: s(p.name),
        text: s(p.text),
    })];
    if with_extended && !p.items.is_empty() {
        d.extend(extended_event(p.items, 24));
    }
    d.push(Descriptor::Component(Component {
        reserved_future_use: 0xF,
        stream_content: 0x1,
        component_type: 0xB3, // 1080i 16:9 without pan vectors (Table 6-5)
        component_tag: 0x00,
        language: *b"jpn",
        text: AribString::default(),
    }));
    d.push(Descriptor::Content(Content {
        items: p
            .genres
            .iter()
            .map(|&(l1, l2)| ContentItem {
                content_nibble_level_1: l1,
                content_nibble_level_2: l2,
                user_nibble_1: 0xF,
                user_nibble_2: 0xF,
            })
            .collect(),
    }));
    d.push(copy_control(0b0100, Some(66))); // 16.5 Mbit/s
    let audio = |tag: u8, main: bool, text: &str| {
        Descriptor::AudioComponent(AudioComponent {
            reserved_future_use_1: 0xF,
            stream_content: 0x2,
            component_type: 0x03, // 2/0 stereo (Table 6-43)
            component_tag: tag,
            stream_type: 0x0F,
            simulcast_group_tag: 0xFF,
            main_component_flag: main,
            quality_indicator: 0b10,
            sampling_rate: 0b111, // 48 kHz (Table 6-45)
            reserved_future_use_2: true,
            language: *b"jpn",
            language_2: None,
            text: if text.is_empty() {
                AribString::default()
            } else {
                s(text)
            },
        })
    };
    if p.dual_audio {
        d.push(audio(0x10, true, "主音声"));
        d.push(audio(0x11, false, "副音声（解説）"));
    } else {
        d.push(audio(
            0x10,
            true,
            if p.event_id.is_multiple_of(2) {
                ""
            } else {
                "ステレオ"
            },
        ));
    }
    // data_content_descriptor (B10 §6.2.28): captions (data_component_id
    // 0x0008, selector = num_languages, language_tag/DMF, ISO 639 code)
    // and, for some events, a data broadcast (0x000C) with component refs.
    d.push(Descriptor::DataContent(DataContent {
        data_component_id: 0x0008,
        entry_component: 0x30,
        selector: vec![0x01, 0x1F, b'j', b'p', b'n'],
        component_refs: vec![],
        language: *b"jpn",
        text: AribString::default(),
    }));
    if !p.event_id.is_multiple_of(2) {
        d.push(Descriptor::DataContent(DataContent {
            data_component_id: 0x000C,
            entry_component: 0x40,
            selector: vec![0x33, 0x3F, 0x00, 0x03, 0x00, 0x00, 0xFF, 0xBF],
            component_refs: vec![0x40, 0x50, 0x51, 0x52, 0x53],
            language: *b"jpn",
            text: AribString::default(),
        }));
    }
    d.push(common_event(p.event_id, &[SUB[0], SUB[1]]));
    event(p, d)
}

fn event(p: &Programme, descriptors: Vec<Descriptor>) -> EitEvent {
    EitEvent {
        event_id: p.event_id,
        start_time: jst(p.day, p.hour, p.minute),
        duration: BcdDuration::from_hms(p.minutes / 60, p.minutes % 60, 0).unwrap(),
        running_status: 0,
        free_ca_mode: false,
        descriptors,
    }
}

/// The same programme on a sub-channel: only an event group descriptor
/// pointing at the HD service's event.
fn sub_event(p: &Programme) -> EitEvent {
    event(p, vec![common_event(p.event_id, &[HD])])
}

#[allow(clippy::too_many_arguments)]
fn eit(
    table_id: u8,
    service_id: u16,
    version: u8,
    section: u8,
    last_section: u8,
    segment_last: u8,
    last_table_id: u8,
    events: Vec<EitEvent>,
) -> Vec<u8> {
    Long {
        header: long_header(table_id, service_id, version, section, last_section),
        body: EitBody {
            transport_stream_id: NETWORK_ID,
            original_network_id: NETWORK_ID,
            segment_last_section_number: segment_last,
            last_table_id,
            events,
        },
    }
    .encode()
    .unwrap()
}

/// EIT p/f actual (table_id 0x4E): section 0 present, section 1 following.
fn eit_pf() -> Vec<Vec<u8>> {
    let mut v = Vec::new();
    for (sec, p) in [(0u8, &PROGRAMMES[2]), (1, &PROGRAMMES[3])] {
        v.push(eit(0x4E, HD, 5, sec, 1, 1, 0x4E, vec![hd_event(p, true)]));
        for &sid in &SUB {
            v.push(eit(0x4E, sid, 5, sec, 1, 1, 0x4E, vec![sub_event(p)]));
        }
        // no programme on the temporary service: empty sections
        v.push(eit(0x4E, TEMP, 2, sec, 1, 1, 0x4E, vec![]));
    }
    v
}

/// One-seg L-EIT (PID 0x0027): short event, content and copy control
/// with user_defined 0b1000 and maximum_bitrate 2 × 1/4 Mbit/s.
fn l_eit() -> Vec<Vec<u8>> {
    [(0u8, &PROGRAMMES[2]), (1, &PROGRAMMES[3])]
        .into_iter()
        .map(|(sec, p)| {
            let d = vec![
                Descriptor::ShortEvent(ShortEvent {
                    language: *b"jpn",
                    event_name: s(p.name),
                    text: s(p.text),
                }),
                Descriptor::Content(Content {
                    items: vec![ContentItem {
                        content_nibble_level_1: p.genres[0].0,
                        content_nibble_level_2: p.genres[0].1,
                        user_nibble_1: 0xF,
                        user_nibble_2: 0xF,
                    }],
                }),
                copy_control(0b1000, Some(2)),
            ];
            eit(0x4E, ONESEG, 7, sec, 1, 1, 0x4E, vec![event(p, d)])
        })
        .collect()
}

/// EIT schedule basic (0x50/0x51) and extended (0x58/0x59): one section
/// per 3-hour segment (section_number = 8 × segment, B10 Part 2 §5.2.7
/// / TR-B14), last_section_number 0xF8.
fn eit_schedule() -> Vec<Vec<u8>> {
    let mut v = Vec::new();
    // basic, HD: segments 1..=2 carry the programmes, 3..=7 are empty
    let seg = |p: &Programme| (p.hour / 3) * 8;
    let mut segs: Vec<u8> = PROGRAMMES.iter().map(seg).collect();
    segs.dedup();
    for &sn in &segs {
        let hd: Vec<EitEvent> = PROGRAMMES
            .iter()
            .filter(|p| seg(p) == sn)
            .map(|p| hd_event(p, false))
            .collect();
        let sub: Vec<EitEvent> = PROGRAMMES
            .iter()
            .filter(|p| seg(p) == sn)
            .map(sub_event)
            .collect();
        v.push(eit(0x50, HD, 9, sn, 0xF8, sn, 0x51, hd));
        for &sid in &SUB {
            v.push(eit(0x50, sid, 9, sn, 0xF8, sn, 0x51, sub.clone()));
        }
    }
    for sn in (24u8..=56).step_by(8) {
        v.push(eit(0x50, HD, 9, sn, 0xF8, sn, 0x51, vec![]));
    }
    // basic, second table_id (days 5–8) of a sub-channel: one event, rest empty
    let later = Programme {
        event_id: 0x1100,
        day: 19,
        ..PROGRAMMES[0]
    };
    v.push(eit(
        0x51,
        SUB[0],
        4,
        0,
        0xF8,
        0,
        0x51,
        vec![sub_event(&later)],
    ));
    for sn in [8u8, 16, 24] {
        v.push(eit(0x51, SUB[0], 4, sn, 0xF8, sn, 0x51, vec![]));
    }
    // extended, HD: events with only extended event descriptors
    for &sn in &segs {
        let ev: Vec<EitEvent> = PROGRAMMES
            .iter()
            .filter(|p| seg(p) == sn && !p.items.is_empty())
            .map(|p| event(p, extended_event(p.items, 12)))
            .collect();
        if !ev.is_empty() {
            v.push(eit(0x58, HD, 3, sn, 0xF8, sn, 0x59, ev));
        }
    }
    for sn in [0u8, 8, 16] {
        v.push(eit(0x59, SUB[0], 2, sn, 0xF8, sn, 0x59, vec![]));
    }
    v
}

/// TOT (0x73) without and with a local time offset descriptor, TDT (0x70).
fn tot_tdt() -> Vec<Vec<u8>> {
    let short = |table_id| ShortHeader {
        table_id,
        private_indicator: true,
        reserved1: 0b11,
    };
    let tot = |t: MjdTime, descriptors| {
        Short {
            header: short(0x73),
            body: TotBody {
                jst_time: t,
                reserved: 0xF,
                descriptors,
            },
        }
        .encode()
        .unwrap()
    };
    let offset = Descriptor::LocalTimeOffset(LocalTimeOffset {
        entries: vec![LocalTimeOffsetEntry {
            country_code: *b"JPN",
            country_region_id: 0,
            reserved: true,
            local_time_offset_polarity: false,
            local_time_offset: BcdOffset::from_hm(0, 0).unwrap(),
            time_of_change: jst(20, 2, 0),
            next_time_offset: BcdOffset::from_hm(1, 0).unwrap(),
        }],
    });
    vec![
        tot(jst(15, 6, 45), vec![]),
        tot(jst(15, 6, 50), vec![offset]),
        Short {
            header: short(0x70),
            body: TdtBody {
                jst_time: jst(15, 6, 55),
            },
        }
        .encode()
        .unwrap(),
    ]
}
