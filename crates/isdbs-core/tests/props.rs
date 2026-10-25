//! Property tests: the TMCC codec is lossless, a superframe of headers
//! survives up to 8 byte errors, and any valid assignment fills the main
//! signal exactly.

use isdbs_core::combination::Combination;
use isdbs_core::frame::{MAIN_SYMBOLS, SLOTS};
use isdbs_core::slots::runs;
use isdbs_core::tmcc::{CombinationEntry, INFO_BYTES, TmccInfo, decode_blocks, headers};
use proptest::prelude::*;

proptest! {
    /// `encode(decode(x)) == x` for any 48 bytes: every bit is a field.
    #[test]
    fn prop_tmcc_info_lossless(v in proptest::collection::vec(any::<u8>(), INFO_BYTES)) {
        let b: [u8; INFO_BYTES] = v.try_into().unwrap();
        prop_assert_eq!(TmccInfo::decode(&b).encode(), b);
    }

    /// Headers → (≤ 8 corrupted TMCC bytes) → the same information.
    #[test]
    fn prop_tmcc_signal_corrects_8_bytes(
        v in proptest::collection::vec(any::<u8>(), INFO_BYTES),
        errs in proptest::collection::vec((0usize..64, 1u8..=255), 0..=8),
    ) {
        let info = TmccInfo::decode(&v.try_into().unwrap());
        let h = headers(&info);
        let mut blocks: [[u8; 8]; 8] = std::array::from_fn(|f| h[f][2..10].try_into().unwrap());
        let mut seen = std::collections::HashSet::new();
        for (pos, e) in errs {
            if seen.insert(pos) {
                blocks[pos / 8][pos % 8] ^= e;
            }
        }
        let d = decode_blocks(&blocks);
        prop_assert_eq!(d.info, Some(info));
        prop_assert_eq!(d.corrected, Some(seen.len()));
    }

    /// Any split of 48 slots into up to four combinations (descending
    /// efficiency, whole units) validates, and its runs cover the 38 976
    /// main symbols exactly, back to back.
    #[test]
    fn prop_slot_assignment_fills_the_frame(
        picks in proptest::collection::vec((0usize..7, 1usize..=12), 1..=4),
    ) {
        let mut chosen: Vec<(Combination, usize)> = Vec::new();
        for (c, units) in picks {
            let comb = Combination::ALL[c];
            if chosen.iter().all(|&(x, _)| x != comb) {
                chosen.push((comb, units));
            }
        }
        chosen.sort_by_key(|&(c, _)| Combination::ALL.iter().position(|&x| x == c));
        // scale to 48 slots: fill with TC8PSK (unit 1) or drop if too long
        let mut entries = Vec::new();
        let mut used = 0;
        for &(c, units) in &chosen {
            let n = units * c.slot_unit().0;
            if used + n <= SLOTS {
                entries.push((c, n));
                used += n;
            }
        }
        prop_assume!(!entries.is_empty());
        // pad with the first entry's combination when it is TC8PSK, else
        // with TC8PSK in front
        let pad = SLOTS - used;
        if pad > 0 {
            if entries[0].0 == Combination::Tc8psk2_3 {
                entries[0].1 += pad;
            } else if entries.len() < 4 {
                entries.insert(0, (Combination::Tc8psk2_3, pad));
            } else {
                return Ok(());
            }
        }
        let mut combinations = [CombinationEntry { word: 0xF, slots: 0 }; 4];
        for (i, &(c, n)) in entries.iter().enumerate() {
            combinations[i] = CombinationEntry { word: c.word(), slots: n as u8 };
        }
        let info = TmccInfo {
            order_of_change: 0,
            combinations,
            relative_ts: [0; SLOTS],
            ts_ids: [0xFFFF; 8],
            emergency: false,
            uplink: 0,
            reserved: 0,
        };
        prop_assert!(info.validate().is_ok(), "{:?}", info.combinations);
        let r = runs(&info);
        let mut at = 0;
        for run in &r {
            prop_assert_eq!(run.symbols.start, at);
            at = run.symbols.end;
        }
        prop_assert_eq!(at, MAIN_SYMBOLS);
        let valid: usize = r.iter().map(|x| x.valid_slots).sum();
        prop_assert_eq!(valid, info.slot_combinations().iter().filter(|c| c.is_some()).count());
    }
}
