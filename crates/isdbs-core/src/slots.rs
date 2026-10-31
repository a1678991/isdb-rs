//! Where each combination's slots sit in the frame's main signal.
//!
//! BO.1408-1 §3.2 and §8: slots are assigned from the most to the least
//! efficient combination, dummy slots are not transmitted, and every unit
//! of annexed table 48 lasts a whole number of 812-symbol TC8PSK slot
//! durations. A combination's run of slots therefore occupies a contiguous
//! range of main-signal symbols, starting where the previous run ended.
//! The puncturing pattern restarts at the start of each run (Table 2 note
//! 1: "initialized at the start of the successive slots that are assigned
//! to the corresponding code"; annexed table 49 note 5).
//!
//! [`packet_symbol`] places the packets of one TS on that timeline: where
//! in the superframe packet k of relative TS r starts, the on-air time
//! both the ISDB-S pacer and the in-process multiplexer stamp PCRs from
//! (docs/plans/isdbs-next/media-and-margin.md §3.6). With
//! [`crate::frame::symbols_to_ticks`] it becomes 27 MHz ticks.

use crate::combination::Combination;
use crate::frame::{FRAME_SYMBOLS, FRAMES, MAIN_SYMBOLS, SLOT_BYTES, main_symbol_position};
use crate::tmcc::TmccInfo;
use std::ops::Range;

/// Information bits of a slot on air: 203 bytes (Table 3 item 3; the MPEG
/// sync byte is not sent).
pub const SLOT_BITS: usize = SLOT_BYTES * 8;

/// One combination's slots in a frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Run {
    /// The combination.
    pub combination: Combination,
    /// Its slots (1-based slot numbers are `slots.start + 1 ..`), dummy
    /// slots included.
    pub slots: Range<usize>,
    /// Valid (non-dummy) slots in the run.
    pub valid_slots: usize,
    /// Main-signal symbols (index into the frame's 38 976 main symbols).
    pub symbols: Range<usize>,
}

/// The runs of `info`, in slot order. Dummy (1111) entries occupy slots but
/// no symbols and give no run.
pub fn runs(info: &TmccInfo) -> Vec<Run> {
    let mut out = Vec::new();
    let (mut slot, mut sym) = (0usize, 0usize);
    for c in &info.combinations {
        let n = usize::from(c.slots);
        if let Some(comb) = c.combination() {
            let (unit, valid) = comb.slot_unit();
            let units = n / unit;
            let len = units * comb.symbols_per_unit();
            out.push(Run {
                combination: comb,
                slots: slot..slot + n,
                valid_slots: units * valid,
                symbols: sym..(sym + len).min(MAIN_SYMBOLS),
            });
            sym += len;
        }
        slot += n;
    }
    out
}

/// The symbol, counted from the start of a run of combination `c`, that
/// carries the first coded bit of the run's information bit `b` (bit 0 is
/// the MSB of the run's first byte).
///
/// Unpunctured codes place E = n/d information bits per symbol (BO.1408-1
/// §3.2 equation (1), [`Combination::efficiency`]): symbol ⌊b · d / n⌋
/// (TC8PSK 2/3: two bits per symbol; QPSK 1/2: one; BPSK 1/2: one bit in
/// two symbols). The punctured QPSK codes send the mother-code bits of
/// Table 2 / annexed table 49 note 4 two per symbol, the pattern restarting
/// with the run (note 1); input bit i of a period is first sent in the
/// earliest pair holding X_i or Y_i (both outputs of the r = 1/2 mother
/// code, generators 171₈ and 133₈, depend on the current input bit).
pub const fn bit_symbol(c: Combination, b: usize) -> usize {
    match c.puncturing() {
        Some(p) => {
            let (period, pairs) = (p.period(), p.pairs());
            let i = b % period + 1;
            let mut j = 0;
            while j < pairs.len() {
                let ((_, x), (_, y)) = pairs[j];
                if x == i || y == i {
                    break;
                }
                j += 1;
            }
            (b / period) * pairs.len() + j
        }
        None => {
            let (n, d) = c.efficiency();
            b * d / n
        }
    }
}

