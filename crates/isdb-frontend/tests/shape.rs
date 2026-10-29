//! The transmit pulse shaper (`shape::Shaper`): chunking is invisible,
//! the signal model (delay, scale, sample counts, tail) holds exactly, the
//! shaped signal is free of intersymbol interference through an ideal
//! matched filter at every sample rate the radios use, its RMS is 1, and
//! the real receive front end (`fast::FastFrontend`, ISDB-S at 40 Msps)
//! recovers its symbols at the output quantiser's floor.

use isdb_frontend::fast::FastFrontend;
use isdb_frontend::metrics::psk_mer;
use isdb_frontend::params::{Design, Params};
use isdb_frontend::shape::{DEFAULT_SPAN, MAX_SPAN, MAX_SPS, ShapeError, Shaper};
use isdb_iq::survey::KNOWN_SYSTEMS;
use num_complex::{Complex32, Complex64};
use proptest::prelude::*;
use std::f64::consts::TAU;

/// ISDB-S symbol rate (BO.1408-1 Annex 1 Table 3) and roll-off (§9).
const RS: f64 = 28.86e6;
const ALPHA: f64 = 0.35;

/// The sample rates the radios use: 40 Msps (1.386 samples per symbol),
/// 57.72 Msps (exactly 2) and 61.44 Msps (the Pluto's highest, 2.129).
const RATES: [f64; 3] = [40e6, 57.72e6, 61.44e6];

/// More rates for the ISI and count tests: (1 + α)·R itself, a rate just
/// above it and 3.465 samples per symbol.
const MORE_RATES: [f64; 3] = [1.35 * RS, 39e6, 100e6];

/// Random 8PSK symbols (unit modulus) from a xorshift64.
fn psk8(n: usize, seed: u64) -> Vec<Complex32> {
    let mut s = seed | 1;
    (0..n)
        .map(|_| {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            let k = u8::try_from(s >> 61).unwrap();
            let a = TAU * f64::from(k) / 8.0;
            // Unit-modulus points rounded to f32 on purpose.
            #[allow(clippy::cast_possible_truncation)]
            let v = Complex32::new(a.cos() as f32, a.sin() as f32);
            v
        })
        .collect()
}

/// One stream: `syms` through `push` in pieces cut at `cuts`, then
/// `finish`.
fn shape_cut(sh: &mut Shaper, syms: &[Complex32], cuts: &[usize]) -> Vec<Complex32> {
    let mut cuts: Vec<usize> = cuts.iter().map(|&c| c.min(syms.len())).collect();
    cuts.sort_unstable();
    let mut out = Vec::new();
    let mut from = 0;
    for c in cuts.into_iter().chain([syms.len()]) {
        sh.push(&syms[from..c], &mut out);
        from = c;
    }
    sh.finish(&mut out);
    out
}

fn bits(x: &[Complex32]) -> Vec<(u32, u32)> {
    x.iter().map(|v| (v.re.to_bits(), v.im.to_bits())).collect()
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    /// Any chunking of the stream gives the same samples, bit for bit, as
    /// one `push`, tail included; and a second stream after `finish` is
    /// the same as on a fresh shaper.
    #[test]
    fn prop_chunking_is_invisible(
        fs in prop::sample::select(RATES.into_iter().chain(MORE_RATES).collect::<Vec<_>>()),
        span in 1usize..=24,
        syms in prop::collection::vec((-2.0f32..2.0, -2.0f32..2.0), 0..700),
        cuts in prop::collection::vec(0usize..700, 0..12),
    ) {
        let syms: Vec<Complex32> = syms.into_iter().map(|(a, b)| Complex32::new(a, b)).collect();
        let mut one = Shaper::new(fs, RS, ALPHA).unwrap().with_span(span).unwrap();
        let mut cut = one.clone();
        let want = shape_cut(&mut one, &syms, &[]);
        let got = shape_cut(&mut cut, &syms, &cuts);
        prop_assert_eq!(bits(&want), bits(&got));
        // A new stream on the same shapers: as on a fresh one.
        let mut fresh = Shaper::new(fs, RS, ALPHA).unwrap().with_span(span).unwrap();
        let again = shape_cut(&mut cut, &syms[syms.len() / 2..], &cuts);
        prop_assert_eq!(bits(&again), bits(&shape_cut(&mut fresh, &syms[syms.len() / 2..], &[])));
    }
}

