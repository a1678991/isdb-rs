//! `isdb iq tmcc`: ISDB-S frame sync and TMCC decoding of a recording
//! (offline; reception to TS is Phase 3 of the ISDB-S plan).

use crate::bands::{parse_count, parse_hz};
use anyhow::{Context, Result, bail};
use clap::Args;
use isdb_iq::{Recording, SampleFormat};
use isdbs_core::TmccInfo;
use isdbs_core::frame::FRAME_SYMBOLS;
use isdbs_phy::{RxOptions, main_mer, receive};
use serde_json::{Value, json};
use std::path::PathBuf;

/// `isdb iq tmcc` arguments.
#[derive(Debug, Args)]
pub struct TmccArgs {
    /// A recording: data file, `.sigmf-meta` file or SigMF base name.
    pub file: PathBuf,
    /// Sample format when there is no SigMF metadata (cs8, cs16,
    /// cs16/FULLSCALE, cf32).
    #[arg(long, value_name = "FMT")]
    pub format: Option<SampleFormat>,
    /// Sample rate when there is no SigMF metadata.
    #[arg(long, value_name = "SPS", value_parser = parse_hz)]
    pub rate: Option<f64>,
    /// Samples processed from the first segment (default: all).
    #[arg(long, value_name = "N", value_parser = parse_count)]
    pub samples: Option<usize>,
    /// Carrier offset in Hz (default: from the survey).
    #[arg(long, value_name = "HZ", allow_negative_numbers = true)]
    pub carrier_offset: Option<f64>,
    /// One summary line instead of a line per superframe.
    #[arg(long, conflicts_with = "json")]
    pub summary: bool,
    /// Print JSON.
    #[arg(long)]
    pub json: bool,
}

/// `TC8PSK 2/3 ×48`, …
fn combinations(i: &TmccInfo) -> Vec<String> {
    i.combinations
        .iter()
        .filter_map(|c| c.combination().map(|k| format!("{k} ×{}", c.slots)))
        .collect()
}

/// The TS_IDs in use with their slot counts: `0x4010 (16 slots)`.
fn streams(i: &TmccInfo) -> Vec<(u16, usize)> {
    let ids = i.slot_ts_ids();
    let mut out: Vec<(u16, usize)> = Vec::new();
    for id in ids.iter().flatten() {
        match out.iter_mut().find(|(t, _)| t == id) {
            Some(e) => e.1 += 1,
            None => out.push((*id, 1)),
        }
    }
    out
}

fn info_json(i: &TmccInfo) -> Value {
    json!({
        "order_of_change": i.order_of_change,
        "combinations": i.combinations.iter().map(|c| json!({
            "word": c.word,
            "combination": c.combination().map(|k| k.to_string()),
            "slots": c.slots,
        })).collect::<Vec<_>>(),
        "relative_ts_ids": i.relative_ts.to_vec(),
        "ts_ids": i.ts_ids.to_vec(),
        "streams": streams(i).iter().map(|(t, n)| json!({"ts_id": t, "slots": n})).collect::<Vec<_>>(),
        "emergency": i.emergency,
        "uplink": i.uplink,
        "reserved": format!("{:062b}", i.reserved),
        "valid": i.validate().err().map_or(Value::Bool(true), |e| Value::String(e.to_string())),
    })
}

