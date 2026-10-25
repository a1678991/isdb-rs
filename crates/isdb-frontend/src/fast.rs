//! A faster implementation of [`crate::fixed::Frontend`] with the same
//! output, bit for bit: the reference stays the specification (and what
//! the FPGA matches); this one is what the receivers run.
//!
//! What differs is only how the same integers are computed:
//!
//! - **Linear delay line.** The newest `taps` mixer outputs are kept
//!   contiguous (a buffer written backwards and slid down when it runs
//!   out), so a filter is a plain dot product, without the reference's
//!   `(head + t) % taps` per tap.
//! - **Planar, fused banks.** The three banks (`h`, `g`, `k`) of each
//!   phase are stored together, padded to a multiple of 8 taps with zero
//!   coefficients (which add nothing), and evaluated in one pass over the
//!   I and Q lines held apart, so LLVM vectorises the loop.
//! - **16-bit products where they provably fit.** A mixer output is
//!   `(xI·c − xQ·s + 2¹⁴) >> 15` with |c|, |s| ≤ 32 767; for input
//!   components |x| ≤ 4 095 (13 bits; the radios deliver 12) that is at
//!   most 8 190 in magnitude, so the delay line fits `i16`. Each
//!   coefficient (18 bits, [`crate::params::COEF_BITS`]) is split as
//!   `c = hi·2⁹ + lo` with `0 ≤ lo < 512` and |hi| ≤ 256, both `i16`.
//!   Then `Σ c·m = 2⁹·Σ hi·m + Σ lo·m`, where every product is 16×16 bits
//!   (what SSE2's `pmaddwd` and NEON's `smlal` do) and each partial sum
//!   stays below 64 taps × 512 × 8 190 < 2²⁸, so `i32` holds it; the two
//!   are combined in `i64`, giving exactly the reference's sum. Each
//!   sample entering the delay line is checked; while any of the last
//!   `taps` inputs had a larger component, the `i64` kernel (on the
//!   unsplit coefficients) is used, so the equality holds for every `i16`
//!   input.
//!
//! The mixer, the symbol decision, the loop update and every rounding and
//! saturation are the reference's, in the same order.

use crate::fixed::{Symbol, round_shift, sat16};
use crate::params::{
    E_MAX, FILTER_SHIFT, FRAC_BITS, GAIN_ACC_BITS, GAIN_ACC_EXTRA_BITS, GAIN_FRAC_BITS, KI_FRAC,
    LOOP_DELAY, NCO_LUT_BITS, ONE, PHASE_BITS, PHASES, POWER_SHIFT, ParamError, Params,
};
use crate::tables::cos_table;

/// Largest input component magnitude for which the 16-bit kernel is exact
/// (module documentation).
pub const I32_INPUT_MAX: i16 = 4095;

/// Lanes the taps are padded to.
const PAD: usize = 8;

/// Same as [`crate::fixed::Frontend`], faster; the same [`Symbol`]s.
#[derive(Debug, Clone)]
pub struct FastFrontend {
    p: Params,
    cos: Vec<i16>,
    /// Taps per phase.
    taps: usize,
    /// Taps padded to a multiple of [`PAD`].
    width: usize,
    /// Per phase: `h`, `g`, `k`, each `width` coefficients (newest sample
    /// first), zero beyond the real taps.
    coef: Vec<i32>,
    /// The same split as `hi·2⁹ + lo`: per phase `h.hi`, `h.lo`, `g.hi`,
    /// `g.lo`, `k.hi`, `k.lo`, each `width` long.
    split: Vec<i16>,
    /// I and Q of the delay line: `li[pos .. pos + width]` holds the
    /// newest sample first. Written backwards from the end.
    li: Vec<i32>,
    lq: Vec<i32>,
    /// The same as `i16` (exact while the 16-bit kernel is in use).
    si: Vec<i16>,
    sq: Vec<i16>,
    pos: usize,
    nco: u32,
    t_acc: i64,
    v: i64,
    period: i64,
    gain_acc: i64,
    /// Loop errors waiting [`LOOP_DELAY`] symbols (ring of that length).
    pending: [(i32, i32); LOOP_DELAY],
    pending_len: usize,
    pending_head: usize,
    /// Samples since the last input beyond [`I32_INPUT_MAX`] (saturating):
    /// the `i32` kernel is exact once it reaches the tap count.
    since_wide: usize,
    samples: u64,
    symbols: u64,
    /// Work buffer: positions of the wide samples of a chunk.
    wides: Vec<usize>,
    /// Index into `wides` of the next one not yet passed.
    next_wide: usize,
}

/// Length of the delay-line buffers: room for many samples before a
/// slide.
const LINE: usize = 4096;

