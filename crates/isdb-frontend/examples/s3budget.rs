//! Loss budget of the front end on ISDB-S3 recordings: where between the
//! antenna's SNR and the front end's output the sync-symbol Es/N0 goes.
//! The investigation behind the format-2 output trim (crate README, "The
//! timing trim"); `ONLY=a,b` selects variants:
//!
//! Each variant processes the same IQ and is scored by
//! [`isdb_frontend::s3sync::measure`] (Es/N0 on the 24 known π/2-BPSK
//! symbols of every modulation slot):
//!
//! * `fixed`, `float` — the bit-exact model and its floating-point
//!   reference at 32 and 64 taps, untrimmed;
//! * `loop`  — loop bandwidth sweep, with the timing wander and jitter of
//!   the symbol instants (short-term jitter ≈ 0.004 T at 10⁻⁶ R);
//! * `ideal` — an FFT root-raised-cosine filter at one fixed timing line
//!   (with and without a brick-wall channel filter): no loop. Valid only
//!   while the symbol clock does not wander against the radio's over the
//!   excerpt (BS7 yes, BS11 / BS17 not);
//! * `fir`   — the model's windowed matched filter at the best fixed
//!   timing, 32 … 128 taps: the filter-length loss (≈ 0.4 dB at 32);
//! * `slot`  — per-slot data-aided timing: the 24 sync symbols of each
//!   slot re-evaluated with a 128-tap filter around the loop's instant,
//!   the best offset kept: the loop's timing error and the SNR with
//!   ideal timing (`EVERY=n` subsamples the slots);
//! * `trim`  — the output filter shifted by a trim with the loop on the
//!   unshifted pair (the format-2 structure), and with `QUANT=1` the
//!   output-width study and the data rings;
//! * `pulse` — the pulse at the matched-filter output against the loop's
//!   instant (correlation with the known symbols; only near τ = 0 is it
//!   meaningful, the π/2 rotation of neighbours mixes in elsewhere);
//! * `pre`   — the model fed through a brick-wall channel filter.
//!
//! Findings (2026-10-25, BS7 / BS11 / BS17 at 61.44 Msps, 56 MHz): the
//! loop locks 0.15–0.18 T away from the best instant on every transponder
//! (`slot`); that costs 5.1–6.4 dB; neighbours (`ideal` with a channel
//! filter, `pre`), filter length (`fir`), jitter (`loop`) and fixed point
//! (`fixed` vs `float`) cost under 0.5 dB together.
//!
//! cargo run --release -p isdb-frontend --example s3budget -- REC [SAMPLES]

use isdb_frontend::fixed::Frontend;
use isdb_frontend::float::FloatFrontend;
use isdb_frontend::params::{Design, Params};
use isdb_frontend::s3sync::{self, SLOT};
use isdb_iq::Recording;
use isdb_iq::survey::{Options, survey};
use num_complex::Complex64 as C;
use rustfft::FftPlanner;
use std::path::PathBuf;

const RS: f64 = 33.7561e6;
const ALPHA: f64 = 0.03;

fn to_i16(x: &[C]) -> Vec<[i16; 2]> {
    x.iter()
        .map(|v| {
            [
                (v.re * 2048.0).round().clamp(-2048.0, 2047.0) as i16,
                (v.im * 2048.0).round().clamp(-2048.0, 2047.0) as i16,
            ]
        })
        .collect()
}

fn fixed(x: &[C], d: &Design) -> Vec<C> {
    let mut fe = Frontend::new(Params::design(d).unwrap()).unwrap();
    let mut out = Vec::new();
    fe.process(&to_i16(x), &mut out);
    out.iter()
        .map(|s| C::new(f64::from(s.z[0]), f64::from(s.z[1])))
        .collect()
}

