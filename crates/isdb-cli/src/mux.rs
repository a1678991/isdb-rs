//! `isdb mux`: service multiplexer — TOML description + media files →
//! per-layer transport streams paced to the ISDB-T OFDM frame (exactly
//! `tsp_per_frame` packets per layer per frame), ready for `isdb tx`.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use isdbs_core::combination::Combination;
use isdbt_mux::analysis::analyze;
use isdbt_mux::audio::AdtsCompliance;
use isdbt_mux::config::StreamKind;
use isdbt_mux::mux::{HZ, clocks_to_ticks};
use isdbt_mux::{Config, Multiplexer, Packet, TsWriter};
use tracing::{error, info, warn};

use crate::input::copy_control_note;
use crate::phyargs::{PhyArgs, describe_params};
use crate::relay::{RelayArgs, build_mux, log_summary, print_config};
use crate::txs::{
    Given, Layout, MuxPcrGrid, MuxSource, SatArgs, TsPacing, TxSystem, open_muxes,
    superframe_seconds, superframes_for_seconds,
};

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
    /// Transmission system the TS is timed for: ISDB-T (OFDM frames, the
    /// default) or ISDB-S: the configuration's TS (`[network] kind = "bs"`,
    /// its services on layer "A"), timed for every sent slot of
    /// --combination, superframe by superframe, as `isdb tx --system isdbs
    /// --mux-config` sends it: the TS a receiver outputs for its TS_ID,
    /// nulls included (--out-ts only; --frames counts superframes).
    #[arg(long, value_enum, default_value_t = TxSystem::Isdbt)]
    pub system: TxSystem,
    /// ISDB-S slot layout, as `isdb tx --combination` (repeatable; default
    /// tc8psk2/3:48).
    #[arg(long, value_name = "WORD:N", value_parser = crate::txs::parse_combination, help_heading = "ISDB-S (--system isdbs)")]
    pub combination: Vec<(Combination, u8)>,
    /// ISDB-S PCR timing, as `isdb tx --mux-pcr-grid`: `uniform` (the
    /// TS's constant-rate grid, default) or `slot` (the true slot times).
    #[arg(long, value_enum, default_value_t = MuxPcrGrid::Uniform, help_heading = "ISDB-S (--system isdbs)")]
    pub mux_pcr_grid: MuxPcrGrid,
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