/// The signal model: one unit symbol gives `G·p(n/sps − span)` (the
/// nearest phase moves it by at most 1/2 048 symbol), peaking at sample
/// span·sps.
#[test]
fn impulse_response_is_the_delayed_pulse() {
    for fs in RATES {
        let mut sh = Shaper::new(fs, RS, ALPHA).unwrap();
        let (sps, span) = (sh.sps(), sh.span() as f64);
        let mut y = Vec::new();
        sh.push(&[Complex32::new(1.0, 0.0)], &mut y);
        sh.finish(&mut y);
        assert_eq!(
            y.len() as u64,
            sh.samples_for(2 * DEFAULT_SPAN as u64),
            "{fs}: one symbol and its tail"
        );
        for (n, v) in y.iter().enumerate() {
            let want = sh.gain() * sh.pulse(n as f64 / sps - span);
            assert!(
                (f64::from(v.re) - want).abs() < 2e-3 && v.im == 0.0,
                "{fs}: sample {n}: {v} against {want}"
            );
        }
        let peak = (0..y.len())
            .max_by(|&a, &b| y[a].re.total_cmp(&y[b].re))
            .unwrap();
        assert!((peak as f64 - span * sps).abs() <= 0.5, "{fs}: peak {peak}");
    }
}

/// Random 8PSK through the shaper, then an ideal matched filter in `f64`
/// (the same pulse) at the exact symbol instants: MER ≥ 40 dB.
#[test]
fn shaped_signal_is_isi_free() {
    let n = 6000;
    let a = psk8(n, 0x9e37_79b9_7f4a_7c15);
    for fs in RATES.into_iter().chain(MORE_RATES) {
        let mut sh = Shaper::new(fs, RS, ALPHA).unwrap();
        let (sps, span) = (sh.sps(), sh.span());
        let mut y = Vec::new();
        sh.push(&a, &mut y);
        sh.finish(&mut y);
        // Symbol k peaks at u = k + span symbols, sample u·sps; the filter
        // is Σ_i y[i]·p(i/sps − u) / sps over the pulse's support.
        let mut z = Vec::new();
        for (k, &ak) in a.iter().enumerate().take(n - 2 * span).skip(2 * span) {
            let u = (k + span) as f64;
            // In range: the support's samples lie in 0..y.len().
            #[allow(clippy::cast_possible_truncation)]
            let (i0, i1) = (
                ((u - span as f64) * sps).ceil() as usize,
                ((u + span as f64) * sps).floor() as usize,
            );
            let acc = (i0..=i1.min(y.len() - 1)).fold(Complex64::new(0.0, 0.0), |s, i| {
                let v = y[i];
                s + Complex64::new(f64::from(v.re), f64::from(v.im)) * sh.pulse(i as f64 / sps - u)
            });
            z.push((acc / sps, ak));
        }
        // The best complex gain, then the error power.
        let (num, den) = z
            .iter()
            .fold((Complex64::new(0.0, 0.0), 0.0), |(c, p), (z, a)| {
                let a = Complex64::new(f64::from(a.re), f64::from(a.im));
                (c + z * a.conj(), p + a.norm_sqr())
            });
        let c = num / den;
        let err: f64 = z
            .iter()
            .map(|(z, a)| (z - c * Complex64::new(f64::from(a.re), f64::from(a.im))).norm_sqr())
            .sum();
        let mer = 10.0 * (c.norm_sqr() * den / err).log10();
        eprintln!(
            "ISI-free: fs {:.2} MHz ({sps:.4} sps): MER {mer:.2} dB, gain {:.5}",
            fs / 1e6,
            c.norm()
        );
        assert!(mer >= 40.0, "fs {fs}: MER {mer:.2} dB");
    }
}

