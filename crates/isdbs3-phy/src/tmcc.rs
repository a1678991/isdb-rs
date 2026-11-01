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
#[non_exhaustive]
pub enum Error {
    /// LDPC did not converge.
    #[error("TMCC LDPC: {0}")]
    Ldpc(#[from] isdb_fec::ldpc::NotConverged),
    /// BCH found too many errors.
    #[error("TMCC BCH uncorrectable")]
    Bch,
}

/// The shortened rate-61/120 LDPC word of a frame's TMCC (§3.5, Fig.
/// 3.5-1) from its 31 680 received values `llr` (the 9 614 data bits,
/// then the 22 066 parity bits): n = 44 880 values, the 1 870 leading and
/// 11 330 trailing known zeros of the information part set to `known`
/// (a strong bit-0 LLR: 10⁴ for the float decoder, +127 for the `i8`
/// one).
///
/// # Panics
///
/// If `llr` does not hold [`isdbs3_core::params::TMCC_SIGNAL_BITS`] values.
pub fn ldpc_word<T: Copy>(llr: &[T], known: T) -> Vec<T> {
    assert_eq!(llr.len(), isdbs3_core::params::TMCC_SIGNAL_BITS);
    let mut w = vec![known; isdbs3_core::params::N_LDPC];
    w[NULL_HEAD..NULL_HEAD + DATA_BITS].copy_from_slice(&llr[..DATA_BITS]);
    let p0 = NULL_HEAD + DATA_BITS + NULL_TAIL;
    w[p0..].copy_from_slice(&llr[DATA_BITS..]);
    w
}

/// Decode 31 680 TMCC LLRs.
///
/// # Panics
///
/// If `llr` does not hold [`isdbs3_core::params::TMCC_SIGNAL_BITS`] values.
pub fn decode(llr: &[f32], max_iter: usize) -> Result<Decoded, Error> {
    let code = isdbs3_core::ldpc::code(CodeRate::TMCC);
    // known zeros as strong LLRs
    let mut w = ldpc_word(llr, 1.0e4);
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

    /// The shortened 61/120 TMCC word (§3.5) through the fixed-point
    /// decoder's two schedules (`isdb_fec::ldpc::fixed::Schedule`), as the
    /// FPGA will decode it: the 31 680 received values quantised, the
    /// 13 200 known zeros at +127 (`ldpc_word`). Both recover the encoded
    /// word from noisy values, decode the same in 16 lanes as in one, and
    /// a word with too much noise fails the same way in both lane widths.
    #[test]
    fn shortened_tmcc_code_decodes_with_both_schedules() {
        use isdb_fec::ldpc::fixed::{Decoder, Params, Schedule};
        let mut s = 0x9E37_79B9_7F4A_7C15u64;
        let mut next = move || {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            s
        };
        let info: Vec<u8> = (0..TMCC_INFO_BITS).map(|_| (next() & 1) as u8).collect();
        let sent = isdbs3_core::tmcc::encode(&TmccInfo::parse(&info));
        let code = isdbs3_core::ldpc::code(CodeRate::TMCC);
        let want = ldpc_word(&sent, 0u8);
        assert_eq!(
            code.syndrome_weight(&want),
            0,
            "the shortened word is a codeword"
        );
        // received values: ±amp plus uniform noise of ±spread, i8
        let mut noisy = |amp: i16, spread: i16| -> Vec<i8> {
            let rx: Vec<i8> = sent
                .iter()
                .map(|&b| {
                    let n = i16::try_from(next() % (2 * spread as u64 + 1)).unwrap() - spread;
                    let x = if b == 0 { amp } else { -amp } + n;
                    i8::try_from(x.clamp(-127, 127)).unwrap()
                })
                .collect();
            ldpc_word(&rx, 127)
        };
        let ws = [noisy(8, 14), noisy(10, 14), noisy(16, 20), noisy(1, 40)];
        let refs: Vec<&[i8]> = ws.iter().map(Vec::as_slice).collect();
        for schedule in [
            Schedule::Sequential,
            Schedule::Layered { step: 1 },
            Schedule::Layered { step: 68 },
            Schedule::Layered { step: 187 },
        ] {
            let p = Params {
                max_iter: 30,
                offset: 1,
                schedule,
            };
            let wide = Decoder::<16>::default().decode_all(code, &refs, p);
            let one = Decoder::<1>::default().decode_all(code, &refs, p);
            assert_eq!(wide, one, "{schedule:?}");
            for (i, (r, bits)) in wide.iter().take(3).enumerate() {
                let d = r.unwrap_or_else(|e| panic!("{schedule:?} word {i}: {e}"));
                assert!(d.iterations > 0, "{schedule:?} word {i}");
                assert_eq!(bits, &want, "{schedule:?} word {i}");
            }
            assert!(wide[3].0.is_err(), "{schedule:?}: {:?}", wide[3].0);
        }
    }
}
