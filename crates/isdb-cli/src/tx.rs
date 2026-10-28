//! `isdb tx`: transport streams → ISDB-T baseband → IQ file or radio.

use crate::input::{FrameInput, InputArgs};
use crate::keys::{Command, Keys};
use crate::phyargs::{PhyArgs, TuneArgs, describe_params, parse_gain};
use crate::producer::{self, EndReason, FrameKind, ProducedFrame, ProducerConfig};
use crate::sink::{FileSink, IqFormat};
use crate::source::Feed;
use anyhow::{Context, Result, bail};
use clap::Args;
use isdb_radio::{DeviceSpec, IqSink, TxOptions};
use isdbt_core::Params;
use isdbt_phy::Transmitter;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use tracing::{info, warn};

/// Legal and safety notice shown by `isdb tx --help` (and in README.md).
pub const LEGAL_NOTICE: &str = "\
LEGAL / SAFETY NOTICE
  Radiating on broadcast (UHF TV) frequencies requires a licence: in Japan
  under the Radio Act (電波法), elsewhere under local law. `isdb tx --radio`
  is intended for shielded or conducted lab setups only (coax with
  attenuators into the receiver, or a shielded enclosure), and radiates only
  with --rf-enable. isdb does not descramble: it has no B-CAS / conditional
  access support and refuses scrambled input (transport_scrambling_control
  != '00'). You are responsible for having the rights to any content you
  transmit or relay.";

/// `isdb tx` arguments.
#[derive(Debug, Args)]
#[command(group(clap::ArgGroup::new("sink").args(["iq", "radio"])))]
#[command(after_help = LEGAL_NOTICE)]
pub struct TxArgs {
    #[command(flatten)]
    pub phy: PhyArgs,
    #[command(flatten)]
    pub input: InputArgs,
    /// Replay a baseband file instead of modulating TS input: cf32 or cs8
    /// at 512/63 MHz centred on the signal (as `isdb tx --iq` or
    /// `hackrf_transfer -r` write it). Its parameters and frame start are
    /// found by the receiver (the PHY options are not used); it is scaled
    /// to unit RMS. Ends at its end (`--feed loop` repeats it).
    #[arg(long, value_name = "PATH", conflicts_with_all = ["a_ts", "b_ts", "c_ts", "mux_ts", "synthetic", "mux_config", "relay"])]
    pub iq_in: Option<std::path::PathBuf>,
    /// Format of --iq-in.
    #[arg(long, value_enum, default_value_t = IqFormat::Cf32, requires = "iq_in")]
    pub iq_in_format: IqFormat,
    /// Playback keys (--relay and --iq-in, when stdin is a terminal):
    /// ←/→ seek by this many seconds.
    #[arg(long, value_name = "S", default_value_t = 10.0)]
    pub seek_step: f64,
    /// Do not read playback keys from the terminal (space pause/resume,
    /// ←/→ seek, ↑/↓ play rate).
    #[arg(long)]
    pub no_keys: bool,
    /// Write baseband to this file ("-" = stdout) instead of a radio.
    #[arg(long, value_name = "PATH", required_unless_present_any = ["radio", "print_config"])]
    pub iq: Option<String>,
    /// IQ file format (cs8 uses --backoff-db scaling like an 8-bit radio).
    #[arg(long, value_enum, default_value_t = IqFormat::Cf32)]
    pub iq_format: IqFormat,
    /// Transmit with a radio: BACKEND[:key=value,...], the backend name and
    /// its device arguments (needs --rf-enable or --dry-run, and
    /// --channel/--freq).  An unknown backend lists the compiled-in ones.
    #[arg(long, value_name = "SPEC", requires = "rf_enable_or_dry")]
    pub radio: Option<DeviceSpec>,
    #[command(flatten)]
    pub tune: TuneArgs,
    /// Radio gain NAME=VALUE (repeatable).  Names, ranges and defaults are
    /// the backend's: an unknown name lists them, --dry-run shows the
    /// values used.
    #[arg(
        long,
        value_name = "NAME=VALUE",
        value_parser = parse_gain,
        requires = "radio",
        conflicts_with = "iq"
    )]
    pub gain: Vec<(String, String)>,
    /// OFDM RMS level below integer full scale in dB (radios with integer
    /// samples and cs8 files); clipped components are counted.
    #[arg(long, default_value_t = 10.0)]
    pub backoff_db: f32,
    /// Actually radiate (licensed or shielded/conducted lab use only; see
    /// the notice below).  `--radio` requires this or `--dry-run`.
    #[arg(long, group = "rf_enable_or_dry")]
    pub rf_enable: bool,
    /// With --radio: validate and print the radio settings, then exit
    /// without opening the device (--radio needs this or --rf-enable).
    /// With --iq: exit before writing anything.
    #[arg(long, group = "rf_enable_or_dry")]
    pub dry_run: bool,
    /// Duration bound in seconds of air time, lead-in and flush included.
    /// Default 0 = unbounded: until the input ends (--feed file at EOF,
    /// --feed live when the writer closes, --relay --at-eof end) or Ctrl-C /
    /// SIGTERM; synthetic, --mux-config, --relay and --feed loop inputs
    /// never end.  An --iq regular file with such an endless input needs an
    /// explicit --seconds.
    #[arg(long, value_name = "S")]
    pub seconds: Option<f64>,
    /// Null frames before the data (default: Transmitter::lead_frames = 2).
    #[arg(long)]
    pub lead_frames: Option<usize>,
    /// Do not append the null frames that flush the interleavers at the end.
    #[arg(long)]
    pub no_flush: bool,
    /// Pace frame production to real time even into a file (default: on
    /// for --feed live with --iq).
    #[arg(long)]
    pub realtime: bool,
    /// Frames produced ahead of the sink (bounded queue).
    #[arg(long, default_value_t = 4)]
    pub queue_frames: usize,
    /// Frames converted before the radio starts streaming.
    #[arg(long, default_value_t = 2)]
    pub prefill_frames: usize,
    /// Status line interval in seconds (0 = only the final summary).
    #[arg(long, default_value_t = 5.0)]
    pub status_secs: f64,
}