/// The fixed model's symbols and a description of its timing: each
/// symbol's instant (input sample + fractional phase) against the straight
/// line fitted through all of them, RMS and peak in symbol periods.
fn fixed_timing(x: &[C], d: &Design) -> (Vec<C>, String) {
    let p = Params::design(d).unwrap();
    let mut fe = Frontend::new(p).unwrap();
    let mut out = Vec::new();
    fe.process(&to_i16(x), &mut out);
    let skip = 3 * SLOT;
    // Instant of symbol k in input samples: the sample at which it was
    // computed minus the phase fraction (t_acc at the decision).
    let t: Vec<f64> = out
        .iter()
        .map(|s| s.sample as f64 + f64::from(s.phase) / 64.0)
        .collect();
    let n = t.len().saturating_sub(skip);
    let ks: Vec<f64> = (0..n).map(|k| k as f64).collect();
    let ts = &t[skip..];
    let (mk, mt) = (
        ks.iter().sum::<f64>() / n as f64,
        ts.iter().sum::<f64>() / n as f64,
    );
    let slope = ks
        .iter()
        .zip(ts)
        .map(|(k, t)| (k - mk) * (t - mt))
        .sum::<f64>()
        / ks.iter().map(|k| (k - mk).powi(2)).sum::<f64>();
    let resid: Vec<f64> = ks
        .iter()
        .zip(ts)
        .map(|(k, t)| (t - mt - slope * (k - mk)) / slope)
        .collect();
    let rms = (resid.iter().map(|r| r * r).sum::<f64>() / n as f64).sqrt();
    // Short-term jitter: residual after a moving average over 4096 symbols
    // (what the loop's slow wander does not explain).
    let w = 4096usize.min(n / 4).max(1);
    let mut short = 0.0;
    let mut cnt = 0usize;
    let mut acc: f64 = resid[..w].iter().sum();
    for i in w..n {
        let mean = acc / w as f64;
        short += (resid[i - w / 2] - mean).powi(2);
        cnt += 1;
        acc += resid[i] - resid[i - w];
    }
    let peak = resid.iter().fold(0.0f64, |a, r| a.max(r.abs()));
    let z = out
        .iter()
        .map(|s| C::new(f64::from(s.z[0]), f64::from(s.z[1])))
        .collect();
    (
        z,
        format!(
            "rate {:+.2} ppm, wander rms {rms:.4} T (peak {peak:.3} T), short-term rms {:.4} T",
            (d.sps() / slope - 1.0) * 1e6,
            (short / cnt.max(1) as f64).sqrt()
        ),
    )
}

fn float(x: &[C], d: &Design) -> Vec<C> {
    let p = Params::design(d).unwrap();
    let mut fe = FloatFrontend::new(p, d.sps(), d.roll_off, d.kaiser_beta);
    let mut z = Vec::new();
    for v in x {
        if let Some(s) = fe.push([v.re * 2048.0, v.im * 2048.0]) {
            z.push(C::new(s.z[0], s.z[1]));
        }
    }
    z
}

/// Multiply by e^{−j2π·cfo·n/fs}.
fn mix(x: &[C], cfo: f64, fs: f64) -> Vec<C> {
    x.iter()
        .enumerate()
        .map(|(n, v)| v * C::from_polar(1.0, -std::f64::consts::TAU * cfo * n as f64 / fs))
        .collect()
}

/// Filter `x` by the frequency response `h(f)` (Hz, at rate fs), by one
/// whole-signal FFT (circular; the ends are discarded by the caller).
fn freq_filter(x: &[C], fs: f64, h: &dyn Fn(f64) -> f64) -> Vec<C> {
    let n = x.len();
    let mut pl = FftPlanner::new();
    let mut buf = x.to_vec();
    pl.plan_fft_forward(n).process(&mut buf);
    for (k, v) in buf.iter_mut().enumerate() {
        let f = if k <= n / 2 {
            k as f64
        } else {
            k as f64 - n as f64
        } * fs
            / n as f64;
        *v *= h(f) / n as f64;
    }
    pl.plan_fft_inverse(n).process(&mut buf);
    buf
}

/// Root-raised-cosine magnitude response (unit passband).
fn rrc_response(f: f64, rs: f64, alpha: f64) -> f64 {
    let (fa, fn_) = (f.abs(), rs / 2.0);
    if fa <= fn_ * (1.0 - alpha) {
        1.0
    } else if fa >= fn_ * (1.0 + alpha) {
        0.0
    } else {
        (0.5 + 0.5 * (std::f64::consts::PI / (2.0 * alpha) * (fn_ - fa) / fn_).sin()).sqrt()
    }
}

