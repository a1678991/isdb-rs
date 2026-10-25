//! `isdb iq`: tools for IQ recordings of any system (`isdb capture`
//! output, maia-sdr SigMF recordings, raw files).

use crate::bands::{parse_count, parse_hz};
use anyhow::{Context, Result, bail};
use clap::{Args, Subcommand};
use isdb_iq::survey::{Options, Survey, survey};
use isdb_iq::{Recording, SampleFormat};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

/// `isdb iq` arguments.
#[derive(Debug, Args)]
pub struct IqArgs {
    #[command(subcommand)]
    cmd: IqCmd,
}

#[derive(Debug, Subcommand)]
enum IqCmd {
    /// Spectrum, symbol-rate line and carrier lines of a single-carrier
    /// signal, and the system (ISDB-S, ISDB-S3) whose symbol rate matches.
    Survey(SurveyArgs),
    /// Symbols of a single-carrier recording through the CPU front end
    /// (the same words as the FPGA front end), or `--read` a words file.
    Symbols(crate::symbols::IqSymbolsArgs),
    /// ISDB-S frame sync and TMCC of a recording: modulation-code
    /// combinations, slots and the TS_ID of each slot.
    Tmcc(crate::tmcc::TmccArgs),
    /// ISDB-S3 slot sync and the SNR on its known synchronisation symbols,
    /// untrimmed and with the front end's timing trim (a recording), or of
    /// a `Symbols` words file (`--read`)
    S3sync(crate::s3sync::S3SyncArgs),
}

/// `isdb iq survey` arguments.
#[derive(Debug, Args)]
struct SurveyArgs {
    /// Recordings: data files, `.sigmf-meta` files or SigMF base names.
    #[arg(required = true)]
    files: Vec<PathBuf>,
    /// Sample format when there is no SigMF metadata (or to override it):
    /// cs8, cs16, cs16/FULLSCALE (cs16/2048 for libiio AD936x samples),
    /// cf32.
    #[arg(long, value_name = "FMT")]
    format: Option<SampleFormat>,
    /// Sample rate when there is no SigMF metadata (or to override it).
    #[arg(long, value_name = "SPS", value_parser = parse_hz)]
    rate: Option<f64>,
    /// Samples analysed (a power of two is used: the largest not above
    /// this); default 4Mi.
    #[arg(long, value_name = "N", value_parser = parse_count)]
    samples: Option<usize>,
    /// SigMF capture segment to analyse (0 = the first).
    #[arg(long, default_value_t = 0)]
    segment: usize,
    /// Print one JSON object per file (JSON lines) instead of text.
    #[arg(long)]
    json: bool,
}

/// Run `isdb iq`.
pub fn run(a: &IqArgs) -> Result<()> {
    match &a.cmd {
        IqCmd::Survey(s) => {
            for f in &s.files {
                survey_file(f, s.format, s.rate, s.samples, s.segment, s.json)?;
            }
            Ok(())
        }
        IqCmd::Symbols(s) => crate::symbols::run_iq(s),
        IqCmd::Tmcc(t) => crate::tmcc::run(t),
        IqCmd::S3sync(a) => crate::s3sync::run(a),
    }
}

/// Survey one recording and print the result.
pub fn survey_file(
    path: &Path,
    format: Option<SampleFormat>,
    rate: Option<f64>,
    samples: Option<usize>,
    segment: usize,
    as_json: bool,
) -> Result<()> {
    let mut rec = Recording::open(path, format, rate)?;
    let segs = rec.segments();
    let seg = segs
        .get(segment)
        .cloned()
        .with_context(|| format!("{}: segment {segment} of {}", path.display(), segs.len()))?;
    let opts = Options {
        fft_len: samples.map_or(1 << 22, |n| 1 << n.max(2).ilog2()),
        ..Options::default()
    };
    let len = usize::try_from(seg.end - seg.start)?.min(opts.fft_len);
    if len < opts.psd_len {
        bail!(
            "{}: segment {segment} has {len} samples; at least {} are needed",
            path.display(),
            opts.psd_len
        );
    }
    let x = rec.read(seg.start, len)?;
    let s = survey(&x, rec.sample_rate, &opts);
    let channel = rec.meta.as_ref().and_then(|m| {
        m.captures
            .get(segment)
            .and_then(|c| c.extra.get("isdb:channel"))
            .and_then(Value::as_str)
            .map(str::to_string)
    });
    let centre = rec
        .meta
        .as_ref()
        .and_then(|m| m.captures.get(segment))
        .and_then(|c| c.frequency)
        .or(rec.frequency);
    if as_json {
        println!(
            "{}",
            json_report(
                path,
                &rec,
                segs.len(),
                segment,
                centre,
                channel.as_deref(),
                &s
            )
        );
    } else {
        print!(
            "{}",
            text_report(
                path,
                &rec,
                segs.len(),
                segment,
                centre,
                channel.as_deref(),
                &s
            )
        );
    }
    Ok(())
}

