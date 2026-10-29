//! The fixed-point transmit interpolator (`shape::fixed::Interpolator`,
//! TX format 1): it equals a plain reading of its specification sample
//! for sample, follows the f32 `Shaper`, is free of intersymbol
//! interference through an ideal matched filter, and feeds the real
//! receive front end (`fast::FastFrontend`, ISDB-S at 40 Msps) at the
//! output quantiser's floor; chunking is invisible, a stall inserts
//! exactly its zero symbols and counts them, an asymmetric bank is
//! refused. The code words (`txformat`) round-trip.

use isdb_frontend::fast::FastFrontend;
use isdb_frontend::metrics::psk_mer;
use isdb_frontend::params::{Design, Params};
use isdb_frontend::shape::Shaper;
use isdb_frontend::shape::fixed::{Interpolator, TxDesign, TxParamError, TxParams, TxSample};
use isdb_frontend::tables::rrc;
use isdb_frontend::txformat::{self, CodePacker};
use isdb_iq::survey::KNOWN_SYSTEMS;
use num_complex::{Complex32, Complex64};
use proptest::prelude::*;

/// ISDB-S symbol rate (BO.1408-1 Annex 1 Table 3) and roll-off (§9).
const RS: f64 = 28.86e6;
const ALPHA: f64 = 0.35;

/// The FPGA's parameters: ISDB-S at 40 Msps, 10 dB backoff, P 512, span
/// 12.
fn isdbs() -> TxParams {
    TxParams::design(&TxDesign::new(40e6, RS, ALPHA)).unwrap()
}

/// xorshift64.
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Self(seed | 1)
    }

    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    /// 0..n.
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

/// `n` random point codes (8PSK, 8..=15): the top three bits of a
/// xorshift64 as the angle index (the symbol sequence of
/// `examples/txinterp.rs`).
fn psk8_codes(n: usize, seed: u64) -> Vec<u8> {
    let mut r = Rng::new(seed);
    (0..n)
        .map(|_| txformat::point_code(r.next().to_le_bytes()[7] >> 5))
        .collect()
}

/// The model on `codes` (then zero words that flush the line), `n`
/// samples.
fn run(p: &TxParams, codes: &[u8], n: usize) -> Vec<[i16; 2]> {
    let mut m = Interpolator::new(p.clone()).unwrap();
    m.push_codes(codes);
    m.flush_codes();
    m.push_words(&vec![0; p.taps().div_ceil(8) + 1]);
    let mut y = Vec::new();
    m.process(n, &mut y);
    y
}

/// Samples whose carries enter `symbols` codes after START (the first
/// code enters at the first carry): ⌈symbols·2²⁴ / step⌉, the last of
/// which carries the last code.
fn samples_for(p: &TxParams, symbols: u64) -> usize {
    usize::try_from(((symbols << 24) - 1) / u64::from(p.step) + 1).unwrap()
}

/// What the plain reading computes for one sample.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Want {
    n: u64,
    frac: u32,
    q: usize,
    line: Vec<u8>,
    sum: [i64; 2],
    out: [i16; 2],
    code: Option<u8>,
}

/// The specification read literally (`shape::fixed` module
/// documentation, "Per output sample" and "Input"): the line a `Vec`
/// rotated per carry, the banks indexed as written, the unpack register a
/// list of codes, stalls `(word_index, n)`, a missing word a stall.
struct Plain {
    p: TxParams,
    frac: u32,
    line: Vec<u8>,
    reg: Vec<u8>,
    reg_missed: bool,
    words: Vec<u32>,
    next_word: usize,
    stalls: Vec<(u64, u32)>,
    consumed: u64,
    missed: u64,
    first_miss: Option<u64>,
    n: u64,
}

impl Plain {
    fn new(p: TxParams, words: Vec<u32>, stalls: Vec<(u64, u32)>) -> Self {
        let taps = p.taps();
        Self {
            p,
            frac: 0,
            line: vec![0; taps],
            reg: Vec::new(),
            reg_missed: false,
            words,
            next_word: 0,
            stalls,
            consumed: 0,
            missed: 0,
            first_miss: None,
            n: 0,
        }
    }

