//! Per segment of an ISDB-S3 recording: the survey's carrier lines and the
//! one `isdbs3_phy::frontend::symbols` would take, then the sync Es/N0 the
//! front end reaches with it (development tool for the block that decoded
//! no TMCC, plan §0.8).
//!
//! cargo run --release -p isdbs3-phy --example blockcarrier -- REC

use isdb_iq::Recording;
use isdb_iq::survey::{Options, survey};
use isdbs3_phy::frontend::{Trim, carrier_candidates, symbols};
use isdbs3_phy::sync::synchronise;

fn main() {
    let path = std::path::PathBuf::from(std::env::args().nth(1).expect("REC"));
    let mut rec = Recording::open(&path, None, None).expect("open");
    let fs = rec.sample_rate;
    for (i, seg) in rec.segments().clone().iter().enumerate() {
        let n = (usize::try_from(seg.end - seg.start).expect("segment length")).min(6_000_000);
        let x = rec.read(seg.start, n).expect("read");
        let sv = survey(&x[..x.len().min(1 << 22)], fs, &Options::default());
        println!("segment {i}: {n} samples");
        for c in &sv.carrier_lines {
            println!(
                "  z^{} shift {:+9.0} Hz: line {:+10.0} Hz {:+5.1} dB -> carrier {:+9.0} Hz",
                c.k, c.shift_hz, c.line.freq_hz, c.line.level_db, c.offset_hz
            );
        }
        let strongest = sv.best_carrier().map_or(0.0, |c| c.offset_hz);
        let cands: Vec<String> = carrier_candidates(&sv)
            .iter()
            .map(|c| format!("{c:+.0}"))
            .collect();
        let s = symbols(&x, fs, None, Trim::Auto).expect("front end");
        let es = synchronise(&s.z).map_or(f64::NAN, |y| y.sync_snr_db);
        println!(
            "  strongest line {strongest:+.0} Hz; candidates [{}] Hz; picked {:+.0} Hz, trim {:+.3}: sync Es/N0 {es:.2} dB",
            cands.join(", "),
            s.carrier_offset_hz,
            s.trim
        );
    }
}
