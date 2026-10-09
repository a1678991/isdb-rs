//! Lossless round-trip properties (workspace README "How it is verified",
//! item 2): `decode(encode(x)) == x` for generated values and
//! `encode(decode(b)) == b` for every byte string a decoder accepts.

use isdb_ts::cc::Counter;
use isdb_ts::demux::{Assembled, SectionAssembler};
use isdb_ts::descriptor::Descriptor;
use isdb_ts::packet::{
    AdaptationField, AdaptationFieldExtension, AdaptationFields, Header, Ltw, Packet,
    PiecewiseRate, afc,
};
use isdb_ts::packetize::{Packing, packetize_sections};
use isdb_ts::pcr::{Pcr, Timestamp};
use isdb_ts::pes::{
    AdditionalCopyInfo, EsRate, Escr, PStdBuffer, PesAssembler, PesBody, PesExtension,
    PesExtension2, PesHeader, PesOutput, PesPacket, ProgramPacketSequenceCounter, PtsDts,
};
use isdb_ts::psi::{Cat, Pat, PatEntry, Pmt, PmtStream};
use isdb_ts::section::{Long, LongBody, LongHeader, build_long};
use proptest::collection::vec;
use proptest::option::of;
use proptest::prelude::*;

fn pcr() -> impl Strategy<Value = Pcr> {
    (0u64..1 << 33, 0u8..64, 0u16..512).prop_map(|(base, reserved, extension)| Pcr {
        base,
        reserved,
        extension,
    })
}

fn timestamp() -> impl Strategy<Value = Timestamp> {
    (0u8..16, 0u64..1 << 33, 0u8..8).prop_map(|(prefix, value, markers)| Timestamp {
        prefix,
        value,
        markers,
    })
}

fn af_extension() -> impl Strategy<Value = AdaptationFieldExtension> {
    (
        0u8..32,
        of((any::<bool>(), 0u16..1 << 15).prop_map(|(valid, offset)| Ltw { valid, offset })),
        of((0u8..4, 0u32..1 << 22).prop_map(|(reserved, rate)| PiecewiseRate { reserved, rate })),
        of(timestamp()),
        vec(any::<u8>(), 0..8),
    )
        .prop_map(
            |(reserved, ltw, piecewise_rate, seamless_splice, reserved_bytes)| {
                AdaptationFieldExtension {
                    reserved,
                    ltw,
                    piecewise_rate,
                    seamless_splice,
                    reserved_bytes,
                }
            },
        )
}

fn af_fields() -> impl Strategy<Value = AdaptationFields> {
    (
        (any::<bool>(), any::<bool>(), any::<bool>()),
        of(pcr()),
        of(pcr()),
        of(any::<i8>()),
        of(vec(any::<u8>(), 0..40)),
        of(af_extension()),
        vec(any::<u8>(), 0..60),
    )
        .prop_map(
            |(
                (d, r, e),
                pcr,
                opcr,
                splice_countdown,
                transport_private_data,
                extension,
                stuffing,
            )| AdaptationFields {
                discontinuity_indicator: d,
                random_access_indicator: r,
                elementary_stream_priority_indicator: e,
                pcr,
                opcr,
                splice_countdown,
                transport_private_data,
                extension,
                stuffing,
            },
        )
}

fn adaptation_field() -> impl Strategy<Value = AdaptationField> {
    prop_oneof![1 => Just(AdaptationField::Empty), 9 => af_fields().prop_map(AdaptationField::Fields)]
}

/// Packets whose parts add up to 188 bytes, any header values.
fn packet() -> impl Strategy<Value = Packet> {
    (
        (
            any::<bool>(),
            any::<bool>(),
            any::<bool>(),
            0u16..0x2000,
            0u8..4,
            0u8..16,
        ),
        of(adaptation_field()),
        any::<bool>(),
        any::<u64>(),
    )
        .prop_map(
            |((tei, pusi, prio, pid, tsc, cc), af, reserved_afc, seed)| {
                let af_len = af.as_ref().map_or(0, AdaptationField::encoded_len);
                let afc = match (&af, reserved_afc) {
                    (None, true) => afc::RESERVED,
                    (None, false) => afc::PAYLOAD_ONLY,
                    (Some(_), true) => afc::ADAPTATION_ONLY,
                    (Some(_), false) => afc::ADAPTATION_AND_PAYLOAD,
                };
                let payload: Vec<u8> = (0..184usize.saturating_sub(af_len))
                    .map(|i| (seed >> (i % 57)) as u8 ^ i as u8)
                    .collect();
                Packet {
                    header: Header {
                        sync_byte: 0x47,
                        transport_error_indicator: tei,
                        payload_unit_start_indicator: pusi,
                        transport_priority: prio,
                        pid,
                        transport_scrambling_control: tsc,
                        adaptation_field_control: afc,
                        continuity_counter: cc,
                    },
                    adaptation_field: af,
                    payload,
                }
            },
        )
        .prop_filter("adaptation field fits in 184 bytes", |p| {
            p.adaptation_field
                .as_ref()
                .is_none_or(|a| a.encoded_len() <= 184)
        })
}

