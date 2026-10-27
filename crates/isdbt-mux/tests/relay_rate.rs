//! The rate a `ts-program` relay needs comes from the whole recording, not
//! from its probed start. The source here is light for its first seconds
//! (≈ 1 Mbit/s of video) and heavy after (≈ 11 Mbit/s), like a recording
//! that begins with the quiet tail of the previous programme. Sized from
//! the probe's mean, a small layer was accepted and the relay then fell
//! ever further behind its PCR schedule, its PES (audio first) leaving
//! after their decoding time. Checks: the probe's whole-file profile;
//! refusal of a layer that carries the start but not the rest; with
//! `rate_check = "warn"`, late PES and a run-time alert; a layer that
//! carries the need relays without a late PES.

mod common;

use common::{bench_params, synth_audio, tmpdir};
use isdbt_core::{CodeRate, LayerConfig, Modulation, Params, Segments, TimeInterleave};
use isdbt_mux::{Config, Multiplexer, MuxError};
use std::path::{Path, PathBuf};

/// MPEG-2 video (bit_rate 12 Mbit/s, vbv_buffer_size 7 Mbit): closed GOPs
/// of 13 pictures, I 110 kB, P 55 kB, B 35 kB divided by each GOP's entry
/// of `divisors`.
fn video(divisors: &[usize]) -> Vec<u8> {
    let mut v = Vec::new();
    for &d in divisors {
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
            v.extend(std::iter::repeat_n(0x55u8, size / d));
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

/// The source: 8 light GOPs (3.5 s) then 12 heavy ones (5.2 s), 8.6 s in
/// all, multiplexed by this crate on the bench layout.
fn source(dir: &Path) -> PathBuf {
    let mut gops = vec![10; 8];
    gops.extend([1; 12]);
    std::fs::write(dir.join("v.m2v"), video(&gops)).unwrap();
    std::fs::write(dir.join("a.adts"), synth_audio(403)).unwrap();
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
    for _ in 0..common::count((8.6 / p.frame_seconds()).ceil()) {
        let (full, _) = mux.next_frame_full();
        for pk in full {
            out.extend_from_slice(&pk);
        }
    }
    let path = dir.join("light-then-heavy.ts");
    std::fs::write(&path, out).unwrap();
    path
}

/// A one-second probe, so that it sees only the light start.
fn relay_cfg(src: &Path, extra: &str) -> Config {
    Config::from_toml(&format!(
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
  probe_seconds = 1
  at_eof = "end"
{extra}
"#,
        src.display()
    ))
    .unwrap()
}

/// The bench layout with layer B = 12 segments of `modulation` `rate`.
fn with_layer_b(modulation: Modulation, code_rate: CodeRate) -> Params {
    let b = bench_params();
    Params::new(
        b.mode(),
        b.guard(),
        b.partial_reception(),
        [
            b.layers()[0],
            Some(LayerConfig {
                segments: Segments::new(12).unwrap(),
                modulation,
                code_rate,
                time_interleave: TimeInterleave::K2,
            }),
            None,
        ],
    )
    .unwrap()
}

/// Run until the relay has sent the whole source; the run-time alerts.
fn run_to_end(mux: &mut Multiplexer) -> Vec<String> {
    let mut alerts = Vec::new();
    for _ in 0..200 {
        if mux.relays_finished() {
            break;
        }
        mux.next_frame();
        alerts.extend(mux.take_alerts());
    }
    assert!(mux.relays_finished(), "the relay did not end");
    alerts
}

#[test]
fn the_whole_recording_sets_the_rate() {
    let dir = tmpdir("relay-rate");
    let src = source(&dir);

    // the probe's mean is the light start; the whole-file need is the rest
    let st = relay_cfg(&src, "").services[0].streams[0].clone();
    let probe = isdbt_mux::relay::probe(&src, &st).unwrap();
    assert!(probe.mean_bps < 3e6, "probed mean {}", probe.mean_bps);
    let rate = probe.rate;
    assert!(rate.span_s > 8.0, "{rate:?}");
    assert!(rate.mean_bps > 6e6, "{rate:?}");
    assert!(rate.required_bps > 9e6, "{rate:?}");
    assert!(rate.required_bps >= rate.mean_bps, "{rate:?}");
    assert!(rate.peak_1s_bps >= rate.required_bps * 0.99, "{rate:?}");
    assert!(
        probe.report().contains("whole file: "),
        "{}",
        probe.report()
    );

    // 12 segments 16QAM 1/2 (≈ 7.5 Mbit/s) carry the start, not the rest
    let small = with_layer_b(Modulation::Qam16, CodeRate::R1_2);
    let cap = isdbt_mux::capacity::layer_bps(&small, isdbt_core::Layer::B);
    assert!(probe.mean_bps < cap && cap < rate.required_bps, "{cap}");
    match Multiplexer::new(&relay_cfg(&src, ""), &small, Path::new(".")) {
        Err(MuxError::OverCapacity {
            need_bps, message, ..
        }) => {
            assert!(need_bps > rate.required_bps, "{need_bps}");
            assert!(message.contains("over the whole file"), "{message}");
        }
        Err(e) => panic!("wrong error: {e}"),
        Ok(_) => panic!("a layer below the recording's need was accepted"),
    }

    // relayed anyway: it falls behind, PES leave late, and that is alerted
    let mut mux = Multiplexer::new(
        &relay_cfg(&src, "  rate_check = \"warn\""),
        &small,
        Path::new("."),
    )
    .unwrap();
    let alerts = run_to_end(&mut mux);
    let st = mux.relay_stats()[0];
    assert!(st.late_pes > 0, "{st:?}");
    assert!(st.max_lateness > 27_000_000 / 2, "{st:?}");
    assert!(
        alerts
            .iter()
            .any(|a| a.contains("after their decoding time")),
        "{alerts:?}"
    );

    // a layer that carries the need: every PES in time, no alert
    let fits = with_layer_b(Modulation::Qam64, CodeRate::R2_3);
    let cap = isdbt_mux::capacity::layer_bps(&fits, isdbt_core::Layer::B);
    assert!(cap > rate.required_bps + 0.2e6, "{cap}");
    let mut mux = Multiplexer::new(&relay_cfg(&src, ""), &fits, Path::new(".")).unwrap();
    let alerts = run_to_end(&mut mux);
    let st = mux.relay_stats()[0];
    assert_eq!(st.late_pes, 0, "{st:?}");
    assert!(alerts.is_empty(), "{alerts:?}");
}
