//! The spectrum of one capture of a band sweep (`tools/band-sweep.sh`): a
//! Welch power spectrum (Hann window, `--fft` bins, every whole block of the
//! first segment up to `--samples`), kept to the centre ±`--keep` Hz (clear
//! of the analog filter's roll-off), with the noise floor (the 10th
//! percentile of the bins: a wideband carrier can fill most of the window,
//! so the median would be the carrier) and every span that stands
//! `--threshold` dB above it. A carrier wider than ≈ 90 % of the window
//! raises the floor itself; the sweep's step keeps windows wider than a
//! transponder. A span is reported only if it is at least `--min-width` Hz
//! wide (default 1 MHz: a modulated carrier) or peaks `--strong` dB above
//! the floor (default 15: a tone); noise crosses the threshold in single
//! bins otherwise.
//!
//! Prints one JSON line: the capture's centre, the floor in dBFS per bin,
//! the kept spectrum decimated to `--points` points (frequency in Hz,
//! level in dBFS per bin), and the spans above the floor (start, end, peak
//! frequency, peak level, mean excess). The levels are relative (dBFS at
//! the recorded gain); use one fixed gain for a whole sweep so they compare.
//!
//! ```sh
//! cargo run --release -p isdb-iq --example sweepspec -- REC [--fft N] [--keep HZ] [--threshold DB] [--min-width HZ] [--strong DB] [--points N] [--samples N]
//! ```

use isdb_iq::Recording;
use num_complex::Complex32 as C32;
use rustfft::FftPlanner;
use serde_json::json;

fn arg(args: &mut Vec<String>, name: &str, default: f64) -> f64 {
    match args.iter().position(|a| a == name) {
        Some(i) => {
            let v = args[i + 1].parse().expect("number");
            args.drain(i..=i + 1);
            v
        }
        None => default,
    }
}

fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let fft = arg(&mut args, "--fft", 4096.0) as usize;
    let keep = arg(&mut args, "--keep", 24e6);
    let threshold = arg(&mut args, "--threshold", 6.0);
    let points = arg(&mut args, "--points", 96.0) as usize;
    let samples = arg(&mut args, "--samples", (1u64 << 20) as f64) as usize;
    let min_width = arg(&mut args, "--min-width", 1e6);
    let strong = arg(&mut args, "--strong", 15.0);
    let path = args.first().expect("usage: sweepspec REC [options]");
    let mut rec = Recording::open(std::path::Path::new(path), None, None).expect("open recording");
    let fs = rec.sample_rate;
    let centre = rec.frequency.unwrap_or(0.0);
    let seg = rec.segments()[0].clone();
    let n = samples.min((seg.end - seg.start) as usize);
    let x = rec.read(seg.start, n).expect("read");

    // Welch: Hann-windowed blocks, power averaged, DC in the middle
    let w: Vec<f32> = (0..fft)
        .map(|i| (std::f32::consts::PI * i as f32 / fft as f32).sin().powi(2))
        .collect();
    let wpow: f64 = w.iter().map(|&v| f64::from(v * v)).sum();
    let plan = FftPlanner::<f32>::new().plan_fft_forward(fft);
    let mut psd = vec![0f64; fft];
    let mut buf = vec![C32::new(0.0, 0.0); fft];
    let mut blocks = 0usize;
    for b in x.chunks_exact(fft) {
        for ((d, s), &k) in buf.iter_mut().zip(b).zip(&w) {
            *d = s * k;
        }
        plan.process(&mut buf);
        for (p, v) in psd.iter_mut().zip(&buf) {
            *p += f64::from(v.norm_sqr());
        }
        blocks += 1;
    }
    assert!(blocks > 0, "fewer than --fft samples");
    // bin k of the shifted spectrum is at (k − fft/2)·fs/fft from the centre
    let df = fs / fft as f64;
    let db: Vec<(f64, f64)> = (0..fft)
        .map(|k| {
            let s = (k + fft / 2) % fft;
            let f = (k as f64 - (fft / 2) as f64) * df;
            let p = psd[s] / (blocks as f64 * fft as f64 * wpow);
            (f, 10.0 * p.max(1e-30).log10())
        })
        .filter(|(f, _)| f.abs() <= keep)
        .collect();
    // the floor is a low percentile, not the median: a wideband carrier
    // (a 34–39 MHz satellite transponder in a 48 MHz window) is most of
    // the bins, and its median is the carrier, not the noise
    let mut levels: Vec<f64> = db.iter().map(|&(_, l)| l).collect();
    levels.sort_by(f64::total_cmp);
    let floor = levels[levels.len() / 10];

    // spans of consecutive bins above floor + threshold
    let mut spans = Vec::new();
    let mut i = 0;
    while i < db.len() {
        if db[i].1 - floor < threshold {
            i += 1;
            continue;
        }
        let start = i;
        while i < db.len() && db[i].1 - floor >= threshold {
            i += 1;
        }
        let s = &db[start..i];
        let (pf, pl) = s
            .iter()
            .copied()
            .max_by(|a, b| a.1.total_cmp(&b.1))
            .expect("non-empty span");
        let mean = s.iter().map(|&(_, l)| l - floor).sum::<f64>() / s.len() as f64;
        // single bins of noise cross the threshold now and then: keep a
        // span only if it is wide (a modulated carrier) or strong (a tone)
        let width = s.len() as f64 * df;
        if width < min_width && pl - floor < strong {
            continue;
        }
        spans.push(json!({
            "start_hz": centre + s[0].0,
            "end_hz": centre + s[s.len() - 1].0 + df,
            "peak_hz": centre + pf,
            "peak_dbfs": pl,
            "mean_excess_db": mean,
        }));
    }

    // decimated spectrum for plotting: the maximum of each group of bins
    let step = db.len().div_ceil(points.max(1));
    let curve: Vec<[f64; 2]> = db
        .chunks(step)
        .map(|g| {
            let (f, l) = g
                .iter()
                .copied()
                .max_by(|a, b| a.1.total_cmp(&b.1))
                .unwrap();
            [centre + f, l]
        })
        .collect();

    println!(
        "{}",
        json!({
            "file": path,
            "centre_hz": centre,
            "sample_rate_hz": fs,
            "blocks": blocks,
            "floor_dbfs": floor,
            "spans": spans,
            "curve": curve,
        })
    );
}
