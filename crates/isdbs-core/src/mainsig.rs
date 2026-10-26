//! The main signal in bytes: which slots are sent, the inter-frame
//! interleaver, the energy dispersal and the TS packets of a superframe.
//!
//! Transmit order (BO.1408-1 Fig. 1; MIC Ordinance No. 87 of 2011, annexed
//! table 48 and its 別記 1–3):
//!
//! ```text
//! TS packets (188 B) ─RS(204,188)→ slots (204 B), 48 per frame, 8 frames
//!   ─energy dispersal (PRBS15 over the superframe, sync bytes skipped)
//!   ─inter-frame interleaver (depth 8, per slot number, sync bytes kept)
//!   ─sync bytes dropped, dummy slots dropped → 203 B per sent slot
//! ```
//!
//! The receiver runs it backwards from the bytes the inner decoder
//! returns for a frame: the sent slots' 203 bytes each, in slot order.

use crate::combination::Combination;
use crate::frame::{FRAMES, PACKET_BYTES, SLOT_BYTES, SLOTS};
use crate::tmcc::TmccInfo;
use isdb_fec::Prbs15;
use std::sync::OnceLock;

/// The MPEG-2 TS sync byte, restored in front of every slot (BO.1408-1
/// §5: "The omitted MPEG sync words have to be recovered at the receiver").
pub const SYNC_BYTE: u8 = 0x47;
/// Bytes of a superframe before the sync bytes are dropped: 8 × 48 × 204.
pub const SUPERFRAME_BYTES: usize = FRAMES * SLOTS * PACKET_BYTES;

/// The slots of `info` that are transmitted, in transmission order (slot
/// numbers from 0). Dummy slots are not sent (BO.1408-1 §8); within a
/// combination's run, the valid slots of each unit of annexed table 48
/// 別記 3 come first (SPEC-NOTES "dummy-slots").
pub fn sent_slots(info: &TmccInfo) -> Vec<usize> {
    info.slot_combinations()
        .iter()
        .enumerate()
        .filter_map(|(s, c)| c.map(|_| s))
        .collect()
}

/// The combination each sent slot is coded with, in the order of
/// [`sent_slots`].
pub fn sent_slot_combinations(info: &TmccInfo) -> Vec<Combination> {
    info.slot_combinations().iter().flatten().copied().collect()
}

/// Position of byte `j` (0..203, after the sync byte) of slot `slot` in
/// frame `frame` within the superframe's 78 336 bytes (sync bytes and
/// dummy slots included).
pub const fn superframe_position(frame: usize, slot: usize, j: usize) -> usize {
    (frame * SLOTS + slot) * PACKET_BYTES + 1 + j
}

/// The energy-dispersal byte of every superframe position (annexed table
/// 48 別記 1, BO.1408-1 §4 and Fig. 5): X¹⁵ + X¹⁴ + 1 loaded with
/// `100101010000000` at the second byte of the superframe, its first bit on
/// the MSB of that byte, clocked through every later byte; the sync-byte
/// positions (the first byte of every slot) get 0, the sequence still
/// advancing over them ("ＴＳ同期バイトには加算は行わないが、擬似乱数符号系列
/// の発生は継続する"). Dummy slots are part of the frame and advance it too.
pub fn dispersal() -> &'static [u8; SUPERFRAME_BYTES] {
    static D: OnceLock<Box<[u8; SUPERFRAME_BYTES]>> = OnceLock::new();
    D.get_or_init(|| {
        let mut d = Box::new([0u8; SUPERFRAME_BYTES]);
        let mut p = Prbs15::new();
        for (i, b) in d.iter_mut().enumerate().skip(1) {
            let v = p.next_byte();
            if i % PACKET_BYTES != 0 {
                *b = v;
            }
        }
        d
    })
}

/// Where the interleaver puts byte `j` (0..203, after the sync byte) of a
/// slot in frame `i`: (frame, byte) of the interleaved superframe. The
/// 8 × 203 matrix of one slot number (row = frame) is read out column by
/// column, "in the order of A1,1; A2,1; A3,1; …" (BO.1408-1 §5, Fig. 6;
/// annexed table 48 別記 2: "バイト単位で８×２０３バイトのブロックインター
/// リーブ … 各フレームのスロット番号が同一のスロット間"), and written back row
/// by row: Fig. 6's interleaved frame 1 starts A1,1 A2,1 A3,1 and ends at
/// A3,26, frame 2 starts at A4,26.
pub const fn interleave_position(i: usize, j: usize) -> (usize, usize) {
    let n = j * FRAMES + i;
    (n / SLOT_BYTES, n % SLOT_BYTES)
}

