//! Run the fixed-point front end and the floating-point reference on a
//! SigMF recording and print timing lock, MER and the output width study.
//!
//! cargo run --release -p isdb-frontend --example analyze -- REC [s|s3] [TAPS] [SAMPLES]

use isdb_frontend::fixed::Frontend;
use isdb_frontend::float::FloatFrontend;
use isdb_frontend::metrics::{psk_mer, ring_stats, sqnr_db};
use isdb_frontend::params::{Design, ONE, Params};
use isdb_iq::Recording;
use isdb_iq::survey::{Options, survey};
use std::path::PathBuf;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let path = PathBuf::from(&args[1]);
    let system = args.get(2).map_or("s", String::as_str);
    let taps: usize = args.get(3).map_or(32, |s| s.parse().unwrap());
    let n: usize = args.get(4).map_or(1 << 22, |s| s.parse().unwrap());
    let beta: f64 = std::env::var("BETA").map_or(5.0, |s| s.parse().unwrap());
    let mut rec = Recording::open(&path, None, None).unwrap();
    let fs = rec.sample_rate;
    let seg = rec.segments()[0].clone();
    let x = rec
        .read(
            seg.start,
            n.min(usize::try_from(seg.end - seg.start).unwrap()),
        )
        .unwrap();
    let sv = survey(&x[..x.len().min(1 << 22)], fs, &Options::default());
    let cfo = sv.best_carrier().map_or(0.0, |c| c.offset_hz);
    let (rs, alpha, rings) = match system {
        "s3" => (33.7561e6, 0.03, 2usize),
        _ => (28.86e6, 0.35, 1usize),
    };
    let rs_meas = sv
        .system
        .map_or(rs, |m| m.symbol_rate_hz * (1.0 + m.ppm * 1e-6));
    let mut d = Design::new(fs, rs, alpha);
    d.carrier_offset = cfo;
    d.taps = taps;
    d.kaiser_beta = beta;
    if let Ok(b) = std::env::var("LOOP_BW") {
        d.loop_bandwidth = b.parse().unwrap();
    }
    let p = Params::design(&d).unwrap();
    println!(
        "{}: fs {fs:.0} Hz, {} samples, survey rate {rs_meas:.1} Bd, carrier {cfo:+.0} Hz; taps {taps}, kaiser {beta}, kp {}>>{} ki {}>>{}, q_shift {}",
        path.display(),
        x.len(),
        p.kp,
        p.kp_shift,
        p.ki,
        p.ki_shift,
        p.q_shift
    );
    // 12-bit domain input (8-bit captures are exact: value·16).
    let xi: Vec<[i16; 2]> = x.iter().map(|&v| isdb_frontend::input_sample(v)).collect();
    let t0 = std::time::Instant::now();
    let mut fe = Frontend::new(p.clone()).unwrap();
    let mut syms = Vec::new();
    fe.process(&xi, &mut syms);
    let dt = t0.elapsed().as_secs_f64();
    let mut ff = FloatFrontend::new(p.clone(), d.sps(), alpha, beta);
    let fsyms: Vec<_> = xi
        .iter()
        .filter_map(|s| ff.push([f64::from(s[0]), f64::from(s[1])]))
        .collect();
    let settle = 40_000.min(syms.len() / 4);
    let z: Vec<[f64; 2]> = syms[settle..]
        .iter()
        .map(|s| [f64::from(s.z[0]), f64::from(s.z[1])])
        .collect();
    let zf: Vec<[f64; 2]> = fsyms[settle.min(fsyms.len())..]
        .iter()
        .map(|s| s.z)
        .collect();
    let period: Vec<f64> = syms[settle..]
        .iter()
        .map(|s| f64::from(s.period) / ONE as f64)
        .collect();
    let pm = period.iter().sum::<f64>() / period.len() as f64;
    let pj = (period.iter().map(|v| (v - pm).powi(2)).sum::<f64>() / period.len() as f64).sqrt();
    let rate = fs / pm;
    let clipped = syms[settle..]
        .iter()
        .filter(|s| s.z.iter().any(|&c| c == i16::MAX || c == i16::MIN))
        .count();
    println!(
        "fixed: {} symbols in {:.2} s ({:.1} Msym/s model speed); rate {rate:.1} Bd ({:+.2} ppm vs nominal, {:+.2} ppm vs survey), period jitter {:.2e} samples; gain {} ; z clipped {clipped}",
        syms.len(),
        dt,
        syms.len() as f64 / dt / 1e6,
        (rate / rs - 1.0) * 1e6,
        (rate / rs_meas - 1.0) * 1e6,
        pj,
        syms.last().unwrap().gain
    );
    for k in 1..=3 {
        let r = ring_stats(&z, k);
        let rf = ring_stats(&zf, k);
        println!(
            "rings {k}: fixed radii {:?} share {:?} ring MER {:.2} dB | float ring MER {:.2} dB",
            r.radii.iter().map(|v| v.round()).collect::<Vec<_>>(),
            r.share
                .iter()
                .map(|v| (v * 1000.0).round() / 1000.0)
                .collect::<Vec<_>>(),
            r.mer_db,
            rf.mer_db
        );
    }
    let psk = |zz: &[[f64; 2]]| psk_mer(zz, 8, 2e-3, 20_000);
    if system != "s3" {
        let a = psk(&z);
        let b = psk(&zf);
        println!(
            "8PSK DD: fixed MER {:.2} dB (phase rms {:.3} rad, residual {:+.1} Hz) | float MER {:.2} dB",
            a.mer_db,
            a.phase_rms,
            a.freq * rate,
            b.mer_db
        );
    }
    // Output width study: requantise z (the quantiser does not feed back).
    println!(
        "bits clip q_shift  SQNR(q vs z)  ring MER(q)  {}  MB/s",
        if system != "s3" { "8PSK MER(q)" } else { "" }
    );
    let base_ring = ring_stats(&z, rings).mer_db;
    for bits in 3u8..=8 {
        for clip in [1.0, 1.4, 2.0, 2.8] {
            let mut dd = d.clone();
            dd.bits = bits;
            dd.clip = clip;
            let pp = Params::design(&dd).unwrap();
            let lim = 1i64 << (bits - 1);
            let qs = u32::from(pp.q_shift);
            let q: Vec<[f64; 2]> = z
                .iter()
                .map(|v| {
                    // z is an i16 value in f64, so the cast is exact.
                    #[allow(clippy::cast_possible_truncation)]
                    let qq = |c: f64| {
                        (isdb_frontend::fixed::round_shift(c as i64, qs)).clamp(-lim, lim - 1)
                            as f64
                            * f64::from(1u32 << qs)
                    };
                    [qq(v[0]), qq(v[1])]
                })
                .collect();
            let sq = sqnr_db(&z, &q, 1.0);
            let rm = ring_stats(&q, rings).mer_db;
            let mbs = rate * pp.words_per_symbol() * 2.0 / 1e6;
            let pskq = if system != "s3" {
                format!("{:10.2}", psk(&q).mer_db)
            } else {
                String::new()
            };
            println!(
                "{bits:4} {clip:4.1} {:7} {sq:12.2} {rm:12.2} ({:+.2}) {pskq} {mbs:6.1}",
                pp.q_shift,
                rm - base_ring
            );
        }
    }
}
