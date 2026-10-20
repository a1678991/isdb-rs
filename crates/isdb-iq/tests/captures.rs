//! Opt-in tests on satellite IF recordings (SigMF, e.g. from `isdb
//! capture`): `ISDB_RS_ISDBS_IQ` names an ISDB-S transponder,
//! `ISDB_RS_ISDBS3_IQ` an ISDB-S3 one (data file, `.sigmf-meta` or base
//! name). Each test prints a `SKIP` line and passes when its variable is
//! unset. See docs/test-data.md.

use isdb_iq::survey::{Options, survey};
use isdb_iq::{C32, Recording};
use std::path::PathBuf;

fn recording(var: &str) -> Option<Recording> {
    let Some(p) = std::env::var_os(var).map(PathBuf::from) else {
        eprintln!("SKIP: {var} not set (path of a SigMF recording)");
        return None;
    };
    let r =
        Recording::open(&p, None, None).unwrap_or_else(|e| panic!("{var}={}: {e}", p.display()));
    assert!(r.meta.is_some(), "{}: no SigMF metadata", p.display());
    Some(r)
}

/// The deviations (rad) of the symbol-rate line's phase from its mean
/// advance, chunk to chunk, over `x`: a lost sample moves it by
/// 2π·R/fs (mod 2π). Meaningful only when fs is not close to 2R (the line
/// and its image fs − R must be resolved within a chunk).
fn symbol_clock_jumps(x: &[C32], fs: f64, line_hz: f64, chunk: usize) -> Vec<f64> {
    let mut phases = Vec::new();
    for (k, c) in x.chunks_exact(chunk).enumerate() {
        let mean = c.iter().map(|v| v.norm_sqr()).sum::<f32>() / chunk as f32;
        let mut acc = num_complex::Complex64::new(0.0, 0.0);
        for (i, v) in c.iter().enumerate() {
            let n = (k * chunk + i) as f64;
            let ph = -2.0 * std::f64::consts::PI * line_hz * n / fs;
            acc += num_complex::Complex64::from_polar(f64::from(v.norm_sqr() - mean), ph);
        }
        phases.push(acc.arg());
    }
    let wrap = |a: f64| {
        (a + std::f64::consts::PI).rem_euclid(2.0 * std::f64::consts::PI) - std::f64::consts::PI
    };
    let steps: Vec<f64> = phases.windows(2).map(|w| wrap(w[1] - w[0])).collect();
    let (s, c) = steps
        .iter()
        .fold((0.0, 0.0), |(s, c), d| (s + d.sin(), c + d.cos()));
    let mean = s.atan2(c);
    steps.iter().map(|d| wrap(d - mean)).collect()
}

fn check(var: &str, system: &str) {
    let Some(mut r) = recording(var) else {
        return;
    };
    let fs = r.sample_rate;
    let seg = r.segments()[0].clone();
    let n = usize::try_from(seg.end - seg.start).unwrap();
    let x = r.read(seg.start, n.min(1 << 22)).unwrap();
    let s = survey(&x, fs, &Options::default());
    eprintln!("{}: {s:#?}", r.data_path.display());
    let m = s.system.unwrap_or_else(|| panic!("no known system: {s:?}"));
    assert_eq!(m.system.name, system);
    assert!(m.ppm.abs() < 20.0, "{m:?}");
    assert!(s.symbol_line.level_db > 20.0, "{s:?}");
    let best = s.best_carrier().unwrap();
    assert!(best.line.level_db > 20.0, "{best:?}");
    // Another clear line of a different kind gives the same offset (on a
    // satellite IF, neighbouring transponders can leave strong unrelated
    // lines in a search window).
    let agree = s
        .carrier_lines
        .iter()
        .filter(|c| c.line.level_db > 15.0 && (c.k, c.shift_hz) != (best.k, best.shift_hz))
        .filter(|c| (c.offset_hz - best.offset_hz).abs() < 10.0)
        .count();
    assert!(
        agree >= 1,
        "carrier offset {best:?} not corroborated: {s:?}"
    );
    // The whole first segment is contiguous: no jump of the symbol clock.
    let rate = m.symbol_rate_hz;
    if (fs / rate - 2.0).abs() > 0.02 {
        let x = r.read(seg.start, n).unwrap();
        let j = symbol_clock_jumps(&x, fs, s.symbol_line.freq_hz, 1 << 18);
        let max = j.iter().fold(0.0f64, |a, b| a.max(b.abs()));
        eprintln!(
            "symbol clock over {n} samples: largest deviation {max:.3} rad (one lost sample: {:.2} rad)",
            2.0 * std::f64::consts::PI * (rate / fs).fract()
        );
        assert!(max < 1.0, "symbol-clock jump of {max} rad: samples lost?");
    } else {
        eprintln!("fs = {fs} is close to 2R: symbol-clock continuity not checked");
    }
}

#[test]
fn isdb_s_recording() {
    check("ISDB_RS_ISDBS_IQ", "ISDB-S");
}

#[test]
fn isdb_s3_recording() {
    check("ISDB_RS_ISDBS3_IQ", "ISDB-S3");
}
