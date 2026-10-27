//! Signal and noise power of a single-carrier recording, referred to the
//! radio's input: for comparing two recordings of the same transponder made
//! through different feeds (e.g. with and without a DC block), where the
//! MER alone understates a loss ahead of the radio whenever the feed's own
//! noise dominates.
//!
//! A Welch power spectrum (Hann window, 4096 bins, every whole block of the
//! first segment) gives
//! - **signal**: power within ±`--half-band` of the carrier (default 13 MHz,
//!   inside the −3 dB width of a 28.86 Mbaud α = 0.35 ISDB-S signal);
//! - **noise**: mean density beyond ±`--guard` (default 19.5 MHz =
//!   (1 + α)·R/2) and inside ±0.4·fs (clear of the analog filter's roll-off
//!   and of the band edge), scaled to the signal band's width;
//!
//! in dBFS, and minus the receive gain recorded in `isdb:gain` ("… N dB at
//! the end", whole dB) as an input-referred level in dB relative to an
//! arbitrary but common reference. Prints one JSON line per recording.
//!
//! ```sh
//! cargo run --release -p isdb-iq --example bandpower -- REC [REC…] [--carrier-offset HZ] [--half-band HZ] [--guard HZ]
//! ```

use isdb_iq::{C32, Recording};
use rustfft::FftPlanner;

const N: usize = 4096;

struct Opts {
    carrier_offset: f64,
    half_band: f64,
    guard: f64,
}

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

/// `"AGC slow_attack, 49 dB at the end"` or `"manual 30 dB"` → 49 / 30.
fn gain_db(rec: &Recording) -> Option<f64> {
    let s = rec
        .meta
        .as_ref()?
        .captures
        .first()?
        .extra
        .get("isdb:gain")?
        .as_str()?;
    let i = s.find(" dB")?;
    let num = s[..i]
        .rsplit(|c: char| !(c.is_ascii_digit() || c == '.' || c == '-'))
        .next()?;
    num.parse().ok()
}

fn measure(path: &str, o: &Opts) -> serde_json::Value {
    let mut rec = Recording::open(std::path::Path::new(path), None, None).expect("open recording");
    let fs = rec.sample_rate;
    let seg = rec.segments()[0].clone();
    let len = usize::try_from(seg.end - seg.start).expect("segment fits in memory");
    let x = rec.read(seg.start, len).expect("read");
    let w: Vec<f32> = (0..N)
        .map(|i| (std::f32::consts::PI * i as f32 / N as f32).sin().powi(2))
        .collect();
    let wpow: f64 = w.iter().map(|&v| f64::from(v * v)).sum();
    let fft = FftPlanner::<f32>::new().plan_fft_forward(N);
    let mut psd = vec![0f64; N];
    let mut blocks = 0usize;
    let mut buf = vec![C32::new(0.0, 0.0); N];
    for b in x.as_chunks::<N>().0 {
        for ((d, s), &k) in buf.iter_mut().zip(b).zip(&w) {
            *d = s * k;
        }
        fft.process(&mut buf);
        for (p, v) in psd.iter_mut().zip(&buf) {
            *p += f64::from(v.norm_sqr());
        }
        blocks += 1;
    }
    // Power per bin, so that the sum over all bins is the mean power of x
    // (full scale = |x| = 1, so 0 dBFS).
    for p in &mut psd {
        *p /= blocks as f64 * N as f64 * wpow;
    }
    let df = fs / N as f64;
    let freq = |k: usize| {
        let k = if k >= N / 2 {
            k as f64 - N as f64
        } else {
            k as f64
        };
        k * df - o.carrier_offset
    };
    let (mut sig, mut nsum, mut nbins) = (0f64, 0f64, 0usize);
    let mut sbins = 0usize;
    for (k, &p) in psd.iter().enumerate() {
        let f = freq(k).abs();
        let fa = (freq(k) + o.carrier_offset).abs();
        if f <= o.half_band {
            sig += p;
            sbins += 1;
        } else if f >= o.guard && fa <= 0.4 * fs {
            nsum += p;
            nbins += 1;
        }
    }
    let noise_in_band = nsum / nbins.max(1) as f64 * sbins as f64;
    let db = |v: f64| 10.0 * v.log10();
    let gain = gain_db(&rec);
    let sig_db = db(sig);
    let noise_db = db(noise_in_band);
    // Input-referred density profile in 2 MHz steps across ±0.45·fs
    // (dB per bin, minus the gain), for locating noise or interference.
    let step = 2e6;
    let mut profile = Vec::new();
    let mut f0 = -(0.45 * fs / step).floor() * step;
    while f0 + step <= 0.45 * fs {
        let (mut s, mut n) = (0f64, 0usize);
        for (k, &p) in psd.iter().enumerate() {
            let f = freq(k) + o.carrier_offset;
            if f >= f0 && f < f0 + step {
                s += p;
                n += 1;
            }
        }
        profile.push(serde_json::json!([
            f0 / 1e6,
            db(s / n.max(1) as f64) - gain.unwrap_or(0.0)
        ]));
        f0 += step;
    }
    serde_json::json!({
        "profile_mhz_db": profile,
        "file": path,
        "centre_hz": rec.frequency,
        "sample_rate_hz": fs,
        "gain_db": gain,
        "signal_dbfs": sig_db,
        "noise_dbfs": noise_db,
        "snr_db": db((sig - noise_in_band).max(1e-30) / noise_in_band),
        "noise_bins": nbins,
        "signal_input_db": gain.map(|g| sig_db - g),
        "noise_input_db": gain.map(|g| noise_db - g),
    })
}

fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let o = Opts {
        carrier_offset: arg(&mut args, "--carrier-offset", 0.0),
        half_band: arg(&mut args, "--half-band", 13e6),
        guard: arg(&mut args, "--guard", 19.5e6),
    };
    assert!(
        !args.is_empty(),
        "usage: bandpower REC [REC…] [--carrier-offset HZ] [--half-band HZ] [--guard HZ]"
    );
    for p in &args {
        println!("{}", measure(p, &o));
    }
}
