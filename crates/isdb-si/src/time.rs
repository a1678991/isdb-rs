//! Time coding of ARIB SI: MJD + BCD date/time, BCD durations and offsets.
//!
//! * `JST_time` / `start_time` / `time_of_change` (ARIB STD-B10 Part 2
//!   §5.2.7, §5.2.8, §5.2.9, §6.2.25): 40 bits, "16 bits giving the 16 LSBs
//!   of MJD followed by 24 bits coded as 6 digits in 4-bit BCD", in Japan
//!   Standard Time ("UTC + 9" hours, §3.1).  All bits '1' means undefined
//!   (§5.2.7 start_time).
//! * `duration` (§5.2.7): 24 bits, 6 BCD digits hh mm ss; all bits '1'
//!   means undefined.
//! * `local_time_offset` / `next_time_offset` (§6.2.25): 16 bits, 4 BCD
//!   digits hh mm.
//! * MJD ⇄ Y/M/D and week conversions: the formulas of ARIB STD-B10 Part 2
//!   Annex C (informative), evaluated in exact rational arithmetic
//!   (integers), "effective from March 1, 1900 to February 28, 2100".
//!
//! The raw field values are kept verbatim ([`MjdTime`], [`BcdDuration`],
//! [`BcdOffset`]) so decoding is lossless; conversions to a civil date-time
//! are separate and fallible.

use isdb_ts::{Error, Result};

/// MJD of 1900-03-01, first day of the Annex C validity range
/// (ARIB STD-B10 Part 2 Annex C, Note).
pub const MJD_MIN: u32 = 15_079;
/// MJD of 2100-02-28, last day of the Annex C validity range
/// (ARIB STD-B10 Part 2 Annex C, Note).
pub const MJD_MAX: u32 = 88_127;

/// Seconds JST is ahead of UTC: "JST (Japanese Standard Time): 'UTC +9'
/// hour" (ARIB STD-B10 Part 2 §3.1).
pub const JST_OFFSET_SECONDS: i64 = 9 * 3600;

/// A Gregorian calendar date.  ARIB STD-B10 Part 2 Annex C uses
/// `Y` = year − 1900, `M` = 1..12, `D` = 1..31; `year` here is the full year.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Date {
    /// Full year (e.g. 2026); Annex C `Y` is `year - 1900`.
    pub year: i32,
    /// Month, January = 1 … December = 12 (Annex C `M`).
    pub month: u8,
    /// Day of month 1..31 (Annex C `D`).
    pub day: u8,
}

/// A civil date and time of day (time zone implied by context: JST for
/// values read straight from SI, UTC after [`MjdTime::to_utc`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DateTime {
    /// Calendar date.
    pub date: Date,
    /// Hour 0..23.
    pub hour: u8,
    /// Minute 0..59.
    pub minute: u8,
    /// Second 0..59.
    pub second: u8,
}

impl std::fmt::Display for DateTime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
            self.date.year, self.date.month, self.date.day, self.hour, self.minute, self.second
        )
    }
}

/// Annex C (a): Y, M, D from MJD.  `None` outside the Annex validity range
/// [`MJD_MIN`]..=[`MJD_MAX`].
///
/// ARIB STD-B10 Part 2 Annex C (a):
/// `Y' = int[(MJD − 15078.2) / 365.25]`,
/// `M' = int{[MJD − 14956.1 − int(Y' × 365.25)] / 30.6001}`,
/// `D = MJD − 14956 − int(Y' × 365.25) − int(M' × 30.6001)`,
/// `K = 1 if M' ∈ {14, 15} else 0`, `Y = Y' + K`, `M = M' − 1 − K × 12`.
pub fn mjd_to_date(mjd: u32) -> Option<Date> {
    if !(MJD_MIN..=MJD_MAX).contains(&mjd) {
        return None;
    }
    let mjd = i64::from(mjd);
    // (MJD − 15078.2) / 365.25 = (100·MJD − 1507820) / 36525 (positive here)
    let yp = (100 * mjd - 1_507_820).div_euclid(36_525);
    let iy = (yp * 36_525).div_euclid(100); // int(Y' × 365.25)
    // (X − 0.1) / 30.6001 = (10000·X − 1000) / 306001 with X = MJD − 14956 − iy
    let x = mjd - 14_956 - iy;
    let mp = (10_000 * x - 1_000).div_euclid(306_001);
    let im = (mp * 306_001).div_euclid(10_000); // int(M' × 30.6001)
    let d = mjd - 14_956 - iy - im;
    let k = if mp == 14 || mp == 15 { 1 } else { 0 };
    let y = yp + k;
    let m = mp - 1 - k * 12;
    Some(Date {
        year: 1900 + y as i32,
        month: m as u8,
        day: d as u8,
    })
}

