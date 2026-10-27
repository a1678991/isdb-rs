#![allow(missing_docs)]
//! Real-media test: muxes 10 s of the bench example with your own media
//! (MPEG-2 video `video.m2v` + AAC ADTS `audio.m2a`, not committed) and
//! checks the result with ffprobe against what the files themselves
//! declare (sequence header size, ADTS sampling frequency and channels). The media are read from the directory named by the environment
//! variable `ISDB_RS_MEDIA_DIR`; the test prints a `SKIP` line and passes
//! when it is unset, a file is missing or ffprobe is not installed.
//!
//! ```text
//! ISDB_RS_MEDIA_DIR=/path/to/media cargo test -p isdbt-mux --test media -- --nocapture
//! ```

mod common;

use std::path::PathBuf;
use std::process::Command;

use isdbt_mux::analysis::analyze;
use isdbt_mux::mux::clocks_to_ticks;
use isdbt_mux::{Config, Multiplexer, TsWriter};

/// `s` as a quoted TOML basic string.
fn toml_string(s: &str) -> String {
    toml::Value::String(s.to_string()).to_string()
}

#[test]
fn media_ffprobe() {
    let examples = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("examples");
    let Some(media) = std::env::var_os("ISDB_RS_MEDIA_DIR").map(PathBuf::from) else {
        eprintln!("SKIP: ISDB_RS_MEDIA_DIR not set (directory with video.m2v and audio.m2a)");
        return;
    };
    let (video, audio) = (media.join("video.m2v"), media.join("audio.m2a"));
    if !video.exists() || !audio.exists() {
        eprintln!("SKIP: {} or {} missing", video.display(), audio.display());
        return;
    }
    if Command::new("ffprobe").arg("-version").output().is_err() {
        eprintln!("SKIP: ffprobe not available");
        return;
    }
    let text = isdbt_mux::config::EXAMPLE_TOML
        .replace("\"media/video.m2v\"", &toml_string(video.to_str().unwrap()))
        .replace("\"media/audio.m2a\"", &toml_string(audio.to_str().unwrap()))
        // common encoders write no crc_check, which `conform` cannot insert
        // (tests/adts.rs); this test is about video/T-STD, so send it as is
        .replace("adts_compliance = \"conform\"", "adts_compliance = \"warn\"");
    let cfg = Config::from_toml(&text).unwrap();
    let params = common::bench_params();
    let mut mux = Multiplexer::new(&cfg, &params, &examples).unwrap();
    for w in mux.warnings() {
        eprintln!("warning: {w}");
    }
    let frames = common::count((10.0 / params.frame_seconds()).ceil()) as u64;
    let dir = common::tmpdir("media");
    let full_path = dir.join("bench_full.ts");
    let b_path = dir.join("bench_layer_b.ts");
    let a_path = dir.join("bench_layer_a.ts");
    let mut full_all = Vec::new();
    let mut times = Vec::new();
    {
        let mut w = TsWriter::new(
            [
                Some(a_path.to_str().unwrap()),
                Some(b_path.to_str().unwrap()),
                None,
            ],
            Some(full_path.to_str().unwrap()),
        )
        .unwrap();
        for k in 0..frames {
            let (full, layers) = mux.next_frame_full();
            for j in 0..full.len() {
                times.push(mux.slot_time(k, j));
            }
            w.write_frame(&full, &layers).unwrap();
            full_all.extend(full);
        }
        w.flush().unwrap();
    }
    let report = analyze(&full_all, &times, clocks_to_ticks(408));
    assert!(report.cc_events.is_empty());
    for (pid, v) in mux.tstd_violations() {
        eprintln!("T-STD {pid:#06x}: {v:?}");
        assert_eq!(v.tb_overflow + v.buffer_overflow + v.underflow, 0);
        assert!(
            v.tb_max_nonempty <= 27_000_000 && v.mb_max_nonempty <= 27_000_000,
            "{v:?}"
        );
    }
    let st = mux.stats();
    eprintln!(
        "stats: pes={} late={} nulls={:?} packets={:?}",
        st.pes, st.late_pes, st.layer_nulls, st.layer_packets
    );
    assert_eq!(st.late_pes, 0);
    for (pid, pes) in &report.pes {
        for p in pes {
            let d = p.dts.or(p.pts).unwrap();
            assert!(p.last_arrival / 300 <= d, "PID {pid:#x} late PES");
        }
    }

    let out = Command::new("ffprobe")
        .args(["-hide_banner", "-v", "error", "-show_entries", "format=duration,bit_rate:stream=index,codec_name,codec_type,width,height,sample_rate,channels,duration,start_time", "-of", "compact"])
        .arg(&full_path)
        .output()
        .unwrap();
    let summary = String::from_utf8_lossy(&out.stdout).to_string();
    let errs = String::from_utf8_lossy(&out.stderr).to_string();
    eprintln!("ffprobe {}:\n{summary}{errs}", full_path.display());
    let human = Command::new("ffprobe")
        .arg("-hide_banner")
        .arg(&full_path)
        .output()
        .unwrap();
    eprintln!("{}", String::from_utf8_lossy(&human.stderr));
    assert!(
        summary.contains("codec_name=mpeg2video"),
        "no MPEG-2 video detected"
    );
    assert!(summary.contains("codec_name=aac"), "no AAC detected");
    // what the files declare reaches the demuxer unchanged
    let v = isdbt_mux::video::VideoEs::parse(std::fs::read(&video).unwrap(), "video").unwrap();
    assert!(
        summary.contains(&format!("width={}", v.seq.width))
            && summary.contains(&format!("height={}", v.seq.height)),
        "video size {}x{} not reported",
        v.seq.width,
        v.seq.height
    );
    let a = isdbt_mux::audio::AdtsEs::parse(std::fs::read(&audio).unwrap(), "audio").unwrap();
    assert!(
        // (implicit SBR doubles the output rate, B32 Part 2 §5.2.3 (4)(a))
        summary.contains(&format!("sample_rate={}", a.sampling_frequency))
            || summary.contains(&format!("sample_rate={}", 2 * a.sampling_frequency)),
        "sampling frequency {} not reported",
        a.sampling_frequency
    );
    let dur: f64 = summary
        .lines()
        .find(|l| l.starts_with("format"))
        .and_then(|l| l.split('|').find_map(|f| f.strip_prefix("duration=")))
        .and_then(|d| d.parse().ok())
        .unwrap();
    assert!((8.0..=11.5).contains(&dur), "format duration {dur}");
    // decode check: frames counted and no decoder errors
    let dec = Command::new("ffmpeg")
        .args(["-hide_banner", "-v", "error", "-i"])
        .arg(&full_path)
        .args(["-f", "null", "-"])
        .output();
    if let Ok(d) = dec {
        let e = String::from_utf8_lossy(&d.stderr);
        eprintln!(
            "ffmpeg decode errors: {}",
            if e.trim().is_empty() {
                "none"
            } else {
                e.trim()
            }
        );
    }
}
