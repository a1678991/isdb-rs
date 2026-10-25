//! Channel tables shared by every command that tunes a radio: Japanese
//! UHF terrestrial channels (ISDB-T) and the satellite IF of BS and 110° CS
//! transponders (ISDB-S, ISDB-S3), plus the `--freq` syntax that accepts
//! either a frequency or a channel name.
//!
//! None of these frequency plans is in the local specifications
//! (SPECS.md §1.2): the UHF plan is the Radio Act ordinance's, the
//! satellite ones are ITU-R Radio Regulations channel frequencies converted
//! by a Japanese right-hand-circular LNB (LO 10.678 GHz). The satellite IF
//! centres were checked against the spectrum of a cable feed's satellite
//! IF (SPECS.md §1.2, VERIFICATION.md "Satellite IF survey").

use anyhow::{Context, Result, bail};
use std::fmt;
use std::str::FromStr;

/// Centre frequency of Japanese UHF terrestrial channel `ch` (13..62):
/// 473 + 6·(ch − 13) + 1/7 MHz (channel plan of the Radio Act ordinance;
/// the 1/7 MHz offset centres the 5.57 MHz OFDM band in the 6 MHz channel).
pub fn uhf_hz(ch: u32) -> Result<u64> {
    if !(13..=62).contains(&ch) {
        bail!("channel T{ch}: outside T13..T62");
    }
    // 1/7 MHz = 142 857.142… Hz, rounded to the nearest hertz.
    Ok(473_000_000 + 6_000_000 * (u64::from(ch) - 13) + 142_857)
}

/// The Japanese UHF terrestrial TV band (channels 13–52, edges 470 and
/// 710 MHz): the centre frequencies `--channel` / `--freq` may select
/// without `--freq-unrestricted`.
pub const TV_BAND_HZ: std::ops::RangeInclusive<u64> = 470_000_000..=710_000_000;

/// LO of the right-hand-circular BS / 110° CS LNB: IF = RF − 10.678 GHz.
pub const LNB_LO_HZ: u64 = 10_678_000_000;

/// RF centre of BS channel `n` (odd 1..=23, right-hand circular):
/// 11 727.48 + 19.18·(n − 1) MHz.
pub fn bs_rf_hz(n: u32) -> Result<u64> {
    if !(1..=23).contains(&n) || n.is_multiple_of(2) {
        bail!(
            "channel BS{n}: odd 1..23 (the right-hand-circular channels; the even, left-hand ones use another LNB LO and are not tabulated)"
        );
    }
    Ok(11_727_480_000 + 19_180_000 * (u64::from(n) - 1))
}

/// RF centre of 110° CS channel ND`n` (even 2..=24): 12 251 + 20·n MHz
/// (ND2 = 12.291 GHz, 40 MHz apart).
pub fn nd_rf_hz(n: u32) -> Result<u64> {
    if !(2..=24).contains(&n) || !n.is_multiple_of(2) {
        bail!("channel ND{n}: even 2..24 (110° CS)");
    }
    Ok(12_251_000_000 + 20_000_000 * u64::from(n))
}

/// A named channel, written `T27`, `BS1` or `ND2` (case-insensitive when
/// parsed; a bare number is a terrestrial channel).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Channel {
    /// Japanese UHF terrestrial channel 13..62 (`T27`).
    T(u32),
    /// BS channel, odd 1..23 (`BS1`), at its satellite IF.
    Bs(u32),
    /// 110° CS channel, even 2..24 (`ND2`), at its satellite IF.
    Nd(u32),
}

impl Channel {
    /// The frequency a radio tunes to: the UHF centre, or the satellite
    /// IF centre (RF − [`LNB_LO_HZ`]).
    pub fn tune_hz(&self) -> Result<u64> {
        Ok(match *self {
            Self::T(c) => uhf_hz(c)?,
            Self::Bs(n) => bs_rf_hz(n)? - LNB_LO_HZ,
            Self::Nd(n) => nd_rf_hz(n)? - LNB_LO_HZ,
        })
    }

    /// The RF centre on air (equal to [`Channel::tune_hz`] for UHF).
    pub fn rf_hz(&self) -> Result<u64> {
        match *self {
            Self::T(c) => uhf_hz(c),
            Self::Bs(n) => bs_rf_hz(n),
            Self::Nd(n) => nd_rf_hz(n),
        }
    }

    /// ` (T27)`, ` (BS1 IF)`, ` (ND2 IF)`.
    pub fn label(&self) -> String {
        match *self {
            Self::T(_) => format!(" ({self})"),
            Self::Bs(_) | Self::Nd(_) => format!(" ({self} IF)"),
        }
    }

