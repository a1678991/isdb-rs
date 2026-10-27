//! TMCC of a frame: the 264 TMCC symbols of each of the 120 modulation
//! slots (4 after every 136 main symbols, Fig. 3.3-1), π/2-shift BPSK, in
//! slot order = the 31 680 TMCC signal bits (9 614 dispersed data bits,
//! then 22 066 LDPC parity bits; Fig. 3.2-5); decoded with the shortened
//! rate-61/120 LDPC code (§3.5), the energy dispersal removed (§3.6 (2))
//! and the BCH code checked (§3.4 (1)).

use crate::sync::{SlotHead, phase_at};
use crate::{Cplx, soft};
use isdbs3_core::params::{
    CodeRate, MAIN_RUN, MOD_SLOT, PILOT_SYMBOLS, RUNS, SLOTS, SYNC_SYMBOLS, TMCC_INFO_BITS,
    TMCC_RUN,
};
use isdbs3_core::tmcc::{DATA_BITS, NULL_HEAD, NULL_TAIL, TmccInfo, disperse};

/// Offsets within a modulation slot of its 264 TMCC symbols, in order.
pub fn tmcc_offsets() -> impl Iterator<Item = usize> {
    (0..RUNS).flat_map(|r| {
        let base = SYNC_SYMBOLS + PILOT_SYMBOLS + r * (MAIN_RUN + TMCC_RUN) + MAIN_RUN;
        base..base + TMCC_RUN
    })
}

/// Derotated π/2-BPSK value of symbol `j` of a slot: the bit-0 point of an
/// odd symbol is at +45°, of an even one at +135° (§3.8); slot symbol 0 is
/// odd (9 296 is even, every slot starts on an odd frame symbol). Returns
/// the projection on the bit-0 direction (positive = bit 0).
pub fn bpsk_soft(v: Cplx, j: usize) -> f64 {
    let ref0 = if j.is_multiple_of(2) {
        Cplx::from_polar(1.0, std::f64::consts::FRAC_PI_4)
    } else {
        Cplx::from_polar(1.0, 3.0 * std::f64::consts::FRAC_PI_4)
    };
    (v * ref0.conj()).re
}

/// The TMCC LLRs of the frame whose slot #1 is `slots[f]`: 31 680 values
/// (ln P(0)/P(1)), from the symbols `z`; `noise_var` is the per-dimension
/// noise variance relative to unit signal amplitude.
pub fn llrs(z: &[Cplx], slots: &[SlotHead], f: usize, noise_var: f64) -> Option<Vec<f32>> {
    if f + SLOTS > slots.len() {
        return None;
    }
    let mut out = Vec::with_capacity(SLOTS * RUNS * TMCC_RUN);
    for i in f..f + SLOTS {
        let at = slots[i].at;
        if at + MOD_SLOT > z.len() {
            return None;
        }
        for j in tmcc_offsets() {
            let (ph, amp) = phase_at(slots, i, j);
            let v = z[at + j] / ph / amp.max(1e-30);
            out.push(soft(2.0 * bpsk_soft(v, j) / noise_var));
        }
    }
    Some(out)
}

/// What decoding a frame's TMCC gave.
#[derive(Debug, Clone)]
pub struct Decoded {
    /// The information.
    pub info: TmccInfo,
    /// LDPC iterations.
    pub ldpc_iterations: usize,
    /// Bits the BCH decoder corrected.
    pub bch_corrected: usize,
}

/// Why a frame's TMCC did not decode.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    /// LDPC did not converge.
    #[error("TMCC LDPC: {0}")]
    Ldpc(#[from] isdb_fec::ldpc::NotConverged),
    /// BCH found too many errors.
    #[error("TMCC BCH uncorrectable")]
    Bch,
}

/// Decode 31 680 TMCC LLRs.
pub fn decode(llr: &[f32], max_iter: usize) -> Result<Decoded, Error> {
    assert_eq!(llr.len(), isdbs3_core::params::TMCC_SIGNAL_BITS);
    let code = isdbs3_core::ldpc::code(CodeRate::new(61).expect("61/120"));
    // rebuild the shortened word: known zeros as strong LLRs
    const KNOWN: f32 = 1.0e4;
    let mut w = vec![KNOWN; code.n()];
    w[NULL_HEAD..NULL_HEAD + DATA_BITS].copy_from_slice(&llr[..DATA_BITS]);
    let p0 = NULL_HEAD + DATA_BITS + NULL_TAIL;
    w[p0..].copy_from_slice(&llr[DATA_BITS..]);
    let mut bits = Vec::new();
    let d = code.decode(&mut w, &mut bits, max_iter, 0.8)?;
    let mut data = bits[NULL_HEAD..NULL_HEAD + DATA_BITS].to_vec();
    disperse(&mut data);
    let corrected = isdb_fec::bch::decode(&mut data).map_err(|_| Error::Bch)?;
    Ok(Decoded {
        info: TmccInfo::parse(&data[..TMCC_INFO_BITS]),
        ldpc_iterations: d.iterations,
        bch_corrected: corrected,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spec_264_tmcc_symbols_per_slot_after_each_136() {
        let o: Vec<usize> = tmcc_offsets().collect();
        assert_eq!(o.len(), 264);
        assert_eq!(&o[..5], &[192, 193, 194, 195, 332]);
        assert_eq!(*o.last().unwrap(), MOD_SLOT - 1);
    }
}
