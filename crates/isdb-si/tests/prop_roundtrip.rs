//! Property tests: lossless round trips for every typed descriptor and every
//! table body (encode → decode → equal → re-encode identical), plus
//! decode-side fuzzing (any bytes a decoder accepts re-encode identically).

use isdb_si::any::AnyTable;
use isdb_si::descriptor::*;
use isdb_si::tables::*;
use isdb_si::time::{BcdDuration, BcdOffset, MjdTime};
use isdb_text::AribString;
use isdb_ts::Pid;
use isdb_ts::section::{
    Long, LongBody, LongHeader, Short, ShortBody, ShortHeader, build_long, build_short,
};
use proptest::prelude::*;
use proptest::strategy::{BoxedStrategy, Union};

fn bytes(max: usize) -> impl Strategy<Value = Vec<u8>> + Clone {
    prop::collection::vec(any::<u8>(), 0..=max)
}
fn s(max: usize) -> impl Strategy<Value = AribString> + Clone {
    bytes(max).prop_map(AribString)
}
fn code3() -> impl Strategy<Value = Code3> + Clone {
    any::<[u8; 3]>()
}
fn bits(n: u32) -> impl Strategy<Value = u8> + Clone {
    0u8..=((1u16 << n) - 1) as u8
}
/// Any 13-bit PID (H.222.0 Table 2-3).
fn pid() -> impl Strategy<Value = Pid> + Clone {
    (0u16..=0x1FFF).prop_map(|v| Pid::new(v).unwrap())
}

fn bits16(n: u32) -> impl Strategy<Value = u16> + Clone {
    0u16..=((1u32 << n) - 1) as u16
}
fn mjd_time() -> impl Strategy<Value = MjdTime> + Clone {
    (any::<u16>(), 0u32..=0xFF_FFFF).prop_map(|(mjd, bcd_hms)| MjdTime { mjd, bcd_hms })
}
fn vec_of<T: std::fmt::Debug>(
    st: impl Strategy<Value = T>,
    max: usize,
) -> impl Strategy<Value = Vec<T>> {
    prop::collection::vec(st, 0..=max)
}