/// Relative TS ID of slot `s` (0..48): the 3-bit TMCC field (Fig. 9).
fn slot_ts(info: &TmccInfo, s: usize) -> usize {
    usize::from(info.relative_ts[s] & 7)
}

/// The sent slots (BO.1408-1 §8: dummy slots are not sent) whose relative
/// TS ID is `r` (Fig. 9), in slot order.
fn ts_slot_numbers(info: &TmccInfo, r: usize) -> impl Iterator<Item = usize> {
    info.slot_combinations()
        .into_iter()
        .enumerate()
        .filter(move |&(s, c)| c.is_some() && slot_ts(info, s) == r)
        .map(|(s, _)| s)
}

/// Sent slots of relative TS `r` per frame: the valid slots (BO.1408-1 §8:
/// dummy slots are not sent) whose relative TS ID is `r` (Fig. 9). A
/// superframe carries 8 times as many of its packets.
pub fn ts_slots(info: &TmccInfo, r: usize) -> usize {
    ts_slot_numbers(info, r).count()
}

/// Frame (0..8) and slot (0..48) of packet `k` of relative TS `r` in a
/// superframe: frame ⌊k / n_r⌋, its (k mod n_r)-th sent slot of `r` in
/// slot order, n_r = [`ts_slots`]. This is the order in which a
/// transmitter fills the slots (Fig. 9: each slot carries the next packet
/// of the TS its relative TS ID names; `isdbs_phy::tx::Transmitter`) and in
/// which a receiver returns the packets. `None` when `k` ≥ 8 · n_r.
pub fn packet_slot(info: &TmccInfo, r: usize, k: usize) -> Option<(usize, usize)> {
    let n = ts_slots(info, r);
    if n == 0 || k >= FRAMES * n {
        return None;
    }
    Some((k / n, ts_slot_numbers(info, r).nth(k % n)?))
}

/// Symbol within the frame (0..39 936) at which on-air byte `byte`
/// (0..203: the bytes after the sync byte) of slot `slot` starts: the
/// symbol that carries its first bit ([`bit_symbol`]) in the slot's run
/// ([`runs`]), placed by [`main_symbol_position`] past the header and the
/// bursts. `None` for a dummy slot or `byte` ≥ 203.
pub fn slot_byte_symbol(info: &TmccInfo, slot: usize, byte: usize) -> Option<usize> {
    if byte >= SLOT_BYTES {
        return None;
    }
    let comb = info.slot_combinations();
    comb.get(slot).copied().flatten()?;
    let run = runs(info).into_iter().find(|r| r.slots.contains(&slot))?;
    // valid slots of the run before this one: the bytes of the run's valid
    // slots follow each other in slot order (`mainsig::frame_bytes`)
    let v = (run.slots.start..slot)
        .filter(|&s| comb[s].is_some())
        .count();
    let m = run.symbols.start + bit_symbol(run.combination, v * SLOT_BITS + 8 * byte);
    (m < run.symbols.end).then(|| main_symbol_position(m))
}

/// Symbol within the superframe (0..319 488) at which on-air byte `byte`
/// (0..203; packet byte `byte + 1`, the sync byte is not sent) of packet `k`
/// of relative TS `r` starts: [`packet_slot`] gives its frame f and slot,
/// [`slot_byte_symbol`] the symbol in the frame, plus 39 936 · f.
///
/// This is the position the byte has before the inter-frame interleaver
/// (BO.1408-1 §5, Fig. 6), which moves byte j of a frame-f slot to frame
/// ⌊(8j + f)/203⌋ of the same slot number (`mainsig::interleave_position`):
/// a receiver has every byte of a superframe only after its frame 8, and
/// its deinterleaver gives each byte back one constant delay after this
/// position. The byte holding the last bit of a PCR's
/// program_clock_reference_base (packet byte 10, H.222.0 §2.4.3.4) is
/// `byte` 9.
pub fn packet_byte_symbol(info: &TmccInfo, r: usize, k: usize, byte: usize) -> Option<usize> {
    let (f, s) = packet_slot(info, r, k)?;
    Some(f * FRAME_SYMBOLS + slot_byte_symbol(info, s, byte)?)
}

