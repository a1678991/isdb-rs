//! `isdb tx --feed live`: an input that stalls must be padded with nulls
//! in real time, so the IQ output stays constant-bit-rate, and every input
//! packet must still come out of `rx`.
//!
//! This is the one test that compares the output with the wall clock (the
//! producer must keep up with real time), so it is a test binary of its
//! own: cargo runs test binaries one after another, and no other test's
//! modulator or decoder competes for the CPU. In `cli.rs` it shared the
//! cores with the PHY loopbacks and fell to 0.1–0.2× real time on CI
//! runners (alone it runs at ≈ 4× real time on a single core).

use std::io::Write;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

mod common;

use common::{FRAME_S, ISDB, SAMPLES_PER_FRAME, assert_same, packets, run, s, tmp};

#[test]
fn live_feed_with_stalling_input_stays_cbr_and_lossless() {
    let d = tmp("live");
    let iq = d.join("x.cs8");
    let mut child = Command::new(ISDB)
        .args([
            "tx",
            "--b-ts",
            "-",
            "--feed",
            "live",
            "--iq",
            s(&iq),
            "--iq-format",
            "cs8",
            "--status-secs",
            "0.5",
        ])
        .stdin(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let t0 = Instant::now();
    // 3000 packets, a 1.5 s stall (≈ 6.5 frames with no input), 3000 more.
    stdin.write_all(&packets(0x0111, 0..3000)).unwrap();
    std::thread::sleep(Duration::from_millis(1500));
    stdin.write_all(&packets(0x0111, 3000..6000)).unwrap();
    drop(stdin);
    let o = child.wait_with_output().unwrap();
    let wall = t0.elapsed().as_secs_f64();
    let err = String::from_utf8_lossy(&o.stderr);
    eprintln!("{err}");
    assert!(o.status.success());

    // Constant bit rate: a whole number of frames, paced to real time, and
    // the stall was filled with frames (not skipped).
    let len = std::fs::metadata(&iq).unwrap().len();
    assert_eq!(len % (SAMPLES_PER_FRAME * 2), 0);
    let frames = len / (SAMPLES_PER_FRAME * 2);
    let air = frames as f64 * FRAME_S;
    // A skipped stall would leave only the data's ≈ 1.07 s; half the stall
    // on top of that proves it was filled, with room for a slow machine
    // (the full stall was missed by one frame there, plan §0.5).
    assert!(
        air >= 0.75 + 2.0 * 6000.0 / 2592.0 * FRAME_S,
        "{frames} frames for a 1.5 s stall"
    );
    assert!(
        (air - wall).abs() < 1.0,
        "air time {air:.2} s vs wall {wall:.2} s"
    );
    // Null fill reported periodically and in the summary.
    assert!(
        err.matches("null fill").count() >= 2,
        "periodic null-fill status"
    );
    let fill: f64 = err
        .lines()
        .find(|l| l.contains("layer B:") && l.contains("null fill"))
        .and_then(|l| l.split('(').nth(1))
        .and_then(|t| t.split('%').next())
        .map(|t| t.trim().parse().unwrap())
        .expect("summary line");
    assert!(fill > 30.0, "null fill {fill}%");

    // Every input packet arrives, in order, with only nulls in between.
    let o = run(&[
        "rx",
        "--iq",
        s(&iq),
        "--iq-format",
        "cs8",
        "--b-out",
        s(&d.join("rb.ts")),
        "--strip-nulls",
    ]);
    assert!(o.status.success());
    assert_same(&d.join("rb.ts"), &packets(0x0111, 0..6000), "layer B");
    std::fs::remove_dir_all(&d).unwrap();
}
