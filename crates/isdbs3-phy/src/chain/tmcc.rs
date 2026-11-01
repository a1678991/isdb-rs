//! Stage 5: the TMCC soft bits and the frame's TMCC codeword (module docs
//! of [`crate::chain`], "Stage 5").

use super::fx::{round_shift, sat127};
use super::ldpc::LdpcDecoder;
use isdbs3_core::params::{CodeRate, TMCC_INFO_BITS, TMCC_SIGNAL_BITS};
use isdbs3_core::slots::{Slot, layout};
use isdbs3_core::tmcc::{DATA_BITS, NULL_HEAD, NULL_TAIL, TmccInfo};

/// Right shift from the Q.10 projection sum to the soft bit: a symbol of
/// unit amplitude on its point gives 1 448 >> 5 = 45.
pub const TMCC_SHIFT: u32 = 5;
/// The soft value of the shortened code's known zeros.
pub const KNOWN_ZERO: i8 = 127;

/// The soft bit of TMCC symbol `z` (Q.10) at slot position `j`
/// (π/2-BPSK, §3.8: bit 0 at +45° on the slot's even positions, which
/// are the frame's odd symbols, and at +135° on odd positions):
/// `sat127(round_shift(z_i + z_q, 5))` for even j, `sat127(round_shift(z_q
/// − z_i, 5))` for odd j; positive reads 0.
pub fn soft(z: [i16; 2], j: u16) -> i8 {
    let (i, q) = (i64::from(z[0]), i64::from(z[1]));
    let s = if j.is_multiple_of(2) { i + q } else { q - i };
    sat127(round_shift(s, TMCC_SHIFT))
}

/// Position in the shortened 44 880-bit word of TMCC signal bit `b` (0 …
/// 31 679, slot-major: 264 per modulation slot, Fig. 3.2-5): `b + 1 870`
/// for the 9 614 data bits, `b + 13 200` for the 22 066 parity bits.
pub fn word_position(b: usize) -> usize {
    if b < DATA_BITS {
        b + NULL_HEAD
    } else {
        b + NULL_HEAD + NULL_TAIL
    }
}

/// What the TMCC codeword of a frame gave.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TmccResult {
    /// LDPC iterations, `None` if it did not converge.
    pub ldpc_iterations: Option<usize>,
    /// BCH bits corrected, `None` if uncorrectable (or LDPC failed).
    pub bch_corrected: Option<usize>,
    /// The 9 422 information bits as decoded (after dispersal and BCH,
    /// whatever the BCH result).
    pub bits: Vec<u8>,
    /// The information, when BCH succeeded.
    pub info: Option<TmccInfo>,
    /// Its slot layout, when the allocation is consistent.
    pub layout: Option<Vec<Slot>>,
}

/// Decode a frame's 31 680 soft bits (TMCC order): the shortened word
/// (known zeros [`KNOWN_ZERO`]), the decoder `ldpc` on the rate-61/120
/// code, then dispersal, BCH (§3.4 (1), §3.6 (2)) and the layout
/// (§3.11 (2), Table 3.2-2).
///
/// # Panics
///
/// If `soft` is not 31 680 values long.
pub fn decode(soft: &[i8], ldpc: &mut dyn LdpcDecoder) -> TmccResult {
    assert_eq!(soft.len(), TMCC_SIGNAL_BITS);
    let w = crate::tmcc::ldpc_word(soft, KNOWN_ZERO);
    let code = isdbs3_core::ldpc::code(CodeRate::TMCC);
    let (res, bits) = ldpc.decode(code, &[&w]).pop().expect("one result per word");
    let ldpc_iterations = res.ok().map(|d| d.iterations);
    let (data, bch) = match crate::tmcc::finish(&bits) {
        Ok((d, c)) => (d, Some(c)),
        Err(_) => {
            let mut d = bits[NULL_HEAD..NULL_HEAD + DATA_BITS].to_vec();
            isdbs3_core::tmcc::disperse(&mut d);
            (d, None)
        }
    };
    let bch_corrected = ldpc_iterations.and(bch);
    let info = bch_corrected.map(|_| TmccInfo::parse(&data[..TMCC_INFO_BITS]));
    let layout = info.as_ref().and_then(layout);
    TmccResult {
        ldpc_iterations,
        bch_corrected,
        bits: data[..TMCC_INFO_BITS].to_vec(),
        info,
        layout,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn soft_bits_read_the_pi2_bpsk_points() {
        // bit 0 at +45° (even j) and +135° (odd j), unit amplitude
        assert_eq!(soft([724, 724], 0), 45);
        assert_eq!(soft([-724, -724], 0), -45);
        assert_eq!(soft([-724, 724], 1), 45);
        assert_eq!(soft([724, -724], 1), -45);
        assert_eq!(soft([i16::MAX, i16::MAX], 0), 127);
    }

    #[test]
    fn word_positions_skip_the_known_zeros() {
        assert_eq!(word_position(0), 1_870);
        assert_eq!(word_position(9_613), 11_483);
        assert_eq!(word_position(9_614), 22_814);
        assert_eq!(word_position(31_679), 44_879);
    }
}
