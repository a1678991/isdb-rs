//! Energy dispersal of ISDB-S3 (ARIB STD-B44 v2.1 §3.6). Every generator
//! outputs its last stage ([`isdb_fec::prbs::B44Prbs`]).
//!
//! - Slot: X²⁵ + X²² + 1 from `1010000000000000000011010` at the start of
//!   each frame, XORed onto the header, data, BCH parity and stuffing of
//!   each valid slot in order; halted over LDPC parity and dummy slots
//!   (§3.6 (1)).
//! - TMCC: in [`crate::tmcc::disperse`].
//! - Pilot: X¹⁵ + X¹⁴ + 1 from `100000000101100` at the start of each frame,
//!   one bit per pilot symbol (32 per modulation slot), 1 turning the
//!   point by 180° (§3.6 (3)).

use isdb_fec::prbs::B44Prbs;

/// The slot dispersal generator of one frame.
#[derive(Debug, Clone)]
pub struct SlotDispersal(B44Prbs);

impl Default for SlotDispersal {
    fn default() -> Self {
        Self::new()
    }
}

impl SlotDispersal {
    /// At the start of a frame.
    pub fn new() -> Self {
        Self(B44Prbs::slot())
    }
    /// XOR the next `bits.len()` PRBS bits onto `bits` (the dispersed part
    /// of one valid slot: header + data + BCH parity + stuffing).
    pub fn apply(&mut self, bits: &mut [u8]) {
        for b in bits {
            *b ^= self.0.next_bit();
        }
    }
}

/// The pilot signs (+1 / −1) of a frame: 32 per modulation slot, 120
/// slots, in order.
pub fn pilot_signs() -> Vec<i8> {
    let mut g = B44Prbs::pilot();
    (0..crate::params::SLOTS * crate::params::PILOT_SYMBOLS)
        .map(|_| if g.next_bit() == 1 { -1 } else { 1 })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spec_initial_states() {
        assert_eq!(B44Prbs::slot().state_string(), "1010000000000000000011010");
        assert_eq!(B44Prbs::pilot().state_string(), "100000000101100");
        assert_eq!(B44Prbs::tmcc().state_string(), "100000000001110");
    }

    #[test]
    fn slot_dispersal_is_an_involution() {
        let mut a = vec![0u8; 1000];
        SlotDispersal::new().apply(&mut a);
        assert!(a.contains(&1));
        SlotDispersal::new().apply(&mut a);
        assert!(a.iter().all(|&b| b == 0));
    }
}
