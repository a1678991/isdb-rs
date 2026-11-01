//! Fixed-point layered min-sum for [`Code`] (ARIB STD-B44 v2.1 §3.4 (2)),
//! decoding up to `W` codewords of one code at once, one per lane.
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
//! - **Check c** (edges in the order of [`Code::check`]): `t_e = sat(L_v −
//!   r_e)` for each edge e to variable v; the sign parity and the two
//!   smallest `|t_e|` (min1 ≤ min2, first edge index of min1 on ties);
//!   then the new message `r_e = ±max(m − offset, 0)` with m = min2 on
//!   the min1 edge and min1 elsewhere, its sign the parity times the sign
//!   of `t_e`; and `L_v = sat(t_e + r_e)`. `|t| ` is computed saturating,
//!   so −128 reads 127.
//! - **Schedule** ([`Schedule`]: the order of the checks in one iteration
//!   and which `L` each reads):
//!   - [`Schedule::Sequential`] (the default, the CPU's fast path): checks
//!     0 … P−1 one at a time, each reading the `L` the previous check
//!     wrote.
//!   - [`Schedule::Layered`] `{ step }` (the FPGA decoder's; the contract
//!     the HDL reproduces bit for bit, docs/plans/isdbs3-fpga-rx.md "The
//!     LDPC schedule contract"): the checks layer by layer, layer l = the
//!     374 checks c ≡ l mod q (l = 0 … q−1, q = P / 374, increasing c
//!     within a layer), each layer cut into steps of `step` consecutive
//!     checks (the last step of a layer shorter when `step` does not
//!     divide 374). Within a step every check reads `L` and its messages
//!     as they were at the step's start and computes its `t_e =
//!     sat(L_v − r_e,old)` and new `r_e,new` as above; `r` is written per
//!     edge. At the step's end, for each variable v with edges e₁ … e_m in
//!     the step (in check order): `L_v = sat(t_e₁ + r_e₁,new + Σ_{i=2..m}
//!     (r_eᵢ,new − r_eᵢ,old))`, the sum in a wide integer (i16) saturated
//!     to i8 once (**delta write-back**). m ≥ 2 only for a variable two
//!     checks of one step share (two entries of one address-table row
//!     congruent mod q, a "conflicting pair",
//!     `tools/ldpc-layer-conflicts.py`); with m = 1 it is `sat(t_e +
//!     r_e,new)`, the sequential update. The next step reads those writes.
//!     With `step` 1 this is a sequential decoder in layer order. (The
//!     first contract, the later check's `sat(t + r)` overwriting, loses
//!     the other check's update and gave an error floor at steps of 187
//!     and 374: docs/plans/isdbs3-fpga-rx.md "Results".) Why layers: the
//!     374 bits of a group hit the checks of
//!     a layer once each (a cyclic shift, §3.4 (2)), so a layer's checks
//!     touch disjoint variables bar the conflicting pairs, and `step`
//!     check-node units can run a step in parallel.
//! - **Stopping**: after each iteration the hard decisions (`L < 0` is a
//!   one) are checked against every parity check; a lane whose word is a
//!   codeword stops there, its bits and iteration count recorded. A lane
//!   already a codeword at the input stops at iteration 0. Other lanes go
//!   on to `max_iter`. The same for both schedules.
//!
//! Lanes past the number of codewords given carry zeros and are ignored.

use super::{Code, Decoded, GROUP, NotConverged};
use crate::simd::{self, Lane};

/// The order in which one iteration visits the checks, and which
/// a-posteriori values each reads (the module docs give both in full).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Schedule {
    /// Checks 0 … P−1 one at a time, each after the previous one's
    /// writes: the CPU decoder.
    #[default]
    Sequential,
    /// Layer by layer (layer l: the checks c ≡ l mod q, §3.4 (2)), each
    /// layer in steps of `step` checks that all read the values of the
    /// step's start; a variable with several edges in a step adds every
    /// one's message change (delta write-back). The FPGA decoder's
    /// schedule (`step` check-node units).
    Layered {
        /// Checks per step, at least 1 (1: sequential in layer order;
        /// 374 or more: a whole layer per step).
        step: usize,
    },
}

