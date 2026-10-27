//! Spec vectors: values printed in ARIB STD-B10 v5.13 Part 2, and sections
//! built field by field from the syntax tables (each `f(name, bits, value)`
//! row mirrors one row of the cited table), then decoded and checked.

use isdb_si::any::AnyTable;
use isdb_si::descriptor::*;
use isdb_si::tables::*;
use isdb_si::time::*;
use isdb_ts::Pid;
use isdb_ts::bits::BitWriter;
use isdb_ts::crc::crc32;
use isdb_ts::section::{LongBody, LongHeader};

/// Serialise `(name, bits, value)` rows MSB first (B10 Part 2 §3.2:
/// uimsbf/bslbf).
fn fields(rows: &[(&'static str, u32, u64)]) -> Vec<u8> {
    let mut w = BitWriter::new();
    for &(name, bits, v) in rows {
        w.write(name, bits, v).unwrap();
    }
    w.into_bytes()
}

fn with_crc(mut v: Vec<u8>) -> Vec<u8> {
    let c = crc32(&v);
    v.extend_from_slice(&c.to_be_bytes());
    v
}

fn desc(tag: u8, body: &[u8]) -> Vec<u8> {
    let mut v = vec![tag, body.len() as u8];
    v.extend_from_slice(body);
    v
}

/// Long-section header rows (B10 Part 2 Tables 5-3 … 5-17, first 8 bytes).
fn long_header_rows(
    table_id: u8,
    section_length: usize,
    ext: u16,
    version: u8,
    sn: u8,
    lsn: u8,
) -> Vec<u8> {
    fields(&[
        ("table_id", 8, u64::from(table_id)),
        ("section_syntax_indicator", 1, 1),
        ("reserved_future_use", 1, 1),
        ("reserved", 2, 0b11),
        ("section_length", 12, section_length as u64),
        ("table_id_extension", 16, u64::from(ext)),
        ("reserved", 2, 0b11),
        ("version_number", 5, u64::from(version)),
        ("current_next_indicator", 1, 1),
        ("section_number", 8, u64::from(sn)),
        ("last_section_number", 8, u64::from(lsn)),
    ])
}

fn long_section(table_id: u8, ext: u16, version: u8, sn: u8, lsn: u8, body: &[u8]) -> Vec<u8> {
    let mut v = long_header_rows(table_id, 5 + body.len() + 4, ext, version, sn, lsn);
    v.extend_from_slice(body);
    with_crc(v)
}

fn roundtrip(pid: Pid, sec: &[u8]) -> AnyTable {
    let t = AnyTable::decode(pid, sec).unwrap();
    assert_eq!(t.encode().unwrap(), sec, "{} re-encode", t.name());
    assert!(
        isdb_si::conformance::check(pid, &t).is_empty(),
        "{:?}",
        isdb_si::conformance::check(pid, &t)
    );
    t
}

// ---------------------------------------------------------------------------
// time (Annex C, §5.2.7, §5.2.8)
// ---------------------------------------------------------------------------

/// Annex C worked example: MJD 45218 ⇔ 1982-09-06, WD 1 (Monday), W 4315,
/// WY 82, WN 36.
#[test]
fn spec_time_annex_c_example() {
    let d = mjd_to_date(45_218).unwrap();
    assert_eq!(
        d,
        Date {
            year: 1982,
            month: 9,
            day: 6
        }
    );
    assert_eq!(date_to_mjd(d), Some(45_218));
    assert_eq!(weekday(45_218), 1);
    assert_eq!(mjd_to_week(45_218), (4315, 82, 36));
    assert_eq!(week_to_mjd(82, 36, 1), 45_218);
}

/// §5.2.7 Example 1 / §5.2.8: "93/10/13 12:45:00 is coded as 0xC079124500".
#[test]
fn spec_time_jst_example() {
    let dt = DateTime {
        date: Date {
            year: 1993,
            month: 10,
            day: 13,
        },
        hour: 12,
        minute: 45,
        second: 0,
    };
    let t = MjdTime::from_jst(dt).unwrap();
    assert_eq!(t.to_u40(), 0xC0_79_12_45_00);
    assert_eq!(MjdTime::from_u40(0xC0_79_12_45_00).to_jst(), Some(dt));
    // JST is UTC + 9 h (§3.1).
    let utc = MjdTime::from_u40(0xC0_79_12_45_00).to_utc().unwrap();
    assert_eq!(
        utc,
        DateTime {
            date: Date {
                year: 1993,
                month: 10,
                day: 13
            },
            hour: 3,
            minute: 45,
            second: 0
        }
    );
    assert_eq!(MjdTime::from_utc(utc).unwrap(), t);
    // Day rollover when converting to UTC.
    let early = MjdTime::from_jst(DateTime {
        date: Date {
            year: 2000,
            month: 3,
            day: 1,
        },
        hour: 5,
        minute: 0,
        second: 0,
    })
    .unwrap();
    assert_eq!(
        early.to_utc().unwrap(),
        DateTime {
            date: Date {
                year: 2000,
                month: 2,
                day: 29
            },
            hour: 20,
            minute: 0,
            second: 0
        }
    );
}

/// §5.2.7 Example 2: "01:45:30 is coded as 0x014530"; all '1' = undefined.
#[test]
fn spec_time_duration_example() {
    let d = BcdDuration::from_hms(1, 45, 30).unwrap();
    assert_eq!(d.0, 0x01_45_30);
    assert_eq!(d.seconds(), Some(6330));
    assert!(BcdDuration(0xFF_FFFF).is_undefined());
    assert_eq!(BcdDuration(0xFF_FFFF).hms(), None);
    assert!(MjdTime::from_u40(0xFF_FFFF_FFFF).is_undefined());
    assert_eq!(MjdTime::UNDEFINED.to_jst(), None);
}

#[test]
fn spec_time_bcd() {
    assert_eq!(bcd_to_u8(0x45), Some(45));
    assert_eq!(bcd_to_u8(0x99), Some(99));
    assert_eq!(bcd_to_u8(0x4A), None);
    assert_eq!(bcd_to_u8(0xA0), None);
    assert_eq!(u8_to_bcd(59).unwrap(), 0x59);
    assert!(u8_to_bcd(100).is_err());
    assert_eq!(BcdOffset(0x0900).hm(), Some((9, 0)));
    assert_eq!(BcdOffset::from_hm(1, 30).unwrap().0, 0x0130);
    // Invalid digits in JST_time are rejected by the civil conversion.
    assert_eq!(MjdTime::from_u40(0xC0_79_1A_45_00).to_jst(), None);
    assert_eq!(MjdTime::from_u40(0xC0_79_24_00_00).to_jst(), None);
}

/// Annex C Note: formulas valid 1900-03-01 … 2100-02-28.  Across that
/// whole range MJD→date→MJD is the identity and consecutive MJDs are
/// consecutive calendar days.
#[test]
fn spec_time_annex_c_range_exhaustive() {
    assert_eq!(
        mjd_to_date(MJD_MIN),
        Some(Date {
            year: 1900,
            month: 3,
            day: 1
        })
    );
    assert_eq!(
        mjd_to_date(MJD_MAX),
        Some(Date {
            year: 2100,
            month: 2,
            day: 28
        })
    );
    assert_eq!(mjd_to_date(MJD_MIN - 1), None);
    assert_eq!(mjd_to_date(MJD_MAX + 1), None);
    // 16-bit MJD limit (§5.2.8 Note: "up to April 22, 2038").
    assert_eq!(
        mjd_to_date(0xFFFF),
        Some(Date {
            year: 2038,
            month: 4,
            day: 22
        })
    );
    let mut prev = mjd_to_date(MJD_MIN).unwrap();
    for mjd in MJD_MIN + 1..=MJD_MAX {
        let d = mjd_to_date(mjd).unwrap();
        assert_eq!(date_to_mjd(d), Some(mjd));
        let next = if prev.day < days_in_month(prev.year, prev.month) {
            Date {
                day: prev.day + 1,
                ..prev
            }
        } else if prev.month < 12 {
            Date {
                year: prev.year,
                month: prev.month + 1,
                day: 1,
            }
        } else {
            Date {
                year: prev.year + 1,
                month: 1,
                day: 1,
            }
        };
        assert_eq!(d, next, "mjd {mjd}");
        prev = d;
    }
    assert_eq!(
        date_to_mjd(Date {
            year: 2001,
            month: 2,
            day: 29
        }),
        None
    );
}

/// §6.2.3 EXAMPLE: "jpn" is "0110 1010 0111 0000 0110 1110"; §6.2.5 "JPN"
/// is "0100 1010 0101 0000 0100 1110".
#[test]
fn spec_descriptor_language_codes() {
    let d = Component::decode(&[0xF0 | 0x1, 0xB3, 0x00, 0x6A, 0x70, 0x6E]).unwrap();
    assert_eq!(&d.language, b"jpn");
    let p = ParentalRating::decode(&[0x4A, 0x50, 0x4E, 0x04]).unwrap();
    assert_eq!(&p.ratings[0].country_code, b"JPN");
    // Table 6-23 EXAMPLE: 0x04 → at least 7 years old (rating + 3).
    assert_eq!(p.ratings[0].rating + 3, 7);
}

// ---------------------------------------------------------------------------
// tables
// ---------------------------------------------------------------------------

/// NIT, Table 5-3, with network_name (Table 6-21), service_list (Table
/// 6-26), terrestrial_delivery_system (Table 6-61), partial_reception
/// (Table 6-64) and system_management (Table 6-33) descriptors.
#[test]
fn spec_nit() {
    let name = desc(0x40, b"NET");
    let sysm = desc(0xFE, &[0x03, 0x01]);
    let slist = desc(0x41, &[0x04, 0x00, 0x01, 0x04, 0x08, 0xC0]);
    let tdsd = desc(
        0xFA,
        &fields(&[
            ("area_code", 12, 0x5A5),
            ("guard_interval", 2, 0b10),
            ("transmission_mode", 2, 0b10),
            ("frequency", 16, 3900),
        ]),
    );
    let pr = desc(0xFB, &[0x04, 0x08]);
    let net: Vec<u8> = [name.clone(), sysm.clone()].concat();
    let tsd: Vec<u8> = [slist, tdsd, pr].concat();
    let mut ts_loop = fields(&[
        ("transport_stream_id", 16, 0x79EE),
        ("original_network_id", 16, 0x79EE),
        ("reserved_future_use", 4, 0xF),
        ("transport_descriptors_length", 12, tsd.len() as u64),
    ]);
    ts_loop.extend_from_slice(&tsd);
    let mut body = fields(&[
        ("reserved_future_use", 4, 0xF),
        ("network_descriptors_length", 12, net.len() as u64),
    ]);
    body.extend_from_slice(&net);
    body.extend(fields(&[
        ("reserved_future_use", 4, 0xF),
        ("transport_stream_loop_length", 12, ts_loop.len() as u64),
    ]));
    body.extend_from_slice(&ts_loop);
    let sec = long_section(0x40, 0x79EE, 3, 0, 0, &body);
    let AnyTable::Nit(n) = roundtrip(Pid::new(0x0010).unwrap(), &sec) else {
        panic!()
    };
    assert_eq!(n.header.table_id_extension, 0x79EE);
    assert_eq!(n.header.version_number, 3);
    assert_eq!(
        n.body.0.descriptors[0],
        Descriptor::NetworkName(NetworkName {
            name: isdb_text::AribString(b"NET".to_vec())
        })
    );
    let Descriptor::SystemManagement(sm) = &n.body.0.descriptors[1] else {
        panic!()
    };
    assert_eq!(
        (
            sm.broadcasting_flag(),
            sm.broadcasting_identifier(),
            sm.additional_broadcasting_identification()
        ),
        (0, 3, 1)
    );
    let ts = &n.body.0.transport_streams[0];
    assert_eq!(ts.reserved_future_use, 0xF);
    let Descriptor::ServiceList(sl) = &ts.descriptors[0] else {
        panic!()
    };
    assert_eq!(
        sl.services,
        vec![
            ServiceListEntry {
                service_id: 0x0400,
                service_type: 0x01
            },
            ServiceListEntry {
                service_id: 0x0408,
                service_type: 0xC0
            },
        ]
    );
    let Descriptor::TerrestrialDeliverySystem(t) = &ts.descriptors[1] else {
        panic!()
    };
    assert_eq!(
        (
            t.area_code,
            t.guard_interval,
            t.transmission_mode,
            t.frequencies.clone()
        ),
        (0x5A5, 2, 2, vec![3900])
    );
    assert!((TerrestrialDeliverySystem::frequency_hz(3900) - 557_142_857.142_857).abs() < 1e-3);
    assert_eq!(
        ts.descriptors[2],
        Descriptor::PartialReception(PartialReception {
            service_ids: vec![0x0408]
        })
    );
}

/// BAT, Table 5-4 (same loops as NIT; bouquet_name Table 6-2).
#[test]
fn spec_bat() {
    let bq = [desc(0x47, b"BQ"), desc(0x41, &[0x00, 0x10, 0x01])].concat();
    let mut body = fields(&[
        ("reserved_future_use", 4, 0xF),
        ("bouquet_descriptors_length", 12, bq.len() as u64),
    ]);
    body.extend_from_slice(&bq);
    let ts = fields(&[
        ("transport_stream_id", 16, 0x0001),
        ("original_network_id", 16, 0x0004),
        ("reserved_future_use", 4, 0xF),
        ("transport_descriptors_length", 12, 0),
    ]);
    body.extend(fields(&[
        ("reserved_future_use", 4, 0xF),
        ("transport_stream_loop_length", 12, ts.len() as u64),
    ]));
    body.extend_from_slice(&ts);
    let sec = long_section(0x4A, 0x1234, 0, 0, 0, &body);
    let AnyTable::Bat(b) = roundtrip(Pid::new(0x0011).unwrap(), &sec) else {
        panic!()
    };
    assert_eq!(b.header.table_id_extension, 0x1234);
    assert_eq!(
        b.body.0.descriptors[0],
        Descriptor::BouquetName(BouquetName {
            name: isdb_text::AribString(b"BQ".to_vec())
        })
    );
    assert_eq!(b.body.0.transport_streams[0].original_network_id, 0x0004);
}

/// SDT, Table 5-5, service loop with the ARIB 3+3+1+1 flag layout and a
/// service_descriptor (Table 6-24).
#[test]
fn spec_sdt() {
    let sd = desc(0x48, &[0x01, 0x02, b'P', b'V', 0x03, b'S', b'V', b'C']);
    let mut body = fields(&[
        ("original_network_id", 16, 0x79EE),
        ("reserved_future_use", 8, 0xFF),
    ]);
    body.extend(fields(&[
        ("service_id", 16, 0x0E08),
        ("reserved_future_use", 3, 0b111),
        ("EIT_user_defined_flags", 3, 0b101),
        ("EIT_schedule_flag", 1, 1),
        ("EIT_present_following_flag", 1, 0),
        ("running_status", 3, 4),
        ("free_CA_mode", 1, 1),
        ("descriptors_loop_length", 12, sd.len() as u64),
    ]));
    body.extend_from_slice(&sd);
    let sec = long_section(0x42, 0x79EE, 1, 0, 0, &body);
    let AnyTable::Sdt(s) = roundtrip(Pid::new(0x0011).unwrap(), &sec) else {
        panic!()
    };
    assert_eq!(s.body.original_network_id, 0x79EE);
    let sv = &s.body.services[0];
    assert_eq!(
        (
            sv.service_id,
            sv.reserved_future_use,
            sv.eit_user_defined_flags,
            sv.eit_schedule_flag,
            sv.eit_present_following_flag,
            sv.running_status,
            sv.free_ca_mode
        ),
        (0x0E08, 0b111, 0b101, true, false, 4, true)
    );
    let Descriptor::Service(x) = &sv.descriptors[0] else {
        panic!()
    };
    assert_eq!(
        (
            x.service_type,
            &x.provider_name.0[..],
            &x.service_name.0[..]
        ),
        (0x01, &b"PV"[..], &b"SVC"[..])
    );
}

/// EIT, Table 5-7: start_time (§5.2.7 Example 1), duration (Example 2),
/// short_event (Table 6-27), extended_event (Table 6-12), content (Table
/// 6-6), component (Table 6-4), audio_component (Table 6-42).
#[test]
fn spec_eit() {
    let se = desc(
        0x4D,
        &[b'j', b'p', b'n', 2, b'E', b'V', 3, b'T', b'X', b'T'],
    );
    let ee = desc(
        0x4E,
        &[
            0x01, b'j', b'p', b'n', 6, 2, b'D', b'1', 2, b'I', b'1', 1, b'X',
        ],
    );
    let ct = desc(0x54, &[0x10, 0xFF]);
    let cp = desc(
        0x50,
        &fields(&[
            ("reserved_future_use", 4, 0xF),
            ("stream_content", 4, 0x1),
            ("component_type", 8, 0xB3),
            ("component_tag", 8, 0x00),
            ("ISO_639_language_code", 24, 0x6A706E),
        ]),
    );
    let ac = desc(
        0xC4,
        &fields(&[
            ("reserved_future_use", 4, 0xF),
            ("stream_content", 4, 0x2),
            ("component_type", 8, 0x02),
            ("component_tag", 8, 0x10),
            ("stream_type", 8, 0x0F),
            ("simulcast_group_tag", 8, 0xFF),
            ("ES_multi_lingual_flag", 1, 1),
            ("main_component_flag", 1, 1),
            ("quality_indicator", 2, 0b11),
            ("sampling_rate", 3, 0b111),
            ("reserved_future_use", 1, 1),
            ("ISO_639_language_code", 24, 0x6A706E),
            ("ISO_639_language_code_2", 24, 0x656E67),
        ]),
    );
    let ds: Vec<u8> = [se, ee, ct, cp, ac].concat();
    let mut body = fields(&[
        ("transport_stream_id", 16, 0x79EE),
        ("original_network_id", 16, 0x79EE),
        ("segment_last_section_number", 8, 0),
        ("last_table_id", 8, 0x4E),
    ]);
    body.extend(fields(&[
        ("event_id", 16, 0x1234),
        ("start_time", 40, 0xC0_79_12_45_00),
        ("duration", 24, 0x01_45_30),
        ("running_status", 3, 4),
        ("free_CA_mode", 1, 0),
        ("descriptors_loop_length", 12, ds.len() as u64),
    ]));
    body.extend_from_slice(&ds);
    let sec = long_section(0x4E, 0x0E08, 7, 0, 1, &body);
    let AnyTable::Eit(e) = roundtrip(Pid::new(0x0012).unwrap(), &sec) else {
        panic!()
    };
    assert_eq!(
        (
            e.header.table_id_extension,
            e.body.transport_stream_id,
            e.body.last_table_id
        ),
        (0x0E08, 0x79EE, 0x4E)
    );
    let ev = &e.body.events[0];
    assert_eq!(ev.event_id, 0x1234);
    assert_eq!(
        ev.start_time.to_jst().unwrap().to_string(),
        "1993-10-13 12:45:00"
    );
    assert_eq!(ev.duration.hms(), Some((1, 45, 30)));
    let Descriptor::ShortEvent(s) = &ev.descriptors[0] else {
        panic!()
    };
    assert_eq!(
        (&s.language, &s.event_name.0[..], &s.text.0[..]),
        (b"jpn", &b"EV"[..], &b"TXT"[..])
    );
    let Descriptor::ExtendedEvent(x) = &ev.descriptors[1] else {
        panic!()
    };
    assert_eq!((x.descriptor_number, x.last_descriptor_number), (0, 1));
    assert_eq!(
        x.items,
        vec![ExtendedEventItem {
            description: isdb_text::AribString(b"D1".to_vec()),
            item: isdb_text::AribString(b"I1".to_vec())
        }]
    );
    assert_eq!(x.text.0, b"X");
    let Descriptor::Content(c) = &ev.descriptors[2] else {
        panic!()
    };
    assert_eq!(
        c.items[0],
        ContentItem {
            content_nibble_level_1: 1,
            content_nibble_level_2: 0,
            user_nibble_1: 0xF,
            user_nibble_2: 0xF
        }
    );
    let Descriptor::Component(c) = &ev.descriptors[3] else {
        panic!()
    };
    assert_eq!(
        (
            c.reserved_future_use,
            c.stream_content,
            c.component_type,
            c.text.0.len()
        ),
        (0xF, 1, 0xB3, 0)
    );
    let Descriptor::AudioComponent(a) = &ev.descriptors[4] else {
        panic!()
    };
    assert_eq!(
        (
            a.stream_type,
            a.simulcast_group_tag,
            a.quality_indicator,
            a.sampling_rate
        ),
        (0x0F, 0xFF, 3, 7)
    );
    assert_eq!(a.language_2, Some(*b"eng"));
}

/// EIT event with undefined start_time and duration (all '1', §5.2.7).
#[test]
fn spec_eit_undefined_times() {
    let mut body = fields(&[
        ("transport_stream_id", 16, 1),
        ("original_network_id", 16, 1),
        ("segment_last_section_number", 8, 0),
        ("last_table_id", 8, 0x50),
    ]);
    let se = desc(0x4D, &[b'j', b'p', b'n', 0, 0]);
    body.extend(fields(&[
        ("event_id", 16, 1),
        ("start_time", 40, 0xFF_FFFF_FFFF),
        ("duration", 24, 0xFF_FFFF),
        ("running_status", 3, 0),
        ("free_CA_mode", 1, 0),
        ("descriptors_loop_length", 12, se.len() as u64),
    ]));
    body.extend_from_slice(&se);
    let sec = long_section(0x50, 1, 0, 0, 0, &body);
    let AnyTable::Eit(e) = roundtrip(Pid::new(0x0012).unwrap(), &sec) else {
        panic!()
    };
    assert!(e.body.events[0].start_time.is_undefined());
    assert!(e.body.events[0].duration.is_undefined());
}

/// TDT, Table 5-8: section_length 0x005, no CRC_32.
#[test]
fn spec_tdt() {
    let sec = fields(&[
        ("table_id", 8, 0x70),
        ("section_syntax_indicator", 1, 0),
        ("reserved_future_use", 1, 1),
        ("reserved", 2, 0b11),
        ("section_length", 12, 5),
        ("JST_time", 40, 0xC0_79_12_45_00),
    ]);
    assert_eq!(sec, [0x70, 0x70, 0x05, 0xC0, 0x79, 0x12, 0x45, 0x00]);
    let AnyTable::Tdt(t) = roundtrip(Pid::new(0x0014).unwrap(), &sec) else {
        panic!()
    };
    assert_eq!(t.body.jst_time.to_u40(), 0xC0_79_12_45_00);
}

/// TOT, Table 5-9: short form with CRC_32 and a local_time_offset
/// descriptor (Table 6-41).
#[test]
fn spec_tot() {
    let lto = desc(
        0x58,
        &fields(&[
            ("country_code", 24, 0x4A504E),
            ("country_region_id", 6, 0),
            ("reserved", 1, 1),
            ("local_time_offset_polarity", 1, 0),
            ("local_time_offset", 16, 0x0000),
            ("time_of_change", 40, 0xC0_79_00_00_00),
            ("next_time_offset", 16, 0x0100),
        ]),
    );
    let mut v = fields(&[
        ("table_id", 8, 0x73),
        ("section_syntax_indicator", 1, 0),
        ("reserved_future_use", 1, 1),
        ("reserved", 2, 0b11),
        ("section_length", 12, (5 + 2 + lto.len() + 4) as u64),
        ("JST_time", 40, 0xC0_79_12_45_00),
        ("reserved", 4, 0xF),
        ("descriptors_loop_length", 12, lto.len() as u64),
    ]);
    v.extend_from_slice(&lto);
    let sec = with_crc(v);
    let AnyTable::Tot(t) = roundtrip(Pid::new(0x0014).unwrap(), &sec) else {
        panic!()
    };
    let Descriptor::LocalTimeOffset(l) = &t.body.descriptors[0] else {
        panic!()
    };
    let e = &l.entries[0];
    assert_eq!(
        (&e.country_code, e.reserved, e.local_time_offset_polarity),
        (b"JPN", true, false)
    );
    assert_eq!(
        (e.local_time_offset.hm(), e.next_time_offset.hm()),
        (Some((0, 0)), Some((1, 0)))
    );
    assert_eq!(
        e.time_of_change.to_jst().unwrap().to_string(),
        "1993-10-13 00:00:00"
    );
    // A corrupted TOT is an error, not silently accepted.
    let mut bad = sec.clone();
    bad[5] ^= 1;
    assert!(AnyTable::decode(Pid::new(0x0014).unwrap(), &bad).is_err());
}

/// RST, Table 5-10 (no CRC_32).
#[test]
fn spec_rst() {
    let mut sec = fields(&[
        ("table_id", 8, 0x71),
        ("section_syntax_indicator", 1, 0),
        ("reserved_future_use", 1, 1),
        ("reserved", 2, 0b11),
        ("section_length", 12, 9),
    ]);
    sec.extend(fields(&[
        ("transport_stream_id", 16, 1),
        ("original_network_id", 16, 2),
        ("service_id", 16, 3),
        ("event_id", 16, 4),
        ("reserved_future_use", 5, 0x1F),
        ("running_status", 3, 2),
    ]));
    let AnyTable::Rst(r) = roundtrip(Pid::new(0x0013).unwrap(), &sec) else {
        panic!()
    };
    assert_eq!(
        r.body.entries[0],
        RstEntry {
            transport_stream_id: 1,
            original_network_id: 2,
            service_id: 3,
            event_id: 4,
            reserved_future_use: 0x1F,
            running_status: 2
        }
    );
}

/// ST, Table 5-11: section_syntax_indicator may be '1' or '0'.
#[test]
fn spec_st() {
    for ssi in [0u64, 1] {
        let mut sec = fields(&[
            ("table_id", 8, 0x72),
            ("section_syntax_indicator", 1, ssi),
            ("reserved_future_use", 1, 1),
            ("reserved", 2, 0b11),
            ("section_length", 12, 3),
        ]);
        sec.extend_from_slice(&[0xAA, 0xBB, 0xCC]);
        let AnyTable::St(s) = roundtrip(Pid::new(0x0011).unwrap(), &sec) else {
            panic!()
        };
        assert_eq!(
            (s.section_syntax_indicator, &s.data[..]),
            (ssi == 1, &[0xAA, 0xBB, 0xCC][..])
        );
    }
    // Not allowed on PID 0x0014 (Table 5-1): stays raw.
    let sec = [0x72, 0x70, 0x00];
    assert!(matches!(
        AnyTable::decode(Pid::new(0x0014).unwrap(), &sec).unwrap(),
        AnyTable::Unknown(_)
    ));
}

/// PCAT, Table 5-12.
#[test]
fn spec_pcat() {
    let dc = desc(
        0xC7,
        &[0x00, 0x0C, 0x40, 0x00, 0x00, b'j', b'p', b'n', 0x00],
    );
    let sched = fields(&[
        ("start_time", 40, 0xC0_79_12_45_00),
        ("duration", 24, 0x00_30_00),
    ]);
    let mut body = fields(&[
        ("transport_stream_id", 16, 0x79EE),
        ("original_network_id", 16, 0x79EE),
        ("content_id", 32, 0xDEAD_BEEF),
        ("num_of_content_version", 8, 1),
        ("content_version", 16, 2),
        ("content_minor_version", 16, 3),
        ("version_indicator", 2, 0b11),
        ("reserved_future_use", 2, 0b11),
        (
            "content_descriptor_length",
            12,
            (sched.len() + dc.len()) as u64,
        ),
        ("reserved_future_use", 4, 0xF),
        ("schedule_description_length", 12, sched.len() as u64),
    ]);
    body.extend_from_slice(&sched);
    body.extend_from_slice(&dc);
    let sec = long_section(0xC2, 0x0F88, 0, 0, 0, &body);
    let AnyTable::Pcat(p) = roundtrip(Pid::new(0x0022).unwrap(), &sec) else {
        panic!()
    };
    assert_eq!(p.body.content_id, 0xDEAD_BEEF);
    let c = &p.body.content_versions[0];
    assert_eq!(
        (
            c.content_version,
            c.content_minor_version,
            c.version_indicator
        ),
        (2, 3, 3)
    );
    assert_eq!(c.schedules[0].duration.seconds(), Some(1800));
    let Descriptor::DataContent(d) = &c.descriptors[0] else {
        panic!()
    };
    assert_eq!(
        (
            d.data_component_id,
            d.entry_component,
            d.selector.len(),
            d.component_refs.len()
        ),
        (0x000C, 0x40, 0, 0)
    );
}

/// BIT, Table 5-13, with SI_parameter (Table 6-69), broadcaster_name
/// (Table 6-70) and extended_broadcaster (Table 6-83) descriptors.
#[test]
fn spec_bit() {
    let sip = desc(0xD7, &[0x01, 0xE3, 0xDB, 0x40, 0x01, 0x01]);
    let bn = desc(0xD8, b"BC");
    let eb = desc(
        0xCE,
        &fields(&[
            ("broadcaster_type", 4, 0x1),
            ("reserved_future_use", 4, 0xF),
            ("terrestrial_broadcaster_id", 16, 0x79EE),
            ("number_of_affiliation_id_loop", 4, 1),
            ("number_of_broadcaster_id_loop", 4, 1),
            ("affiliation_id", 8, 0x02),
            ("original_network_id", 16, 0x0004),
            ("broadcaster_id", 8, 0x10),
        ]),
    );
    let mut body = fields(&[
        ("reserved_future_use", 3, 0b111),
        ("broadcast_view_propriety", 1, 1),
        ("first_descriptors_length", 12, sip.len() as u64),
    ]);
    body.extend_from_slice(&sip);
    let bd = [bn, eb].concat();
    body.extend(fields(&[
        ("broadcaster_id", 8, 0xFF),
        ("reserved_future_use", 4, 0xF),
        ("broadcaster_descriptors_length", 12, bd.len() as u64),
    ]));
    body.extend_from_slice(&bd);
    let sec = long_section(0xC4, 0x79EE, 0, 0, 0, &body);
    let AnyTable::Bit(b) = roundtrip(Pid::new(0x0024).unwrap(), &sec) else {
        panic!()
    };
    assert!(b.body.broadcast_view_propriety);
    let Descriptor::SiParameter(s) = &b.body.first_descriptors[0] else {
        panic!()
    };
    assert_eq!(
        (
            s.parameter_version,
            s.update_time,
            s.tables[0].table_id,
            &s.tables[0].description[..]
        ),
        (1, 0xE3DB, 0x40, &[1u8][..])
    );
    let bc = &b.body.broadcasters[0];
    assert_eq!(bc.broadcaster_id, 0xFF);
    let Descriptor::ExtendedBroadcaster(x) = &bc.descriptors[1] else {
        panic!()
    };
    assert_eq!(x.broadcaster_type(), 1);
    let ExtendedBroadcasterBody::TerrestrialTv(t) = &x.body else {
        panic!()
    };
    assert_eq!(
        (t.terrestrial_broadcaster_id, &t.affiliation_ids[..]),
        (0x79EE, &[2u8][..])
    );
    assert_eq!(
        t.broadcasters,
        vec![NetworkBroadcaster {
            original_network_id: 4,
            broadcaster_id: 0x10
        }]
    );
}

/// NBIT, Table 5-14, with a board_information descriptor (Table 6-76).
#[test]
fn spec_nbit() {
    let bi = desc(0xDB, &[1, b'T', 2, b'B', b'D']);
    let mut body = fields(&[
        ("information_id", 16, 0x0101),
        ("information_type", 4, 0x2),
        ("description_body_location", 2, 0b01),
        ("reserved_future_use", 2, 0b11),
        ("user_defined", 8, 0xAB),
        ("number_of_keys", 8, 2),
        ("key_id", 16, 0x0E08),
        ("key_id", 16, 0x0E09),
        ("reserved_future_use", 4, 0xF),
        ("descriptors_loop_length", 12, bi.len() as u64),
    ]);
    body.extend_from_slice(&bi);
    let sec = long_section(0xC5, 0x79EE, 0, 0, 0, &body);
    let AnyTable::Nbit(n) = roundtrip(Pid::new(0x0025).unwrap(), &sec) else {
        panic!()
    };
    let i = &n.body.informations[0];
    assert_eq!(
        (
            i.information_type,
            i.description_body_location,
            i.user_defined,
            &i.key_ids[..]
        ),
        (2, 1, 0xAB, &[0x0E08u16, 0x0E09][..])
    );
    let Descriptor::BoardInformation(b) = &i.descriptors[0] else {
        panic!()
    };
    assert_eq!((&b.title.0[..], &b.text.0[..]), (&b"T"[..], &b"BD"[..]));
}

/// LDT, Table 5-17.
#[test]
fn spec_ldt() {
    let se = desc(0x4D, &[b'j', b'p', b'n', 0, 1, b'x']);
    let mut body = fields(&[
        ("transport_stream_id", 16, 0x79EE),
        ("original_network_id", 16, 0x79EE),
        ("description_id", 16, 0x0001),
        ("reserved_future_use", 12, 0xFFF),
        ("descriptors_loop_length", 12, se.len() as u64),
    ]);
    body.extend_from_slice(&se);
    let sec = long_section(0xC7, 0x0E08, 0, 0, 0, &body);
    let AnyTable::Ldt(l) = roundtrip(Pid::new(0x0025).unwrap(), &sec) else {
        panic!()
    };
    assert_eq!(
        (
            l.header.table_id_extension,
            l.body.descriptions[0].description_id,
            l.body.descriptions[0].reserved_future_use
        ),
        (0x0E08, 1, 0xFFF)
    );
}

// ---------------------------------------------------------------------------
// descriptors with conditional syntax
// ---------------------------------------------------------------------------

/// digital_copy_control_descriptor, Table 6-38, with maximum_bitrate and a
/// component loop.
#[test]
fn spec_descriptor_digital_copy_control() {
    let body = fields(&[
        ("digital_recording_control_data", 2, 0b10),
        ("maximum_bitrate_flag", 1, 1),
        ("component_control_flag", 1, 1),
        ("user_defined", 4, 0xF),
        ("maximum_bitrate", 8, 0x50),
        ("component_control_length", 8, 3),
        ("component_tag", 8, 0x00),
        ("digital_recording_control_data", 2, 0b11),
        ("maximum_bitrate_flag", 1, 1),
        ("reserved_future_use", 1, 1),
        ("user_defined", 4, 0xF),
        ("maximum_bitrate", 8, 0x40),
    ]);
    let d = DigitalCopyControl::decode(&body).unwrap();
    assert_eq!(
        (
            d.digital_recording_control_data,
            d.maximum_bitrate,
            d.user_defined
        ),
        (2, Some(0x50), 0xF)
    );
    assert_eq!(
        d.component_control.as_ref().unwrap()[0],
        ComponentCopyControl {
            component_tag: 0,
            digital_recording_control_data: 3,
            reserved_future_use: true,
            user_defined: 0xF,
            maximum_bitrate: Some(0x40)
        }
    );
    let mut out = Vec::new();
    d.encode(&mut out).unwrap();
    assert_eq!(out, body);
}

/// TS_information_descriptor, Table 6-82.
#[test]
fn spec_descriptor_ts_information() {
    let mut body = fields(&[
        ("remote_control_key_id", 8, 10),
        ("length_of_ts_name", 6, 3),
        ("transmission_type_count", 2, 1),
    ]);
    body.extend_from_slice(b"TST");
    body.extend(fields(&[
        ("transmission_type_info", 8, 0x0F),
        ("num_of_service", 8, 1),
        ("service_id", 16, 0x0E08),
    ]));
    let d = TsInformation::decode(&body).unwrap();
    assert_eq!(
        (d.remote_control_key_id, &d.ts_name.0[..]),
        (10, &b"TST"[..])
    );
    assert_eq!(
        d.transmission_types,
        vec![TransmissionType {
            transmission_type_info: 0x0F,
            service_ids: vec![0x0E08]
        }]
    );
    assert!(d.reserved_future_use.is_empty());
}

/// logo_transmission_descriptor, Table 6-85, types 0x01/0x02/0x03.
#[test]
fn spec_descriptor_logo_transmission() {
    let t1 = fields(&[
        ("logo_transmission_type", 8, 0x01),
        ("reserved_future_use", 7, 0x7F),
        ("logo_id", 9, 0x106),
        ("reserved_future_use", 4, 0xF),
        ("logo_version", 12, 0x003),
        ("download_data_id", 16, 0x0313),
    ]);
    let d = LogoTransmission::decode(&t1).unwrap();
    assert_eq!(
        d.body,
        LogoTransmissionBody::Cdt1 {
            reserved_future_use_1: 0x7F,
            logo_id: 0x106,
            reserved_future_use_2: 0xF,
            logo_version: 3,
            download_data_id: 0x0313
        }
    );
    let t2 = fields(&[
        ("logo_transmission_type", 8, 0x02),
        ("reserved_future_use", 7, 0x7F),
        ("logo_id", 9, 0x106),
    ]);
    assert_eq!(
        LogoTransmission::decode(&t2).unwrap().body,
        LogoTransmissionBody::Cdt2 {
            reserved_future_use: 0x7F,
            logo_id: 0x106
        }
    );
    let d3 = LogoTransmission::decode(&[0x03, b'L']).unwrap();
    assert_eq!(d3.logo_transmission_type(), 3);
    // Trailing bytes after a type-0x02 body are not described by the syntax.
    assert!(LogoTransmission::decode(&[t2.clone(), vec![0]].concat()).is_err());
}

/// event_group_descriptor, Table 6-67, group_type 0x4 (relay to other
/// networks) and 0x1 (private data tail).
#[test]
fn spec_descriptor_event_group() {
    let body = fields(&[
        ("group_type", 4, 0x4),
        ("event_count", 4, 1),
        ("service_id", 16, 0x0E08),
        ("event_id", 16, 0x0001),
        ("original_network_id", 16, 0x0004),
        ("transport_stream_id", 16, 0x0010),
        ("service_id", 16, 0x0101),
        ("event_id", 16, 0x0002),
    ]);
    let d = EventGroup::decode(&body).unwrap();
    assert_eq!(
        d.events,
        vec![EventRef {
            service_id: 0x0E08,
            event_id: 1
        }]
    );
    assert_eq!(
        d.other_network_events,
        vec![NetworkEventRef {
            original_network_id: 4,
            transport_stream_id: 0x10,
            service_id: 0x0101,
            event_id: 2
        }]
    );
    let d1 = EventGroup::decode(&[0x11, 0x0E, 0x08, 0x00, 0x01, 0xEE]).unwrap();
    assert_eq!((d1.group_type, &d1.private_data[..]), (1, &[0xEEu8][..]));
}

/// component_group_descriptor, Table 6-71, with total_bit_rate.
#[test]
fn spec_descriptor_component_group() {
    let body = fields(&[
        ("component_group_type", 3, 0),
        ("total_bit_rate_flag", 1, 1),
        ("num_of_group", 4, 1),
        ("component_group_id", 4, 0),
        ("num_of_CA_unit", 4, 1),
        ("CA_unit_id", 4, 1),
        ("num_of_component", 4, 2),
        ("component_tag", 8, 0x00),
        ("component_tag", 8, 0x10),
        ("total_bit_rate", 8, 0x40),
        ("text_length", 8, 0),
    ]);
    let d = ComponentGroup::decode(&body).unwrap();
    assert_eq!(
        d.groups[0].ca_units[0],
        CaUnit {
            ca_unit_id: 1,
            component_tags: vec![0x00, 0x10]
        }
    );
    assert_eq!(d.groups[0].total_bit_rate, Some(0x40));
}

/// conditional_access_descriptor, H.222.0 Table 2-58, and
/// access_control_descriptor, B10 Table 6-100.
#[test]
fn spec_descriptor_ca() {
    let d = ConditionalAccess::decode(&[0x00, 0x05, 0xE0 | 0x01, 0x23, 0x99]).unwrap();
    assert_eq!(
        (d.ca_system_id, d.reserved, d.ca_pid, &d.private_data[..]),
        (5, 7, Pid::new(0x123).unwrap(), &[0x99u8][..])
    );
    let a = AccessControl::decode(&[0x00, 0x05, 0xE0 | 0x01, 0x23]).unwrap();
    assert_eq!((a.transmission_type, a.pid), (7, Pid::new(0x123).unwrap()));
}

/// emergency_information_descriptor, Table 6-40.
#[test]
fn spec_descriptor_emergency() {
    let body = fields(&[
        ("service_id", 16, 0x0E08),
        ("start_end_flag", 1, 1),
        ("signal_level", 1, 0),
        ("reserved_future_use", 6, 0x3F),
        ("area_code_length", 8, 2),
        ("area_code", 12, 0x5A5),
        ("reserved_future_use", 4, 0xF),
    ]);
    let d = EmergencyInformation::decode(&body).unwrap();
    assert_eq!(
        d.services[0].area_codes,
        vec![AreaCode {
            area_code: 0x5A5,
            reserved_future_use: 0xF
        }]
    );
}

// ---------------------------------------------------------------------------
// error behaviour
// ---------------------------------------------------------------------------

/// Typed descriptors that cannot account for every byte are errors.
#[test]
fn spec_descriptor_rejects_unaccounted_bytes() {
    assert!(StreamIdentifier::decode(&[0x00, 0x01]).is_err());
    assert!(Service::decode(&[0x01, 0x00, 0x00, 0xFF]).is_err());
    assert!(Service::decode(&[0x01, 0x05, b'a']).is_err());
    assert!(ServiceList::decode(&[0x00, 0x01]).is_err());
    assert!(ExtendedEvent::decode(&[0x00, b'j', b'p', b'n', 3, 2, b'a', b'b', 0]).is_err());
    assert!(VideoDecodeControl::decode(&[]).is_err());
    // A descriptor loop whose last descriptor overruns the loop.
    assert!(decode_loop(&[0x52, 0x02, 0x00]).is_err());
    // Unknown tags are kept raw.
    assert_eq!(
        decode_loop(&[0x99, 0x01, 0x42]).unwrap(),
        vec![Descriptor::Unknown {
            tag: 0x99,
            data: vec![0x42]
        }]
    );
}

/// CRC errors and length errors are errors in the dispatcher.
#[test]
fn spec_dispatch_errors() {
    let body = fields(&[
        ("original_network_id", 16, 1),
        ("reserved_future_use", 8, 0xFF),
    ]);
    let mut sec = long_section(0x42, 1, 0, 0, 0, &body);
    assert!(AnyTable::decode(Pid::new(0x0011).unwrap(), &sec).is_ok());
    let n = sec.len();
    sec[n - 1] ^= 0x01;
    assert!(AnyTable::decode(Pid::new(0x0011).unwrap(), &sec).is_err());
    // An SI table_id on a PID Table 5-1 does not assign: raw, but CRC still checked.
    let good = long_section(0x42, 1, 0, 0, 0, &body);
    let t = AnyTable::decode(Pid::new(0x0100).unwrap(), &good).unwrap();
    assert!(matches!(t, AnyTable::Unknown(_)));
    assert_eq!(
        isdb_si::conformance::check(Pid::new(0x0100).unwrap(), &t)[0].kind,
        isdb_si::conformance::Kind::Placement
    );
    assert!(AnyTable::decode(Pid::new(0x0100).unwrap(), &sec).is_err());
    assert!(AnyTable::decode(Pid::new(0x0011).unwrap(), &good[..good.len() - 1]).is_err());
    assert!(AnyTable::decode(Pid::new(0x0011).unwrap(), &[0xFF, 0xFF, 0xFF]).is_err());
    // Body whose loop length overruns the section.
    let bad = long_section(0x42, 1, 0, 0, 0, &[0, 1, 0xFF, 0, 1, 0xFF, 0xF0, 0x05]);
    assert!(AnyTable::decode(Pid::new(0x0011).unwrap(), &bad).is_err());
}

/// The conformance checker reports reserved bits that are not '1'.
#[test]
fn spec_conformance_reserved_bits() {
    let body = fields(&[
        ("original_network_id", 16, 1),
        ("reserved_future_use", 8, 0x00),
    ]);
    let mut h = LongHeader::new(0x42, 1, 0);
    h.private_indicator = false;
    let sec = isdb_ts::section::build_long(&h, &body).unwrap();
    let t = AnyTable::decode(Pid::new(0x0011).unwrap(), &sec).unwrap();
    let devs = isdb_si::conformance::check(Pid::new(0x0011).unwrap(), &t);
    assert_eq!(devs.len(), 2, "{devs:?}");
    assert!(
        devs.iter()
            .all(|d| d.kind == isdb_si::conformance::Kind::ReservedBits)
    );
    // SdtBody accepts only SDT table_ids.
    assert!(SdtBody::accepts_table_id(0x46) && !SdtBody::accepts_table_id(0x4A));
}
