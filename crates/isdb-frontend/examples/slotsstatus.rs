//! Frame by frame, what a recorded `Slots` stream says about the chain:
//! for each status block the frame, the taps in use (their frame and
//! centre tap), the latched sums' frame, n0 and the LLR scale, the frame
//! the constellation was learned on, and the frame's slot blocks (valid,
//! failed, mean iterations). For finding when a board run's decoding
//! changed and what the chain had in force then.
//!
//! `cargo run --release -p isdb-frontend --example slotsstatus -- FILE.slots [EVERY]`
//!
//! EVERY: print every n-th status block, and every block whose frame had
//! a failed slot or whose taps changed (default 1). At the end, the
//! totals (slots, not converged, dropped: 0 iterations, the decoder had no
//! bank; frames with drops), the failures per minute of signal, and the
//! last status block's learned constellation points.

use std::collections::BTreeMap;
use std::io::Read;

use isdb_frontend::slots::{Block, Event, Reader};

#[derive(Default)]
struct FrameSlots {
    valid: u32,
    failed: u32,
    /// Not converged with 0 iterations: the decoder dropped the codeword
    /// (no free bank) rather than failing it.
    dropped: u32,
    iterations: u64,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    let path = args.get(1).ok_or("usage: slotsstatus FILE.slots [EVERY]")?;
    let every: u32 = args.get(2).map_or(Ok(1), |s| s.parse())?;
    let mut f = std::fs::File::open(path)?;
    let mut reader = Reader::new();
    let mut events = Vec::new();
    let mut buf = vec![0u8; 1 << 22];
    let mut slots: BTreeMap<u32, FrameSlots> = BTreeMap::new();
    let mut prev_taps_from = None;
    let mut n = 0u32;
    let mut last = None;
    // totals, and failures per minute of signal (30.26 frames/s)
    let (mut t_frames, mut t_valid, mut t_failed, mut t_dropped) = (0u32, 0u64, 0u64, 0u64);
    let (mut drop_frames, mut drop_max) = (0u32, 0u32);
    let mut first_frame = None;
    let mut per_minute: BTreeMap<u32, u64> = BTreeMap::new();
    println!(
        "frame  taps_from  centre_tap      sums_frame  n0      llr_scale  learned_from  slots(valid/failed)  mean_iter  dropped"
    );
    loop {
        let got = f.read(&mut buf)?;
        if got == 0 {
            break;
        }
        reader.feed_bytes(&buf[..got], &mut events);
        for e in events.drain(..) {
            match e {
                Event::Block {
                    block: Block::Slot(s),
                    ..
                } => {
                    if !s.dummy {
                        let e = slots.entry(s.frame).or_default();
                        e.valid += 1;
                        e.iterations += u64::from(s.iterations);
                        if !s.converged {
                            e.failed += 1;
                            if s.iterations == 0 {
                                e.dropped += 1;
                            }
                        }
                    }
                }
                Event::Block {
                    block: Block::Status(st),
                    ..
                } => {
                    let fs = slots.remove(&st.frame).unwrap_or_default();
                    let f0 = *first_frame.get_or_insert(st.frame);
                    t_frames += 1;
                    t_valid += u64::from(fs.valid);
                    t_failed += u64::from(fs.failed);
                    t_dropped += u64::from(fs.dropped);
                    if fs.dropped > 0 {
                        drop_frames += 1;
                        drop_max = drop_max.max(fs.dropped);
                    }
                    *per_minute
                        .entry(st.frame.saturating_sub(f0) / 1816)
                        .or_default() += u64::from(fs.failed);
                    let changed = prev_taps_from != Some(st.taps_from);
                    prev_taps_from = Some(st.taps_from);
                    if n.is_multiple_of(every) || fs.failed > 0 || changed {
                        let mean = if fs.valid > 0 {
                            fs.iterations as f64 / f64::from(fs.valid)
                        } else {
                            0.0
                        };
                        println!(
                            "{:5}  {:>9}  {:>6},{:>6}   {:>10}  {:6}  {:9}  {:>12}  {:4} / {:4}          {:.2}  {}",
                            st.frame,
                            st.taps_from.map_or("-".into(), |v| v.to_string()),
                            st.taps[5][0],
                            st.taps[5][1],
                            st.sums_frame.map_or("-".into(), |v| v.to_string()),
                            st.n0,
                            st.llr_scale,
                            st.learned_from.map_or("-".into(), |v| v.to_string()),
                            fs.valid,
                            fs.failed,
                            mean,
                            fs.dropped
                        );
                    }
                    n += 1;
                    last = Some(st);
                }
                Event::Loss { blocks, words } => println!("loss: {blocks} blocks, {words} words"),
                _ => {}
            }
        }
    }
    println!(
        "SUMMARY {t_frames} status blocks, {t_valid} valid slots, {t_failed} not converged \
         ({t_dropped} dropped: 0 iterations); {drop_frames} frames with drops, at most {drop_max} per frame"
    );
    let pm: Vec<String> = per_minute.values().map(u64::to_string).collect();
    println!("SUMMARY failures per minute of signal: {}", pm.join(" "));
    // the learned constellation of the last status block, per modulation
    // (count 0: the nominal points in force), Q.10, with each point's
    // radius, for a look at whether it is a plausible constellation
    if let Some(st) = last {
        let names = ["pi/2-BPSK", "QPSK", "8PSK", "16APSK", "32APSK"];
        for (name, pts) in names.iter().zip(&st.constellation) {
            if pts.is_empty() {
                continue;
            }
            println!(
                "learned {name} at frame {} ({} points, Q.10, re im radius):",
                st.frame,
                pts.len()
            );
            for (i, p) in pts.iter().enumerate() {
                let r = f64::from(p[0]).hypot(f64::from(p[1]));
                println!("  {i:2}: {:6} {:6}  {r:7.1}", p[0], p[1]);
            }
        }
    }
    Ok(())
}
