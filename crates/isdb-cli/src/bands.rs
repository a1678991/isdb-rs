//! Channel tables shared by every command that tunes a radio: Japanese
//! UHF terrestrial channels (ISDB-T) and the satellite IF of BS and 110° CS
//! transponders (ISDB-S, ISDB-S3), plus the `--freq` syntax that accepts
//! either a frequency or a channel name.
//!
//! None of these frequency plans is in the local specifications
//! (SPECS.md §1.2): the UHF plan is the Radio Act ordinance's, the
//! satellite ones are ITU-R Radio Regulations channel frequencies converted
//! by a Japanese dual-polarisation LNB. The LNB has one LO per circular
//! polarisation: 10.678 GHz for right-hand (BS1..BS23 odd, ND2..ND24 even;
//! IF 1032–2071 MHz) and 9.505 GHz for left-hand (BS2..BS24 even,
//! ND1..ND23 odd; IF 2224–3224 MHz). Both LOs and every IF carrier are
//! listed in MIC Ordinance No. 95 of 2011 (有線一般放送の品質に関する技術基準を定める省令,
//! Art. 18 and 19), which is not one of the local specifications either.
//! The right-hand IF centres were checked against the spectrum of a cable
//! feed's satellite IF, and the RF and polarisation of BS7, 11, 14 and 17
//! against the TLV-NIT on air (SPECS.md §1.2, VERIFICATION.md "Satellite IF
//! survey"); the lab feed carries no left-hand IF, so no left-hand IF has
//! been received.

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

/// LO of the right-hand-circular BS / 110° CS LNB: IF = RF − 10.678 GHz
/// (MIC Ordinance No. 95 of 2011, Art. 19 item 4: "右旋円偏波 … 一〇・六七八ギガヘルツの局部発振周波数").
pub const LNB_LO_RIGHT_HZ: u64 = 10_678_000_000;

/// LO of the left-hand-circular BS / 110° CS LNB: IF = RF − 9.505 GHz
/// (MIC Ordinance No. 95 of 2011, Art. 19 item 4: "左旋円偏波 … 九・五〇五ギガヘルツの局部発振周波数"),
/// which puts the left-hand band at 2224–3224 MHz, above the right-hand
/// one.
pub const LNB_LO_LEFT_HZ: u64 = 9_505_000_000;

/// The circular polarisation of a BS / 110° CS channel, which decides the
/// LNB LO that converts it to the satellite IF.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Polarisation {
    /// Right-hand circular: odd BS, even ND channels; LO [`LNB_LO_RIGHT_HZ`].
    Right,
    /// Left-hand circular: even BS, odd ND channels; LO [`LNB_LO_LEFT_HZ`].
    Left,
}

impl Polarisation {
    /// The LNB LO for this polarisation.
    pub fn lnb_lo_hz(self) -> u64 {
        match self {
            Self::Right => LNB_LO_RIGHT_HZ,
            Self::Left => LNB_LO_LEFT_HZ,
        }
    }

    /// `right` / `left`: the SigMF `isdb:polarisation` value.
    pub fn name(self) -> &'static str {
        match self {
            Self::Right => "right",
            Self::Left => "left",
        }
    }
}

/// RF centre of BS channel `n` (1..=24): 11 727.48 + 19.18·(n − 1) MHz.
/// Odd channels are right-hand circular, even ones left-hand.
pub fn bs_rf_hz(n: u32) -> Result<u64> {
    if !(1..=24).contains(&n) {
        bail!("channel BS{n}: outside BS1..BS24 (odd right-hand, even left-hand circular)");
    }
    Ok(11_727_480_000 + 19_180_000 * (u64::from(n) - 1))
}

/// RF centre of 110° CS channel ND`n` (1..=24): 12 251 + 20·n MHz
/// (ND1 = 12.271 GHz, 20 MHz apart). Even channels are right-hand
/// circular, odd ones left-hand.
pub fn nd_rf_hz(n: u32) -> Result<u64> {
    if !(1..=24).contains(&n) {
        bail!(
            "channel ND{n}: outside ND1..ND24 (even right-hand, odd left-hand circular; 110° CS)"
        );
    }
    Ok(12_251_000_000 + 20_000_000 * u64::from(n))
}

/// A named channel, written `T27`, `BS1` or `ND2` (case-insensitive when
/// parsed; a bare number is a terrestrial channel).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Channel {
    /// Japanese UHF terrestrial channel 13..62 (`T27`).
    T(u32),
    /// BS channel 1..24 (`BS1`; odd right-hand, even left-hand circular),
    /// at its satellite IF.
    Bs(u32),
    /// 110° CS channel 1..24 (`ND2`; even right-hand, odd left-hand
    /// circular), at its satellite IF.
    Nd(u32),
}