/// The decoder's settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Params {
    /// Iteration limit.
    pub max_iter: usize,
    /// Offset subtracted from every check-to-variable magnitude (offset
    /// min-sum), in LLR units of the quantised input.
    pub offset: i8,
    /// The check schedule ([`Schedule::Sequential`] by default).
    pub schedule: Schedule,
}

impl Params {
    /// `max_iter` and `offset` with the default schedule,
    /// [`Schedule::Sequential`].
    pub const fn new(max_iter: usize, offset: i8) -> Self {
        Self {
            max_iter,
            offset,
            schedule: Schedule::Sequential,
        }
    }
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

/// The per-iteration callback of [`Decoder::decode_all_traced`].
type Trace<'a> = Option<&'a mut dyn FnMut(usize, &[u8])>;

/// A variable with two or more edges in one step of the layered schedule
/// (a conflicting pair, or more): its delta write-back.
#[derive(Debug, Clone)]
struct Conflict {
    /// The variable.
    v: u32,
    /// Index of its first edge's `t` in the step's `t` scratch.
    t1: u32,
    /// Its edges in check order (global edge indices into `r`).
    edges: Vec<u32>,
}

/// The conflicts of every step of `Layered { step }` for one code, in
/// step order (layer by layer): `steps[s]` is step s's.
#[derive(Debug, Clone)]
struct Conflicts {
    steps: Vec<Vec<Conflict>>,
}

impl Conflicts {
    fn new(code: &Code, step: usize) -> Self {
        let q = code.q();
        let mut steps = Vec::new();
        // per variable of the current step: (t index of its first edge,
        // its edges in check order)
        let mut seen: std::collections::HashMap<u32, (u32, Vec<u32>)> =
            std::collections::HashMap::new();
        for layer in 0..q {
            let mut i0 = 0;
            while i0 < GROUP {
                let i1 = (i0 + step).min(GROUP);
                seen.clear();
                let mut at = 0u32;
                for i in i0..i1 {
                    let c = layer + i * q;
                    let s = code.row_start[c];
                    for (e, &v) in code.check(c).iter().enumerate() {
                        // a check's degree (≤ 34) and an edge index
                        // (< 2³²: Code::new) are inside u32
                        #[allow(clippy::cast_possible_truncation)]
                        let ge = s + e as u32;
                        seen.entry(v).or_insert_with(|| (at, Vec::new())).1.push(ge);
                        at += 1;
                    }
                }
                let mut cs: Vec<Conflict> = seen
                    .drain()
                    .filter(|(_, (_, edges))| edges.len() > 1)
                    .map(|(v, (t1, edges))| Conflict { v, t1, edges })
                    .collect();
                // a fixed order (the writes are independent; for
                // reproducible scratch use)
                cs.sort_unstable_by_key(|c| c.t1);
                steps.push(cs);
                i0 = i1;
            }
        }
        Self { steps }
    }
}

/// Work buffers for one code and lane width, reused across calls.
#[derive(Debug, Clone)]
pub struct Decoder<const W: usize> {
    l: Vec<Lane<W>>,
    r: Vec<Lane<W>>,
    t: Vec<Lane<W>>,
    /// The old messages of a layered step's conflicting edges.
    r_old: Vec<Lane<W>>,
}

impl<const W: usize> Default for Decoder<W> {
    fn default() -> Self {
        Self {
            l: Vec::new(),
            r: Vec::new(),
            t: Vec::new(),
            r_old: Vec::new(),
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
    /// If a word's length is not the code's n, or the schedule is
    /// [`Schedule::Layered`] with `step` 0.
    pub fn decode_all(
        &mut self,
        code: &Code,
        words: &[&[i8]],
        p: Params,
    ) -> Vec<(LaneResult, Vec<u8>)> {
        self.run(code, words, p, None)
    }

    /// [`Decoder::decode_all`], calling `trace(w, bits)` after every
    /// iteration of codeword `w` (index into `words`) with its hard
    /// decisions (0/1, length n) after that iteration: for the FPGA
    /// vectors, which carry them to locate a mismatch. The results are
    /// [`Decoder::decode_all`]'s.
    ///
    /// # Panics
    ///
    /// As [`Decoder::decode_all`].
    pub fn decode_all_traced(
        &mut self,
        code: &Code,
        words: &[&[i8]],
        p: Params,
        mut trace: impl FnMut(usize, &[u8]),
    ) -> Vec<(LaneResult, Vec<u8>)> {
        self.run(code, words, p, Some(&mut trace))
    }

    fn run(
        &mut self,
        code: &Code,
        words: &[&[i8]],
        p: Params,
        mut trace: Trace<'_>,
    ) -> Vec<(LaneResult, Vec<u8>)> {
        let n = code.n;
        for w in words {
            assert_eq!(w.len(), n, "codeword length");
        }
        if let Schedule::Layered { step } = p.schedule {
            assert!(step >= 1, "layered schedule with step 0");
        }
        let mut hard = Vec::new();
        // the layered schedule's conflicts, built at its first iteration
        // (one pass over the edges; per call, as `code` may differ)
        let mut conflicts: Option<Conflicts> = None;
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
            let (sat, unsat) = match p.schedule {
                Schedule::Sequential => {
                    iteration(code, &mut self.l, &mut self.r, &mut self.t, p.offset)
                }
                Schedule::Layered { step } => {
                    let conflicts = conflicts.get_or_insert_with(|| Conflicts::new(code, step));
                    layered_iteration(
                        code,
                        &mut self.l,
                        &mut self.r,
                        &mut self.t,
                        &mut self.r_old,
                        conflicts,
                        p.offset,
                        step,
                    )
                }
            };
            for i in 0..W {
                if let Some(w) = held[i] {
                    iters[i] += 1;
                    if let Some(f) = trace.as_mut() {
                        hard.clear();
                        hard.extend(self.l.iter().map(|x| u8::from(x.0[i] < 0)));
                        f(w, &hard);
                    }
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
    /// vector per codeword, a codeword's length is not the code's n, or
    /// the schedule is [`Schedule::Layered`] with `step` 0.
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

/// One iteration of [`Schedule::Layered`] `{ step }`, then the syndrome:
/// per layer, per step, the old messages of the step's conflicting edges
/// kept; every check's `t` and new messages from the step's starting `L`
/// (the step's `t`, check after check, kept in `t`); `L_v = sat(t + r)`
/// for every edge; then each conflicting variable rewritten by the delta
/// rule (module docs) from its first edge's `t` and its edges' messages.
#[inline(never)]
#[allow(clippy::too_many_arguments)] // the decoder's buffers, passed apart for the borrows
fn layered_iteration<const W: usize>(
    code: &Code,
    l: &mut [Lane<W>],
    r: &mut [Lane<W>],
    t: &mut Vec<Lane<W>>,
    r_old: &mut Vec<Lane<W>>,
    conflicts: &Conflicts,
    offset: i8,
    step: usize,
) -> ([bool; W], [usize; W]) {
    let q = code.q();
    let edges = |c: usize| (code.row_start[c] as usize, code.row_start[c + 1] as usize);
    let mut steps = conflicts.steps.iter();
    // P = 374 q (Code::new): every layer has exactly 374 checks, l + i·q
    for layer in 0..q {
        let mut i0 = 0;
        while i0 < GROUP {
            let i1 = (i0 + step).min(GROUP);
            let cs = steps.next().expect("one conflict list per step");
            r_old.clear();
            for c in cs {
                r_old.extend(c.edges[1..].iter().map(|&e| r[e as usize]));
            }
            t.clear();
            for i in i0..i1 {
                let (s, e) = edges(layer + i * q);
                simd::check_messages(&code.rows[s..e], l, &mut r[s..e], t, offset);
            }
            let mut at = 0;
            for i in i0..i1 {
                let (s, e) = edges(layer + i * q);
                let d = e - s;
                simd::apply_messages(&code.rows[s..e], &t[at..at + d], &r[s..e], l);
                at += d;
            }
            let mut o = 0;
            for c in cs {
                let m = c.edges.len() - 1;
                l[c.v as usize] = simd::delta_writeback(
                    &t[c.t1 as usize],
                    &r[c.edges[0] as usize],
                    c.edges[1..].iter().map(|&e| &r[e as usize]),
                    &r_old[o..o + m],
                );
                o += m;
            }
            i0 = i1;
        }
    }
    syndrome(code, l)
}
