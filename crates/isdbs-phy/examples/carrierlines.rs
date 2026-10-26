//! The ISDB-S carrier lines of a recording (z², z⁴, z⁸ and the strongest),
//! and whether the strongest is corroborated by another line within 2 kHz
//! (development tool; plan §0.16: at 40 Msps several z^k lines alias).
//!
//! cargo run --release -p isdbs-phy --example carrierlines -- REC [REC…]

use isdb_iq::Recording;
use isdb_iq::survey::{Options, survey};

fn main() {
    for path in std::env::args().skip(1) {
        let mut rec = Recording::open(std::path::Path::new(&path), None, None).expect("open");
        let fs = rec.sample_rate;
        let seg = rec.segments()[0].clone();
        let n = ((seg.end - seg.start) as usize).min(1 << 22);
        let x = rec.read(seg.start, n).expect("read");
        let sv = survey(&x, fs, &Options::default());
        let best = sv.best_carrier().map(|c| c.offset_hz);
        let lines: Vec<String> = sv
            .carrier_lines
            .iter()
            .map(|c| {
                format!(
                    "z^{}{:+.0}: {:+.0} Hz {:.1} dB",
                    c.k, c.shift_hz, c.offset_hz, c.line.level_db
                )
            })
            .collect();
        let corroborated = best.is_some_and(|b| {
            sv.carrier_lines
                .iter()
                .filter(|c| (c.offset_hz - b).abs() < 2_000.0)
                .count()
                >= 2
        });
        println!(
            "{}: fs {:.2} MHz, strongest {:+.0} Hz ({}); {}",
            std::path::Path::new(&path)
                .file_name()
                .unwrap()
                .to_string_lossy(),
            fs / 1e6,
            best.unwrap_or(f64::NAN),
            if corroborated {
                "corroborated"
            } else {
                "ALONE"
            },
            lines.join(" | ")
        );
    }
}
