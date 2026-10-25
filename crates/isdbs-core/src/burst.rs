//! The phase-reference burst (BO.1408-1 §7; MIC Ordinance No. 87 of 2011,
//! annexed table 51).
//!
//! Each frame has 192 bursts of 4 BPSK symbols. Their data is the
//! X⁹ + X⁴ + 1 PRBS from `111101101`, restarted at every frame and clocked
//! during the burst symbols only, "added" to the burst signal (table 51:
//! "電力拡散信号の発生及び加算処理は位相基準バースト信号期間のみ行う"). The
//! burst signal it is added to is not stated in any local text; it is all
//! zeros: every burst symbol of the lab's recordings equals BPSK of the
//! PRBS alone (SPEC-NOTES "burst-data").

use crate::frame::{BURST_SYMBOLS, BURSTS};
use isdb_fec::Prbs9;

/// Burst bits per frame.
pub const BITS_PER_FRAME: usize = BURSTS * BURST_SYMBOLS;

/// The 768 burst bits of a frame, in transmission order (burst 0 symbol 0
/// first); BPSK maps 0 to the +1 point (table 46: "０ … ０度").
pub fn frame_bits() -> [u8; BITS_PER_FRAME] {
    let mut p = Prbs9::new();
    std::array::from_fn(|_| p.next_bit())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 768 bits per frame; the first ones follow from the initial state.
    #[test]
    fn spec_table_51_burst_bits() {
        let b = frame_bits();
        assert_eq!(b.len(), 768);
        assert_eq!(&b[..6], &[0, 1, 0, 0, 0, 0]);
        // 768 > 511: the sequence wraps within a frame.
        assert_eq!(&b[511..517], &b[..6]);
    }
}