/// Matched-filtered `y` (input rate) sampled at the symbol instants
/// t0 + k·T (samples), cubic interpolation.
fn sample_at(y: &[C], t0: f64, period: f64) -> Vec<C> {
    let mut z = Vec::new();
    let mut t = t0;
    while t + 3.0 < y.len() as f64 {
        let i = t.floor() as usize;
        if i >= 1 {
            let mu = t - i as f64;
            let (a, b, c, d) = (y[i - 1], y[i], y[i + 1], y[i + 2]);
            // Catmull-Rom
            let v = b
                + (c - a) * (0.5 * mu)
                + (a * 2.0 - b * 5.0 + c * 4.0 - d) * (0.5 * mu * mu)
                + (b * 3.0 - a - c * 3.0 + d) * (0.5 * mu * mu * mu);
            z.push(v);
        }
        t += period;
    }
    z
}

/// The ideal chain: matched filter by FFT, then the best fixed timing
/// (offset and rate searched by maximising the sync SNR).
fn ideal(x: &[C], fs: f64, cfo: f64, channel: Option<f64>, rate_ppm: f64) -> (Vec<C>, f64) {
    let m = mix(x, cfo, fs);
    let y = freq_filter(&m, fs, &|f| {
        let cf = channel.map_or(1.0, |b| if f.abs() <= b { 1.0 } else { 0.0 });
        rrc_response(f, RS, ALPHA) * cf
    });
    let period = fs / (RS * (1.0 + rate_ppm * 1e-6));
    // Coarse search of the fractional start over one symbol.
    let (mut best, mut best_t) = (f64::MIN, 0.0);
    for k in 0..16 {
        let t0 = 2000.0 + period * f64::from(k) / 16.0;
        let z = sample_at(&y, t0, period);
        if let Some(r) = s3sync::measure(&z)
            && r.snr_db > best
        {
            (best, best_t) = (r.snr_db, t0);
        }
    }
    for k in -8..=8 {
        let t0 = best_t + period * f64::from(k) / 128.0;
        let z = sample_at(&y, t0, period);
        if let Some(r) = s3sync::measure(&z)
            && r.snr_db > best
        {
            (best, best_t) = (r.snr_db, t0);
        }
    }
    (sample_at(&y, best_t, period), best)
}

/// Output width: quantise `z` (scaled to the model's AGC RMS 0.25·32768
/// and a full scale of `clip`·RMS per component, as `Params::design`) to
/// n bits and measure the sync-symbol Es/N0 and the ring statistics.
fn quant_study(z: &[C]) {
    let z = &z[(3 * SLOT).min(z.len())..];
    let rms = (z.iter().map(C::norm_sqr).sum::<f64>() / z.len() as f64).sqrt();
    let base = s3sync::measure(z).map_or(f64::NAN, |r| r.snr_db);
    for clip in [1.5f64, 2.0, 2.5] {
        let mut line = format!("  clip {clip}: unquantised {base:.2} dB |");
        for bits in [3u8, 4, 5, 6, 7, 8] {
            let half = f64::from(1u32 << (bits - 1));
            let step = clip / half; // in units of the RMS of |z|
            let q: Vec<C> = z
                .iter()
                .map(|v| {
                    let u = v / rms;
                    let f = |c: f64| (c / step).round().clamp(-half, half - 1.0) * step;
                    C::new(f(u.re), f(u.im))
                })
                .collect();
            let s = s3sync::measure(&q).map_or(f64::NAN, |r| r.snr_db);
            line += &format!(" {bits}b {s:.2}");
        }
        println!("{line}");
    }
    // Rings of the data symbols (derotated by the sync phase), for the
    // modulation: 8PSK has one ring, 16APSK two (4 + 12, ratio γ).
    if let Some(r) = s3sync::measure(z) {
        let d = s3sync::derotated_data(z, &r);
        let v: Vec<[f64; 2]> = d.iter().map(|c| [c.re, c.im]).collect();
        for rings in [1, 2, 3] {
            let st = isdb_frontend::metrics::ring_stats(&v, rings);
            println!(
                "  data rings {rings}: radii {:?} share {:?} ring MER {:.2} dB",
                st.radii
                    .iter()
                    .map(|x| (x * 1000.0).round() / 1000.0)
                    .collect::<Vec<_>>(),
                st.share
                    .iter()
                    .map(|x| (x * 1000.0).round() / 1000.0)
                    .collect::<Vec<_>>(),
                st.mer_db
            );
        }
        // Angle histogram of each ring (24 bins) for the two-ring fit.
        let st = isdb_frontend::metrics::ring_stats(&v, 2);
        let split = 0.5 * (st.radii[0] + st.radii[1]);
        for (name, inner) in [("inner", true), ("outer", false)] {
            let mut h = [0usize; 24];
            for c in &d {
                if (c.norm() < split) == inner {
                    let a = c.arg().rem_euclid(std::f64::consts::TAU);
                    h[((a / std::f64::consts::TAU * 24.0) as usize).min(23)] += 1;
                }
            }
            let tot: usize = h.iter().sum();
            let s: Vec<String> = h
                .iter()
                .map(|&n| format!("{}", (n * 100 / tot.max(1))))
                .collect();
            println!(
                "  {name} ring angles (15° bins, % of ring): {}",
                s.join(" ")
            );
        }
    }
}