    /// True for a BS or 110° CS channel (a satellite IF).
    pub fn is_satellite(&self) -> bool {
        matches!(self, Self::Bs(_) | Self::Nd(_))
    }
}

impl fmt::Display for Channel {
    /// `T27`, `BS1`, `ND2`: the form [`Channel::from_str`] reads.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::T(c) => write!(f, "T{c}"),
            Self::Bs(n) => write!(f, "BS{n}"),
            Self::Nd(n) => write!(f, "ND{n}"),
        }
    }
}

impl FromStr for Channel {
    type Err = anyhow::Error;

    /// `T27`, `BS1`, `ND2`, case-insensitive; a bare number (`27`) is a
    /// terrestrial channel. The number is checked against the table.
    fn from_str(s: &str) -> Result<Self> {
        const WANT: &str = "T13..T62 (UHF), BS1..BS23 (odd) or ND2..ND24 (even)";
        let l = s.trim().to_ascii_lowercase();
        let i = l
            .find(|c: char| c.is_ascii_digit())
            .with_context(|| format!("channel {s:?}: want {WANT}"))?;
        let (kind, num) = l.split_at(i);
        let n: u32 = num
            .parse()
            .with_context(|| format!("channel {s:?}: want {WANT}"))?;
        let c = match kind {
            "" | "t" => Self::T(n),
            "bs" => Self::Bs(n),
            "nd" => Self::Nd(n),
            _ => bail!("channel {s:?}: want {WANT}"),
        };
        c.tune_hz()?;
        Ok(c)
    }
}

/// A `--freq` value: a frequency (`1049480000`, `1049.48M`, `557.142857MHz`,
/// `2.4G`) or a channel name with its prefix (`T27`, `BS1`, `ND2`; a bare
/// number is a frequency here).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Freq {
    /// The frequency to tune to, Hz.
    pub hz: u64,
    /// The channel it was given as, if any.
    pub channel: Option<Channel>,
}

impl FromStr for Freq {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self> {
        if s.trim()
            .starts_with(|c: char| c.is_ascii_digit() || c == '.')
        {
            let hz = parse_hz(s)?;
            let hz = u64::try_from(hz.round() as i128)
                .ok()
                .filter(|&h| h > 0)
                .with_context(|| format!("frequency {s:?}: must be > 0"))?;
            return Ok(Self { hz, channel: None });
        }
        let c: Channel = s.parse()?;
        Ok(Self {
            hz: c.tune_hz()?,
            channel: Some(c),
        })
    }
}

/// A rate or frequency in Hz with an optional `k`, `M`, `G` suffix (and an
/// optional trailing `Hz` / `sps`): `57.72M`, `61440000`, `44MHz`.
pub fn parse_hz(s: &str) -> Result<f64> {
    let t = s.trim();
    let t = t
        .strip_suffix("Hz")
        .or_else(|| t.strip_suffix("hz"))
        .or_else(|| t.strip_suffix("sps"))
        .unwrap_or(t);
    let (num, mul) = match t.chars().last() {
        Some('k' | 'K') => (&t[..t.len() - 1], 1e3),
        Some('M') => (&t[..t.len() - 1], 1e6),
        Some('G') => (&t[..t.len() - 1], 1e9),
        _ => (t, 1.0),
    };
    let v: f64 = num
        .parse()
        .ok()
        .filter(|v: &f64| v.is_finite() && *v >= 0.0)
        .with_context(|| format!("{s:?}: want a number of Hz, optionally with k, M or G"))?;
    Ok(v * mul)
}

