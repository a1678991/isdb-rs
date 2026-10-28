//! Fixed-point layered min-sum for [`Code`], decoding up to `W` codewords
//! of one code at once, one per lane.
//!
//! The arithmetic is defined per lane and is the same whatever the lane
//! count and whichever [`crate::simd`] back end runs it, so a codeword
//! decodes to the same bits, in the same number of iterations, alone
//! (`W` = 1, the scalar reference) or next to others in any lane. That is
//! the bit-exact agreement between the `CpuScalar` and `CpuSimd`
//! implementations that the project requires of a stage (plan §7.2).
//!
//! Per lane:
//!
//! - **Input**: an `i8` LLR per code bit (ln P(b=0)/P(b=1) times a scale
//!   the caller chooses; [`quantise`] does it), saturating at ±127.
//! - **Messages**: an `i8` check-to-variable message per edge, zero at the
//!   start; a-posteriori LLRs `L` in `i8`, saturating.
//! - **Check c** (checks in order 0 … P−1, edges in the order of
//!   [`Code`]'s adjacency): `t_e = sat(L_v − r_e)` for each edge e to
//!   variable v; the sign parity and the two smallest `|t_e|` (min1 ≤
//!   min2, first edge index of min1 on ties); then the new message
//!   `r_e = ±max(m − offset, 0)` with m = min2 on the min1 edge and min1
//!   elsewhere, its sign the parity times the sign of `t_e`; and
//!   `L_v = sat(t_e + r_e)`. `|t| ` is computed saturating, so −128
//!   reads 127.
//! - **Stopping**: after each iteration the hard decisions (`L < 0` is a
//!   one) are checked against every parity check; a lane whose word is a
//!   codeword stops there, its bits and iteration count recorded. A lane
//!   already a codeword at the input stops at iteration 0. Other lanes go
//!   on to `max_iter`.
//!
//! Lanes past the number of codewords given carry zeros and are ignored.

use super::{Code, Decoded, NotConverged};
use crate::simd::{self, Lane};

/// The decoder's settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Params {
    /// Iteration limit.
    pub max_iter: usize,
    /// Offset subtracted from every check-to-variable magnitude (offset
    /// min-sum), in LLR units of the quantised input.
    pub offset: i8,
}

/// Quantise float LLRs: `round(llr · scale)` saturated to ±127.
pub fn quantise(llr: &[f32], scale: f32, out: &mut Vec<i8>) {
    out.clear();
    // rounded and clamped to ±127, so the cast is exact
    #[allow(clippy::cast_possible_truncation)]
    let q = |l: f32| (l * scale).round().clamp(-127.0, 127.0) as i8;
    out.extend(llr.iter().map(|&l| q(l)));
}

/// The result for one lane.
pub type LaneResult = Result<Decoded, NotConverged>;

/// Work buffers for one code and lane width, reused across calls.
#[derive(Debug, Clone)]
pub struct Decoder<const W: usize> {
    l: Vec<Lane<W>>,
    r: Vec<Lane<W>>,
    t: Vec<Lane<W>>,
}

impl<const W: usize> Default for Decoder<W> {
    fn default() -> Self {
        Self {
            l: Vec::new(),
            r: Vec::new(),
            t: Vec::new(),
        }
    }
}

