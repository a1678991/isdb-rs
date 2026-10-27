//! Frequency interleaving (ARIB STD-B31 §3.11.2, Figs. 3-25..3-27,
//! Table 3-13).
//!
//! Segment division: data segments 0..12 (data-segment order, see
//! [`crate::combine`]) form up to three groups — the partial-reception
//! segment (segment 0 if the partial-reception flag is set), the
//! differential segments (DQPSK layers) and the coherent segments.
//!
//! 1. Inter-segment interleaving (Fig. 3-26), per group of `n` segments
//!    except the partial-reception portion: with the group's symbols
//!    numbered `S_{k·nc+i}` (segment `k` of the group, carrier `i`), the
//!    output segment `k` carries `S_{i·n+k}` at position `i`.
//! 2. Carrier rotation (Fig. 3-27): position `i` of segment `k` takes
//!    `S'_{(i+k) mod nc}`.  Which segment number `k` is (the data-segment
//!    number 0..12, or the index within the group) is selectable via
//!    [`RotationNumbering`]; see SPEC-NOTES.
//! 3. Carrier randomising (Table 3-13): the symbol at rotated position `i`
//!    is assigned to carrier `T[i]` ("which carriers are assigned … to
//!    carrier-rotated data arranged in ascending order"; confirmed by
//!    Fig. 3-28 (b), see tests).
//!
//! The output is again in data-segment order, position `i` now being the
//! `i`-th data carrier of the OFDM segment (Figs. 3-29/3-30 `S_{i,j}`).

use crate::tables::{CARRIER_RANDOMIZE_M1, CARRIER_RANDOMIZE_M2, CARRIER_RANDOMIZE_M3};
use isdbt_core::{Mode, Params};

/// Carrier-randomising table for a mode (Table 3-13 a/b/c): `T[before] = after`.
pub fn randomize_table(mode: Mode) -> &'static [u16] {
    match mode {
        Mode::M1 => &CARRIER_RANDOMIZE_M1,
        Mode::M2 => &CARRIER_RANDOMIZE_M2,
        Mode::M3 => &CARRIER_RANDOMIZE_M3,
    }
}

/// Segment number `k` used by the carrier rotation of Fig. 3-27.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum RotationNumbering {
    /// `k` = data-segment number 0..12 (default; "carrier rotation by
    /// segment number").
    #[default]
    DataSegmentNumber,
    /// `k` = index of the segment within its group (0..n−1), the numbering
    /// used by Fig. 3-26.
    GroupRelative,
}

/// The groups of data segments (first segment, count) in order:
/// partial reception, differential, coherent (empty groups omitted), and
/// whether each is inter-segment interleaved.
pub fn segment_groups(params: &Params) -> Vec<(usize, usize, bool)> {
    let mut groups = Vec::new();
    let mut start = 0;
    if params.partial_reception() {
        groups.push((0, 1, false));
        start = 1;
    }
    let nd = (start..13)
        .take_while(|&s| params.segment_is_differential(s))
        .count();
    if nd > 0 {
        groups.push((start, nd, true));
    }
    if start + nd < 13 {
        groups.push((start + nd, 13 - start - nd, true));
    }
    groups
}

/// Frequency interleaver: a fixed permutation of the `13·nc` data
/// positions of an OFDM symbol.
#[derive(Debug, Clone)]
pub struct FrequencyInterleaver {
    /// `out[perm[q]] = in[q]`.
    perm: Vec<u32>,
}

impl FrequencyInterleaver {
    /// Build for `params` with the default rotation numbering.
    pub fn new(params: &Params) -> Self {
        Self::with_numbering(params, RotationNumbering::default())
    }

    /// Build for `params` with an explicit rotation numbering.
    pub fn with_numbering(params: &Params, numbering: RotationNumbering) -> Self {
        let nc = params.mode().data_carriers_per_segment();
        let table = randomize_table(params.mode());
        let mut perm = vec![u32::MAX; 13 * nc];
        for (g0, n, inter) in segment_groups(params) {
            for k_in in 0..n {
                for i_in in 0..nc {
                    let q = (g0 + k_in) * nc + i_in;
                    // Inter-segment interleaving (Fig. 3-26).
                    let (k, i) = if inter {
                        let f = k_in * nc + i_in;
                        (f % n, f / n)
                    } else {
                        (k_in, i_in)
                    };
                    // Carrier rotation (Fig. 3-27): out[j] = in[(j + kr) mod nc].
                    let kr = match numbering {
                        RotationNumbering::DataSegmentNumber => g0 + k,
                        RotationNumbering::GroupRelative => k,
                    };
                    let r = (i + nc - kr % nc) % nc;
                    // Carrier randomising (Table 3-13).
                    let t = table[r] as usize;
                    perm[q] = crate::frame::carrier_index((g0 + k) * nc + t);
                }
            }
        }
        debug_assert!(perm.iter().all(|&p| p != u32::MAX));
        Self { perm }
    }

    /// The permutation (`out[perm[q]] = in[q]`).
    pub fn permutation(&self) -> &[u32] {
        &self.perm
    }

    /// Interleave one OFDM symbol (data-segment order in and out).
    pub fn interleave<T: Copy>(&self, input: &[T], out: &mut [T]) {
        assert_eq!(input.len(), self.perm.len());
        assert_eq!(out.len(), self.perm.len());
        for (q, &p) in self.perm.iter().enumerate() {
            out[p as usize] = input[q];
        }
    }

    /// Inverse of [`Self::interleave`].
    pub fn deinterleave<T: Copy>(&self, input: &[T], out: &mut [T]) {
        assert_eq!(input.len(), self.perm.len());
        assert_eq!(out.len(), self.perm.len());
        for (q, &p) in self.perm.iter().enumerate() {
            out[q] = input[p as usize];
        }
    }
}
