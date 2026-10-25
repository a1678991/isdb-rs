//! `fast::FastFrontend` equals `fixed::Frontend` symbol for symbol (every
//! field of every `Symbol`): on random parameters and inputs, including
//! full-range `i16` components (the `i64` kernel), chunked arbitrarily,
//! and on recordings when `ISDB_RS_ISDBS3_IQ` / `ISDB_RS_ISDBS_IQ` name
//! them; for every instruction set this host runs (`simd::Isa`: the
//! baseline, AVX2, AVX-512), through both `process` and `process_each`.

use isdb_frontend::fast::FastFrontend;
use isdb_frontend::fixed::{Frontend, Symbol};
use isdb_frontend::params::{Design, Params};
use isdb_frontend::simd::Isa;
use proptest::prelude::*;

fn reference(p: &Params, x: &[[i16; 2]]) -> Vec<Symbol> {
    let mut f = Frontend::new(p.clone()).unwrap();
    let mut out = Vec::new();
    f.process(x, &mut out);
    out
}

/// The fast front end on instruction set `isa` (which must be available:
/// the caller loops over [`isas`]).
fn fast_isa(p: &Params, x: &[[i16; 2]], chunk: usize, isa: Isa) -> Vec<Symbol> {
    let mut f = FastFrontend::with_isa(p.clone(), isa).unwrap();
    assert_eq!(f.isa(), isa, "{isa} not available");
    let mut out = Vec::new();
    for c in x.chunks(chunk.max(1)) {
        f.process(c, &mut out);
    }
    out
}

/// The same through `process_each`.
fn fast_each(p: &Params, x: &[[i16; 2]], chunk: usize, isa: Isa) -> Vec<Symbol> {
    let mut f = FastFrontend::with_isa(p.clone(), isa).unwrap();
    let mut out = Vec::new();
    for c in x.chunks(chunk.max(1)) {
        f.process_each(c, |s| out.push(*s));
    }
    out
}

/// The instruction sets this host runs (the baseline at least).
fn isas() -> Vec<Isa> {
    Isa::ALL.into_iter().filter(|i| i.available()).collect()
}

/// A noisy pulse train with a carrier offset, `amp` peak per component.
fn signal(n: usize, seed: u32, amp: f64, sps: f64) -> Vec<[i16; 2]> {
    let mut s = seed | 1;
    let mut rnd = || {
        s = s.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        f64::from(s >> 8) / f64::from(1u32 << 24) - 0.5
    };
    let mut sym = (1.0, 1.0);
    (0..n)
        .map(|i| {
            if (i as f64 / sps).fract() < 1.0 / sps {
                sym = (rnd().signum(), rnd().signum());
            }
            let ph = i as f64 * 0.013;
            let (c, si) = (ph.cos(), ph.sin());
            let (a, b) = (sym.0 * 0.6 + rnd() * 0.3, sym.1 * 0.6 + rnd() * 0.3);
            [
                ((a * c - b * si) * amp).round().clamp(-32768.0, 32767.0) as i16,
                ((a * si + b * c) * amp).round().clamp(-32768.0, 32767.0) as i16,
            ]
        })
        .collect()
}

fn params(
    s3: bool,
    taps: usize,
    trim: f64,
    cfo: f64,
    bits: u8,
    loop_bw: f64,
    fs: f64,
) -> Option<Params> {
    let (rs, alpha) = if s3 {
        (33.7561e6, 0.03)
    } else {
        (28.86e6, 0.35)
    };
    let mut d = Design::new(fs, rs, alpha);
    d.taps = taps;
    d.trim = trim;
    d.carrier_offset = cfo;
    d.bits = bits;
    d.loop_bandwidth = loop_bw;
    Params::design(&d).ok()
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(48))]

    #[test]
    fn prop_fast_equals_reference(
        s3 in any::<bool>(),
        taps in (2usize..=32).prop_map(|t| 2 * t),
        trim in -0.25f64..0.1,
        cfo in -400e3f64..400e3,
        bits in 2u8..=8,
        loop_bw in 1e-5f64..2e-3,
        fs in prop::sample::select(vec![36e6, 40e6, 57.72e6, 61.44e6]),
        amp in prop::sample::select(vec![300.0, 1500.0, 2047.0, 4095.0, 9000.0, 32767.0]),
        chunk in 1usize..5000,
        seed in any::<u32>(),
    ) {
        let Some(p) = params(s3, taps, trim, cfo, bits, loop_bw, fs) else {
            return Ok(());
        };
        let x = signal(12_000, seed, amp, fs / if s3 { 33.7561e6 } else { 28.86e6 });
        let a = reference(&p, &x);
        for isa in isas() {
            for (how, b) in [("process", fast_isa(&p, &x, chunk, isa)), ("process_each", fast_each(&p, &x, chunk, isa))] {
                prop_assert_eq!(a.len(), b.len(), "{} {}", isa, how);
                for (i, (r, f)) in a.iter().zip(&b).enumerate() {
                    prop_assert_eq!(r, f, "{} {}: symbol {}", isa, how, i);
                }
            }
        }
    }
}

#[test]
fn process_each_equals_process() {
    let p = params(false, 24, -0.08, 90e3, 6, 2e-4, 57.72e6).unwrap();
    let x = signal(30_000, 11, 2047.0, 57.72e6 / 28.86e6);
    let mut f = FastFrontend::new(p.clone()).unwrap();
    let mut b = Vec::new();
    for c in x.chunks(777) {
        f.process_each(c, |s| b.push(*s));
    }
    assert_eq!(reference(&p, &x), b);
}

