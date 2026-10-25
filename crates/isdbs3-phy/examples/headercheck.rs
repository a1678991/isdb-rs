//! Check the slot headers of a recording: head-TLV indications and the
//! 160 undefined bits (B44 Fig. 3.2-1 Appendix 1: all ones) (development
//! tool).
//!
//! cargo run --release -p isdbs3-phy --example headercheck -- REC

use isdb_iq::Recording;
use isdbs3_core::slots::layout;
use isdbs3_phy::frontend::{Trim, symbols};
use isdbs3_phy::main_signal::{decode_frame, learned_points, main_symbols, pilots};
use isdbs3_phy::sync::synchronise;
use isdbs3_phy::tmcc::{decode, llrs};

fn main() {
    let path = std::path::PathBuf::from(std::env::args().nth(1).expect("REC"));
    let mut rec = Recording::open(&path, None, None).unwrap();
    let seg = rec.segments()[0].clone();
    let x = rec.read(seg.start, (seg.end - seg.start) as usize).unwrap();
    let s = symbols(&x, rec.sample_rate, None, Trim::Auto).unwrap();
    let sy = synchronise(&s.z).unwrap();
    let es = 10f64.powf(sy.sync_snr_db / 10.0);
    let (mut slots, mut ones_ok, mut ff) = (0, 0, 0);
    for &f in &sy.frames {
        let Some(l) = llrs(&s.z, &sy.slots, f, 1.0 / (2.0 * es)) else {
            break;
        };
        let t = decode(&l, 50).unwrap().info;
        let lay = layout(&t).unwrap();
        let (Some(ms), Some(pil)) = (main_symbols(&s.z, &sy.slots, f), pilots(&s.z, &sy.slots, f))
        else {
            break;
        };
        let pts = learned_points(&pil, &lay, lay[0].modulation).unwrap();
        let n0 = 0.04;
        for d in decode_frame(
            &ms,
            &lay,
            n0,
            &isdbs3_phy::main_signal::Ldpc::FLOAT,
            &|_, _| pts.clone(),
        ) {
            if d.bch_corrected.is_none() {
                continue;
            }
            slots += 1;
            if d.header[16..].iter().all(|&b| b == 1) {
                ones_ok += 1;
            }
            if d.header[..16].iter().all(|&b| b == 1) {
                ff += 1;
            }
        }
    }
    println!(
        "{}: {slots} slots; undefined 160 bits all ones in {ones_ok}; indication FFFFh in {ff}",
        path.display()
    );
}