fn json_report(
    path: &Path,
    rec: &Recording,
    segments: usize,
    segment: usize,
    centre: Option<f64>,
    channel: Option<&str>,
    s: &Survey,
) -> Value {
    let carrier = s.best_carrier();
    json!({
        "file": path.display().to_string(),
        "format": rec.format.to_string(),
        "sample_rate_hz": rec.sample_rate,
        "centre_hz": centre,
        "channel": channel,
        "segment": segment,
        "segments": segments,
        "samples": s.samples,
        "power_dbfs": s.power_dbfs,
        "band": {
            "centre_offset_hz": s.band.center_hz,
            "width_3db_hz": s.band.width_3db_hz,
            "width_20db_hz": s.band.width_20db_hz,
            "level_db": s.band.level_db,
        },
        "symbol_line": {"freq_hz": s.symbol_line.freq_hz, "level_db": s.symbol_line.level_db},
        "carrier_lines": s.carrier_lines.iter().map(|c| json!({
            "k": c.k, "shift_hz": c.shift_hz, "freq_hz": c.line.freq_hz,
            "level_db": c.line.level_db, "offset_hz": c.offset_hz,
        })).collect::<Vec<_>>(),
        "carrier_offset_hz": carrier.map(|c| c.offset_hz),
        "system": s.system.map(|m| json!({
            "name": m.system.name, "symbol_rate_hz": m.symbol_rate_hz, "ppm": m.ppm,
        })),
    })
}

fn text_report(
    path: &Path,
    rec: &Recording,
    segments: usize,
    segment: usize,
    centre: Option<f64>,
    channel: Option<&str>,
    s: &Survey,
) -> String {
    let mhz = |hz: f64| hz / 1e6;
    let mut o = format!(
        "{}: {} at {:.6} Msps{}{}, segment {}/{}; {} samples ({:.1} ms) surveyed\n",
        path.display(),
        rec.format,
        mhz(rec.sample_rate),
        centre.map_or(String::new(), |c| format!(", centre {:.6} MHz", mhz(c))),
        channel.map_or(String::new(), |c| format!(" ({c})")),
        segment + 1,
        segments,
        s.samples,
        s.samples as f64 / rec.sample_rate * 1e3
    );
    o += &format!("  power          {:.1} dBFS\n", s.power_dbfs);
    o += &format!(
        "  band           centre {:+.3} MHz; width {:.2} MHz at -3 dB, {} at -20 dB\n",
        mhz(s.band.center_hz),
        mhz(s.band.width_3db_hz),
        s.band
            .width_20db_hz
            .map_or("beyond the sampled band".into(), |w| format!(
                "{:.2} MHz",
                mhz(w)
            ))
    );
    o += &format!(
        "  symbol line    {:.5} MHz, {:+.1} dB{}\n",
        mhz(s.symbol_line.freq_hz),
        s.symbol_line.level_db,
        s.system.map_or(String::new(), |m| format!(
            " -> {} at {:.5} Mbaud ({:+.0} ppm)",
            m.system.name,
            mhz(m.symbol_rate_hz),
            m.ppm
        ))
    );
    for c in &s.carrier_lines {
        let name = match c.shift_hz {
            0.0 => format!("z^{}", c.k),
            sh if sh > 0.0 => format!("z^{}+R/2", c.k),
            _ => format!("z^{}-R/2", c.k),
        };
        o += &format!(
            "  {name:<14} {:+.2} kHz, {:+.1} dB -> carrier offset {:+.3} kHz\n",
            c.line.freq_hz / 1e3,
            c.line.level_db,
            c.offset_hz / 1e3
        );
    }
    if let Some(c) = s.best_carrier() {
        o += &format!(
            "  carrier        {:+.3} kHz from the tuned centre (strongest line){}\n",
            c.offset_hz / 1e3,
            centre.map_or(String::new(), |f| format!(
                " = {:.6} MHz",
                (f + c.offset_hz) / 1e6
            ))
        );
    }
    o += &format!(
        "  system         {}\n",
        s.system
            .map_or("no known symbol rate".into(), |m| m.system.name.to_string())
    );
    o
}
