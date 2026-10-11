//! Command-line description of ISDB-T transmission parameters and of the
//! RF tuning shared by `tx --radio` and `rx --radio`.

use anyhow::{Context, Result, bail};
use clap::Args;
use isdb_radio::{DeviceSpec, RadioConfig};
use isdbt_core::*;

/// Transmission parameters (what TMCC signals, ARIB STD-B31 §3.15).
/// Defaults are the Japanese terrestrial norm: Mode 3, GI 1/8, partial
/// reception, A = one-seg QPSK 2/3, B = 12 seg 64QAM 3/4, both TI index 2
/// (I = 2 in Mode 3, Table 3-12).
#[derive(Debug, Clone, Args)]
pub struct PhyArgs {
    /// Mode 1, 2 or 3.
    #[arg(long, default_value_t = 3)]
    pub mode: u8,
    /// Guard-interval denominator: 4, 8, 16 or 32 (Japanese broadcasts use 8).
    #[arg(long, default_value_t = 8)]
    pub gi: usize,
    /// Partial reception (layer A = the single centre segment, one-seg).
    #[arg(long, default_value_t = true, action = clap::ArgAction::Set)]
    pub partial: bool,
    /// Layer A: SEGMENTS,MOD,RATE,TI  (MOD dqpsk|qpsk|16qam|64qam, RATE
    /// 1/2|2/3|3/4|5/6|7/8, TI = TMCC time-interleave index 0..3).
    #[arg(long = "layer-a", default_value = "1,qpsk,2/3,2")]
    pub layer_a: String,
    /// Layer B (same syntax); "none" for a single-layer transmission.
    #[arg(long = "layer-b", default_value = "12,64qam,3/4,2")]
    pub layer_b: String,
    /// Layer C (same syntax) or "none".
    #[arg(long = "layer-c", default_value = "none")]
    pub layer_c: String,
}

/// Parse a layer spec `SEGMENTS,MOD,RATE,TI` (or `none`).
pub fn parse_layer(s: &str) -> Result<Option<LayerConfig>> {
    if s.eq_ignore_ascii_case("none") {
        return Ok(None);
    }
    let f: Vec<&str> = s.split(',').map(str::trim).collect();
    if f.len() != 4 {
        bail!("layer spec {s:?}: want SEGMENTS,MOD,RATE,TI (e.g. 12,64qam,3/4,2) or none");
    }
    let modulation = match f[1].to_ascii_lowercase().as_str() {
        "dqpsk" => Modulation::Dqpsk,
        "qpsk" => Modulation::Qpsk,
        "16qam" => Modulation::Qam16,
        "64qam" => Modulation::Qam64,
        m => bail!("layer spec {s:?}: modulation {m:?} (dqpsk|qpsk|16qam|64qam)"),
    };
    let code_rate = match f[2] {
        "1/2" => CodeRate::R1_2,
        "2/3" => CodeRate::R2_3,
        "3/4" => CodeRate::R3_4,
        "5/6" => CodeRate::R5_6,
        "7/8" => CodeRate::R7_8,
        r => bail!("layer spec {s:?}: code rate {r:?} (1/2|2/3|3/4|5/6|7/8)"),
    };
    let ti: u8 = f[3]
        .parse()
        .with_context(|| format!("layer spec {s:?}: time-interleave index"))?;
    if ti > 3 {
        bail!("layer spec {s:?}: time-interleave index {ti} not in 0..3");
    }
    Ok(Some(LayerConfig {
        segments: f[0]
            .parse()
            .with_context(|| format!("layer spec {s:?}: segments"))?,
        modulation,
        code_rate,
        time_interleave: TimeInterleave(ti),
    }))
}

impl PhyArgs {
    /// Validated parameters.
    pub fn params(&self) -> Result<Params> {
        let mode = Mode::from_number(self.mode).context("--mode must be 1, 2 or 3")?;
        let gi = GuardInterval::from_denominator(self.gi).context("--gi must be 4, 8, 16 or 32")?;
        Ok(Params::new(
            mode,
            gi,
            self.partial,
            [
                parse_layer(&self.layer_a)?,
                parse_layer(&self.layer_b)?,
                parse_layer(&self.layer_c)?,
            ],
        )?)
    }
}

/// One-line description of a parameter set, e.g.
/// `Mode 3, GI 1/8, partial; A 1seg QPSK 2/3 TI2 (64 TSP/frame); …`.
pub fn describe_params(p: &Params) -> String {
    let mut s = format!(
        "Mode {}, GI 1/{}, {}",
        p.mode().number(),
        p.guard().denominator(),
        if p.partial_reception() {
            "partial reception"
        } else {
            "no partial reception"
        }
    );
    for (id, l) in p.active_layers() {
        s += &format!("; {id} {}", describe_layer(l, p.mode()));
    }
    s
}