/// Run `isdb iq tmcc`.
pub fn run(a: &TmccArgs) -> Result<()> {
    let mut rec = Recording::open(&a.file, a.format, a.rate)?;
    let fs = rec.sample_rate;
    let seg = rec.segments()[0].clone();
    let n = usize::try_from(seg.end - seg.start)?.min(a.samples.unwrap_or(usize::MAX));
    let x = rec.read(seg.start, n)?;
    let s = isdbs_phy::frontend::symbols(&x, fs, a.carrier_offset)
        .context("ISDB-S front end parameters for this sample rate")?;
    if s.z.len() < 2 * FRAME_SYMBOLS {
        bail!(
            "{}: {} symbols, fewer than two ISDB-S frames ({} symbols each)",
            a.file.display(),
            s.z.len(),
            FRAME_SYMBOLS
        );
    }
    let r = receive(&s.z, &RxOptions::default());
    let headers_ok = r.frames.iter().filter(|f| f.header_ok).count();
    let pilot_mer =
        r.frames.iter().map(|f| f.pilot_mer_db).sum::<f64>() / r.frames.len().max(1) as f64;
    let rs_ok = r.superframes.iter().filter(|sf| sf.info.is_some()).count();
    let corrected: usize = r.superframes.iter().filter_map(|sf| sf.corrected).sum();
    let mers: Vec<f64> = r
        .superframes
        .iter()
        .filter_map(|sf| {
            let i = sf.info.as_ref()?;
            main_mer(&r.frames[sf.first_frame + 1], i)
        })
        .collect();
    let main_mer_db = (!mers.is_empty()).then(|| mers.iter().sum::<f64>() / mers.len() as f64);
    let last = r.superframes.iter().rev().find_map(|sf| sf.info.as_ref());
    if a.json {
        let j = json!({
            "file": a.file.display().to_string(),
            "sample_rate_hz": fs,
            "symbols": s.z.len(),
            "carrier_offset_hz": s.carrier_offset_hz,
            "symbol_rate_hz": s.symbol_rate_hz,
            "survey_isdbs": s.survey_says_isdbs,
            "frames": r.frames.len(),
            "headers_ok": headers_ok,
            "slips": r.slips,
            "pilot_mer_db": pilot_mer,
            "main_mer_db": main_mer_db,
            "superframes": r.superframes.iter().map(|sf| json!({
                "first_frame": sf.first_frame,
                "start_symbol": r.frames[sf.first_frame].mark.start,
                "rs_corrected": sf.corrected,
                "tmcc": sf.info.as_ref().map(info_json),
            })).collect::<Vec<_>>(),
        });
        println!("{}", serde_json::to_string_pretty(&j)?);
        return Ok(());
    }
    let tail = last.map_or("no TMCC decoded".into(), |i| {
        format!(
            "{}; TS {}",
            combinations(i).join(" + "),
            streams(i)
                .iter()
                .map(|(t, n)| format!("{t:#06x} ({n} slots)"))
                .collect::<Vec<_>>()
                .join(", ")
        )
    });
    let mer = main_mer_db.map_or("-".into(), |m| format!("{m:.1} dB"));
    if a.summary {
        println!(
            "{} frames ({headers_ok} headers ok), {} slips; {} superframes, RS ok {rs_ok} ({corrected} bytes corrected); pilot MER {pilot_mer:.1} dB, main MER {mer}; {tail}",
            r.frames.len(),
            r.slips,
            r.superframes.len()
        );
        return Ok(());
    }
    println!(
        "{}: {} samples at {fs:.0} Hz → {} symbols ({:.1} Bd, carrier {:+.0} Hz{})",
        a.file.display(),
        x.len(),
        s.z.len(),
        s.symbol_rate_hz,
        s.carrier_offset_hz,
        if s.survey_says_isdbs {
            ""
        } else {
            "; the survey did not match ISDB-S"
        }
    );
    println!(
        "frames {} ({headers_ok} with intact sync words), slips {}, pilot MER {pilot_mer:.1} dB, main MER {mer}",
        r.frames.len(),
        r.slips
    );
    let mut prev: Option<&TmccInfo> = None;
    for sf in &r.superframes {
        let at = r.frames[sf.first_frame].mark.start;
        match (&sf.info, sf.corrected) {
            (Some(i), Some(c)) => {
                if prev != Some(i) {
                    println!(
                        "superframe @ symbol {at}: RS ok ({c} corrected); order of change {}; {tail}; emergency {}, uplink {}{}",
                        i.order_of_change,
                        i.emergency,
                        i.uplink,
                        i.validate()
                            .err()
                            .map_or(String::new(), |e| format!("; INCONSISTENT: {e}")),
                        tail = combinations(i).join(" + ")
                            + "; TS "
                            + &streams(i)
                                .iter()
                                .map(|(t, n)| format!("{t:#06x} ({n} slots)"))
                                .collect::<Vec<_>>()
                                .join(", ")
                    );
                } else {
                    println!("superframe @ symbol {at}: RS ok ({c} corrected), unchanged");
                }
                prev = Some(i);
            }
            _ => println!("superframe @ symbol {at}: RS(64,48) uncorrectable"),
        }
    }
    Ok(())
}
