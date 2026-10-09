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
    p[1] = (pid >> 8) as u8;
    p[2] = pid as u8;
    p[3] = 0x10 | (n & 15) as u8;
    p[4..8].copy_from_slice(&n.to_be_bytes());
    let mut x = (n as u64 + 1).wrapping_mul(0x9e37_79b9_7f4a_7c15) ^ pid as u64;
    for b in &mut p[8..] {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        *b = (x >> 32) as u8;
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
    p[..6].copy_from_slice(&[0x47, (pid >> 8) as u8, pid as u8, 0x20, 183, 0x10]);
    p[6] = (base >> 25) as u8;
    p[7] = (base >> 17) as u8;
    p[8] = (base >> 9) as u8;
    p[9] = (base >> 1) as u8;
    p[10] = (((base & 1) << 7) as u8) | 0x7E | ((ext >> 8) as u8 & 1);
    p[11] = ext as u8;
    p
}

/// A TS of `seconds` at `bps` (all packets counted): data on PID 0x0140
/// with a PCR packet on 0x0111 every 40 ms, PCR advancing at exactly that
/// rate.  Returns the file bytes and the data packets.
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
    assert!(String::from_utf8_lossy(&o.stderr).contains("WARNING (--allow-overrate)"));
    std::fs::remove_dir_all(&d).unwrap();
}