impl FastFrontend {
    /// A front end with parameters `p`, in the reference's initial state.
    pub fn new(p: Params) -> Result<Self, ParamError> {
        p.validate()?;
        let taps = p.taps();
        let width = taps.div_ceil(PAD) * PAD;
        let mut coef = vec![0i32; PHASES * 3 * width];
        for ph in 0..PHASES {
            for (b, bank) in [&p.h, &p.g, &p.k].into_iter().enumerate() {
                let at = (ph * 3 + b) * width;
                coef[at..at + taps].copy_from_slice(&bank[ph]);
            }
        }
        let mut split = vec![0i16; PHASES * 6 * width];
        for ph in 0..PHASES {
            for b in 0..3 {
                let src = (ph * 3 + b) * width;
                let hi_at = (ph * 6 + 2 * b) * width;
                let lo_at = hi_at + width;
                for t in 0..width {
                    let (hi, lo) = split_coef(coef[src + t]);
                    split[hi_at + t] = hi;
                    split[lo_at + t] = lo;
                }
            }
        }
        Ok(Self {
            cos: cos_table(),
            taps,
            width,
            coef,
            split,
            li: vec![0; LINE + width],
            lq: vec![0; LINE + width],
            si: vec![0; LINE + width],
            sq: vec![0; LINE + width],
            pos: LINE,
            nco: 0,
            t_acc: 0,
            v: 0,
            period: i64::from(p.period_nominal),
            gain_acc: i64::from(p.agc_init),
            pending: [(0, 0); LOOP_DELAY],
            pending_len: 0,
            pending_head: 0,
            since_wide: usize::MAX,
            samples: 0,
            symbols: 0,
            wides: Vec::new(),
            next_wide: 0,
            p,
        })
    }

    /// The parameters.
    pub fn params(&self) -> &Params {
        &self.p
    }

    /// Symbols emitted so far.
    pub fn symbols(&self) -> u64 {
        self.symbols
    }

    /// Push a block of samples, appending the symbols to `out` (the
    /// reference's `process`).
    pub fn process(&mut self, x: &[[i16; 2]], out: &mut Vec<Symbol>) {
        self.process_each(x, |s| out.push(*s));
    }

    /// Push a block of samples, handing each symbol to `f` as it is
    /// computed (no `Vec<Symbol>`: for callers that keep only part of it).
    pub fn process_each(&mut self, x: &[[i16; 2]], mut f: impl FnMut(&Symbol)) {
        // at most LINE samples at a time: the delay line holds them all
        for c in x.chunks(LINE) {
            match self.width {
                8 => self.chunk::<8>(c, &mut f),
                16 => self.chunk::<16>(c, &mut f),
                24 => self.chunk::<24>(c, &mut f),
                32 => self.chunk::<32>(c, &mut f),
                40 => self.chunk::<40>(c, &mut f),
                48 => self.chunk::<48>(c, &mut f),
                56 => self.chunk::<56>(c, &mut f),
                _ => self.chunk::<64>(c, &mut f),
            }
        }
    }

    /// Push one sample (the reference's `push`). [`FastFrontend::process`]
    /// is the fast way.
    pub fn push(&mut self, x: [i16; 2]) -> Option<Symbol> {
        let mut y = None;
        self.process_each(&[x], |s| y = Some(*s));
        y
    }

    /// One chunk (≤ [`LINE`] samples): the mixer for every sample in one
    /// pass into the delay line, then the symbols in order.
    fn chunk<const W: usize>(&mut self, x: &[[i16; 2]], f: &mut impl FnMut(&Symbol)) {
        let n = x.len();
        if n == 0 {
            return;
        }
        // room for the whole chunk below `pos`: slide the newest
        // `width − 1` samples up to the end first if needed
        if self.pos < n {
            let keep = self.width - 1;
            let src = self.pos;
            self.li.copy_within(src..src + keep, LINE + 1);
            self.lq.copy_within(src..src + keep, LINE + 1);
            self.si.copy_within(src..src + keep, LINE + 1);
            self.sq.copy_within(src..src + keep, LINE + 1);
            self.pos = LINE + 1;
        }
        let base = self.pos - n;
        self.mix(x, base);
        // symbols: sample j of the chunk is the newest at line index
        // base + n − 1 − j
        for j in 0..n {
            // `since_wide` after sample j (the line holds the last `taps`
            // inputs, this one included)
            if self.next_wide < self.wides.len() && self.wides[self.next_wide] == j {
                self.since_wide = 0;
                self.next_wide += 1;
            } else {
                self.since_wide = self.since_wide.saturating_add(1);
            }
            if self.t_acc < ONE {
                let pos = base + n - 1 - j;
                let sym = if self.since_wide >= self.taps {
                    self.symbol::<false, W>(pos)
                } else {
                    self.symbol::<true, W>(pos)
                };
                f(&sym);
            }
            self.t_acc -= ONE;
            self.samples += 1;
        }
        self.pos = base;
    }

