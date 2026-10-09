//! `--relay FILE`: rebroadcast a recorded program without a TOML file, and
//! `--auto-layout`: let a `ts-program` relay pick the layer layout its
//! measured rate needs.
//!
//! The configuration is `isdbt_mux::relay_template::RelayOptions` (the
//! values of `isdbt-mux/examples/relay-ts.toml`) filled from the options;
//! `--print-config` prints it as TOML. All spec logic (program selection,
//! stream roles, PID plan, capacities) is in `isdbt-mux`.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use clap::Args;
use isdbt_core::{Layer, Params};
use isdbt_mux::config::{AtEof, CaMode, LayerName, StreamCfg};
use isdbt_mux::relay_template::{DEFAULT_NAME, RelayOptions};
use isdbt_mux::{Config, Multiplexer, MuxError};

use crate::input::{FrameInput, InputArgs, MuxConfigInput};
use crate::phyargs::{channel_hz, describe_params, phy_options};

/// `--relay` and its options (shared by `tx`, `loopback` and `mux`).
#[derive(Debug, Clone, Default, Args)]
pub struct RelayArgs {
    /// Rebroadcast one program of a recorded TS (unscrambled; one you have
    /// the rights to) as a terrestrial service, without a TOML file: the configuration of
    /// isdbt-mux/examples/relay-ts.toml is built in memory (service 0x0400,
    /// PMT 0x01F0, PCR 0x01FF, NIT for --channel), the program and the
    /// stream roles / PIDs are read from the file.
    #[arg(long, value_name = "TS")]
    pub relay: Option<PathBuf>,
    /// --relay: program_number (= service_id) in the recording's PAT
    /// (hex 0x… or decimal). Default: its only program (or the only one
    /// whose streams carry packets).
    #[arg(long, value_name = "N", requires = "relay", value_parser = parse_u16)]
    pub program: Option<u16>,
    /// --relay: select the program by its SDT service name (exact, else a
    /// unique substring; full-width ASCII folded, e.g. "TV12").
    #[arg(
        long,
        value_name = "NAME",
        requires = "relay",
        conflicts_with = "program"
    )]
    pub service_name: Option<String>,
    /// --relay: remote_control_key_id 1..12 (default 9).
    #[arg(long, value_name = "1..12", requires = "relay")]
    pub remote_key: Option<u8>,
    /// --relay: area_code of the terrestrial delivery system descriptor
    /// (12 bits, B10 Annex D Table D-2; default 0x5A5, set it to the
    /// receiver's area).
    #[arg(long, value_name = "CODE", requires = "relay", value_parser = parse_u16)]
    pub area_code: Option<u16>,
    /// --relay: network / TS / service / provider name (default: the
    /// recording's SDT service name, else "ベンチ").
    #[arg(long, value_name = "NAME", requires = "relay")]
    pub name: Option<String>,
    /// --relay: digital_recording_control_data of the service. Default: the
    /// source program's own value (its PMT program loop, else its relayed
    /// ES loops, else its SDT, else its EIT p/f digital_copy_control_descriptor;
    /// copy-free only when the source signals none), with its per-component
    /// (ES loop) descriptors kept. An explicit value is used as given (the
    /// ES-loop descriptors are then dropped); the source's value is printed
    /// either way. copy_control_type is '01' in every case.
    #[arg(long, value_enum, value_name = "MODE", requires = "relay")]
    pub copy: Option<CopyArg>,
    /// --relay: the recording's conditional-access signalling: keep
    /// (default; PMT conditional_access_descriptors, the ECM PIDs and the
    /// CAT's CA_descriptors with the EMM PIDs present in the recording are
    /// relayed, CA_PIDs following the PID mapping) or drop. The payload is
    /// unscrambled either way (scrambled input is refused), so receivers
    /// see CA signalling with a clear payload.
    #[arg(long, value_enum, value_name = "MODE", requires = "relay")]
    pub ca: Option<CaArg>,
    /// --relay: at the end of the recording loop (default) or end.
    #[arg(long, value_enum, value_name = "WHAT", requires = "relay")]
    pub at_eof: Option<AtEofArg>,
    /// --relay: EIT present/following event names, texts and genres from
    /// the recording (default true; `--eit-from-source false` sends the
    /// fallback event).
    #[arg(
        long,
        value_name = "BOOL",
        requires = "relay",
        num_args = 0..=1,
        default_missing_value = "true",
        action = clap::ArgAction::Set
    )]
    pub eit_from_source: Option<bool>,
    /// --relay: fixed JST start of the stream clock, YYYY-MM-DDTHH:MM:SS
    /// (default: the system clock); makes the output reproducible.
    #[arg(long, value_name = "JST", requires = "relay")]
    pub clock_start: Option<String>,
    /// --relay: print the resolved configuration as TOML on stdout (to
    /// save, edit and use with --mux-config / -c) and exit.
    #[arg(long, requires = "relay")]
    pub print_config: bool,
    /// When a ts-program relay (--relay or --mux-config) needs more than
    /// its layer carries, use the first layout that fits and keeps the
    /// layer set (partial reception and the one-seg layer A), e.g. B
    /// 12seg 64QAM 7/8, instead of refusing; the choice is printed.
    #[arg(long)]
    pub auto_layout: bool,
}

