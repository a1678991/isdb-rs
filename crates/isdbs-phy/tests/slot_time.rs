//! `isdbs_core::slots::packet_symbol` against the transmitter: for every
//! layout (one or several TSs; TC8PSK, the QPSK codes, BPSK; runs with
//! dummy slots), packet k of relative TS r goes into the frame and slot
//! that `packet_slot` names (`Transmitter`'s `placed`), and the symbols of
//! each slot start where `slot_byte_symbol` says: flipping the first bit of
//! the byte the interleaver puts at (frame f, slot s, byte b) changes the
//! transmitted codes first at symbol 39 936 f + `slot_byte_symbol(s, b)`.

use isdbs_core::combination::Combination;
use isdbs_core::frame::{FRAME_SYMBOLS, FRAMES, SLOT_BYTES, SLOTS};
use isdbs_core::mainsig::interleave_position;
use isdbs_core::slots::{
    packet_byte_symbol, packet_slot, packet_symbol, slot_byte_symbol, ts_slots,
};
use isdbs_core::tmcc::TmccInfo;
use isdbs_phy::tx::{NULL_PACKET, Packet, SUPERFRAME_SYMBOLS, Transmitter};

/// (name, combinations, (TS_ID, sent slots) per TS).
type Layout = (
    &'static str,
    &'static [(Combination, u8)],
    &'static [(u16, u8)],
);

const LAYOUTS: [Layout; 10] = [
    ("bench", &[(Combination::Tc8psk2_3, 48)], &[(0x4010, 48)]),
    (
        "bs-3ts",
        &[(Combination::Tc8psk2_3, 48)],
        &[(0x4010, 16), (0x4011, 16), (0x4012, 16)],
    ),
    (
        "cs",
        &[(Combination::Qpsk3_4, 48)],
        &[(0x6020, 18), (0x6021, 18)],
    ),
    (
        "tc8+qpsk34",
        &[(Combination::Tc8psk2_3, 44), (Combination::Qpsk3_4, 4)],
        &[(0x4010, 40), (0x4011, 7)],
    ),
    (
        "tc8+qpsk12",
        &[(Combination::Tc8psk2_3, 46), (Combination::Qpsk1_2, 2)],
        &[(0x4010, 47)],
    ),
    (
        "tc8+bpsk",
        &[(Combination::Tc8psk2_3, 44), (Combination::Bpsk1_2, 4)],
        &[(0x4010, 30), (0x4011, 15)],
    ),
    ("qpsk23", &[(Combination::Qpsk2_3, 48)], &[(0x4010, 32)]),
    ("qpsk56", &[(Combination::Qpsk5_6, 48)], &[(0x4010, 40)]),
    ("qpsk78", &[(Combination::Qpsk7_8, 48)], &[(0x4010, 42)]),
    (
        "four-runs",
        &[
            (Combination::Tc8psk2_3, 20),
            (Combination::Qpsk7_8, 8),
            (Combination::Qpsk3_4, 12),
            (Combination::Bpsk1_2, 8),
        ],
        &[(0x4010, 10), (0x4011, 14), (0x4012, 14)],
    ),
];

fn info(l: &Layout) -> TmccInfo {
    TmccInfo::assign(l.1, l.2).unwrap()
}

/// The superframe's codes with every packet a null packet but `edit`.
fn codes(info: &TmccInfo, edit: Option<(usize, usize, usize)>) -> Vec<u8> {
    let mut tx = Transmitter::new(info.clone()).unwrap();
    let mut ts: Vec<Vec<Packet>> = (0..tx.ts_count())
        .map(|r| vec![NULL_PACKET; tx.packets_per_superframe(r)])
        .collect();
    if let Some((r, k, byte)) = edit {
        ts[r][k][byte] ^= 0x80;
    }
    let refs: Vec<&[Packet]> = ts.iter().map(Vec::as_slice).collect();
    let sf = tx.superframe_codes(&refs).unwrap();
    assert_eq!(sf.codes.len(), SUPERFRAME_SYMBOLS);
    sf.codes
}