    fn sample(&mut self) -> Want {
        const SI: [i64; 8] = [1, 1, 0, -1, -1, -1, 0, 1];
        const SQ: [i64; 8] = [0, 1, 1, 1, 0, -1, -1, -1];
        let q = (self.frac >> (24 - self.p.phase_bits())) as usize;
        let (mut si, mut sq) = (0i64, 0i64);
        for (t, &c) in self.line.iter().enumerate() {
            if (8..=15).contains(&c) {
                let k = usize::from(c - 8);
                let v = if k % 2 == 1 {
                    self.p.b[q][t]
                } else {
                    self.p.a[q][t]
                };
                si += SI[k] * i64::from(v);
                sq += SQ[k] * i64::from(v);
            }
        }
        let s = u32::from(self.p.shift);
        let out = |v: i64| {
            let r = if s == 0 { v } else { (v + (1 << (s - 1))) >> s };
            i16::try_from(r.clamp(-2048, 2047)).unwrap()
        };
        let want = Want {
            n: self.n,
            frac: self.frac,
            q,
            line: self.line.clone(),
            sum: [si, sq],
            out: [out(si), out(sq)],
            code: None,
        };
        self.n += 1;
        self.frac += self.p.step;
        if self.frac < 1 << 24 {
            return want;
        }
        self.frac -= 1 << 24;
        if self.reg.is_empty() {
            let i = self.next_word as u64;
            let stall = self.stalls.iter_mut().find(|(at, n)| *at == i && *n > 0);
            let word = self.words.get(self.next_word).copied();
            match (stall, word) {
                (None, Some(w)) => {
                    self.next_word += 1;
                    self.reg = txformat::codes(w).to_vec();
                    self.reg_missed = false;
                }
                (stall, _) => {
                    if let Some((_, n)) = stall {
                        *n -= 1;
                    }
                    self.first_miss.get_or_insert(self.consumed);
                    self.reg = vec![0; 8];
                    self.reg_missed = true;
                }
            }
        }
        let c = self.reg.remove(0);
        if self.reg_missed {
            self.missed += 1;
        } else {
            self.consumed += 1;
        }
        self.line.rotate_right(1);
        self.line[0] = c;
        Want {
            code: Some(c),
            ..want
        }
    }
}

/// The model's sample against the plain reading's.
fn check(got: &TxSample, want: &Want) {
    let n = want.n;
    assert_eq!(got.n, n);
    assert_eq!(got.frac, want.frac, "n {n}");
    assert_eq!(usize::from(got.q), want.q, "n {n}");
    assert_eq!(got.line(), &want.line[..], "n {n}");
    assert_eq!(got.sum.map(i64::from), want.sum, "n {n}");
    assert_eq!(got.out, want.out, "n {n}");
    assert_eq!(got.code, want.code, "n {n}");
    assert_eq!(got.wrap, want.code.is_some(), "n {n}");
    assert_eq!(got.dac(), want.out.map(|v| v * 16), "n {n}");
}

/// Codes of every kind: points, zero and reserved codes.
fn mixed_codes(n: usize, seed: u64) -> Vec<u8> {
    let mut r = Rng::new(seed);
    (0..n)
        .map(|_| match r.below(20) {
            0 => 0,
            1 => 1 + u8::try_from(r.below(7)).unwrap(),
            _ => 8 + u8::try_from(r.below(8)).unwrap(),
        })
        .collect()
}

