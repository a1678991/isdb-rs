//! End-to-end tests of the `isdb` binary: `tx --iq` → `rx --iq` must
//! reproduce the input packets of every layer byte-exact, including a live
//! feed that stalls (null padding must keep the output constant-bit-rate),
//! and `loopback` must PASS through an impaired channel.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

const ISDB: &str = env!("CARGO_BIN_EXE_isdb");
/// Mode 3, GI 1/8: 8192·(1 + 1/8)·204 samples per OFDM frame.
const SAMPLES_PER_FRAME: u64 = 1_880_064;
const FRAME_S: f64 = SAMPLES_PER_FRAME as f64 * 63.0 / 512e6;

fn tmp(name: &str) -> PathBuf {
    let d = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("isdb-cli-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// Distinct TS packet `n` of `pid` with a running continuity counter.
fn packet(pid: u16, n: u32) -> [u8; 188] {
    let mut p = [0u8; 188];
    p[0] = 0x47;
    p[1..3].copy_from_slice(&pid.to_be_bytes());
    p[3] = 0x10 | (n & 15) as u8;
    p[4..8].copy_from_slice(&n.to_be_bytes());
    let mut x = (u64::from(n) + 1).wrapping_mul(0x9e37_79b9_7f4a_7c15) ^ u64::from(pid);
    for b in &mut p[8..] {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        *b = x.to_le_bytes()[4]; // bits 39..32
    }
    p
}

fn packets(pid: u16, range: std::ops::Range<u32>) -> Vec<u8> {
    range.flat_map(|n| packet(pid, n)).collect()
}

fn run(args: &[&str]) -> Output {
    let o = Command::new(ISDB).args(args).output().expect("run isdb");
    eprintln!(
        "$ isdb {}\n{}",
        args.join(" "),
        String::from_utf8_lossy(&o.stderr)
    );
    o
}

fn s(p: &Path) -> &str {
    p.to_str().unwrap()
}

fn assert_same(got: &Path, want: &[u8], what: &str) {
    let g = std::fs::read(got).unwrap();
    assert_eq!(
        g.len(),
        want.len(),
        "{what}: {} vs {} bytes",
        g.len(),
        want.len()
    );
    let first = g
        .chunks(188)
        .zip(want.chunks(188))
        .position(|(a, b)| a != b);
    assert!(first.is_none(), "{what}: first differing packet {first:?}");
}

#[test]
fn tx_then_rx_reproduces_every_layer_byte_exact() {
    let d = tmp("files");
    let a = packets(0x1fc8, 0..300);
    let b = packets(0x0111, 0..9000);
    std::fs::write(d.join("a.ts"), &a).unwrap();
    std::fs::write(d.join("b.ts"), &b).unwrap();
    let iq = d.join("x.cf32");
    let o = run(&[
        "tx",
        "--a-ts",
        s(&d.join("a.ts")),
        "--b-ts",
        s(&d.join("b.ts")),
        "--iq",
        s(&iq),
    ]);
    assert!(o.status.success());
    // CBR: whole frames only.
    assert_eq!(
        std::fs::metadata(&iq).unwrap().len() % (SAMPLES_PER_FRAME * 8),
        0
    );
    let o = run(&[
        "rx",
        "--iq",
        s(&iq),
        "--a-out",
        s(&d.join("ra.ts")),
        "--b-out",
        s(&d.join("rb.ts")),
        "--out",
        s(&d.join("mux.ts")),
        "--strip-nulls",
        "--json",
    ]);
    assert!(o.status.success());
    assert_same(&d.join("ra.ts"), &a, "layer A");
    assert_same(&d.join("rb.ts"), &b, "layer B");
    // The §3.2.2 reproduced TS carries both layers in order.
    let mux = std::fs::read(d.join("mux.ts")).unwrap();
    let (ma, mb): (Vec<&[u8]>, Vec<&[u8]>) =
        mux.chunks(188).partition(|p| p[1] == 0x1f && p[2] == 0xc8);
    assert_eq!(ma.concat(), a);
    assert_eq!(mb.concat(), b);
    let j: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(j["params"]["mode"], 3);
    assert_eq!(j["params"]["guard_interval"], "1/8");
    assert_eq!(j["layers"]["B"]["rs_uncorrectable"], 0);
    assert_eq!(j["written_packets"]["A"], 300);
    std::fs::remove_dir_all(&d).unwrap();
}

#[test]
fn mux_ts_is_split_by_pid() {
    let d = tmp("mux");
    // A station-like mux: A PIDs 0x0000/0x1fc8 interleaved with B PIDs and
    // null packets (which are dropped).
    let mut mux = Vec::new();
    let (mut a, mut b) = (Vec::new(), Vec::new());
    for n in 0..6000u32 {
        let p = match n % 40 {
            0 => packet(0x0000, n),
            1 => packet(0x1fc8, n),
            2 | 3 => packet(0x1fff, n),
            k if k % 2 == 0 => packet(0x0100, n),
            _ => packet(0x0110, n),
        };
        match (p[1] & 0x1f, p[2]) {
            (0x00, 0x00) | (0x1f, 0xc8) => a.extend_from_slice(&p),
            (0x1f, 0xff) => {}
            _ => b.extend_from_slice(&p),
        }
        mux.extend_from_slice(&p);
    }
    std::fs::write(d.join("m.ts"), &mux).unwrap();
    let iq = d.join("x.cs8");
    let o = run(&[
        "tx",
        "--mux-ts",
        s(&d.join("m.ts")),
        "--a-pids",
        "0x0000,0x1fc8",
        "--iq",
        s(&iq),
        "--iq-format",
        "cs8",
    ]);
    assert!(o.status.success());
    let o = run(&[
        "rx",
        "--iq",
        s(&iq),
        "--iq-format",
        "cs8",
        "--a-out",
        s(&d.join("ra.ts")),
        "--b-out",
        s(&d.join("rb.ts")),
        "--strip-nulls",
    ]);
    assert!(o.status.success());
    assert_same(&d.join("ra.ts"), &a, "layer A");
    assert_same(&d.join("rb.ts"), &b, "layer B");
    std::fs::remove_dir_all(&d).unwrap();
}

/// AF-only PCR packet on `pid` (H.222.0 Table 2-6, base·300 + ext = `ticks`).
fn pcr_packet(pid: u16, ticks: u64) -> [u8; 188] {
    let (base, ext) = (ticks / 300, ticks % 300);
    let mut p = [0xFFu8; 188];
    let [hi, lo] = pid.to_be_bytes();
    p[..6].copy_from_slice(&[0x47, hi, lo, 0x20, 183, 0x10]);
    // base bits 32..0 in bytes 6..=10, then 6 reserved bits and ext
    p[6..11].copy_from_slice(&(base << 7).to_be_bytes()[3..]);
    let [.., e_hi, e_lo] = ext.to_be_bytes();
    p[10] |= 0x7E | (e_hi & 1);
    p[11] = e_lo;
    p
}

/// A TS of `seconds` at `bps` (all packets counted): data on PID 0x0140
/// with a PCR packet on 0x0111 every 40 ms, PCR advancing at exactly that
/// rate.  Returns the file bytes and the data packets.
// f64 -> u64 saturates: packet counts and PCR ticks of a test TS of seconds
#[allow(clippy::cast_possible_truncation)]
fn pcr_timed_ts(bps: f64, seconds: f64) -> (Vec<u8>, Vec<u8>) {
    let tick_per_pkt = 188.0 * 8.0 * 27e6 / bps;
    let per_pcr = (0.040 * 27e6 / tick_per_pkt).round() as u64; // packets per PCR interval
    let total = (seconds * 27e6 / tick_per_pkt) as u64;
    let (mut ts, mut data) = (Vec::new(), Vec::new());
    let mut n = 0u32;
    for i in 0..total {
        if i % per_pcr == 0 {
            ts.extend_from_slice(&pcr_packet(
                0x0111,
                5_000_000 + (i as f64 * tick_per_pkt) as u64,
            ));
        } else {
            let p = packet(0x0140, n);
            n += 1;
            ts.extend_from_slice(&p);
            data.extend_from_slice(&p);
        }
    }
    (ts, data)
}

#[test]
fn mux_ts_file_is_paced_by_its_pcr() {
    let d = tmp("paced");
    // 8 Mbit/s into layer B (16.85 Mbit/s): about half of every frame is
    // null padding, nothing runs ahead of the PCR, nothing is lost.
    let (ts, data) = pcr_timed_ts(8e6, 3.0);
    std::fs::write(d.join("m.ts"), &ts).unwrap();
    let iq = d.join("x.cs8");
    let o = run(&[
        "tx",
        "--mux-ts",
        s(&d.join("m.ts")),
        "--iq",
        s(&iq),
        "--iq-format",
        "cs8",
        "--status-secs",
        "0",
    ]);
    assert!(o.status.success());
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(err.contains("paced by the input's PCR"), "{err}");
    assert!(err.contains("layer B mean 8.00 Mbit/s"), "{err}");
    // 3 s of input at 16.85 Mbit/s capacity: the data frames span ≈ 3 s.
    let b_line = err.lines().find(|l| l.contains("layer B:")).unwrap();
    let fill: f64 = b_line
        .split('(')
        .nth(1)
        .unwrap()
        .split('%')
        .next()
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert!((45.0..60.0).contains(&fill), "null fill {fill}: {b_line}");
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
    let got: Vec<u8> = std::fs::read(d.join("rb.ts"))
        .unwrap()
        .chunks(188)
        .filter(|p| p[1] == 0x01 && p[2] == 0x40)
        .flatten()
        .copied()
        .collect();
    assert_eq!(got.len(), data.len());
    assert!(got == data, "data packets differ");
    std::fs::remove_dir_all(&d).unwrap();
}

#[test]
fn mux_ts_over_capacity_is_refused_with_layouts_that_fit() {
    let d = tmp("overrate");
    let (ts, _) = pcr_timed_ts(19.1e6, 2.0);
    std::fs::write(d.join("m.ts"), &ts).unwrap();
    let o = run(&[
        "tx",
        "--mux-ts",
        s(&d.join("m.ts")),
        "--iq",
        "/dev/null",
        "--status-secs",
        "0",
    ]);
    assert!(!o.status.success());
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(
        err.contains(
            "needs 19.10 Mbit/s but layer B (12seg 64QAM 3/4, mode 3, GI 1/8) carries 16.85 Mbit/s"
        ),
        "{err}"
    );
    assert!(err.contains("--layer-b 12,64qam,7/8,2"), "{err}");
    assert!(
        err.contains("--partial false --layer-a 13,64qam,5/6,2"),
        "{err}"
    );
    // the suggested layout is accepted
    let o = run(&[
        "tx",
        "--mux-ts",
        s(&d.join("m.ts")),
        "--iq",
        "/dev/null",
        "--status-secs",
        "0",
        "--layer-b",
        "12,64qam,7/8,2",
    ]);
    assert!(o.status.success());
    // --allow-overrate sends it anyway (slow motion, reported)
    let o = run(&[
        "tx",
        "--mux-ts",
        s(&d.join("m.ts")),
        "--iq",
        "/dev/null",
        "--status-secs",
        "0",
        "--allow-overrate",
    ]);
    assert!(o.status.success());
    assert!(String::from_utf8_lossy(&o.stderr).contains("WARNING: (--allow-overrate)"));
    std::fs::remove_dir_all(&d).unwrap();
}

/// Regression benchmark for producer starvation (TX underruns): the producer
/// must sustain ≥ 2× real time for a high-rate `--mux-ts` file (19.5 Mbit/s
/// into B 12seg 64QAM 7/8).  The cause was an unoptimised (dev profile)
/// build at ≈ 0.2× real time; the workspace now optimises the PHY crates in
/// the dev profile too.  Run with
/// `cargo test --release -p isdb-cli --test cli -- --ignored mux_ts_producer_throughput --nocapture`
/// (or without `--release`: the test binary is built with the test profile).
#[test]
#[ignore = "timing benchmark (≈ 10 s)"]
fn mux_ts_producer_throughput() {
    let d = tmp("throughput");
    let seconds = 20.0;
    let (ts, _) = pcr_timed_ts(19.5e6, seconds + 1.0);
    std::fs::write(d.join("m.ts"), &ts).unwrap();
    let t = Instant::now();
    let o = run(&[
        "tx",
        "--mux-ts",
        s(&d.join("m.ts")),
        "--iq",
        "/dev/null",
        "--iq-format",
        "cs8",
        "--layer-b",
        "12,64qam,7/8,2",
        "--seconds",
        &seconds.to_string(),
        "--status-secs",
        "5",
    ]);
    let wall = t.elapsed().as_secs_f64();
    assert!(o.status.success());
    let speed = seconds / wall;
    eprintln!(
        "mux_ts_producer_throughput: {seconds} s on air in {wall:.2} s = {speed:.2}× real time"
    );
    assert!(speed >= 2.0, "producer {speed:.2}× real time (< 2×)");
    std::fs::remove_dir_all(&d).unwrap();
}

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
    // on top of that proves it was filled, with room for a producer slowed
    // by the rest of the test suite running on every core (the full stall
    // was missed by one frame there, plan §0.5).
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

#[test]
fn loopback_passes_with_impairments() {
    let o = run(&[
        "loopback",
        "--frames",
        "4",
        "--snr-db",
        "24",
        "--cfo-carriers",
        "-1.3",
        "--sample-offset",
        "-700000",
        "--json",
    ]);
    assert!(o.status.success());
    let j: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(j["result"], "PASS");
    assert_eq!(j["verify"]["A"]["input_pass"], 4 * 64);
    assert_eq!(j["verify"]["B"]["input_pass"], 4 * 2592);
    assert_eq!(j["verify"]["B"]["fail"], 0);
    assert!(j["ts_frames_exact"].as_u64().unwrap() >= 4);
}

#[test]
fn loopback_with_file_inputs_and_detects_loss() {
    let d = tmp("lbfile");
    std::fs::write(d.join("b.ts"), packets(0x0111, 0..5000)).unwrap();
    let b = d.join("b.ts");
    let o = run(&["loopback", "--b-ts", s(&b), "--snr-db", "30"]);
    assert!(o.status.success());
    // Without the flush frames the tail of the input never leaves the
    // interleavers: reported as lost, verdict FAIL.
    let o = run(&["loopback", "--b-ts", s(&b), "--no-flush", "--json"]);
    assert!(!o.status.success());
    let j: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(j["result"], "FAIL");
    assert!(j["verify"]["B"]["lost"].as_u64().unwrap() > 0);
    assert_eq!(j["verify"]["B"]["fail"], 0);
    std::fs::remove_dir_all(&d).unwrap();
}

#[cfg(feature = "hackrf")]
#[test]
fn radio_tx_refuses_without_rf_enable() {
    // --dry-run prints the RF settings and exits before touching a device.
    let o = run(&[
        "tx",
        "--radio",
        "hackrf",
        "--channel",
        "13",
        "--gain",
        "txvga=20",
        "--synthetic",
        "--dry-run",
    ]);
    assert!(!o.status.success());
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(err.contains("hackrf TX settings (T13):"), "{err}");
    assert!(err.contains("centre 473.142857 MHz"), "{err}");
    assert!(err.contains("TX VGA       20 dB"), "{err}");
    assert!(err.contains("RF amp       off"), "{err}");
    assert!(err.contains("not transmitting"), "{err}");
    assert!(
        err.contains("use --rf-enable instead of --dry-run"),
        "{err}"
    );
    // Without --rf-enable or --dry-run the command line is rejected.
    assert!(
        !run(&["tx", "--radio", "hackrf", "--channel", "13", "--synthetic"])
            .status
            .success()
    );
}

#[cfg(feature = "hackrf")]
#[test]
fn radio_rejects_unknown_backends_gains_and_ranges() {
    let dry = |extra: &[&str]| {
        let mut args = vec!["tx", "--channel", "13", "--synthetic", "--dry-run"];
        args.extend_from_slice(extra);
        let o = run(&args);
        assert!(!o.status.success(), "{extra:?}");
        String::from_utf8_lossy(&o.stderr).into_owned()
    };
    let e = dry(&["--radio", "sdr"]);
    assert!(e.contains("unknown radio backend \"sdr\""), "{e}");
    assert!(
        e.contains(
            "compiled in: hackrf (isdb-radio feature `hackrf`, forwarded by isdb-cli feature `hackrf`)"
        ),
        "{e}"
    );
    let e = dry(&["--radio", "hackrf", "--gain", "bogus=1"]);
    assert!(e.contains("no gain \"bogus\""), "{e}");
    assert!(e.contains("txvga=0..47 dB (default 0)"), "{e}");
    let e = dry(&["--radio", "hackrf", "--gain", "txvga=48"]);
    assert!(e.contains("outside 0..47"), "{e}");
    let e = dry(&["--radio", "hackrf:colour=red"]);
    assert!(e.contains("unknown device argument \"colour\""), "{e}");
    let e = dry(&["--radio", "hackrf", "--bb-filter-hz", "30000000"]);
    assert!(
        e.contains("above the largest HackRF (MAX2837) bandwidth"),
        "{e}"
    );
    let e = dry(&["--radio", "hackrf", "--backoff-db", "41"]);
    assert!(e.contains("backoff 41 dB outside 0..40"), "{e}");
    // RX gains are checked before a device is opened, too.
    let o = run(&[
        "rx",
        "--radio",
        "hackrf",
        "--channel",
        "27",
        "--gain",
        "lna=12",
    ]);
    assert!(!o.status.success());
    let e = String::from_utf8_lossy(&o.stderr);
    assert!(e.contains("lna=12: must be 0..40 dB in 8 dB steps"), "{e}");
}

#[cfg(feature = "pluto")]
#[test]
fn pluto_dry_run_validates_without_a_device() {
    // The address is TEST-NET-1 (RFC 5737): a dry run never connects.
    let dry = |extra: &[&str]| {
        let mut args = vec!["tx", "--channel", "13", "--synthetic", "--dry-run"];
        args.extend_from_slice(extra);
        let o = run(&args);
        assert!(!o.status.success(), "{extra:?}");
        String::from_utf8_lossy(&o.stderr).into_owned()
    };
    let e = dry(&[
        "--radio",
        "pluto:uri=ip:192.0.2.1,loopback=on",
        "--gain",
        "att=30",
    ]);
    assert!(e.contains("pluto TX settings (T13):"), "{e}");
    assert!(e.contains("device       Pluto ip:192.0.2.1, TX1"), "{e}");
    assert!(e.contains("TX atten     30 dB"), "{e}");
    assert!(e.contains("nothing radiated"), "{e}");
    assert!(e.contains("below cs12 full scale"), "{e}");
    assert!(e.contains("not transmitting"), "{e}");
    // The default attenuation is the maximum.
    let e = dry(&["--radio", "pluto:uri=ip:192.0.2.1"]);
    assert!(e.contains("TX atten     89.75 dB (maximum)"), "{e}");
    let e = dry(&["--radio", "pluto", "--gain", "att=90"]);
    assert!(
        e.contains("att=90: must be 0..89.75 dB in 0.25 dB steps"),
        "{e}"
    );
    let e = dry(&["--radio", "pluto", "--gain", "txvga=20"]);
    assert!(e.contains("no gain \"txvga\""), "{e}");
    let e = dry(&["--radio", "pluto:channel=3"]);
    assert!(e.contains("channel=3: want 1 or 2"), "{e}");
    let e = dry(&["--radio", "pluto", "--bb-filter-hz", "50000000"]);
    assert!(e.contains("RF bandwidth 50000000 Hz outside"), "{e}");
    // RX: loopback is a TX argument; manual gain and AGC exclude each other.
    for (extra, want) in [
        (
            &["--radio", "pluto:loopback=on"][..],
            "unknown device argument \"loopback\"",
        ),
        (
            &[
                "--radio",
                "pluto",
                "--gain",
                "agc=fast_attack",
                "--gain",
                "gain=20",
            ][..],
            "cannot be combined with agc=fast_attack",
        ),
    ] {
        let mut args = vec!["rx", "--channel", "27"];
        args.extend_from_slice(extra);
        let o = run(&args);
        assert!(!o.status.success(), "{extra:?}");
        let e = String::from_utf8_lossy(&o.stderr);
        assert!(e.contains(want), "{e}");
    }
}

#[cfg(feature = "pluto")]
#[test]
fn capture_dry_run_validates_without_a_device() {
    let dir = tmp("capture-dry");
    let out = dir.join("x.sigmf-data");
    let cap = |extra: &[&str]| {
        let mut args = vec!["capture", "--rate", "57.72M", "-o", s(&out), "--dry-run"];
        args.extend_from_slice(extra);
        run(&args)
    };
    let o = cap(&["--radio", "pluto:uri=ip:192.0.2.1", "--freq", "bs1"]);
    assert!(o.status.success());
    let e = String::from_utf8_lossy(&o.stderr);
    assert!(e.contains("pluto capture settings (BS1 IF):"), "{e}");
    assert!(e.contains("programmed 1049480000 Hz"), "{e}");
    assert!(e.contains("recorder     libiio single buffer"), "{e}");
    assert!(e.contains("1 segment(s) of 16515072 samples"), "{e}");
    assert!(e.contains("x.sigmf-meta"), "{e}");
    assert!(e.contains("dry run: radio not opened"), "{e}");
    assert!(!out.exists(), "a dry run writes nothing");
    let o = cap(&[
        "--radio",
        "pluto:uri=ip:192.0.2.1,recorder=maia",
        "--freq",
        "nd24",
    ]);
    let e = String::from_utf8_lossy(&o.stderr);
    assert!(o.status.success(), "{e}");
    assert!(
        e.contains("(ND24 IF)") && e.contains("programmed 2053000000 Hz"),
        "{e}"
    );
    assert!(e.contains("maia-sdr recorder at http://192.0.2.1/"), "{e}");
    assert!(e.contains("of 33554432 samples"), "{e}");
    // A left-hand-circular channel: its IF from the 9.505 GHz LO.
    let o = cap(&["--radio", "pluto:uri=ip:192.0.2.1", "--freq", "BS14"]);
    let e = String::from_utf8_lossy(&o.stderr);
    assert!(o.status.success(), "{e}");
    assert!(
        e.contains("(BS14 IF, left-hand)") && e.contains("programmed 2471820000 Hz"),
        "{e}"
    );
    for (extra, want) in [
        (
            &["--radio", "pluto", "--freq", "bs1", "--samples", "16515073"][..],
            "captures at most 16515072 contiguous samples",
        ),
        (
            &[
                "--radio",
                "pluto:recorder=maia,buffer=65536",
                "--freq",
                "bs1",
            ][..],
            "buffer applies to recorder=stream and the symbol stream, not recorder=maia",
        ),
        (
            &["--radio", "pluto:loopback=on", "--freq", "bs1"][..],
            "(known: uri, channel, recorder, bits, buffer (recorder=stream and the symbol stream), kbuffers (the symbol stream))",
        ),
        (
            &["--radio", "pluto:recorder=stream", "--freq", "bs1"][..],
            "a streamed capture needs --samples or --seconds",
        ),
        (
            &["--radio", "pluto", "--freq", "bs25"][..],
            "channel BS25: outside BS1..BS24",
        ),
        (
            &["--radio", "pluto", "--freq", "bs1", "--bw", "60M"][..],
            "RF bandwidth 60000000 Hz outside",
        ),
        (
            &["--radio", "hackrf", "--freq", "bs1"][..],
            "cannot capture",
        ),
    ] {
        let o = cap(extra);
        let e = String::from_utf8_lossy(&o.stderr);
        assert!(!o.status.success() && e.contains(want), "{extra:?}: {e}");
    }
}

/// A QPSK signal at `fs` with `sps` samples per symbol (raised-cosine
/// transitions, so |z|² has a strong symbol-rate line), as cs16 with full
/// scale 2048, offset `cfo` Hz.
// f64 -> usize / i16 saturate: symbol indices below n / sps + 2, and
// components of magnitude ≤ √2 scaled by 600, inside i16
#[allow(clippy::cast_possible_truncation)]
fn synthetic_qpsk_cs16(fs: f64, sps: f64, cfo: f64, n: usize) -> Vec<u8> {
    let mut rng = 0x2545_f491_4f6c_dd1du64;
    let symbols: Vec<(f64, f64)> = (0..(n as f64 / sps) as usize + 2)
        .map(|_| {
            rng ^= rng << 13;
            rng ^= rng >> 7;
            rng ^= rng << 17;
            let ph = std::f64::consts::FRAC_PI_4 + std::f64::consts::FRAC_PI_2 * (rng % 4) as f64;
            (ph.cos(), ph.sin())
        })
        .collect();
    let mut out = Vec::with_capacity(4 * n);
    for i in 0..n {
        let t = i as f64 / sps;
        let k = t as usize;
        let w = 0.5 - 0.5 * (std::f64::consts::PI * (t - k as f64)).cos();
        let (a, b) = (symbols[k], symbols[k + 1]);
        let (re, im) = (a.0 * (1.0 - w) + b.0 * w, a.1 * (1.0 - w) + b.1 * w);
        let ph = 2.0 * std::f64::consts::PI * cfo * i as f64 / fs;
        let (c, s) = (ph.cos(), ph.sin());
        for v in [re * c - im * s, re * s + im * c] {
            out.extend(((v * 600.0).round() as i16).to_le_bytes());
        }
    }
    out
}

#[test]
fn iq_survey_finds_rate_carrier_and_system() {
    let dir = tmp("iq-survey");
    let f = dir.join("qpsk.cs16");
    let fs = 61.44e6;
    std::fs::write(&f, synthetic_qpsk_cs16(fs, fs / 28.86e6, -115e3, 1 << 18)).unwrap();
    // A raw file needs its format and rate.
    let o = run(&["iq", "survey", s(&f)]);
    assert!(!o.status.success());
    assert!(String::from_utf8_lossy(&o.stderr).contains("give the sample format"));
    let o = run(&[
        "iq",
        "survey",
        "--format",
        "cs16/2048",
        "--rate",
        "61.44M",
        "--json",
        s(&f),
    ]);
    assert!(o.status.success());
    let v: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(v["system"]["name"], "ISDB-S", "{v}");
    assert!(
        (v["system"]["symbol_rate_hz"].as_f64().unwrap() - 28.86e6).abs() < 300.0,
        "{v}"
    );
    let res = fs / f64::from(1 << 18);
    assert!(
        (v["carrier_offset_hz"].as_f64().unwrap() + 115e3).abs() < res,
        "{v}"
    );
    assert_eq!(v["samples"], 1 << 18);
    // With SigMF metadata the format and rate come from it.
    std::fs::write(
        dir.join("qpsk.cs16.sigmf-meta"),
        r#"{"global":{"core:datatype":"ci16_le","core:sample_rate":61440000,"isdb:full_scale":2048},
            "captures":[{"core:sample_start":0,"core:frequency":1613000000,"isdb:channel":"nd2"}]}"#,
    )
    .unwrap();
    let o = run(&["iq", "survey", s(&f)]);
    let t = String::from_utf8_lossy(&o.stdout);
    assert!(o.status.success(), "{t}");
    assert!(
        t.contains("cs16/2048 at 61.440000 Msps, centre 1613.000000 MHz (nd2)"),
        "{t}"
    );
    assert!(t.contains("-> ISDB-S at 28.86"), "{t}");
    assert!(t.contains("system         ISDB-S\n"), "{t}");
}

#[test]
fn iq_symbols_writes_words_that_read_back() {
    let dir = tmp("iq-symbols");
    let f = dir.join("qpsk.sigmf-data");
    std::fs::write(
        &f,
        synthetic_qpsk_cs16(40e6, 40e6 / 28.86e6, -115e3, 1 << 20),
    )
    .unwrap();
    std::fs::write(
        dir.join("qpsk.sigmf-meta"),
        r#"{"global":{"core:datatype":"ci16_le","core:sample_rate":40000000,"isdb:full_scale":2048},
            "captures":[{"core:sample_start":0,"core:frequency":1049480000}]}"#,
    )
    .unwrap();
    let out = dir.join("q.syms");
    let o = run(&["iq", "symbols", "--system", "isdb-s", s(&f), "-o", s(&out)]);
    let t = String::from_utf8_lossy(&o.stdout);
    assert!(
        o.status.success(),
        "{t}{}",
        String::from_utf8_lossy(&o.stderr)
    );
    assert!(t.contains("whole blocks; symbol rate"), "{t}");
    let meta: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(dir.join("q.syms.json")).unwrap()).unwrap();
    assert_eq!(meta["source"], "cpu");
    assert_eq!(meta["system"], "ISDB-S");
    assert_eq!(meta["params"]["bits"], 6);
    assert_eq!(meta["params"]["taps"], 32);
    let o = run(&["iq", "symbols", "--read", s(&out)]);
    let t = String::from_utf8_lossy(&o.stdout);
    assert!(o.status.success(), "{t}");
    assert!(t.contains("blocks of 4096 symbols"), "{t}");
    assert!(t.contains("0 loss event(s)"), "{t}");
    // Not overwritten without --overwrite.
    let o = run(&["iq", "symbols", "--system", "isdb-s", s(&f), "-o", s(&out)]);
    assert!(!o.status.success());
}

