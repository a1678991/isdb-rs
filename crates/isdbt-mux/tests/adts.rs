#![allow(missing_docs)]
//! ADTS header checks / buffer-fullness model / B32 Part 2 §5.2 gate on
//! real encoder output (optional inputs from environment variables; each
//! one unset or missing prints `SKIP` and passes):
//!
//! * `ISDB_RS_MEDIA_DIR/a.m2a`, the bench media (an encoder that writes
//!   no CRC): protection_absent '1' and
//!   adts_buffer_fullness 0x7FF in every frame, so [`b32::conform`]
//!   refuses it (crc_check cannot be inserted without a raw_data_block
//!   walk) and reports the constant-rate fullness model for reference;
//!   `warn` sends it unchanged;
//! * `ISDB_RS_CAPTURE_TS`, a terrestrial capture: every frame of every
//!   ADTS PID passes the header rules and passes [`b32::conform`]
//!   byte-identically.  (During development, the full-seg encoder of a live
//!   capture also followed the constant-rate reservoir recurrence of
//!   [`b32::cbr_fullness`] exactly.)

use std::path::PathBuf;

use isdbt_mux::adts::AdtsHeader;
use isdbt_mux::adts::b32::{self, QualityMode, Rule};
use isdbt_mux::audio::{AdtsCompliance, AdtsEs};

/// `$var` (joined with `rel` when given), or `None` with a SKIP line.
fn env_path(var: &str, rel: Option<&str>) -> Option<PathBuf> {
    let Some(base) = std::env::var_os(var).map(PathBuf::from) else {
        eprintln!("SKIP: {var} not set");
        return None;
    };
    Some(match rel {
        Some(r) => base.join(r),
        None => base,
    })
}

#[test]
fn bench_media_refused_by_conform() {
    let Some(src) = env_path("ISDB_RS_MEDIA_DIR", Some("a.m2a")) else {
        return;
    };
    let Ok(data) = std::fs::read(&src) else {
        eprintln!("SKIP: {} absent", src.display());
        return;
    };
    let (rep, _) = b32::check_stream(&data).unwrap();
    eprintln!("input:\n  {}", rep.lines().join("\n  "));
    assert_eq!(rep.frames, 470);
    for r in [Rule::ProtectionAbsent, Rule::BufferFullness] {
        assert_eq!(rep.rules[&r].frames, 470, "{r}");
    }
    let e = b32::conform(&data).unwrap_err();
    eprintln!("{e}");
    assert!(e.reason.contains("crc_check cannot be inserted"), "{e}");
    let m = e.fullness.as_ref().unwrap();
    assert_eq!(m.channels, 2);
    let cap = ((12288 - m.rate_frame_bits) / 64) as u16;
    assert!(m.values.iter().all(|&f| f != 0x7FF && f <= cap));
    let (es, w) = AdtsEs::load(data.clone(), "a.m2a", AdtsCompliance::Warn).unwrap();
    assert_eq!(es.data, data);
    assert!(w.iter().any(|l| l.contains("for reference")), "{w:?}");
    eprintln!(
        "bit rate {:.0} bit/s, {:?}",
        es.bit_rate(),
        es.quality_mode()
    );
    assert_eq!(es.quality_mode(), QualityMode::Mode2);
    assert_eq!(es.quality_mode().code(), 0b10);
}

/// ADTS bytes of one PID (PES payloads concatenated), from the first
/// syncword after the first PUSI.
fn pid_es(ts: &[u8], pid: u16) -> Vec<u8> {
    let mut es = Vec::new();
    let mut started = false;
    for p in ts.as_chunks::<188>().0 {
        if p[0] != 0x47 || (((p[1] & 0x1F) as u16) << 8 | p[2] as u16) != pid {
            continue;
        }
        let afc = p[3] >> 4 & 3;
        let mut off = 4;
        if afc & 2 != 0 {
            off += 1 + p[4] as usize;
        }
        if afc & 1 == 0 || off >= 188 {
            continue;
        }
        let pl = &p[off..];
        if p[1] & 0x40 != 0 {
            // not a PES PID (sections, or a malformed header)
            if pl.len() < 9 || pl[..3] != [0, 0, 1] || 9 + pl[8] as usize > pl.len() {
                return Vec::new();
            }
            started = true;
            es.extend_from_slice(&pl[9 + pl[8] as usize..]);
        } else if started {
            es.extend_from_slice(pl);
        }
    }
    let start = (0..es.len().saturating_sub(1))
        .find(|&i| es[i] == 0xFF && es[i + 1] & 0xF6 == 0xF0)
        .unwrap_or(0);
    es.split_off(start)
}

#[test]
fn capture_all_frames_conform() {
    let Some(path) = env_path("ISDB_RS_CAPTURE_TS", None) else {
        return;
    };
    let Ok(ts) = std::fs::read(&path) else {
        eprintln!("SKIP: {} absent", path.display());
        return;
    };
    let mut pids: Vec<u16> = ts
        .as_chunks::<188>()
        .0
        .iter()
        .filter(|p| p[0] == 0x47 && p[1] & 0x40 != 0)
        .map(|p| ((p[1] & 0x1F) as u16) << 8 | p[2] as u16)
        .collect();
    pids.sort_unstable();
    pids.dedup();
    let mut audio = 0;
    for pid in pids {
        let es = pid_es(&ts, pid);
        // an audio PID: its PES payload parses as ≥ 20 ADTS frames
        let Ok((rep, checks)) = b32::check_stream(&es) else {
            continue;
        };
        if rep.frames < 20 {
            continue;
        }
        audio += 1;
        eprintln!("PID {pid:#06x}: {} frames, {:?}", rep.frames, rep.lines());
        assert!(rep.rules.is_empty(), "PID {pid:#06x}: {:?}", rep.lines());
        assert!(checks.iter().all(|c| c.header.crc_check.is_some()));
        // conforming input passes byte-identically
        let spans = b32::split_frames(&es).unwrap();
        let end = spans.last().map(|&(o, l)| o + l).unwrap();
        assert_eq!(b32::conform(&es[..end]).unwrap().data, &es[..end]);
        let sfi = checks[0].header.sampling_frequency_index;
        let fs = isdbt_mux::adts::SAMPLING_FREQUENCIES[sfi as usize];
        let bytes: u64 = checks
            .iter()
            .map(|c| c.header.aac_frame_length as u64)
            .sum();
        let bps = b32::bit_rate(bytes, 1024 * rep.frames as u64, fs);
        eprintln!("  {bps:.0} bit/s → {:?}", QualityMode::derive(fs, bps, 2));
        // adts_buffer_fullness never signals variable rate
        let h: Vec<AdtsHeader> = spans
            .iter()
            .map(|&(o, _)| AdtsHeader::parse(&es[o..]).unwrap())
            .collect();
        assert!(h.iter().all(|h| h.adts_buffer_fullness != 0x7FF));
    }
    assert!(audio > 0, "no ADTS PID found in {}", path.display());
}