/// The model equals the plain reading on every field of every sample,
/// with stalls (one before the first word, random ones, one after the
/// last), every kind of code and the queue running dry at the end; at
/// three designs (the FPGA's, another rate with a lower backoff that
/// clips, a small odd geometry).
#[test]
fn model_is_the_specification() {
    let designs = [
        TxDesign::new(40e6, RS, ALPHA),
        TxDesign {
            backoff_db: 1.0,
            ..TxDesign::new(61.44e6, RS, ALPHA)
        },
        TxDesign {
            span: 3,
            phases: 16,
            ..TxDesign::new(57.72e6, RS, ALPHA)
        },
    ];
    for (k, d) in designs.iter().enumerate() {
        let p = TxParams::design(d).unwrap();
        let words = txformat::pack(&mixed_codes(3001, 77 + k as u64));
        let mut r = Rng::new(5 + k as u64);
        let mut stalls = vec![(0u64, 2u32)];
        for i in 1..words.len() as u64 {
            if r.below(15) == 0 {
                stalls.push((i, 1 + u32::try_from(r.below(4)).unwrap()));
            }
        }
        stalls.push((words.len() as u64, 3));
        let mut m = Interpolator::new(p.clone()).unwrap();
        let mut at = stalls.iter().peekable();
        for (i, &w) in (0u64..).zip(&words) {
            while let Some(&&(_, n)) = at.peek().filter(|s| s.0 == i) {
                m.stall(n);
                at.next();
            }
            m.push_words(&[w]);
        }
        for &(_, n) in at {
            m.stall(n);
        }
        let mut plain = Plain::new(p.clone(), words.clone(), stalls.clone());
        let stalled: u64 = stalls.iter().map(|&(_, n)| u64::from(n)).sum();
        let n = samples_for(&p, 8 * (words.len() as u64 + stalled + 5)) + 50;
        let mut clipped = 0;
        for _ in 0..n {
            let s = m.output();
            clipped += usize::from(s.out.iter().any(|&v| v == -2048 || v == 2047));
            check(&s, &plain.sample());
        }
        assert_eq!(m.consumed(), plain.consumed);
        assert_eq!(m.missed(), plain.missed);
        assert_eq!(m.first_miss(), plain.first_miss);
        assert_eq!(m.first_miss(), Some(0), "the first request stalls");
        assert_eq!(m.samples(), n as u64);
        assert_eq!(m.consumed(), 8 * words.len() as u64);
        assert!(
            m.missed() > 8 * stalled,
            "the queue ran dry after the last stall"
        );
        eprintln!(
            "design {k}: {n} samples equal the plain reading; {} consumed, {} missed, {clipped} clipped",
            m.consumed(),
            m.missed()
        );
        if k == 1 {
            assert!(clipped > 0, "1 dB backoff clips");
        } else {
            assert_eq!(clipped, 0);
        }
    }
}

/// The signal model: sample 0 is 0, the first code enters at the first
/// carry, and one unit point gives D·G·p(u − span − 1) (within the phase
/// grid and the rounding) on both components as its angle says.
#[test]
fn impulse_response_is_the_pulse_one_symbol_late() {
    let d = TxDesign::new(40e6, RS, ALPHA);
    let p = TxParams::design(&d).unwrap();
    let sh = Shaper::new(40e6, RS, ALPHA).unwrap().with_span(12).unwrap();
    let (g, dac, span) = (sh.gain(), d.dac_rms(), 12.0);
    for k in [0u8, 1, 2, 5] {
        let mut m = Interpolator::new(p.clone()).unwrap();
        m.push_codes(&[txformat::point_code(k), 0, 0, 0, 0, 0, 0, 0]);
        let first = m.output();
        assert_eq!((first.out, first.frac, first.code), ([0, 0], 0, None));
        let n = samples_for(&p, 30);
        let pt = txformat::point(txformat::point_code(k));
        for i in 1..n {
            let s = m.output();
            let u = i as f64 * f64::from(p.step) / f64::from(1u32 << 24);
            let want = pt * (dac * g * sh.pulse(u - span - 1.0));
            let err = (f64::from(s.out[0]) - want.re)
                .abs()
                .max((f64::from(s.out[1]) - want.im).abs());
            // the phase grid moves a sample by ≤ 1/1024 symbol: ≤ 1.2 LSB
            // at the pulse's steepest, plus the rounding
            assert!(err <= 2.0, "k {k} sample {i}: {:?} against {want}", s.out);
        }
        assert_eq!(m.consumed(), 8);
    }
}