/// One strategy per typed descriptor tag.
fn descriptor_strategies() -> Vec<(u8, BoxedStrategy<Descriptor>)> {
    let mut v: Vec<(u8, BoxedStrategy<Descriptor>)> = Vec::new();
    macro_rules! add {
        ($tag:expr, $st:expr) => {
            v.push(($tag, $st.prop_map(Descriptor::from).boxed()));
        };
    }
    add!(
        0x09,
        (any::<u16>(), bits(3), pid(), bytes(40)).prop_map(|(a, b, c, d)| ConditionalAccess {
            ca_system_id: a,
            reserved: b,
            ca_pid: c,
            private_data: d
        })
    );
    add!(0x40, s(60).prop_map(|name| NetworkName { name }));
    add!(
        0x41,
        vec_of(
            (any::<u16>(), any::<u8>()).prop_map(|(service_id, service_type)| ServiceListEntry {
                service_id,
                service_type
            }),
            20
        )
        .prop_map(|services| ServiceList { services })
    );
    add!(0x42, bytes(60).prop_map(|bytes| Stuffing { bytes }));
    add!(
        0x43,
        (
            any::<u32>(),
            any::<u16>(),
            any::<bool>(),
            bits(2),
            bits(5),
            0u32..(1 << 28),
            bits(4)
        )
            .prop_map(|(f, o, we, p, m, sr, fec)| {
                SatelliteDeliverySystem {
                    frequency: f,
                    orbital_position: o,
                    west_east_flag: we,
                    polarization: p,
                    modulation: m,
                    symbol_rate: sr,
                    fec_inner: fec,
                }
            })
    );
    add!(0x47, s(60).prop_map(|name| BouquetName { name }));
    add!(
        0x48,
        (any::<u8>(), s(40), s(40)).prop_map(|(service_type, provider_name, service_name)| {
            Service {
                service_type,
                provider_name,
                service_name,
            }
        })
    );
    add!(
        0x49,
        (any::<bool>(), bits(7), vec_of(code3(), 10)).prop_map(|(a, b, c)| CountryAvailability {
            country_availability_flag: a,
            reserved_future_use: b,
            country_codes: c
        })
    );
    add!(
        0x4A,
        (
            any::<u16>(),
            any::<u16>(),
            any::<u16>(),
            any::<u8>(),
            bytes(40)
        )
            .prop_map(|(a, b, c, d, e)| Linkage {
                transport_stream_id: a,
                original_network_id: b,
                service_id: c,
                linkage_type: d,
                private_data: e
            })
    );
    add!(
        0x4B,
        vec_of(
            (any::<u16>(), any::<u16>(), any::<u16>()).prop_map(|(a, b, c)| ServiceTriplet {
                transport_stream_id: a,
                original_network_id: b,
                service_id: c
            }),
            10
        )
        .prop_map(|services| NvodReference { services })
    );
    add!(
        0x4C,
        any::<u16>().prop_map(|reference_service_id| TimeShiftedService {
            reference_service_id
        })
    );
    add!(
        0x4D,
        (code3(), s(60), s(60)).prop_map(|(language, event_name, text)| ShortEvent {
            language,
            event_name,
            text
        })
    );
    add!(
        0x4E,
        (
            bits(4),
            bits(4),
            code3(),
            vec_of(
                (s(20), s(30))
                    .prop_map(|(description, item)| ExtendedEventItem { description, item }),
                4
            ),
            s(40)
        )
            .prop_map(|(a, b, language, items, text)| ExtendedEvent {
                descriptor_number: a,
                last_descriptor_number: b,
                language,
                items,
                text
            })
    );
    add!(
        0x4F,
        (any::<u16>(), any::<u16>()).prop_map(|(a, b)| TimeShiftedEvent {
            reference_service_id: a,
            reference_event_id: b
        })
    );
    add!(
        0x50,
        (bits(4), bits(4), any::<u8>(), any::<u8>(), code3(), s(60)).prop_map(
            |(a, b, c, d, language, text)| Component {
                reserved_future_use: a,
                stream_content: b,
                component_type: c,
                component_tag: d,
                language,
                text
            }
        )
    );
    add!(
        0x52,
        any::<u8>().prop_map(|component_tag| StreamIdentifier { component_tag })
    );
    add!(
        0x53,
        vec_of(any::<u16>(), 20).prop_map(|ca_system_ids| CaIdentifier { ca_system_ids })
    );
    add!(
        0x54,
        vec_of(
            (bits(4), bits(4), bits(4), bits(4)).prop_map(|(a, b, c, d)| ContentItem {
                content_nibble_level_1: a,
                content_nibble_level_2: b,
                user_nibble_1: c,
                user_nibble_2: d
            }),
            20
        )
        .prop_map(|items| Content { items })
    );
    add!(
        0x55,
        vec_of(
            (code3(), any::<u8>()).prop_map(|(country_code, rating)| ParentalRatingEntry {
                country_code,
                rating
            }),
            10
        )
        .prop_map(|ratings| ParentalRating { ratings })
    );
    add!(
        0x58,
        vec_of(
            (
                code3(),
                bits(6),
                any::<bool>(),
                any::<bool>(),
                any::<u16>(),
                mjd_time(),
                any::<u16>()
            )
                .prop_map(|(c, r, res, pol, lto, toc, nto)| {
                    LocalTimeOffsetEntry {
                        country_code: c,
                        country_region_id: r,
                        reserved: res,
                        local_time_offset_polarity: pol,
                        local_time_offset: BcdOffset(lto),
                        time_of_change: toc,
                        next_time_offset: BcdOffset(nto),
                    }
                }),
            5
        )
        .prop_map(|entries| LocalTimeOffset { entries })
    );
    add!(
        0xC0,
        (bits(7), any::<bool>(), bits(3), pid()).prop_map(|(a, b, c, d)| {
            HierarchicalTransmission {
                reserved_future_use_1: a,
                quality_level: b,
                reserved_future_use_2: c,
                reference_pid: d,
            }
        })
    );
    let ccc = (
        any::<u8>(),
        bits(2),
        any::<bool>(),
        bits(4),
        prop::option::of(any::<u8>()),
    )
        .prop_map(|(a, b, c, d, e)| ComponentCopyControl {
            component_tag: a,
            digital_recording_control_data: b,
            reserved_future_use: c,
            user_defined: d,
            maximum_bitrate: e,
        });
    add!(
        0xC1,
        (
            bits(2),
            bits(4),
            prop::option::of(any::<u8>()),
            prop::option::of(vec_of(ccc, 8))
        )
            .prop_map(|(a, b, c, d)| {
                DigitalCopyControl {
                    digital_recording_control_data: a,
                    user_defined: b,
                    maximum_bitrate: c,
                    component_control: d,
                }
            })
    );
    add!(
        0xC4,
        (
            (
                bits(4),
                bits(4),
                any::<u8>(),
                any::<u8>(),
                any::<u8>(),
                any::<u8>()
            ),
            (
                any::<bool>(),
                bits(2),
                bits(3),
                any::<bool>(),
                code3(),
                prop::option::of(code3()),
                s(40)
            )
        )
            .prop_map(
                |((r1, sc, ct, tag, st, sg), (main, q, sr, r2, language, language_2, text))| {
                    AudioComponent {
                        reserved_future_use_1: r1,
                        stream_content: sc,
                        component_type: ct,
                        component_tag: tag,
                        stream_type: st,
                        simulcast_group_tag: sg,
                        main_component_flag: main,
                        quality_indicator: q,
                        sampling_rate: sr,
                        reserved_future_use_2: r2,
                        language,
                        language_2,
                        text,
                    }
                }
            )
    );
    add!(
        0xC5,
        (any::<u8>(), any::<u8>(), bytes(20), bytes(20)).prop_map(|(a, b, c, d)| Hyperlink {
            hyper_linkage_type: a,
            link_destination_type: b,
            selector: c,
            private_data: d
        })
    );
    add!(
        0xC7,
        (
            any::<u16>(),
            any::<u8>(),
            bytes(30),
            bytes(10),
            code3(),
            s(40)
        )
            .prop_map(|(a, b, c, d, language, text)| DataContent {
                data_component_id: a,
                entry_component: b,
                selector: c,
                component_refs: d,
                language,
                text
            })
    );
    add!(
        0xC8,
        (any::<bool>(), any::<bool>(), bits(4), bits(2)).prop_map(|(a, b, c, d)| {
            VideoDecodeControl {
                still_picture_flag: a,
                sequence_end_code_flag: b,
                video_encode_format: c,
                transfer_characteristics: d,
            }
        })
    );
    add!(
        0xCD,
        (
            any::<u8>(),
            s(40),
            vec_of(
                (any::<u8>(), vec_of(any::<u16>(), 8)).prop_map(|(a, b)| TransmissionType {
                    transmission_type_info: a,
                    service_ids: b
                }),
                3
            ),
            bytes(10)
        )
            .prop_map(|(a, ts_name, transmission_types, r)| TsInformation {
                remote_control_key_id: a,
                ts_name,
                transmission_types,
                reserved_future_use: r
            })
    );
    let terr = || {
        (
            any::<u16>(),
            vec_of(any::<u8>(), 15),
            vec_of(
                (any::<u16>(), any::<u8>()).prop_map(|(a, b)| NetworkBroadcaster {
                    original_network_id: a,
                    broadcaster_id: b,
                }),
                15,
            ),
            bytes(20),
        )
            .prop_map(|(a, b, c, d)| TerrestrialBroadcaster {
                terrestrial_broadcaster_id: a,
                affiliation_ids: b,
                broadcasters: c,
                private_data: d,
            })
    };
    let eb_body = prop_oneof![
        terr().prop_map(ExtendedBroadcasterBody::TerrestrialTv),
        terr().prop_map(ExtendedBroadcasterBody::TerrestrialSound),
        (
            bits(4).prop_filter("not 1/2", |t| *t != 1 && *t != 2),
            bytes(20)
        )
            .prop_map(|(broadcaster_type, data)| ExtendedBroadcasterBody::Other {
                broadcaster_type,
                data
            }),
    ];
    add!(
        0xCE,
        (bits(4), eb_body).prop_map(|(reserved_future_use, body)| ExtendedBroadcaster {
            reserved_future_use,
            body
        })
    );
    let logo_body = prop_oneof![
        (bits(7), bits16(9), bits(4), bits16(12), any::<u16>()).prop_map(|(a, b, c, d, e)| {
            LogoTransmissionBody::Cdt1 {
                reserved_future_use_1: a,
                logo_id: b,
                reserved_future_use_2: c,
                logo_version: d,
                download_data_id: e,
            }
        }),
        (bits(7), bits16(9)).prop_map(|(a, b)| LogoTransmissionBody::Cdt2 {
            reserved_future_use: a,
            logo_id: b
        }),
        s(40).prop_map(LogoTransmissionBody::Simple),
        (
            any::<u8>().prop_filter("not 1..=3", |t| !(1..=3).contains(t)),
            bytes(20)
        )
            .prop_map(
                |(logo_transmission_type, data)| LogoTransmissionBody::Other {
                    logo_transmission_type,
                    data
                }
            ),
    ];
    add!(0xCF, logo_body.prop_map(|body| LogoTransmission { body }));
    add!(
        0xD5,
        (
            any::<u16>(),
            bits(4),
            bits(3),
            any::<bool>(),
            any::<u16>(),
            bits16(12),
            bits16(12),
            s(60)
        )
            .prop_map(|(a, b, c, d, e, f, g, h)| Series {
                series_id: a,
                repeat_label: b,
                program_pattern: c,
                expire_date_valid_flag: d,
                expire_date: e,
                episode_number: f,
                last_episode_number: g,
                series_name: h
            })
    );
    let evref = (any::<u16>(), any::<u16>()).prop_map(|(service_id, event_id)| EventRef {
        service_id,
        event_id,
    });
    let netev =
        (any::<u16>(), any::<u16>(), any::<u16>(), any::<u16>()).prop_map(|(a, b, c, d)| {
            NetworkEventRef {
                original_network_id: a,
                transport_stream_id: b,
                service_id: c,
                event_id: d,
            }
        });
    add!(
        0xD6,
        (bits(4), vec_of(evref, 15), vec_of(netev, 8), bytes(30)).prop_map(
            |(group_type, events, other, private)| {
                let relay = group_type == 4 || group_type == 5;
                EventGroup {
                    group_type,
                    events,
                    other_network_events: if relay { other } else { vec![] },
                    private_data: if relay { vec![] } else { private },
                }
            }
        )
    );
    let tdesc = (any::<u8>(), bytes(20)).prop_map(|(table_id, description)| TableDescription {
        table_id,
        description,
    });
    add!(
        0xD7,
        (any::<u8>(), any::<u16>(), vec_of(tdesc.clone(), 6)).prop_map(|(a, b, tables)| {
            SiParameter {
                parameter_version: a,
                update_time: b,
                tables,
            }
        })
    );
    add!(0xD8, s(60).prop_map(|name| BroadcasterName { name }));
    add!(
        0xD9,
        (
            bits(3),
            any::<bool>(),
            vec_of(
                (bits(4), vec_of((bits(4), bytes(4)), 3), any::<u8>(), s(20)),
                4
            )
        )
            .prop_map(|(t, flag, groups)| ComponentGroup {
                component_group_type: t,
                total_bit_rate_flag: flag,
                groups: groups
                    .into_iter()
                    .map(|(id, units, rate, text)| ComponentGroupEntry {
                        component_group_id: id,
                        ca_units: units
                            .into_iter()
                            .map(|(ca_unit_id, component_tags)| CaUnit {
                                ca_unit_id,
                                component_tags
                            })
                            .collect(),
                        total_bit_rate: flag.then_some(rate),
                        text,
                    })
                    .collect(),
            })
    );
    add!(
        0xDA,
        (
            any::<u8>(),
            any::<u16>(),
            any::<u16>(),
            any::<u16>(),
            vec_of(tdesc, 6)
        )
            .prop_map(|(a, b, c, d, tables)| SiPrimeTs {
                parameter_version: a,
                update_time: b,
                si_prime_ts_network_id: c,
                si_prime_transport_stream_id: d,
                tables
            })
    );
    add!(
        0xDB,
        (s(60), s(60)).prop_map(|(title, text)| BoardInformation { title, text })
    );
    add!(
        0xDC,
        (
            any::<u16>(),
            any::<u16>(),
            any::<u16>(),
            vec_of(
                (any::<u16>(), bits(4), bits(4), any::<u8>()).prop_map(|(a, b, c, d)| {
                    LdtLinkageEntry {
                        description_id: a,
                        reserved_future_use: b,
                        description_type: c,
                        user_defined: d,
                    }
                }),
                20
            )
        )
            .prop_map(|(a, b, c, descriptions)| LdtLinkage {
                original_service_id: a,
                transport_stream_id: b,
                original_network_id: c,
                descriptions
            })
    );
    add!(
        0xDD,
        (any::<u16>(), bits(2), bits(2), bits(2), bits(2), bytes(30)).prop_map(
            |(a, b, c, d, e, f)| ConnectedTransmission {
                connected_transmission_group_id: a,
                segment_type: b,
                modulation_type_a: c,
                modulation_type_b: d,
                modulation_type_c: e,
                additional_info: f
            }
        )
    );
    add!(
        0xDE,
        (
            any::<bool>(),
            any::<bool>(),
            any::<bool>(),
            any::<bool>(),
            bits(3),
            any::<bool>(),
            bytes(10)
        )
            .prop_map(|(a, b, c, d, e, f, g)| ContentAvailability {
                reserved_future_use: a,
                copy_restriction_mode: b,
                image_constraint_token: c,
                retention_mode: d,
                retention_state: e,
                encryption_mode: f,
                reserved_future_use_bytes: g
            })
    );
    add!(
        0xF6,
        (any::<u16>(), bits(3), pid(), bytes(30)).prop_map(|(a, b, c, d)| AccessControl {
            ca_system_id: a,
            transmission_type: b,
            pid: c,
            private_data: d
        })
    );
    add!(
        0xFA,
        (bits16(12), bits(2), bits(2), vec_of(any::<u16>(), 20)).prop_map(|(a, b, c, d)| {
            TerrestrialDeliverySystem {
                area_code: a,
                guard_interval: b,
                transmission_mode: c,
                frequencies: d,
            }
        })
    );
    add!(
        0xFB,
        vec_of(any::<u16>(), 20).prop_map(|service_ids| PartialReception { service_ids })
    );
    add!(
        0xFC,
        vec_of(
            (
                any::<u16>(),
                any::<bool>(),
                any::<bool>(),
                bits(6),
                vec_of(
                    (bits16(12), bits(4)).prop_map(|(area_code, r)| AreaCode {
                        area_code,
                        reserved_future_use: r
                    }),
                    10
                )
            )
                .prop_map(|(a, b, c, d, e)| EmergencyService {
                    service_id: a,
                    start_end_flag: b,
                    signal_level: c,
                    reserved_future_use: d,
                    area_codes: e
                }),
            4
        )
        .prop_map(|services| EmergencyInformation { services })
    );
    add!(
        0xFD,
        (any::<u16>(), bytes(40)).prop_map(|(a, b)| DataComponent {
            data_component_id: a,
            additional_data_component_info: b
        })
    );
    add!(
        0xFE,
        (any::<u16>(), bytes(40)).prop_map(|(a, b)| SystemManagement {
            system_management_id: a,
            additional_identification_info: b
        })
    );
    v
}

