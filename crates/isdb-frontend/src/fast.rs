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
use crate::simd::{self, Isa, Kernels};
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
    /// The instruction set [`FastFrontend::process`] and
    /// [`FastFrontend::process_each`] run on.
    isa: Isa,
}

/// Length of the delay-line buffers: room for many samples before a
/// slide.
const LINE: usize = 4096;

impl FastFrontend {
    /// A front end with parameters `p`, in the reference's initial state,
    /// on the widest instruction set this CPU and build run
    /// ([`Isa::best`], lowered by `ISDB_SIMD`).
    pub fn new(p: Params) -> Result<Self, ParamError> {
        Self::with_isa(p, Isa::best())
    }

    /// The same on instruction set `isa` (one the CPU does not run falls
    /// back to the baseline kernels): the output is the same for every
    /// `isa`.
    pub fn with_isa(p: Params, isa: Isa) -> Result<Self, ParamError> {
        p.validate()?;
        let isa = if isa.available() { isa } else { Isa::Baseline };
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
            isa,
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

    /// The instruction set the block methods run on.
    pub fn isa(&self) -> Isa {
        self.isa
    }

    /// Push a block of samples, appending the symbols to `out` (the
    /// reference's `process`).
    pub fn process(&mut self, x: &[[i16; 2]], out: &mut Vec<Symbol>) {
        // the padded width chosen once per call: every loop below has a
        // constant trip count
        match self.width {
            8 => self.process_isa::<8>(x, out),
            16 => self.process_isa::<16>(x, out),
            24 => self.process_isa::<24>(x, out),
            32 => self.process_isa::<32>(x, out),
            40 => self.process_isa::<40>(x, out),
            48 => self.process_isa::<48>(x, out),
            56 => self.process_isa::<56>(x, out),
            _ => self.process_isa::<64>(x, out),
        }
    }

    fn process_isa<const W: usize>(&mut self, x: &[[i16; 2]], out: &mut Vec<Symbol>) {
        let k = Kernels {
            baseline: process_baseline::<W>,
            #[cfg(target_arch = "x86_64")]
            avx2: process_avx2::<W>,
            #[cfg(all(target_arch = "x86_64", isdb_frontend_avx512))]
            avx512: process_avx512::<W>,
        };
        simd::run(&k, self.isa, self, (x, out));
    }

    /// Push a block of samples, handing each symbol to `f` as it is
    /// computed (no `Vec<Symbol>`: for callers that keep only part of it).
    pub fn process_each(&mut self, x: &[[i16; 2]], mut f: impl FnMut(&Symbol)) {
        match self.width {
            8 => self.each_isa::<8, _>(x, &mut f),
            16 => self.each_isa::<16, _>(x, &mut f),
            24 => self.each_isa::<24, _>(x, &mut f),
            32 => self.each_isa::<32, _>(x, &mut f),
            40 => self.each_isa::<40, _>(x, &mut f),
            48 => self.each_isa::<48, _>(x, &mut f),
            56 => self.each_isa::<56, _>(x, &mut f),
            _ => self.each_isa::<64, _>(x, &mut f),
        }
    }

    fn each_isa<const W: usize, F: FnMut(&Symbol)>(&mut self, x: &[[i16; 2]], f: &mut F) {
        let k = Kernels {
            baseline: each_baseline::<W, F>,
            #[cfg(target_arch = "x86_64")]
            avx2: each_avx2::<W, F>,
            #[cfg(all(target_arch = "x86_64", isdb_frontend_avx512))]
            avx512: each_avx512::<W, F>,
        };
        simd::run(&k, self.isa, self, (x, f));
    }

    /// Push one sample (the reference's `push`). [`FastFrontend::process`]
    /// is the fast way; this one dispatches the width for every sample
    /// and runs the baseline kernels (same output).
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

    /// One sample through the baseline kernels (what [`FastFrontend::push`]
    /// runs).
    #[inline(always)]
    fn push_n<const W: usize>(&mut self, s: [i16; 2]) -> Option<Symbol> {
        if !self.advance(s) {
            return None;
        }
        let d = if self.wide() {
            self.dots_wide::<W>()
        } else {
            let (c6, xi, xq) = self.split_operands::<W>();
            dot6_n::<W>(c6, xi, xq)
        };
        Some(self.finish(d))
    }

    /// Whether the newest `taps` inputs need the `i64` kernel.
    #[inline(always)]
    fn wide(&self) -> bool {
        self.since_wide < self.taps
    }

    /// The `i64` kernel's six sums at the current phase and position.
    #[inline(always)]
    fn dots_wide<const W: usize>(&self) -> [i64; 6] {
        let at = self.phase() * 3 * W;
        dot6_i64(
            &self.coef[at..at + 3 * W],
            &self.li[self.pos..self.pos + W],
            &self.lq[self.pos..self.pos + W],
        )
    }

    /// The 16-bit kernel's operands at the current phase and position:
    /// six split coefficient rows and the `i16` delay line.
    #[inline(always)]
    fn split_operands<const W: usize>(&self) -> (&[i16], &[i16], &[i16]) {
        let at = self.phase() * 6 * W;
        (
            &self.split[at..at + 6 * W],
            &self.si[self.pos..self.pos + W],
            &self.sq[self.pos..self.pos + W],
        )
    }

    /// The filter phase of the symbol about to be computed.
    #[inline(always)]
    fn phase(&self) -> usize {
        ((self.t_acc >> (FRAC_BITS - PHASE_BITS)) & ((1 << PHASE_BITS) - 1)) as usize
    }

    /// One input sample: the wide-input bookkeeping, the mixer and the
    /// delay line. Returns whether a symbol is computed at it; if so, the
    /// caller evaluates the six sums at [`Self::phase`] and the current
    /// position (with the kernel of its instruction set) and passes them to
    /// [`Self::finish`].
    #[inline(always)]
    fn advance(&mut self, x: [i16; 2]) -> bool {
        let wide_in = x[0].unsigned_abs() > I32_INPUT_MAX as u16
            || x[1].unsigned_abs() > I32_INPUT_MAX as u16;
        self.since_wide = if wide_in {
            0
        } else {
            self.since_wide.saturating_add(1)
        };
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
        let fire = self.t_acc < ONE;
        if !fire {
            self.t_acc -= ONE;
            self.samples += 1;
        }
        fire
    }

    /// The symbol at the sample [`Self::advance`] reported, from the six
    /// sums `d` (h·I, h·Q, g·I, g·Q, k·I, k·Q at its phase): the loop
    /// update, the decision and the output, as the reference does them.
    #[inline(always)]
    fn finish(&mut self, d: [i64; 6]) -> Symbol {
        let phase = self.phase();
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
        let [yi, yq, di, dq, ki, kq] = d;
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
        // the sample's own bookkeeping, deferred by `advance`
        self.t_acc -= ONE;
        self.samples += 1;
        sym
    }
}

/// The block loops, one per instruction set ([`crate::simd`]). Each is the
/// same loop over [`FastFrontend::advance`] and [`FastFrontend::finish`]
/// (generic, inlined) around its own 16-bit dot-product kernel; the `i64`
/// kernel for wide inputs is shared. The `#[target_feature]` loops call
/// their kernels without `unsafe` because the kernels have the same
/// features; [`crate::simd::run`] is what calls the loops.
type Block<'a> = (&'a [[i16; 2]], &'a mut Vec<Symbol>);
type Each<'a, F> = (&'a [[i16; 2]], &'a mut F);

macro_rules! block_loops {
    ($process:ident, $each:ident, $dot6:ident $(, #[$attr:meta])*) => {
        $(#[$attr])*
        fn $process<const W: usize>(f: &mut FastFrontend, (x, out): Block<'_>) {
            for &s in x {
                if f.advance(s) {
                    let d = if f.wide() {
                        f.dots_wide::<W>()
                    } else {
                        let (c6, xi, xq) = f.split_operands::<W>();
                        $dot6::<W>(c6, xi, xq)
                    };
                    out.push(f.finish(d));
                }
            }
        }

        $(#[$attr])*
        fn $each<const W: usize, F: FnMut(&Symbol)>(f: &mut FastFrontend, (x, g): Each<'_, F>) {
            for &s in x {
                if f.advance(s) {
                    let d = if f.wide() {
                        f.dots_wide::<W>()
                    } else {
                        let (c6, xi, xq) = f.split_operands::<W>();
                        $dot6::<W>(c6, xi, xq)
                    };
                    g(&f.finish(d));
                }
            }
        }
    };
}

block_loops!(process_baseline, each_baseline, dot6_n);
block_loops!(
    process_avx2,
    each_avx2,
    dot6_avx2,
    #[cfg(target_arch = "x86_64")],
    #[target_feature(enable = "avx2,fma,bmi1,bmi2,lzcnt,popcnt")]
);
block_loops!(
    process_avx512,
    each_avx512,
    dot6_avx512,
    #[cfg(all(target_arch = "x86_64", isdb_frontend_avx512))],
    #[target_feature(enable = "avx2,fma,bmi1,bmi2,lzcnt,popcnt,avx512f,avx512bw,avx512vl,avx512dq,avx512cd")]
);

/// `c = hi·2⁹ + lo`, `0 ≤ lo < 512`: exact for 18-bit `c` (|hi| ≤ 256).
fn split_coef(c: i32) -> (i16, i16) {
    let lo = c & 0x1FF;
    ((c >> 9) as i16, lo as i16)
}

/// `Σ c·x` over `W` 16-bit pairs, in `i32`. Written as `pmaddwd` does it
/// (adjacent products summed in pairs into eight `i32` lanes, the lanes
/// summed at the end) so that LLVM vectorises it wherever it is inlined,
/// including into the `#[target_feature]` loops of [`crate::simd`], where
/// a plain reduction loop was left scalar. Integer addition is exact here
/// (every partial sum is below 2²⁸, module documentation), so the order
/// does not change the result.
#[inline(always)]
fn dot_n<const W: usize>(c: &[i16; W], x: &[i16; W]) -> i32 {
    let mut lanes = [0i32; 8];
    for (cc, xx) in c.as_chunks::<16>().0.iter().zip(x.as_chunks::<16>().0) {
        for (l, lane) in lanes.iter_mut().enumerate() {
            *lane += i32::from(cc[2 * l]) * i32::from(xx[2 * l])
                + i32::from(cc[2 * l + 1]) * i32::from(xx[2 * l + 1]);
        }
    }
    // W is a multiple of 8: a last half block of 8 taps
    let tail = W - W % 16;
    for t in tail..W {
        lanes[(t - tail) / 2] += i32::from(c[t]) * i32::from(x[t]);
    }
    lanes.iter().sum()
}

/// The six dot products of one symbol on the split coefficients
/// (`c6` = h.hi, h.lo, g.hi, g.lo, k.hi, k.lo, each `W` long) and the
/// `i16` delay line: 16×16-bit products summed in `i32` (exact within
/// [`I32_INPUT_MAX`], module documentation), combined in `i64`. `W` is
/// the padded width, a constant per instantiation, so LLVM unrolls and
/// vectorises the loops (`pmaddwd` / `vpmaddwd` / NEON `smlal`).
#[inline(always)]
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

/// [`dot6_n`] with AVX2: each row's sum is `vpmaddwd` over 16 taps at a
/// time (`vpmaddwd` on 8 for a last half block), the eight lanes summed at
/// the end. The same integers (every partial sum below 2²⁸).
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2,fma,bmi1,bmi2,lzcnt,popcnt")]
fn dot6_avx2<const W: usize>(c6: &[i16], li: &[i16], lq: &[i16]) -> [i64; 6] {
    use std::arch::x86_64::*;
    let xi: &[i16; W] = li.try_into().expect("width");
    let xq: &[i16; W] = lq.try_into().expect("width");
    let c6: &[i16] = &c6[..6 * W];
    let full = W / 16;
    let mut r = [0i32; 12];
    for (row, out) in r.as_chunks_mut::<2>().0.iter_mut().enumerate() {
        let c = &c6[row * W..(row + 1) * W];
        let (mut ai, mut aq) = (_mm256_setzero_si256(), _mm256_setzero_si256());
        for b in 0..full {
            let cv = load16(&c[16 * b..16 * b + 16]);
            ai = _mm256_add_epi32(ai, _mm256_madd_epi16(cv, load16(&xi[16 * b..16 * b + 16])));
            aq = _mm256_add_epi32(aq, _mm256_madd_epi16(cv, load16(&xq[16 * b..16 * b + 16])));
        }
        let (mut si, mut sq) = (fold256(ai), fold256(aq));
        if !W.is_multiple_of(16) {
            let at = 16 * full;
            let cv = load8(&c[at..at + 8]);
            si = _mm_add_epi32(si, _mm_madd_epi16(cv, load8(&xi[at..at + 8])));
            sq = _mm_add_epi32(sq, _mm_madd_epi16(cv, load8(&xq[at..at + 8])));
        }
        out[0] = hsum128(si);
        out[1] = hsum128(sq);
    }
    combine(&r)
}

/// [`dot6_n`] with AVX-512: `vpmaddwd` on 32 taps at a time, then the
/// AVX2 steps for what is left (16, then 8 taps).
#[cfg(all(target_arch = "x86_64", isdb_frontend_avx512))]
// only compiled when the build script found Rust ≥ 1.89 (AVX-512)
#[clippy::msrv = "1.89"]
#[target_feature(
    enable = "avx2,fma,bmi1,bmi2,lzcnt,popcnt,avx512f,avx512bw,avx512vl,avx512dq,avx512cd"
)]
fn dot6_avx512<const W: usize>(c6: &[i16], li: &[i16], lq: &[i16]) -> [i64; 6] {
    use std::arch::x86_64::*;
    let xi: &[i16; W] = li.try_into().expect("width");
    let xq: &[i16; W] = lq.try_into().expect("width");
    let c6: &[i16] = &c6[..6 * W];
    let full = W / 32;
    let mut r = [0i32; 12];
    for (row, out) in r.as_chunks_mut::<2>().0.iter_mut().enumerate() {
        let c = &c6[row * W..(row + 1) * W];
        let (mut ai, mut aq) = (_mm512_setzero_si512(), _mm512_setzero_si512());
        for b in 0..full {
            let at = 32 * b;
            let cv = load32(&c[at..at + 32]);
            ai = _mm512_add_epi32(ai, _mm512_madd_epi16(cv, load32(&xi[at..at + 32])));
            aq = _mm512_add_epi32(aq, _mm512_madd_epi16(cv, load32(&xq[at..at + 32])));
        }
        let (mut ai, mut aq) = (fold512(ai), fold512(aq));
        let mut at = 32 * full;
        if W - at >= 16 {
            let cv = load16(&c[at..at + 16]);
            ai = _mm256_add_epi32(ai, _mm256_madd_epi16(cv, load16(&xi[at..at + 16])));
            aq = _mm256_add_epi32(aq, _mm256_madd_epi16(cv, load16(&xq[at..at + 16])));
            at += 16;
        }
        let (mut si, mut sq) = (fold256(ai), fold256(aq));
        if W - at >= 8 {
            let cv = load8(&c[at..at + 8]);
            si = _mm_add_epi32(si, _mm_madd_epi16(cv, load8(&xi[at..at + 8])));
            sq = _mm_add_epi32(sq, _mm_madd_epi16(cv, load8(&xq[at..at + 8])));
        }
        out[0] = hsum128(si);
        out[1] = hsum128(sq);
    }
    combine(&r)
}

/// The twelve row sums (h.hi, h.lo, g.hi, g.lo, k.hi, k.lo; I then Q each)
/// combined into the six banks: `hi·2⁹ + lo` in `i64`.
#[inline(always)]
fn combine(r: &[i32; 12]) -> [i64; 6] {
    let bank = |b: usize, iq: usize| {
        (i64::from(r[2 * (2 * b) + iq]) << 9) + i64::from(r[2 * (2 * b + 1) + iq])
    };
    [
        bank(0, 0),
        bank(0, 1),
        bank(1, 0),
        bank(1, 1),
        bank(2, 0),
        bank(2, 1),
    ]
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
fn load16(a: &[i16]) -> std::arch::x86_64::__m256i {
    let a: &[i16; 16] = a.try_into().expect("16 taps");
    std::arch::x86_64::_mm256_setr_epi16(
        a[0], a[1], a[2], a[3], a[4], a[5], a[6], a[7], a[8], a[9], a[10], a[11], a[12], a[13],
        a[14], a[15],
    )
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
fn load8(a: &[i16]) -> std::arch::x86_64::__m128i {
    let a: &[i16; 8] = a.try_into().expect("8 taps");
    std::arch::x86_64::_mm_setr_epi16(a[0], a[1], a[2], a[3], a[4], a[5], a[6], a[7])
}

/// The eight `i32` lanes of `v` folded to four (low half + high half).
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
fn fold256(v: std::arch::x86_64::__m256i) -> std::arch::x86_64::__m128i {
    use std::arch::x86_64::*;
    _mm_add_epi32(_mm256_castsi256_si128(v), _mm256_extracti128_si256::<1>(v))
}

/// The sum of four `i32` lanes.
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
fn hsum128(v: std::arch::x86_64::__m128i) -> i32 {
    use std::arch::x86_64::*;
    let v = _mm_add_epi32(v, _mm_shuffle_epi32::<0b01_00_11_10>(v));
    let v = _mm_add_epi32(v, _mm_shuffle_epi32::<0b10_11_00_01>(v));
    _mm_cvtsi128_si32(v)
}

#[cfg(all(target_arch = "x86_64", isdb_frontend_avx512))]
// only compiled when the build script found Rust ≥ 1.89 (AVX-512)
#[clippy::msrv = "1.89"]
#[target_feature(enable = "avx512f,avx512bw,avx512vl,avx2")]
fn load32(a: &[i16]) -> std::arch::x86_64::__m512i {
    use std::arch::x86_64::*;
    let (lo, hi) = (load16(&a[..16]), load16(&a[16..32]));
    _mm512_inserti64x4::<1>(_mm512_castsi256_si512(lo), hi)
}

/// The sixteen `i32` lanes of `v` folded to eight.
#[cfg(all(target_arch = "x86_64", isdb_frontend_avx512))]
// only compiled when the build script found Rust ≥ 1.89 (AVX-512)
#[clippy::msrv = "1.89"]
#[target_feature(enable = "avx512f,avx512bw,avx512vl,avx2")]
fn fold512(v: std::arch::x86_64::__m512i) -> std::arch::x86_64::__m256i {
    use std::arch::x86_64::*;
    _mm256_add_epi32(_mm512_castsi512_si256(v), _mm512_extracti64x4_epi64::<1>(v))
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
