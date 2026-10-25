//! Energy dispersal (ARIB STD-B31 §3.5, Fig. 3-8).
//!
//! > "The initial value of the register in the PRBS-generating circuit
//! > must be “100101010000000” (arranged in ascending order of bits, from
//! > left to right), and this value must be initialized every OFDM
//! > frame." — §3.5
//!
//! In short (§3.5): every bit of each transmission TSP except its sync byte
//! is XORed with the PRBS, per layer; the PRBS restarts every OFDM frame,
//! its first bit meeting the MSB of the byte after a TSP's sync byte, and
//! the register keeps shifting during sync bytes (which are not XORed).
//!
//! Reading of Fig. 3-8 (g(x) = X¹⁵ + X¹⁴ + 1): fifteen D stages numbered
//! 1..15 shift from stage 1 towards stage 15; the outputs of stages 14 and
//! 15 are XORed, the sum is fed back into stage 1, and the **output** is
//! taken from that feedback node (the same bit that enters stage 1). The
//! initial string assigns its leftmost character to stage 1 ("ascending
//! order of bits, from left to right"). See SPEC-NOTES.md.
//!
//! The XOR is self-inverse, so the same [`Prbs`] / [`disperse_frame`] are
//! used on the receiving side.

use isdbt_core::TRANSMISSION_TSP;

/// Initial register contents "100101010000000", stage 1 first (§3.5).
pub const INITIAL_STATE: &str = isdb_fec::prbs::PRBS15_INITIAL_STATE;

/// Period of the maximal-length sequence of g(x) = X¹⁵ + X¹⁴ + 1:
/// 2¹⁵ − 1 (§3.5, Fig. 3-8).
pub const PERIOD: usize = isdb_fec::prbs::PRBS15_PERIOD;

/// The PRBS generator of Fig. 3-8 (§3.5): `isdb_fec::Prbs15`, shared with
/// ISDB-S, which restarts it per superframe instead of per OFDM frame.
pub type Prbs = isdb_fec::Prbs15;

/// PRBS bytes for one layer frame of `slots` transmission-TSP slots,
/// starting from the reset state: element *i* is the byte XORed onto layer
/// stream byte *i* (sync-byte positions included, where it is not applied).
pub fn frame_sequence(slots: usize) -> Vec<u8> {
    let mut p = Prbs::new();
    (0..slots * TRANSMISSION_TSP)
        .map(|_| p.next_byte())
        .collect()
}

/// Apply energy dispersal to one layer frame held in the §3.4 "shifted"
/// byte order: the frame is a sequence of 204-byte slots, each = the 203
/// bytes following a sync byte, then the next sync byte (slot offset 203).
/// The PRBS restarts at the frame start, advances through every byte, and is
/// not applied at sync-byte offsets (§3.5). Self-inverse.
pub fn disperse_frame(frame: &mut [u8], prbs: &[u8]) {
    assert_eq!(frame.len() % TRANSMISSION_TSP, 0);
    assert!(prbs.len() >= frame.len());
    for (slot, seq) in frame
        .as_chunks_mut::<TRANSMISSION_TSP>()
        .0
        .iter_mut()
        .zip(prbs.as_chunks::<TRANSMISSION_TSP>().0)
    {
        for (b, &p) in slot[..TRANSMISSION_TSP - 1].iter_mut().zip(seq) {
            *b ^= p;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Independent formulation: with o_t the output, stage i holds o_{t−i},
    /// so o_t = o_{t−14} ⊕ o_{t−15}, with virtual past o_{−i} = stage i of
    /// "100101010000000".
    fn recurrence(n: usize) -> Vec<u8> {
        let init: Vec<u8> = INITIAL_STATE.bytes().map(|c| c - b'0').collect();
        // hist[k] = o_{k-15}
        let mut hist: Vec<u8> = (0..15).map(|k| init[14 - k]).collect();
        for t in 0..n {
            let v = hist[t + 15 - 14] ^ hist[t];
            hist.push(v);
        }
        hist[15..].to_vec()
    }

    /// §3.5 / Fig. 3-8: first output bits from the stated initial state.
    /// Hand derivation (SPEC-NOTES.md): o_0..o_15 = 0000 0011 1111 0110,
    /// i.e. the first two PRBS bytes are 0x03, 0xF6.
    #[test]
    fn spec_fig_3_8_first_bits() {
        let mut p = Prbs::new();
        assert_eq!(p.state_string(), INITIAL_STATE);
        let bits: Vec<u8> = (0..16).map(|_| p.next_bit()).collect();
        assert_eq!(bits, [0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 1, 1, 0, 1, 1, 0]);
        let mut p = Prbs::new();
        assert_eq!([p.next_byte(), p.next_byte()], [0x03, 0xF6]);
        let mut p = Prbs::new();
        let circuit: Vec<u8> = (0..5000).map(|_| p.next_bit()).collect();
        assert_eq!(circuit, recurrence(5000));
    }

    /// g(x) = X¹⁵+X¹⁴+1 is primitive: the register returns to the initial
    /// state after exactly 2¹⁵ − 1 clocks and not before.
    #[test]
    fn spec_fig_3_8_period() {
        let start = Prbs::new();
        let mut p = start;
        for i in 1..=PERIOD {
            p.next_bit();
            if p == start {
                assert_eq!(i, PERIOD);
                return;
            }
        }
        panic!("no return to the initial state");
    }

    /// Sync-byte offsets keep their value while the PRBS keeps advancing.
    #[test]
    fn spec_3_5_sync_skipped_register_advances() {
        let seq = frame_sequence(2);
        let mut f = vec![0u8; 2 * TRANSMISSION_TSP];
        f[203] = 0x47;
        f[407] = 0x47;
        disperse_frame(&mut f, &seq);
        assert_eq!(f[203], 0x47);
        assert_eq!(f[407], 0x47);
        // slot 1 byte 0 uses PRBS byte 204 (the register advanced 8 clocks
        // during the sync byte at offset 203).
        assert_eq!(f[204], seq[204]);
        assert_eq!(&f[..2], &[0x03, 0xF6]);
        disperse_frame(&mut f, &seq);
        assert!(
            f.iter()
                .enumerate()
                .all(|(i, &b)| b == if i == 203 || i == 407 { 0x47 } else { 0 })
        );
    }
}
