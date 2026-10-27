//! Property tests of the TLV assembler.

// Narrowing casts are proven or fallible here (docs/plans/type-safety.md section 2).
#![warn(clippy::cast_possible_truncation)]

use isdb_tlv::tlv::{Assembler, Packet, PacketType};
use proptest::prelude::*;

fn packet() -> impl Strategy<Value = Packet> {
    (
        prop::sample::select(vec![
            PacketType::Ipv4,
            PacketType::Ipv6,
            PacketType::Compressed,
            PacketType::Control,
            PacketType::Null,
        ]),
        prop::collection::vec(any::<u8>(), 0..1500),
    )
        .prop_map(|(kind, data)| Packet { kind, data })
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    /// Packets cut into slots of any size, each with its head-TLV
    /// indication, come back unchanged and in order, with no mismatch.
    #[test]
    fn prop_slots_round_trip(ps in prop::collection::vec(packet(), 1..40), size in 64usize..6000) {
        let mut stream = Vec::new();
        let mut starts = Vec::new();
        for p in &ps {
            starts.push(stream.len());
            stream.extend(p.encode().unwrap());
        }
        let mut a = Assembler::new();
        let mut out = Vec::new();
        for (i, c) in stream.chunks(size).enumerate() {
            let lo = i * size;
            let head = starts
                .iter()
                .find(|&&s| s >= lo && s < lo + c.len())
                .map_or(0xFFFF, |&s| u16::try_from(s - lo).expect("offset within a slot"));
            a.push_slot(Some(c), head, &mut out);
        }
        prop_assert_eq!(&out, &ps);
        prop_assert_eq!(a.stats.pointer_mismatches, 0);
    }
}