fn report(name: &str, z: &[C]) {
    // Skip the loop's settling (first 3 slots).
    let z = &z[(3 * SLOT).min(z.len())..];
    match s3sync::measure(z) {
        Some(r) => println!(
            "{name:28} Es/N0 {:6.2} dB | peak {:.3} off {:.3} | phase step {:+.4} ±{:.4} rad | slots {}",
            r.snr_db, r.peak, r.off_peak, r.phase_step_mean, r.phase_step_rms, r.slots
        ),
        None => println!("{name:28} no sync ({} symbols)", z.len()),
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let path = PathBuf::from(&args[1]);
    let n: usize = args.get(2).map_or(12 << 20, |s| s.parse().unwrap());
    // START=k skips k samples (e.g. to look after a slow loop converged
    // on a preceding run; the loops here restart at the start anyway).
    let start: u64 = std::env::var("START").map_or(0, |s| s.parse().unwrap());
    let mut rec = Recording::open(&path, None, None).unwrap();
    let fs = rec.sample_rate;
    let seg = rec.segments()[0].clone();
    let x32 = rec
        .read(
            seg.start + start,
            n.min((seg.end - seg.start - start) as usize),
        )
        .unwrap();
    let x: Vec<C> = x32
        .iter()
        .map(|v| C::new(f64::from(v.re), f64::from(v.im)))
        .collect();
    let sv = survey(&x32[..x32.len().min(1 << 22)], fs, &Options::default());
    let cfo = sv.best_carrier().map_or(0.0, |c| c.offset_hz);
    let ppm = sv.system.map_or(0.0, |m| m.ppm);
    println!(
        "{}: {} samples at {fs} Hz, carrier {cfo:+.0} Hz, symbol rate {ppm:+.2} ppm",
        path.display(),
        x.len()
    );
    let base = |taps: usize| {
        let mut d = Design::new(fs, RS, ALPHA);
        d.carrier_offset = cfo;
        d.taps = taps;
        d.loop_bandwidth = 5e-6;
        d.bits = 8;
        d
    };
    let which = std::env::var("ONLY").unwrap_or_default();
    let want = |s: &str| which.is_empty() || which.split(',').any(|w| s.starts_with(w));
    if want("fixed") {
        for t in [32, 64] {
            report(&format!("fixed {t}"), &fixed(&x, &base(t)));
        }
    }
    if want("float") {
        for t in [32, 64] {
            report(&format!("float {t}"), &float(&x, &base(t)));
        }
    }
    if want("loop") {
        for bw in [1e-6, 2e-6, 5e-6, 2e-5] {
            let mut d = base(32);
            d.loop_bandwidth = bw;
            let fe = fixed_timing(&x, &d);
            report(&format!("fixed 32, loop {bw:.0e}"), &fe.0);
            println!("{:28} timing: {}", "", fe.1);
        }
    }
    if want("ideal") {
        let (z, _) = ideal(&x, fs, cfo, None, ppm);
        report("ideal (FFT RRC)", &z);
        for b in [17.6e6, 18.0e6, 19.0e6] {
            let (z, _) = ideal(&x, fs, cfo, Some(b), ppm);
            report(&format!("ideal + cf ±{:.1} MHz", b / 1e6), &z);
        }
    }
    if want("fir") {
        // The model's windowed matched filter (same continuous function
        // as its banks) at ideal timing: filter-shape loss alone.
        let m = mix(&x, cfo, fs);
        let sps = fs / RS;
        let period = fs / (RS * (1.0 + ppm * 1e-6));
        for (taps, beta) in [
            (32, 5.0),
            (64, 5.0),
            (64, 0.0),
            (128, 5.0),
            (256, 5.0),
            (512, 5.0),
        ] {
            // Direct evaluation at each symbol instant.
            let run = |t0: f64| -> Vec<C> {
                let half = taps as f64 / 2.0;
                let mut z = Vec::new();
                let mut t = t0;
                while t + half + 2.0 < m.len() as f64 {
                    let n0 = (t - half).ceil() as i64;
                    let mut acc = C::new(0.0, 0.0);
                    for n in n0..n0 + taps as i64 {
                        if n >= 0 {
                            acc += m[n as usize]
                                * isdb_frontend::tables::matched_filter(
                                    t - n as f64,
                                    sps,
                                    ALPHA,
                                    taps,
                                    beta,
                                );
                        }
                    }
                    z.push(acc);
                    t += period;
                }
                z
            };
            let (mut best, mut best_t) = (f64::MIN, 0.0);
            for k in 0..16 {
                let t0 = 4000.0 + period * f64::from(k) / 16.0;
                if let Some(r) = s3sync::measure(&run(t0))
                    && r.snr_db > best
                {
                    (best, best_t) = (r.snr_db, t0);
                }
            }
            report(&format!("fir {taps} β{beta}, ideal timing"), &run(best_t));
        }
    }
    if want("slot") {
        // Data-aided per-slot timing: run the float model for its timing,
        // then for each slot re-evaluate the 24 sync symbols with the
        // exact 128-tap matched filter at instants shifted by δ (the same
        // δ for the whole word) and keep the δ of best fit. Gives the SNR
        // with ideal (slot-wise) timing and the loop's timing error.
        let mut d = base(32);
        d.loop_bandwidth = 2e-6;
        let p = Params::design(&d).unwrap();
        let mut fe = FloatFrontend::new(p, d.sps(), ALPHA, d.kaiser_beta);
        let mut inst = Vec::new();
        let mut zs = Vec::new();
        for (n, v) in x.iter().enumerate() {
            if let Some(s) = fe.push([v.re * 2048.0, v.im * 2048.0]) {
                // instant of this symbol: n − taps/2 + mu
                inst.push(n as f64 - 16.0 + s.mu);
                zs.push(C::new(s.z[0], s.z[1]));
            }
        }
        let skip = 3 * SLOT;
        let r = s3sync::measure(&zs[skip..]).unwrap();
        println!("float 32 loop: Es/N0 {:.2} dB", r.snr_db);
        let m = mix(&x, cfo, fs);
        let sps = fs / RS;
        let mf = |t: f64| -> C {
            let half = 64.0;
            let n0 = (t - half).ceil() as i64;
            let mut acc = C::new(0.0, 0.0);
            for nn in n0..n0 + 128 {
                if nn >= 0 && (nn as usize) < m.len() {
                    acc += m[nn as usize]
                        * isdb_frontend::tables::matched_filter(
                            t - nn as f64,
                            sps,
                            ALPHA,
                            128,
                            5.0,
                        );
                }
            }
            acc
        };
        let fsy = s3sync::sync_symbols(s3sync::FSYNC);
        let ssy = s3sync::sync_symbols(s3sync::SSYNC);
        let (mut sig, mut err) = (0.0, 0.0);
        let mut deltas = Vec::new();
        // EVERY=n evaluates every n-th slot only (long recordings).
        let every: usize = std::env::var("EVERY").map_or(1, |s| s.parse().unwrap());
        for k in (0..r.slots).step_by(every) {
            let at = skip + r.offset + k * SLOT;
            let word = if (k + r.ssync_parity).is_multiple_of(2) {
                &ssy
            } else {
                &fsy
            };
            let mut best = (f64::MIN, 0.0, C::new(0.0, 0.0), Vec::new());
            for di in -24i32..=24 {
                let delta = f64::from(di) / 48.0 * sps; // ±0.5 T
                let v: Vec<C> = (0..24).map(|j| mf(inst[at + j] + delta)).collect();
                let c: C = word.iter().zip(&v).map(|(a, b)| b * a.conj()).sum();
                if c.norm() > best.0 {
                    best = (c.norm(), delta / sps, c, v);
                }
            }
            let (_, dt, c, v) = best;
            let amp = c.norm() / 24.0;
            let ph = c / c.norm();
            for (a, b) in word.iter().zip(&v) {
                let ideal = a * ph * amp;
                sig += ideal.norm_sqr();
                err += (b - ideal).norm_sqr();
            }
            deltas.push(dt);
        }
        let mean = deltas.iter().sum::<f64>() / deltas.len() as f64;
        let sd =
            (deltas.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / deltas.len() as f64).sqrt();
        println!(
            "per-slot best timing: Es/N0 {:.2} dB; loop error mean {mean:+.4} T, rms {sd:.4} T",
            10.0 * (sig / err).log10()
        );
        let first: Vec<String> = deltas
            .iter()
            .step_by(deltas.len() / 16 + 1)
            .map(|v| format!("{v:+.3}"))
            .collect();
        println!("  δ along the recording: {}", first.join(" "));
        let h = deltas.len() / 2;
        let m2 = deltas[h..].iter().sum::<f64>() / (deltas.len() - h) as f64;
        println!("  second half: loop error mean {m2:+.4} T");
    }
    if want("trim") {
        // Output from a shifted copy of the matched filter (same taps,
        // evaluated at the loop's instant + trim), the loop still on the
        // unshifted pair: the SNR a trim register would give.
        let quick = std::env::var("QUANT").is_ok();
        for taps in if quick {
            vec![32usize]
        } else {
            vec![32usize, 48, 64]
        } {
            let mut d = base(taps);
            d.loop_bandwidth = 2e-6;
            let p = Params::design(&d).unwrap();
            let mut fe = FloatFrontend::new(p, d.sps(), ALPHA, d.kaiser_beta);
            let m = mix(&x, cfo, fs);
            let sps = fs / RS;
            let mut inst = Vec::new();
            for (n, v) in x.iter().enumerate() {
                if let Some(s) = fe.push([v.re * 2048.0, v.im * 2048.0]) {
                    inst.push(n as f64 - taps as f64 / 2.0 + s.mu);
                }
            }
            for trim_t in if quick {
                vec![-0.17]
            } else {
                vec![-0.25, -0.20, -0.17, -0.14, -0.10]
            } {
                let trim = trim_t * sps;
                // The same `taps`-sample window as the model, positioned
                // on the loop's sample grid (the window does not move with
                // the trim: the shifted pulse is evaluated inside it).
                let z: Vec<C> = inst
                    .iter()
                    .map(|&t| {
                        let n_new = (t + taps as f64 / 2.0).floor() as i64;
                        let mut acc = C::new(0.0, 0.0);
                        for k in 0..taps as i64 {
                            let nn = n_new - k;
                            if nn >= 0 && (nn as usize) < m.len() {
                                let tau = t + trim - nn as f64;
                                // centre of the window: the loop's
                                // instant (window from t − taps/2 to
                                // t + taps/2 in the model)
                                let w = isdb_frontend::tables::kaiser(
                                    t - nn as f64,
                                    taps as f64 / 2.0,
                                    d.kaiser_beta,
                                );
                                acc += m[nn as usize]
                                    * isdb_frontend::tables::rrc(tau / sps, ALPHA)
                                    * w;
                            }
                        }
                        acc
                    })
                    .collect();
                report(&format!("float {taps} + trim {trim_t:+.2} T"), &z);
                if taps == 32 && (trim_t + 0.17f64).abs() < 1e-9 {
                    quant_study(&z);
                }
            }
        }
    }
    if want("pulse") {
        // The overall pulse at the matched-filter output, sampled at the
        // optimum instant and at ±k/8 symbol offsets around it: least
        // squares of the received sync and pilot... sync symbols only (the
        // known 24 per slot), as h(τ) = E[r(t_k + τ)·conj(a_k)] with
        // derotation per slot. A symmetric pulse gives |h(+τ)| = |h(−τ)|.
        let mut d = base(32);
        d.loop_bandwidth = 2e-6;
        let p = Params::design(&d).unwrap();
        let mut fe = FloatFrontend::new(p, d.sps(), ALPHA, d.kaiser_beta);
        let mut inst = Vec::new();
        let mut zs = Vec::new();
        for (n, v) in x.iter().enumerate() {
            if let Some(s) = fe.push([v.re * 2048.0, v.im * 2048.0]) {
                inst.push(n as f64 - 16.0 + s.mu);
                zs.push(C::new(s.z[0], s.z[1]));
            }
        }
        let skip = (x.len() as f64 / d.sps() / 2.0) as usize; // second half
        let r = s3sync::measure(&zs[skip..]).unwrap();
        let m = mix(&x, cfo, fs);
        let sps = fs / RS;
        let mf = |t: f64| -> C {
            let n0 = (t - 64.0).ceil() as i64;
            let mut acc = C::new(0.0, 0.0);
            for nn in n0..n0 + 128 {
                if nn >= 0 && (nn as usize) < m.len() {
                    acc += m[nn as usize]
                        * isdb_frontend::tables::matched_filter(
                            t - nn as f64,
                            sps,
                            ALPHA,
                            128,
                            5.0,
                        );
                }
            }
            acc
        };
        let fsy = s3sync::sync_symbols(s3sync::FSYNC);
        let ssy = s3sync::sync_symbols(s3sync::SSYNC);
        // τ grid in symbols, relative to the loop's instants.
        let taus: Vec<f64> = (-24..=24).map(|i| f64::from(i) / 16.0).collect();
        let mut h = vec![C::new(0.0, 0.0); taus.len()];
        let mut cnt = 0.0;
        for k in (0..r.slots).step_by(4) {
            let at = skip + r.offset + k * SLOT;
            let word = if (k + r.ssync_parity).is_multiple_of(2) {
                &ssy
            } else {
                &fsy
            };
            // phase from the word at the loop instant
            let c0: C = word
                .iter()
                .enumerate()
                .map(|(j, a)| mf(inst[at + j]) * a.conj())
                .sum();
            let ph = c0 / c0.norm();
            for (ti, tau) in taus.iter().enumerate() {
                // correlate symbol j's known value with the signal at
                // instant(j) + τ, over the inner symbols of the word
                let mut acc = C::new(0.0, 0.0);
                for j in 4..20 {
                    acc += mf(inst[at + j] + tau * sps) / ph * word[j].conj();
                }
                h[ti] += acc / 16.0;
            }
            cnt += 1.0;
        }
        let peak = h.iter().map(|v| v.norm()).fold(0.0, f64::max) / cnt;
        let imax = h
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.norm().total_cmp(&b.1.norm()))
            .unwrap()
            .0;
        println!(
            "pulse at the matched-filter output (relative to the loop's instant), peak at τ = {:+.3} T:",
            taus[imax]
        );
        for (ti, tau) in taus.iter().enumerate().step_by(2) {
            let v = h[ti] / cnt / peak;
            println!(
                "  τ {tau:+.3} T: {:.3} ∠{:+6.1}°  {}",
                v.norm(),
                v.arg().to_degrees(),
                "#".repeat((v.norm() * 40.0) as usize)
            );
        }
    }
    if want("pre") {
        let pre = freq_filter(&x, fs, &|f| {
            if (f - cfo).abs() <= 17.6e6 { 1.0 } else { 0.0 }
        });
        report("fixed 64 + brick ±17.6", &fixed(&pre, &base(64)));
        report("float 64 + brick ±17.6", &float(&pre, &base(64)));
    }
}