/// Symbol within the superframe (0..319 488) at which packet `k` (0..8 ·
/// n_r) of relative TS `r` starts: [`packet_byte_symbol`] of on-air byte 0.
/// `None` when `r` has no slot or `k` ≥ 8 · [`ts_slots`].
///
/// On the bench layout (TC8PSK 2/3 × 48, one TS) packet k = 48 f + s starts
/// at 39 936 f + 192 + 828 s: 812 main symbols per slot plus 4 bursts of 4
/// symbols.
pub fn packet_symbol(info: &TmccInfo, r: usize, k: usize) -> Option<usize> {
    packet_byte_symbol(info, r, k, 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tmcc::{CM, CombinationEntry, CombinationWord, TM};

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
            relative_ts: [0; 48],
            ts_ids: [0xFFFF; TM],
            emergency: false,
            uplink: 0,
            reserved: 0,
        }
    }

    /// BO.1408-1 Fig. 4 examples: the runs fill the 38 976 main symbols
    /// exactly whenever the counts sum to 48 in whole units.
    #[test]
    fn spec_fig_4_examples() {
        // a) TC8PSK + QPSK 1/2: 46 + 2 slots, one dummy.
        let r = runs(&info(&[
            (Combination::Tc8psk2_3, 46),
            (Combination::Qpsk1_2, 2),
        ]));
        assert_eq!(r[0].symbols, 0..46 * 812);
        assert_eq!((r[1].valid_slots, r[1].symbols.end), (1, MAIN_SYMBOLS));
        // b) TC8PSK + BPSK 1/2: 44 + 4, three dummies.
        let r = runs(&info(&[
            (Combination::Tc8psk2_3, 44),
            (Combination::Bpsk1_2, 4),
        ]));
        assert_eq!((r[1].valid_slots, r[1].symbols.end), (1, MAIN_SYMBOLS));
        // c) TC8PSK + QPSK 3/4: 44 + 4, one dummy for three active slots.
        let r = runs(&info(&[
            (Combination::Tc8psk2_3, 44),
            (Combination::Qpsk3_4, 4),
        ]));
        assert_eq!((r[1].valid_slots, r[1].symbols.end), (3, MAIN_SYMBOLS));
        // single QPSK 3/4 (the CS transponders): 36 valid slots.
        let r = runs(&info(&[(Combination::Qpsk3_4, 48)]));
        assert_eq!(
            (r[0].valid_slots, r[0].symbols.clone()),
            (36, 0..MAIN_SYMBOLS)
        );
    }

    /// The bench layout (TC8PSK 2/3 × 48, one TS): packet 48 f + s starts
    /// at symbol 39 936 f + 192 + 828 s, 192 − 4 s symbols after the
    /// uniform 832-symbol grid (docs/plans/isdbs-next/pcr-pacing.md §3.2:
    /// a frame-periodic sawtooth from 6.65 µs down to 0.14 µs).
    #[test]
    fn bench_layout_slot_symbols() {
        use crate::frame::{SUPERFRAME_SYMBOLS, SYMBOL_RATE, symbols_to_ticks};
        let i = TmccInfo::assign(&[(Combination::Tc8psk2_3, 48)], &[(0x4010, 48)]).unwrap();
        assert_eq!(ts_slots(&i, 0), 48);
        assert_eq!(ts_slots(&i, 1), 0);
        for k in 0..FRAMES * 48 {
            let (f, s) = (k / 48, k % 48);
            assert_eq!(packet_slot(&i, 0, k), Some((f, s)));
            let at = packet_symbol(&i, 0, k).unwrap();
            assert_eq!(at, 39_936 * f + 192 + 828 * s, "packet {k}");
            assert_eq!(at - 832 * k, 192 - 4 * s);
            // PCR byte (on-air byte 9): 9 bytes of 4 symbols, one burst
            // further when the slot crosses one (never within 36 symbols)
            assert_eq!(packet_byte_symbol(&i, 0, k, 9), Some(at + 36));
        }
        assert_eq!(packet_symbol(&i, 0, FRAMES * 48), None);
        assert_eq!(packet_symbol(&i, 1, 0), None);
        assert_eq!(packet_byte_symbol(&i, 0, 0, 203), None);
        // 6.65 µs down to 0.14 µs at 28.86 Mbaud
        let ns = |sym: u64| sym * 1_000_000_000 / SYMBOL_RATE;
        assert_eq!(ns(192), 6_652);
        assert_eq!(ns(4), 138);
        assert_eq!(symbols_to_ticks(192), 179);
        assert!(packet_symbol(&i, 0, FRAMES * 48 - 1).unwrap() < SUPERFRAME_SYMBOLS);
    }

    /// pcr-pacing.md §3.2: a valid slot of a run lasts u/v × 812 symbols
    /// (812 TC8PSK, 1 082⅔ QPSK 3/4, 1 624 QPSK 1/2, 3 248 BPSK 1/2); its
    /// first symbol is the one carrying its first bit.
    #[test]
    fn slot_lengths_per_combination() {
        let starts = |c: Combination| -> Vec<usize> {
            (0..4).map(|v| bit_symbol(c, v * SLOT_BITS)).collect()
        };
        assert_eq!(starts(Combination::Tc8psk2_3), [0, 812, 1624, 2436]);
        assert_eq!(starts(Combination::Qpsk1_2), [0, 1624, 3248, 4872]);
        assert_eq!(starts(Combination::Bpsk1_2), [0, 3248, 6496, 9744]);
        assert_eq!(starts(Combination::Qpsk2_3), [0, 1218, 2436, 3654]);
        assert_eq!(starts(Combination::Qpsk7_8), [0, 928, 1856, 2784]);
        // 1624 bits = 541 periods of 3 + 1: the slot's first bit is the
        // period's second (Y2, the second pair) → ⌈1624 · 2/3⌉
        assert_eq!(starts(Combination::Qpsk3_4), [0, 1083, 2165, 3248]);
        // 5/6: X1 Y1 | Y2 X3 | Y4 X5; 1624 = 5 · 324 + 4 → bit 5 (X5): pair 2
        assert_eq!(bit_symbol(Combination::Qpsk5_6, SLOT_BITS), 3 * 324 + 2);
        // a whole unit lasts exactly its 812 · u symbols
        for c in Combination::ALL {
            let (u, v) = c.slot_unit();
            assert_eq!(bit_symbol(c, v * SLOT_BITS), u * 812, "{c}");
        }
    }

    /// Packets of several TSs and of runs with dummy slots: frame by frame,
    /// each TS's own sent slots in slot order; a run's valid slots follow
    /// each other in its symbols.
    #[test]
    fn packets_of_mixed_layouts() {
        // TC8PSK 44 + QPSK 3/4 4 (3 valid, 1 dummy); TS 0 40 slots, TS 1 7
        let i = TmccInfo::assign(
            &[(Combination::Tc8psk2_3, 44), (Combination::Qpsk3_4, 4)],
            &[(0x4010, 40), (0x4011, 7)],
        )
        .unwrap();
        assert_eq!((ts_slots(&i, 0), ts_slots(&i, 1)), (40, 7));
        // TS 1: slots 40..44 (TC8PSK), then 44, 45, 46 (QPSK 3/4); 47 dummy
        let want = [40, 41, 42, 43, 44, 45, 46];
        for (q, &s) in want.iter().enumerate() {
            assert_eq!(packet_slot(&i, 1, 7 + q), Some((1, s)));
        }
        assert_eq!(slot_byte_symbol(&i, 47, 0), None);
        let run2 = 44 * 812;
        for (v, sym) in [(0, 0), (1, 1083), (2, 2165)] {
            assert_eq!(
                packet_symbol(&i, 1, 4 + v),
                Some(main_symbol_position(run2 + sym))
            );
        }
        assert_eq!(packet_symbol(&i, 1, 7 * 8), None);
    }
}
