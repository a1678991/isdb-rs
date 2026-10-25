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
        // the padded width chosen once per call: every loop below has a
        // constant trip count
        match self.width {
            8 => self.process_n::<8>(x, out),
            16 => self.process_n::<16>(x, out),
            24 => self.process_n::<24>(x, out),
            32 => self.process_n::<32>(x, out),
            40 => self.process_n::<40>(x, out),
            48 => self.process_n::<48>(x, out),
            56 => self.process_n::<56>(x, out),
            _ => self.process_n::<64>(x, out),
        }
    }

    /// Push a block of samples, handing each symbol to `f` as it is
    /// computed (no `Vec<Symbol>`: for callers that keep only part of it).
    pub fn process_each(&mut self, x: &[[i16; 2]], mut f: impl FnMut(&Symbol)) {
        match self.width {
            8 => self.each_n::<8>(x, &mut f),
            16 => self.each_n::<16>(x, &mut f),
            24 => self.each_n::<24>(x, &mut f),
            32 => self.each_n::<32>(x, &mut f),
            40 => self.each_n::<40>(x, &mut f),
            48 => self.each_n::<48>(x, &mut f),
            56 => self.each_n::<56>(x, &mut f),
            _ => self.each_n::<64>(x, &mut f),
        }
    }

    fn each_n<const W: usize>(&mut self, x: &[[i16; 2]], f: &mut impl FnMut(&Symbol)) {
        for &s in x {
            if let Some(y) = self.push_n::<W>(s) {
                f(&y);
            }
        }
    }

    /// Push one sample (the reference's `push`). [`FastFrontend::process`]
    /// is the fast way; this one dispatches the width for every sample.
    pub fn push(&mut self, x: [i16; 2]) -> Option<Symbol> {
        match self.width {
            8 => self.push_n::<8>(x),
            16 => self.push_n::<16>(x),
            24 => self.push_n::<24>(x),
            32 => self.push_n::<32>(x),
            40 => self.push_n::<40>(x),
            48 => self.push_n::<48>(x),
            56 => self.push_n::<56>(x),
            _ => self.push_n::<64>(x),
        }
    }

    fn process_n<const W: usize>(&mut self, x: &[[i16; 2]], out: &mut Vec<Symbol>) {
        for &s in x {
            if let Some(y) = self.push_n::<W>(s) {
                out.push(y);
            }
        }
    }

    #[inline]
    fn push_n<const W: usize>(&mut self, s: [i16; 2]) -> Option<Symbol> {
        let wide = s[0].unsigned_abs() > I32_INPUT_MAX as u16
            || s[1].unsigned_abs() > I32_INPUT_MAX as u16;
        self.since_wide = if wide {
            0
        } else {
            self.since_wide.saturating_add(1)
        };
        // the line holds the last `taps` inputs, this one included
        if self.since_wide >= self.taps {
            self.push_with::<false, W>(s)
        } else {
            self.push_with::<true, W>(s)
        }
    }

    #[inline]
    fn push_with<const WIDE: bool, const W: usize>(&mut self, x: [i16; 2]) -> Option<Symbol> {
        // mixer (the reference's `mix`)
        let idx = (self.nco >> (32 - NCO_LUT_BITS)) as usize;
        let mask = (1 << NCO_LUT_BITS) - 1;
        let c = i32::from(self.cos[idx]);
        let s = i32::from(self.cos[(idx + 768) & mask]);
        let (xi, xq) = (i32::from(x[0]), i32::from(x[1]));
        let re = xi * c - xq * s;
        let im = xi * s + xq * c;
        self.nco = self.nco.wrapping_add(self.p.nco_step as u32);
        let m = [(re + (1 << 14)) >> 15, (im + (1 << 14)) >> 15];
        // delay line: newest at `pos`
        if self.pos == 0 {
            // slide the newest `width − 1` samples to the end
            let w = self.width;
            self.li.copy_within(0..w - 1, LINE + 1);
            self.lq.copy_within(0..w - 1, LINE + 1);
            self.si.copy_within(0..w - 1, LINE + 1);
            self.sq.copy_within(0..w - 1, LINE + 1);
            self.pos = LINE + 1;
        }
        self.pos -= 1;
        self.li[self.pos] = m[0];
        self.lq[self.pos] = m[1];
        // truncation only matters for the wide kernel, which reads `li`/`lq`
        self.si[self.pos] = m[0] as i16;
        self.sq[self.pos] = m[1] as i16;
        let out = if self.t_acc < ONE {
            Some(self.symbol::<WIDE, W>())
        } else {
            None
        };
        self.t_acc -= ONE;
        self.samples += 1;
        out
    }

    #[inline]
    fn symbol<const WIDE: bool, const W: usize>(&mut self) -> Symbol {
        let phase = ((self.t_acc >> (FRAC_BITS - PHASE_BITS)) & ((1 << PHASE_BITS) - 1)) as usize;
        if self.pending_len == LOOP_DELAY {
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
        let [yi, yq, di, dq, ki, kq] = if WIDE {
            let at = phase * 3 * W;
            dot6_i64(
                &self.coef[at..at + 3 * W],
                &self.li[self.pos..self.pos + W],
                &self.lq[self.pos..self.pos + W],
            )
        } else {
            let at = phase * 6 * W;
            dot6_n::<W>(
                &self.split[at..at + 6 * W],
                &self.si[self.pos..self.pos + W],
                &self.sq[self.pos..self.pos + W],
            )
        };
        let p = &self.p;
        let filt = |a: i64| sat16(round_shift(a, FILTER_SHIFT));
        let y = [filt(yi), filt(yq)];
        let dy = [filt(di), filt(dq)];
        let yk = [filt(ki), filt(kq)];
        let gain = (self.gain_acc >> GAIN_ACC_EXTRA_BITS) as u16;
        let scale = |v: i16| sat16(round_shift(i64::from(v) * i64::from(gain), GAIN_FRAC_BITS));
        let z = [scale(y[0]), scale(y[1])];
        let dz = [scale(dy[0]), scale(dy[1])];
        let zk = [scale(yk[0]), scale(yk[1])];
        let lim = 1i64 << (p.bits - 1);
        let quant =
            |v: i16| round_shift(i64::from(v), u32::from(p.q_shift)).clamp(-lim, lim - 1) as i8;
        let q = [quant(zk[0]), quant(zk[1])];
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
        let sym = Symbol {
            sample: self.samples,
            phase: phase as u8,
            y,
            dy,
            yk,
            gain,
            period: self.period as u32,
            z,
            dz,
            zk,
            q,
            e,
            err,
        };
        self.t_acc += self.period;
        self.symbols += 1;
        sym
    }
}