/// `--copy` values (B10 Table 6-39 digital_recording_control_data); without
/// `--copy` the source program's value is kept.
#[derive(clap::ValueEnum, Clone, Copy, Debug, PartialEq, Eq)]
#[allow(clippy::enum_variant_names)] // the values are the B10 Table 6-39 names
pub enum CopyArg {
    /// '00' copy without restriction.
    CopyFree,
    /// '10' one generation of copies.
    CopyOneGeneration,
    /// '11' copy forbidden.
    CopyNever,
}

/// `--ca` values (= `isdbt_mux::config::CaMode`).
#[derive(clap::ValueEnum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum CaArg {
    /// Relay the CA descriptors, ECM and EMM.
    Keep,
    /// Drop them.
    Drop,
}

/// `--at-eof` values.
#[derive(clap::ValueEnum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum AtEofArg {
    /// Restart at the first access point, timestamps continuing.
    Loop,
    /// Stop relaying (the transmission then ends).
    End,
}

/// Parse `0x…` hex or decimal.
pub fn parse_u16(s: &str) -> std::result::Result<u16, String> {
    let r = match s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        Some(h) => u16::from_str_radix(h, 16),
        None => s.parse(),
    };
    r.map_err(|e| format!("{s:?}: {e}"))
}

/// The layer a relayed service goes on: the largest active layer, layer A
/// excluded under partial reception (it is the one-seg layer).
pub fn service_layer(p: &Params) -> Result<LayerName> {
    let (id, _) = p
        .active_layers()
        .filter(|(id, _)| !(p.partial_reception && *id == Layer::A))
        .max_by_key(|(id, l)| (l.segments, std::cmp::Reverse(id.index())))
        .context("no layer can carry the relayed service")?;
    Ok(match id {
        Layer::A => LayerName::A,
        Layer::B => LayerName::B,
        Layer::C => LayerName::C,
    })
}

impl RelayArgs {
    /// The relay options for `channel` and `params` (the recording's SDT
    /// service name is read when `--name` is not given).
    pub fn options(&self, channel: u32, params: &Params) -> Result<RelayOptions> {
        let src = self.relay.as_ref().context("--relay FILE")?;
        channel_hz(channel)?;
        let abs =
            std::fs::canonicalize(src).with_context(|| format!("--relay {}", src.display()))?;
        let mut o = RelayOptions::new(&abs);
        o.program = self.program;
        o.service_name = self.service_name.clone();
        o.channel = channel;
        if let Some(k) = self.remote_key {
            o.remote_control_key_id = k;
        }
        if let Some(a) = self.area_code {
            if a > 0x0FFF {
                bail!("--area-code {a:#x}: the area_code is 12 bits");
            }
            o.area_code = a;
        }
        o.recording_control = match self.copy {
            None => isdbt_mux::signalling::RecordingControlCfg::SOURCE,
            Some(CopyArg::CopyFree) => "copy-free",
            Some(CopyArg::CopyOneGeneration) => "copy-one-generation",
            Some(CopyArg::CopyNever) => "copy-never",
        }
        .into();
        o.ca = match self.ca.unwrap_or(CaArg::Keep) {
            CaArg::Keep => CaMode::Keep,
            CaArg::Drop => CaMode::Drop,
        };
        o.at_eof = match self.at_eof.unwrap_or(AtEofArg::Loop) {
            AtEofArg::Loop => AtEof::Loop,
            AtEofArg::End => AtEof::End,
        };
        o.eit_from_source = self.eit_from_source.unwrap_or(true);
        o.layer = service_layer(params)?;
        o.clock_start = self.clock_start.clone();
        o.name = match &self.name {
            Some(n) => n.clone(),
            None => {
                let st: StreamCfg = o.config()?.services[0].streams[0].clone();
                let (_, name, _) = isdbt_mux::relay::select_source_program(&abs, &st)
                    .context("reading the recording's PAT/PMT/SDT")?;
                name.filter(|n| !n.trim().is_empty())
                    .unwrap_or_else(|| DEFAULT_NAME.to_string())
            }
        };
        Ok(o)
    }
}