/// Unit-modulus random symbols give an output RMS of 1 (away from the
/// stream's start, where the delay line fills), within 10⁻⁴: the products
/// of different symbols average out with the Nyquist cascade's weights
/// (module documentation, "Scaling"), not as 1/√N.
#[test]
fn output_rms_is_one() {
    for fs in RATES {
        let mut sh = Shaper::new(fs, RS, ALPHA).unwrap();
        let mut y = Vec::new();
        sh.push(&psk8(200_000, 0x2545_f491_4f6c_dd1d), &mut y);
        let skip = usize::try_from(sh.samples_for(2 * DEFAULT_SPAN as u64)).unwrap();
        let p = y[skip..]
            .iter()
            .map(|v| f64::from(v.norm_sqr()))
            .sum::<f64>()
            / (y.len() - skip) as f64;
        let rms = p.sqrt();
        eprintln!("RMS at {:.2} MHz: {rms:.7}", fs / 1e6);
        assert!((rms - 1.0).abs() < 1e-4, "fs {fs}: RMS {rms}");
        // Scale in, scale out.
        let mut small = Shaper::new(fs, RS, ALPHA).unwrap();
        let mut ys = Vec::new();
        let syms: Vec<Complex32> = psk8(1000, 3).iter().map(|v| v * 0.25).collect();
        small.push(&syms, &mut ys);
        let mut unit = Shaper::new(fs, RS, ALPHA).unwrap();
        let mut yu = Vec::new();
        unit.push(&psk8(1000, 3), &mut yu);
        assert!(
            ys.iter()
                .zip(&yu)
                .all(|(s, u)| (s - u * 0.25).norm() < 1e-6)
        );
    }
}

/// `finish` appends the samples of 2·span − 1 zero symbols (after which
/// every sample is exactly zero: nothing of the pulses is left), then a
/// new `push` starts a stream identical to a fresh shaper's.
#[test]
fn finish_emits_the_tail_and_resets() {
    for fs in RATES {
        for span in [1, 4, 16] {
            let syms = psk8(500, 77);
            let fresh = Shaper::new(fs, RS, ALPHA).unwrap().with_span(span).unwrap();
            let mut sh = fresh.clone();
            let mut y = Vec::new();
            sh.push(&syms, &mut y);
            let body = y.len();
            sh.finish(&mut y);
            let n = syms.len() as u64;
            assert_eq!(y.len() as u64, sh.samples_for(n + 2 * span as u64 - 1));
            assert!(y.len() > body);
            assert_eq!((sh.symbols(), sh.samples()), (0, 0));
            // The tail equals pushing 2·span − 1 zeros, and nothing follows.
            let mut z = fresh.clone();
            let mut w = Vec::new();
            z.push(&syms, &mut w);
            z.push(&vec![Complex32::new(0.0, 0.0); 2 * span - 1], &mut w);
            assert_eq!(bits(&w), bits(&y), "fs {fs} span {span}");
            let mut rest = Vec::new();
            z.push(&[Complex32::new(0.0, 0.0); 40], &mut rest);
            assert!(rest.iter().all(|v| v.re == 0.0 && v.im == 0.0));
            assert_ne!(y.last().unwrap(), &Complex32::new(0.0, 0.0));
            // After finish: a fresh stream.
            let other = psk8(300, 5);
            let mut a = Vec::new();
            sh.push(&other, &mut a);
            let mut b = Vec::new();
            fresh.clone().push(&other, &mut b);
            assert_eq!(bits(&a), bits(&b));
        }
        // An empty stream has no tail.
        let mut sh = Shaper::new(fs, RS, ALPHA).unwrap();
        let mut y = Vec::new();
        sh.finish(&mut y);
        assert!(y.is_empty());
    }
}

/// `samples_for` is what `push` emits, for any length and cut.
#[test]
fn samples_for_matches_push() {
    for fs in RATES.into_iter().chain(MORE_RATES) {
        let mut sh = Shaper::new(fs, RS, ALPHA).unwrap();
        let syms = psk8(5000, 11);
        let mut y = Vec::new();
        for (k, s) in syms.chunks(37).enumerate() {
            sh.push(s, &mut y);
            let pushed = ((k + 1) * 37).min(syms.len()) as u64;
            assert_eq!(y.len() as u64, sh.samples_for(pushed), "fs {fs}");
            assert_eq!(sh.samples(), y.len() as u64);
        }
        assert_eq!(sh.samples_for(0), 0);
        // ⌈n·sps⌉ (the exact rate is within 2⁻⁴⁸ of sps).
        let n = 1_000_003u64;
        let approx = (n as f64 * sh.sps()).ceil();
        assert!((sh.samples_for(n) as f64 - approx).abs() <= 1.0, "fs {fs}");
    }
    // ISDB-S at 40 Msps: 442 810.81 samples per superframe of 319 488
    // symbols, so superframes take 442 810 or 442 811, and they add up.
    let sh = Shaper::new(40e6, RS, ALPHA).unwrap();
    let per: Vec<u64> = (0..6)
        .map(|k| sh.samples_for((k + 1) * 319_488) - sh.samples_for(k * 319_488))
        .collect();
    assert!(per.iter().all(|&p| p == 442_810 || p == 442_811), "{per:?}");
    assert!(per.contains(&442_810) && per.contains(&442_811), "{per:?}");
    assert_eq!(per.iter().sum::<u64>(), sh.samples_for(6 * 319_488));
}

