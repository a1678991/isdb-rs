//! Sample formats of IQ files.

use crate::{C32, Error, Result};
use std::fmt;
use std::str::FromStr;

/// How the samples of an IQ file are stored: interleaved I, Q components,
/// little-endian where it matters.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SampleFormat {
    /// Signed 8-bit components; 128 is full scale (HackRF `hackrf_transfer`).
    Cs8,
    /// Signed 16-bit little-endian components; `full_scale` is the value
    /// that decodes to 1.0. AD936x samples read through libiio are 12 bits
    /// sign-extended (`2048`); maia-sdr's 16-bit recordings and most other
    /// tools use the whole range (`32768`).
    Cs16 {
        /// The component value of 1.0 (> 0).
        full_scale: f32,
    },
    /// 32-bit float little-endian components (GNU Radio complex).
    Cf32,
}

impl SampleFormat {
    /// The libiio AD936x RX format: 12-bit samples sign-extended in `i16`.
    pub const AD936X_CS16: Self = Self::Cs16 { full_scale: 2048.0 };

    /// Bytes per complex sample.
    pub fn bytes_per_sample(&self) -> usize {
        match self {
            Self::Cs8 => 2,
            Self::Cs16 { .. } => 4,
            Self::Cf32 => 8,
        }
    }

    /// The SigMF `core:datatype` (`ci8`, `ci16_le`, `cf32_le`). The full
    /// scale of `ci16_le` is carried separately (`isdb:full_scale`).
    pub fn sigmf_datatype(&self) -> &'static str {
        match self {
            Self::Cs8 => "ci8",
            Self::Cs16 { .. } => "ci16_le",
            Self::Cf32 => "cf32_le",
        }
    }

    /// The format of SigMF `core:datatype` `datatype`, with `full_scale`
    /// for `ci16_le` (default 32768, the type's own range).
    pub fn from_sigmf(datatype: &str, full_scale: Option<f32>) -> Result<Self> {
        Ok(match datatype {
            "ci8" | "ci8_le" | "ci8_be" => Self::Cs8,
            "ci16_le" => {
                let full_scale = full_scale.unwrap_or(32768.0);
                if !(full_scale.is_finite() && full_scale > 0.0) {
                    return Err(Error::FullScale { full_scale });
                }
                Self::Cs16 { full_scale }
            }
            "cf32_le" => Self::Cf32,
            d => {
                return Err(Error::Datatype {
                    datatype: d.to_owned(),
                });
            }
        })
    }

    /// The component value of 1.0.
    pub fn full_scale(&self) -> f32 {
        match self {
            Self::Cs8 => 128.0,
            Self::Cs16 { full_scale } => *full_scale,
            Self::Cf32 => 1.0,
        }
    }

    /// Decode the whole samples in `bytes` and append them to `out`
    /// (a trailing partial sample is ignored).
    pub fn decode(&self, bytes: &[u8], out: &mut Vec<C32>) {
        match self {
            Self::Cs8 => out.extend(
                bytes
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .map(|&[i, q]| C32::new(f32::from(i as i8), f32::from(q as i8)) / 128.0),
            ),
            Self::Cs16 { full_scale } => {
                let k = 1.0 / full_scale;
                out.extend(bytes.as_chunks::<4>().0.iter().map(|&[a, b, c, d]| {
                    C32::new(
                        f32::from(i16::from_le_bytes([a, b])) * k,
                        f32::from(i16::from_le_bytes([c, d])) * k,
                    )
                }));
            }
            Self::Cf32 => out.extend(bytes.as_chunks::<8>().0.iter().map(
                |&[a, b, c, d, e, f, g, h]| {
                    C32::new(
                        f32::from_le_bytes([a, b, c, d]),
                        f32::from_le_bytes([e, f, g, h]),
                    )
                },
            )),
        }
    }
}

impl fmt::Display for SampleFormat {
    /// `cs8`, `cs16` (full scale 32768), `cs16/2048`, `cf32`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Cs8 => f.write_str("cs8"),
            Self::Cs16 { full_scale } if *full_scale == 32768.0 => f.write_str("cs16"),
            Self::Cs16 { full_scale } => write!(f, "cs16/{full_scale}"),
            Self::Cf32 => f.write_str("cf32"),
        }
    }
}

impl FromStr for SampleFormat {
    type Err = Error;

    /// `cs8`, `cs16` (full scale 32768), `cs16/FULLSCALE` (e.g. `cs16/2048`
    /// for libiio AD936x samples), `cf32`.
    fn from_str(s: &str) -> Result<Self> {
        let s = s.trim().to_ascii_lowercase();
        match s.as_str() {
            "cs8" => Ok(Self::Cs8),
            "cs16" => Ok(Self::Cs16 {
                full_scale: 32768.0,
            }),
            "cf32" => Ok(Self::Cf32),
            _ => {
                let fs = s
                    .strip_prefix("cs16/")
                    .and_then(|v| v.parse::<f32>().ok())
                    .ok_or_else(|| Error::FormatName { name: s.clone() })?;
                Self::from_sigmf("ci16_le", Some(fs))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_each_format() {
        let mut v = Vec::new();
        SampleFormat::Cs8.decode(&[64, 0xc0, 1], &mut v);
        assert_eq!(v, vec![C32::new(0.5, -0.5)]);
        v.clear();
        SampleFormat::Cs16 {
            full_scale: 32768.0,
        }
        .decode(&[0, 0x40, 0, 0x80], &mut v);
        assert_eq!(v, vec![C32::new(0.5, -1.0)]);
        v.clear();
        let mut b = 0.25f32.to_le_bytes().to_vec();
        b.extend((-2.0f32).to_le_bytes());
        SampleFormat::Cf32.decode(&b, &mut v);
        assert_eq!(v, vec![C32::new(0.25, -2.0)]);
    }

    #[test]
    fn names_and_sigmf_types() {
        for (s, f) in [
            ("cs8", SampleFormat::Cs8),
            (
                "cs16",
                SampleFormat::Cs16 {
                    full_scale: 32768.0,
                },
            ),
            ("cs16/2048", SampleFormat::AD936X_CS16),
            ("cf32", SampleFormat::Cf32),
        ] {
            assert_eq!(s.parse::<SampleFormat>().unwrap(), f);
            assert_eq!(f.to_string(), s);
            let back = SampleFormat::from_sigmf(f.sigmf_datatype(), Some(f.full_scale())).unwrap();
            assert_eq!(back, f, "{s}");
        }
        assert!(matches!(
            "CS12".parse::<SampleFormat>(),
            Err(Error::FormatName { name }) if name == "cs12"
        ));
        assert!(matches!(
            "cs16/0".parse::<SampleFormat>(),
            Err(Error::FullScale { full_scale: 0.0 })
        ));
        assert!(matches!(
            SampleFormat::from_sigmf("ru8", None),
            Err(Error::Datatype { datatype }) if datatype == "ru8"
        ));
        assert_eq!(
            SampleFormat::from_sigmf("ci16_le", None)
                .unwrap()
                .full_scale(),
            32768.0
        );
    }
}