/// Annex C (b): MJD from Y, M, D.  `None` if the date is not a valid
/// Gregorian date or lies outside the Annex validity range.
///
/// ARIB STD-B10 Part 2 Annex C (b): `L = 1 if M ∈ {1, 2} else 0`,
/// `MJD = 14956 + D + int[(Y − L) × 365.25] + int[(M + 1 + L × 12) × 30.6001]`.
pub fn date_to_mjd(date: Date) -> Option<u32> {
    if !(1..=12).contains(&date.month)
        || date.day == 0
        || date.day > days_in_month(date.year, date.month)
    {
        return None;
    }
    let y = i64::from(date.year - 1900);
    let m = i64::from(date.month);
    let l = if m == 1 || m == 2 { 1 } else { 0 };
    let mjd = 14_956
        + i64::from(date.day)
        + ((y - l) * 36_525).div_euclid(100)
        + ((m + 1 + l * 12) * 306_001).div_euclid(10_000);
    if mjd < i64::from(MJD_MIN) || mjd > i64::from(MJD_MAX) {
        return None;
    }
    Some(mjd as u32)
}

/// Gregorian month length (used only to reject impossible dates before
/// applying Annex C (b), which would otherwise silently roll over).
pub fn days_in_month(year: i32, month: u8) -> u8 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if (year % 4 == 0 && year % 100 != 0) || year % 400 == 0 => 29,
        2 => 28,
        _ => 0,
    }
}

/// Annex C (c): week day, Monday = 1 … Sunday = 7:
/// `WD = [(MJD + 2) mod 7] + 1` (ARIB STD-B10 Part 2 Annex C (c)).
pub fn weekday(mjd: u32) -> u8 {
    ((u64::from(mjd) + 2) % 7) as u8 + 1
}

/// Annex C (d): MJD from week-number year `wy` (since 1900), ISO week
/// number `wn` and week day `wd`:
/// `MJD = 15012 + WD + 7 × {WN + int[(WY × 1461 / 28) + 0.41]}`
/// (ARIB STD-B10 Part 2 Annex C (d)).
pub fn week_to_mjd(wy: i64, wn: i64, wd: i64) -> i64 {
    // WY × 1461 / 28 + 0.41 = (146100·WY + 1148) / 2800
    15_012 + wd + 7 * (wn + (146_100 * wy + 1_148).div_euclid(2_800))
}

/// Annex C (e): week-number year `WY` (since 1900) and week number `WN`
/// from MJD (ARIB STD-B10 Part 2 Annex C (e)):
/// `W = int[(MJD / 7) − 2144.64]`, `WY = int[(W × 28 / 1461) − 0.0079]`,
/// `WN = W − int[(WY × 1461 / 28) + 0.41]`.  Returns `(W, WY, WN)`.
pub fn mjd_to_week(mjd: u32) -> (i64, i64, i64) {
    let mjd = i64::from(mjd);
    // MJD/7 − 2144.64 = (100·MJD − 1501248) / 700
    let w = (100 * mjd - 1_501_248).div_euclid(700);
    // W·28/1461 − 0.0079 = (280000·W − 115419) / 14610000
    let wy = (280_000 * w - 115_419).div_euclid(14_610_000);
    let wn = w - (146_100 * wy + 1_148).div_euclid(2_800);
    (w, wy, wn)
}

/// Decode one 4-bit-BCD byte (two digits) to 0..=99, `None` if a nibble > 9.
pub fn bcd_to_u8(b: u8) -> Option<u8> {
    let (hi, lo) = (b >> 4, b & 0x0F);
    (hi <= 9 && lo <= 9).then_some(hi * 10 + lo)
}

/// Encode 0..=99 as two 4-bit BCD digits.
pub fn u8_to_bcd(v: u8) -> Result<u8> {
    if v > 99 {
        return Err(Error::Overflow {
            what: "BCD digit pair",
            value: u64::from(v),
            bits: 8,
        });
    }
    Ok(((v / 10) << 4) | (v % 10))
}

