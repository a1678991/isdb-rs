//! Slot allocation of a frame (ARIB STD-B44 v2.1 §3.2 (2), Tables 3.2-2/3,
//! §3.11 (2)) and where each slot's LDPC codeword sits in the frame's main
//! symbols.
//!
//! Modes 1…8 take consecutive slots from slot #1 in the order the TMCC
//! lists them. Within each unit of five slots of a mode, the first
//! `valid_per_unit` slots carry data and the rest are dummies (Fig. 3.2-3).
//!
//! The main signal of a frame is one symbol stream: Fig. 3.3-1 numbers its
//! parts 1 … 7 920 continuously across the 120 modulation slots, 136
//! symbols each. Each valid slot's 44 880-bit codeword (after the bit
//! interleaver) occupies 44 880 / bits consecutive symbols of that stream,
//! in slot order; a unit of five modulation slots holds exactly the
//! unit's valid codewords (`params` test "main symbols fill whole
//! codewords"). Dummy slots take no symbols. (SPEC-NOTES "Codeword
//! placement".)

use crate::params::{CodeRate, MAIN_PER_MOD_SLOT, Modulation, N_LDPC, SLOT_UNIT, SLOTS};
use crate::tmcc::TmccInfo;

/// One slot of the 120.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Slot {
    /// 0-based slot index (slot #1 = 0).
    pub index: usize,
    /// Transmission mode index 0…7.
    pub mode: usize,
    /// Modulation of the mode.
    pub modulation: Modulation,
    /// Code rate of the mode.
    pub rate: CodeRate,
    /// Whether the slot is a dummy.
    pub dummy: bool,
    /// First main-signal symbol of its codeword (valid slots only).
    pub symbol_start: usize,
    /// Symbols of its codeword: 44 880 / bits (0 for dummies).
    pub symbols: usize,
}

/// The slots of a frame from its TMCC; `None` when the allocation is
/// inconsistent ([`TmccInfo::allocation_ok`]).
pub fn layout(t: &TmccInfo) -> Option<Vec<Slot>> {
    if !t.allocation_ok() {
        return None;
    }
    let mut out = Vec::with_capacity(SLOTS);
    let mut sym = 0usize;
    for (mi, m) in t.modes.iter().enumerate().filter(|(_, m)| m.is_used()) {
        let (modulation, rate) = (m.modulation()?, m.rate()?);
        let valid = modulation.valid_per_unit();
        for j in 0..m.slots as usize {
            let dummy = j % SLOT_UNIT >= valid;
            let symbols = if dummy { 0 } else { N_LDPC / modulation.bits() };
            out.push(Slot {
                index: out.len(),
                mode: mi,
                modulation,
                rate,
                dummy,
                symbol_start: sym,
                symbols,
            });
            sym += symbols;
        }
    }
    (out.len() == SLOTS && sym == SLOTS * MAIN_PER_MOD_SLOT).then_some(out)
}

/// The modulation of modulation slot `k` (0-based): that of the slot with
/// the same index. Used for the pilot symbols (§3.9; SPEC-NOTES "Pilot
/// modulation").
pub fn modulation_of(layout: &[Slot], k: usize) -> Modulation {
    layout[k].modulation
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tmcc::Mode;

    fn info(modes: &[(Modulation, u8, u8)]) -> TmccInfo {
        let mut t = crate::tmcc::tests::sample();
        t.modes = [Mode::UNUSED; 8];
        for (i, &(m, r, s)) in modes.iter().enumerate() {
            t.modes[i] = Mode {
                modulation_code: m.code(),
                rate_code: CodeRate::new(r).unwrap().code(),
                slots: s,
                obo: 0,
            };
        }
        t
    }

    #[test]
    fn spec_fig_3_2_3_c_8psk_plus_qpsk() {
        // (c): 8PSK in 1…115, QPSK in 116…120: 8PSK units 3 valid + 2 dummy,
        // QPSK slots 116, 117 valid, 118–120 dummy
        let l = layout(&info(&[
            (Modulation::Psk8, 93, 115),
            (Modulation::Qpsk, 61, 5),
        ]))
        .unwrap();
        assert!(!l[0].dummy && !l[2].dummy && l[3].dummy && l[4].dummy);
        assert!(!l[115].dummy && !l[116].dummy && l[117].dummy && l[119].dummy);
    }

    #[test]
    fn all_16apsk_uses_96_codewords() {
        let l = layout(&info(&[(Modulation::Apsk16, 93, 120)])).unwrap();
        assert_eq!(l.iter().filter(|s| !s.dummy).count(), 96);
        assert_eq!(l[1].symbol_start, 11_220);
    }

    #[test]
    fn bad_allocations_are_refused() {
        assert!(layout(&info(&[(Modulation::Apsk16, 93, 115)])).is_none());
        // wrong order: lower modulation first
        assert!(
            layout(&info(&[
                (Modulation::Qpsk, 61, 5),
                (Modulation::Psk8, 93, 115)
            ]))
            .is_none()
        );
    }
}