/// The ISI through an ideal `f64` matched filter (the same pulse, span
/// 12) at the exact symbol instants of the realised rate, 20 000 random
/// 8PSK symbols, the best complex gain: MER ≥ 40 dB, and within 0.5 dB of
/// interpolator.md's 54.4 dB.
#[test]
fn model_is_isi_free() {
    let p = isdbs();
    let n = 20_000;
    let codes = psk8_codes(n, 0x9e37_79b9_7f4a_7c15);
    let y = run(&p, &codes, samples_for(&p, n as u64 + 30));
    let (sps, span) = (p.sps(), 12.0);
    let pulse = |x: f64| if x.abs() < span { rrc(x, ALPHA) } else { 0.0 };
    let (mut num, mut den) = (Complex64::new(0.0, 0.0), 0.0);
    let mut z = Vec::new();
    for (j, &c) in codes.iter().enumerate().take(n - 24).skip(24) {
        // code j peaks at u = j + span + 1
        let u = j as f64 + span + 1.0;
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let (i0, i1) = (
            ((u - span) * sps).ceil() as usize,
            ((u + span) * sps).floor() as usize,
        );
        let acc = (i0..=i1.min(y.len() - 1)).fold(Complex64::new(0.0, 0.0), |s, i| {
            let v = Complex64::new(f64::from(y[i][0]), f64::from(y[i][1]));
            s + v * pulse(i as f64 / sps - u)
        }) / sps;
        let a = txformat::point(c);
        num += acc * a.conj();
        den += a.norm_sqr();
        z.push((acc, a));
    }
    let g = num / den;
    let err: f64 = z.iter().map(|(z, a)| (z - g * a).norm_sqr()).sum();
    let mer = 10.0 * (g.norm_sqr() * den / err).log10();
    eprintln!(
        "ISI through an ideal matched filter: MER {mer:.2} dB, gain {:.2} LSB (D {:.2})",
        g.norm(),
        TxDesign::new(40e6, RS, ALPHA).dac_rms()
    );
    assert!(mer >= 40.0, "{mer:.2} dB");
    assert!((mer - 54.4).abs() <= 0.5, "{mer:.2} dB against 54.4");
}

/// Against the f32 `Shaper` at the same span: the shaper with one
/// leading zero symbol (the model's code j peaks one symbol later),
/// scaled by D, is the model's signal; the model's 12-bit samples differ
/// from it by the phase grid and the rounding only: SNR ≥ 40 dB.
#[test]
fn model_follows_the_shaper() {
    let d = TxDesign::new(40e6, RS, ALPHA);
    let p = TxParams::design(&d).unwrap();
    let n = 20_000;
    let codes = psk8_codes(n, 0x2545_f491_4f6c_dd1d);
    let mut sh = Shaper::new(40e6, RS, ALPHA).unwrap().with_span(12).unwrap();
    // unit points rounded to the shaper's f32 on purpose
    #[allow(clippy::cast_possible_truncation)]
    let pts: Vec<Complex32> = std::iter::once(Complex32::new(0.0, 0.0))
        .chain(codes.iter().map(|&c| {
            let v = txformat::point(c);
            Complex32::new(v.re as f32, v.im as f32)
        }))
        .collect();
    let mut want = Vec::new();
    sh.push(&pts, &mut want);
    sh.finish(&mut want);
    let y = run(&p, &codes, want.len());
    let (mut sig, mut err) = (0.0, 0.0);
    for (got, w) in y.iter().zip(&want) {
        let w = Complex64::new(f64::from(w.re), f64::from(w.im)) * d.dac_rms();
        let e = Complex64::new(f64::from(got[0]), f64::from(got[1])) - w;
        sig += w.norm_sqr();
        err += e.norm_sqr();
    }
    let snr = 10.0 * (sig / err).log10();
    let rms = (sig / want.len() as f64).sqrt();
    eprintln!(
        "against the f32 shaper (span 12): SNR {snr:.2} dB over {} samples, RMS {rms:.1} LSB",
        want.len()
    );
    assert!(snr >= 40.0, "{snr:.2} dB");
    assert!((rms / d.dac_rms() - 1.0).abs() < 0.01, "RMS {rms}");
}

