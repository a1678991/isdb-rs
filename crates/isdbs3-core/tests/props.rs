//! Property tests of the ISDB-S3 codecs.

use isdbs3_core::params::{CodeRate, Modulation, SLOTS};
use isdbs3_core::tmcc::{
    Mode, ModulationCode, PacketFormat, RateCode, StreamType, TmccInfo, disperse,
};
use proptest::prelude::*;

fn info() -> impl Strategy<Value = TmccInfo> {
    (
        any::<u8>(),
        prop::sample::select(vec![Modulation::Qpsk, Modulation::Psk8, Modulation::Apsk16]),
        prop::sample::select(isdbs3_core::params::RATES.to_vec()),
        any::<u8>(),
        prop::collection::vec(any::<u16>(), 16),
        prop::collection::vec((any::<u16>(), any::<u16>()), SLOTS),
        prop::collection::vec(0u8..16, SLOTS),
        any::<bool>(),
        0u8..8,
    )
        .prop_map(|(change, m, r, obo, ids, ptr, rel, ews, up)| {
            let mut modes = [Mode::UNUSED; 8];
            modes[0] = Mode {
                modulation_code: ModulationCode::Assigned(m),
                rate_code: RateCode::Assigned(CodeRate::new(r).unwrap()),
                slots: 120,
                obo,
            };
            TmccInfo {
                change,
                modes,
                stream_type: [StreamType::Tlv; 16],
                packet_format: [PacketFormat::default(); 16],
                pointers: ptr.try_into().unwrap(),
                slot_stream: rel.try_into().unwrap(),
                stream_id: ids.try_into().unwrap(),
                ews,
                uplink: up,
                reserved: 0xF,
                extension_id: 0,
                extension_all_ones: true,
            }
        })
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(16))]

    /// The 9 422 information bits round-trip.
    #[test]
    fn prop_tmcc_info_lossless(t in info()) {
        prop_assert_eq!(TmccInfo::parse(&t.bits()), t);
    }

    /// The TMCC signal (BCH, dispersal, shortened LDPC 61/120) is a
    /// codeword whose data part undoes to the information.
    #[test]
    fn prop_tmcc_signal_round_trip(t in info()) {
        use isdbs3_core::tmcc::{DATA_BITS, NULL_HEAD, NULL_TAIL, encode};
        let s = encode(&t);
        let code = isdbs3_core::ldpc::code(CodeRate::new(61).unwrap());
        let mut w = vec![0u8; NULL_HEAD];
        w.extend_from_slice(&s[..DATA_BITS]);
        w.resize(NULL_HEAD + DATA_BITS + NULL_TAIL, 0);
        w.extend_from_slice(&s[DATA_BITS..]);
        prop_assert_eq!(code.syndrome_weight(&w), 0);
        let mut d = s[..DATA_BITS].to_vec();
        disperse(&mut d);
        prop_assert_eq!(isdb_fec::bch::decode(&mut d), Ok(0));
        prop_assert_eq!(TmccInfo::parse(&d[..isdbs3_core::params::TMCC_INFO_BITS]), t);
    }

    /// Every rate's LDPC code encodes to a codeword.
    #[test]
    fn prop_ldpc_codewords(r in prop::sample::select(isdbs3_core::params::RATES.to_vec()), seed in any::<u64>()) {
        let code = isdbs3_core::ldpc::code(CodeRate::new(r).unwrap());
        let mut x = seed | 1;
        let info: Vec<u8> = (0..code.k()).map(|_| { x ^= x << 13; x ^= x >> 7; x ^= x << 17; (x & 1) as u8 }).collect();
        prop_assert_eq!(code.syndrome_weight(&code.encode(&info)), 0);
    }
}
