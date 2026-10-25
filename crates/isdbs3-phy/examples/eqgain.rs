//! What the pilot-trained equaliser gains: sync-symbol Es/N0 and main
//! MER (learned constellation) before and after, and whether every
//! codeword still decodes (development tool).
//!
//! cargo run --release -p isdbs3-phy --example eqgain -- REC [FRAMES]

use isdb_iq::Recording;
use isdbs3_core::slots::layout;
use isdbs3_phy::eq::{apply, train};
use isdbs3_phy::frontend::{Trim, symbols};
use isdbs3_phy::main_signal::{decode_frame, learned_points, main_symbols, pilots};
use isdbs3_phy::sync::synchronise;
use isdbs3_phy::tmcc::{decode, llrs};

fn mer(ms: &[num_complex::Complex<f64>], pts: &[num_complex::Complex<f64>]) -> f64 {
    let e = ms
        .iter()
        .step_by(7)
        .map(|y| {
            pts.iter()
                .map(|p| (y - p).norm_sqr())
                .fold(f64::MAX, f64::min)
        })
        .sum::<f64>()
        / ms.iter().step_by(7).count() as f64;
    -10.0 * e.log10()
}

fn main() {
    let mut a = std::env::args().skip(1);
    let path = std::path::PathBuf::from(a.next().expect("REC"));
    let frames: usize = a.next().map_or(4, |s| s.parse().unwrap());
    let mut rec = Recording::open(&path, None, None).unwrap();
    let seg = rec.segments()[0].clone();
    let x = rec.read(seg.start, (seg.end - seg.start) as usize).unwrap();
    let s = symbols(&x, rec.sample_rate, None, Trim::Auto).unwrap();
    let sy = synchronise(&s.z).unwrap();
    let es = 10f64.powf(sy.sync_snr_db / 10.0);
    println!(
        "{}: sync Es/N0 {:.2} dB (before)",
        path.display(),
        sy.sync_snr_db
    );
    let mut first_taps = None;
    for &f in sy.frames.iter().take(frames) {
        let Some(l) = llrs(&s.z, &sy.slots, f, 1.0 / (2.0 * es)) else {
            break;
        };
        let t = decode(&l, 50).unwrap().info;
        let lay = layout(&t).unwrap();
        let w = train(&s.z, &sy.slots, f, &lay).expect("train");
        if first_taps.is_none() {
            first_taps = Some(w.clone());
        }
        // equalise the whole stream with this frame's taps and re-measure
        let ze = apply(&s.z, &w);
        let sye = synchronise(&ze).unwrap();
        let fe = sye
            .frames
            .iter()
            .copied()
            .find(|&g| sye.slots[g].at == sy.slots[f].at)
            .unwrap_or(f);
        for (name, z, syx, fx) in [("before", &s.z, &sy, f), ("after", &ze, &sye, fe)] {
            let ms = main_symbols(z, &syx.slots, fx).unwrap();
            let pil = pilots(z, &syx.slots, fx).unwrap();
            let pts = learned_points(&pil, &lay, lay[0].modulation).unwrap();
            let m = mer(&ms, &pts);
            let n0 = 10f64.powf(-m / 10.0);
            let out = decode_frame(
                &ms,
                &lay,
                n0,
                &isdbs3_phy::main_signal::Ldpc::FLOAT,
                &|_, _| pts.clone(),
            );
            let ok = out.iter().filter(|d| d.bch_corrected.is_some()).count();
            let it: usize = out.iter().filter_map(|d| d.ldpc_iterations).sum();
            println!(
                "  frame at slot {f} {name}: main MER {m:.2} dB, {ok}/{} BCH-clean, mean LDPC iterations {:.2}",
                out.len(),
                it as f64 / out.len() as f64
            );
        }
        println!("  sync Es/N0 after: {:.2} dB", sye.sync_snr_db);
    }
    if let Some(w) = first_taps {
        let mags: Vec<String> = w.iter().map(|c| format!("{:.3}", c.norm())).collect();
        println!("taps |w|: {}", mags.join(" "));
    }
}
