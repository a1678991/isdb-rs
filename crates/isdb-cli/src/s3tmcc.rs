//! `isdb iq s3tmcc`: ISDB-S3 slot and frame sync and TMCC decoding of a
//! recording (ARIB STD-B44 §3.11), one line per frame or a summary.

use crate::bands::parse_count;
use anyhow::{Context, Result, bail};
use clap::Args;
use isdb_iq::Recording;
use isdbs3_core::TmccInfo;
use isdbs3_phy::frontend::{Trim, symbols};
use isdbs3_phy::sync::synchronise;
use isdbs3_phy::tmcc::{decode, llrs};
use serde_json::{Value, json};
use std::path::PathBuf;

/// `isdb iq s3tmcc` arguments.
#[derive(Debug, Args)]
pub struct S3TmccArgs {
    /// A SigMF recording (data file, `.sigmf-meta` or base name).
    pub file: PathBuf,
    /// Samples processed from the first segment (default: all).
    #[arg(long, value_name = "N", value_parser = parse_count)]
    pub samples: Option<usize>,
    /// Carrier offset in Hz (default: from the survey).
    #[arg(long, value_name = "HZ", allow_negative_numbers = true)]
    pub carrier_offset: Option<f64>,
    /// One summary line instead of a line per frame.
    #[arg(long, conflicts_with = "json")]
    pub summary: bool,
    /// Print JSON.
    #[arg(long)]
    pub json: bool,
}

/// `16APSK 93/120 (7/9) ×120`, …
fn modes(t: &TmccInfo) -> Vec<String> {
    t.modes
        .iter()
        .filter(|m| m.is_used())
        .filter_map(|m| {
            Some(format!(
                "{} {} ×{}",
                m.modulation()?.name(),
                m.rate()?,
                m.slots
            ))
        })
        .collect()
}

/// `0xb0b0 TLV (40 slots)`, …
fn streams(t: &TmccInfo) -> Vec<String> {
    (0..16)
        .filter(|&k| t.stream_type[k] != 0xFF)
        .map(|k| {
            let n = t
                .slot_stream
                .iter()
                .filter(|&&s| usize::from(s) == k)
                .count();
            let ty = match t.stream_type[k] {
                isdbs3_core::tmcc::STREAM_TS => "TS".to_string(),
                isdbs3_core::tmcc::STREAM_TLV => "TLV".to_string(),
                v => format!("type {v:#04x}"),
            };
            format!("{:#06x} {ty} ({n} slots)", t.stream_id[k])
        })
        .collect()
}

fn info_json(t: &TmccInfo) -> Value {
    json!({
        "order_of_change": t.change,
        "allocation_ok": t.allocation_ok(),
        "modes": t.modes.iter().filter(|m| m.is_used()).map(|m| json!({
            "modulation_code": m.modulation_code,
            "modulation": m.modulation().map(|x| x.name()),
            "rate_code": m.rate_code,
            "code_rate": m.rate().map(|r| format!("{}/120", r.numerator())),
            "slots": m.slots,
            "obo_db": f64::from(m.obo) / 10.0,
        })).collect::<Vec<_>>(),
        "streams": (0..16).filter(|&k| t.stream_type[k] != 0xFF).map(|k| json!({
            "relative": k,
            "type": t.stream_type[k],
            "id": t.stream_id[k],
            "slots": t.slot_stream.iter().filter(|&&s| usize::from(s) == k).count(),
        })).collect::<Vec<_>>(),
        "ews": t.ews,
        "uplink": t.uplink,
        "extension_id": t.extension_id,
    })
}

/// Run `isdb iq s3tmcc`.
pub fn run(a: &S3TmccArgs) -> Result<()> {
    let mut rec = Recording::open(&a.file, None, None)
        .with_context(|| format!("{}: needs its SigMF metadata", a.file.display()))?;
    let seg = rec.segments()[0].clone();
    let n = a
        .samples
        .map_or(seg.end - seg.start, |n| (n as u64).min(seg.end - seg.start));
    let x = rec.read(seg.start, usize::try_from(n)?)?;
    let s = symbols(&x, rec.sample_rate, a.carrier_offset, Trim::Auto)
        .context("ISDB-S3 front end parameters for this sample rate")?;
    let Some(sy) = synchronise(&s.z) else {
        bail!("no ISDB-S3 slot sync found (fewer than three slots, or no signal)");
    };
    let es = 10f64.powf(sy.sync_snr_db / 10.0);
    let mut frames = Vec::new();
    for &f in &sy.frames {
        let Some(l) = llrs(&s.z, &sy.slots, f, 1.0 / (2.0 * es)) else {
            break;
        };
        frames.push((f, decode(&l, 50)));
    }
    let ok = frames.iter().filter(|(_, r)| r.is_ok()).count();
    if a.json {
        let j = json!({
            "file": a.file.display().to_string(),
            "carrier_offset_hz": s.carrier_offset_hz,
            "trim": s.trim,
            "survey_isdbs3": s.survey_says_isdbs3,
            "slots": sy.slots.len(),
            "sync_esn0_db": sy.sync_snr_db,
            "frames": frames.iter().map(|(f, r)| match r {
                Ok(d) => json!({"slot": f, "ok": true, "ldpc_iterations": d.ldpc_iterations, "bch_corrected": d.bch_corrected, "tmcc": info_json(&d.info)}),
                Err(e) => json!({"slot": f, "ok": false, "error": e.to_string()}),
            }).collect::<Vec<_>>(),
        });
        println!("{}", serde_json::to_string_pretty(&j)?);
        return Ok(());
    }
    let last = frames.iter().rev().find_map(|(_, r)| r.as_ref().ok());
    if a.summary {
        println!(
            "{} frames, TMCC ok {ok}; sync Es/N0 {:.1} dB; {}; streams {}",
            frames.len(),
            sy.sync_snr_db,
            last.map_or("-".into(), |d| modes(&d.info).join(" + ")),
            last.map_or("-".into(), |d| streams(&d.info).join(", "))
        );
        return Ok(());
    }
    println!(
        "{}: {} slots, carrier {:+.0} Hz, trim {:.3}, sync Es/N0 {:.2} dB",
        a.file.display(),
        sy.slots.len(),
        s.carrier_offset_hz,
        s.trim,
        sy.sync_snr_db
    );
    for (f, r) in &frames {
        match r {
            Ok(d) => println!(
                "frame at slot {f}: change {} | {} | {} | EWS {} | LDPC {} it, BCH {} bits",
                d.info.change,
                modes(&d.info).join(" + "),
                streams(&d.info).join(", "),
                u8::from(d.info.ews),
                d.ldpc_iterations,
                d.bch_corrected
            ),
            Err(e) => println!("frame at slot {f}: {e}"),
        }
    }
    Ok(())
}
