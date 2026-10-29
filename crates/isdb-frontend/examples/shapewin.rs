//! Which window for the transmit pulse ([`isdb_frontend::shape`]): for a
//! root-raised cosine of roll-off α cut to ±`span` symbols under no window
//! (truncation) or a Kaiser window of shape β, the intersymbol
//! interference of the cascade with the same pulse as matched filter (the
//! MER bound for random symbols, sampled at the exact symbol instants),
//! the energy outside the occupied band (1 + α)·R/2 and the highest
//! spectral level beyond the band edge + R/20 (development tool).
//!
//! cargo run --release -p isdb-frontend --example shapewin -- [ALPHA] [SPAN…]

use isdb_frontend::tables::{kaiser, rrc};
use std::f64::consts::TAU;

/// Points per symbol of the numerical integrals.
const STEPS: usize = 512;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let alpha: f64 = args.get(1).map_or(0.35, |s| s.parse().unwrap());
    let spans: Vec<usize> = if args.len() > 2 {
        args[2..].iter().map(|s| s.parse().unwrap()).collect()
    } else {
        vec![8, 12, 16, 24, 32]
    };
    println!(
        "roll-off {alpha}: window, span → ISI MER bound, energy outside the band, peak level beyond edge + R/20"
    );
    for &span in &spans {
        for beta in [0.0, 1.0, 2.0, 3.0, 5.0] {
            let half = span as f64;
            let p = |t: f64| {
                if t.abs() >= half {
                    0.0
                } else {
                    rrc(t, alpha)
                        * if beta > 0.0 {
                            kaiser(t, half, beta)
                        } else {
                            1.0
                        }
                }
            };
            // The pulse on a grid of 1/STEPS symbol over ±span.
            let n = 2 * span * STEPS;
            let h = 1.0 / STEPS as f64;
            let grid: Vec<f64> = (0..=n).map(|i| p(i as f64 * h - half)).collect();
            // Cascade at integer k: Σ p(τ) p(k − τ) h on the grid (k·STEPS
            // grid points apart, so both factors are grid values).
            let cascade = |k: usize| -> f64 {
                let s = k * STEPS;
                (0..=n.saturating_sub(s))
                    .map(|i| grid[i] * grid[i + s])
                    .sum::<f64>()
                    * h
            };
            let c0 = cascade(0);
            let isi: f64 = (1..2 * span).map(|k| 2.0 * cascade(k).powi(2)).sum();
            // Spectrum P(ν) = Σ p(τ) cos(2πντ) h (p is even).
            let spec = |nu: f64| -> f64 {
                grid.iter()
                    .enumerate()
                    .map(|(i, &v)| v * (TAU * nu * (i as f64 * h - half)).cos())
                    .sum::<f64>()
                    * h
            };
            let edge = (1.0 + alpha) / 2.0;
            let (mut inside, mut outside, mut peak) = (0.0, 0.0, 0.0f64);
            let dn = 1.0 / 1024.0;
            let mut nu = dn / 2.0;
            while nu < 2.0 {
                let e = spec(nu).powi(2);
                if nu < edge {
                    inside += e;
                } else {
                    outside += e;
                }
                if nu > edge + 0.05 {
                    peak = peak.max(e);
                }
                nu += dn;
            }
            let p0 = spec(0.0).powi(2);
            let name = if beta > 0.0 {
                format!("Kaiser β {beta}")
            } else {
                "truncation".to_owned()
            };
            println!(
                "  {name:13} span {span:2}: ISI MER {:6.1} dB, outside {:6.1} dB, peak {:6.1} dB, energy {:.5}",
                -10.0 * (isi / (c0 * c0)).log10(),
                10.0 * (outside / (inside + outside)).log10(),
                10.0 * (peak / p0).log10(),
                c0
            );
        }
    }
}