/// `c = hi·2⁹ + lo`, `0 ≤ lo < 512`: exact for 18-bit `c` (|hi| ≤ 256).
fn split_coef(c: i32) -> (i16, i16) {
    let lo = c & 0x1FF;
    ((c >> 9) as i16, lo as i16)
}

/// `Σ c·x` over `W` 16-bit pairs, in `i32`.
#[inline]
fn dot_n<const W: usize>(c: &[i16; W], x: &[i16; W]) -> i32 {
    let mut a = 0i32;
    for t in 0..W {
        a += i32::from(c[t]) * i32::from(x[t]);
    }
    a
}

/// The six dot products of one symbol on the split coefficients
/// (`c6` = h.hi, h.lo, g.hi, g.lo, k.hi, k.lo, each `W` long) and the
/// `i16` delay line: 16×16-bit products summed in `i32` (exact within
/// [`I32_INPUT_MAX`], module documentation), combined in `i64`. `W` is
/// the padded width, a constant per instantiation, so LLVM unrolls and
/// vectorises the loops (`pmaddwd` / `vpmaddwd` / NEON `smlal`).
#[inline]
fn dot6_n<const W: usize>(c6: &[i16], li: &[i16], lq: &[i16]) -> [i64; 6] {
    let xi: &[i16; W] = li.try_into().expect("width");
    let xq: &[i16; W] = lq.try_into().expect("width");
    let c = |d: usize| -> &[i16; W] { c6[d * W..(d + 1) * W].try_into().expect("width") };
    let bank = |b: usize, x: &[i16; W]| -> i64 {
        (i64::from(dot_n(c(2 * b), x)) << 9) + i64::from(dot_n(c(2 * b + 1), x))
    };
    [
        bank(0, xi),
        bank(0, xq),
        bank(1, xi),
        bank(1, xq),
        bank(2, xi),
        bank(2, xq),
    ]
}

/// The same with `i64` sums on the unsplit coefficients (`c3` = h, g, k,
/// each `li.len()` long): exact for every `i16` input.
#[inline]
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