/// Build the multiplexer; with `auto_layout`, a `ts-program` refused for
/// its rate is retried once with the first suggested layout that keeps the
/// layer set (`isdbt_mux::capacity::suggest`). Returns the parameters used.
pub fn build_mux(
    cfg: &Config,
    base: &Path,
    params: Params,
    auto_layout: bool,
) -> Result<(Multiplexer, Params)> {
    match Multiplexer::new(cfg, &params, base) {
        Ok(m) => Ok((m, params)),
        Err(MuxError::OverCapacity {
            layer,
            need_bps,
            message,
        }) if auto_layout => {
            let pick = isdbt_mux::capacity::suggest(&params, layer, need_bps, 64)
                .into_iter()
                .find(|s| s.layer == layer && s.params.partial_reception == params.partial_reception);
            let Some(s) = pick else {
                bail!(
                    "{message}\n  --auto-layout: no layout keeps layer {layer} and the layer set; choose one of the above"
                );
            };
            eprintln!(
                "isdb: --auto-layout: the relay needs {:.2} Mbit/s on layer {layer} ({:.2} Mbit/s available); using {} [{}]",
                need_bps / 1e6,
                isdbt_mux::capacity::layer_bps(&params, layer) / 1e6,
                s.summary,
                s.options
            );
            let m = Multiplexer::new(cfg, &s.params, base).context("building the multiplexer")?;
            Ok((m, s.params))
        }
        Err(e @ MuxError::OverCapacity { .. }) => {
            Err(anyhow::Error::new(e)).context("building the multiplexer (--auto-layout picks the first layout above that keeps partial reception)")
        }
        Err(e) => Err(anyhow::Error::new(e)).context("building the multiplexer"),
    }
}

/// `--print-config`: the TOML of `o` with a header naming the source
/// selection and the transmission options (after `--auto-layout`).
pub fn print_config(o: &RelayOptions, params: &Params, auto_layout: bool) -> Result<()> {
    let cfg = o.config()?;
    let mut o = o.clone();
    let used = match build_mux(&cfg, Path::new("."), params.clone(), auto_layout) {
        Ok((m, p)) => {
            for n in m.notes() {
                eprintln!("{n}");
            }
            Some(p)
        }
        Err(e) => {
            eprintln!("isdb: warning: the configuration does not run with these parameters: {e:#}");
            None
        }
    };
    o.header = vec![
        "Generated by `isdb … --relay … --print-config`; use it with".into(),
        format!(
            "  isdb tx --mux-config THIS.toml {} --hackrf --channel {} --txvga 20 --rf-enable",
            phy_options(used.as_ref().unwrap_or(params)),
            o.channel
        ),
        format!(
            "Transmission parameters: {}{}",
            describe_params(used.as_ref().unwrap_or(params)),
            if used.is_none() {
                " (DO NOT carry the measured rate; see stderr)"
            } else {
                ""
            }
        ),
    ];
    print!("{}", o.to_toml());
    Ok(())
}

/// One line: the service a `--relay` builds.
pub fn summary(src: &Path, o: &RelayOptions) -> String {
    format!(
        "isdb: relay {}: service 0x0400 \"{}\" on layer {:?}, UHF ch{} (NIT), remote key {}, area {:#05x}, {}, CA {}",
        src.display(),
        o.name,
        o.layer,
        o.channel,
        o.remote_control_key_id,
        o.area_code,
        if o.recording_control == isdbt_mux::signalling::RecordingControlCfg::SOURCE {
            "copy control as the source".to_string()
        } else {
            format!("copy control {} (explicit)", o.recording_control)
        },
        if o.ca == CaMode::Drop {
            "dropped"
        } else {
            "kept"
        }
    )
}

