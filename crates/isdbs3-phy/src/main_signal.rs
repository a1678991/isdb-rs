//! The main signal of a frame: per valid slot, the codeword's symbols
//! (`isdbs3_core::slots`), max-log soft demapping against the constellation
//! (the nominal one of B44 §3.8, or the points learned from the pilots),
//! bit de-interleaving (§3.7), LDPC (§3.4 (2)), BCH (§3.4 (1)), energy
//! dispersal removal (§3.6 (1)), and the slot's header and data bits.
//!
//! Pilot (§3.9): each modulation slot's 32 pilot symbols are the
//! modulation's points in label order (16APSK: 0000 … 1111 twice), each
//! turned 0° / 180° by the pilot PRBS (§3.6 (3)). Averaging them over a
//! frame gives the transmitted points as received, compression of the
//! transponder included: B44 §2 introduces the pilot "for restrain of
//! deterioration" from the TWTA.

use crate::Cplx;
use crate::sync::{SlotHead, phase_at};
use isdbs3_core::constellation::points;
use isdbs3_core::dispersal::{SlotDispersal, pilot_signs};
use isdbs3_core::interleave::permutation;
use isdbs3_core::params::{
    BCH_PARITY_BITS, HEADER_BITS, MAIN_RUN, MOD_SLOT, Modulation, N_LDPC, PILOT_SYMBOLS, RUNS,
    SLOTS, STUFF_BITS, SYNC_SYMBOLS, TMCC_RUN,
};
use isdbs3_core::slots::Slot;

/// Offsets within a modulation slot of its 8 976 main symbols, in order.
pub fn main_offsets() -> impl Iterator<Item = usize> {
    (0..RUNS).flat_map(|r| {
        let base = SYNC_SYMBOLS + PILOT_SYMBOLS + r * (MAIN_RUN + TMCC_RUN);
        base..base + MAIN_RUN
    })
}

/// The frame's main symbols (1 077 120), derotated by the slot phase and
/// scaled by the sync amplitude.
pub fn main_symbols(z: &[Cplx], slots: &[SlotHead], f: usize) -> Option<Vec<Cplx>> {
    if f + SLOTS > slots.len() {
        return None;
    }
    let mut out = Vec::with_capacity(SLOTS * RUNS * MAIN_RUN);
    for i in f..f + SLOTS {
        let at = slots[i].at;
        if at + MOD_SLOT > z.len() {
            return None;
        }
        for j in main_offsets() {
            let (ph, amp) = phase_at(slots, i, j);
            out.push(z[at + j] / ph / amp.max(1e-30));
        }
    }
    Some(out)
}

/// The frame's pilot symbols, derotated, scaled and with the pilot PRBS
/// sign removed: 32 per modulation slot.
pub fn pilots(z: &[Cplx], slots: &[SlotHead], f: usize) -> Option<Vec<Cplx>> {
    if f + SLOTS > slots.len() {
        return None;
    }
    let signs = pilot_signs();
    let mut out = Vec::with_capacity(SLOTS * PILOT_SYMBOLS);
    for (k, i) in (f..f + SLOTS).enumerate() {
        let at = slots[i].at;
        if at + MOD_SLOT > z.len() {
            return None;
        }
        for j in SYNC_SYMBOLS..SYNC_SYMBOLS + PILOT_SYMBOLS {
            let (ph, amp) = phase_at(slots, i, j);
            let s = f64::from(signs[k * PILOT_SYMBOLS + j - SYNC_SYMBOLS]);
            out.push(z[at + j] / ph / amp.max(1e-30) * s);
        }
    }
    Some(out)
}

/// The constellation of `m` as received: the mean of the pilot symbols of
/// every modulation slot of that modulation, per label (pilot symbol t
/// carries label t mod 2^bits, §3.9). `None` if no slot carries `m`.
pub fn learned_points(pil: &[Cplx], layout: &[Slot], m: Modulation) -> Option<Vec<Cplx>> {
    let n = 1usize << m.bits();
    let mut sum = vec![Cplx::new(0.0, 0.0); n];
    let mut cnt = vec![0usize; n];
    for (k, s) in layout.iter().enumerate() {
        if s.modulation != m {
            continue;
        }
        for t in 0..PILOT_SYMBOLS {
            let lab = t % n;
            sum[lab] += pil[k * PILOT_SYMBOLS + t];
            cnt[lab] += 1;
        }
    }
    (cnt.iter().all(|&c| c > 0)).then(|| sum.iter().zip(&cnt).map(|(s, &c)| s / c as f64).collect())
}

/// The nominal constellation of `m` at rate `r`, at the sync symbols'
/// amplitude (unit power: the sync symbols have the main signal's mean
/// power, §3.8).
pub fn nominal_points(m: Modulation, r: isdbs3_core::CodeRate) -> Vec<Cplx> {
    points(m, r)
        .into_iter()
        .map(|(i, q)| Cplx::new(i, q))
        .collect()
}

/// Max-log LLRs of `sym` against `pts` (label-indexed), `bits` per symbol
/// MSB first, noise variance per complex dimension `n0` (E|n|²).
pub fn demap(sym: &[Cplx], pts: &[Cplx], bits: usize, n0: f64, out: &mut Vec<f32>) {
    out.clear();
    out.reserve(sym.len() * bits);
    let mut d = vec![0f64; pts.len()];
    for &y in sym {
        for (di, p) in d.iter_mut().zip(pts) {
            *di = (y - p).norm_sqr();
        }
        for b in (0..bits).rev() {
            let (mut m0, mut m1) = (f64::MAX, f64::MAX);
            for (lab, &di) in d.iter().enumerate() {
                if (lab >> b) & 1 == 0 {
                    m0 = m0.min(di);
                } else {
                    m1 = m1.min(di);
                }
            }
            out.push(((m1 - m0) / n0) as f32);
        }
    }
}