/// The opt-in radio tests may name the same device: one at a time.
#[cfg(feature = "pluto")]
static RADIO: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Opt-in hardware test (receive only): `ISDB_RS_PLUTO_URI` names a
/// PlutoSDR-family radio, e.g. `ip:192.0.2.30`; `ISDB_RS_PLUTO_CAPTURE_FREQ`
/// the frequency or channel to capture (default `BS1`, the BS1 satellite
/// IF). Captures two short segments and checks the recording and its
/// metadata; the signal itself is not checked.
#[cfg(feature = "pluto")]
#[test]
fn capture_from_a_pluto() {
    let Some(uri) = std::env::var_os("ISDB_RS_PLUTO_URI") else {
        eprintln!("SKIP capture_from_a_pluto: ISDB_RS_PLUTO_URI not set (e.g. ip:192.0.2.30)");
        return;
    };
    let _radio = RADIO
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let freq = std::env::var("ISDB_RS_PLUTO_CAPTURE_FREQ").unwrap_or_else(|_| "BS1".into());
    let dir = tmp("capture-pluto");
    let out = dir.join("c.sigmf-data");
    let radio = format!("pluto:uri={}", uri.to_string_lossy());
    let o = run(&[
        "capture",
        "--radio",
        &radio,
        "--freq",
        &freq,
        "--rate",
        "61.44M",
        "--samples",
        "256Ki",
        "--segments",
        "2",
        "-o",
        s(&out),
    ]);
    assert!(o.status.success());
    assert_eq!(std::fs::metadata(&out).unwrap().len(), 2 * 4 * 262_144);
    let meta: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(dir.join("c.sigmf-meta")).unwrap()).unwrap();
    let g = &meta["global"];
    assert_eq!(g["core:datatype"], "ci16_le");
    assert_eq!(g["isdb:full_scale"], 2048.0);
    assert!((g["core:sample_rate"].as_f64().unwrap() / 61.44e6 - 1.0).abs() < 10e-6);
    let c = meta["captures"].as_array().unwrap();
    assert_eq!(c.len(), 2);
    assert_eq!(c[1]["core:sample_start"], 262_144);
    assert_eq!(c[1]["isdb:gap_before"], true);
    assert_eq!(c[0]["isdb:capture_method"], "libiio single buffer");
    // Not overwritten without --overwrite.
    let o = run(&[
        "capture",
        "--radio",
        &radio,
        "--freq",
        &freq,
        "--rate",
        "61.44M",
        "--samples",
        "1Ki",
        "-o",
        s(&out),
    ]);
    assert!(!o.status.success());
    assert!(String::from_utf8_lossy(&o.stderr).contains("--overwrite replaces it"));
    assert_eq!(std::fs::metadata(&out).unwrap().len(), 2 * 4 * 262_144);
}