    /// The symbol whose newest sample is at line index `pos`.
    #[inline(always)]
    fn symbol<const WIDE: bool, const W: usize>(&mut self, pos: usize) -> Symbol {
        let phase = ((self.t_acc >> (FRAC_BITS - PHASE_BITS)) & ((1 << PHASE_BITS) - 1)) as usize;
        if self.pending_len == LOOP_DELAY {
            self.loop_update();
        }
        let a = if WIDE {
            let at = phase * 3 * W;
            dot6_i64(
                &self.coef[at..at + 3 * W],
                &self.li[pos..pos + W],
                &self.lq[pos..pos + W],
            )
        } else {
            let at = phase * 6 * W;
            let (r, _) = self.split[at..at + 6 * W].as_chunks::<W>();
            dot6_n::<W>(
                r.try_into().expect("six rows"),
                self.si[pos..pos + W].try_into().expect("width"),
                self.sq[pos..pos + W].try_into().expect("width"),
            )
        };
        let q = Planned {
            phase,
            sample: self.samples,
            gain_acc: self.gain_acc,
            period: self.period,
        };
        self.t_acc += self.period;
        self.finish(&q, &a)
    }

    /// The mixer (the reference's `mix`) for every sample of the chunk:
    /// sample j goes to line index `base + n − 1 − j`. It does not depend
    /// on the timing loop. Also records which samples are wide (beyond
    /// [`I32_INPUT_MAX`]; never with 12-bit radios).
    fn mix(&mut self, x: &[[i16; 2]], base: usize) {
        let n = x.len();
        let mask = (1usize << NCO_LUT_BITS) - 1;
        let step = self.p.nco_step as u32;
        let mut nco = self.nco;
        self.wides.clear();
        for (j, s) in x.iter().enumerate() {
            let idx = (nco >> (32 - NCO_LUT_BITS)) as usize;
            let c = i32::from(self.cos[idx]);
            let sn = i32::from(self.cos[(idx + 768) & mask]);
            let (xi, xq) = (i32::from(s[0]), i32::from(s[1]));
            let re = (xi * c - xq * sn + (1 << 14)) >> 15;
            let im = (xi * sn + xq * c + (1 << 14)) >> 15;
            nco = nco.wrapping_add(step);
            let at = base + n - 1 - j;
            self.li[at] = re;
            self.lq[at] = im;
            // truncation only matters for the wide kernel, which reads li/lq
            self.si[at] = re as i16;
            self.sq[at] = im as i16;
            if s[0].unsigned_abs() > I32_INPUT_MAX as u16
                || s[1].unsigned_abs() > I32_INPUT_MAX as u16
            {
                self.wides.push(j);
            }
        }
        self.nco = nco;
        self.next_wide = 0;
    }

    /// The loop update a symbol applies before its filter outputs, with
    /// the oldest queued error (that of the symbol [`LOOP_DELAY`] earlier).
    #[inline(always)]
    fn loop_update(&mut self) {
        {
            let (e, err) = self.pending[self.pending_head];
            self.pending_len -= 1;
            self.pending_head += 1;
            if self.pending_head == LOOP_DELAY {
                self.pending_head = 0;
            }
            let p = &self.p;
            let vmax = i64::from(p.integrator_max) << KI_FRAC;
            let ki_e = round_shift(i64::from(p.ki) * i64::from(e), u32::from(p.ki_shift));
            self.v = (self.v + ki_e).clamp(-vmax, vmax);
            self.period = (i64::from(p.period_nominal)
                + (self.v >> KI_FRAC)
                + round_shift(i64::from(p.kp) * i64::from(e), u32::from(p.kp_shift)))
            .clamp(i64::from(p.period_min), i64::from(p.period_max));
            self.gain_acc = (self.gain_acc + (i64::from(err) >> p.agc_shift))
                .clamp(0, (1 << GAIN_ACC_BITS) - 1);
        }
    }

