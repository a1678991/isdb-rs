//! The K = 7 convolutional mother code with generators 171₈ / 133₈ and its
//! soft-decision Viterbi decoder, the inner code of every ISDB system:
//! ISDB-T (ARIB STD-B31 v2.2 §3.8, Fig. 3-10) and ISDB-S (ITU-R BO.1408-1
//! Annex 1 §9, Fig. 13; MIC Ordinance No. 87 of 2011, annexed table 49
//! item 3). Puncturing differs between the systems and lives with them
//! (`isdbt-fec::conv`, `isdbs-core`).
//!
//! Fig. 3-10 / Fig. 13 reading: six D stages; output X sums the data input
//! and the outputs of stages 1, 2, 3 and 6; output Y sums the input and
//! stages 2, 3, 5 and 6. With the 7-bit window (u_t, u_{t−1}, …, u_{t−6})
//! written MSB first, these are the octal masks 171 = 1111001₂ and 133 =
//! 1011011₂, the MSB of each mask being the undelayed input.
//!
//! Soft input: f32 LLRs ln P(b=0)/P(b=1) (positive ⇒ 0); punctured
//! positions are given LLR 0.

/// Constraint length k = 7.
pub const CONSTRAINT_LENGTH: usize = 7;
/// G1 = 171 octal, output X.
pub const G1: u8 = 0o171;
/// G2 = 133 octal, output Y.
pub const G2: u8 = 0o133;
/// Encoder states 2^(k−1).
pub const STATES: usize = 1 << (CONSTRAINT_LENGTH - 1);

/// Mother-code encoder (Fig. 3-10 / Fig. 13). The register holds the six
/// previous input bits, u_{t−1} in bit 5 … u_{t−6} in bit 0; it starts at
/// zero and runs continuously.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ConvEncoder {
    state: u8,
}

#[inline]
fn parity(x: u8) -> u8 {
    (x.count_ones() & 1) as u8
}

/// (X, Y) for input `u` from register `state`.
#[inline]
pub fn branch_output(state: u8, u: u8) -> (u8, u8) {
    let w = (u << 6) | state;
    (parity(w & G1), parity(w & G2))
}

impl ConvEncoder {
    /// Zero register.
    pub fn new() -> Self {
        Self::default()
    }
    /// Current register contents (u_{t−1} in bit 5).
    pub fn state(&self) -> u8 {
        self.state
    }
    /// Encode one bit, returning the mother-code pair (X, Y).
    #[inline]
    pub fn push(&mut self, u: u8) -> (u8, u8) {
        let out = branch_output(self.state, u & 1);
        self.state = ((u & 1) << 5) | (self.state >> 1);
        out
    }
}

const HALF: usize = STATES / 2;

/// Branch sign tables for the vectorised add-compare-select. For new state
/// `ns = (u << 5) | (s >> 1)` the predecessors are `s = 2·(ns mod 32) + b`.
struct Tables {
    // [u][b][j]: sign (+1 for code bit 0, −1 for 1) of X and Y on the branch
    // from s = 2j + b with input u into ns = 32u + j.
    sx: [[[f32; HALF]; 2]; 2],
    sy: [[[f32; HALF]; 2]; 2],
}

fn tables() -> &'static Tables {
    use std::sync::OnceLock;
    static T: OnceLock<Tables> = OnceLock::new();
    T.get_or_init(|| {
        let mut t = Tables {
            sx: [[[0.0; HALF]; 2]; 2],
            sy: [[[0.0; HALF]; 2]; 2],
        };
        for u in 0..2u8 {
            for b in 0..2u8 {
                for j in 0..HALF {
                    let s = (2 * j) as u8 | b;
                    let (x, y) = branch_output(s, u);
                    t.sx[u as usize][b as usize][j] = if x == 0 { 1.0 } else { -1.0 };
                    t.sy[u as usize][b as usize][j] = if y == 0 { 1.0 } else { -1.0 };
                }
            }
        }
        t
    })
}