fn pes_extension() -> impl Strategy<Value = PesExtension> {
    (
        0u8..8,
        of(any::<[u8; 16]>()),
        of(vec(any::<u8>(), 0..20)),
        of((0u8..4, 0u8..128, any::<bool>(), 0u8..64).prop_map(
            |(markers, counter, m, original_stuff_length)| ProgramPacketSequenceCounter {
                markers,
                counter,
                mpeg1_mpeg2_identifier: m,
                original_stuff_length,
            },
        )),
        of(
            (0u8..4, any::<bool>(), 0u16..1 << 13).prop_map(|(prefix, scale, size)| PStdBuffer {
                prefix,
                scale,
                size,
            }),
        ),
        of((any::<bool>(), vec(any::<u8>(), 0..12))
            .prop_map(|(marker, data)| PesExtension2 { marker, data })),
    )
        .prop_map(
            |(
                reserved,
                private_data,
                pack_header,
                program_packet_sequence_counter,
                p_std_buffer,
                extension2,
            )| PesExtension {
                reserved,
                private_data,
                pack_header,
                program_packet_sequence_counter,
                p_std_buffer,
                extension2,
            },
        )
}

fn pes_header() -> impl Strategy<Value = PesHeader> {
    let pts_dts = prop_oneof![
        Just(PtsDts::None),
        Just(PtsDts::Forbidden),
        timestamp().prop_map(PtsDts::Pts),
        (timestamp(), timestamp()).prop_map(|(p, d)| PtsDts::PtsDts(p, d)),
    ];
    (
        (
            0u8..4,
            0u8..4,
            any::<bool>(),
            any::<bool>(),
            any::<bool>(),
            any::<bool>(),
        ),
        pts_dts,
        of((0u8..4, 0u64..1 << 33, 0u16..512, 0u8..16).prop_map(
            |(reserved, base, extension, markers)| Escr {
                reserved,
                base,
                extension,
                markers,
            },
        )),
        of((0u8..4, 0u32..1 << 22).prop_map(|(markers, rate)| EsRate { markers, rate })),
        of(any::<u8>()),
        of((any::<bool>(), 0u8..128)
            .prop_map(|(marker, info)| AdditionalCopyInfo { marker, info })),
        of(any::<u16>()),
        of(pes_extension()),
        vec(any::<u8>(), 0..40),
    )
        .prop_map(
            |(
                (mb, sc, pr, da, cp, oc),
                pts_dts,
                escr,
                es_rate,
                trick_mode,
                aci,
                crc,
                extension,
                stuffing,
            )| PesHeader {
                marker_bits: mb,
                scrambling_control: sc,
                priority: pr,
                data_alignment_indicator: da,
                copyright: cp,
                original_or_copy: oc,
                pts_dts,
                escr,
                es_rate,
                trick_mode,
                additional_copy_info: aci,
                previous_pes_packet_crc: crc,
                extension,
                stuffing,
            },
        )
}

fn pes_packet() -> impl Strategy<Value = PesPacket> {
    let with_header = (
        prop_oneof![Just(0xBDu8), 0xC0u8..=0xEF, Just(0xF3u8), 0xF9u8..=0xFE],
        pes_header(),
        vec(any::<u8>(), 0..600),
        any::<bool>(),
    )
        .prop_map(|(stream_id, header, data, unbounded)| PesPacket {
            stream_id,
            unbounded,
            body: PesBody::Header { header, data },
        });
    let raw = (
        prop_oneof![
            Just(0xBCu8),
            Just(0xBF),
            Just(0xF0),
            Just(0xF1),
            Just(0xF2),
            Just(0xF8),
            Just(0xFF)
        ],
        vec(any::<u8>(), 1..400),
    )
        .prop_map(|(stream_id, d)| PesPacket {
            stream_id,
            unbounded: false,
            body: PesBody::Data(d),
        });
    let pad = vec(any::<u8>(), 1..300).prop_map(|d| PesPacket {
        stream_id: 0xBE,
        unbounded: false,
        body: PesBody::Padding(d),
    });
    prop_oneof![6 => with_header, 1 => raw, 1 => pad]
}