/// Through the real receive front end, ISDB-S at 40 Msps as the receivers
/// design it (trim 0): 200 000 random 8PSK symbols, the model's 12-bit
/// integers straight in (the digital loopback). The 8PSK MER of the
/// 6-bit output over the second half, in the steady state (the receiver's
/// outputs over the zero tail after the last symbol excluded: `psk_mer`
/// would count them as errors, `examples/txloop.rs`), is ≥ 36.0 dB
/// (interpolator.md §6; 36.96 dB measured, the f32 shaper 36.98 dB).
#[test]
fn front_end_recovers_the_model_samples() {
    let p = isdbs();
    let n = 200_000;
    let codes = psk8_codes(n, 0x0123_4567_89ab_cdef);
    let x = run(&p, &codes, samples_for(&p, n as u64 + 30));
    let d = Design::for_system(&KNOWN_SYSTEMS[0], 40e6);
    assert_eq!((d.symbol_rate, d.roll_off, d.trim), (RS, ALPHA, 0.0));
    let mut fe = FastFrontend::new(Params::design(&d).unwrap()).unwrap();
    let mut out = Vec::new();
    fe.process(&x, &mut out);
    // the last 64 outputs: the tail and the receive filter's delay
    out.truncate(out.len() - 64);
    let half = out.len() / 2;
    let q: Vec<[f64; 2]> = out.iter().map(|s| s.q.map(f64::from)).collect();
    let z: Vec<[f64; 2]> = out.iter().map(|s| s.zk.map(f64::from)).collect();
    let mq = psk_mer(&q, 8, 1e-3, half);
    let mz = psk_mer(&z, 8, 1e-3, half);
    let peak = x.iter().flatten().map(|v| v.unsigned_abs()).max().unwrap();
    eprintln!(
        "front end on the model's samples: {} symbols, 8PSK MER {:.2} dB at the 6-bit output, {:.2} dB before the quantiser; peak component {peak} LSB",
        out.len(),
        mq.mer_db,
        mz.mer_db
    );
    assert!(mq.mer_db >= 36.0, "{mq:?}");
    assert!(peak <= 1042, "peak {peak}");
}

/// A stall of n requests before word i is exactly 8·n zero symbols before
/// it: the samples equal those of the stream with n zero words inserted
/// there, CONSUMED counts the delivered codes, MISSED the stalls' zero
/// symbols, FIRST_MISS the codes before the first stall, and the sticky
/// flag is set.
#[test]
fn stalls_insert_exactly_their_zero_symbols() {
    let p = isdbs();
    for seed in 1..6u64 {
        let mut r = Rng::new(seed);
        let words = txformat::pack(&mixed_codes(2000, seed));
        let mut stalls: Vec<(u64, u32)> = Vec::new();
        for i in 0..words.len() as u64 {
            if r.below(12) == 0 {
                stalls.push((i, 1 + u32::try_from(r.below(5)).unwrap()));
            }
        }
        if seed == 3 {
            stalls = vec![(100, 64)]; // one long stall: 512 symbols
        }
        if stalls.is_empty() {
            stalls.push((7, 1));
        }
        let mut stalled = Interpolator::new(p.clone()).unwrap();
        let mut zeroed = Interpolator::new(p.clone()).unwrap();
        for (i, &w) in (0u64..).zip(&words) {
            if let Some(&(_, n)) = stalls.iter().find(|s| s.0 == i) {
                stalled.stall(n);
                zeroed.push_words(&vec![0; n as usize]);
            }
            stalled.push_words(&[w]);
            zeroed.push_words(&[w]);
        }
        let total: u64 = stalls.iter().map(|&(_, n)| u64::from(n)).sum();
        let samples = samples_for(&p, 8 * (words.len() as u64 + total));
        for k in 0..samples {
            let (a, b) = (stalled.output(), zeroed.output());
            assert_eq!(a, b, "seed {seed} sample {k}");
        }
        assert!(stalled.input_done() && zeroed.input_done());
        assert_eq!(stalled.consumed(), 8 * words.len() as u64, "seed {seed}");
        assert_eq!(stalled.missed(), 8 * total, "seed {seed}");
        assert_eq!(stalled.first_miss(), Some(8 * stalls[0].0), "seed {seed}");
        assert!(stalled.underrun());
        assert_eq!(zeroed.consumed(), 8 * (words.len() as u64 + total));
        assert_eq!((zeroed.missed(), zeroed.first_miss()), (0, None));
        assert!(!zeroed.underrun());
        assert_eq!(stalled.samples(), samples as u64);
    }
}