/// Default traceback depth (≈ 16 × constraint length is ample for the 7/8
/// punctured code).
pub const DEFAULT_TRACEBACK: usize = 112;

/// Streaming soft-decision Viterbi decoder for the k = 7 mother code,
/// maximising the correlation Σ ±LLR. Path metrics persist across
/// calls; decisions are released `traceback` steps late until
/// [`ViterbiDecoder::flush`] traces back from the best state and releases
/// the rest.
#[derive(Debug, Clone)]
pub struct ViterbiDecoder {
    pm: [f32; STATES],
    decisions: Vec<u64>,
    traceback: usize,
    block: usize,
}

impl ViterbiDecoder {
    /// Decoder starting in the all-zero encoder state with the given
    /// traceback depth (≥ 1).
    pub fn new(traceback: usize) -> Self {
        let mut pm = [-1.0e6f32; STATES];
        pm[0] = 0.0;
        let traceback = traceback.max(1);
        Self {
            pm,
            decisions: Vec::new(),
            traceback,
            block: 4096.max(traceback),
        }
    }

    /// Decoder that assumes an unknown start state (all metrics equal), for
    /// receivers joining mid-stream.
    pub fn new_unknown_state(traceback: usize) -> Self {
        let mut d = Self::new(traceback);
        d.pm = [0.0; STATES];
        d
    }

    /// Decoder starting in the known encoder state `state` (the register
    /// of [`ConvEncoder::state`]), e.g. after a sync word whose last six
    /// bits fixed it.
    pub fn with_state(traceback: usize, state: u8) -> Self {
        let mut d = Self::new(traceback);
        d.pm = [-1.0e6f32; STATES];
        d.pm[usize::from(state) % STATES] = 0.0;
        d
    }

    /// Advance over input bits known to the receiver (a following sync
    /// word), with no channel information: only the branches with the
    /// known input survive. Six known bits fix the end state, so a
    /// [`ViterbiDecoder::flush`] afterwards traces back from it.
    pub fn push_known(&mut self, bits: &[u8], out: &mut Vec<u8>) {
        for &b in bits {
            self.acs(0.0, 0.0);
            for (ns, p) in self.pm.iter_mut().enumerate() {
                if (ns >> 5) as u8 != (b & 1) {
                    *p = -1.0e9;
                }
            }
            self.renormalise();
            if self.decisions.len() >= self.block + self.traceback {
                let s = self.best_state();
                self.release(s, self.block, out);
            }
        }
    }

    /// Traceback depth.
    pub fn traceback(&self) -> usize {
        self.traceback
    }

    /// Steps buffered but not yet released.
    pub fn pending(&self) -> usize {
        self.decisions.len()
    }

    #[inline]
    fn acs(&mut self, lx: f32, ly: f32) {
        let t = tables();
        let mut ev = [0f32; HALF];
        let mut od = [0f32; HALF];
        for j in 0..HALF {
            ev[j] = self.pm[2 * j];
            od[j] = self.pm[2 * j + 1];
        }
        let mut new = [0f32; STATES];
        let mut dec: u64 = 0;
        for u in 0..2 {
            let (sx0, sy0) = (&t.sx[u][0], &t.sy[u][0]);
            let (sx1, sy1) = (&t.sx[u][1], &t.sy[u][1]);
            let mut m0 = [0f32; HALF];
            let mut m1 = [0f32; HALF];
            for j in 0..HALF {
                m0[j] = ev[j] + sx0[j] * lx + sy0[j] * ly;
                m1[j] = od[j] + sx1[j] * lx + sy1[j] * ly;
            }
            let mut d: u32 = 0;
            for j in 0..HALF {
                let c = m1[j] > m0[j];
                new[u * HALF + j] = if c { m1[j] } else { m0[j] };
                d |= (c as u32) << j;
            }
            dec |= (d as u64) << (u * HALF);
        }
        self.pm = new;
        self.decisions.push(dec);
    }