/// The `--mode/--gi/--partial/--layer-*` options selecting `p`.
pub fn phy_options(p: &Params) -> String {
    let layer = |i: usize| {
        p.layers()[i].map_or("none".to_string(), |l| isdbt_mux::capacity::layer_spec(&l))
    };
    format!(
        "--mode {} --gi {} --partial {} --layer-a {} --layer-b {} --layer-c {}",
        p.mode().number(),
        p.guard().denominator(),
        p.partial_reception(),
        layer(0),
        layer(1),
        layer(2)
    )
}

/// `12seg 64QAM 3/4 TI2 (2592 TSP/frame)`.
pub fn describe_layer(l: &LayerConfig, mode: Mode) -> String {
    let m = match l.modulation {
        Modulation::Dqpsk => "DQPSK",
        Modulation::Qpsk => "QPSK",
        Modulation::Qam16 => "16QAM",
        Modulation::Qam64 => "64QAM",
    };
    let (n, d) = l.code_rate.ratio();
    format!(
        "{}seg {m} {n}/{d} TI{} ({} TSP/frame)",
        l.segments,
        l.time_interleave.0,
        l.tsp_per_frame(mode)
    )
}

/// Centre frequency of Japanese UHF terrestrial channel `ch` (13..62):
/// 473 + 6·(ch − 13) + 1/7 MHz (channel plan of the Radio Act ordinance;
/// the 1/7 MHz offset centres the 5.57 MHz OFDM band in the 6 MHz channel).
pub fn channel_hz(ch: u32) -> Result<u64> {
    if !(13..=62).contains(&ch) {
        bail!("UHF channel {ch} outside 13..62");
    }
    // 1/7 MHz = 142 857.142… Hz, rounded to the nearest hertz.
    Ok(473_000_000 + 6_000_000 * (ch as u64 - 13) + 142_857)
}

/// The Japanese UHF terrestrial TV band (channels 13–52, edges 470 and
/// 710 MHz): the centre frequencies `--channel` / `--freq` may select
/// without `--freq-unrestricted`.
pub const TV_BAND_HZ: std::ops::RangeInclusive<u64> = 470_000_000..=710_000_000;

/// RF tuning shared by `tx --radio` and `rx --radio`.
#[derive(Debug, Clone, Args)]
pub struct TuneArgs {
    /// Japanese UHF channel 13..62 (centre 473 + 6·(ch−13) + 1/7 MHz).
    #[arg(long, conflicts_with = "freq")]
    pub channel: Option<u32>,
    /// Centre frequency in Hz (alternative to --channel); 470–710 MHz, the
    /// Japanese UHF TV band, unless --freq-unrestricted.
    #[arg(long)]
    pub freq: Option<u64>,
    /// Accept a centre frequency (from --freq or --channel) outside the
    /// 470–710 MHz UHF TV band (shielded lab setups only); the radio's own
    /// tuning range still applies.
    #[arg(long)]
    pub freq_unrestricted: bool,
    /// Reference-clock error of the radio in ppm (positive = the device
    /// clock runs fast); the tuning frequency and the sample clock are both
    /// divided by (1 + ppm·1e-6) so the signal on air is at the nominal
    /// values.
    #[arg(long, default_value_t = 0.0, allow_negative_numbers = true)]
    pub freq_corr_ppm: f64,
    /// Analog (baseband filter) bandwidth request in Hz; the backend chooses
    /// how to honour it and reports the bandwidth it programmed. 6 MHz
    /// passes the 5.57 MHz ISDB-T band.
    #[arg(long, default_value_t = 6_000_000)]
    pub bb_filter_hz: u32,
}

/// Frequency and sample-clock request derived from [`TuneArgs`].
#[derive(Debug, Clone, PartialEq)]
pub struct Tuning {
    /// Nominal centre frequency.
    pub nominal_hz: u64,
    /// Frequency to program into the radio (ppm-corrected).
    pub set_hz: u64,
    /// Channel number if given.
    pub channel: Option<u32>,
    /// Sample-rate request as a rational number `rate_num / rate_den` Hz
    /// (ppm-corrected): the ISDB-T 512/63 MHz clock.
    pub rate_num: u32,
    /// Denominator of the sample-rate request.
    pub rate_den: u32,
}