/// A ready input for `tx` / `loopback`, or nothing to transmit.
pub enum Prepared {
    /// `--print-config` done.
    Exit,
    /// Run with these parameters; `Some` input when it was built here
    /// (`--relay`, `--mux-config`), else open the other inputs.
    Ready(Params, Option<Box<dyn FrameInput>>),
}

/// Resolve `--relay` / `--mux-config` (with `--auto-layout`) before the
/// transmitter is built, since they may change the parameters.
pub fn prepare(
    input: &InputArgs,
    params: Params,
    channel: impl FnOnce() -> Result<u32>,
) -> Result<Prepared> {
    let r = &input.relay;
    if let Some(src) = &r.relay {
        let ch = channel()?;
        let o = r.options(ch, &params)?;
        if r.print_config {
            print_config(&o, &params, r.auto_layout)?;
            return Ok(Prepared::Exit);
        }
        let cfg = o.config()?;
        let (mux, params) = build_mux(&cfg, Path::new("."), params, r.auto_layout)?;
        eprintln!("{}", summary(src, &o));
        let name = format!("--relay {}", src.display());
        return Ok(Prepared::Ready(
            params,
            Some(Box::new(MuxConfigInput::from_mux(mux, name, &cfg))),
        ));
    }
    if let Some(path) = &input.mux_config {
        let text =
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        let cfg =
            Config::from_toml(&text).with_context(|| format!("parsing {}", path.display()))?;
        let base = path.parent().map(PathBuf::from).unwrap_or_default();
        let (mux, params) = build_mux(&cfg, &base, params, r.auto_layout)?;
        let name = path.display().to_string();
        return Ok(Prepared::Ready(
            params,
            Some(Box::new(MuxConfigInput::from_mux(mux, name, &cfg))),
        ));
    }
    if r.auto_layout {
        bail!("--auto-layout applies to --relay and --mux-config");
    }
    Ok(Prepared::Ready(params, None))
}

/// The NIT channel of a `tx --relay`: `--channel`, else the UHF channel
/// whose centre is `--freq` (±0.5 MHz), else 44.
pub fn tx_channel(channel: Option<u32>, freq: Option<u64>) -> Result<u32> {
    if let Some(c) = channel {
        return Ok(c);
    }
    if let Some(f) = freq {
        return (13..=62)
            .find(|&c| channel_hz(c).is_ok_and(|h| h.abs_diff(f) <= 500_000))
            .with_context(|| {
                format!(
                    "--relay: {f} Hz is not a UHF channel centre; the NIT must name the transmit channel, give --channel"
                )
            });
    }
    Ok(44)
}

#[cfg(test)]
mod tests {
    use super::*;
    use isdbt_core::{CodeRate, GuardInterval, LayerConfig, Mode, Modulation, TimeInterleave};

    #[test]
    fn numbers_channels_and_layers() {
        assert_eq!(parse_u16("0x00d3"), Ok(0xD3));
        assert_eq!(parse_u16("211"), Ok(211));
        assert!(parse_u16("0x1_0000").is_err());
        assert_eq!(tx_channel(None, None).unwrap(), 44);
        assert_eq!(tx_channel(Some(27), Some(1)).unwrap(), 27);
        assert_eq!(tx_channel(None, Some(659_142_857)).unwrap(), 44);
        assert!(tx_channel(None, Some(660_000_000)).is_err());
        let l = |segments| {
            Some(LayerConfig {
                segments,
                modulation: Modulation::Qam64,
                code_rate: CodeRate::R3_4,
                time_interleave: TimeInterleave(2),
            })
        };
        let p = Params::new(Mode::M3, GuardInterval::G1_8, true, [l(1), l(12), None]).unwrap();
        assert_eq!(service_layer(&p).unwrap(), LayerName::B);
        let p = Params::new(Mode::M3, GuardInterval::G1_8, false, [l(13), None, None]).unwrap();
        assert_eq!(service_layer(&p).unwrap(), LayerName::A);
        let p = Params::new(Mode::M3, GuardInterval::G1_8, true, [l(1), l(4), l(8)]).unwrap();
        assert_eq!(service_layer(&p).unwrap(), LayerName::C);
    }
}
