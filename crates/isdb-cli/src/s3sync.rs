//! `isdb iq s3sync`: the ISDB-S3 slot synchronisation and the SNR on its
//! known synchronisation symbols (`isdb_frontend::s3sync`), from a
//! recording through the CPU front end or from a `Symbols` words file. The
//! measure of an ISDB-S3 recording's quality before a decoder exists, and
//! the yardstick of the front end's timing trim.

use crate::bands::parse_count;
use crate::symbols::TrimArg;
use anyhow::{Context, Result, bail};
use clap::Args;
use isdb_frontend::fast::FastFrontend as Frontend;
use isdb_frontend::format::{Event, Reader};
use isdb_frontend::params::{Design, Params};
use isdb_frontend::s3sync::{self, SLOT, SyncReport};
use isdb_iq::Recording;
use isdb_iq::survey::{KNOWN_SYSTEMS, Options, survey};
use isdbs_phy::Cplx;
use serde_json::json;
use std::path::PathBuf;

/// `isdb iq s3sync` arguments.
#[derive(Debug, Args)]
pub struct S3SyncArgs {
    /// An ISDB-S3 recording (data file, `.sigmf-meta` or SigMF base name),
    /// or with `--read` a `Symbols` words file.
    pub file: PathBuf,
    /// Read a words file (`isdb symbols` / `isdb iq symbols --out`).
    #[arg(long)]
    pub read: bool,
    /// Output trim of the front end, symbol periods, or `auto` (measured
    /// on the first 0.15 s); a recording also gets the untrimmed figure
    /// for comparison.
    #[arg(long, value_name = "auto|T", default_value = "auto", value_parser = crate::symbols::parse_trim)]
    pub trim: TrimArg,
    /// Taps per phase of the front end's filters.
    #[arg(long, default_value_t = 32)]
    pub taps: usize,
    /// Samples processed from the first segment (default: all).
    #[arg(long, value_name = "N", value_parser = parse_count)]
    pub samples: Option<usize>,
    /// Print JSON on stdout.
    #[arg(long)]
    pub json: bool,
}

fn report_json(r: &SyncReport) -> serde_json::Value {
    json!({
        "snr_db": r.snr_db,
        "slots": r.slots,
        "offset": r.offset,
        "peak": r.peak,
        "off_peak": r.off_peak,
        "ssync_parity": r.ssync_parity,
        "fsync": r.fsync,
        "nfsync": r.nfsync,
        "phase_step_rad": {"mean": r.phase_step_mean, "rms": r.phase_step_rms},
        "residual_carrier_hz": r.residual_hz(KNOWN_SYSTEMS[1].symbol_rate_hz),
    })
}

fn print_report(label: &str, r: &SyncReport) {
    println!(
        "{label}: sync-symbol Es/N0 {:.2} dB over {} slots; slot sync at symbol {} of {SLOT} (correlation {:.3}, {:.3} elsewhere); odd slots FSync {} / !FSync {}; carrier phase step {:+.4} ± {:.4} rad between SSync slots ({:+.1} Hz)",
        r.snr_db,
        r.slots,
        r.offset,
        r.peak,
        r.off_peak,
        r.fsync,
        r.nfsync,
        r.phase_step_mean,
        r.phase_step_rms,
        r.residual_hz(KNOWN_SYSTEMS[1].symbol_rate_hz)
    );
}

/// The front end's output symbols for `x` (12-bit input) with design `d`,
/// skipping the loop's settling (three slots).
fn output(x: &[[i16; 2]], d: &Design) -> Result<Vec<Cplx>> {
    let mut fe = Frontend::new(Params::design(d)?)?;
    let mut out = Vec::new();
    fe.process(x, &mut out);
    Ok(out
        .iter()
        .skip(3 * SLOT)
        .map(|s| Cplx::new(f64::from(s.zk[0]), f64::from(s.zk[1])))
        .collect())
}

/// Run `isdb iq s3sync`.
pub fn run(a: &S3SyncArgs) -> Result<()> {
    if a.read {
        let bytes = std::fs::read(&a.file).with_context(|| format!("read {}", a.file.display()))?;
        let mut r = Reader::new();
        let mut ev = Vec::new();
        r.feed_bytes(&bytes, &mut ev);
        r.finish(&mut ev);
        let mut z = Vec::new();
        let mut lost = 0u64;
        for e in ev {
            match e {
                Event::Block { symbols, .. } => z.extend(
                    symbols
                        .iter()
                        .map(|s| Cplx::new(f64::from(s[0]), f64::from(s[1]))),
                ),
                Event::Loss { blocks, .. } => lost += blocks,
                Event::Truncated { .. } => {}
            }
        }
        let rep = s3sync::measure(&z).context("fewer than three ISDB-S3 slots of symbols")?;
        if a.json {
            let mut j = report_json(&rep);
            j["file"] = json!(a.file.display().to_string());
            j["lost_blocks"] = json!(lost);
            println!("{j}");
        } else {
            if lost > 0 {
                println!("{lost} lost blocks: the slot sync spans the gaps unaligned");
            }
            print_report(&a.file.display().to_string(), &rep);
        }
        return Ok(());
    }
    let mut rec = Recording::open(&a.file, None, None)?;
    let fs = rec.sample_rate;
    let seg = rec.segments()[0].clone();
    let n = usize::try_from(seg.end - seg.start)?.min(a.samples.unwrap_or(usize::MAX));
    let x = rec.read(seg.start, n)?;
    let sv = survey(&x[..x.len().min(1 << 22)], fs, &Options::default());
    if sv
        .system
        .is_some_and(|m| m.system.name != KNOWN_SYSTEMS[1].name)
    {
        bail!(
            "{}: the survey finds {}, not ISDB-S3",
            a.file.display(),
            sv.system.map_or("?", |m| m.system.name)
        );
    }
    let mut d = Design::for_system(&KNOWN_SYSTEMS[1], fs);
    d.carrier_offset = sv.best_carrier().map_or(0.0, |c| c.offset_hz);
    d.taps = a.taps;
    let xi: Vec<[i16; 2]> = x
        .iter()
        .map(|v| {
            [
                (v.re * 2048.0).round().clamp(-2048.0, 2047.0) as i16,
                (v.im * 2048.0).round().clamp(-2048.0, 2047.0) as i16,
            ]
        })
        .collect();
    let trim = match a.trim {
        TrimArg::Fixed(t) => t,
        TrimArg::Auto => {
            let m = ((0.15 * fs) as usize).min(xi.len());
            let cands: Vec<f64> = (-6..=1).map(|i| f64::from(i) * 0.05).collect();
            s3sync::best_trim(&xi[..m], &d, &cands)?.0
        }
    };
    let plain = s3sync::measure(&output(&xi, &d)?).context("no ISDB-S3 slot sync")?;
    d.trim = trim;
    let trimmed = s3sync::measure(&output(&xi, &d)?).context("no ISDB-S3 slot sync")?;
    if a.json {
        println!(
            "{}",
            json!({
                "file": a.file.display().to_string(),
                "sample_rate_hz": fs,
                "carrier_offset_hz": d.carrier_offset,
                "taps": a.taps,
                "trim_symbols": trim,
                "untrimmed": report_json(&plain),
                "trimmed": report_json(&trimmed),
            })
        );
    } else {
        println!(
            "{}: {} samples at {fs:.0} Hz, carrier {:+.0} Hz, {} taps",
            a.file.display(),
            xi.len(),
            d.carrier_offset,
            a.taps
        );
        print_report("  untrimmed", &plain);
        print_report(&format!("  trim {trim:+.3}"), &trimmed);
    }
    Ok(())
}