/// Opt-in hardware test (receive only): `ISDB_RS_PLUTO_URI` names a
/// PlutoSDR-family radio. `isdb symbols --probe` reads whether its
/// bitstream has the FPGA symbol front end (one register read, nothing
/// written); without one, `--offload fpga` must refuse.
#[cfg(feature = "pluto")]
#[test]
fn symbols_probe_on_a_pluto() {
    let Some(uri) = std::env::var_os("ISDB_RS_PLUTO_URI") else {
        eprintln!("SKIP symbols_probe_on_a_pluto: ISDB_RS_PLUTO_URI not set (e.g. ip:192.0.2.30)");
        return;
    };
    let _radio = RADIO
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let freq = std::env::var("ISDB_RS_PLUTO_CAPTURE_FREQ").unwrap_or_else(|_| "BS1".into());
    let radio = format!("pluto:uri={}", uri.to_string_lossy());
    let o = run(&["symbols", "--radio", &radio, "--freq", &freq, "--probe"]);
    let e = String::from_utf8_lossy(&o.stderr);
    assert!(o.status.success(), "{e}");
    eprintln!("{e}");
    if e.contains("offers no symbol front end") {
        let dir = tmp("symbols-pluto");
        let out = dir.join("s.syms");
        let o = run(&[
            "symbols",
            "--radio",
            &radio,
            "--freq",
            &freq,
            "--offload",
            "fpga",
            "-o",
            s(&out),
        ]);
        assert!(!o.status.success());
        assert!(
            String::from_utf8_lossy(&o.stderr).contains("no compatible symbol front end"),
            "{}",
            String::from_utf8_lossy(&o.stderr)
        );
        assert!(!out.exists());
    } else {
        assert!(e.contains("offers the symbol front end"), "{e}");
    }
}