#[test]
fn push_one_by_one_equals_process() {
    let p = params(true, 32, -0.17, -130e3, 6, 5e-6, 40e6).unwrap();
    let x = signal(20_000, 7, 2047.0, 40e6 / 33.7561e6);
    let mut f = FastFrontend::new(p.clone()).unwrap();
    let b: Vec<Symbol> = x.iter().filter_map(|&s| f.push(s)).collect();
    assert_eq!(reference(&p, &x), b);
}

#[test]
fn spec_extreme_components_use_the_wide_kernel_exactly() {
    // i16::MIN and i16::MAX: beyond the i32 bound, where only the i64
    // kernel is exact; alternating with small chunks switches kernels.
    let p = params(false, 64, 0.0, 1e6, 8, 2e-3, 40e6).unwrap();
    let x: Vec<[i16; 2]> = (0..30_000)
        .map(|i| match i % 7 {
            0 => [i16::MIN, i16::MAX],
            1 => [i16::MAX, i16::MIN],
            2 => [4095, -4095],
            3 => [-4096, 4096],
            _ => [
                ((i * 37) % 4000) as i16 - 2000,
                ((i * 91) % 4000) as i16 - 2000,
            ],
        })
        .collect();
    let want = reference(&p, &x);
    for isa in isas() {
        for chunk in [1, 3, 7, 1000, 30_000] {
            assert_eq!(want, fast_isa(&p, &x, chunk, isa), "{isa} chunk {chunk}");
            assert_eq!(
                want,
                fast_each(&p, &x, chunk, isa),
                "{isa} each, chunk {chunk}"
            );
        }
    }
}

/// The chunked mixer and the delay-line slides: chunk sizes around the
/// internal block of 4 096 samples and its divisors and multiples, on a
/// stream long enough for many slides, with 64 taps (the longest line).
#[test]
fn spec_chunk_boundaries_and_slides() {
    let p = params(true, 64, -0.16, -130e3, 6, 5e-6, 40e6).unwrap();
    let x = signal(60_000, 7, 2047.0, 40e6 / 33.7561e6);
    let want = reference(&p, &x);
    for isa in isas() {
        for chunk in [1, 63, 64, 65, 4095, 4096, 4097, 8191, 8192, 12_289, 60_000] {
            assert_eq!(want, fast_isa(&p, &x, chunk, isa), "{isa} chunk {chunk}");
        }
    }
}

/// Opt-in: a recording through both, ISDB-S3 or ISDB-S at its rate.
fn recording(var: &str, s3: bool) {
    let Ok(path) = std::env::var(var) else {
        eprintln!("SKIP: {var} not set");
        return;
    };
    let mut rec = isdb_iq::Recording::open(std::path::Path::new(&path), None, None).unwrap();
    let fs = rec.sample_rate;
    let n = 4_000_000usize.min((rec.segments()[0].end - rec.segments()[0].start) as usize);
    let x: Vec<[i16; 2]> = rec
        .read(rec.segments()[0].start, n)
        .unwrap()
        .iter()
        .map(|v| {
            [
                (v.re * 2048.0).round().clamp(-2048.0, 2047.0) as i16,
                (v.im * 2048.0).round().clamp(-2048.0, 2047.0) as i16,
            ]
        })
        .collect();
    let sv = isdb_iq::survey::survey(&x_f32(&x), fs, &isdb_iq::survey::Options::default());
    let cfo = sv.best_carrier().map_or(0.0, |c| c.offset_hz);
    let p = params(s3, 32, -0.16, cfo, 6, 5e-6, fs).unwrap();
    let a = reference(&p, &x);
    for isa in isas() {
        let b = fast_isa(&p, &x, 1 << 16, isa);
        assert_eq!(a.len(), b.len(), "{isa}");
        assert!(a == b, "{path}: fast ({isa}) differs from the reference");
        eprintln!("{path}: {} symbols identical ({isa})", a.len());
    }
}

fn x_f32(x: &[[i16; 2]]) -> Vec<num_complex::Complex<f32>> {
    x.iter()
        .take(1 << 22)
        .map(|s| num_complex::Complex::new(f32::from(s[0]) / 2048.0, f32::from(s[1]) / 2048.0))
        .collect()
}

#[test]
fn isdbs3_recording_fast_equals_reference() {
    recording("ISDB_RS_ISDBS3_IQ", true);
}

#[test]
fn isdbs_recording_fast_equals_reference() {
    recording("ISDB_RS_ISDBS_IQ", false);
}

/// Every padded width (8 … 64 taps, so the 16-bit kernels' 8-tap tails and
/// the AVX-512 32/16/8-tap steps all run), on every instruction set, with
/// the input range that switches between the 16-bit and `i64` kernels.
#[test]
fn spec_every_width_on_every_instruction_set() {
    for taps in (4..=64).step_by(4) {
        for (s3, fs) in [(true, 40e6), (false, 57.72e6)] {
            let Some(p) = params(s3, taps, -0.1, 50e3, 6, 2e-4, fs) else {
                continue;
            };
            for amp in [2047.0, 9000.0] {
                let x = signal(
                    9_000,
                    taps as u32,
                    amp,
                    fs / if s3 { 33.7561e6 } else { 28.86e6 },
                );
                let want = reference(&p, &x);
                for isa in isas() {
                    assert_eq!(
                        want,
                        fast_isa(&p, &x, 1000, isa),
                        "{isa} taps {taps} amp {amp}"
                    );
                    assert_eq!(want, fast_each(&p, &x, 777, isa), "{isa} each, taps {taps}");
                }
            }
        }
    }
}

#[test]
fn host_runs_the_instruction_sets_it_reports() {
    eprintln!(
        "instruction sets on this host: {:?} (best: {})",
        isas(),
        Isa::best()
    );
    assert!(isas().contains(&Isa::Baseline));
}