    /// One trellis step with an arbitrary metric per branch output:
    /// `m[2·x + y]` is added to every branch that emits (X, Y) = (x, y).
    /// [`ViterbiDecoder::acs`] is the special case m = ±lx ± ly.
    #[inline]
    fn acs4(&mut self, m: &[f32; 4]) {
        let t = tables();
        let mut new = [0f32; STATES];
        let mut dec: u64 = 0;
        for u in 0..2 {
            let mut d: u32 = 0;
            for j in 0..HALF {
                let idx = |b: usize| {
                    let x = usize::from(t.sx[u][b][j] < 0.0);
                    let y = usize::from(t.sy[u][b][j] < 0.0);
                    2 * x + y
                };
                let m0 = self.pm[2 * j] + m[idx(0)];
                let m1 = self.pm[2 * j + 1] + m[idx(1)];
                let c = m1 > m0;
                new[u * HALF + j] = if c { m1 } else { m0 };
                d |= (c as u32) << j;
            }
            dec |= (d as u64) << (u * HALF);
        }
        self.pm = new;
        self.decisions.push(dec);
    }

    /// Feed one metric quadruple per trellis step (`m[2·x + y]` for the
    /// branches emitting (X, Y) = (x, y), larger = more likely), for
    /// branch metrics that are not a sum of two bit LLRs: the set-partition
    /// metric of trellis-coded 8PSK, where (X, Y) select a pair of
    /// antipodal points and the uncoded bit picks one of them.
    pub fn push_metrics(&mut self, steps: &[[f32; 4]], out: &mut Vec<u8>) {
        for (i, m) in steps.iter().enumerate() {
            self.acs4(m);
            if i % 64 == 63 {
                self.renormalise();
            }
            if self.decisions.len() >= self.block + self.traceback {
                let s = self.best_state();
                self.release(s, self.block, out);
            }
        }
        self.renormalise();
    }

    fn renormalise(&mut self) {
        let m = self.pm.iter().copied().fold(f32::MIN, f32::max);
        for p in self.pm.iter_mut() {
            *p -= m;
        }
    }

    fn best_state(&self) -> usize {
        let mut best = 0;
        for s in 1..STATES {
            if self.pm[s] > self.pm[best] {
                best = s;
            }
        }
        best
    }

    /// Trace back over all buffered steps from `state` and append the first
    /// `release` decoded bits to `out`, dropping them from the buffer.
    fn release(&mut self, state: usize, release: usize, out: &mut Vec<u8>) {
        let n = self.decisions.len();
        let mut s = state;
        let mut bits = vec![0u8; release];
        for t in (0..n).rev() {
            let u = (s >> 5) as u8;
            if t < release {
                bits[t] = u;
            }
            let d = (self.decisions[t] >> s) & 1;
            s = ((s << 1) & (STATES - 1)) | d as usize;
        }
        out.extend_from_slice(&bits);
        self.decisions.drain(..release);
    }

    /// Feed mother-code LLR pairs `[Lx₀, Ly₀, Lx₁, Ly₁, …]` (depunctured,
    /// 0.0 at punctured positions); appends decided bits to `out`, lagging the
    /// input by at least `traceback` steps.
    pub fn push(&mut self, pairs: &[f32], out: &mut Vec<u8>) {
        assert_eq!(pairs.len() % 2, 0);
        for (i, p) in pairs.as_chunks::<2>().0.iter().enumerate() {
            self.acs(p[0], p[1]);
            if i % 64 == 63 {
                self.renormalise();
            }
            if self.decisions.len() >= self.block + self.traceback {
                let s = self.best_state();
                self.release(s, self.block, out);
            }
        }
        self.renormalise();
    }

