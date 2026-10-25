//! Check the pilot reading: the learned 16APSK points against the nominal
//! angles, and what averaging gives without the pilot PRBS signs
//! (development tool for SPEC-NOTES "pilot").
//!
//! cargo run --release -p isdbs3-phy --example pilotcheck -- REC

use isdb_iq::Recording;
use isdbs3_core::slots::layout;
use isdbs3_phy::frontend::{Trim, symbols};
use isdbs3_phy::main_signal::{learned_points, nominal_points, pilots};
use isdbs3_phy::sync::synchronise;
use isdbs3_phy::tmcc::{decode, llrs};

fn main() {
    let path = std::path::PathBuf::from(std::env::args().nth(1).expect("REC"));
    let mut rec = Recording::open(&path, None, None).unwrap();
    let seg = rec.segments()[0].clone();
    let n = ((seg.end - seg.start) as usize).min(6_000_000);
    let x = rec.read(seg.start, n).unwrap();
    let s = symbols(&x, rec.sample_rate, None, Trim::Auto).unwrap();
    let sy = synchronise(&s.z).expect("slots");
    let f = sy.frames[0];
    let es = 10f64.powf(sy.sync_snr_db / 10.0);
    let t = decode(&llrs(&s.z, &sy.slots, f, 1.0 / (2.0 * es)).unwrap(), 50)
        .unwrap()
        .info;
    let lay = layout(&t).unwrap();
    let pil = pilots(&s.z, &sy.slots, f).unwrap();
    let m = lay[0].modulation;
    let nom = nominal_points(m, lay[0].rate);
    let lp = learned_points(&pil, &lay, m).unwrap();
    println!("label  nominal(deg, r)   learned(deg, r)");
    let mut worst: f64 = 0.0;
    for (i, (a, b)) in nom.iter().zip(&lp).enumerate() {
        let d = (b / a).arg().to_degrees();
        worst = worst.max(d.abs());
        println!(
            "{i:04b}   {:7.1} {:.3}    {:7.1} {:.3}   Δ {d:+.1}°",
            a.arg().to_degrees(),
            a.norm(),
            b.arg().to_degrees(),
            b.norm()
        );
    }
    println!("largest angle error {worst:.1}°");
    // without the pilot PRBS signs: pilots() multiplies by the sign, so
    // multiply back
    let signs = isdbs3_core::dispersal::pilot_signs();
    let raw: Vec<_> = pil
        .iter()
        .zip(&signs)
        .map(|(v, &s)| v * f64::from(s))
        .collect();
    let rp = learned_points(&raw, &lay, m).unwrap();
    let mean_r =
        |p: &[num_complex::Complex<f64>]| p.iter().map(|v| v.norm()).sum::<f64>() / p.len() as f64;
    println!(
        "mean |point| with signs {:.3}, without signs {:.3}",
        mean_r(&lp),
        mean_r(&rp)
    );
}
