//! `isdb mux`: service multiplexer — TOML description + media files →
//! per-layer transport streams paced to the ISDB-T OFDM frame (exactly
//! `tsp_per_frame` packets per layer per frame), ready for `isdb tx`.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use isdbt_mux::analysis::analyze;
use isdbt_mux::audio::AdtsCompliance;
use isdbt_mux::config::StreamKind;
use isdbt_mux::mux::{HZ, clocks_to_ticks};
use isdbt_mux::{Config, TsWriter};
use tracing::{error, info, warn};

use crate::input::copy_control_note;
use crate::phyargs::{PhyArgs, describe_params};
use crate::relay::{RelayArgs, build_mux, log_summary, print_config};

/// `isdb mux` arguments.
#[derive(clap::Args, Debug)]
pub struct MuxArgs {
    /// Multiplex description (TOML); see --print-config-example.
    #[arg(
        long,
        short = 'c',
        value_name = "TOML",
        required_unless_present_any = ["print_config_example", "relay"],
        conflicts_with = "relay"
    )]
    pub config: Option<PathBuf>,
    /// `--relay FILE` and its options: rebroadcast a recording without a
    /// TOML file (see `isdb tx --help`).
    #[command(flatten)]
    pub relay: RelayArgs,
    /// --relay: terrestrial channel named in the NIT (default 44, i.e. T44).
    #[arg(long, value_name = "13..62", requires = "relay")]
    pub channel: Option<u32>,
    /// Transmission parameters (same syntax as `isdb tx`); they fix the
    /// packets per layer per frame and the values ARIB derives from them.
    #[command(flatten)]
    pub phy: PhyArgs,
    /// Layer A transport stream output ("-" = stdout).
    #[arg(long = "out-a", value_name = "PATH")]
    pub out_a: Option<String>,
    /// Layer B transport stream output ("-" = stdout).
    #[arg(long = "out-b", value_name = "PATH")]
    pub out_b: Option<String>,
    /// Layer C transport stream output ("-" = stdout).
    #[arg(long = "out-c", value_name = "PATH")]
    pub out_c: Option<String>,
    /// The single multiplex-frame TS a 13-segment receiver reproduces
    /// (all layers interleaved in slot order, null slots included;
    /// "-" = stdout). Usable with `isdb tx` as one TS split by PID, and
    /// with ffprobe/players.
    #[arg(long = "out-ts", value_name = "PATH")]
    pub out_ts: Option<String>,
    /// Number of OFDM frames to produce.
    #[arg(long, conflicts_with = "seconds")]
    pub frames: Option<u64>,
    /// Duration in seconds of stream time (default 10; media loop).
    #[arg(long)]
    pub seconds: Option<f64>,
    /// Print the example configuration (examples/bench.toml) and exit.
    #[arg(long)]
    pub print_config_example: bool,
    /// Measure the output (section intervals, PCR, timestamps, T-STD) and
    /// print a report on stderr.
    #[arg(long)]
    pub report: bool,
    /// Override `adts_compliance` of every aac-adts stream: what to do
    /// with ADTS whose headers violate ARIB STD-B32 Part 2 §5.2 (conform:
    /// send conforming input unchanged, refuse anything else — crc_check
    /// is never inserted and protected headers are never rewritten;
    /// warn: send unchanged and report; pass-through: send unchanged).
    #[arg(long, value_enum, value_name = "POLICY")]
    pub adts_compliance: Option<AdtsPolicy>,
}

/// `--adts-compliance` values (= `isdbt_mux::audio::AdtsCompliance`).
#[derive(clap::ValueEnum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum AdtsPolicy {
    /// Send conforming input unchanged, refuse any header violation.
    Conform,
    /// Send unchanged, report every violation as a warning.
    Warn,
    /// Send unchanged without checking.
    PassThrough,
}

impl From<AdtsPolicy> for AdtsCompliance {
    fn from(p: AdtsPolicy) -> Self {
        match p {
            AdtsPolicy::Conform => AdtsCompliance::Conform,
            AdtsPolicy::Warn => AdtsCompliance::Warn,
            AdtsPolicy::PassThrough => AdtsCompliance::PassThrough,
        }
    }
}