/// Interleave the 8 × 203 bytes of one slot number across a superframe.
pub fn interleave(rows: &[[u8; SLOT_BYTES]; FRAMES]) -> [[u8; SLOT_BYTES]; FRAMES] {
    let mut out = [[0u8; SLOT_BYTES]; FRAMES];
    for (i, row) in rows.iter().enumerate() {
        for (j, &b) in row.iter().enumerate() {
            let (f, k) = interleave_position(i, j);
            out[f][k] = b;
        }
    }
    out
}

/// The inverse of [`interleave`].
pub fn deinterleave(rows: &[[u8; SLOT_BYTES]; FRAMES]) -> [[u8; SLOT_BYTES]; FRAMES] {
    let mut out = [[0u8; SLOT_BYTES]; FRAMES];
    for (i, o) in out.iter_mut().enumerate() {
        for (j, b) in o.iter_mut().enumerate() {
            let (f, k) = interleave_position(i, j);
            *b = rows[f][k];
        }
    }
    out
}

/// The RS codewords (204 bytes, sync byte restored, not yet RS-decoded) of
/// a superframe, from the received bytes of its 8 frames: `frames[f]` is
/// frame `f`'s output of the inner decoder, 203 bytes per sent slot in the
/// order of [`sent_slots`]. Returns `(frame, slot, codeword)` in the order
/// the packets were sent (frame by frame, slot by slot).
pub fn codewords(
    info: &TmccInfo,
    frames: &[&[u8]; FRAMES],
) -> Vec<(usize, usize, [u8; PACKET_BYTES])> {
    let slots = sent_slots(info);
    let d = dispersal();
    let mut out = vec![(0, 0, [0u8; PACKET_BYTES]); FRAMES * slots.len()];
    for (n, &s) in slots.iter().enumerate() {
        let rows: [[u8; SLOT_BYTES]; FRAMES] = std::array::from_fn(|f| {
            frames[f][n * SLOT_BYTES..(n + 1) * SLOT_BYTES]
                .try_into()
                .expect("203 bytes per sent slot")
        });
        let orig = deinterleave(&rows);
        for (i, row) in orig.iter().enumerate() {
            let mut cw = [0u8; PACKET_BYTES];
            cw[0] = SYNC_BYTE;
            for (j, &b) in row.iter().enumerate() {
                cw[1 + j] = b ^ d[superframe_position(i, s, j)];
            }
            out[i * slots.len() + n] = (i, s, cw);
        }
    }
    out
}

