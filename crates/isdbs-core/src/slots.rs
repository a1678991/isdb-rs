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

use crate::combination::Combination;
use crate::frame::MAIN_SYMBOLS;
use crate::tmcc::TmccInfo;
use std::ops::Range;

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tmcc::{CM, CombinationEntry, TM};

    fn info(c: &[(u8, u8)]) -> TmccInfo {
        let mut combinations = [CombinationEntry {
            word: 0xF,
            slots: 0,
        }; CM];
        for (i, &(w, s)) in c.iter().enumerate() {
            combinations[i] = CombinationEntry { word: w, slots: s };
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
        let r = runs(&info(&[(7, 46), (2, 2)]));
        assert_eq!(r[0].symbols, 0..46 * 812);
        assert_eq!((r[1].valid_slots, r[1].symbols.end), (1, MAIN_SYMBOLS));
        // b) TC8PSK + BPSK 1/2: 44 + 4, three dummies.
        let r = runs(&info(&[(7, 44), (1, 4)]));
        assert_eq!((r[1].valid_slots, r[1].symbols.end), (1, MAIN_SYMBOLS));
        // c) TC8PSK + QPSK 3/4: 44 + 4, one dummy for three active slots.
        let r = runs(&info(&[(7, 44), (4, 4)]));
        assert_eq!((r[1].valid_slots, r[1].symbols.end), (3, MAIN_SYMBOLS));
        // single QPSK 3/4 (the CS transponders): 36 valid slots.
        let r = runs(&info(&[(4, 48)]));
        assert_eq!(
            (r[0].valid_slots, r[0].symbols.clone()),
            (36, 0..MAIN_SYMBOLS)
        );
    }
}