impl TuneArgs {
    /// Resolve channel/frequency and the ppm correction.
    pub fn tuning(&self) -> Result<Tuning> {
        let nominal_hz = match (self.channel, self.freq) {
            (Some(ch), None) => channel_hz(ch)?,
            (None, Some(f)) => f,
            (None, None) => bail!("give --channel N or --freq HZ"),
            (Some(_), Some(_)) => bail!("--channel and --freq are exclusive"),
        };
        if !self.freq_unrestricted && !TV_BAND_HZ.contains(&nominal_hz) {
            bail!(
                "centre frequency {:.6} MHz{} is outside the 470–710 MHz UHF TV band (channels 13–52); add --freq-unrestricted only for a shielded lab setup",
                nominal_hz as f64 / 1e6,
                self.channel
                    .map(|c| format!(" (UHF ch{c})"))
                    .unwrap_or_default()
            );
        }
        if !(-200.0..=200.0).contains(&self.freq_corr_ppm) {
            bail!("--freq-corr-ppm {} out of range ±200", self.freq_corr_ppm);
        }
        let k = 1.0 + self.freq_corr_ppm * 1e-6;
        Ok(Tuning {
            nominal_hz,
            set_hz: (nominal_hz as f64 / k).round() as u64,
            channel: self.channel,
            rate_num: (SAMPLE_RATE_NUM as f64 / k).round() as u32,
            rate_den: SAMPLE_RATE_DEN as u32,
        })
    }

    /// The radio configuration for `spec` with `gains` (passed through
    /// unparsed).
    pub fn radio_config(
        &self,
        spec: &DeviceSpec,
        gains: &[(String, String)],
    ) -> Result<(RadioConfig, Tuning)> {
        let t = self.tuning()?;
        Ok((
            RadioConfig {
                center_hz: t.set_hz,
                nominal_hz: t.nominal_hz,
                rate_num: t.rate_num,
                rate_den: t.rate_den,
                bandwidth_hz: self.bb_filter_hz,
                args: spec.args.clone(),
                gains: gains.to_vec(),
            },
            t,
        ))
    }
}

impl Tuning {
    /// ` (UHF ch13)` when tuned by channel, else empty.
    pub fn channel_label(&self) -> String {
        self.channel
            .map(|c| format!(" (UHF ch{c})"))
            .unwrap_or_default()
    }
}

/// Parse a `--gain NAME=VALUE` argument (the backend checks the name and
/// the value).
pub fn parse_gain(s: &str) -> Result<(String, String)> {
    isdb_radio::parse_key_value(s)
}