/// Run `isdb mux`; `given` are the arguments given on the command line
/// (the options of the other system are refused).
pub fn run(args: MuxArgs, given: &Given) -> Result<()> {
    if args.print_config_example {
        print!("{}", isdbt_mux::config::EXAMPLE_TOML);
        return Ok(());
    }
    let wrong = match args.system {
        TxSystem::Isdbt => MUX_ISDBS_ONLY,
        TxSystem::Isdbs => MUX_ISDBT_ONLY,
    };
    let bad: Vec<String> = wrong
        .iter()
        .filter(|id| given.has(id))
        .map(|id| crate::txs::flag(id))
        .collect();
    if !bad.is_empty() {
        match args.system {
            TxSystem::Isdbs => bail!(
                "{}: ISDB-T option(s), not used with --system isdbs (the TS takes every sent slot of --combination and goes to --out-ts)",
                bad.join(", ")
            ),
            TxSystem::Isdbt => bail!(
                "{}: ISDB-S option(s), used with --system isdbs only (the default system is ISDB-T)",
                bad.join(", ")
            ),
        }
    }
    if args.system == TxSystem::Isdbs {
        return run_isdbs(&args);
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
    summarize(
        &mux,
        args.report
            .then(|| (&full_all[..], &times[..], clocks_to_ticks(408))),
        "multiplex-frame TS",
    );
    Ok(())
}

/// The statistics every run ends with (PES, T-STD, relays, errors) and,
/// with `report` = (packets, their arrival times, ticks per packet), the
/// `--report` measurements of the `what` output.
fn summarize(mux: &Multiplexer, report: Option<(&[Packet], &[u64], u64)>, what: &str) {
    let st = mux.stats();
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

    if let Some((full_all, times, packet_ticks)) = report {
        let r = analyze(full_all, times, packet_ticks);
        info!(
            packets = full_all.len(),
            "--- report ({what}, {} packets)",
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
}

/// `isdb mux` options that only ISDB-T uses (argument ids).
pub const MUX_ISDBT_ONLY: &[&str] = &[
    "mode",
    "gi",
    "partial",
    "layer_a",
    "layer_b",
    "layer_c",
    "out_a",
    "out_b",
    "out_c",
    "relay",
    "auto_layout",
    "channel",
];

/// `isdb mux` options that only ISDB-S uses.
pub const MUX_ISDBS_ONLY: &[&str] = &["combination", "mux_pcr_grid"];

/// `isdb mux --system isdbs`: the configuration's TS (`[network] kind =
/// "bs"`, TS_ID from `[transport]`) in every sent slot of the
/// `--combination` layout, `--frames` superframes (or `--seconds`), timed
/// for them on the `--mux-pcr-grid` as `isdb tx --system isdbs
/// --mux-config` sends it: the TS a receiver outputs for that TS_ID,
/// nulls included, to `--out-ts`.
fn run_isdbs(args: &MuxArgs) -> Result<()> {
    let path = args.config.as_ref().context("--config is required")?;
    if args.out_ts.is_none() && !args.report {
        bail!("nothing to do: give --out-ts and/or --report");
    }
    let sat = SatArgs {
        combination: args.combination.clone(),
        ts: Vec::new(),
        rate: None,
        lead_superframes: None,
        ts_pacing: TsPacing::Auto,
        mux_pcr_grid: Some(args.mux_pcr_grid),
    };
    let mut layout = Layout::of_inputs(&sat, std::slice::from_ref(path))?;
    if let (Some(p), Some(m)) = (args.adts_compliance, layout.mux[0].as_mut()) {
        for st in m
            .config
            .services
            .iter_mut()
            .flat_map(|s| s.streams.iter_mut())
        {
            if st.kind == StreamKind::AacAdts {
                st.adts_compliance = p.into();
            }
        }
    }
    info!("ISDB-S (ITU-R BO.1408-1): {}", layout.describe());
    let mut mux = open_muxes(&layout)?
        .pop()
        .flatten()
        .map(MuxSource::into_mux)
        .context("the multiplexer of --config")?;
    let sf_s = superframe_seconds();
    let frames = match (args.frames, args.seconds) {
        (Some(f), _) => f,
        (None, s) => superframes_for_seconds(s.unwrap_or(10.0)),
    };
    info!(
        superframes = frames,
        seconds = frames as f64 * sf_s,
        clock_start = %isdbt_mux::jst::format(mux.clock_start()),
        "{frames} superframes ({:.2} s), clock start {} JST",
        frames as f64 * sf_s,
        isdbt_mux::jst::format(mux.clock_start())
    );
    let mut writer = TsWriter::new([None, None, None], args.out_ts.as_deref())?;
    let none: [Vec<Packet>; 3] = Default::default();
    let (mut full_all, mut times) = (Vec::new(), Vec::new());
    for k in 0..frames {
        if mux.relays_finished() {
            info!(
                superframes = k,
                "every ts-program source ended after {k} superframes"
            );
            break;
        }
        let p = mux.next_period();
        for w in mux.take_alerts() {
            warn!("{w}");
        }
        writer.write_frame(&p, &none)?;
        if args.report {
            times.extend((0..p.len()).map(|q| mux.slot_time(k, q)));
            full_all.extend(p);
        }
    }
    writer.flush()?;
    let st = mux.stats();
    let ts_id = layout.ts[0].ts_id;
    info!(
        ts_id,
        packets = st.layer_packets[0],
        null_packets = st.layer_nulls[0],
        "TS_ID {ts_id:#06x}: {} packets per superframe, {} packets, {} null ({:.1} %)",
        mux.schedule().lane_slots(0),
        st.layer_packets[0],
        st.layer_nulls[0],
        100.0 * st.layer_nulls[0] as f64 / st.layer_packets[0].max(1) as f64
    );
    let packet_ticks = mux.schedule().period_ticks() / mux.schedule().lane_slots(0) as u64;
    summarize(
        &mux,
        args.report
            .then(|| (&full_all[..], &times[..], packet_ticks)),
        "superframe-timed TS",
    );
    Ok(())
}