/// Decode 6 BCD digits `hhmmss` packed in the low 24 bits.
fn bcd_hms(v: u32) -> Option<(u8, u8, u8)> {
    Some((
        bcd_to_u8((v >> 16) as u8)?,
        bcd_to_u8((v >> 8) as u8)?,
        bcd_to_u8(v as u8)?,
    ))
}

fn hms_bcd(h: u8, m: u8, s: u8) -> Result<u32> {
    Ok((u32::from(u8_to_bcd(h)?) << 16)
        | (u32::from(u8_to_bcd(m)?) << 8)
        | u32::from(u8_to_bcd(s)?))
}

/// 40-bit MJD + BCD date/time field (`JST_time`, `start_time`,
/// `time_of_change`, PCAT `start_time`), ARIB STD-B10 Part 2 §5.2.7,
/// §5.2.8, §5.2.9, §6.2.25; Annex C.  Kept raw (lossless); the time zone is
/// JST.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct MjdTime {
    /// 16 LSBs of the Modified Julian Date (Japan time).
    pub mjd: u16,
    /// 24 bits: hour, minute, second as 6 BCD digits.
    pub bcd_hms: u32,
}

impl MjdTime {
    /// All bits '1': "If the start time is undefined … all bits of the
    /// field are set to '1'" (ARIB STD-B10 Part 2 §5.2.7).
    pub const UNDEFINED: MjdTime = MjdTime {
        mjd: 0xFFFF,
        bcd_hms: 0xFF_FFFF,
    };

    /// From the 40-bit field value (low 40 bits of `v`).
    pub fn from_u40(v: u64) -> Self {
        Self {
            mjd: (v >> 24) as u16,
            bcd_hms: (v & 0xFF_FFFF) as u32,
        }
    }

    /// The 40-bit field value.
    pub fn to_u40(self) -> u64 {
        (u64::from(self.mjd) << 24) | (u64::from(self.bcd_hms) & 0xFF_FFFF)
    }

    /// The 40-bit field value; errors if `bcd_hms` exceeds 24 bits.
    pub fn checked_u40(self) -> Result<u64> {
        if self.bcd_hms > 0xFF_FFFF {
            return Err(Error::Overflow {
                what: "MJD time (BCD part)",
                value: u64::from(self.bcd_hms),
                bits: 24,
            });
        }
        Ok(self.to_u40())
    }

    /// True when every bit is '1' (undefined, §5.2.7).
    pub fn is_undefined(self) -> bool {
        self == Self::UNDEFINED
    }

    /// `(hour, minute, second)` if every BCD nibble is a decimal digit.
    pub fn hms(self) -> Option<(u8, u8, u8)> {
        bcd_hms(self.bcd_hms)
    }

    /// The JST civil date-time, or `None` if undefined, a BCD nibble is not
    /// a digit, the time of day is out of range, or the MJD is outside the
    /// Annex C validity range.
    pub fn to_jst(self) -> Option<DateTime> {
        if self.is_undefined() {
            return None;
        }
        let (hour, minute, second) = self.hms()?;
        if hour > 23 || minute > 59 || second > 59 {
            return None;
        }
        Some(DateTime {
            date: mjd_to_date(u32::from(self.mjd))?,
            hour,
            minute,
            second,
        })
    }

    /// Encode a JST civil date-time.  Errors if the date is invalid, outside
    /// the Annex C range, or its MJD does not fit the 16-bit field (§5.2.8
    /// Note: "present date can be indicated up to April 22, 2038").
    pub fn from_jst(dt: DateTime) -> Result<Self> {
        if dt.hour > 23 || dt.minute > 59 || dt.second > 59 {
            return Err(Error::invalid(
                "JST_time",
                format!("time of day {dt} out of range"),
            ));
        }
        let mjd = date_to_mjd(dt.date).ok_or_else(|| {
            Error::invalid("JST_time", format!("date {dt} outside Annex C range"))
        })?;
        let mjd = u16::try_from(mjd).map_err(|_| Error::Overflow {
            what: "MJD",
            value: u64::from(mjd),
            bits: 16,
        })?;
        Ok(Self {
            mjd,
            bcd_hms: hms_bcd(dt.hour, dt.minute, dt.second)?,
        })
    }