    /// Release every buffered decision, tracing back from the currently best
    /// state (no future information is used for the last `traceback` bits).
    pub fn flush(&mut self, out: &mut Vec<u8>) {
        let n = self.decisions.len();
        if n > 0 {
            let s = self.best_state();
            self.release(s, n, out);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::XorShift;

    /// Fig. 3-10 / Fig. 13: impulse response of X and Y equals the
    /// generator taps 171₈ = 1111001₂ and 133₈ = 1011011₂.
    #[test]
    fn spec_impulse_response() {
        let mut e = ConvEncoder::new();
        let mut xs = Vec::new();
        let mut ys = Vec::new();
        for t in 0..CONSTRAINT_LENGTH + 3 {
            let (x, y) = e.push(u8::from(t == 0));
            xs.push(x);
            ys.push(y);
        }
        assert_eq!(xs, [1, 1, 1, 1, 0, 0, 1, 0, 0, 0]);
        assert_eq!(ys, [1, 0, 1, 1, 0, 1, 1, 0, 0, 0]);
        assert_eq!(e.state(), 0);
    }

    /// Rate-1/2 round trip, in odd chunk sizes.
    #[test]
    fn viterbi_rate_half_streaming() {
        let mut rng = XorShift::new(11);
        let bits: Vec<u8> = (0..20000).map(|_| (rng.next() & 1) as u8).collect();
        let mut e = ConvEncoder::new();
        let llr: Vec<f32> = bits
            .iter()
            .flat_map(|&u| {
                let (x, y) = e.push(u);
                [x, y].map(|b| if b == 0 { 8.0 } else { -8.0 })
            })
            .collect();
        let mut v = ViterbiDecoder::new(64);
        let mut out = Vec::new();
        for c in llr.chunks(2 * 777) {
            v.push(c, &mut out);
        }
        v.flush(&mut out);
        assert_eq!(out, bits);
    }

    /// `push_metrics` with m = ±lx ± ly decides exactly as `push`.
    #[test]
    fn viterbi_metrics_equal_llr_pairs() {
        let mut rng = XorShift::new(23);
        let llr: Vec<f32> = (0..4000)
            .map(|_| ((rng.next() % 2001) as f32 - 1000.0) / 100.0)
            .collect();
        let steps: Vec<[f32; 4]> = llr
            .as_chunks::<2>()
            .0
            .iter()
            .map(|&[x, y]| [x + y, x - y, -x + y, -x - y])
            .collect();
        let (mut a, mut b) = (Vec::new(), Vec::new());
        let mut v = ViterbiDecoder::new(64);
        v.push(&llr, &mut a);
        v.flush(&mut a);
        let mut w = ViterbiDecoder::new(64);
        w.push_metrics(&steps, &mut b);
        w.flush(&mut b);
        assert_eq!(a, b);
    }

    /// A known start state and known trailing bits: the decoder recovers a
    /// block coded from a non-zero register, the trailing bits included,
    /// even with the last coded pairs erased.
    #[test]
    fn viterbi_known_state_and_tail() {
        let mut rng = XorShift::new(5);
        let mut e = ConvEncoder::new();
        for _ in 0..13 {
            e.push((rng.next() & 1) as u8);
        }
        let start = e.state();
        let bits: Vec<u8> = (0..3000).map(|_| (rng.next() & 1) as u8).collect();
        let mut llr: Vec<f32> = bits
            .iter()
            .flat_map(|&u| {
                let (x, y) = e.push(u);
                [x, y].map(|b| if b == 0 { 4.0 } else { -4.0 })
            })
            .collect();
        // erase the last 4 pairs: only the known tail disambiguates them
        let n = llr.len();
        llr[n - 8..].fill(0.0);
        let tail = [0u8, 0, 0, 1, 1, 0];
        let mut v = ViterbiDecoder::with_state(64, start);
        let mut out = Vec::new();
        v.push(&llr, &mut out);
        v.push_known(&tail, &mut out);
        v.flush(&mut out);
        assert_eq!(&out[..bits.len() - 4], &bits[..bits.len() - 4]);
        assert_eq!(&out[bits.len()..], &tail);
    }
}
