//! The timing trim on an ISDB-S3 recording, end to end with the bit-exact
//! model: measure the best trim on an excerpt (`s3sync::best_trim`), then
//! run the whole first segment with and without it and report the
//! sync-symbol Es/N0, and the output-width study on the data symbols.
//!
//! cargo run --release -p isdb-frontend --example s3trim -- REC [TAPS] [EXCERPT_SAMPLES]

// Narrowing casts are proven or fallible here (docs/plans/type-safety.md section 2).
#![warn(clippy::cast_possible_truncation)]
use isdb_frontend::fixed::Frontend;
use isdb_frontend::params::{Design, Params};
use isdb_frontend::s3sync::{self, SLOT};
use isdb_iq::Recording;
use isdb_iq::survey::{KNOWN_SYSTEMS, Options, survey};
use num_complex::Complex64 as C;
use std::path::PathBuf;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let path = PathBuf::from(&args[1]);
    let taps: usize = args.get(2).map_or(32, |s| s.parse().unwrap());
    let excerpt: usize = args.get(3).map_or(6 << 20, |s| s.parse().unwrap());
    let mut rec = Recording::open(&path, None, None).unwrap();
    let fs = rec.sample_rate;
    let seg = rec.segments()[0].clone();
    let x = rec
        .read(seg.start, usize::try_from(seg.end - seg.start).unwrap())
        .unwrap();
    let sv = survey(&x[..x.len().min(1 << 22)], fs, &Options::default());
    let cfo = sv.best_carrier().map_or(0.0, |c| c.offset_hz);
    let xi: Vec<[i16; 2]> = x.iter().map(|&v| isdb_frontend::input_sample(v)).collect();
    let mut d = Design::for_system(&KNOWN_SYSTEMS[1], fs);
    d.carrier_offset = cfo;
    d.taps = taps;
    d.bits = 8;
    let cands: Vec<f64> = (-6..=1).map(|i| f64::from(i) * 0.05).collect();
    let t0 = std::time::Instant::now();
    let (trim, est) = s3sync::best_trim(&xi[..excerpt.min(xi.len())], &d, &cands).unwrap();
    println!(
        "{}: {} samples at {fs} Hz; best trim {trim:+.3} T (excerpt Es/N0 {est:.2} dB, {:.1} s)",
        path.display(),
        xi.len(),
        t0.elapsed().as_secs_f64()
    );
    for t in [0.0, trim] {
        let mut dd = d.clone();
        dd.trim = t;
        let mut fe = Frontend::new(Params::design(&dd).unwrap()).unwrap();
        let mut out = Vec::new();
        fe.process(&xi, &mut out);
        let zk: Vec<C> = out
            .iter()
            .skip(3 * SLOT)
            .map(|s| C::new(f64::from(s.zk[0]), f64::from(s.zk[1])))
            .collect();
        let r = s3sync::measure(&zk).unwrap();
        println!(
            "  {taps} taps, trim {t:+.3} T: Es/N0 {:.2} dB over {} slots (8-bit output)",
            r.snr_db, r.slots
        );
        if t != 0.0 {
            // Output width on the bit-exact model's z (scale of the AGC):
            // each width's own q_shift, Es/N0 on the sync symbols and the
            // ring MER of the 16APSK data symbols.
            for bits in [4u8, 5, 6, 7, 8] {
                let mut db = dd.clone();
                db.bits = bits;
                let p = Params::design(&db).unwrap();
                let lim = 1i64 << (bits - 1);
                let q: Vec<C> = out
                    .iter()
                    .skip(3 * SLOT)
                    .map(|s| {
                        let f = |v: i16| {
                            isdb_frontend::fixed::round_shift(i64::from(v), u32::from(p.q_shift))
                                .clamp(-lim, lim - 1) as f64
                        };
                        C::new(f(s.zk[0]), f(s.zk[1]))
                    })
                    .collect();
                let rq = s3sync::measure(&q).unwrap();
                let data = s3sync::derotated_data(&q, &rq);
                let v: Vec<[f64; 2]> = data.iter().map(|c| [c.re, c.im]).collect();
                let rings = isdb_frontend::metrics::ring_stats(&v, 2);
                let clipped = q
                    .iter()
                    .filter(|c| c.re.abs() >= (lim - 1) as f64 || c.im.abs() >= (lim - 1) as f64)
                    .count() as f64
                    / q.len() as f64;
                println!(
                    "    {bits} bits (q_shift {}): Es/N0 {:.2} dB, 16APSK ring MER {:.2} dB (radii {:.3} / {:.3}), at full scale {:.3} %",
                    p.q_shift,
                    rq.snr_db,
                    rings.mer_db,
                    rings.radii[0],
                    rings.radii[1],
                    clipped * 100.0
                );
            }
        }
    }
}