impl Channel {
    /// The frequency a radio tunes to: the UHF centre, or the satellite
    /// IF centre (RF − the LNB LO of the channel's polarisation).
    pub fn tune_hz(&self) -> Result<u64> {
        let rf = self.rf_hz()?;
        Ok(match self.lnb_lo_hz() {
            Some(lo) => rf - lo,
            None => rf,
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

    /// The circular polarisation of a satellite channel (`None` for UHF):
    /// odd BS and even ND channels are right-hand, the others left-hand.
    pub fn polarisation(&self) -> Option<Polarisation> {
        let right = match *self {
            Self::T(_) => return None,
            Self::Bs(n) => !n.is_multiple_of(2),
            Self::Nd(n) => n.is_multiple_of(2),
        };
        Some(if right {
            Polarisation::Right
        } else {
            Polarisation::Left
        })
    }

    /// The LNB LO that converts a satellite channel to its IF (`None` for
    /// UHF).
    pub fn lnb_lo_hz(&self) -> Option<u64> {
        self.polarisation().map(Polarisation::lnb_lo_hz)
    }

    /// ` (T27)`, ` (BS1 IF)`, ` (ND2 IF)`; a left-hand-circular channel adds
    /// it: ` (BS14 IF, left-hand)`.
    pub fn label(&self) -> String {
        match self.polarisation() {
            None => format!(" ({self})"),
            Some(Polarisation::Right) => format!(" ({self} IF)"),
            Some(Polarisation::Left) => format!(" ({self} IF, left-hand)"),
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
        const WANT: &str = "T13..T62 (UHF), BS1..BS24 or ND1..ND24";
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
            // f64 -> i128 saturates; try_from then range-checks the value
            #[allow(clippy::cast_possible_truncation)]
            let hz = parse_hz(s)?.round() as i128;
            let hz = u64::try_from(hz)
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
        // Right-hand (LO 10.678 GHz): BSn IF = 1049.48 + 19.18·(n − 1) MHz,
        // odd n; the IF carriers of MIC Ordinance No. 95 of 2011, Art. 18.
        let bs = |n| Channel::Bs(n).tune_hz().unwrap();
        assert_eq!(bs(1), 1_049_480_000);
        assert_eq!(bs(3), 1_087_840_000);
        assert_eq!(bs(23), 1_471_440_000);
        assert_eq!(Channel::Bs(1).rf_hz().unwrap(), 11_727_480_000);
        assert!(Channel::Bs(0).tune_hz().is_err());
        assert!(Channel::Bs(25).tune_hz().is_err());
        // NDn IF = 1613 + 20·(n − 2) MHz, even n.
        let nd = |n| Channel::Nd(n).tune_hz().unwrap();
        assert_eq!(nd(2), 1_613_000_000);
        assert_eq!(nd(24), 2_053_000_000);
        assert_eq!(Channel::Nd(2).rf_hz().unwrap(), 12_291_000_000);
        assert!(Channel::Nd(0).tune_hz().is_err());
        assert!(Channel::Nd(25).tune_hz().is_err());
        assert!(Channel::Nd(26).tune_hz().is_err());
    }

    #[test]
    fn spec_ordinance_95_art18_left_hand_if() {
        // Left-hand (LO 9.505 GHz), even BS: the same RF formula. BS14
        // (NHK BS8K) is at 11 976.82 MHz, as the TLV-NIT on air says
        // (TLV stream 0xB0E0, 11 976 820 kHz, polarisation '10' = left).
        let bs14 = Channel::Bs(14);
        assert_eq!(bs14.rf_hz().unwrap(), 11_976_820_000);
        assert_eq!(bs14.tune_hz().unwrap(), 2_471_820_000);
        assert_eq!(bs14.polarisation(), Some(Polarisation::Left));
        assert_eq!(bs14.lnb_lo_hz(), Some(9_505_000_000));
        // The ends: BS2 is the ordinance's first left-hand BS carrier
        // (2 241.66 MHz) and BS22 its last (2 625.26 MHz); BS24 is in
        // the 2224–2681 MHz receiver band but not in the ordinance's list.
        let bs = |n| Channel::Bs(n).tune_hz().unwrap();
        assert_eq!(Channel::Bs(2).rf_hz().unwrap(), 11_746_660_000);
        assert_eq!(bs(2), 2_241_660_000);
        assert_eq!(bs(22), 2_625_260_000);
        assert_eq!(Channel::Bs(24).rf_hz().unwrap(), 12_168_620_000);
        assert_eq!(bs(24), 2_663_620_000);
        // Every left-hand BS carrier of Art. 18 (2 241.66 … 2 625.26 MHz,
        // 38.36 MHz apart) is an even channel's IF.
        for (k, n) in (2..=22).step_by(2).enumerate() {
            assert_eq!(bs(n), 2_241_660_000 + 38_360_000 * k as u64, "BS{n}");
        }
        // Left-hand 110° CS, odd ND: Art. 18's 2 766 … 3 206 MHz.
        let nd = |n| Channel::Nd(n).tune_hz().unwrap();
        assert_eq!(Channel::Nd(1).rf_hz().unwrap(), 12_271_000_000);
        assert_eq!(nd(1), 2_766_000_000);
        assert_eq!(nd(9), 2_926_000_000);
        assert_eq!(nd(23), 3_206_000_000);
        assert_eq!(Channel::Nd(1).polarisation(), Some(Polarisation::Left));
        // Odd BS and even ND stay right-hand.
        assert_eq!(Channel::Bs(1).polarisation(), Some(Polarisation::Right));
        assert_eq!(Channel::Bs(1).lnb_lo_hz(), Some(10_678_000_000));
        assert_eq!(Channel::Nd(2).polarisation(), Some(Polarisation::Right));
        assert_eq!(Channel::T(27).polarisation(), None);
        assert_eq!(Channel::T(27).lnb_lo_hz(), None);
        // The left-hand IF band lies above the right-hand one.
        let right = (1..=24).map(Channel::Bs).chain((1..=24).map(Channel::Nd));
        for c in right {
            let f = c.tune_hz().unwrap();
            match c.polarisation().unwrap() {
                Polarisation::Right => assert!((1_032_000_000..=2_071_000_000).contains(&f), "{c}"),
                Polarisation::Left => assert!((2_224_000_000..=3_224_000_000).contains(&f), "{c}"),
            }
        }
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
        assert_eq!(
            f("BS14"),
            Freq {
                hz: 2_471_820_000,
                channel: Some(Channel::Bs(14))
            }
        );
        for bad in ["0", "-5", "1x", "BS25", "nan", "ch27"] {
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
