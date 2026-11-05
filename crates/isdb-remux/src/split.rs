//! What `--split-events` adds to the Matroska form: which parts the recording
//! falls into, and what their files are called. The cutting is in
//! [`crate::mkv::Mkv::finish_parts`].
//!
//! Names come from the broadcast, so what could not be part of a file name, or
//! could lead outside the output directory, is replaced, and an
//! event name too long for a file name is shortened.

use crate::events::Aired;
use isdb_si::time::mjd_to_date;
use std::path::Path;

/// The default file name.
pub const DEFAULT_TEMPLATE: &str = "${START_TIME}-${EVENT_NAME}-${CHANNEL_NAME}.mkv";
/// `${EVENT_NAME}` of a part no event names.
pub const NO_EVENT: &str = "unknown-event";
/// The longest file name of the usual file systems (bytes), less room for the
/// `-N` of a name already taken.
const MAX_NAME: usize = 255 - 4;
/// The MJD of NTP's epoch, 1900-01-01.
const MJD_NTP_EPOCH: u64 = 15_020;
/// JST − UTC (seconds).
const JST_OFFSET: u64 = 9 * 3600;

/// One part of the recording, from `start` (unwrapped PTS) to the next's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Part {
    /// Where it starts: its event's switch.
    pub start: i64,
    /// Its event's name; `None` for a time no event covers.
    pub title: Option<String>,
}

/// The parts of the span from `origin` to `end`: one per event, and one per
/// stretch no event covers, so that no media is left out.
#[must_use]
pub fn plan(aired: &[Aired], origin: i64, end: i64) -> Vec<Part> {
    let mut parts = Vec::with_capacity(aired.len() + 1);
    let mut covered = origin;
    for e in aired {
        if e.start > covered {
            parts.push(Part {
                start: covered,
                title: None,
            });
        }
        parts.push(Part {
            start: e.start,
            title: e.title.clone(),
        });
        covered = e.stop;
    }
    if covered < end || parts.is_empty() {
        parts.push(Part {
            start: covered,
            title: None,
        });
    }
    parts
}

/// A piece of a file name template.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Piece {
    Text(String),
    StartTime,
    EventName,
    ChannelName,
}

/// A file name template: text with `${START_TIME}`, `${EVENT_NAME}` and
/// `${CHANNEL_NAME}`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Template(Vec<Piece>);

impl std::str::FromStr for Template {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, String> {
        if s.contains(['/', '\\', '\0']) {
            return Err("a file name, not a path: the directory is -o's".to_owned());
        }
        if s.is_empty() {
            return Err("an empty file name".to_owned());
        }
        let mut pieces = Vec::new();
        let mut rest = s;
        while let Some(at) = rest.find("${") {
            if at > 0 {
                pieces.push(Piece::Text(rest[..at].to_owned()));
            }
            let after = &rest[at + 2..];
            let Some(close) = after.find('}') else {
                return Err(format!("no closing brace after ${{ in {s:?}"));
            };
            pieces.push(match &after[..close] {
                "START_TIME" => Piece::StartTime,
                "EVENT_NAME" => Piece::EventName,
                "CHANNEL_NAME" => Piece::ChannelName,
                other => {
                    return Err(format!(
                        "unknown ${{{other}}}: START_TIME, EVENT_NAME and CHANNEL_NAME are known"
                    ));
                }
            });
            rest = &after[close + 1..];
        }
        if !rest.is_empty() {
            pieces.push(Piece::Text(rest.to_owned()));
        }
        Ok(Self(pieces))
    }
}

impl Template {
    /// The file name for a part, its event name shortened to fit.
    #[must_use]
    pub fn render(&self, start: &str, event: &str, channel: &str) -> String {
        let (event, channel) = (sanitize(event), sanitize(channel));
        let fixed: usize = self
            .0
            .iter()
            .map(|p| match p {
                Piece::Text(t) => t.len(),
                Piece::StartTime => start.len(),
                Piece::EventName => 0,
                Piece::ChannelName => channel.len(),
            })
            .sum();
        let events = self
            .0
            .iter()
            .filter(|p| matches!(p, Piece::EventName))
            .count()
            .max(1);
        let event = shorten(&event, MAX_NAME.saturating_sub(fixed) / events);
        let mut name = String::new();
        for p in &self.0 {
            name.push_str(match p {
                Piece::Text(t) => t,
                Piece::StartTime => start,
                Piece::EventName => event,
                Piece::ChannelName => &channel,
            });
        }
        // A channel's name alone can still be too long.
        shorten_keeping_extension(&name, MAX_NAME)
    }
}

/// A name from the broadcast as part of a file name: path separators, what
/// common file systems refuse and control characters become `_`.
fn sanitize(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_control() || matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|') {
                '_'
            } else {
                c
            }
        })
        .collect()
}