    /// A planned symbol from its filter sums: rounding, gain, decision and
    /// its loop errors (queued for [`LOOP_DELAY`] symbols later).
    #[inline(always)]
    fn finish(&mut self, q: &Planned, a: &[i64; 6]) -> Symbol {
        let p = &self.p;
        let [yi, yq, di, dq, ki, kq] = *a;
        let filt = |a: i64| sat16(round_shift(a, FILTER_SHIFT));
        let y = [filt(yi), filt(yq)];
        let dy = [filt(di), filt(dq)];
        let yk = [filt(ki), filt(kq)];
        let gain = (q.gain_acc >> GAIN_ACC_EXTRA_BITS) as u16;
        let scale = |v: i16| sat16(round_shift(i64::from(v) * i64::from(gain), GAIN_FRAC_BITS));
        let z = [scale(y[0]), scale(y[1])];
        let dz = [scale(dy[0]), scale(dy[1])];
        let zk = [scale(yk[0]), scale(yk[1])];
        let lim = 1i64 << (p.bits - 1);
        let quant =
            |v: i16| round_shift(i64::from(v), u32::from(p.q_shift)).clamp(-lim, lim - 1) as i8;
        let q8 = [quant(zk[0]), quant(zk[1])];
        let ted = i64::from(z[0]) * i64::from(dz[0]) + i64::from(z[1]) * i64::from(dz[1]);
        let e = (ted >> p.ted_shift).clamp(-E_MAX, E_MAX) as i32;
        let pow = (i64::from(zk[0]).pow(2) + i64::from(zk[1]).pow(2)) >> POWER_SHIFT;
        let err = (i64::from(p.agc_target) - pow) as i32;
        let mut tail = self.pending_head + self.pending_len;
        if tail >= LOOP_DELAY {
            tail -= LOOP_DELAY;
        }
        self.pending[tail] = (e, err);
        self.pending_len += 1;
        self.symbols += 1;
        Symbol {
            sample: q.sample,
            phase: q.phase as u8,
            y,
            dy,
            yk,
            gain,
            period: q.period as u32,
            z,
            dz,
            zk,
            q: q8,
            e,
            err,
        }
    }
}

/// A symbol's state before its filter outputs: what
/// [`FastFrontend::finish`] needs besides the sums.
#[derive(Debug, Clone, Copy, Default)]
struct Planned {
    /// Interpolation phase.
    phase: usize,
    /// Its `Symbol::sample`.
    sample: u64,
    /// The AGC accumulator after its loop update.
    gain_acc: i64,
    /// The period after its loop update.
    period: i64,
}

/// The six dot products of one symbol on the split coefficients
/// (rows h.hi, h.lo, g.hi, g.lo, k.hi, k.lo) and the `i16` delay line:
/// 16×16-bit products summed in `i32` (exact within [`I32_INPUT_MAX`],
/// module documentation), combined in `i64` as `2⁹·Σ hi·x + Σ lo·x`.
#[inline]
fn dot6_n<const W: usize>(c6: &[[i16; W]; 6], xi: &[i16; W], xq: &[i16; W]) -> [i64; 6] {
    let d = |v: i32| i64::from(v);
    [
        (d(dot_n(&c6[0], xi)) << 9) + d(dot_n(&c6[1], xi)),
        (d(dot_n(&c6[0], xq)) << 9) + d(dot_n(&c6[1], xq)),
        (d(dot_n(&c6[2], xi)) << 9) + d(dot_n(&c6[3], xi)),
        (d(dot_n(&c6[2], xq)) << 9) + d(dot_n(&c6[3], xq)),
        (d(dot_n(&c6[4], xi)) << 9) + d(dot_n(&c6[5], xi)),
        (d(dot_n(&c6[4], xq)) << 9) + d(dot_n(&c6[5], xq)),
    ]
}

/// `Σ c·x` over `W` 16-bit pairs, in `i32`. A small function on its own,
/// its loop becomes multiply-adds (`pmaddwd`, NEON `smlal`); the shapes
/// tried instead (twelve products inlined into one body, I and Q in one
/// loop, eight-lane accumulators) stayed scalar under LLVM's SLP
/// vectoriser and ran up to 3× slower (measured; plan §0.11).
#[inline]
fn dot_n<const W: usize>(c: &[i16; W], x: &[i16; W]) -> i32 {
    let mut a = 0i32;
    for t in 0..W {
        a += i32::from(c[t]) * i32::from(x[t]);
    }
    a
}

/// `c = hi·2⁹ + lo`, `0 ≤ lo < 512`: exact for 18-bit `c` (|hi| ≤ 256).
fn split_coef(c: i32) -> (i16, i16) {
    let lo = c & 0x1FF;
    ((c >> 9) as i16, lo as i16)
}

/// The same with `i64` sums on the unsplit coefficients (`c3` = h, g, k,
/// each `li.len()` long): exact for every `i16` input.
#[inline(always)]
fn dot6_i64(c3: &[i32], li: &[i32], lq: &[i32]) -> [i64; 6] {
    let w = li.len();
    let mut acc = [0i64; 6];
    for b in 0..3 {
        for t in 0..w {
            let c = i64::from(c3[b * w + t]);
            acc[2 * b] += c * i64::from(li[t]);
            acc[2 * b + 1] += c * i64::from(lq[t]);
        }
    }
    acc
}