fn descriptors() -> impl Strategy<Value = Vec<Descriptor>> {
    vec(
        (any::<u8>(), vec(any::<u8>(), 0..30)).prop_map(|(t, d)| Descriptor::new(t, d)),
        0..5,
    )
}

fn long_header(table_id: u8) -> impl Strategy<Value = LongHeader> {
    (
        any::<bool>(),
        0u8..4,
        any::<u16>(),
        0u8..4,
        0u8..32,
        any::<bool>(),
        any::<u8>(),
        any::<u8>(),
    )
        .prop_map(move |(pi, r1, ext, r2, v, cni, sn, lsn)| LongHeader {
            table_id,
            private_indicator: pi,
            reserved1: r1,
            table_id_extension: ext,
            reserved2: r2,
            version_number: v,
            current_next_indicator: cni,
            section_number: sn,
            last_section_number: lsn,
        })
}

fn pat() -> impl Strategy<Value = Long<Pat>> {
    (
        long_header(0),
        vec(
            (any::<u16>(), 0u8..8, 0u16..0x2000).prop_map(|(program_number, reserved, pid)| {
                PatEntry {
                    program_number,
                    reserved,
                    pid,
                }
            }),
            0..50,
        ),
    )
        .prop_map(|(header, entries)| Long {
            header,
            body: Pat { entries },
        })
}

fn cat() -> impl Strategy<Value = Long<Cat>> {
    (long_header(1), descriptors()).prop_map(|(header, descriptors)| Long {
        header,
        body: Cat { descriptors },
    })
}

fn pmt() -> impl Strategy<Value = Long<Pmt>> {
    let stream = (any::<u8>(), 0u8..8, 0u16..0x2000, 0u8..16, descriptors()).prop_map(
        |(stream_type, reserved_pid, elementary_pid, reserved_info, descriptors)| PmtStream {
            stream_type,
            reserved_pid,
            elementary_pid,
            reserved_info,
            descriptors,
        },
    );
    (
        long_header(2),
        0u8..8,
        0u16..0x2000,
        0u8..16,
        descriptors(),
        vec(stream, 0..8),
    )
        .prop_map(
            |(header, reserved_pcr, pcr_pid, reserved_info, program_info, streams)| Long {
                header,
                body: Pmt {
                    reserved_pcr,
                    pcr_pid,
                    reserved_info,
                    program_info,
                    streams,
                },
            },
        )
}