impl<const W: usize> Decoder<W> {
    /// Decode every codeword of `words` (quantised input LLRs, length n
    /// each, all of `code`), `W` at a time: a lane that stops takes the
    /// next codeword at once, so one slow or failing codeword does not
    /// hold the others. Returns each codeword's result and hard decisions
    /// (0/1, length n), in input order, the same as decoding it alone.
    ///
    /// # Panics
    ///
    /// If a word's length is not the code's n.
    pub fn decode_all(
        &mut self,
        code: &Code,
        words: &[&[i8]],
        p: Params,
    ) -> Vec<(LaneResult, Vec<u8>)> {
        let n = code.n;
        for w in words {
            assert_eq!(w.len(), n, "codeword length");
        }
        let mut out: Vec<Option<(LaneResult, Vec<u8>)>> = vec![None; words.len()];
        self.l.clear();
        self.l.resize(n, Lane::splat(0));
        self.r.clear();
        self.r.resize(code.rows.len(), Lane::splat(0));
        // per lane: the codeword it holds and its iterations so far
        let mut held: [Option<usize>; W] = [None; W];
        let mut iters = [0usize; W];
        let mut next = 0usize;
        loop {
            // fill the empty lanes; a word already a codeword (or with
            // max_iter 0) stops at iteration 0 and the lane refills
            loop {
                let mut loaded = false;
                for i in 0..W {
                    if held[i].is_none() && next < words.len() {
                        self.load(i, words[next]);
                        held[i] = Some(next);
                        iters[i] = 0;
                        next += 1;
                        loaded = true;
                    }
                }
                if !loaded {
                    break;
                }
                let fresh: [bool; W] = std::array::from_fn(|i| held[i].is_some() && iters[i] == 0);
                let (sat, unsat) = syndrome(code, &self.l);
                for i in 0..W {
                    if fresh[i]
                        && (sat[i] || p.max_iter == 0)
                        && let Some(w) = held[i].take()
                    {
                        out[w] = Some(self.result(i, 0, sat[i], unsat[i]));
                    }
                }
            }
            if held.iter().all(Option::is_none) {
                break;
            }
            let (sat, unsat) = iteration(code, &mut self.l, &mut self.r, &mut self.t, p.offset);
            for i in 0..W {
                if let Some(w) = held[i] {
                    iters[i] += 1;
                    if sat[i] || iters[i] == p.max_iter {
                        out[w] = Some(self.result(i, iters[i], sat[i], unsat[i]));
                        held[i] = None;
                    }
                }
            }
        }
        // The loop ends when no lane holds a codeword and none is left to
        // load: every codeword has left its lane with a result.
        out.into_iter()
            .map(|o| o.expect("decode_all: every codeword leaves its lane with a result"))
            .collect()
    }

    /// Decode up to `W` codewords of `code` in one batch: `llrs[i]` are
    /// the quantised input LLRs, `bits[i]` receives the hard decisions.
    /// The same results as [`Decoder::decode_all`].
    ///
    /// # Panics
    ///
    /// If `llrs` holds no codeword or more than `W`, `bits` not one
    /// vector per codeword, or a codeword's length is not the code's n.
    pub fn decode(
        &mut self,
        code: &Code,
        llrs: &[&[i8]],
        bits: &mut [Vec<u8>],
        p: Params,
    ) -> Vec<LaneResult> {
        assert!(
            (1..=W).contains(&llrs.len()),
            "{} codewords for {W} lanes",
            llrs.len()
        );
        assert_eq!(bits.len(), llrs.len());
        self.decode_all(code, llrs, p)
            .into_iter()
            .zip(bits.iter_mut())
            .map(|((r, b), out)| {
                *out = b;
                r
            })
            .collect()
    }

    /// Put codeword `w` in lane `i` with zero messages.
    fn load(&mut self, i: usize, w: &[i8]) {
        for (l, &x) in self.l.iter_mut().zip(w) {
            l.0[i] = x;
        }
        for r in &mut self.r {
            r.0[i] = 0;
        }
    }

    fn result(&self, i: usize, it: usize, sat: bool, unsat: usize) -> (LaneResult, Vec<u8>) {
        let bits = self.l.iter().map(|x| u8::from(x.0[i] < 0)).collect();
        let r = if sat {
            Ok(Decoded { iterations: it })
        } else {
            Err(NotConverged {
                iterations: it,
                unsatisfied: unsat,
            })
        };
        (r, bits)
    }
}

/// Per lane: whether the hard decisions satisfy every check, and how
/// many checks they do not.
fn syndrome<const W: usize>(code: &Code, l: &[Lane<W>]) -> ([bool; W], [usize; W]) {
    let mut unsat = [0usize; W];
    for c in 0..code.n - code.k {
        let (s, e) = (code.row_start[c] as usize, code.row_start[c + 1] as usize);
        let par = simd::sign_parity(&code.rows[s..e], l);
        for (u, &p) in unsat.iter_mut().zip(&par.0) {
            *u += usize::from(p < 0);
        }
    }
    (std::array::from_fn(|i| unsat[i] == 0), unsat)
}

/// One decoding iteration: every check node in order, then the syndrome.
/// A function of its own (never inlined into `decode_all`): measured
/// 4 600 → 5 700 codewords/s per thread at the baseline, against the same
/// loop inlined into the refill logic (plan §0.11).
#[inline(never)]
fn iteration<const W: usize>(
    code: &Code,
    l: &mut [Lane<W>],
    r: &mut [Lane<W>],
    t: &mut Vec<Lane<W>>,
    offset: i8,
) -> ([bool; W], [usize; W]) {
    for c in 0..code.n - code.k {
        let (s, e) = (code.row_start[c] as usize, code.row_start[c + 1] as usize);
        simd::check_node(&code.rows[s..e], l, &mut r[s..e], t, offset);
    }
    syndrome(code, l)
}