/// Streaming options independent of the sink.
#[derive(Debug, Clone)]
pub struct TxOpts {
    /// Null lead-in frames.
    pub lead_frames: usize,
    /// Flush at the end.
    pub flush: bool,
    /// Total frame bound.
    pub max_frames: Option<u64>,
    /// Pace to real time.
    pub pace: bool,
    /// Producer queue depth.
    pub queue_frames: usize,
    /// Status interval (s), 0 = none.
    pub status_secs: f64,
}

/// Outcome of a transmission.
#[derive(Debug, Clone, Default)]
pub struct TxReport {
    /// Frames handed to the sink.
    pub frames: u64,
    /// Of which lead-in / data / flush.
    pub lead: u64,
    /// Data frames.
    pub data: u64,
    /// Flush frames.
    pub flush: u64,
    /// Input packets per layer.
    pub real: [u64; 3],
    /// Null padding per layer in data frames.
    pub nulls: [u64; 3],
    /// Why the producer ended (None if the sink failed first).
    pub end: Option<EndReason>,
    /// Sink summary.
    pub sink: String,
}

fn ratio(n: u64, d: u64) -> f64 {
    if d == 0 {
        0.0
    } else {
        100.0 * n as f64 / d as f64
    }
}

fn null_fill(real: &[u64; 3], nulls: &[u64; 3], params: &Params) -> String {
    if real.iter().chain(nulls).all(|&n| n == 0) {
        return "-".into(); // no packets (IQ playback, lead-in only)
    }
    params
        .active_layers()
        .map(|(id, _)| {
            let i = id.index();
            format!("{id} {:5.1}%", ratio(nulls[i], real[i] + nulls[i]))
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn ms_per(d: Duration, frames: u64) -> f64 {
    if frames == 0 {
        0.0
    } else {
        d.as_secs_f64() * 1e3 / frames as f64
    }
}

/// Producer speed in multiples of real time: on-air time of the frames
/// produced divided by the time spent producing them (input + modulation;
/// waiting on a full queue is not counted).
pub fn producer_speed(frames: u64, busy: Duration, frame_s: f64) -> Option<f64> {
    (frames > 0 && !busy.is_zero()).then(|| frames as f64 * frame_s / busy.as_secs_f64())
}

fn describe_speed(speed: Option<f64>, frames: u64, input: Duration, modulate: Duration) -> String {
    match speed {
        Some(x) => format!(
            "producer {x:.1}× real time (input {:.1} + modulate {:.1} ms/frame)",
            ms_per(input, frames),
            ms_per(modulate, frames)
        ),
        None => "producer -".into(),
    }
}

/// Run a transmission from `input` into `sink`.
#[cfg(test)]
pub fn stream(
    params: &Params,
    input: Box<dyn FrameInput>,
    sink: &mut dyn IqSink,
    opts: &TxOpts,
    stop: Arc<AtomicBool>,
) -> Result<TxReport> {
    stream_controlled(params, input, sink, opts, stop, None)
}

/// Run a transmission from `input` into `sink`, with playback commands
/// applied to the input.
fn stream_controlled(
    params: &Params,
    input: Box<dyn FrameInput>,
    sink: &mut dyn IqSink,
    opts: &TxOpts,
    stop: Arc<AtomicBool>,
    controls: Option<std::sync::mpsc::Receiver<Command>>,
) -> Result<TxReport> {
    let cfg = ProducerConfig {
        params: params.clone(),
        lead_frames: opts.lead_frames,
        flush: opts.flush,
        max_frames: opts.max_frames,
        pace: opts.pace,
        record: false,
        depth: opts.queue_frames,
    };
    let (frames, worker) = producer::spawn(cfg, input, stop.clone(), controls);
    drive(params, frames, worker, sink, opts, &stop)
}

/// Hand the produced frames to `sink` with status lines until the producer
/// ends, the sink fails or a stop is requested.
fn drive(
    params: &Params,
    frames: std::sync::mpsc::Receiver<ProducedFrame>,
    worker: std::thread::JoinHandle<Result<EndReason>>,
    sink: &mut dyn IqSink,
    opts: &TxOpts,
    stop: &AtomicBool,
) -> Result<TxReport> {
    let frame_s = params.frame_seconds();
    let mut rep = TxReport::default();
    let (mut iv_real, mut iv_nulls) = ([0u64; 3], [0u64; 3]);
    // Producer work per status interval: frames, input and modulation time.
    let (mut iv_frames, mut iv_input, mut iv_mod) = (0u64, Duration::ZERO, Duration::ZERO);
    let realtime_sink = sink.realtime();
    let t0 = Instant::now();
    let mut next_status = t0 + Duration::from_secs_f64(opts.status_secs.max(0.0));
    let mut aborted = false;
    let mut err = None;
    let mut input_status = String::new();
    for f in frames.iter() {
        if stop.load(Ordering::Relaxed) {
            aborted = true;
            break;
        }
        if let Err(e) = sink.write(&f.samples, stop) {
            aborted = true;
            err = Some(e);
            break;
        }
        rep.frames += 1;
        iv_frames += 1;
        iv_input += f.input_time;
        iv_mod += f.modulate_time;
        if !f.input_status.is_empty() {
            input_status = f.input_status;
        }
        match f.kind {
            FrameKind::Lead => rep.lead += 1,
            FrameKind::Flush => rep.flush += 1,
            FrameKind::Data => {
                rep.data += 1;
                for l in 0..3 {
                    rep.real[l] += f.real[l] as u64;
                    rep.nulls[l] += f.nulls[l] as u64;
                    iv_real[l] += f.real[l] as u64;
                    iv_nulls[l] += f.nulls[l] as u64;
                }
            }
        }
        if opts.status_secs > 0.0 && Instant::now() >= next_status {
            next_status += Duration::from_secs_f64(opts.status_secs);
            let speed = producer_speed(iv_frames, iv_input + iv_mod, frame_s);
            info!(
                on_air_s = rep.frames as f64 * frame_s,
                frames = rep.frames,
                data_frames = rep.data,
                speed = speed.unwrap_or(f64::NAN),
                "{:7.1} s on air, {} frames ({} data) | null fill {} (total {}){input_status} | {}{}",
                rep.frames as f64 * frame_s,
                rep.frames,
                rep.data,
                null_fill(&iv_real, &iv_nulls, params),
                null_fill(&rep.real, &rep.nulls, params),
                describe_speed(speed, iv_frames, iv_input, iv_mod),
                sink.status()
            );
            if realtime_sink && speed.is_some_and(|x| x < 1.0) {
                warn!(
                    speed = speed.unwrap_or(0.0),
                    input_ms = ms_per(iv_input, iv_frames),
                    modulation_ms = ms_per(iv_mod, iv_frames),
                    frame_ms = frame_s * 1e3,
                    "the producer runs below real time ({:.2}×): the radio underruns. \
                     Per frame: input {:.1} ms + modulation {:.1} ms for a {:.1} ms frame{}",
                    speed.unwrap_or(0.0),
                    ms_per(iv_input, iv_frames),
                    ms_per(iv_mod, iv_frames),
                    frame_s * 1e3,
                    if cfg!(debug_assertions) {
                        " — this is a debug build; use `cargo build --release -p isdb-cli`"
                    } else {
                        ""
                    }
                );
            }
            iv_real = [0; 3];
            iv_nulls = [0; 3];
            (iv_frames, iv_input, iv_mod) = (0, Duration::ZERO, Duration::ZERO);
        }
    }
    if stop.load(Ordering::Relaxed) {
        aborted = true;
    }
    drop(frames); // unblocks the producer if it is waiting on the queue
    let end = worker
        .join()
        .map_err(|_| anyhow::anyhow!("producer panicked"))?;
    let fin = sink.finish(aborted);
    if let Some(e) = err {
        if let Ok(s) = &fin {
            info!("{s}");
        }
        return Err(e.into());
    }
    rep.end = Some(end?);
    rep.sink = fin?;
    Ok(rep)
}

/// The `--seconds` bound of `tx` and `rx` (0 = none, the default).
pub(crate) fn resolve_seconds(seconds: Option<f64>) -> Result<f64> {
    match seconds {
        Some(s) if !(s >= 0.0 && s.is_finite()) => bail!("--seconds must be a finite value ≥ 0"),
        Some(s) => Ok(s),
        None => Ok(0.0),
    }
}

/// True if writing to `path` cannot fill a disk: stdout ("-") or an
/// existing non-regular file (FIFO, pipe, character device such as
/// /dev/null).
fn is_stream_path(path: &str) -> bool {
    path == "-" || std::fs::metadata(path).is_ok_and(|m| !m.is_file())
}

/// Refuse an endless input into an `--iq` regular file without an explicit
/// `--seconds` (it would grow until the disk is full).
fn check_iq_bound(path: &str, endless: bool, seconds: Option<f64>) -> Result<()> {
    if endless && seconds.is_none() && !is_stream_path(path) {
        bail!(
            "--iq {path}: this input never ends and --seconds defaults to unbounded, so the file would grow until the disk is full; give --seconds S (or --seconds 0 to really write without a bound)"
        );
    }
    Ok(())
}

/// Entry point.
pub fn run(a: TxArgs) -> Result<()> {
    if !(a.seek_step > 0.0 && a.seek_step.is_finite()) {
        bail!("--seek-step must be a positive number of seconds");
    }
    // --iq-in: the parameters are the file's
    let iq = match &a.iq_in {
        Some(path) => Some(crate::iqplay::IqFile::detect(path, a.iq_in_format)?),
        None => None,
    };
    let (params, prepared) = match &iq {
        Some(f) => (f.params.clone(), None),
        None => match crate::relay::prepare(&a.input, a.phy.params()?, || {
            crate::relay::tx_channel(a.tune.channel, a.tune.freq)
        })? {
            crate::relay::Prepared::Exit => return Ok(()),
            crate::relay::Prepared::Ready(p, i) => (p, i),
        },
    };
    let tx = Transmitter::new(params.clone());
    let frame_s = params.frame_seconds();
    let seconds = resolve_seconds(a.seconds)?;
    // Inputs opened here (synthetic, --feed …) vs. built by relay::prepare.
    let endless = match &prepared {
        Some(i) => i.endless(),
        None => a.input.synthetic || a.input.feed == Feed::Loop,
    };
    if let Some(f) = &iq {
        info!(
            frames = f.frames,
            seconds = f.seconds(),
            first_sample = f.first_sample,
            scale = f.scale,
            "--iq-in {}: {} whole frames ({:.1} s) from sample {}, level ×{:.3} to unit RMS (parameters from its TMCC)",
            a.iq_in
                .as_deref()
                .unwrap_or(std::path::Path::new(""))
                .display(),
            f.frames,
            f.seconds(),
            f.first_sample,
            f.scale
        );
    }
    info!("{}", describe_params(&params));
    info!(
        frame_s = params.frame_seconds(),
        samples_per_frame = params.samples_per_frame(),
        "per frame: {} | frame {:.4} s, {} samples at 512/63 MHz",
        params
            .active_layers()
            .map(|(id, _)| format!(
                "{id} {} TSP ({:.2} Mbit/s)",
                tx.tsps_per_frame(id),
                isdbt_mux::capacity::layer_bps(&params, id) / 1e6
            ))
            .collect::<Vec<_>>()
            .join(", "),
        frame_s,
        params.samples_per_frame()
    );
    let stop = crate::stop::flag();
    let mut sink: Box<dyn IqSink> = if let Some(spec) = &a.radio {
        let backend = isdb_radio::backend(&spec.backend)?;
        let (cfg, tuning) = a.tune.radio_config(spec, &a.gain)?;
        let opts = TxOptions {
            prefill_frames: a.prefill_frames,
            frame_seconds: frame_s,
            backoff_db: a.backoff_db,
        };
        let settings = backend.describe_tx(&cfg, &opts)?;
        info!(
            backend = backend.name(),
            "{} TX settings{}:\n{settings}",
            backend.name(),
            tuning.channel_label()
        );
        // clap admits --radio only with exactly one of --rf-enable and
        // --dry-run; the gate does not rely on it.
        if !a.rf_enable || a.dry_run {
            bail!(
                "dry run, not transmitting: use --rf-enable instead of --dry-run to radiate with the settings above (coax/attenuator or shielded setup only)"
            );
        }
        if seconds == 0.0 {
            info!(
                "note: unbounded transmission ({}); stop with Ctrl-C",
                if endless {
                    "the input never ends"
                } else {
                    "until the input ends"
                }
            );
        }
        let (sink, info) = backend
            .open_tx(&cfg, &opts)
            .with_context(|| format!("open radio {spec} for TX"))?;
        info!("opened: {info}");
        sink
    } else {
        let path = a.iq.as_deref().expect("clap group");
        if a.dry_run {
            bail!("dry run: nothing written");
        }
        check_iq_bound(path, endless, a.seconds)?;
        Box::new(FileSink::create(path, a.iq_format, a.backoff_db)?)
    };
    let opts = TxOpts {
        // a replayed file carries its own lead-in and flush
        lead_frames: if iq.is_some() {
            0
        } else {
            a.lead_frames.unwrap_or(tx.lead_frames())
        },
        flush: !a.no_flush && iq.is_none(),
        max_frames: (seconds > 0.0).then(|| producer::frames_for_seconds(&params, seconds)),
        pace: a.realtime || producer::default_pace(a.input.feed, sink.realtime()),
        queue_frames: a.queue_frames,
        status_secs: a.status_secs,
    };
    info!(
        lead_frames = opts.lead_frames,
        paced = opts.pace,
        "duration: {} | lead-in {} frames, flush {} frames{}",
        opts.max_frames.map_or_else(
            || if endless {
                "unbounded (the input never ends); stop with Ctrl-C".into()
            } else {
                "unbounded (until the input ends); stop with Ctrl-C".into()
            },
            |f| format!("{seconds} s ({f} frames)")
        ),
        opts.lead_frames,
        if opts.flush { tx.flush_frames() } else { 0 },
        if opts.pace {
            ", paced to real time"
        } else {
            ""
        }
    );
    let keys_hint = |k: &Option<Keys>| {
        if k.is_some() {
            info!("keys: {}", crate::keys::help(a.seek_step));
        }
    };
    let iq_play = iq.is_some();
    let (rep, keys) = if let Some(f) = iq {
        let mut keys = (!a.no_keys).then(|| Keys::start(a.seek_step)).flatten();
        keys_hint(&keys);
        let cfg = crate::iqplay::PlayConfig {
            max_frames: opts.max_frames,
            looped: a.input.feed == Feed::Loop,
            pace: opts.pace,
            depth: opts.queue_frames,
        };
        let (frames, worker) =
            crate::iqplay::spawn(f, cfg, stop.clone(), keys.as_mut().and_then(Keys::take_rx));
        (
            drive(&params, frames, worker, sink.as_mut(), &opts, &stop)?,
            keys,
        )
    } else {
        let mut input = match prepared {
            Some(i) => i,
            None => a.input.open(&params)?,
        };
        info!("input: {}", input.describe());
        let mut keys = if !a.no_keys && input.enable_control()? {
            Keys::start(a.seek_step)
        } else {
            None
        };
        keys_hint(&keys);
        let controls = keys.as_mut().and_then(Keys::take_rx);
        (
            stream_controlled(&params, input, sink.as_mut(), &opts, stop, controls)?,
            keys,
        )
    };
    drop(keys); // the terminal back to its mode
    crate::source::check_not_scrambled()?;
    info!(
        end = ?rep.end.unwrap_or(EndReason::Stopped),
        frames = rep.frames,
        on_air_s = rep.frames as f64 * frame_s,
        lead_frames = rep.lead,
        data_frames = rep.data,
        flush_frames = rep.flush,
        "done ({:?}): {} frames = {:.2} s on air ({} lead, {} data, {} flush)",
        rep.end.unwrap_or(EndReason::Stopped),
        rep.frames,
        rep.frames as f64 * frame_s,
        rep.lead,
        rep.data,
        rep.flush
    );
    for (id, _) in params.active_layers().filter(|_| !iq_play) {
        let i = id.index();
        info!(
            layer = %id,
            input_packets = rep.real[i],
            null_packets = rep.nulls[i],
            null_fill_pct = ratio(rep.nulls[i], rep.real[i] + rep.nulls[i]),
            "  layer {id}: {} input packets, {} null padding ({:.1}% null fill)",
            rep.real[i],
            rep.nulls[i],
            ratio(rep.nulls[i], rep.real[i] + rep.nulls[i])
        );
    }
    info!("{}", rep.sink);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::SyntheticInput;
    use clap::Parser;
    use isdb_radio::mock::{MemorySink, MockRadio};
    use isdb_radio::{Cs8, Quantizer, QueuedSink, RadioTx};

    #[derive(Parser)]
    struct T {
        #[command(flatten)]
        a: TxArgs,
    }

    fn parse(args: &[&str]) -> Result<TxArgs, clap::Error> {
        let mut v = vec!["tx"];
        v.extend_from_slice(args);
        T::try_parse_from(v).map(|t| t.a)
    }

    #[test]
    fn tx_args_defaults_and_sinks() {
        let a = parse(&["--iq", "x.cf32", "--synthetic"]).unwrap();
        assert_eq!((a.phy.mode, a.phy.gi, a.phy.partial), (3, 8, true));
        assert_eq!(a.phy.layer_a, "1,qpsk,2/3,2");
        assert_eq!(a.phy.layer_b, "12,64qam,3/4,2");
        assert_eq!(
            (a.radio, a.gain.len(), a.backoff_db, a.rf_enable),
            (None, 0, 10.0, false)
        );
        assert_eq!(a.input.feed, Feed::File);
        // A sink is required, and only one.
        assert!(parse(&["--synthetic"]).is_err());
        assert!(parse(&["--iq", "x", "--radio", "sdr", "--rf-enable", "--synthetic"]).is_err());
        // --radio needs --rf-enable (or an explicit --dry-run).
        assert!(parse(&["--radio", "sdr", "--channel", "13", "--synthetic"]).is_err());
        // The device spec and the gains reach the backend unparsed.
        let h = parse(&[
            "--radio",
            "sdr:serial=0000abcd",
            "--channel",
            "13",
            "--rf-enable",
            "--gain",
            "if=20",
            "--gain",
            "amp=on",
            "--b-ts",
            "-",
        ])
        .unwrap();
        let spec = h.radio.as_ref().unwrap();
        assert_eq!(
            (spec.backend.as_str(), spec.get("serial")),
            ("sdr", Some("0000abcd"))
        );
        assert_eq!(h.tune.channel, Some(crate::bands::Channel::T(13)));
        assert_eq!(
            h.gain,
            vec![("if".into(), "20".into()), ("amp".into(), "on".into())]
        );
        assert!(
            parse(&[
                "--radio",
                "sdr",
                "--channel",
                "13",
                "--dry-run",
                "--synthetic"
            ])
            .is_ok()
        );
        // Malformed specs and gains are command-line errors; gains need a radio.
        assert!(parse(&["--radio", "sdr:serial", "--dry-run", "--synthetic"]).is_err());
        assert!(parse(&["--radio", "", "--dry-run", "--synthetic"]).is_err());
        assert!(parse(&["--radio", "sdr", "--dry-run", "--gain", "20", "--synthetic"]).is_err());
        assert!(parse(&["--iq", "x", "--gain", "if=20", "--synthetic"]).is_err());
        // --mux-ts excludes per-layer inputs; PID lists require it.
        assert!(parse(&["--iq", "x", "--mux-ts", "m.ts", "--b-ts", "b.ts"]).is_err());
        assert!(parse(&["--iq", "x", "--a-pids", "0x1fc8", "--b-ts", "b.ts"]).is_err());
        let m = parse(&[
            "--iq",
            "x",
            "--mux-ts",
            "-",
            "--a-pids",
            "0x0,0x1fc8",
            "--feed",
            "live",
        ])
        .unwrap();
        assert_eq!(m.input.feed, Feed::Live);
        assert!(
            parse(&[
                "--iq",
                "x",
                "--channel",
                "13",
                "--freq",
                "473142857",
                "--synthetic"
            ])
            .is_err()
        );
        assert!(parse(&["--iq", "x", "--freq-corr-ppm", "-1.5", "--synthetic"]).is_ok());
    }

    #[test]
    fn seconds_defaults_to_unbounded_for_every_input() {
        for args in [
            &["--iq", "x", "--synthetic"][..],
            &["--iq", "x", "--b-ts", "b.ts"],
            &["--iq", "x", "--b-ts", "b.ts", "--feed", "loop"],
            &["--iq", "x", "--b-ts", "-", "--feed", "live"],
            &["--iq", "x", "--mux-config", "m.toml"],
            &["--iq", "x", "--relay", "r.ts"],
            &[
                "--radio",
                "sdr",
                "--channel",
                "13",
                "--rf-enable",
                "--synthetic",
            ],
            &[
                "--radio",
                "sdr",
                "--channel",
                "13",
                "--dry-run",
                "--b-ts",
                "b.ts",
            ],
        ] {
            let a = parse(args).unwrap();
            assert_eq!(a.seconds, None, "{args:?}");
            assert_eq!(resolve_seconds(a.seconds).unwrap(), 0.0, "{args:?}");
        }
        let a = parse(&["--iq", "x", "--synthetic", "--seconds", "12.5"]).unwrap();
        assert_eq!(resolve_seconds(a.seconds).unwrap(), 12.5);
        assert!(resolve_seconds(Some(-1.0)).is_err());
        assert!(resolve_seconds(Some(f64::NAN)).is_err());
        assert!(resolve_seconds(Some(f64::INFINITY)).is_err());
    }

    #[test]
    fn endless_input_into_iq_file_needs_seconds() {
        let d = std::env::temp_dir().join(format!("isdb-tx-iq-bound-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let new = d.join("new.cf32");
        let old = d.join("old.cf32");
        std::fs::write(&old, b"").unwrap();
        for p in [&new, &old] {
            let p = p.to_str().unwrap();
            let e = check_iq_bound(p, true, None).unwrap_err().to_string();
            assert!(e.contains("--seconds") && e.contains("never ends"), "{e}");
            // A bound (or an explicit --seconds 0) or a finite input is fine.
            check_iq_bound(p, true, Some(10.0)).unwrap();
            check_iq_bound(p, true, Some(0.0)).unwrap();
            check_iq_bound(p, false, None).unwrap();
        }
        assert!(!new.exists(), "the check must not create the file");
        // stdout, a FIFO and a character device cannot fill a disk.
        check_iq_bound("-", true, None).unwrap();
        check_iq_bound("/dev/null", true, None).unwrap();
        let fifo = d.join("fifo");
        let st = std::process::Command::new("mkfifo").arg(&fifo).status();
        if st.is_ok_and(|s| s.success()) {
            check_iq_bound(fifo.to_str().unwrap(), true, None).unwrap();
        }
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn synthetic_input_is_endless() {
        assert!(SyntheticInput::new().endless());
    }

    fn mode1() -> Params {
        PhyArgs {
            mode: 1,
            gi: 8,
            partial: true,
            layer_a: "1,qpsk,2/3,2".into(),
            layer_b: "12,16qam,1/2,1".into(),
            layer_c: "none".into(),
        }
        .params()
        .unwrap()
    }

    /// The queued radio sink of the backends over an in-memory radio
    /// consuming `rate` samples/s.
    fn radio_sink(p: &Params, rate: f64) -> QueuedSink<MockRadio<Cs8>> {
        QueuedSink::new(
            MockRadio::new(rate),
            &TxOptions {
                prefill_frames: 2,
                frame_seconds: p.frame_seconds(),
                backoff_db: 10.0,
            },
        )
        .unwrap()
    }

    #[test]
    fn stream_to_memory_and_mock_radio_agree() {
        let p = mode1();
        let opts = TxOpts {
            lead_frames: 1,
            flush: true,
            max_frames: Some(8),
            pace: false,
            queue_frames: 2,
            status_secs: 0.0,
        };
        let mut mem = MemorySink::default();
        let rep = stream(
            &p,
            Box::new(SyntheticInput::new()),
            &mut mem,
            &opts,
            Arc::new(AtomicBool::new(false)),
        )
        .unwrap();
        let flush = Transmitter::new(p.clone()).flush_frames() as u64;
        assert_eq!(rep.end, Some(EndReason::Limit));
        assert_eq!(
            (rep.frames, rep.lead, rep.flush, rep.data),
            (8, 1, flush, 8 - 1 - flush)
        );
        assert_eq!(rep.nulls, [0; 3]);
        let samples = mem.samples.lock().unwrap().clone();
        assert_eq!(samples.len(), 8 * p.samples_per_frame());

        // The same transmission through the radio sink (prefill, queue,
        // drain): exactly the cs8 quantisation of those samples.
        let mut radio = radio_sink(&p, 64e6);
        let rep2 = stream(
            &p,
            Box::new(SyntheticInput::new()),
            &mut radio,
            &opts,
            Arc::new(AtomicBool::new(false)),
        )
        .unwrap();
        assert!(rep2.sink.contains("queue drained"), "{}", rep2.sink);
        assert!(
            rep2.sink.contains("device stopped streaming: yes"),
            "{}",
            rep2.sink
        );
        assert_eq!(radio.radio().starts, 1);
        let sent = radio.radio().sent.lock().unwrap().clone();
        assert_eq!(sent, Quantizer::<Cs8>::new(10.0).convert(&samples));
    }

    #[test]
    fn stop_flag_aborts_radio_stream() {
        let p = mode1();
        let opts = TxOpts {
            lead_frames: 0,
            flush: true,
            max_frames: None,
            pace: false,
            queue_frames: 2,
            status_secs: 0.0,
        };
        let stop = Arc::new(AtomicBool::new(false));
        let s2 = stop.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(300));
            s2.store(true, Ordering::SeqCst);
        });
        // Real-time mock radio: the producer is throttled by back-pressure.
        let mut radio = radio_sink(&p, isdbt_core::sample_rate_hz());
        let rep = stream(&p, Box::new(SyntheticInput::new()), &mut radio, &opts, stop).unwrap();
        assert_eq!(rep.end, Some(EndReason::Stopped));
        assert!(rep.sink.contains("stopped immediately"), "{}", rep.sink);
        assert!(!radio.radio_mut().is_streaming());
        // Bounded by back-pressure: ≈ 0.3 s + queue, far below unbounded.
        assert!(rep.frames < 40, "{} frames", rep.frames);
    }
}