/// Run `isdb mux`.
pub fn run(args: MuxArgs) -> Result<()> {
    if args.print_config_example {
        print!("{}", isdbt_mux::config::EXAMPLE_TOML);
        return Ok(());
    }
    let params = args.phy.params()?;
    let (mut cfg, base) = if args.relay.relay.is_some() {
        let o = args.relay.options(args.channel.unwrap_or(44), &params)?;
        if args.relay.print_config {
            return print_config(&o, &params, args.relay.auto_layout);
        }
        log_summary(args.relay.relay.as_deref().unwrap_or(Path::new("")), &o);
        (o.config()?, PathBuf::from("."))
    } else {
        let path = args.config.as_ref().context("--config is required")?;
        let text =
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        let cfg =
            Config::from_toml(&text).with_context(|| format!("parsing {}", path.display()))?;
        (cfg, path.parent().map(PathBuf::from).unwrap_or_default())
    };
    if let Some(p) = args.adts_compliance {
        for st in cfg.services.iter_mut().flat_map(|s| s.streams.iter_mut()) {
            if st.kind == StreamKind::AacAdts {
                st.adts_compliance = p.into();
            }
        }
    }
    let outs = [&args.out_a, &args.out_b, &args.out_c, &args.out_ts];
    if outs.iter().filter(|o| o.as_deref() == Some("-")).count() > 1 {
        bail!("only one output can be stdout");
    }
    if outs.iter().all(|o| o.is_none()) && !args.report {
        bail!("nothing to do: give --out-a/--out-b/--out-c/--out-ts and/or --report");
    }
    let (mut mux, params) = build_mux(&cfg, &base, params, args.relay.auto_layout)?;
    for (i, o) in [&args.out_a, &args.out_b, &args.out_c].iter().enumerate() {
        if o.is_some() && params.layers()[i].is_none() {
            bail!(
                "layer {} output requested but the layer is not active",
                ["A", "B", "C"][i]
            );
        }
    }
    let frames = match (args.frames, args.seconds) {
        (Some(f), _) => f,
        (None, s) => {
            // f64 -> u64 saturates: a frame count (0 for a negative --seconds)
            #[allow(clippy::cast_possible_truncation)]
            let n = (s.unwrap_or(10.0) / params.frame_seconds()).ceil() as u64;
            n
        }
    };
    info!("{}", describe_params(&params));
    info!(
        frames,
        seconds = frames as f64 * params.frame_seconds(),
        clock_start = %isdbt_mux::jst::format(mux.clock_start()),
        "{frames} frames ({:.2} s), clock start {} JST",
        frames as f64 * params.frame_seconds(),
        isdbt_mux::jst::format(mux.clock_start())
    );
    for n in mux.notes() {
        match copy_control_note(n) {
            Some((source, output)) => info!(source, output, "{n}"),
            None => info!("{n}"),
        }
    }
    for w in mux.warnings() {
        warn!("{w}");
    }
    let mut writer = TsWriter::new(
        [
            args.out_a.as_deref(),
            args.out_b.as_deref(),
            args.out_c.as_deref(),
        ],
        args.out_ts.as_deref(),
    )?;
    let mut full_all = Vec::new();
    let mut times = Vec::new();
    for k in 0..frames {
        if mux.relays_finished() {
            info!(frames = k, "every ts-program source ended after {k} frames");
            break;
        }
        let (full, layers) = mux.next_frame_full();
        for w in mux.take_alerts() {
            warn!("{w}");
        }
        writer.write_frame(&full, &layers)?;
        if args.report {
            times.extend((0..full.len()).map(|j| mux.slot_time(k, j)));
            full_all.extend(full);
        }
    }
    writer.flush()?;

    let st = mux.stats();
    for (i, name) in ["A", "B", "C"].iter().enumerate() {
        if st.layer_packets[i] > 0 {
            info!(
                layer = name,
                packets_per_frame =
                    params.layers()[i].map_or(0, |l| l.tsp_per_frame(params.mode())),
                packets = st.layer_packets[i],
                null_packets = st.layer_nulls[i],
                null_fill_pct = 100.0 * st.layer_nulls[i] as f64 / st.layer_packets[i] as f64,
                "layer {name}: {} packets/frame, {} packets, {} null ({:.1} %)",
                params.layers()[i].map_or(0, |l| l.tsp_per_frame(params.mode())),
                st.layer_packets[i],
                st.layer_nulls[i],
                100.0 * st.layer_nulls[i] as f64 / st.layer_packets[i] as f64
            );
        }
    }
    info!(
        pes = st.pes,
        late_pes = st.late_pes,
        version_changes = st.version_changes,
        "PES packets: {}, late: {}, SI version changes: {}",
        st.pes,
        st.late_pes,
        st.version_changes
    );
    for (pid, v) in mux.tstd_violations() {
        let bad = v.tb_overflow + v.buffer_overflow + v.underflow;
        let line = format!(
            "T-STD {pid:#06x}: {} (TB overflow {}, buffer overflow {}, underflow {}, max TB busy {:.0} ms, max MB busy {:.0} ms)",
            if bad == 0 { "ok" } else { "VIOLATIONS" },
            v.tb_overflow,
            v.buffer_overflow,
            v.underflow,
            v.tb_max_nonempty as f64 * 1e3 / HZ as f64,
            v.mb_max_nonempty as f64 * 1e3 / HZ as f64
        );
        if bad == 0 {
            info!(pid = %pid, tb_overflow = v.tb_overflow, buffer_overflow = v.buffer_overflow, underflow = v.underflow, "{line}");
        } else {
            warn!(pid = %pid, tb_overflow = v.tb_overflow, buffer_overflow = v.buffer_overflow, underflow = v.underflow, "{line}");
        }
    }
    for r in mux.relay_status() {
        info!("{r}");
    }
    for e in &st.errors {
        error!("{e}");
    }

    if args.report {
        let r = analyze(&full_all, &times, clocks_to_ticks(408));
        info!(
            packets = full_all.len(),
            "--- report (multiplex-frame TS, {} packets)",
            full_all.len()
        );
        for ((pid, tid, ext, sn), (gap, n)) in r.section_intervals() {
            info!(
                pid = %pid,
                table_id = tid,
                extension = ext,
                section_number = sn,
                times = n,
                max_interval_ms = gap as f64 * 1e3 / HZ as f64,
                "section pid {pid:#06x} table_id {tid:#04x} ext {ext:#06x} sec {sn}: {n} times, max interval {:.1} ms",
                gap as f64 * 1e3 / HZ as f64
            );
        }
        for (pid, p) in &r.pcrs {
            let max = p.windows(2).map(|w| w[1].2 - w[0].2).max().unwrap_or(0);
            info!(
                pid = %pid,
                pcrs = p.len(),
                max_interval_ms = max as f64 * 1e3 / HZ as f64,
                "PCR pid {pid:#06x}: {} PCRs, max interval {:.2} ms",
                p.len(),
                max as f64 * 1e3 / HZ as f64
            );
        }
        for (pid, pes) in &r.pes {
            let late = pes
                .iter()
                .filter(|p| p.last_arrival / 300 > p.dts.or(p.pts).unwrap_or(u64::MAX))
                .count();
            let first = pes.first().and_then(|p| p.pts).unwrap_or(0) as f64 / 90_000.0;
            info!(
                pid = %pid,
                packets = pes.len(),
                first_pts_s = first,
                late,
                "PES pid {pid:#06x}: {} packets, first PTS {first:.3} s, arriving after DTS: {late}",
                pes.len()
            );
        }
        info!(
            cc_events = r.cc_events.len(),
            "continuity events: {}",
            r.cc_events.len()
        );
        for p in r.problems.iter().take(10) {
            warn!("problem: {p}");
        }
    }
    Ok(())
}