/// Regression benchmark for the 2026-10-09 starvation report: the producer
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
    assert!(
        air >= 1.5 + 2.0 * 6000.0 / 2592.0 * FRAME_S,
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

#[test]
fn hackrf_tx_refuses_without_rf_enable() {
    // --dry-run prints the RF settings and exits before touching a device.
    let o = run(&[
        "tx",
        "--hackrf",
        "--channel",
        "13",
        "--txvga",
        "20",
        "--synthetic",
        "--dry-run",
    ]);
    assert!(!o.status.success());
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(err.contains("473.142857 MHz (UHF ch13)"), "{err}");
    assert!(err.contains("TX VGA       20 dB"));
    assert!(err.contains("RF amp       off"));
    assert!(err.contains("not transmitting"));
    // Without --rf-enable or --dry-run the command line is rejected.
    assert!(
        !run(&["tx", "--hackrf", "--channel", "13", "--synthetic"])
            .status
            .success()
    );
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

/// Directory of the bench media (`v63p.m2v`, `a.m2a`) named by the
/// environment variable `ISDB_RS_MEDIA_DIR`, or `None` (with a `SKIP` line)
/// when it is unset or a file is missing.
fn media_dir(test: &str) -> Option<PathBuf> {
    let Some(dir) = std::env::var_os("ISDB_RS_MEDIA_DIR").map(PathBuf::from) else {
        eprintln!("SKIP {test}: ISDB_RS_MEDIA_DIR not set (directory with v63p.m2v and a.m2a)");
        return None;
    };
    for f in ["v63p.m2v", "a.m2a"] {
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
        .replace("\"media/video.m2v\"", &path("v63p.m2v"))
        .replace("\"media/audio.m2a\"", &path("a.m2a"));
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
    if let Some(media) = media_dir("loopback_with_in_process_multiplexer (bench media part)") {
        let bench = bench_media_config(&d, &media);
        // the bench audio has no crc_check, which `conform` cannot insert
        // (bench_media_aac_conforms_to_b32_part2); this test is about the
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

/// `isdb mux` with the bench media (opt-in: `ISDB_RS_MEDIA_DIR`), whose ADTS has protection_absent '1' and adts_buffer_fullness
/// 0x7FF: the default `adts_compliance = "conform"` refuses it (crc_check
/// cannot be inserted); `--adts-compliance warn` sends the file unchanged,
/// the validator reports its protection_absent / adts_buffer_fullness
/// errors and the EIT carries quality_indicator '10' (Mode 2).
#[test]
fn bench_media_aac_conforms_to_b32_part2() {
    let Some(media) = media_dir("bench_media_aac_conforms_to_b32_part2") else {
        return;
    };
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
    let ts = d.join("refused.ts");
    let o = run(&["mux", "-c", s(&cfg), "--seconds", "1", "--out-ts", s(&ts)]);
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(
        !o.status.success() && err.contains("crc_check cannot be inserted"),
        "{err}"
    );
    let f = findings(&["--adts-compliance", "warn"]);
    for check in ["aac.protection_absent", "aac.adts_buffer_fullness"] {
        assert!(
            f.iter().any(|(sev, c, _)| sev == "error" && c == check),
            "{check}: {f:?}"
        );
    }
    assert!(
        f.iter()
            .any(|(_, c, m)| c == "aac.quality_indicator" && m.contains("'10' (Mode 2)")),
        "{f:?}"
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
/// On the bench BS recording (18.5 Mbit/s): B 12 seg 64QAM 7/8 chosen,
/// default layer B (16.85 Mbit/s) refused.
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
            let w: u32 = (tr as u32) << 22 | (ty as u32) << 19 | 0x1234 << 3;
            v.extend_from_slice(&w.to_be_bytes());
            v.extend_from_slice(&[0, 0, 1, 0xB5, 0x8F, 0xFF, 0xF3, 0x80, 0x80]);
            v.extend_from_slice(&[0, 0, 1, 0x01]);
            v.extend(std::iter::repeat_n(0x55u8, size));
        }
    }
    v
}

/// A BS-like recording (network 0x0004, CA descriptor + ECM PID, copy
/// control) of one program 0x00D3 "テスト局" with video 0x0140, audio
/// 0x0141 and a caption stream 0x0145 in its PMT, ≈ 8.6 Mbit/s, written
/// by the isdbt-mux multiplexer.
fn bs_like_recording(dir: &Path, seconds: f64) -> PathBuf {
    use isdbt_mux::{Config, Multiplexer};
    std::fs::write(dir.join("v.m2v"), video_es(6)).unwrap();
    let a = isdbt_mux::audio::SilentFrame::stereo_48k(384).build();
    std::fs::write(dir.join("a.adts"), a.repeat(122)).unwrap();
    let cfg = Config::from_toml(
        r#"
[network]
network_id = 0x0004
name = "BSデジタル"
area_code = 0x5A5
remote_control_key_id = 1
channels = [27]

[transport]
transport_stream_id = 0x4090

[clock]
source = "fixed"
start = "2026-10-08T11:59:50"

[[service]]
service_id = 0x00D3
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
  start = "2026-10-08T11:00:00"
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
    for _ in 0..(seconds / p.frame_seconds()).ceil() as usize {
        for pk in mux.next_frame_full().0 {
            out.extend_from_slice(&pk);
        }
    }
    let path = dir.join("recording.ts");
    std::fs::write(&path, out).unwrap();
    path
}

fn isdb_cli_layer(
    segments: usize,
    modulation: isdbt_core::Modulation,
    code_rate: isdbt_core::CodeRate,
) -> Option<isdbt_core::LayerConfig> {
    Some(isdbt_core::LayerConfig {
        segments,
        modulation,
        code_rate,
        time_interleave: isdbt_core::TimeInterleave(2),
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
    let src = bs_like_recording(&d, 6.0);
    let narrow = ["--layer-b", "12,16qam,1/2,2"]; // 7.49 Mbit/s < ≈ 8.6 needed
    let clock = ["--clock-start", "2026-10-09T09:00:00"];

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
        "program 0x00d3 \"テスト局\" (the only program in the PAT",
        "relay 0x0140 -> 0x0111 video",
        "relay 0x0141 -> 0x0112 audio (main)",
        "relay 0x0145 -> 0x0130 captions",
        "dropped PID 0x0121: ECM",
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
        "0xd3",
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
        "program = 0x00D3",
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
    assert!(String::from_utf8_lossy(&o.stderr).contains("0x00d3 \"テスト局\""));

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
        err.contains("isdb: relay ") && err.contains("UHF ch44 (NIT), remote key 9, area 0x8d5"),
        "{err}"
    );
    std::fs::remove_dir_all(&d).unwrap();
}

/// Real data, no TOML (opt-in, `ISDB_RS_RELAY_TS`): `isdb mux --relay
/// <recording> --auto-layout` for 20 s names the selected program, relays
/// its video to PID 0x0111 with no late PES, validates without errors, and
/// `isdb loopback --relay … --auto-layout` passes.  (Roles and PIDs of a
/// BS-like PMT are asserted exactly on the synthetic recording of
/// `relay_without_toml`.)
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