/// A dry queue is a stall: after the last word every request enters
/// eight zero symbols, the output decays to exactly 0 within 2·span
/// symbols, and START clears the counters and the state but keeps the
/// queued words.
#[test]
fn a_dry_queue_is_silence_and_start_resets() {
    let p = isdbs();
    let mut m = Interpolator::new(p.clone()).unwrap();
    let codes = psk8_codes(80, 9);
    m.push_codes(&codes);
    let n = samples_for(&p, 80 + 24);
    let mut y = Vec::new();
    m.process(n, &mut y);
    assert_eq!((m.consumed(), m.first_miss()), (80, Some(80)));
    assert_eq!(m.missed(), 24);
    let mut rest = Vec::new();
    m.process(200, &mut rest);
    assert!(rest.iter().all(|v| *v == [0, 0]));
    assert!(y.iter().any(|v| *v != [0, 0]));
    m.push_codes(&codes);
    m.start();
    assert_eq!(
        (m.consumed(), m.missed(), m.first_miss(), m.samples()),
        (0, 0, None, 0)
    );
    assert_eq!(m.queued(), 10);
    let mut again = Vec::new();
    m.process(n, &mut again);
    assert_eq!(again, y, "after START: as a fresh interpolator");
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(48))]

    /// Any cut of the codes into `push_codes` calls, with samples taken
    /// between them while the queue holds a word, gives the samples of one
    /// `push_words` of the packed stream, bit for bit, and the same
    /// counters.
    #[test]
    fn prop_chunking_is_invisible(
        codes in prop::collection::vec(0u8..16, 0..900),
        cuts in prop::collection::vec(0usize..900, 0..12),
        takes in prop::collection::vec(0usize..200, 12),
        fs in prop::sample::select(vec![40e6, 57.72e6, 61.44e6]),
    ) {
        let p = TxParams::design(&TxDesign::new(fs, RS, ALPHA)).unwrap();
        let total = samples_for(&p, codes.len().div_ceil(8) as u64 * 8 + 1);
        let mut one = Interpolator::new(p.clone()).unwrap();
        one.push_words(&txformat::pack(&codes));
        let mut want = Vec::new();
        one.process(total, &mut want);
        let mut cuts: Vec<usize> = cuts.into_iter().map(|c| c.min(codes.len())).collect();
        cuts.sort_unstable();
        let mut cut = Interpolator::new(p.clone()).unwrap();
        let mut got = Vec::new();
        let mut from = 0;
        for (k, c) in cuts.into_iter().chain([codes.len()]).enumerate() {
            cut.push_codes(&codes[from..c]);
            from = c;
            // take samples only while a word waits: never a stall
            for _ in 0..takes[k % takes.len()] {
                if cut.queued() == 0 && cut.next_requests() {
                    break;
                }
                let mut y = Vec::new();
                cut.process(1, &mut y);
                got.extend(y);
            }
        }
        cut.flush_codes();
        let rest = total - got.len();
        cut.process(rest, &mut got);
        prop_assert_eq!(&got, &want);
        prop_assert_eq!(
            (cut.consumed(), cut.missed(), cut.first_miss(), cut.samples()),
            (one.consumed(), one.missed(), one.first_miss(), one.samples())
        );
    }

    /// Packing round trip: codes → words → codes (padded with zero
    /// codes to whole words), through the DMA's i16 channel words too, and
    /// any cut of the packer's input gives the same words.
    #[test]
    fn prop_pack_unpack_round_trip(
        codes in prop::collection::vec(0u8..16, 0..300),
        cuts in prop::collection::vec(0usize..300, 0..8),
    ) {
        let words = txformat::pack(&codes);
        prop_assert_eq!(words.len(), codes.len().div_ceil(8));
        let back = txformat::unpack(&words);
        prop_assert_eq!(&back[..codes.len()], &codes[..]);
        prop_assert!(back[codes.len()..].iter().all(|&c| c == txformat::ZERO));
        let mut dma = Vec::new();
        txformat::to_dma(&words, &mut dma);
        let again: Vec<u32> = dma
            .as_chunks::<2>()
            .0
            .iter()
            .map(|&c| txformat::from_components(c))
            .collect();
        prop_assert_eq!(&again, &words);
        let mut cuts: Vec<usize> = cuts.into_iter().map(|c| c.min(codes.len())).collect();
        cuts.sort_unstable();
        let mut pk = CodePacker::new();
        let mut cut = Vec::new();
        let mut from = 0;
        for c in cuts.into_iter().chain([codes.len()]) {
            pk.push(&codes[from..c], &mut cut);
            from = c;
        }
        pk.finish(&mut cut);
        prop_assert_eq!(&cut, &words);
    }
}