/// At most `max` bytes of `s`, on a character boundary.
fn shorten(s: &str, max: usize) -> &str {
    let mut end = s.len().min(max);
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// At most `max` bytes of a file name, its extension kept.
fn shorten_keeping_extension(name: &str, max: usize) -> String {
    if name.len() <= max {
        return name.to_owned();
    }
    let extension = Path::new(name)
        .extension()
        .and_then(|e| e.to_str())
        .filter(|e| e.len() < max / 2)
        .map_or_else(String::new, |e| format!(".{e}"));
    format!(
        "{}{extension}",
        shorten(name, max.saturating_sub(extension.len()))
    )
}

/// An NTP timestamp as the JST date and time, to the millisecond, as a file name
/// can hold it: `2026-11-05_02-24-02.698`.
#[must_use]
pub fn start_time(ntp: u64) -> String {
    let seconds = (ntp >> 32) + JST_OFFSET;
    let millis = ((ntp & 0xffff_ffff) * 1000) >> 32;
    let of_day = seconds % 86_400;
    let date = u32::try_from(seconds / 86_400 + MJD_NTP_EPOCH)
        .ok()
        .and_then(mjd_to_date);
    let clock = format!(
        "{:02}-{:02}-{:02}.{millis:03}",
        of_day / 3600,
        of_day / 60 % 60,
        of_day % 60
    );
    date.map_or_else(
        || format!("ntp{}_{clock}", seconds / 86_400),
        |d| format!("{:04}-{:02}-{:02}_{clock}", d.year, d.month, d.day),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn aired(start: i64, stop: i64, title: &str) -> Aired {
        Aired {
            start,
            stop,
            title: Some(title.to_owned()),
            language: None,
            scheduled: false,
        }
    }

    fn part(start: i64, title: Option<&str>) -> Part {
        Part {
            start,
            title: title.map(str::to_owned),
        }
    }

    #[test]
    fn plan_covers_the_span_with_events_and_what_is_between_them() {
        // Running from the start, meeting, then nothing to the end.
        let a = [aired(0, 10, "a"), aired(10, 20, "b")];
        assert_eq!(
            plan(&a, 0, 30),
            vec![part(0, Some("a")), part(10, Some("b")), part(20, None)]
        );
        // Starting late, with a gap between, to the very end.
        let b = [aired(5, 10, "a"), aired(15, 30, "b")];
        assert_eq!(
            plan(&b, 0, 30),
            vec![
                part(0, None),
                part(5, Some("a")),
                part(10, None),
                part(15, Some("b"))
            ]
        );
        // No event at all: one part.
        assert_eq!(plan(&[], 7, 30), vec![part(7, None)]);
    }

    #[test]
    fn the_default_template_names_a_file() {
        let t: Template = DEFAULT_TEMPLATE.parse().unwrap();
        assert_eq!(
            t.render("2026-11-05_02-24-02.698", "番組", "ＮＨＫ　ＢＳＰ４Ｋ"),
            "2026-11-05_02-24-02.698-番組-ＮＨＫ　ＢＳＰ４Ｋ.mkv"
        );
    }

    #[test]
    fn a_template_is_checked_before_a_run() {
        assert!("${START_TIME}.mkv".parse::<Template>().is_ok());
        assert!("plain.mkv".parse::<Template>().is_ok());
        for bad in [
            "",
            "dir/${START_TIME}.mkv",
            "..\\x.mkv",
            "${NOPE}.mkv",
            "${START_TIME.mkv",
        ] {
            assert!(bad.parse::<Template>().is_err(), "{bad:?}");
        }
    }

    #[test]
    fn a_name_from_the_broadcast_stays_in_the_directory() {
        let t: Template = "${EVENT_NAME}.mkv".parse().unwrap();
        let name = t.render("", "../../etc/passwd\n:*?\"<>|\\", "");
        assert!(!name.contains(['/', '\\', '\n', ':', '*', '?', '"', '<', '>', '|']));
        assert_eq!(name, ".._.._etc_passwd_________.mkv");
    }

    #[test]
    fn a_long_event_name_is_shortened_on_a_character_boundary() {
        let t: Template = DEFAULT_TEMPLATE.parse().unwrap();
        let long = "番組".repeat(100);
        let name = t.render("2026-11-05_02-24-02.698", &long, "ＮＨＫ");
        assert!(name.len() <= 255 - 4, "{}", name.len());
        assert!(name.ends_with("-ＮＨＫ.mkv"), "{name}");
        // A very long channel name: still fits, and still a .mkv.
        let name = t.render("t", "e", &"局".repeat(200));
        assert!(name.len() <= 255 - 4, "{}", name.len());
        assert_eq!(
            Path::new(&name).extension().and_then(|e| e.to_str()),
            Some("mkv"),
            "{name}"
        );
    }

    #[test]
    fn start_time_is_jst_to_the_millisecond() {
        // 2026-11-04 17:24:02.5 UTC.
        let seconds = (61_348 - 15_020) * 86_400 + 17 * 3600 + 24 * 60 + 2;
        assert_eq!(
            start_time((seconds << 32) | (1 << 31)),
            "2026-11-05_02-24-02.500"
        );
        // Before the date arithmetic's range: no date, and no panic.
        assert_eq!(start_time(0), "ntp0_09-00-00.000");
    }
}