/// A sample count with an optional binary suffix: `4194304`, `4Mi`,
/// `512Ki`.
pub fn parse_count(s: &str) -> Result<usize> {
    let t = s.trim();
    let (num, mul) = if let Some(n) = t.strip_suffix("Mi") {
        (n, 1usize << 20)
    } else if let Some(n) = t.strip_suffix("Ki") {
        (n, 1 << 10)
    } else {
        (t, 1)
    };
    let n: usize = num
        .parse()
        .with_context(|| format!("{s:?}: want a whole number, optionally with Ki or Mi"))?;
    n.checked_mul(mul)
        .with_context(|| format!("{s:?}: too large"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uhf_channel_centres() {
        // T13 = 473 + 1/7 MHz; T62 = 767 + 1/7 MHz; 6 MHz raster.
        assert_eq!(uhf_hz(13).unwrap(), 473_142_857);
        assert_eq!(uhf_hz(14).unwrap(), 479_142_857);
        assert_eq!(uhf_hz(27).unwrap(), 557_142_857);
        assert_eq!(uhf_hz(62).unwrap(), 767_142_857);
        assert!(uhf_hz(12).is_err());
        assert!(uhf_hz(63).is_err());
    }

    #[test]
    fn satellite_if_centres() {
        // BSn IF = 1049.48 + 19.18·(n − 1) MHz, odd n.
        let bs = |n| Channel::Bs(n).tune_hz().unwrap();
        assert_eq!(bs(1), 1_049_480_000);
        assert_eq!(bs(3), 1_087_840_000);
        assert_eq!(bs(23), 1_471_440_000);
        assert_eq!(Channel::Bs(1).rf_hz().unwrap(), 11_727_480_000);
        assert!(Channel::Bs(2).tune_hz().is_err());
        assert!(Channel::Bs(25).tune_hz().is_err());
        // NDn IF = 1613 + 20·(n − 2) MHz, even n.
        let nd = |n| Channel::Nd(n).tune_hz().unwrap();
        assert_eq!(nd(2), 1_613_000_000);
        assert_eq!(nd(24), 2_053_000_000);
        assert_eq!(Channel::Nd(2).rf_hz().unwrap(), 12_291_000_000);
        assert!(Channel::Nd(3).tune_hz().is_err());
        assert!(Channel::Nd(26).tune_hz().is_err());
    }

    #[test]
    fn channel_names() {
        for (s, c) in [
            ("T27", Channel::T(27)),
            ("t13", Channel::T(13)),
            ("BS1", Channel::Bs(1)),
            ("bs23", Channel::Bs(23)),
            ("Bs5", Channel::Bs(5)),
            ("ND2", Channel::Nd(2)),
            ("nd24", Channel::Nd(24)),
            ("Nd12", Channel::Nd(12)),
        ] {
            assert_eq!(s.parse::<Channel>().unwrap(), c, "{s}");
            assert_eq!(c.to_string().parse::<Channel>().unwrap(), c);
        }
        for bad in [
            "", "bs", "BS2", "ND1", "T12", "T63", "CS2", "x1", "BS1.5", "ch27", "uhf27", "bs-1",
        ] {
            assert!(bad.parse::<Channel>().is_err(), "{bad}");
        }
        assert_eq!(Channel::T(27).to_string(), "T27");
        assert_eq!(Channel::Bs(1).label(), " (BS1 IF)");
        assert_eq!(Channel::Nd(2).label(), " (ND2 IF)");
        assert_eq!(Channel::T(27).label(), " (T27)");
        assert!(Channel::Bs(1).is_satellite() && !Channel::T(27).is_satellite());
    }

    #[test]
    fn bare_number_is_terrestrial() {
        assert_eq!("44".parse::<Channel>().unwrap(), Channel::T(44));
        assert_eq!(" 13 ".parse::<Channel>().unwrap(), Channel::T(13));
        assert!("12".parse::<Channel>().is_err());
        assert!("63".parse::<Channel>().is_err());
    }

    #[test]
    fn freq_and_count_syntax() {
        let f = |s: &str| s.parse::<Freq>().unwrap();
        assert_eq!(
            f("1049480000"),
            Freq {
                hz: 1_049_480_000,
                channel: None
            }
        );
        assert_eq!(f("1049.48M").hz, 1_049_480_000);
        assert_eq!(f("557.142857MHz").hz, 557_142_857);
        assert_eq!(f("2.4G").hz, 2_400_000_000);
        assert_eq!(
            f("BS1"),
            Freq {
                hz: 1_049_480_000,
                channel: Some(Channel::Bs(1))
            }
        );
        assert_eq!(f("t27").hz, 557_142_857);
        assert_eq!(f("nd2").channel, Some(Channel::Nd(2)));
        for bad in ["0", "-5", "1x", "BS2", "nan", "ch27"] {
            assert!(bad.parse::<Freq>().is_err(), "{bad}");
        }
        assert_eq!(parse_hz("57.72M").unwrap(), 57.72e6);
        assert_eq!(parse_hz("61440000sps").unwrap(), 61.44e6);
        assert_eq!(parse_hz("44kHz").unwrap(), 44e3);
        assert!(parse_hz("M").is_err());
        assert_eq!(parse_count("4Mi").unwrap(), 4_194_304);
        assert_eq!(parse_count("512Ki").unwrap(), 524_288);
        assert_eq!(parse_count("1000").unwrap(), 1000);
        assert!(parse_count("1.5Mi").is_err());
    }
}