/// One decoded slot.
#[derive(Debug, Clone)]
pub struct SlotData {
    /// Slot index 0 … 119.
    pub slot: usize,
    /// LDPC iterations, or `None` if it did not converge.
    pub ldpc_iterations: Option<usize>,
    /// BCH bits corrected, or `None` if uncorrectable (or LDPC failed).
    pub bch_corrected: Option<usize>,
    /// The 176 header bits and the data bits after dispersal removal
    /// (meaningful only when BCH succeeded).
    pub header: Vec<u8>,
    /// The main data, packed MSB first.
    pub data: Vec<u8>,
}

/// Decode every valid slot of a frame from its main symbols `ms` (unit
/// sync amplitude), with the constellation `pts_for(modulation, rate)`.
/// The slot dispersal runs across the frame's valid slots in order, so
/// every slot's dispersed bits are produced even when its decode fails.
pub fn decode_frame(
    ms: &[Cplx],
    layout: &[Slot],
    n0: f64,
    max_iter: usize,
    pts_for: &(dyn Fn(Modulation, isdbs3_core::CodeRate) -> Vec<Cplx> + Sync),
) -> Vec<SlotData> {
    // decode the codewords (independent) …
    let valid: Vec<&Slot> = layout.iter().filter(|s| !s.dummy).collect();
    let decoded: Vec<(usize, Option<usize>, Vec<u8>)> = std::thread::scope(|sc| {
        let workers = std::thread::available_parallelism()
            .map_or(4, |n| n.get())
            .min(16);
        let chunk = valid.len().div_ceil(workers).max(1);
        let handles: Vec<_> = valid
            .chunks(chunk)
            .map(|grp| {
                sc.spawn(move || {
                    grp.iter()
                        .map(|s| decode_codeword(ms, s, n0, max_iter, pts_for))
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        handles
            .into_iter()
            .flat_map(|h| h.join().expect("decoder thread"))
            .collect()
    });
    // … then undo the dispersal in slot order (its PRBS runs across slots)
    let mut disp = SlotDispersal::new();
    decoded
        .into_iter()
        .map(|(slot, it, bits)| {
            let s = layout[slot];
            let k = s.rate.k_ldpc();
            let mut msg = bits[..k].to_vec();
            // Fig. 3.2-1: BCH (step 2) before dispersal (step 3), which
            // covers header, data, BCH parity and stuffing; the BCH word
            // is header + data + parity, the stuffing outside it.
            let spread = HEADER_BITS + s.rate.data_bits() + BCH_PARITY_BITS + STUFF_BITS;
            debug_assert_eq!(spread, k);
            disp.apply(&mut msg[..spread]);
            let bch = it.and_then(|_| isdb_fec::bch::decode(&mut msg[..k - STUFF_BITS]).ok());
            let header = msg[..HEADER_BITS].to_vec();
            let data = msg[HEADER_BITS..HEADER_BITS + s.rate.data_bits()]
                .chunks(8)
                .map(|c| c.iter().fold(0u8, |a, &b| (a << 1) | b))
                .collect();
            SlotData {
                slot,
                ldpc_iterations: it,
                bch_corrected: bch,
                header,
                data,
            }
        })
        .collect()
}

/// Demap, de-interleave and LDPC-decode one slot's codeword; returns the
/// slot index, the iterations (None if not converged) and the hard bits.
fn decode_codeword(
    ms: &[Cplx],
    s: &Slot,
    n0: f64,
    max_iter: usize,
    pts_for: &(dyn Fn(Modulation, isdbs3_core::CodeRate) -> Vec<Cplx> + Sync),
) -> (usize, Option<usize>, Vec<u8>) {
    let sym = &ms[s.symbol_start..s.symbol_start + s.symbols];
    let pts = pts_for(s.modulation, s.rate);
    let mut lab = Vec::new();
    demap(sym, &pts, s.modulation.bits(), n0, &mut lab);
    let perm = permutation(s.modulation, s.rate);
    let mut llr = vec![0f32; N_LDPC];
    for (i, &p) in perm.iter().enumerate() {
        llr[p as usize] = lab[i];
    }
    let code = isdbs3_core::ldpc::code(s.rate);
    let mut bits = Vec::new();
    match code.decode(&mut llr, &mut bits, max_iter, 0.8) {
        Ok(d) => (s.index, Some(d.iterations), bits),
        Err(_) => (s.index, None, bits),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spec_8976_main_symbols_per_slot() {
        let o: Vec<usize> = main_offsets().collect();
        assert_eq!(o.len(), 8_976);
        assert_eq!(o[0], 56);
        assert_eq!(o[136], 56 + 140);
    }

    #[test]
    fn demap_is_exact_on_the_points() {
        let r = isdbs3_core::CodeRate::new(93).unwrap();
        let pts = nominal_points(Modulation::Apsk16, r);
        let mut out = Vec::new();
        demap(&pts, &pts, 4, 0.01, &mut out);
        for (lab, l) in out.chunks(4).enumerate() {
            for (b, v) in l.iter().enumerate() {
                let bit = (lab >> (3 - b)) & 1;
                assert_eq!(*v < 0.0, bit == 1, "label {lab:04b} bit {b}");
            }
        }
    }
}