/// Sample rates below (1 + α)·R, non-finite or non-positive rates,
/// roll-offs outside 0 < α ≤ 1 and spans outside 1..=MAX_SPAN are
/// refused; (1 + α)·R itself is accepted.
#[test]
fn rate_below_nyquist_is_refused() {
    let rate = |fs: f64, rs: f64| {
        matches!(
            Shaper::new(fs, rs, ALPHA),
            Err(ShapeError::Rate { sample_rate, symbol_rate, roll_off })
                if (sample_rate.to_bits(), symbol_rate.to_bits(), roll_off) == (fs.to_bits(), rs.to_bits(), ALPHA)
        )
    };
    assert!(rate(38e6, RS), "1.317 samples per symbol");
    assert!(rate(36e6, RS));
    assert!(rate(RS, RS));
    for bad in [0.0, -40e6, f64::NAN, f64::INFINITY] {
        assert!(rate(bad, RS), "{bad}");
        assert!(rate(40e6, bad), "{bad}");
    }
    assert!(rate(MAX_SPS * 2.0 * RS, RS));
    assert!(Shaper::new(1.35 * RS, RS, ALPHA).is_ok());
    assert!(Shaper::new(38.961e6, RS, ALPHA).is_ok());
    assert!(Shaper::new(MAX_SPS * RS, RS, ALPHA).is_ok());
    for bad in [0.0, -0.1, 1.5, f64::NAN] {
        assert!(
            matches!(Shaper::new(40e6, RS, bad), Err(ShapeError::RollOff(r)) if r.to_bits() == bad.to_bits()),
            "{bad}"
        );
    }
    let sh = Shaper::new(40e6, RS, ALPHA).unwrap();
    assert_eq!(sh.clone().with_span(0).unwrap_err(), ShapeError::Span(0));
    assert_eq!(
        sh.clone().with_span(MAX_SPAN + 1).unwrap_err(),
        ShapeError::Span(MAX_SPAN + 1)
    );
    assert_eq!(sh.with_span(MAX_SPAN).unwrap().span(), MAX_SPAN);
    let msg = Shaper::new(38e6, RS, ALPHA).unwrap_err().to_string();
    assert!(msg.contains("roll-off"), "{msg}");
}

/// Through the real receive front end: ISDB-S at 40 Msps (the receivers'
/// design, trim 0) on the shaped signal at RMS 0.3 of full scale,
/// quantised to the 12-bit input. The 8PSK MER of the output symbols
/// after the first half is bounded by the 6-bit output quantiser.
#[test]
fn front_end_recovers_the_shaped_symbols() {
    let fs = 40e6;
    let d = Design::for_system(&KNOWN_SYSTEMS[0], fs);
    assert_eq!((d.symbol_rate, d.roll_off, d.trim), (RS, ALPHA, 0.0));
    let p = Params::design(&d).unwrap();
    let mut sh = Shaper::new(fs, RS, ALPHA).unwrap();
    let mut y = Vec::new();
    sh.push(&psk8(200_000, 0x0123_4567_89ab_cdef), &mut y);
    sh.finish(&mut y);
    let x: Vec<[i16; 2]> = y
        .iter()
        .map(|&v| isdb_frontend::input_sample(v * 0.3))
        .collect();
    let mut fe = FastFrontend::new(p).unwrap();
    let mut out = Vec::new();
    fe.process(&x, &mut out);
    let half = out.len() / 2;
    let q: Vec<[f64; 2]> = out
        .iter()
        .map(|s| [f64::from(s.q[0]), f64::from(s.q[1])])
        .collect();
    let z: Vec<[f64; 2]> = out
        .iter()
        .map(|s| [f64::from(s.zk[0]), f64::from(s.zk[1])])
        .collect();
    let mq = psk_mer(&q, 8, 1e-3, half);
    let mz = psk_mer(&z, 8, 1e-3, half);
    eprintln!(
        "front end on the shaped signal: {} symbols, 8PSK MER {:.2} dB at the 6-bit output, {:.2} dB before the quantiser",
        out.len(),
        mq.mer_db,
        mz.mer_db
    );
    assert!(mq.mer_db >= 25.0, "{mq:?}");
}