#[test]
fn packets_go_to_the_slots_packet_slot_names() {
    for l in &LAYOUTS {
        let info = info(l);
        let mut tx = Transmitter::new(info.clone()).unwrap();
        let placed = tx.null_superframe_codes().placed;
        for (r, &(id, n)) in l.2.iter().enumerate() {
            assert_eq!(ts_slots(&info, r), usize::from(n), "{}", l.0);
            let mine: Vec<(usize, usize)> = placed
                .iter()
                .filter(|p| p.2 == id)
                .map(|p| (p.0, p.1))
                .collect();
            assert_eq!(mine.len(), FRAMES * usize::from(n), "{}", l.0);
            for (k, &fs) in mine.iter().enumerate() {
                assert_eq!(
                    packet_slot(&info, r, k),
                    Some(fs),
                    "{} TS {r} packet {k}",
                    l.0
                );
            }
            assert_eq!(packet_slot(&info, r, mine.len()), None);
            // packets of a TS are in time order
            let t: Vec<usize> = (0..mine.len())
                .map(|k| packet_symbol(&info, r, k).unwrap())
                .collect();
            assert!(t.windows(2).all(|w| w[0] < w[1]), "{} TS {r}", l.0);
        }
        assert_eq!(packet_slot(&info, l.2.len(), 0), None);
    }
}

/// The packet (relative TS, index) the transmitter puts into `slot` of
/// `frame` before interleaving.
fn packet_at(info: &TmccInfo, frame: usize, slot: usize) -> (usize, usize) {
    for r in 0..8 {
        for k in 0..FRAMES * ts_slots(info, r) {
            if packet_slot(info, r, k) == Some((frame, slot)) {
                return (r, k);
            }
        }
    }
    panic!("no packet in frame {frame} slot {slot}");
}

/// The transmitted symbol of on-air byte `b` of `slot` in frame `f`, from
/// the codes: the first one that changes when that byte's first bit flips.
/// The interleaver puts there byte j of the slot's packet in frame i, with
/// 8 j + i = 203 f + b (BO.1408-1 Fig. 6).
fn measured(info: &TmccInfo, base: &[u8], f: usize, slot: usize, b: usize) -> usize {
    let n = SLOT_BYTES * f + b;
    let (i, j) = (n % FRAMES, n / FRAMES);
    assert_eq!(interleave_position(i, j), (f, b));
    let (r, k) = packet_at(info, i, slot);
    // on-air byte j is packet byte j + 1 (the sync byte is not sent);
    // packet bytes only (the RS parity follows byte 187)
    assert!(j + 1 < 188);
    let got = codes(info, Some((r, k, j + 1)));
    base.iter()
        .zip(&got)
        .position(|(a, b)| a != b)
        .expect("the flipped bit changes the codes")
}

#[test]
fn slot_symbols_are_where_the_transmitter_puts_them() {
    for l in &LAYOUTS {
        let info = info(l);
        let base = codes(&info, None);
        let sent: Vec<usize> = (0..SLOTS)
            .filter(|&s| slot_byte_symbol(&info, s, 0).is_some())
            .collect();
        // every sent slot in one frame; a spread of slots in the others,
        // and the PCR byte (on-air byte 9) of a few
        let mut checks: Vec<(usize, usize, usize)> = sent.iter().map(|&s| (3, s, 0)).collect();
        for (q, &s) in sent.iter().enumerate().step_by(5) {
            checks.push((q % FRAMES, s, 0));
            checks.push(((q + 4) % FRAMES, s, 9));
        }
        checks.push((7, *sent.last().unwrap(), 9));
        checks.push((0, sent[0], 202 - 187)); // byte 15, a later bit
        for (f, s, b) in checks {
            let want = f * FRAME_SYMBOLS + slot_byte_symbol(&info, s, b).unwrap();
            assert_eq!(
                measured(&info, &base, f, s, b),
                want,
                "{}: frame {f} slot {s} byte {b}",
                l.0
            );
            // the same position through the packet that sits there
            let (r, k) = packet_at(&info, f, s);
            assert_eq!(packet_byte_symbol(&info, r, k, b), Some(want));
        }
    }
}