    /// Seconds since MJD 0 00:00:00 JST.
    fn jst_seconds(self) -> Option<i64> {
        let dt = self.to_jst()?;
        Some(
            i64::from(self.mjd) * 86_400
                + i64::from(dt.hour) * 3600
                + i64::from(dt.minute) * 60
                + i64::from(dt.second),
        )
    }

    /// The same instant in UTC (JST − 9 h, §3.1).
    pub fn to_utc(self) -> Option<DateTime> {
        let s = self.jst_seconds()? - JST_OFFSET_SECONDS;
        let mjd = u32::try_from(s.div_euclid(86_400)).ok()?;
        let tod = s.rem_euclid(86_400);
        Some(DateTime {
            date: mjd_to_date(mjd)?,
            hour: (tod / 3600) as u8,
            minute: (tod / 60 % 60) as u8,
            second: (tod % 60) as u8,
        })
    }

    /// Encode a UTC instant as JST (UTC + 9 h, §3.1).
    pub fn from_utc(dt: DateTime) -> Result<Self> {
        let mjd = date_to_mjd(dt.date).ok_or_else(|| {
            Error::invalid("JST_time", format!("date {dt} outside Annex C range"))
        })?;
        let s = i64::from(mjd) * 86_400
            + i64::from(dt.hour) * 3600
            + i64::from(dt.minute) * 60
            + i64::from(dt.second)
            + JST_OFFSET_SECONDS;
        let jmjd = s.div_euclid(86_400) as u32;
        let tod = s.rem_euclid(86_400);
        let date = mjd_to_date(jmjd)
            .ok_or_else(|| Error::invalid("JST_time", "date outside Annex C range"))?;
        Self::from_jst(DateTime {
            date,
            hour: (tod / 3600) as u8,
            minute: (tod / 60 % 60) as u8,
            second: (tod % 60) as u8,
        })
    }
}

/// 24-bit BCD duration `hhmmss` (ARIB STD-B10 Part 2 §5.2.7 `duration`,
/// §5.2.12 PCAT `duration`): "6 digits, 4-bit BCD = 24 bit"; all bits '1'
/// when not defined.  Kept raw.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct BcdDuration(pub u32);

impl BcdDuration {
    /// All bits '1' (§5.2.7: "When duration is not defined … all bits in
    /// this field are set to '1'").
    pub const UNDEFINED: BcdDuration = BcdDuration(0xFF_FFFF);

    /// True when undefined.
    pub fn is_undefined(self) -> bool {
        self == Self::UNDEFINED
    }

    /// `(hours, minutes, seconds)` if all nibbles are decimal digits and
    /// minutes/seconds are below 60.
    pub fn hms(self) -> Option<(u8, u8, u8)> {
        let (h, m, s) = bcd_hms(self.0)?;
        (m < 60 && s < 60).then_some((h, m, s))
    }

    /// Total seconds, if defined and valid.
    pub fn seconds(self) -> Option<u32> {
        let (h, m, s) = self.hms()?;
        Some(u32::from(h) * 3600 + u32::from(m) * 60 + u32::from(s))
    }

    /// Encode `hours:minutes:seconds` (hours 0..=99).
    pub fn from_hms(h: u8, m: u8, s: u8) -> Result<Self> {
        if m > 59 || s > 59 {
            return Err(Error::invalid(
                "duration",
                format!("{h:02}:{m:02}:{s:02} out of range"),
            ));
        }
        Ok(Self(hms_bcd(h, m, s)?))
    }
}

/// 16-bit BCD time offset `hhmm` (ARIB STD-B10 Part 2 §6.2.25
/// `local_time_offset` / `next_time_offset`: "4 digits in 4-bit BCD in the
/// order hour tens, hour, minute tens, and minutes").  Kept raw.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct BcdOffset(pub u16);

impl BcdOffset {
    /// `(hours, minutes)` if every nibble is a decimal digit.
    pub fn hm(self) -> Option<(u8, u8)> {
        Some((bcd_to_u8((self.0 >> 8) as u8)?, bcd_to_u8(self.0 as u8)?))
    }

    /// Encode `hours:minutes` (each 0..=99).
    pub fn from_hm(h: u8, m: u8) -> Result<Self> {
        Ok(Self(
            (u16::from(u8_to_bcd(h)?) << 8) | u16::from(u8_to_bcd(m)?),
        ))
    }
}
