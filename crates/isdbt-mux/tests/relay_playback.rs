//! Playback control of a `ts-program` relay (pause, seek, play rate) on a
//! source multiplexed by this crate (MPEG-2 video with I/P/B pictures,
//! stereo AAC). Checks on the multiplex: video DTS and audio PTS increase,
//! audio PES are contiguous one ADTS frame each, no PES leaves after its
//! decoding time, the relay sends nothing while paused, and the position
//! moves by the rate, the pause and the seeks.

mod common;

use common::{bench_params, synth_audio, tmpdir};
use isdbt_mux::{Config, Multiplexer, Packet};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

const HZ: f64 = 27e6;
const VIDEO: u16 = 0x0111;
const AUDIO: u16 = 0x0112;
/// One AAC frame at 48 kHz in 90 kHz units.
const FRAME_90K: i64 = 1920;

/// MPEG-2 video: closed GOPs of 13 pictures (I, then P B B ×4), I 60 kB,
/// P 30 kB, B 15 kB (≈ 6 Mbit/s).
fn video(gops: usize) -> Vec<u8> {
    let mut v = Vec::new();
    for _ in 0..gops {
        v.extend_from_slice(&[0, 0, 1, 0xB3]);
        let (w, h): (u32, u32) = (1440, 1080);
        v.extend_from_slice(&[
            (w >> 4) as u8,
            ((w & 0xF) << 4 | h >> 8) as u8,
            h as u8,
            0x34,
        ]);
        let x: u32 = 30_000u32 << 14 | 1 << 13 | 427 << 3;
        v.extend_from_slice(&x.to_be_bytes());
        v.extend_from_slice(&[0, 0, 1, 0xB5, 0x14, 0x62, 0x00, 0x01, 0x00, 0x00]);
        v.extend_from_slice(&[0, 0, 1, 0xB8, 0x00, 0x08, 0x00, 0x40]);
        let mut pics: Vec<(u16, u8, usize)> = vec![(0, 1, 60_000)];
        for k in 1..=4u16 {
            pics.push((3 * k, 2, 30_000));
            pics.push((3 * k - 2, 3, 15_000));
            pics.push((3 * k - 1, 3, 15_000));
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

const NETWORK: &str = r#"
[network]
network_id = 0x79EE
name = "テスト"
area_code = 0x5A5
remote_control_key_id = 9
channels = [27]

[transport]
transport_stream_id = 0x79EE

[clock]
source = "fixed"
start = "2026-01-01T12:00:00"
"#;

/// A 40 s source (its media loop: 46 GOPs ≈ 20 s, twice), made once for
/// all the tests of this file (they run in parallel).
fn source() -> PathBuf {
    static SOURCE: OnceLock<PathBuf> = OnceLock::new();
    SOURCE
        .get_or_init(|| make_source(&tmpdir("relay-playback")))
        .clone()
}

fn make_source(dir: &Path) -> PathBuf {
    let path = dir.join("playback-source.ts");
    std::fs::write(dir.join("v.m2v"), video(46)).unwrap();
    std::fs::write(dir.join("a.adts"), synth_audio(937)).unwrap();
    let text = format!(
        r#"{NETWORK}
[[service]]
service_id = 0x0400
name = "ソース"
layer = "B"
pmt_pid = 0x01F0
pcr_pid = 0x01FF

  [[service.stream]]
  kind = "mpeg2-video"
  pid = 0x0111
  component_tag = 0x00
  source = "v.m2v"

  [[service.stream]]
  kind = "aac-adts"
  pid = 0x0112
  component_tag = 0x10
  source = "a.adts"
"#
    );
    let cfg = Config::from_toml(&text).unwrap();
    let p = bench_params();
    let mut mux = Multiplexer::new(&cfg, &p, dir).unwrap();
    let mut out = Vec::new();
    for _ in 0..(40.0 / p.frame_seconds()).ceil() as usize {
        let (full, _) = mux.next_frame_full();
        for pk in full {
            out.extend_from_slice(&pk);
        }
    }
    std::fs::write(&path, out).unwrap();
    path
}

fn relay(src: &Path) -> Multiplexer {
    let cfg = Config::from_toml(&format!(
        r#"{NETWORK}
[[service]]
service_id = 0x0400
name = "再送信"
layer = "B"
pmt_pid = 0x01F0
pcr_pid = 0x01FF

  [[service.stream]]
  kind = "ts-program"
  source = "{}"
"#,
        src.display()
    ))
    .unwrap();
    let mut m = Multiplexer::new(&cfg, &bench_params(), Path::new(".")).unwrap();
    m.enable_playback_control().unwrap();
    m
}

/// A PES start of the relayed video or audio: PID, PTS, DTS.
#[derive(Debug, Clone, Copy)]
struct Start {
    pid: u16,
    pts: Option<i64>,
    dts: Option<i64>,
}

fn ts90(b: &[u8]) -> i64 {
    (((b[0] as i64 >> 1) & 7) << 30)
        | ((b[1] as i64) << 22)
        | ((b[2] as i64 >> 1) << 15)
        | ((b[3] as i64) << 7)
        | (b[4] as i64 >> 1)
}

#[derive(Default)]
struct Run {
    starts: Vec<Start>,
    /// Slot times of every relayed video / audio packet.
    packets: Vec<(f64, u16)>,
}

/// Run `seconds` of frames, recording the relayed PES starts.
fn run(m: &mut Multiplexer, seconds: f64, r: &mut Run) {
    let p = bench_params();
    for _ in 0..(seconds / p.frame_seconds()).ceil() as usize {
        let k = m.frames();
        let (full, _) = m.next_frame_full();
        for (j, pk) in full.iter().enumerate() {
            note(m.slot_time(k, j) as f64 / HZ, pk, r);
        }
    }
}

fn note(t: f64, p: &Packet, r: &mut Run) {
    let pid = (u16::from(p[1] & 0x1F) << 8) | u16::from(p[2]);
    if pid != VIDEO && pid != AUDIO {
        return;
    }
    r.packets.push((t, pid));
    if p[1] & 0x40 == 0 {
        return;
    }
    let afc = (p[3] >> 4) & 3;
    let s = if afc & 2 != 0 { 5 + p[4] as usize } else { 4 };
    let pl = &p[s..];
    let flags = pl[7] >> 6;
    r.starts.push(Start {
        pid,
        pts: (flags & 2 != 0).then(|| ts90(&pl[9..14])),
        dts: (flags == 3).then(|| ts90(&pl[14..19])),
    });
}

/// Every check that holds at any rate and after any control change.
fn check(m: &Multiplexer, r: &Run, what: &str) {
    let st = m.relay_stats()[0];
    assert_eq!(st.late_pes, 0, "{what}: late PES {st:?}");
    // video: decoding times increase
    let v: Vec<i64> = r
        .starts
        .iter()
        .filter(|s| s.pid == VIDEO)
        .filter_map(|s| s.dts.or(s.pts))
        .collect();
    assert!(v.len() > 10, "{what}: {} video PES", v.len());
    assert!(
        v.windows(2).all(|w| w[1] > w[0]),
        "{what}: video DTS not increasing"
    );
    // audio: one ADTS frame per PES, contiguous PTS (except across a pause
    // or seek, where it jumps forward)
    let a: Vec<i64> = r
        .starts
        .iter()
        .filter(|s| s.pid == AUDIO)
        .filter_map(|s| s.pts)
        .collect();
    assert!(a.len() > 10, "{what}: {} audio PES", a.len());
    let steps: Vec<i64> = a.windows(2).map(|w| w[1] - w[0]).collect();
    assert!(
        steps.iter().all(|&d| d >= FRAME_90K),
        "{what}: audio PTS step below a frame"
    );
    let contiguous = steps.iter().filter(|&&d| d == FRAME_90K).count();
    assert!(
        contiguous + 3 >= steps.len(),
        "{what}: {} of {} audio steps contiguous",
        contiguous,
        steps.len()
    );
}

fn position(m: &Multiplexer) -> f64 {
    m.playback_status()[0].position_s
}

#[test]
fn normal_play_under_control_is_clean() {
    let mut m = relay(&source());
    let mut r = Run::default();
    run(&mut m, 8.0, &mut r);
    check(&m, &r, "1×");
    let p = position(&m);
    assert!((6.5..8.5).contains(&p), "position {p}");
}

#[test]
fn pause_sends_nothing_and_resume_continues() {
    let mut m = relay(&source());
    let mut r = Run::default();
    run(&mut m, 4.0, &mut r);
    let before = position(&m);
    assert_eq!(m.toggle_pause(), Some(true));
    let t_pause = m.slot_time(m.frames(), 0) as f64 / HZ;
    run(&mut m, 3.0, &mut r);
    assert!(m.playback_status()[0].paused);
    // after the units in progress complete (well within 0.5 s), no relayed
    // packet until the resume
    let during = r.packets.iter().filter(|(t, _)| *t > t_pause + 0.5).count();
    assert_eq!(during, 0, "{during} relayed packets while paused");
    let still = position(&m);
    assert!(
        (still - before).abs() < 1.0,
        "{before} → {still} while paused"
    );
    assert_eq!(m.toggle_pause(), Some(false));
    run(&mut m, 4.0, &mut r);
    check(&m, &r, "pause");
    let after = position(&m);
    assert!(
        (after - still - 4.0).abs() < 1.5,
        "{still} → {after} in 4 s after resume"
    );
}

#[test]
fn fast_play_skips_and_stays_in_time() {
    let mut m = relay(&source());
    let mut r = Run::default();
    run(&mut m, 3.0, &mut r);
    for rate in [2.0, 4.0] {
        assert!(m.set_rate(rate));
        let p0 = position(&m);
        run(&mut m, 4.0, &mut r);
        let moved = position(&m) - p0;
        assert!(
            (moved - 4.0 * rate).abs() < 0.3 * 4.0 * rate,
            "{rate}×: moved {moved} s in 4 s"
        );
        check(&m, &r, &format!("{rate}×"));
    }
    let st = m.relay_stats()[0];
    assert!(
        st.skipped_pictures > 0 && st.skipped_audio_frames > 0,
        "{st:?}"
    );
}

#[test]
fn slow_play_repeats_audio() {
    let mut m = relay(&source());
    let mut r = Run::default();
    run(&mut m, 3.0, &mut r);
    assert!(m.set_rate(0.5));
    let p0 = position(&m);
    run(&mut m, 4.0, &mut r);
    let moved = position(&m) - p0;
    assert!((moved - 2.0).abs() < 0.8, "0.5×: moved {moved} s in 4 s");
    check(&m, &r, "0.5×");
    let st = m.relay_stats()[0];
    assert!(st.repeated_audio_frames > 50, "{st:?}");
    assert_eq!(st.skipped_pictures, 0, "{st:?}");
}

#[test]
fn seek_forward_and_back() {
    let mut m = relay(&source());
    let mut r = Run::default();
    run(&mut m, 4.0, &mut r);
    let p0 = position(&m);
    let to = m.seek(15.0);
    assert_eq!(to.len(), 1);
    assert!((to[0] - (p0 + 15.0)).abs() < 1.0, "{p0} + 15 → {to:?}");
    run(&mut m, 4.0, &mut r);
    let p1 = position(&m);
    assert!(
        (p1 - (p0 + 15.0 + 4.0)).abs() < 2.5,
        "{p0} + 15 s seek + 4 s → {p1}"
    );
    m.seek(-10.0);
    run(&mut m, 4.0, &mut r);
    let p2 = position(&m);
    assert!((p2 - (p1 - 10.0 + 4.0)).abs() < 2.5, "{p1} − 10 + 4 → {p2}");
    check(&m, &r, "seeks");
}