/// Parse a PID list such as `0x1fc8,0x0000,16`.
pub fn parse_pids(s: &str) -> Result<Vec<u16>> {
    s.split(',')
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(|t| {
            let v = if let Some(h) = t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
                u16::from_str_radix(h, 16)
            } else {
                t.parse()
            }
            .with_context(|| format!("PID {t:?}"))?;
            if v > 0x1fff {
                bail!("PID {t} > 0x1FFF");
            }
            Ok(v)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn phy(a: &str, b: &str, c: &str) -> PhyArgs {
        PhyArgs {
            mode: 3,
            gi: 8,
            partial: true,
            layer_a: a.into(),
            layer_b: b.into(),
            layer_c: c.into(),
        }
    }

    #[test]
    fn default_layers_are_the_japanese_norm() {
        let p = phy("1,qpsk,2/3,2", "12,64qam,3/4,2", "none")
            .params()
            .unwrap();
        assert_eq!(p.mode(), Mode::M3);
        assert_eq!(p.guard(), GuardInterval::G1_8);
        assert!(p.partial_reception());
        let a = p.layers()[0].unwrap();
        assert_eq!(
            (a.segments, a.modulation, a.code_rate, a.time_interleave.0),
            (1, Modulation::Qpsk, CodeRate::R2_3, 2)
        );
        let b = p.layers()[1].unwrap();
        assert_eq!(
            (b.segments, b.modulation, b.code_rate),
            (12, Modulation::Qam64, CodeRate::R3_4)
        );
        assert!(p.layers()[2].is_none());
        // Table 3-3 × N for Mode 3: 64 and 2592 TSPs per frame.
        assert_eq!(a.tsp_per_frame(p.mode()), 64);
        assert_eq!(b.tsp_per_frame(p.mode()), 2592);
        assert!(describe_params(&p).contains("B 12seg 64QAM 3/4 TI2 (2592 TSP/frame)"));
    }

    #[test]
    fn layer_spec_errors() {
        assert!(parse_layer("12,64qam,3/4").is_err());
        assert!(parse_layer("12,256qam,3/4,2").is_err());
        assert!(parse_layer("12,64qam,4/5,2").is_err());
        assert!(parse_layer("12,64qam,3/4,4").is_err());
        assert!(parse_layer("x,64qam,3/4,2").is_err());
        assert_eq!(parse_layer("NONE").unwrap(), None);
        assert_eq!(
            parse_layer(" 13 , 16QAM , 1/2 , 0")
                .unwrap()
                .unwrap()
                .modulation,
            Modulation::Qam16
        );
        // Segments must sum to 13; partial reception needs A = 1 segment.
        assert!(
            phy("1,qpsk,2/3,2", "11,64qam,3/4,2", "none")
                .params()
                .is_err()
        );
        assert!(
            phy("2,qpsk,2/3,2", "11,64qam,3/4,2", "none")
                .params()
                .is_err()
        );
        assert!(
            phy("1,qpsk,2/3,2", "none", "12,64qam,3/4,2")
                .params()
                .is_err()
        );
        let mut p = phy("13,64qam,3/4,2", "none", "none");
        p.partial = false;
        assert!(p.params().is_ok());
        p.mode = 4;
        assert!(p.params().is_err());
    }

    #[test]
    fn uhf_channel_centres() {
        // ch13 = 473 + 1/7 MHz; ch62 = 767 + 1/7 MHz; 6 MHz raster.
        assert_eq!(channel_hz(13).unwrap(), 473_142_857);
        assert_eq!(channel_hz(14).unwrap(), 479_142_857);
        assert_eq!(channel_hz(27).unwrap(), 557_142_857);
        assert_eq!(channel_hz(62).unwrap(), 767_142_857);
        assert!(channel_hz(12).is_err());
        assert!(channel_hz(63).is_err());
    }

    #[test]
    fn tuning_with_ppm_correction() {
        let mut t = TuneArgs {
            channel: Some(13),
            freq: None,
            freq_unrestricted: false,
            freq_corr_ppm: 0.0,
            bb_filter_hz: 6_000_000,
        };
        let n = t.tuning().unwrap();
        assert_eq!(
            (n.set_hz, n.rate_num, n.rate_den),
            (473_142_857, 512_000_000, 63)
        );
        t.freq_corr_ppm = 10.0;
        let c = t.tuning().unwrap();
        // f / (1 + 10e-6): 473142857 / 1.00001 = 473138125.6…
        assert_eq!(c.set_hz, 473_138_126);
        assert_eq!(c.rate_num, 511_994_880);
        t.channel = None;
        assert!(t.tuning().is_err());
        t.freq = Some(500_000_000);
        t.freq_corr_ppm = 0.0;
        assert_eq!(t.tuning().unwrap().set_hz, 500_000_000);
    }

    #[test]
    fn freq_restricted_to_the_uhf_tv_band() {
        let t = |channel: Option<u32>, freq: Option<u64>, unrestricted: bool| {
            TuneArgs {
                channel,
                freq,
                freq_unrestricted: unrestricted,
                freq_corr_ppm: 0.0,
                bb_filter_hz: 6_000_000,
            }
            .tuning()
        };
        // band edges and the channels inside it
        assert!(t(None, Some(470_000_000), false).is_ok());
        assert!(t(None, Some(710_000_000), false).is_ok());
        assert!(t(Some(13), None, false).is_ok());
        assert!(t(Some(52), None, false).is_ok());
        // outside: refused unless --freq-unrestricted
        for f in [469_999_999, 710_000_001, 1_575_420_000, 2_400_000_000] {
            let e = t(None, Some(f), false).unwrap_err().to_string();
            assert!(e.contains("--freq-unrestricted"), "{e}");
            assert_eq!(t(None, Some(f), true).unwrap().nominal_hz, f);
        }
        // channel → frequency is unchanged; 53–62 lie above the band
        let e = t(Some(53), None, false).unwrap_err().to_string();
        assert!(e.contains("713.142857 MHz (UHF ch53)"), "{e}");
        assert_eq!(t(Some(62), None, true).unwrap().nominal_hz, 767_142_857);
        // the device range is the radio backend's to check, not the CLI's
        assert!(t(None, Some(7_000_000_000), true).is_ok());
    }

    #[test]
    fn radio_config_carries_tuning_args_and_gains() {
        let a = TuneArgs {
            channel: Some(13),
            freq: None,
            freq_unrestricted: false,
            freq_corr_ppm: 10.0,
            bb_filter_hz: 5_000_000,
        };
        let spec: DeviceSpec = "x:serial=1".parse().unwrap();
        let gains = vec![parse_gain("g=3").unwrap()];
        let (c, t) = a.radio_config(&spec, &gains).unwrap();
        assert_eq!((c.center_hz, c.nominal_hz), (473_138_126, 473_142_857));
        assert_eq!((c.rate_num, c.rate_den), (511_994_880, 63));
        assert_eq!(c.bandwidth_hz, 5_000_000);
        assert_eq!(c.args, spec.args);
        assert_eq!(c.gains, vec![("g".to_string(), "3".to_string())]);
        assert_eq!(t.channel_label(), " (UHF ch13)");
        assert!(parse_gain("gain").is_err());
    }

    #[test]
    fn pid_lists() {
        assert_eq!(
            parse_pids("0x1fc8,0x0000, 16,0X11").unwrap(),
            vec![0x1fc8, 0, 16, 0x11]
        );
        assert!(parse_pids("0x2000").is_err());
        assert!(parse_pids("zz").is_err());
    }
}
