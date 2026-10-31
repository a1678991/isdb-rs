//! Where a streamed reception's decode call spends its time, stage by
//! stage, on the path `isdb rx --symbols` / the FPGA stream takes once
//! locked: carrier derotation, the slot sync measured at the known offset
//! (no search), equaliser training and application, the sync measured
//! again, the per-frame preparation (TMCC, main symbols, pilots) and the
//! main signal's decode. Stages that `rx::receive` runs on one thread show
//! up as a large share of the wall time with the cores idle, which is what
//! leaves a live receiver no margin (development tool; the 2026-11-01
//! BS17 run that fell behind after 1975 s).
//!
//! With FAIL, the main signal is decoded once more with the symbols of
//! FAIL codewords replaced by noise, which fail after the LDPC's maximum
//! iterations (the live BS17 run failed ≈ 7 codewords per 24 frames).
//!
//! cargo run --release -p isdbs3-phy --example streamprof -- WORDS_FILE [FRAMES [FAIL]]

use isdb_frontend::format::{Event, Reader};
use isdbs3_core::params::FRAME_SYMBOLS;
use isdbs3_core::slots::layout;
use isdbs3_phy::Cplx;
use isdbs3_phy::main_signal::{
    FrameInput, Ldpc, decode_frames, learned_points, main_symbols, nominal_points, pilots,
};
use isdbs3_phy::rx::{RxOptions, derotate, noise_of, receive};
use isdbs3_phy::sync::{synchronise, synchronise_at};
use isdbs3_phy::{eq, tmcc};
use std::io::Read;
use std::time::Instant;

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let frames: usize = a.get(2).map_or(24, |s| s.parse().unwrap());
    let fail: usize = a.get(3).map_or(0, |s| s.parse().unwrap());
    let want = (frames + 1) * FRAME_SYMBOLS;
    // read only as much of the words file as the chunk needs
    let mut f = std::fs::File::open(&a[1]).expect("open words");
    let mut r = Reader::new();
    let mut ev = Vec::new();
    let mut z: Vec<Cplx> = Vec::new();
    let mut buf = vec![0u8; 1 << 20];
    while z.len() < want {
        let n = f.read(&mut buf).expect("read");
        if n == 0 {
            break;
        }
        r.feed_bytes(&buf[..n], &mut ev);
        for e in ev.drain(..) {
            if let Event::Block { symbols, .. } = e {
                z.extend(
                    symbols
                        .iter()
                        .map(|s| Cplx::new(f64::from(s[0]), f64::from(s[1]))),
                );
            }
        }
    }
    // start on a frame, as a continuing stream's call does
    let first = synchronise(&z).expect("sync");
    let z0 = first.slots[first.frames[0]].at;
    let z = &z[z0..(z0 + want).min(z.len())];
    let secs = z.len() as f64 / isdbs3_core::params::SYMBOL_RATE;
    let carrier = isdbs3_phy::sync::residual_hz(&first).unwrap_or(0.0);
    println!(
        "{} symbols = {secs:.3} s of signal; residual carrier {carrier:+.1} Hz",
        z.len()
    );
    for _ in 0..3 {
        let mut lap = Instant::now();
        let mut total = 0.0;
        let mut step = |name: &str, lap: &mut Instant| {
            let t = lap.elapsed().as_secs_f64();
            total += t;
            println!("  {name:34} {t:7.3} s");
            *lap = Instant::now();
        };
        let zd = derotate(z, carrier);
        step("derotate", &mut lap);
        let sy0 = synchronise_at(&zd, 0).expect("sync at 0");
        step("synchronise_at (raw)", &mut lap);
        let es = 10f64.powf(sy0.sync_snr_db / 10.0);
        let f0 = sy0.frames[0];
        let l = tmcc::llrs(&zd, &sy0.slots, f0, 1.0 / (2.0 * es)).expect("llrs");
        let t = tmcc::decode(&l, 50).expect("tmcc");
        let lay = layout(&t.info).expect("layout");
        let w = eq::train(&zd, &sy0.slots, f0, &lay).expect("train");
        step("first TMCC + equaliser train", &mut lap);
        let zeq = eq::apply(&zd, &w);
        step("equaliser apply", &mut lap);
        let sy = synchronise_at(&zeq, sy0.offset).expect("sync");
        step("synchronise_at (equalised)", &mut lap);
        let es = 10f64.powf(sy.sync_snr_db / 10.0);
        let preps: Vec<_> = std::thread::scope(|sc| {
            let hs: Vec<_> = sy
                .frames
                .iter()
                .map(|&f| {
                    let (zeq, sy) = (&zeq, &sy);
                    sc.spawn(move || {
                        let l = tmcc::llrs(zeq, &sy.slots, f, 1.0 / (2.0 * es))?;
                        let t = tmcc::decode(&l, 50).ok()?;
                        let lay = layout(&t.info)?;
                        Some((
                            lay,
                            main_symbols(zeq, &sy.slots, f)?,
                            pilots(zeq, &sy.slots, f)?,
                        ))
                    })
                })
                .collect();
            hs.into_iter().filter_map(|h| h.join().unwrap()).collect()
        });
        step("prepare frames (parallel)", &mut lap);
        let pts: Vec<_> = preps
            .iter()
            .map(|(lay, _, pil)| {
                move |m, r| learned_points(pil, lay, m).unwrap_or_else(|| nominal_points(m, r))
            })
            .collect();
        let n0s: Vec<f64> = preps
            .iter()
            .zip(&pts)
            .map(|((lay, ms, _), p)| noise_of(ms, lay, p))
            .collect();
        step("noise_of (sequential)", &mut lap);
        let inputs: Vec<FrameInput<'_>> = preps
            .iter()
            .zip(&pts)
            .zip(&n0s)
            .map(|(((lay, ms, _), p), &n0)| FrameInput {
                ms,
                layout: lay,
                n0,
                pts_for: p,
            })
            .collect();
        let d = decode_frames(&inputs, &Ldpc::FIXED);
        step("decode_frames (demap, LDPC, BCH)", &mut lap);
        if fail > 0 {
            // the same frames with the main symbols of `fail` codewords
            // replaced by noise (a weak signal's failing codewords, which
            // run the LDPC's maximum iterations)
            let mut noisy: Vec<Vec<Cplx>> = preps.iter().map(|(_, ms, _)| ms.clone()).collect();
            let mut seed = 0x2545_f491_4f6c_dd1du64;
            let mut rnd = || {
                seed ^= seed << 13;
                seed ^= seed >> 7;
                seed ^= seed << 17;
                (seed >> 11) as f64 / (1u64 << 53) as f64 - 0.5
            };
            for k in 0..fail {
                let fi = k % noisy.len();
                let lay = &preps[fi].0;
                let used: Vec<_> = lay.iter().filter(|s| !s.dummy).collect();
                let s = used[(k * 37) % used.len()];
                for v in &mut noisy[fi][s.symbol_start..s.symbol_start + s.symbols] {
                    *v = Cplx::new(2.0 * rnd(), 2.0 * rnd());
                }
            }
            let inputs: Vec<FrameInput<'_>> = preps
                .iter()
                .zip(&noisy)
                .zip(&pts)
                .zip(&n0s)
                .map(|((((lay, _, _), ms), p), &n0)| FrameInput {
                    ms,
                    layout: lay,
                    n0,
                    pts_for: p,
                })
                .collect();
            let t = Instant::now();
            let d = decode_frames(&inputs, &Ldpc::FIXED);
            let failed = d
                .iter()
                .flatten()
                .filter(|s| s.ldpc_iterations.is_none())
                .count();
            println!(
                "  {:34} {:7.3} s ({failed} LDPC failures; not in the sum)",
                format!("decode_frames, {fail} noise codewords"),
                t.elapsed().as_secs_f64()
            );
            lap = Instant::now();
        }
        drop((zd, zeq));
        step("free buffers", &mut lap);
        let slots: usize = d.iter().map(Vec::len).sum();
        println!(
            "  {:34} {total:7.3} s = {:.2}× real time ({} frames, {slots} slots)",
            "sum of stages",
            secs / total,
            preps.len()
        );
        let t = Instant::now();
        let rx = receive(
            z,
            &RxOptions {
                slot_offset: Some(0),
                carrier_hz: carrier,
                ..RxOptions::default()
            },
        );
        let t = t.elapsed().as_secs_f64();
        println!(
            "  {:34} {t:7.3} s = {:.2}× real time ({} frames)",
            "receive (hinted, whole)",
            secs / t,
            rx.frames.len()
        );
        let t = Instant::now();
        let rx = receive(
            z,
            &RxOptions {
                carrier_hz: carrier,
                ..RxOptions::default()
            },
        );
        let t = t.elapsed().as_secs_f64();
        println!(
            "  {:34} {t:7.3} s = {:.2}× real time ({} frames)",
            "receive (searched, after a loss)",
            secs / t,
            rx.frames.len()
        );
    }
}