fn any_descriptor() -> BoxedStrategy<Descriptor> {
    let unknown = (
        any::<u8>().prop_filter("untyped tag", |t| !TYPED_TAGS.contains(t)),
        bytes(30),
    )
        .prop_map(|(tag, data)| Descriptor::Unknown { tag, data })
        .boxed();
    let mut all: Vec<BoxedStrategy<Descriptor>> = descriptor_strategies()
        .into_iter()
        .map(|(_, s)| s)
        .collect();
    all.push(unknown);
    Union::new(all).boxed()
}

fn descriptors(max: usize) -> impl Strategy<Value = Vec<Descriptor>> {
    vec_of(any_descriptor(), max)
}

/// The strategy table covers exactly the typed tags.
#[test]
fn prop_strategies_cover_every_typed_descriptor() {
    let mut tags: Vec<u8> = descriptor_strategies()
        .into_iter()
        .map(|(t, _)| t)
        .collect();
    tags.sort();
    let mut typed = TYPED_TAGS.to_vec();
    typed.sort();
    assert_eq!(tags, typed);
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(4000))]

    /// Every typed descriptor: encode → decode → equal, and the tag matches.
    #[test]
    fn prop_descriptor_roundtrip(d in any_descriptor()) {
        let mut b = Vec::new();
        match d.encode(&mut b) {
            Ok(()) => {
                let back = Descriptor::decode(&b).unwrap();
                prop_assert_eq!(&back, &d);
                prop_assert_eq!(back.tag(), b[0]);
                let mut b2 = Vec::new();
                back.encode(&mut b2).unwrap();
                prop_assert_eq!(b2, b);
            }
            // Only a body over 255 bytes may fail to encode.
            Err(e) => prop_assert!(matches!(e, isdb_ts::Error::Overflow { what: "descriptor_length", .. }), "{e}"),
        }
    }

    /// Decoder fuzzing: arbitrary bytes under a typed tag either fail or
    /// re-encode byte-identically.
    #[test]
    fn prop_descriptor_decode_is_lossless(tag in prop::sample::select(TYPED_TAGS.to_vec()), data in bytes(64)) {
        if let Ok(d) = Descriptor::decode_body(tag, &data) {
            let mut out = Vec::new();
            d.encode(&mut out).unwrap();
            prop_assert_eq!(out[0], tag);
            prop_assert_eq!(&out[2..], &data[..]);
        }
    }
}