/// Opt-in hardware test (receive only) of what needs the Tezuka firmware:
/// `ISDB_RS_TEZUKA_URI` names a PlutoSDR-family radio running Tezuka
/// (e.g. `ip:192.0.2.30`), `ISDB_RS_PLUTO_CAPTURE_FREQ` the frequency
/// (default `bs1`). Takes an 8-bit libiio buffer, a 1 s 8-bit stream and
/// maia-sdr recordings in 8 and 12 bits, and checks each file against its
/// metadata; the signal itself is not checked.
#[cfg(feature = "pluto")]
#[test]
fn tezuka_captures() {
    let Some(uri) = std::env::var_os("ISDB_RS_TEZUKA_URI") else {
        eprintln!("SKIP tezuka_captures: ISDB_RS_TEZUKA_URI not set (e.g. ip:192.0.2.30)");
        return;
    };
    let _radio = RADIO
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let freq = std::env::var("ISDB_RS_PLUTO_CAPTURE_FREQ").unwrap_or_else(|_| "BS1".into());
    let dir = tmp("capture-tezuka");
    let uri = uri.to_string_lossy();
    // (device arguments, rate, length option, data type, full scale,
    // capture method contains)
    for (i, (args, rate, len, datatype, full_scale, method)) in [
        (
            "bits=8",
            "40M",
            ["--samples", "1Mi"],
            "ci8",
            None,
            "libiio single buffer",
        ),
        (
            "recorder=stream,bits=8",
            "10M",
            ["--seconds", "1"],
            "ci8",
            None,
            "libiio stream",
        ),
        (
            "recorder=maia,bits=8",
            "40M",
            ["--samples", "4Mi"],
            "ci8",
            None,
            "IQ8bit",
        ),
        (
            "recorder=maia,bits=12",
            "40M",
            ["--samples", "4Mi"],
            "ci16_le",
            Some(2048.0),
            "IQ12bit",
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let out = dir.join(format!("t{i}.sigmf-data"));
        let radio = format!("pluto:uri={uri},{args}");
        let o = run(&[
            "capture",
            "--radio",
            &radio,
            "--freq",
            &freq,
            "--rate",
            rate,
            len[0],
            len[1],
            "-o",
            s(&out),
        ]);
        assert!(o.status.success(), "{args}");
        let meta: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(dir.join(format!("t{i}.sigmf-meta"))).unwrap(),
        )
        .unwrap();
        let g = &meta["global"];
        assert_eq!(g["core:datatype"], datatype, "{args}");
        let want_rate = if rate == "40M" { 40e6 } else { 10e6 };
        assert!(
            (g["core:sample_rate"].as_f64().unwrap() / want_rate - 1.0).abs() < 10e-6,
            "{args}: {g:?}"
        );
        assert_eq!(g["isdb:full_scale"].as_f64(), full_scale, "{args}");
        let c = meta["captures"].as_array().unwrap();
        assert!(
            c[0]["isdb:capture_method"]
                .as_str()
                .unwrap()
                .contains(method),
            "{args}: {c:?}"
        );
        let bytes = std::fs::metadata(&out).unwrap().len();
        let bps = if datatype == "ci8" { 2 } else { 4 };
        let want = match len {
            ["--samples", "1Mi"] => 1 << 20,
            ["--samples", _] => 4 << 20,
            // A stream writes what it did not discard around losses.
            _ => {
                let discarded: u64 = c
                    .iter()
                    .flat_map(|s| {
                        [
                            s["isdb:samples_discarded"].as_u64(),
                            s["isdb:samples_discarded_after"].as_u64(),
                        ]
                    })
                    .flatten()
                    .sum();
                assert!(c[1..].iter().all(|s| s["isdb:gap_before"] == true));
                10_000_000 - discarded
            }
        };
        assert_eq!(bytes, want * bps, "{args}");
    }
}

#[cfg(not(any(feature = "hackrf", feature = "pluto")))]
#[test]
fn radio_without_backends_names_the_feature() {
    for name in ["hackrf", "pluto"] {
        for args in [
            &[
                "tx",
                "--radio",
                name,
                "--channel",
                "13",
                "--synthetic",
                "--dry-run",
            ][..],
            &["rx", "--radio", name, "--channel", "13"],
        ] {
            let o = run(args);
            assert!(!o.status.success());
            let e = String::from_utf8_lossy(&o.stderr);
            assert!(
                e.contains(&format!(
                    "radio backend \"{name}\" is not compiled in (isdb-radio feature `{name}`, \
                     forwarded by isdb-cli feature `{name}`); this build has no radio backends"
                )),
                "{e}"
            );
            // A dry run without a backend prints no settings.
            assert!(!e.contains("TX settings"), "{e}");
        }
    }
}

