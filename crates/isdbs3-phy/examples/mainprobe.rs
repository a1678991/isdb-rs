//! Decode the main signal of the first frames of a recording and report
//! LDPC/BCH results per slot, with the nominal and the pilot-learned
//! constellations (development tool).
//!
//! cargo run --release -p isdbs3-phy --example mainprobe -- REC [FRAMES]

use isdb_iq::Recording;
use isdbs3_core::slots::layout;
use isdbs3_phy::frontend::{Trim, symbols};
use isdbs3_phy::main_signal::{decode_frame, learned_points, main_symbols, nominal_points, pilots};
use isdbs3_phy::sync::synchronise;
use isdbs3_phy::tmcc::{decode, llrs};

fn main() {
    let mut a = std::env::args().skip(1);
    let path = std::path::PathBuf::from(a.next().expect("REC"));
    let frames: usize = a.next().map_or(2, |s| s.parse().unwrap());
    let mut rec = Recording::open(&path, None, None).unwrap();
    let seg = rec.segments()[0].clone();
    let x = rec.read(seg.start, (seg.end - seg.start) as usize).unwrap();
    let s = symbols(&x, rec.sample_rate, None, Trim::Auto).unwrap();
    let sy = synchronise(&s.z).expect("slots");
    let es_n0 = 10f64.powf(sy.sync_snr_db / 10.0);
    for &f in sy.frames.iter().take(frames) {
        let Some(l) = llrs(&s.z, &sy.slots, f, 1.0 / (2.0 * es_n0)) else {
            continue;
        };
        let t = decode(&l, 50).expect("TMCC").info;
        let lay = layout(&t).expect("layout");
        let ms = main_symbols(&s.z, &sy.slots, f).unwrap();
        let pil = pilots(&s.z, &sy.slots, f).unwrap();
        let m = lay[0].modulation;
        let nominal = nominal_points(m, lay[0].rate);
        let learned = learned_points(&pil, &lay, m).unwrap();
        println!("frame at slot {f}: {} {}", m.name(), lay[0].rate);
        let rad = |p: &[num_complex::Complex<f64>]| -> String {
            let mut r: Vec<f64> = p.iter().map(|v| v.norm()).collect();
            r.sort_by(f64::total_cmp);
            format!(
                "inner {:.3} outer {:.3} ratio {:.3}",
                r[0],
                r[r.len() - 1],
                r[r.len() - 1] / r[0]
            )
        };
        println!("  nominal points: {}", rad(&nominal));
        println!("  learned points: {}", rad(&learned));
        for (name, pts) in [("nominal", nominal.clone()), ("learned", learned.clone())] {
            // noise: mean distance of the main symbols to their nearest point
            let n0 = ms
                .iter()
                .take(200_000)
                .map(|y| {
                    pts.iter()
                        .map(|p| (y - p).norm_sqr())
                        .fold(f64::MAX, f64::min)
                })
                .sum::<f64>()
                / 200_000.0;
            let t0 = std::time::Instant::now();
            let out = decode_frame(&ms, &lay, n0, 50, &|_, _| pts.clone());
            let dt = t0.elapsed().as_secs_f64();
            let ok = out.iter().filter(|d| d.ldpc_iterations.is_some()).count();
            let bch = out.iter().filter(|d| d.bch_corrected.is_some()).count();
            let it: usize = out.iter().filter_map(|d| d.ldpc_iterations).sum();
            let corr: usize = out.iter().filter_map(|d| d.bch_corrected).sum();
            println!(
                "  {name}: MER {:.2} dB; LDPC {ok}/{} converged (mean {:.1} it), BCH ok {bch} ({corr} bits corrected); {dt:.2} s",
                -10.0 * n0.log10(),
                out.len(),
                it as f64 / ok.max(1) as f64
            );
            if let Some(d) = out.iter().find(|d| d.bch_corrected.is_some()) {
                let h: String = d
                    .header
                    .chunks(8)
                    .take(4)
                    .map(|c| format!("{:02x}", c.iter().fold(0u8, |a, &b| a << 1 | b)))
                    .collect();
                let dd: String = d.data.iter().take(16).map(|b| format!("{b:02x}")).collect();
                println!("    slot {} header {h}… data {dd}…", d.slot);
            }
        }
    }
}