/// The transmit direction of [`codewords`]: from the 204-byte RS
/// codewords of every sent slot of 8 frames (`packets[f][n]` for sent slot
/// `n`), the bytes the inner coder takes for each frame (203 per sent slot,
/// slot order). The sync bytes are dropped.
pub fn frame_bytes(
    info: &TmccInfo,
    packets: &[Vec<[u8; PACKET_BYTES]>; FRAMES],
) -> [Vec<u8>; FRAMES] {
    let slots = sent_slots(info);
    let d = dispersal();
    let mut out: [Vec<u8>; FRAMES] = std::array::from_fn(|_| vec![0u8; slots.len() * SLOT_BYTES]);
    for (n, &s) in slots.iter().enumerate() {
        let rows: [[u8; SLOT_BYTES]; FRAMES] = std::array::from_fn(|i| {
            std::array::from_fn(|j| packets[i][n][1 + j] ^ d[superframe_position(i, s, j)])
        });
        let il = interleave(&rows);
        for (f, row) in il.iter().enumerate() {
            out[f][n * SLOT_BYTES..(n + 1) * SLOT_BYTES].copy_from_slice(row);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tmcc::{CM, CombinationEntry, CombinationWord, TM};
    use isdb_fec::RS_204_188;
    use proptest::prelude::*;

    fn info(c: &[(Combination, u8)]) -> TmccInfo {
        let mut combinations = [CombinationEntry::DUMMY; CM];
        for (i, &(w, s)) in c.iter().enumerate() {
            combinations[i] = CombinationEntry {
                word: CombinationWord::Assigned(w),
                slots: s,
            };
        }
        TmccInfo {
            order_of_change: 0,
            combinations,
            relative_ts: [0; SLOTS],
            ts_ids: [0xFFFF; TM],
            emergency: false,
            uplink: 0,
            reserved: 0,
        }
    }

    /// BO.1408-1 Fig. 6: the first interleaved row is A1,1 A2,1 … A3,26,
    /// the second starts A4,26, the third A7,51 A8,51 A1,52, and the last
    /// ends at A8,203 (frames and bytes numbered from 1 in the figure).
    #[test]
    fn spec_fig_6_interleaver() {
        let at = |i: usize, j: usize| interleave_position(i - 1, j - 1);
        assert_eq!(at(1, 1), (0, 0));
        assert_eq!(at(2, 1), (0, 1));
        assert_eq!(at(3, 26), (0, 202));
        assert_eq!(at(4, 26), (1, 0));
        assert_eq!(at(7, 51), (2, 0));
        assert_eq!(at(1, 52), (2, 2));
        assert_eq!(at(6, 178), (7, 0));
        assert_eq!(at(8, 203), (7, 202));
    }

    /// Annexed table 48 別記 1 / BO.1408-1 §4: no dispersal on the first
    /// byte of the superframe or on any sync byte; the PRBS's first byte
    /// lands on the second byte of slot 1, frame 1; it keeps running over
    /// the sync bytes.
    #[test]
    fn spec_dispersal_alignment() {
        let d = dispersal();
        let mut p = Prbs15::new();
        assert_eq!(d[0], 0);
        assert_eq!(d[1], p.next_byte());
        for (k, &v) in d.iter().enumerate().take(PACKET_BYTES).skip(2) {
            assert_eq!(v, p.next_byte(), "{k}");
        }
        // the sync byte of slot 2 is skipped, its PRBS byte discarded
        let skipped = p.next_byte();
        assert_eq!(d[PACKET_BYTES], 0);
        assert_ne!(skipped, 0);
        assert_eq!(d[PACKET_BYTES + 1], p.next_byte());
        assert!(
            (0..SUPERFRAME_BYTES)
                .step_by(PACKET_BYTES)
                .all(|i| d[i] == 0)
        );
    }

    /// Dummy slots of annexed table 48 別記 3: not sent, the valid slots of
    /// each unit first.
    #[test]
    fn spec_sent_slots() {
        assert_eq!(
            sent_slots(&info(&[(Combination::Tc8psk2_3, 48)])),
            (0..48).collect::<Vec<_>>()
        );
        let cs = sent_slots(&info(&[(Combination::Qpsk3_4, 48)]));
        assert_eq!(cs.len(), 36);
        assert_eq!(&cs[..6], &[0, 1, 2, 4, 5, 6]);
        let mixed = sent_slots(&info(&[
            (Combination::Tc8psk2_3, 44),
            (Combination::Bpsk1_2, 4),
        ]));
        assert_eq!(mixed.len(), 45);
        assert_eq!(*mixed.last().unwrap(), 44);
    }

    proptest! {
        /// The interleaver is a permutation and `deinterleave` undoes it.
        #[test]
        fn prop_interleave_round_trip(seed in any::<u64>()) {
            let mut x = seed | 1;
            let rows: [[u8; SLOT_BYTES]; FRAMES] = std::array::from_fn(|_| std::array::from_fn(|_| {
                x ^= x << 13; x ^= x >> 7; x ^= x << 17; x as u8
            }));
            prop_assert_eq!(deinterleave(&interleave(&rows)), rows);
        }

        /// `codewords` inverts `frame_bytes`: random TS packets with RS
        /// parity go through dispersal and interleaving and come back as
        /// valid codewords, in sent order.
        #[test]
        fn prop_superframe_round_trip(seed in any::<u64>(), cs in any::<bool>()) {
            let inf = if cs { info(&[(Combination::Qpsk3_4, 48)]) } else { info(&[(Combination::Tc8psk2_3, 44), (Combination::Qpsk1_2, 4)]) };
            let n = sent_slots(&inf).len();
            let mut x = seed | 1;
            let packets: [Vec<[u8; PACKET_BYTES]>; FRAMES] = std::array::from_fn(|_| {
                (0..n).map(|_| {
                    let mut p = [0u8; PACKET_BYTES];
                    p[0] = SYNC_BYTE;
                    for b in &mut p[1..188] { x ^= x << 13; x ^= x >> 7; x ^= x << 17; *b = x as u8; }
                    RS_204_188.encode_in_place(&mut p);
                    p
                }).collect()
            });
            let fb = frame_bytes(&inf, &packets);
            let refs: [&[u8]; FRAMES] = std::array::from_fn(|f| fb[f].as_slice());
            let cw = codewords(&inf, &refs);
            prop_assert_eq!(cw.len(), FRAMES * n);
            for (k, (i, _s, c)) in cw.iter().enumerate() {
                prop_assert_eq!(*i, k / n);
                prop_assert_eq!(c, &packets[k / n][k % n]);
            }
        }
    }
}
