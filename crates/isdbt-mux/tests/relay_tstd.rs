//! Relaying a transport stream that this crate multiplexed itself: the
//! source's video is paced by the multiplexer's T-STD scheduler, which
//! fills MBn/EBn to their limits (H.222.0 §2.4.2.3), so the source has no
//! buffer headroom. The relay's packets leave in the first free slot of
//! the output layer at or after their source time, i.e. up to about a
//! millisecond later and unevenly; without the short re-timing hold
//! ([`isdbt_mux::relay::RETIME_HOLD`]) that jitter overflowed MBn by a packet or two at
//! hundreds of points although the source itself is clean.

mod common;

use common::{bench_params, synth_audio, tmpdir};
use isdbt_mux::{Config, Multiplexer};
use std::path::{Path, PathBuf};

/// MPEG-2 video at about 11 Mbit/s (bit_rate 12 Mbit/s, vbv_buffer_size
/// 7 Mbit): closed GOPs of 13 pictures, I 110 kB, P 55 kB, B 35 kB.
fn heavy_video(gops: usize) -> Vec<u8> {
    let mut v = Vec::new();
    for _ in 0..gops {
        v.extend_from_slice(&[0, 0, 1, 0xB3]);
        let (w, h): (u32, u32) = (1440, 1080);
        v.extend_from_slice(&[
            u8::try_from(w >> 4).unwrap(),
            u8::try_from((w & 0xF) << 4 | h >> 8).unwrap(),
            (h & 0xFF) as u8,
            0x34,
        ]);
        let br = 30_000u32; // × 400 = 12 Mbit/s
        let vbv = 427u32; // × 16 384 bits ≈ 7 Mbit
        let x: u32 = br << 14 | 1 << 13 | vbv << 3;
        v.extend_from_slice(&x.to_be_bytes());
        // sequence_extension: Main@High-1440, interlaced, 4:2:0
        v.extend_from_slice(&[0, 0, 1, 0xB5, 0x14, 0x62, 0x00, 0x01, 0x00, 0x00]);
        // closed GOP
        v.extend_from_slice(&[0, 0, 1, 0xB8, 0x00, 0x08, 0x00, 0x40]);
        let mut pics: Vec<(u16, u8, usize)> = vec![(0, 1, 110_000)];
        for k in 1..=4u16 {
            pics.push((3 * k, 2, 55_000));
            pics.push((3 * k - 2, 3, 35_000));
            pics.push((3 * k - 1, 3, 35_000));
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

fn frames_of(mux: &mut Multiplexer, seconds: f64) -> Vec<u8> {
    let frames = common::count((seconds / bench_params().frame_seconds()).ceil());
    let mut out = Vec::new();
    for _ in 0..frames {
        let (full, _) = mux.next_frame_full();
        for pk in full {
            out.extend_from_slice(&pk);
        }
    }
    out
}

fn assert_clean(mux: &Multiplexer, what: &str) {
    for (pid, v) in mux.tstd_violations() {
        assert_eq!(
            v.tb_overflow + v.buffer_overflow + v.underflow,
            0,
            "{what}: T-STD {pid:#06x}: {v:?}"
        );
        // H.222.0 §2.4.2.6 (leak method): MBn empties at least once a second
        assert!(
            v.mb_max_nonempty <= 27_000_000,
            "{what}: T-STD {pid:#06x}: MBn busy {} ms",
            v.mb_max_nonempty / 27_000
        );
    }
}

/// The source: the bench layout with heavy video, multiplexed by this
/// crate (its own T-STD model must find it clean).
fn source(dir: &Path, seconds: f64) -> PathBuf {
    std::fs::write(dir.join("v.m2v"), heavy_video(10)).unwrap();
    std::fs::write(dir.join("a.adts"), synth_audio(200)).unwrap();
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
    let mut mux = Multiplexer::new(&cfg, &bench_params(), dir).unwrap();
    let out = frames_of(&mut mux, seconds);
    assert_clean(&mux, "source");
    let path = dir.join("own.ts");
    std::fs::write(&path, out).unwrap();
    path
}

#[test]
fn relaying_own_output_keeps_the_t_std_clean() {
    let dir = tmpdir("relay-own-output");
    let src = source(&dir, 8.0);
    let text = format!(
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
    );
    let cfg = Config::from_toml(&text).unwrap();
    let mut mux = Multiplexer::new(&cfg, &bench_params(), Path::new(".")).unwrap();
    let notes = mux.notes().join("\n");
    assert!(
        notes.contains("T-STD on the source timing: ok"),
        "the probe finds the source clean:\n{notes}"
    );
    frames_of(&mut mux, 6.0);
    assert_clean(&mux, "relay");
    let st = mux.relay_stats()[0];
    assert_eq!(st.late_pes, 0);
    // the hold only absorbs slot jitter: packets stay within a few ms of
    // the source's pace
    assert!(
        st.max_lateness <= 27_000_000 / 200,
        "max lateness {} µs",
        st.max_lateness / 27
    );
}