#[test]
fn backend_features_are_forwarded() {
    // isdb-radio's "unknown backend" error names the isdb-cli feature
    // forwarding each backend feature under the same name.
    let toml = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml")).unwrap();
    assert!(toml.contains(r#"hackrf = ["isdb-radio/hackrf"]"#), "{toml}");
    assert!(toml.contains(r#"pluto = ["isdb-radio/pluto"]"#), "{toml}");
}

#[test]
fn endless_input_into_iq_file_is_refused_without_seconds() {
    let d = tmp("iq-bound");
    let iq = d.join("x.cf32");
    let o = run(&["tx", "--synthetic", "--iq", s(&iq)]);
    assert!(!o.status.success());
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(
        err.contains("never ends") && err.contains("--seconds"),
        "{err}"
    );
    assert!(!iq.exists(), "nothing written");
    // With a bound it runs; into stdout it needs none (the reader decides).
    let o = run(&["tx", "--synthetic", "--iq", s(&iq), "--seconds", "0.5"]);
    assert!(o.status.success());
    assert!(std::fs::metadata(&iq).unwrap().len() > 0);
    std::fs::remove_dir_all(&d).unwrap();
}

#[test]
fn completions_cover_every_subcommand() {
    const SUBCOMMANDS: [&str; 8] = [
        "tx",
        "rx",
        "loopback",
        "ts",
        "mux",
        "capture",
        "iq",
        "completions",
    ];
    let o = run(&["completions", "bash"]);
    assert!(o.status.success());
    let bash = String::from_utf8(o.stdout).unwrap();
    assert!(
        bash.contains("_isdb()") && bash.contains("complete -F _isdb"),
        "{bash}"
    );
    for sub in SUBCOMMANDS {
        assert!(bash.contains(&format!("isdb,{sub})")), "bash: {sub}");
    }
    assert!(bash.contains("--radio"), "bash: --radio");

    let o = run(&["completions", "zsh"]);
    assert!(o.status.success());
    let zsh = String::from_utf8(o.stdout).unwrap();
    assert!(zsh.starts_with("#compdef isdb\n"), "{zsh}");
    for sub in SUBCOMMANDS {
        assert!(zsh.contains(&format!("'{sub}:")), "zsh: {sub}");
    }
    assert!(zsh.contains("--radio="), "zsh: --radio");

    // Not a shell: a usage error.
    assert_eq!(run(&["completions", "csh"]).status.code(), Some(2));
}

fn bench_toml() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../isdbt-mux/examples/bench.toml")
}

/// The bench multiplex with its media streams turned into `declared`
/// streams (PSI/SI/PCR only), so it runs without the media files.
fn declared_only_config(dir: &Path) -> PathBuf {
    let text = std::fs::read_to_string(bench_toml()).unwrap();
    let out: Vec<String> = text
        .lines()
        .filter(|l| {
            !l.trim_start().starts_with("source =") && !l.trim_start().starts_with("frames_per_pes")
        })
        .map(|l| {
            l.replace(
                "kind = \"mpeg2-video\"",
                "kind = \"declared\"\n  stream_type = 0x02",
            )
            .replace(
                "kind = \"aac-adts\"",
                "kind = \"declared\"\n  stream_type = 0x0F",
            )
        })
        .collect();
    let p = dir.join("declared.toml");
    std::fs::write(&p, out.join("\n")).unwrap();
    p
}

/// `s` as a quoted TOML basic string.
fn toml_string(s: &str) -> String {
    toml::Value::String(s.to_string()).to_string()
}

/// Directory of your own media (`video.m2v` MPEG-2 video ES, `audio.m2a`
/// AAC ADTS) named by the
/// environment variable `ISDB_RS_MEDIA_DIR`, or `None` (with a `SKIP` line)
/// when it is unset or a file is missing.
fn media_dir(test: &str) -> Option<PathBuf> {
    let Some(dir) = std::env::var_os("ISDB_RS_MEDIA_DIR").map(PathBuf::from) else {
        eprintln!(
            "SKIP {test}: ISDB_RS_MEDIA_DIR not set (directory with video.m2v and audio.m2a)"
        );
        return None;
    };
    for f in ["video.m2v", "audio.m2a"] {
        if !dir.join(f).is_file() {
            eprintln!("SKIP {test}: {} missing", dir.join(f).display());
            return None;
        }
    }
    Some(dir)
}

/// `bench.toml` with its placeholder media paths pointed at the files of
/// `media`, written into `dir`.
fn bench_media_config(dir: &Path, media: &Path) -> PathBuf {
    let path = |f: &str| toml_string(media.join(f).to_str().unwrap());
    let text = std::fs::read_to_string(bench_toml()).unwrap();
    assert!(text.contains("\"media/video.m2v\"") && text.contains("\"media/audio.m2a\""));
    let text = text
        .replace("\"media/video.m2v\"", &path("video.m2v"))
        .replace("\"media/audio.m2a\"", &path("audio.m2a"));
    let p = dir.join("bench-media.toml");
    std::fs::write(&p, text).unwrap();
    p
}

#[test]
fn mux_config_and_ts_tools_smoke() {
    let d = tmp("muxts");
    let o = run(&["mux", "--print-config-example"]);
    assert!(o.status.success());
    assert!(String::from_utf8_lossy(&o.stdout).contains("[network]"));
    let cfg = declared_only_config(&d);
    let full = d.join("full.ts");
    let o = run(&["mux", "-c", s(&cfg), "--frames", "8", "--out-ts", s(&full)]);
    assert!(o.status.success());
    let o = run(&["ts", "inspect", s(&full)]);
    assert!(o.status.success());
    let out = String::from_utf8_lossy(&o.stdout);
    assert!(
        out.contains("PAT") && out.contains("PMT program 0x0400"),
        "{out}"
    );
    std::fs::remove_dir_all(&d).unwrap();
}

#[test]
fn loopback_with_in_process_multiplexer() {
    let d = tmp("muxcfg");
    // Declared-only streams: runs everywhere.
    let cfg = declared_only_config(&d);
    let o = run(&[
        "loopback",
        "--mux-config",
        s(&cfg),
        "--frames",
        "3",
        "--snr-db",
        "28",
        "--json",
    ]);
    assert!(o.status.success());
    let j: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(j["result"], "PASS");
    assert!(
        j["verify"]["A"]["input_pass"].as_u64().unwrap() > 0,
        "PSI/SI in layer A"
    );
    // The bench config with its media (opt-in: ISDB_RS_MEDIA_DIR).
    if let Some(media) = media_dir("loopback_with_in_process_multiplexer (media part)") {
        let bench = bench_media_config(&d, &media);
        // common encoders write no crc_check, which `conform` cannot insert
        // (media_aac_gate_matches_b32_part2_report); this test is about the
        // multiplexer → PHY path, so send it unchanged
        let text = std::fs::read_to_string(&bench).unwrap();
        assert!(text.contains("adts_compliance = \"conform\""));
        std::fs::write(
            &bench,
            text.replace(
                "adts_compliance = \"conform\"",
                "adts_compliance = \"warn\"",
            ),
        )
        .unwrap();
        let o = run(&[
            "loopback",
            "--mux-config",
            s(&bench),
            "--frames",
            "4",
            "--json",
        ]);
        assert!(o.status.success());
        let j: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
        assert_eq!(j["result"], "PASS");
        assert!(
            j["verify"]["B"]["input_pass"].as_u64().unwrap() > 1000,
            "video/audio in layer B"
        );
    }
    // --mux-config excludes the other inputs.
    assert!(
        !run(&["loopback", "--mux-config", s(&cfg), "--synthetic"])
            .status
            .success()
    );
    std::fs::remove_dir_all(&d).unwrap();
}

/// `isdb mux` with your own media (opt-in: `ISDB_RS_MEDIA_DIR`): the
/// expectations come from the audio file's own ADTS headers. The default
/// `adts_compliance = "conform"` accepts the file exactly when its headers
/// break no error-severity B32 Part 2 §5.2 rule (refused otherwise, with
/// "crc_check cannot be inserted" for protection_absent '1');
/// `--adts-compliance warn` sends it unchanged, the validator reports each
/// such per-frame rule as an `aac.*` error, and the EIT carries the
/// quality_indicator Table 1 derives from the file's rate.
#[test]
fn media_aac_gate_matches_b32_part2_report() {
    use isdbt_mux::adts::b32::{self, Rule};
    let Some(media) = media_dir("media_aac_gate_matches_b32_part2_report") else {
        return;
    };
    let audio = std::fs::read(media.join("audio.m2a")).unwrap();
    let (rep, _) = b32::check_stream(&audio).unwrap();
    let blocking: Vec<Rule> = rep.blocking().map(|(r, _)| *r).collect();
    let (es, _) =
        isdbt_mux::audio::AdtsEs::load(audio, "audio.m2a", isdbt_mux::audio::AdtsCompliance::Warn)
            .unwrap();
    let quality = es.quality_mode();
    let d = tmp("aacb32");
    let bench = bench_media_config(&d, &media);
    let findings = |extra: &[&str]| {
        let ts = d.join("bench.ts");
        let cfg = bench.clone();
        let mut args = vec!["mux", "-c", s(&cfg), "--seconds", "12", "--out-ts", s(&ts)];
        args.extend_from_slice(extra);
        let o = run(&args);
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
        let o = run(&["ts", "validate", "--json", "--fail-on", "never", s(&ts)]);
        let j: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
        j["findings"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|f| f["check"].as_str().unwrap().starts_with("aac."))
            .map(|f| {
                (
                    f["severity"].as_str().unwrap().to_string(),
                    f["check"].as_str().unwrap().to_string(),
                    f["message"].as_str().unwrap().to_string(),
                )
            })
            .collect::<Vec<_>>()
    };
    let cfg = bench.clone();
    let ts = d.join("conform.ts");
    let o = run(&["mux", "-c", s(&cfg), "--seconds", "1", "--out-ts", s(&ts)]);
    let err = String::from_utf8_lossy(&o.stderr);
    assert_eq!(
        o.status.success(),
        blocking.is_empty(),
        "{blocking:?}: {err}"
    );
    if blocking.contains(&Rule::ProtectionAbsent) {
        assert!(err.contains("crc_check cannot be inserted"), "{err}");
    }
    let f = findings(&["--adts-compliance", "warn"]);
    for r in blocking
        .iter()
        .filter(|r| !matches!(r, Rule::Layer | Rule::ConstantRate))
    {
        let check = isdb_analyze::aac::check_name(*r);
        assert!(
            f.iter().any(|(sev, c, _)| sev == "error" && c == check),
            "{check}: {f:?}"
        );
    }
    let want = format!("'{:02b}' ({})", quality.code(), quality.name());
    assert!(
        f.iter()
            .any(|(_, c, m)| c == "aac.quality_indicator" && m.contains(&want)),
        "{want}: {f:?}"
    );
    std::fs::remove_dir_all(&d).unwrap();
}

/// Environment variable naming a recording of another network (an MPEG-2
/// TS file, read in place, never copied) for the opt-in relay tests.
const RELAY_ENV: &str = "ISDB_RS_RELAY_TS";

/// The recording named by `ISDB_RS_RELAY_TS`, or `None` (with a `SKIP`
/// line) when the variable is unset or the file is missing.
fn relay_recording_path(test: &str) -> Option<PathBuf> {
    let Some(p) = std::env::var_os(RELAY_ENV).map(PathBuf::from) else {
        eprintln!("SKIP {test}: {RELAY_ENV} not set (path of a recorded TS)");
        return None;
    };
    if !p.is_file() {
        eprintln!("SKIP {test}: {RELAY_ENV}={} missing", p.display());
        return None;
    }
    Some(p)
}

fn have(cmd: &str) -> bool {
    Command::new(cmd)
        .arg("-version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// Real data (opt-in, `ISDB_RS_RELAY_TS`): the first 60 s of a recording
/// rebroadcast with the shipped `isdbt-mux/examples/relay-ts.toml` (its
/// placeholder `source` replaced) and `--auto-layout` → the video takes
/// PID 0x0111, no late PES, T-STD ok, `isdb ts validate` without errors,
/// ffmpeg decodes it (at most the AAC resync at the cut-in, where the
/// recording's audio PES need not start on an ADTS frame), `isdb loopback`
/// passes, and without `--auto-layout` the default layer B either carries
/// the recording or is refused with the list of layouts that fit.
#[test]
fn relay_recording() {
    let Some(rec) = relay_recording_path("relay_recording") else {
        return;
    };
    let d = tmp("relay-rec");
    let example = Path::new(env!("CARGO_MANIFEST_DIR")).join("../isdbt-mux/examples/relay-ts.toml");
    let text = std::fs::read_to_string(example).unwrap();
    assert!(text.contains("source = \"recording.ts\""));
    let ex = d.join("relay-ts.toml");
    std::fs::write(
        &ex,
        text.replace(
            "source = \"recording.ts\"",
            &format!("source = {}", toml_string(rec.to_str().unwrap())),
        ),
    )
    .unwrap();
    let out = d.join("relay.ts");
    let o = run(&[
        "mux",
        "-c",
        s(&ex),
        "--auto-layout",
        "--seconds",
        "60",
        "--out-ts",
        s(&out),
    ]);
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(o.status.success(), "{err}");
    assert!(err.contains("-> 0x0111 video (stream_type"), "{err}");
    assert!(err.contains("late PES 0,"), "{err}");
    assert!(err.contains("T-STD 0x0111: ok"), "{err}");
    let o = run(&["ts", "validate", s(&out)]);
    assert!(o.status.success(), "validate reported errors");
    if have("ffmpeg") {
        let o = Command::new("ffmpeg")
            .args([
                "-hide_banner",
                "-nostdin",
                "-v",
                "error",
                "-i",
                s(&out),
                "-t",
                "55",
                "-f",
                "null",
                "-",
            ])
            .output()
            .unwrap();
        let log = String::from_utf8_lossy(&o.stderr);
        eprintln!("ffmpeg:\n{log}");
        assert!(o.status.success());
        assert!(!log.contains("mpeg2video"), "video decode errors:\n{log}");
        assert!(
            log.lines().count() <= 6,
            "more than the cut-in AAC resync:\n{log}"
        );
    } else {
        eprintln!("ffmpeg absent: decode check skipped");
    }
    let o = run(&[
        "loopback",
        "--mux-config",
        s(&ex),
        "--auto-layout",
        "--frames",
        "4",
        "--snr-db",
        "30",
    ]);
    assert!(o.status.success());
    assert!(String::from_utf8_lossy(&o.stderr).contains("[loopback] PASS"));
    let o = run(&[
        "mux",
        "-c",
        s(&ex),
        "--seconds",
        "1",
        "--out-ts",
        "/dev/null",
    ]);
    if !o.status.success() {
        let err = String::from_utf8_lossy(&o.stderr);
        assert!(err.contains("Mbit/s") && err.contains("--layer-b"), "{err}");
    }
    std::fs::remove_dir_all(&d).unwrap();
}

/// Synthetic MPEG-2 video (closed GOPs of 13 pictures, 1440×1080i,
/// 29.97 Hz) of ≈ 8.2 Mbit/s: picture sizes I 120 kB, P 45 kB, B 18 kB;
/// bit_rate 10 Mbit/s, vbv_buffer_size 229 kB.
fn video_es(gops: usize) -> Vec<u8> {
    let mut v = Vec::new();
    for _ in 0..gops {
        v.extend_from_slice(&[0, 0, 1, 0xB3, 0x5A, 0x04, 0x38, 0x34]);
        let x: u32 = 25_000 << 14 | 1 << 13 | 112 << 3;
        v.extend_from_slice(&x.to_be_bytes());
        v.extend_from_slice(&[0, 0, 1, 0xB5, 0x14, 0x62, 0x00, 0x01, 0x00, 0x00]);
        v.extend_from_slice(&[0, 0, 1, 0xB8, 0x00, 0x08, 0x00, 0x40]);
        let mut pics: Vec<(u16, u8, usize)> = vec![(0, 1, 120_000)];
        for k in 1..=4u16 {
            pics.push((3 * k, 2, 45_000));
            pics.push((3 * k - 2, 3, 18_000));
            pics.push((3 * k - 1, 3, 18_000));
        }
        for (tr, ty, size) in pics {
            v.extend_from_slice(&[0, 0, 1, 0x00]);
            let w: u32 = u32::from(tr) << 22 | u32::from(ty) << 19 | 0x1234 << 3;
            v.extend_from_slice(&w.to_be_bytes());
            v.extend_from_slice(&[0, 0, 1, 0xB5, 0x8F, 0xFF, 0xF3, 0x80, 0x80]);
            v.extend_from_slice(&[0, 0, 1, 0x01]);
            v.extend(std::iter::repeat_n(0x55u8, size));
        }
    }
    v
}

/// A recording of another network (CA descriptor + ECM PID, copy control;
/// unscrambled) of one program 0x0101 "テスト局" with video 0x0140, audio
/// 0x0141 and a caption stream 0x0145 in its PMT, ≈ 8.6 Mbit/s, written
/// by the isdbt-mux multiplexer.
fn foreign_recording(dir: &Path, seconds: f64) -> PathBuf {
    use isdbt_mux::{Config, Multiplexer};
    std::fs::write(dir.join("v.m2v"), video_es(6)).unwrap();
    let a = isdbt_mux::audio::SilentFrame::stereo_48k(384).build();
    std::fs::write(dir.join("a.adts"), a.repeat(122)).unwrap();
    let cfg = Config::from_toml(
        r#"
[network]
network_id = 0x7C0E
name = "ソース網"
area_code = 0x5A5
remote_control_key_id = 1
channels = [27]

[transport]
transport_stream_id = 0x7C0E

[clock]
source = "fixed"
start = "2026-01-01T11:59:50"

[[service]]
service_id = 0x0101
name = "テスト局"
provider = "テスト"
layer = "B"
pmt_pid = 0x0101
pcr_pid = 0x0111
program_descriptors = [ { tag = 0x09, hex = "0005e121" } ]
copy_control = { recording_control = "copy-one-generation", in_pmt = true, in_sdt = false }

  [[service.stream]]
  kind = "mpeg2-video"
  pid = 0x0140
  component_tag = 0x00
  source = "v.m2v"

  [[service.stream]]
  kind = "aac-adts"
  pid = 0x0141
  component_tag = 0x10
  source = "a.adts"

  [[service.stream]]
  kind = "declared"
  pid = 0x0145
  stream_type = 0x06
  component_tag = 0x30
  descriptors = [ { tag = 0xFD, hex = "0008 3d" } ]

  [[service.event]]
  event_id = 0x0101
  start = "2026-01-01T11:00:00"
  duration = "01:00:00"
  name = "元の番組"

[[raw_table]]
pid = 0x0121
layer = "B"
interval_ms = 100
sections = ["82 70 05 01 02 03 04 05"]
"#,
    )
    .unwrap();
    let p = isdbt_core::Params::new(
        isdbt_core::Mode::M3,
        isdbt_core::GuardInterval::G1_8,
        true,
        [
            isdb_cli_layer(1, isdbt_core::Modulation::Qpsk, isdbt_core::CodeRate::R2_3),
            isdb_cli_layer(
                12,
                isdbt_core::Modulation::Qam64,
                isdbt_core::CodeRate::R3_4,
            ),
            None,
        ],
    )
    .unwrap();
    let mut mux = Multiplexer::new(&cfg, &p, dir).unwrap();
    let mut out = Vec::new();
    // f64 -> usize saturates: the frames in a few seconds
    #[allow(clippy::cast_possible_truncation)]
    let frames = (seconds / p.frame_seconds()).ceil() as usize;
    for _ in 0..frames {
        for pk in mux.next_frame_full().0 {
            out.extend_from_slice(&pk);
        }
    }
    let path = dir.join("recording.ts");
    std::fs::write(&path, out).unwrap();
    path
}

fn isdb_cli_layer(
    segments: u8,
    modulation: isdbt_core::Modulation,
    code_rate: isdbt_core::CodeRate,
) -> Option<isdbt_core::LayerConfig> {
    Some(isdbt_core::LayerConfig {
        segments: isdbt_core::Segments::new(segments).unwrap(),
        modulation,
        code_rate,
        time_interleave: isdbt_core::TimeInterleave::K2,
    })
}

/// `--relay` without a TOML: roles and PIDs from the recording, the
/// over-capacity refusal and the `--auto-layout` choice, `--print-config`
/// whose TOML (with the chosen layer options in its header) reproduces the
/// same multiplex byte for byte, option values in the printed TOML, the
/// channel of `tx` from --channel / --freq, and a loopback PASS.
#[test]
fn relay_without_toml() {
    let d = tmp("relay-cli");
    let src = foreign_recording(&d, 6.0);
    let narrow = ["--layer-b", "12,16qam,1/2,2"]; // 7.49 Mbit/s < ≈ 8.6 needed
    let clock = ["--clock-start", "2026-01-02T09:00:00"];

    // refused without --auto-layout, with the layouts that fit
    let o = run(&[
        "mux",
        "--relay",
        s(&src),
        narrow[0],
        narrow[1],
        "--seconds",
        "1",
        "--report",
    ]);
    assert!(!o.status.success());
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(
        err.contains("layouts that fit")
            && err.contains("--layer-b 12,16qam,2/3,2")
            && err.contains("--auto-layout"),
        "{err}"
    );

    // --auto-layout: B 12seg 16QAM 2/3 (the most robust rate that fits)
    let a = d.join("a.ts");
    let mut args = vec![
        "mux",
        "--relay",
        s(&src),
        "--auto-layout",
        "--seconds",
        "3",
        "--out-ts",
        s(&a),
    ];
    args.extend_from_slice(&narrow);
    args.extend_from_slice(&clock);
    let o = run(&args);
    assert!(o.status.success());
    let err = String::from_utf8_lossy(&o.stderr);
    for want in [
        "--auto-layout: the relay needs",
        "using B 12seg 16QAM 2/3",
        "[--layer-b 12,16qam,2/3,2]",
        "program 0x0101 \"テスト局\" (the only program in the PAT",
        "relay 0x0140 -> 0x0111 video",
        "relay 0x0141 -> 0x0112 audio (main)",
        "relay 0x0145 -> 0x0130 captions",
        "ECM 0x0121 -> 0x0121",
        "receivers see CA signalling with a clear payload",
        "source copy control: copy-one-generation (PMT); relayed as: copy-one-generation (source)",
        "copy control as the source, CA kept",
        "B 12seg 16QAM 2/3 TI2",
    ] {
        assert!(err.contains(want), "missing {want:?}:\n{err}");
    }
    let o = run(&["ts", "validate", s(&a)]);
    assert!(o.status.success(), "validate reported errors");

    // --print-config: the same multiplex from the printed TOML
    args.retain(|x| *x != "--out-ts" && *x != s(&a));
    args.push("--print-config");
    let o = run(&args);
    assert!(o.status.success());
    let toml_text = String::from_utf8(o.stdout).unwrap();
    assert!(toml_text.contains("name = \"テスト局\""), "{toml_text}");
    assert!(toml_text.contains("channels = [44]"), "{toml_text}");
    // without --copy the relay keeps the source's copy restriction
    assert!(
        toml_text.contains("recording_control = \"source\""),
        "{toml_text}"
    );
    let layer_b = toml_text
        .split("--layer-b ")
        .nth(1)
        .and_then(|x| x.split_whitespace().next())
        .unwrap()
        .to_string();
    assert_eq!(layer_b, "12,16qam,2/3,2");
    let cfg = d.join("relay.toml");
    std::fs::write(&cfg, &toml_text).unwrap();
    let b = d.join("b.ts");
    let o = run(&[
        "mux",
        "-c",
        s(&cfg),
        "--layer-b",
        &layer_b,
        "--seconds",
        "3",
        "--out-ts",
        s(&b),
    ]);
    assert!(o.status.success());
    assert_same(&b, &std::fs::read(&a).unwrap(), "--print-config round trip");

    // options land in the configuration; tx takes the NIT channel from --channel / --freq
    let o = run(&[
        "tx",
        "--relay",
        s(&src),
        "--channel",
        "27",
        "--remote-key",
        "3",
        "--area-code",
        "0x5A5",
        "--name",
        "再送",
        "--copy",
        "copy-one-generation",
        "--at-eof",
        "end",
        "--eit-from-source",
        "false",
        "--program",
        "0x101",
        "--ca",
        "drop",
        "--print-config",
    ]);
    assert!(o.status.success());
    let t = String::from_utf8_lossy(&o.stdout);
    for want in [
        "channels = [27]",
        "remote_control_key_id = 3",
        "area_code = 0x5A5",
        "name = \"再送\"",
        "recording_control = \"copy-one-generation\"",
        "at_eof = \"end\"",
        "eit_from_source = false",
        "program = 0x0101",
        "ca = \"drop\"",
    ] {
        assert!(t.contains(want), "missing {want:?}:\n{t}");
    }
    let o = run(&[
        "tx",
        "--relay",
        s(&src),
        "--freq",
        "473142857",
        "--print-config",
    ]);
    assert!(String::from_utf8_lossy(&o.stdout).contains("channels = [13]"));
    let o = run(&[
        "tx",
        "--relay",
        s(&src),
        "--service-name",
        "nothing",
        "--print-config",
    ]);
    assert!(!o.status.success());
    assert!(String::from_utf8_lossy(&o.stderr).contains("0x0101 \"テスト局\""));

    // the in-memory configuration through the software channel
    let mut args = vec![
        "loopback",
        "--relay",
        s(&src),
        "--auto-layout",
        "--frames",
        "3",
        "--snr-db",
        "30",
    ];
    args.extend_from_slice(&narrow);
    let o = run(&args);
    assert!(o.status.success());
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(err.contains("[loopback] PASS"), "{err}");
    assert!(
        err.contains("[relay] relay ") && err.contains("T44 (NIT), remote key 9, area 0x5a5"),
        "{err}"
    );
    std::fs::remove_dir_all(&d).unwrap();
}

/// Real data, no TOML (opt-in, `ISDB_RS_RELAY_TS`): `isdb mux --relay
/// <recording> --auto-layout` for 20 s names the selected program, relays
/// its video to PID 0x0111 with no late PES, validates without errors, and
/// `isdb loopback --relay … --auto-layout` passes.  (Roles and PIDs are
/// asserted exactly on the synthetic recording of `relay_without_toml`.)
#[test]
fn relay_recording_without_toml() {
    let Some(rec) = relay_recording_path("relay_recording_without_toml") else {
        return;
    };
    let d = tmp("relay-rec-cli");
    let out = d.join("relay.ts");
    let o = run(&[
        "mux",
        "--relay",
        s(&rec),
        "--auto-layout",
        "--seconds",
        "20",
        "--out-ts",
        s(&out),
    ]);
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(o.status.success(), "{err}");
    for want in ["program 0x", "-> 0x0111 video (stream_type", "late PES 0,"] {
        assert!(err.contains(want), "missing {want:?}:\n{err}");
    }
    let o = run(&["ts", "validate", s(&out)]);
    assert!(o.status.success(), "validate reported errors");
    assert!(String::from_utf8_lossy(&o.stdout).contains("0 errors"));
    let o = run(&[
        "loopback",
        "--relay",
        s(&rec),
        "--auto-layout",
        "--frames",
        "4",
        "--snr-db",
        "30",
    ]);
    assert!(o.status.success());
    assert!(String::from_utf8_lossy(&o.stderr).contains("[loopback] PASS"));
    std::fs::remove_dir_all(&d).unwrap();
}

/// Scrambled input (transport_scrambling_control ≠ '00') is refused: isdb
/// does not descramble and does not transmit scrambled content.
#[test]
fn scrambled_input_is_refused() {
    let d = tmp("scrambled");
    let mut b = packets(0x0111, 0..2000);
    // scramble one packet in the middle ('10' = even key)
    let k = 188 * 1000 + 3;
    b[k] = (b[k] & 0x3F) | 0x80;
    let path = d.join("b.ts");
    std::fs::write(&path, &b).unwrap();
    let iq = d.join("x.cf32");
    let o = run(&["tx", "--b-ts", s(&path), "--iq", s(&iq)]);
    assert!(!o.status.success());
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(err.contains("scrambled input is refused"), "{err}");
    let o = run(&["loopback", "--b-ts", s(&path)]);
    assert!(!o.status.success());
    std::fs::remove_dir_all(&d).unwrap();
}

/// `ts validate --expect-services` takes a comma list (and repeats) of
/// service ids and compares them with the PAT/SDT, without a panic.
#[test]
fn ts_validate_expect_services() {
    let d = tmp("expect-services");
    let cfg = declared_only_config(&d);
    // service ids of the example configuration
    let ids: Vec<String> = std::fs::read_to_string(&cfg)
        .unwrap()
        .lines()
        .filter_map(|l| l.trim().strip_prefix("service_id = "))
        .map(|v| v.split_whitespace().next().unwrap().to_string())
        .collect();
    assert!(!ids.is_empty());
    let full = d.join("full.ts");
    let o = run(&["mux", "-c", s(&cfg), "--frames", "8", "--out-ts", s(&full)]);
    assert!(o.status.success());
    let list = ids.join(",");
    let o = run(&["ts", "validate", s(&full), "--expect-services", &list]);
    let out = String::from_utf8_lossy(&o.stdout);
    assert_ne!(o.status.code(), Some(101), "panicked");
    assert!(!out.contains("[expect]"), "{out}");
    // repeated flags accumulate
    let mut args = vec!["ts", "validate", s(&full)];
    for id in &ids {
        args.extend(["--expect-services", id.as_str()]);
    }
    let o = run(&args);
    assert!(!String::from_utf8_lossy(&o.stdout).contains("[expect]"));
    // a wrong list is an error finding, not a panic
    let o = run(&["ts", "validate", s(&full), "--expect-services", "0xFFF0"]);
    assert_eq!(o.status.code(), Some(1));
    let out = String::from_utf8_lossy(&o.stdout);
    assert!(out.contains("[expect]") && out.contains("fff0"), "{out}");
    // a malformed value is a usage error
    let o = run(&["ts", "validate", s(&full), "--expect-services", "0x10000"]);
    assert_eq!(o.status.code(), Some(2));
    std::fs::remove_dir_all(&d).unwrap();
}

/// Opt-in: `ISDB_RS_ISDBS3_SYMBOLS` names an ISDB-S3 `Symbols` words file
/// recorded from the FPGA front end over tens of seconds (the front end
/// has no carrier loop: the reference recording drifts −97 Hz → −830 Hz in
/// 40 s and has two stream losses). Every frame must decode but those a
/// loss touches (≈ 1 per loss plus the frame it cuts): before the tracked
/// carrier, the receiver lost every frame from ≈ 30 s on (plan §0.18).
#[test]
fn isdbs3_symbols_stream_tracks_the_drift_and_recovers_from_losses() {
    let Some(f) = std::env::var_os("ISDB_RS_ISDBS3_SYMBOLS") else {
        eprintln!(
            "SKIP isdbs3_symbols_stream_tracks_the_drift_and_recovers_from_losses: ISDB_RS_ISDBS3_SYMBOLS not set"
        );
        return;
    };
    let d = tmp("s3symbols");
    let o = run(&[
        "rx",
        "--symbols",
        f.to_str().unwrap(),
        "--out",
        s(&d.join("s-{tlv_id}.tlv")),
        "--json",
        "--status-secs",
        "0",
    ]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let j: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    let frames = j["frames"].as_u64().unwrap();
    let tmcc = j["tmcc_ok"].as_u64().unwrap();
    let losses = j["stream"]["losses"].as_u64().unwrap();
    let resyncs = j["stream"]["resyncs"].as_u64().unwrap();
    let secs = j["seconds"].as_f64().unwrap_or(0.0);
    eprintln!(
        "{secs:.1} s: {frames} frames, TMCC {tmcc}, {losses} losses, {resyncs} resyncs, tracked {}",
        j["stream"]["tracked_carrier_hz"]
    );
    assert_eq!(resyncs, 0, "lost lock");
    assert_eq!(tmcc, frames, "frames without TMCC");
    // 30.26 frames per second; each loss may cost the frame it cuts and one more
    // f64 -> u64 saturates: a frame count
    #[allow(clippy::cast_possible_truncation)]
    let expected = (secs * isdb_s3_frames_per_second()).floor() as u64;
    assert!(
        frames + 2 * losses + 2 >= expected,
        "{frames} frames in {secs} s (expected ≈ {expected})"
    );
    std::fs::remove_dir_all(&d).unwrap();
}

/// ISDB-S3 frames per second: 33.7561 Mbaud / (120 · 9 296 symbols).
fn isdb_s3_frames_per_second() -> f64 {
    33.7561e6 / (120.0 * 9296.0)
}

/// Diagnostics are `tracing` events: `--log-format json` gives one object
/// per line with the event's fields, `--log-level` filters, and the text
/// format keeps the `[tx] …` lines (the other tests match on them).
#[test]
fn diagnostics_are_structured_events() {
    let d = tmp("log-json");
    let o = run(&[
        "--log-format",
        "json",
        "tx",
        "--synthetic",
        "--seconds",
        "0.5",
        "--iq",
        s(&d.join("x.cf32")),
    ]);
    assert!(o.status.success());
    let err = String::from_utf8_lossy(&o.stderr);
    let mut done = None;
    for line in err.lines() {
        let j: serde_json::Value =
            serde_json::from_str(line).unwrap_or_else(|e| panic!("{e}: {line}"));
        assert_eq!(j["level"], "INFO", "{line}");
        assert_eq!(j["target"], "isdb::tx", "{line}");
        if j["message"]
            .as_str()
            .is_some_and(|m| m.starts_with("done ("))
        {
            done = Some(j);
        }
    }
    let done = done.expect("the end-of-run event");
    assert_eq!(done["end"], "Limit");
    assert_eq!(
        done["frames"].as_u64(),
        Some(
            done["lead_frames"].as_u64().unwrap()
                + done["data_frames"].as_u64().unwrap()
                + done["flush_frames"].as_u64().unwrap()
        )
    );

    let o = run(&[
        "--log-level",
        "warn",
        "tx",
        "--synthetic",
        "--seconds",
        "0.5",
        "--iq",
        s(&d.join("y.cf32")),
    ]);
    assert!(o.status.success());
    assert!(o.stderr.is_empty(), "info events above --log-level warn");
}

/// The transmit and tools side emits events too: with `--log-format json`,
/// `isdb mux --relay` reports the relay policy's copy control as an event
/// with `source` and `output` fields (CONTRIBUTING.md "Relay policy"),
/// `isdb loopback` ends with a `pass` verdict event, and `isdb ts edit`
/// says what it wrote with `packets` and `path`.
#[test]
fn tools_diagnostics_are_structured_events() {
    let d = tmp("log-json-tools");
    let events = |o: &Output| -> Vec<serde_json::Value> {
        String::from_utf8_lossy(&o.stderr)
            .lines()
            .map(|l| serde_json::from_str(l).unwrap_or_else(|e| panic!("{e}: {l}")))
            .collect()
    };
    let field = |e: &serde_json::Value, k: &str| e[k].as_str().unwrap_or_default().to_string();

    // mux --relay --copy copy-free: the source's value and the relaxed one
    let src = foreign_recording(&d, 2.0);
    let out = d.join("relay.ts");
    let o = run(&[
        "--log-format",
        "json",
        "mux",
        "--relay",
        s(&src),
        "--auto-layout",
        "--copy",
        "copy-free",
        "--seconds",
        "1",
        "--out-ts",
        s(&out),
    ]);
    assert!(o.status.success());
    let ev = events(&o);
    let cc = ev
        .iter()
        .find(|e| field(e, "message").contains("; relayed as: "))
        .expect("the copy-control note event");
    assert_eq!(cc["level"], "INFO", "{cc}");
    assert_eq!(cc["target"], "isdb::mux", "{cc}");
    assert_eq!(cc["source"], "copy-one-generation (PMT)", "{cc}");
    assert_eq!(cc["output"], "copy-free (explicit)", "{cc}");
    let summary = ev
        .iter()
        .find(|e| field(e, "message").starts_with("relay "))
        .expect("the relay summary event");
    assert_eq!(summary["target"], "isdb::relay", "{summary}");
    assert_eq!(summary["copy_control"], "copy-free", "{summary}");
    assert_eq!(summary["ca"], "Keep", "{summary}");

    // loopback: the verdict
    let o = run(&[
        "--log-format",
        "json",
        "loopback",
        "--synthetic",
        "--frames",
        "1",
    ]);
    assert!(o.status.success());
    let ev = events(&o);
    assert!(ev.iter().all(|e| e["target"] == "isdb::loopback"), "{ev:?}");
    let verdict = ev.last().expect("the verdict event");
    assert_eq!(verdict["message"], "PASS", "{verdict}");
    assert_eq!(verdict["pass"], true, "{verdict}");
    assert_eq!(verdict["end"], "Limit", "{verdict}");

    // ts edit: what was written
    let edited = d.join("edited.ts");
    let o = run(&[
        "--log-format",
        "json",
        "ts",
        "edit",
        s(&out),
        s(&edited),
        "--null-pid",
        "0x0111",
    ]);
    assert!(o.status.success());
    let ev = events(&o);
    let wrote = ev
        .iter()
        .find(|e| field(e, "message").starts_with("wrote "))
        .expect("the wrote event");
    assert_eq!(wrote["target"], "isdb::ts", "{wrote}");
    assert_eq!(wrote["path"], s(&edited), "{wrote}");
    assert_eq!(
        wrote["packets"].as_u64(),
        Some(std::fs::metadata(&edited).unwrap().len() / 188)
    );
    let nulled = ev
        .iter()
        .find(|e| field(e, "message").contains("nulled "))
        .expect("the nulled event");
    assert!(
        nulled["packets"].as_u64().is_some_and(|n| n > 0),
        "{nulled}"
    );
    std::fs::remove_dir_all(&d).unwrap();
}
