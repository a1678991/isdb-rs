//! Decode the ISDB-S3 TMCC of every whole frame in a recording and print
//! what it says (development tool for the readings in SPEC-NOTES).
//!
//! cargo run --release -p isdbs3-phy --example tmccprobe -- REC

use isdb_iq::Recording;
use isdbs3_core::tmcc::{ModulationCode, StreamType};
use isdbs3_phy::frontend::{Trim, symbols};
use isdbs3_phy::sync::synchronise;
use isdbs3_phy::tmcc::{decode, llrs};

fn main() {
    let path = std::path::PathBuf::from(std::env::args().nth(1).expect("REC"));
    let mut rec = Recording::open(&path, None, None).unwrap();
    let seg = rec.segments()[0].clone();
    let x = rec.read(seg.start, (seg.end - seg.start) as usize).unwrap();
    let s = symbols(&x, rec.sample_rate, None, Trim::Auto).unwrap();
    println!(
        "{}: {} symbols, cfo {:.0} Hz, trim {:.3}, survey ISDB-S3 {}",
        path.display(),
        s.z.len(),
        s.carrier_offset_hz,
        s.trim,
        s.survey_says_isdbs3
    );
    let sy = synchronise(&s.z).expect("slots");
    println!(
        "slots {}, frame heads at slots {:?}, sync Es/N0 {:.2} dB",
        sy.slots.len(),
        sy.frames,
        sy.sync_snr_db
    );
    let es_n0 = 10f64.powf(sy.sync_snr_db / 10.0);
    let noise_var = 1.0 / (2.0 * es_n0);
    for &f in &sy.frames {
        let Some(l) = llrs(&s.z, &sy.slots, f, noise_var) else {
            println!("frame at slot {f}: incomplete");
            continue;
        };
        let neg = l.iter().filter(|v| **v < 0.0).count();
        match decode(&l, 50) {
            Ok(d) => {
                let t = &d.info;
                println!(
                    "frame at slot {f}: OK ({} LDPC iterations, {} BCH bits) change {} alloc_ok {} ews {} uplink {:03b} reserved {:04b} ext {:#06x} all-ones {}",
                    d.ldpc_iterations,
                    d.bch_corrected,
                    t.change,
                    t.allocation_ok(),
                    t.ews,
                    t.uplink,
                    t.reserved,
                    t.extension_id,
                    t.extension_all_ones
                );
                for (i, m) in t.modes.iter().enumerate() {
                    if m.slots > 0 || m.modulation_code != ModulationCode::Unused {
                        println!(
                            "  mode {}: mod {:04b} ({}) rate {:04b} ({}) slots {} obo {:.1} dB",
                            i + 1,
                            m.modulation_code.code(),
                            m.modulation_code,
                            m.rate_code.code(),
                            m.rate_code,
                            m.slots,
                            f64::from(m.obo) / 10.0
                        );
                    }
                }
                for k in 0..16 {
                    if k < 4 || t.stream_type[k] != StreamType::None || t.stream_id[k] != 0xFFFF {
                        println!(
                            "  rel stream {k}: type {:#04x} ({}) id {:#06x} format {:?}",
                            t.stream_type[k].code(),
                            t.stream_type[k],
                            t.stream_id[k],
                            t.packet_format[k]
                        );
                    }
                }
                let mut runs: Vec<(u8, usize)> = Vec::new();
                for &s in &t.slot_stream {
                    match runs.last_mut() {
                        Some((v, n)) if *v == s => *n += 1,
                        _ => runs.push((s, 1)),
                    }
                }
                println!("  slot streams (value×count): {runs:?}");
                println!("  pointers slot1..4: {:?}", &t.pointers[..4]);
            }
            Err(e) => println!(
                "frame at slot {f}: {e} ({neg} of {} negative LLRs)",
                l.len()
            ),
        }
    }
}