/// The banks are exactly symmetric, `a[q][t] == a[P−1−q][2S−1−t]` (and
/// b), and b is a/√2 within the rounding; an asymmetric bank (one LSB in
/// a, in b, or in the far tap of a pair), a coefficient beyond 18 bits,
/// a ragged row and out-of-range registers are refused, by `validate` and
/// by `Interpolator::new`.
#[test]
fn symmetry_is_enforced() {
    let p = isdbs();
    let (rows, taps) = (p.phases(), p.taps());
    for bank in [&p.a, &p.b] {
        for q in 0..rows {
            for t in 0..taps {
                assert_eq!(bank[q][t], bank[rows - 1 - q][taps - 1 - t]);
            }
        }
    }
    for q in 0..rows {
        for t in 0..taps {
            let want = f64::from(p.a[q][t]) / std::f64::consts::SQRT_2;
            assert!((f64::from(p.b[q][t]) - want).abs() <= 1.0);
        }
    }
    let refuse = |f: &dyn Fn(&mut TxParams), want: &dyn Fn(&TxParamError) -> bool| {
        let mut bad = p.clone();
        f(&mut bad);
        let e = bad.validate().unwrap_err();
        assert!(want(&e), "{e}");
        assert_eq!(Interpolator::new(bad).unwrap_err(), e);
    };
    refuse(&|x| x.a[3][5] += 1, &|e| {
        matches!(
            e,
            TxParamError::Asymmetric {
                bank: "a",
                row: 3,
                tap: 5,
                mirror_row: 508,
                mirror_tap: 18,
                ..
            }
        )
    });
    refuse(&|x| x.b[400][0] -= 1, &|e| {
        matches!(
            e,
            TxParamError::Asymmetric {
                bank: "b",
                row: 111,
                tap: 23,
                ..
            }
        )
    });
    refuse(&|x| x.a[0][23] += 1, &|e| {
        matches!(
            e,
            TxParamError::Asymmetric {
                bank: "a",
                row: 0,
                tap: 23,
                mirror_row: 511,
                mirror_tap: 0,
                ..
            }
        )
    });
    refuse(
        &|x| {
            x.a[7][2] = 1 << 17;
            x.a[504][21] = 1 << 17;
        },
        &|e| {
            matches!(
                e,
                TxParamError::Coefficient {
                    bank: "a",
                    row: 7,
                    tap: 2,
                    value: 131_072
                }
            )
        },
    );
    refuse(&|x| x.b[9].push(0), &|e| {
        matches!(
            e,
            TxParamError::RaggedBank {
                bank: "b",
                row: 9,
                len: 25,
                taps: 24
            }
        )
    });
    refuse(&|x| x.step = 0, &|e| *e == TxParamError::Step(0));
    refuse(&|x| x.step = 1 << 24, &|e| {
        *e == TxParamError::Step(1 << 24)
    });
    refuse(&|x| x.shift = 24, &|e| *e == TxParamError::Shift(24));
    refuse(
        &|x| {
            x.b.pop();
        },
        &|e| matches!(e, TxParamError::BankRows { a: 512, b: 511 }),
    );
    refuse(
        &|x| {
            x.a.truncate(384);
            x.b.truncate(384);
        },
        &|e| matches!(e, TxParamError::BankRows { a: 384, b: 384 }),
    );
    assert_eq!(p.validate(), Ok(()));
}
