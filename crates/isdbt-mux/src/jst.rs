//! Japan Standard Time wall clock as seconds, and conversion to the
//! MJD + BCD coding of ARIB STD-B10 Annex C (via `isdb_si::time`).

use isdb_si::time::{BcdDuration, Date, DateTime, MjdTime, date_to_mjd, mjd_to_date};

use crate::error::{MuxError, Result};

/// MJD of 1970-01-01 (B10 Annex C: MJD 40587).
const MJD_UNIX_EPOCH: i64 = 40_587;

/// Seconds since 1970-01-01T00:00:00 *JST* (i.e. Unix time + 9 h).
pub type JstSeconds = i64;

/// Current system time as [`JstSeconds`].
pub fn now() -> JstSeconds {
    let unix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    unix + isdb_si::time::JST_OFFSET_SECONDS
}

/// Calendar form of `t`.
pub fn to_datetime(t: JstSeconds) -> Option<DateTime> {
    let days = t.div_euclid(86_400);
    let secs = t.rem_euclid(86_400);
    let mjd = u32::try_from(days + MJD_UNIX_EPOCH).ok()?;
    let date = mjd_to_date(mjd)?;
    Some(DateTime {
        date,
        hour: u8::try_from(secs / 3600).ok()?, // secs < 86 400
        minute: (secs / 60 % 60) as u8,
        second: (secs % 60) as u8,
    })
}

/// [`JstSeconds`] of a calendar JST time.
pub fn from_datetime(dt: DateTime) -> Option<JstSeconds> {
    let mjd = i64::from(date_to_mjd(dt.date)?);
    Some(
        (mjd - MJD_UNIX_EPOCH) * 86_400
            + i64::from(dt.hour) * 3600
            + i64::from(dt.minute) * 60
            + i64::from(dt.second),
    )
}

/// MJD/BCD coding of `t` (B10 Annex C).
pub fn to_mjd_time(t: JstSeconds) -> Result<MjdTime> {
    let dt = to_datetime(t)
        .ok_or_else(|| MuxError::Config(format!("time {t} outside the MJD range")))?;
    MjdTime::from_jst(dt).map_err(MuxError::Encode)
}

/// Parse `YYYY-MM-DDTHH:MM:SS` (a space may replace `T`).
pub fn parse(s: &str) -> Result<JstSeconds> {
    let bad = || MuxError::Config(format!("time {s:?}: want YYYY-MM-DDTHH:MM:SS (JST)"));
    let s = s.trim();
    if s.len() != 19 {
        return Err(bad());
    }
    let b = s.as_bytes();
    if b[4] != b'-'
        || b[7] != b'-'
        || !(b[10] == b'T' || b[10] == b' ')
        || b[13] != b':'
        || b[16] != b':'
    {
        return Err(bad());
    }
    let year = s[0..4].parse::<u32>().map_err(|_| bad())?;
    // two decimal digits always fit a u8
    let num = |r: std::ops::Range<usize>| s[r].parse::<u8>().map_err(|_| bad());
    let dt = DateTime {
        date: Date {
            year: year as i32,
            month: num(5..7)?,
            day: num(8..10)?,
        },
        hour: num(11..13)?,
        minute: num(14..16)?,
        second: num(17..19)?,
    };
    if dt.date.month == 0
        || dt.date.month > 12
        || dt.date.day == 0
        || dt.date.day > 31
        || dt.hour > 23
        || dt.minute > 59
        || dt.second > 59
    {
        return Err(bad());
    }
    from_datetime(dt).ok_or_else(bad)
}

/// Format as `YYYY-MM-DDTHH:MM:SS`.
pub fn format(t: JstSeconds) -> String {
    match to_datetime(t) {
        Some(d) => format!(
            "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}",
            d.date.year, d.date.month, d.date.day, d.hour, d.minute, d.second
        ),
        None => format!("<{t}>"),
    }
}

/// Parse `HH:MM:SS` into seconds.
pub fn parse_hms(s: &str) -> Result<u32> {
    let bad = || MuxError::Config(format!("duration {s:?}: want HH:MM:SS"));
    let f: Vec<&str> = s.trim().split(':').collect();
    if f.len() != 3 {
        return Err(bad());
    }
    let h: u32 = f[0].parse().map_err(|_| bad())?;
    let m: u32 = f[1].parse().map_err(|_| bad())?;
    let sec: u32 = f[2].parse().map_err(|_| bad())?;
    if m > 59 || sec > 59 || h > 99 {
        return Err(bad());
    }
    Ok(h * 3600 + m * 60 + sec)
}

/// Parse `HH:MM` into (h, m).
pub fn parse_hm(s: &str) -> Result<(u8, u8)> {
    let bad = || MuxError::Config(format!("offset {s:?}: want HH:MM"));
    let f: Vec<&str> = s.trim().split(':').collect();
    if f.len() != 2 {
        return Err(bad());
    }
    Ok((
        f[0].parse().map_err(|_| bad())?,
        f[1].parse().map_err(|_| bad())?,
    ))
}

/// BCD duration of `secs` (B10 §5.2.7 "6 digits, 4-bit BCD").
pub fn bcd_duration(secs: u32) -> Result<BcdDuration> {
    let h = u8::try_from(secs / 3600)
        .ok()
        .filter(|&h| h <= 99)
        .ok_or_else(|| MuxError::Config(format!("duration {secs}s exceeds 99:59:59")))?;
    BcdDuration::from_hms(h, (secs / 60 % 60) as u8, (secs % 60) as u8).map_err(MuxError::Encode)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spec_annex_c_example() {
        // B10 §5.2.7 Example 1: 93/10/13 12:45:00 is coded as 0xC079124500.
        let t = parse("1993-10-13T12:45:00").unwrap();
        assert_eq!(to_mjd_time(t).unwrap().to_u40(), 0xC0_7912_4500);
        assert_eq!(format(t), "1993-10-13T12:45:00");
    }

    #[test]
    fn duration_example() {
        // B10 §5.2.7 Example 2: 01:45:30 is coded as 0x014530.
        assert_eq!(
            bcd_duration(parse_hms("01:45:30").unwrap()).unwrap().0,
            0x014530
        );
    }
}