// ---------------------------------------------------------------------------
// tables
// ---------------------------------------------------------------------------

fn long_header(table_ids: Vec<u8>) -> impl Strategy<Value = LongHeader> {
    (
        prop::sample::select(table_ids),
        any::<bool>(),
        bits(2),
        any::<u16>(),
        bits(2),
        bits(5),
        any::<bool>(),
        any::<u8>(),
        any::<u8>(),
    )
        .prop_map(|(table_id, pi, r1, ext, r2, v, cni, sn, lsn)| LongHeader {
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

fn short_header(table_id: u8) -> impl Strategy<Value = ShortHeader> {
    (any::<bool>(), bits(2)).prop_map(move |(pi, r)| ShortHeader {
        table_id,
        private_indicator: pi,
        reserved1: r,
    })
}

fn network_loops() -> impl Strategy<Value = NetworkLoops> {
    let ts = (any::<u16>(), any::<u16>(), bits(4), descriptors(3)).prop_map(|(a, b, c, d)| {
        TransportStreamEntry {
            transport_stream_id: a,
            original_network_id: b,
            reserved_future_use: c,
            descriptors: d,
        }
    });
    (bits(4), descriptors(3), bits(4), vec_of(ts, 3)).prop_map(|(a, b, c, d)| NetworkLoops {
        reserved_future_use_1: a,
        descriptors: b,
        reserved_future_use_2: c,
        transport_streams: d,
    })
}

fn sdt_body() -> impl Strategy<Value = SdtBody> {
    let svc = (
        any::<u16>(),
        bits(3),
        bits(3),
        any::<bool>(),
        any::<bool>(),
        bits(3),
        any::<bool>(),
        descriptors(3),
    )
        .prop_map(|(a, b, c, d, e, f, g, h)| SdtService {
            service_id: a,
            reserved_future_use: b,
            eit_user_defined_flags: c,
            eit_schedule_flag: d,
            eit_present_following_flag: e,
            running_status: f,
            free_ca_mode: g,
            descriptors: h,
        });
    (any::<u16>(), any::<u8>(), vec_of(svc, 4)).prop_map(|(a, b, c)| SdtBody {
        original_network_id: a,
        reserved_future_use: b,
        services: c,
    })
}

fn eit_body() -> impl Strategy<Value = EitBody> {
    let ev = (
        any::<u16>(),
        mjd_time(),
        0u32..=0xFF_FFFF,
        bits(3),
        any::<bool>(),
        descriptors(3),
    )
        .prop_map(|(a, b, c, d, e, f)| EitEvent {
            event_id: a,
            start_time: b,
            duration: BcdDuration(c),
            running_status: d,
            free_ca_mode: e,
            descriptors: f,
        });
    (
        any::<u16>(),
        any::<u16>(),
        any::<u8>(),
        any::<u8>(),
        vec_of(ev, 4),
    )
        .prop_map(|(a, b, c, d, e)| EitBody {
            transport_stream_id: a,
            original_network_id: b,
            segment_last_section_number: c,
            last_table_id: d,
            events: e,
        })
}

fn pcat_body() -> impl Strategy<Value = PcatBody> {
    let sched = (mjd_time(), 0u32..=0xFF_FFFF).prop_map(|(start_time, d)| PcatSchedule {
        start_time,
        duration: BcdDuration(d),
    });
    let cv = (
        any::<u16>(),
        any::<u16>(),
        bits(2),
        bits(2),
        bits(4),
        vec_of(sched, 4),
        descriptors(3),
    )
        .prop_map(|(a, b, c, d, e, f, g)| PcatContentVersion {
            content_version: a,
            content_minor_version: b,
            version_indicator: c,
            reserved_future_use_1: d,
            reserved_future_use_2: e,
            schedules: f,
            descriptors: g,
        });
    (any::<u16>(), any::<u16>(), any::<u32>(), vec_of(cv, 3)).prop_map(|(a, b, c, d)| PcatBody {
        transport_stream_id: a,
        original_network_id: b,
        content_id: c,
        content_versions: d,
    })
}

fn bit_body() -> impl Strategy<Value = BitBody> {
    let bc = (any::<u8>(), bits(4), descriptors(3)).prop_map(|(a, b, c)| BitBroadcaster {
        broadcaster_id: a,
        reserved_future_use: b,
        descriptors: c,
    });
    (bits(3), any::<bool>(), descriptors(3), vec_of(bc, 4)).prop_map(|(a, b, c, d)| BitBody {
        reserved_future_use: a,
        broadcast_view_propriety: b,
        first_descriptors: c,
        broadcasters: d,
    })
}

fn nbit_body() -> impl Strategy<Value = NbitBody> {
    let info = (
        any::<u16>(),
        bits(4),
        bits(2),
        bits(2),
        any::<u8>(),
        vec_of(any::<u16>(), 5),
        bits(4),
        descriptors(3),
    )
        .prop_map(|(a, b, c, d, e, f, g, h)| NbitInformation {
            information_id: a,
            information_type: b,
            description_body_location: c,
            reserved_future_use_1: d,
            user_defined: e,
            key_ids: f,
            reserved_future_use_2: g,
            descriptors: h,
        });
    vec_of(info, 4).prop_map(|informations| NbitBody { informations })
}

fn ldt_body() -> impl Strategy<Value = LdtBody> {
    let d = (any::<u16>(), bits16(12), descriptors(3)).prop_map(|(a, b, c)| LdtDescription {
        description_id: a,
        reserved_future_use: b,
        descriptors: c,
    });
    (any::<u16>(), any::<u16>(), vec_of(d, 4)).prop_map(|(a, b, c)| LdtBody {
        transport_stream_id: a,
        original_network_id: b,
        descriptions: c,
    })
}

fn check_long<B: LongBody + PartialEq + std::fmt::Debug + Clone>(
    pid: Pid,
    header: LongHeader,
    body: B,
) -> Result<(), TestCaseError> {
    let t = Long { header, body };
    let sec = match t.encode() {
        Ok(s) => s,
        Err(e) => {
            // Only oversize sections/descriptors may fail.
            prop_assert!(matches!(e, isdb_ts::Error::Overflow { .. }), "{e}");
            return Ok(());
        }
    };
    let back = Long::<B>::decode(&sec).unwrap();
    prop_assert_eq!(&back, &t);
    let any = AnyTable::decode(pid, &sec).unwrap();
    prop_assert!(!matches!(any, AnyTable::Unknown(_)));
    prop_assert_eq!(any.encode().unwrap(), sec);
    Ok(())
}

fn check_short<B: ShortBody + PartialEq + std::fmt::Debug + Clone>(
    pid: Pid,
    header: ShortHeader,
    body: B,
) -> Result<(), TestCaseError> {
    let t = Short { header, body };
    let sec = t.encode().unwrap();
    let back = Short::<B>::decode(&sec).unwrap();
    prop_assert_eq!(&back, &t);
    let any = AnyTable::decode(pid, &sec).unwrap();
    prop_assert_eq!(any.encode().unwrap(), sec);
    Ok(())
}

/// Decode-side fuzz for a long body: any body bytes the decoder accepts
/// re-encode identically.
fn fuzz_long<B: LongBody>(header: LongHeader, body: &[u8]) -> Result<(), TestCaseError> {
    let sec = build_long(&header, body).unwrap();
    if let Ok(t) = Long::<B>::decode(&sec) {
        prop_assert_eq!(t.encode().unwrap(), sec);
    }
    Ok(())
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(500))]

    #[test]
    fn prop_nit_roundtrip(h in long_header(vec![0x40, 0x41]), b in network_loops()) { check_long(Pid::new(0x0010).unwrap(), h, NitBody(b))?; }
    #[test]
    fn prop_bat_roundtrip(h in long_header(vec![0x4A]), b in network_loops()) { check_long(Pid::new(0x0011).unwrap(), h, BatBody(b))?; }
    #[test]
    fn prop_sdt_roundtrip(h in long_header(vec![0x42, 0x46]), b in sdt_body()) { check_long(Pid::new(0x0011).unwrap(), h, b)?; }
    #[test]
    fn prop_eit_roundtrip(h in long_header((0x4E..=0x6F).collect()), b in eit_body(), pid in prop::sample::select(vec![0x12u16, 0x26, 0x27]).prop_map(|v| Pid::new(v).unwrap())) {
        check_long(pid, h, b)?;
    }
    #[test]
    fn prop_pcat_roundtrip(h in long_header(vec![0xC2]), b in pcat_body()) { check_long(Pid::new(0x0022).unwrap(), h, b)?; }
    #[test]
    fn prop_bit_roundtrip(h in long_header(vec![0xC4]), b in bit_body()) { check_long(Pid::new(0x0024).unwrap(), h, b)?; }
    #[test]
    fn prop_nbit_roundtrip(h in long_header(vec![0xC5, 0xC6]), b in nbit_body()) { check_long(Pid::new(0x0025).unwrap(), h, b)?; }
    #[test]
    fn prop_ldt_roundtrip(h in long_header(vec![0xC7]), b in ldt_body()) { check_long(Pid::new(0x0025).unwrap(), h, b)?; }

    #[test]
    fn prop_tdt_roundtrip(h in short_header(0x70), t in mjd_time()) { check_short(Pid::new(0x0014).unwrap(), h, TdtBody { jst_time: t })?; }
    #[test]
    fn prop_tot_roundtrip(h in short_header(0x73), t in mjd_time(), r in bits(4), d in descriptors(4)) {
        check_short(Pid::new(0x0014).unwrap(), h, TotBody { jst_time: t, reserved: r, descriptors: d })?;
    }
    #[test]
    fn prop_rst_roundtrip(h in short_header(0x71), e in vec_of((any::<u16>(), any::<u16>(), any::<u16>(), any::<u16>(), bits(5), bits(3)).prop_map(
        |(a, b, c, d, r, s)| RstEntry { transport_stream_id: a, original_network_id: b, service_id: c, event_id: d, reserved_future_use: r, running_status: s }), 20)) {
        check_short(Pid::new(0x0013).unwrap(), h, RstBody { entries: e })?;
    }
    #[test]
    fn prop_st_roundtrip(ssi in any::<bool>(), rfu in any::<bool>(), r in bits(2), data in bytes(300), pid in (0x0002u16..0x1FFF).prop_map(|v| Pid::new(v).unwrap())) {
        prop_assume!(pid.get() != 0x0014);
        let st = StuffingSection { table_id: 0x72, section_syntax_indicator: ssi, reserved_future_use: rfu, reserved: r, data };
        let sec = st.encode().unwrap();
        prop_assert_eq!(&StuffingSection::decode(&sec).unwrap(), &st);
        prop_assert_eq!(AnyTable::decode(pid, &sec).unwrap(), AnyTable::St(st));
    }

    #[test]
    fn prop_table_decode_is_lossless(h in long_header(vec![0x40]), body in bytes(80)) {
        let mut h = h;
        for t in [0x40u8, 0x4A, 0x42, 0x4E, 0xC2, 0xC4, 0xC5, 0xC7] {
            h.table_id = t;
            match t {
                0x40 => fuzz_long::<NitBody>(h, &body)?,
                0x4A => fuzz_long::<BatBody>(h, &body)?,
                0x42 => fuzz_long::<SdtBody>(h, &body)?,
                0x4E => fuzz_long::<EitBody>(h, &body)?,
                0xC2 => fuzz_long::<PcatBody>(h, &body)?,
                0xC4 => fuzz_long::<BitBody>(h, &body)?,
                0xC5 => fuzz_long::<NbitBody>(h, &body)?,
                _ => fuzz_long::<LdtBody>(h, &body)?,
            }
        }
        for (t, crc) in [(0x70u8, false), (0x73, true), (0x71, false)] {
            let sh = ShortHeader { table_id: t, private_indicator: true, reserved1: 3 };
            let sec = build_short(&sh, &body, crc).unwrap();
            if let Ok(any) = AnyTable::decode(if t == 0x71 { Pid::new(0x13).unwrap() } else { Pid::new(0x14).unwrap() }, &sec) {
                prop_assert_eq!(any.encode().unwrap(), sec);
            }
        }
    }
}