fn sections() -> impl Strategy<Value = Vec<Vec<u8>>> {
    vec(
        (0x40u8..0xFF, vec(any::<u8>(), 0..1100))
            .prop_map(|(tid, body)| build_long(&LongHeader::new(tid, 1, 0), &body).unwrap()),
        1..8,
    )
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 512, failure_persistence: None, ..ProptestConfig::default() })]

    #[test]
    fn prop_packet_roundtrip(p in packet()) {
        let b = p.encode().unwrap();
        let d = Packet::decode(&b).unwrap();
        prop_assert_eq!(&d, &p);
        prop_assert_eq!(d.encode().unwrap(), b);
    }

    /// Any 188-byte input the decoder accepts re-encodes byte-identically.
    #[test]
    fn prop_packet_bytes_lossless(head in any::<[u8; 32]>(), tail in vec(any::<u8>(), 156), af_len in 0u8..=184) {
        let mut raw = [0u8; 188];
        raw[..32].copy_from_slice(&head);
        raw[32..].copy_from_slice(&tail);
        raw[4] = af_len; // make plausible adaptation fields common
        if let Ok(p) = Packet::decode(&raw) {
            prop_assert_eq!(p.encode().unwrap(), raw);
        }
    }

    #[test]
    fn prop_pes_roundtrip(p in pes_packet()) {
        let b = p.encode().unwrap();
        let d = PesPacket::decode(&b).unwrap();
        prop_assert_eq!(&d, &p);
        prop_assert_eq!(d.encode().unwrap(), b);
    }

    #[test]
    fn prop_pes_bytes_lossless(id in 0xBCu8..=0xFF, body in vec(any::<u8>(), 0..300)) {
        let mut b = vec![0, 0, 1, id, 0, 0];
        let len = body.len() as u16;
        b[4..6].copy_from_slice(&len.to_be_bytes());
        b.extend_from_slice(&body);
        if let Ok(p) = PesPacket::decode(&b) {
            prop_assert_eq!(p.encode().unwrap(), b);
        }
    }

    #[test]
    fn prop_pat_roundtrip(s in pat()) {
        let b = s.encode().unwrap();
        let d = Long::<Pat>::decode(&b).unwrap();
        prop_assert_eq!(&d, &s);
        prop_assert_eq!(d.encode().unwrap(), b);
    }

    #[test]
    fn prop_cat_roundtrip(s in cat()) {
        let b = s.encode().unwrap();
        prop_assert_eq!(Long::<Cat>::decode(&b).unwrap(), s);
    }

    #[test]
    fn prop_pmt_roundtrip(s in pmt()) {
        let b = s.encode().unwrap();
        let d = Long::<Pmt>::decode(&b).unwrap();
        prop_assert_eq!(&d, &s);
        prop_assert_eq!(d.encode().unwrap(), b);
    }

    /// Any body bytes the PSI decoders accept re-encode identically.
    #[test]
    fn prop_psi_body_bytes_lossless(body in vec(any::<u8>(), 0..200)) {
        let h = LongHeader::new(2, 1, 0);
        if let Ok(p) = Pmt::decode(&h, &body) {
            let mut v = Vec::new();
            p.encode(&mut v).unwrap();
            prop_assert_eq!(v, body.clone());
        }
        if let Ok(p) = Pat::decode(&h, &body) {
            let mut v = Vec::new();
            p.encode(&mut v).unwrap();
            prop_assert_eq!(v, body.clone());
        }
        if let Ok(p) = Cat::decode(&h, &body) {
            let mut v = Vec::new();
            p.encode(&mut v).unwrap();
            prop_assert_eq!(v, body);
        }
    }

    /// Sections → packets → SectionAssembler is the identity.
    #[test]
    fn prop_section_packetize_assemble(secs in sections(), packed in any::<bool>(), cc0 in 0u8..16) {
        let packing = if packed { Packing::Packed } else { Packing::OnePerPacket };
        let mut cc = Counter::new(cc0);
        let pkts = packetize_sections(0x30, &secs, packing, &mut cc).unwrap();
        let mut a = SectionAssembler::new(0x30);
        let mut got = Vec::new();
        for (i, p) in pkts.iter().enumerate() {
            prop_assert!(isdb_ts::packet::check(&Packet::decode(p).unwrap()).is_empty());
            for o in a.push(i as u64, p) {
                match o {
                    Assembled::Section(s) => got.push(s.data),
                    Assembled::Event(e) => prop_assert!(false, "event {:?}", e),
                }
            }
        }
        prop_assert_eq!(got, secs);
    }

    /// PES → packets → PesAssembler is the identity.
    #[test]
    fn prop_pes_packetize_assemble(data in vec(any::<u8>(), 0..2000), pts in 0u64..1 << 33, with_pcr in any::<bool>()) {
        let h = PesHeader { pts_dts: PtsDts::pts_only(pts), ..Default::default() };
        let pes = PesPacket::new(0xE0, h, data).encode().unwrap();
        let af = with_pcr.then(|| AdaptationFields { pcr: Some(Pcr::from_ticks(pts * 300)), ..Default::default() });
        let mut cc = Counter::new(0);
        let pkts = isdb_ts::pes::packetize(0x40, &pes, af, &mut cc).unwrap();
        let mut a = PesAssembler::new(0x40);
        let mut got = Vec::new();
        for (i, p) in pkts.iter().enumerate() {
            prop_assert!(isdb_ts::packet::check(&Packet::decode(p).unwrap()).is_empty());
            got.extend(a.push(i as u64, p));
        }
        prop_assert_eq!(got.len(), 1);
        match &got[0] {
            PesOutput::Pes(x) => prop_assert_eq!(&x.data, &pes),
            e => prop_assert!(false, "{:?}", e),
        }
    }
}
