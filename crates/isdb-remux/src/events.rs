//! The chapters of the Matroska form: a service's MH-EIT events (B60
//! §7.3.3.9, Table 7-18) on the recording's timeline.
//!
//! An event's `start_time` is JST and the media's clock is the NTP timestamp
//! of the MPT's descriptors, UTC (B60 Chapter 3), so a start is exact on the
//! timeline ([`crate::timing::Clock`]) — the broadcast's *scheduled* time, not
//! a detected change of the picture.
//!
//! The tables are read for the whole run, not at its end: a p/f section drops
//! an event as soon as it has aired. What each event says accumulates by
//! `event_id`, with these rules (`crates/isdb-remux/SPEC-NOTES.md`):
//!
//! - a name comes from any table that has one (the extended schedule tables,
//!   94h–9Bh, repeat an event's time with no MH-short event descriptor);
//! - the time of a p/f section (8Bh) outranks a schedule's, whose copy can lag
//!   behind a change; within a rank the last section read wins;
//! - an event whose start lies inside an event read more recently was replaced
//!   and has no chapter; another overlap ends the earlier chapter at the next
//!   one's start.

use crate::timing::Clock;
use isdb_mkv::Chapter;
use isdb_mmt::message::TableBytes;
use isdb_mmt::table::Eit;
use std::cmp::Reverse;
use std::collections::BTreeMap;

/// 90 kHz ticks per second.
const TICKS: i64 = 90_000;
/// The MJD of 1900-01-01, NTP's epoch (B10 Annex C's date arithmetic).
const MJD_NTP_EPOCH: i64 = 15_020;
/// JST − UTC (B60 §7.4.3.32: "Japan Standard Time (UTC+9 hours)").
const JST_OFFSET: i64 = 9 * 3600;

/// What the tables say of one event.
#[derive(Debug, Clone)]
struct Entry {
    /// Its start (NTP seconds).
    start: u32,
    /// Its duration in seconds; `None` undecided (Table 7-18, all bits '1').
    duration: Option<u32>,
    /// The MH-short event descriptor's name and language.
    name: Option<String>,
    language: Option<String>,
    /// A p/f section described it.
    present_following: bool,
    /// The number of the last section naming it.
    read: u64,
}

/// The events of every service, as the tables have described them so far.
#[derive(Debug, Default)]
pub struct Schedule {
    services: BTreeMap<u16, BTreeMap<u16, Entry>>,
    /// Sections taken.
    read: u64,
}

impl Schedule {
    /// An empty schedule.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// One table of a control message: an MH-EIT section (table id 8Bh p/f,
    /// 8Ch–9Bh schedule) in force updates its service's events. Any other
    /// table, or a section that does not check, changes nothing.
    pub fn feed(&mut self, t: &TableBytes) {
        if !matches!(t.bytes.first(), Some(0x8B..=0x9B)) {
            return;
        }
        let Ok(eit) = Eit::parse(&t.bytes) else {
            return;
        };
        if !eit.header.current {
            return;
        }
        self.read += 1;
        let present_following = eit.header.table_id == 0x8B;
        let events = self.services.entry(eit.header.extension).or_default();
        for e in &eit.events {
            let Some(start) = ntp_seconds(e.start) else {
                continue;
            };
            let entry = events.entry(e.event_id).or_insert(Entry {
                start,
                duration: None,
                name: None,
                language: None,
                present_following,
                read: self.read,
            });
            entry.read = self.read;
            let ranks = present_following || !entry.present_following;
            if ranks {
                entry.start = start;
                entry.duration = e.duration.seconds();
                entry.present_following = present_following;
            }
            if let Some((name, language)) = short_event(e)
                && (ranks || entry.name.is_none())
            {
                entry.name = Some(name);
                entry.language = language;
            }
        }
    }

    /// The chapters of `service` on the timeline from the unwrapped PTS
    /// `origin` to `end`, in time order: each event in that span, clipped to
    /// it, with `clock` placing the event's time.
    #[must_use]
    pub fn chapters(&self, service: u16, clock: Clock, origin: i64, end: i64) -> Vec<Chapter> {
        let Some(events) = self.services.get(&service) else {
            return Vec::new();
        };
        let mut placed: Vec<Placed> = events
            .values()
            .map(|entry| {
                let start = clock.ticks_at(entry.start);
                let stop = entry.duration.map(|d| start + i64::from(d) * TICKS);
                Placed { entry, start, stop }
            })
            // Those over before the origin stay for now: one may be what ends
            // an earlier event of undecided duration.
            .filter(|p| p.start < end)
            .collect();
        // The most recently read first: what replaced an event is kept over
        // it. (The sort is stable, the order of equal reads the event ids'.)
        placed.sort_by_key(|p| Reverse(p.entry.read));
        let mut kept: Vec<Placed> = Vec::with_capacity(placed.len());
        for p in placed {
            if !kept.iter().any(|k| k.covers(p.start)) {
                kept.push(p);
            }
        }
        kept.sort_by_key(|p| p.start);
        kept.iter()
            .enumerate()
            .filter_map(|(i, p)| {
                let start = p.start.max(origin);
                let stop = [p.stop, kept.get(i + 1).map(|n| n.start)]
                    .into_iter()
                    .flatten()
                    .fold(end, std::cmp::Ord::min);
                (stop > start).then(|| Chapter {
                    start_ns: nanoseconds(start - origin),
                    end_ns: Some(nanoseconds(stop - origin)),
                    title: p.entry.name.clone(),
                    language: p.entry.language.clone(),
                })
            })
            .collect()
    }
}

/// An event on the recorder's clock.
struct Placed<'a> {
    entry: &'a Entry,
    start: i64,
    stop: Option<i64>,
}

impl Placed<'_> {
    /// Whether the instant lies in the event (an undecided duration counts
    /// for its start only).
    fn covers(&self, t: i64) -> bool {
        self.start <= t && t < self.stop.unwrap_or(self.start + 1)
    }
}

/// An event's name and, when it is one, its ISO 639 language.
fn short_event(e: &isdb_mmt::table::Event) -> Option<(String, Option<String>)> {
    e.descriptors.iter().find_map(|d| match d {
        isdb_mmt::desc::Descriptor::ShortEvent { language, name, .. } if !name.is_empty() => {
            let language = language
                .iter()
                .all(u8::is_ascii_lowercase)
                .then(|| String::from_utf8_lossy(language).into_owned());
            Some((name.clone(), language))
        }
        _ => None,
    })
}

/// A `start_time` as NTP seconds: `None` when undefined (all bits '1') or not
/// a time. NTP's seconds run on a 32-bit ring and so does the result.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "onto the NTP seconds' own ring"
)]
fn ntp_seconds(start: isdb_si::time::MjdTime) -> Option<u32> {
    start.to_jst()?;
    let (h, m, s) = start.hms()?;
    let of_day = i64::from(h) * 3600 + i64::from(m) * 60 + i64::from(s);
    Some(((i64::from(start.mjd) - MJD_NTP_EPOCH) * 86_400 + of_day - JST_OFFSET) as u32)
}

/// 90 kHz ticks from the origin as nanoseconds.
fn nanoseconds(ticks: i64) -> u64 {
    u64::try_from(ticks.max(0)).unwrap_or(0) * 100_000 / 9
}

#[cfg(test)]
mod tests {
    use super::*;
    use isdb_si::time::MjdTime;

    /// 2026-11-05 as an MJD.
    const DAY: u16 = 61_349;
    const SERVICE: u16 = 0x0065;

    /// An MH-EIT section of `table_id` with events `(id, start, duration,
    /// name)`: JST `hhmmss` BCD, all bits '1' for undefined.
    fn section(
        table_id: u8,
        service: u16,
        current: bool,
        events: &[(u16, u32, u32, Option<&str>)],
    ) -> TableBytes {
        let mut body = vec![0xb1, 0x10, 0x00, 0x0b, 0x00, table_id];
        for &(id, start, duration, name) in events {
            let mut descriptors = Vec::new();
            if let Some(n) = name {
                let mut b = b"jpn".to_vec();
                b.push(u8::try_from(n.len()).unwrap());
                b.extend(n.as_bytes());
                b.extend([0, 0]);
                descriptors.extend(0xF001u16.to_be_bytes());
                descriptors.extend(u16::try_from(b.len()).unwrap().to_be_bytes());
                descriptors.extend(b);
            }
            body.extend(id.to_be_bytes());
            let mjd = if start == 0xff_ffff { 0xffff } else { DAY };
            body.extend(mjd.to_be_bytes());
            body.extend(&start.to_be_bytes()[1..]);
            body.extend(&duration.to_be_bytes()[1..]);
            body.extend(u16::try_from(descriptors.len()).unwrap().to_be_bytes());
            body.extend(descriptors);
        }
        let mut s = vec![table_id, 0xf0, 0];
        s.extend(service.to_be_bytes());
        s.extend([0xc0 | u8::from(current), 0, 0]);
        s.extend(body);
        let len = u16::try_from(s.len() - 3 + 4).unwrap();
        s[1] = 0xf0 | u8::try_from(len >> 8).unwrap();
        s[2] = u8::try_from(len & 0xff).unwrap();
        s.extend(isdb_ts::crc::crc32(&s).to_be_bytes());
        TableBytes {
            packet_id: 0x8000,
            message_id: 0x8000,
            bytes: s,
        }
    }

    /// The NTP seconds of a JST `hhmmss` of `DAY`.
    fn at(hhmmss: u32) -> u32 {
        ntp_seconds(MjdTime {
            mjd: DAY,
            bcd_hms: hhmmss,
        })
        .unwrap()
    }

    /// The recording's span, from JST `from` to `to`, and its clock: PTS 0
    /// at `from`.
    fn span(from: u32, to: u32) -> (Clock, i64, i64) {
        let clock = Clock {
            ticks: 0,
            ntp: u64::from(at(from)) << 32,
        };
        (clock, 0, clock.ticks_at(at(to)))
    }

    /// `(start s, end s, title)` of a schedule's chapters in a span.
    fn listed(s: &Schedule, from: u32, to: u32) -> Vec<(u64, u64, Option<String>)> {
        let (clock, origin, end) = span(from, to);
        s.chapters(SERVICE, clock, origin, end)
            .into_iter()
            .map(|c| {
                (
                    c.start_ns / 1_000_000_000,
                    c.end_ns.unwrap() / 1_000_000_000,
                    c.title,
                )
            })
            .collect()
    }

    fn title(s: &str) -> Option<String> {
        Some(s.to_owned())
    }

    #[test]
    fn spec_b60_table_7_18_start_time_is_jst() {
        // B60 §7.3.3.9: "93/10/13 12:45:00 is coded as 0xC079124500", JST:
        // 03:45:00 UTC, 2 959 472 700 s after 1900-01-01.
        assert_eq!(
            ntp_seconds(MjdTime::from_u40(0xC0_7912_4500)),
            Some(2_959_472_700)
        );
    }

    #[test]
    fn spec_b60_table_7_18_undefined_start_has_no_time() {
        assert_eq!(ntp_seconds(MjdTime::UNDEFINED), None);
        // Not BCD, or not a time of day.
        assert_eq!(ntp_seconds(MjdTime::from_u40(0xC0_791A_4500)), None);
        assert_eq!(ntp_seconds(MjdTime::from_u40(0xC0_7925_0000)), None);
    }

    #[test]
    fn an_event_per_chapter_clipped_to_the_recording() {
        let mut s = Schedule::new();
        s.feed(&section(
            0x8C,
            SERVICE,
            true,
            &[
                (1, 0x002500, 0x010000, Some("before")),
                (2, 0x012500, 0x005900, Some("first")),
                (3, 0x022400, 0x005900, Some("second")),
                (4, 0x032300, 0x003700, Some("third")),
                (5, 0x040000, 0x010000, Some("after")),
            ],
        ));
        assert_eq!(
            listed(&s, 0x022126, 0x033517),
            vec![
                (0, 154, title("first")),
                (154, 154 + 59 * 60, title("second")),
                (154 + 59 * 60, 3 + 73 * 60 + 48, title("third")),
            ]
        );
    }

    #[test]
    fn the_chapter_start_is_the_events_place_in_nanoseconds() {
        let mut s = Schedule::new();
        s.feed(&section(
            0x8C,
            SERVICE,
            true,
            &[(1, 0x010000, 0x010000, None)],
        ));
        // The recording starts half a second past 00:59:59: its origin's
        // PTS is arbitrary.
        let clock = Clock {
            ticks: 5_000_000,
            ntp: (u64::from(at(0x005959)) << 32) | (1 << 31),
        };
        let c = s.chapters(SERVICE, clock, 5_000_000, 5_000_000 + 3_600 * TICKS);
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].start_ns, 500_000_000);
        assert_eq!(c[0].end_ns, Some(3_600_000_000_000));
    }

    #[test]
    fn a_name_merges_across_tables_with_its_language() {
        let mut s = Schedule::new();
        // Extended table first: the time alone.
        s.feed(&section(
            0x94,
            SERVICE,
            true,
            &[(1, 0x010000, 0x010000, None)],
        ));
        s.feed(&section(
            0x8C,
            SERVICE,
            true,
            &[(1, 0x010000, 0x010000, Some("名前"))],
        ));
        // And again without: the name stays.
        s.feed(&section(
            0x94,
            SERVICE,
            true,
            &[(1, 0x010000, 0x010000, None)],
        ));
        let (clock, origin, end) = span(0x010000, 0x020000);
        let c = s.chapters(SERVICE, clock, origin, end);
        assert_eq!(c[0].title.as_deref(), Some("名前"));
        assert_eq!(c[0].language.as_deref(), Some("jpn"));
    }

    #[test]
    fn an_event_never_named_has_no_title() {
        let mut s = Schedule::new();
        s.feed(&section(
            0x94,
            SERVICE,
            true,
            &[(1, 0x010000, 0x010000, None)],
        ));
        assert_eq!(listed(&s, 0x010000, 0x013000), vec![(0, 1800, None)]);
    }

    #[test]
    fn present_following_outranks_a_lagging_schedule() {
        let mut s = Schedule::new();
        s.feed(&section(
            0x8B,
            SERVICE,
            true,
            &[(1, 0x010000, 0x013000, Some("live"))],
        ));
        // A schedule copy not yet updated, read after it.
        s.feed(&section(
            0x8C,
            SERVICE,
            true,
            &[(1, 0x010000, 0x010000, Some("old"))],
        ));
        assert_eq!(
            listed(&s, 0x010000, 0x040000),
            vec![(0, 5400, title("live"))]
        );
    }

    #[test]
    fn the_last_section_of_a_rank_wins() {
        let mut s = Schedule::new();
        s.feed(&section(
            0x8B,
            SERVICE,
            true,
            &[(1, 0x010000, 0x010000, Some("a"))],
        ));
        s.feed(&section(
            0x8B,
            SERVICE,
            true,
            &[(1, 0x011000, 0x010000, Some("b"))],
        ));
        assert_eq!(
            listed(&s, 0x010000, 0x040000),
            vec![(600, 4200, title("b"))]
        );
    }

    #[test]
    fn an_event_that_aired_stays_after_its_table_drops_it() {
        let mut s = Schedule::new();
        s.feed(&section(
            0x8B,
            SERVICE,
            true,
            &[
                (1, 0x010000, 0x010000, Some("one")),
                (2, 0x020000, 0x010000, Some("two")),
            ],
        ));
        s.feed(&section(
            0x8B,
            SERVICE,
            true,
            &[(2, 0x020000, 0x010000, Some("two"))],
        ));
        assert_eq!(
            listed(&s, 0x013000, 0x023000),
            vec![(0, 1800, title("one")), (1800, 3600, title("two"))]
        );
    }

    #[test]
    fn a_replaced_event_has_no_chapter() {
        let mut s = Schedule::new();
        s.feed(&section(
            0x8C,
            SERVICE,
            true,
            &[(1, 0x010000, 0x010000, Some("stale"))],
        ));
        s.feed(&section(
            0x8C,
            SERVICE,
            true,
            &[
                (2, 0x010000, 0x003000, Some("news")),
                (3, 0x013000, 0x003000, Some("rest")),
            ],
        ));
        assert_eq!(
            listed(&s, 0x010000, 0x020000),
            vec![(0, 1800, title("news")), (1800, 3600, title("rest"))]
        );
    }

    #[test]
    fn an_overlap_ends_the_earlier_chapter_at_the_next() {
        let mut s = Schedule::new();
        // The aired event's table copy is old and its nominal end overlaps
        // what followed; it started first, so it stays.
        s.feed(&section(
            0x8B,
            SERVICE,
            true,
            &[(1, 0x010000, 0x010000, Some("one"))],
        ));
        s.feed(&section(
            0x8B,
            SERVICE,
            true,
            &[(2, 0x015000, 0x010000, Some("two"))],
        ));
        assert_eq!(
            listed(&s, 0x010000, 0x030000),
            vec![(0, 3000, title("one")), (3000, 6600, title("two"))]
        );
    }

    #[test]
    fn an_undecided_duration_runs_to_the_next_event() {
        let mut s = Schedule::new();
        s.feed(&section(
            0x8B,
            SERVICE,
            true,
            &[
                (1, 0x010000, 0xff_ffff, Some("news")),
                (2, 0x020000, 0x010000, Some("next")),
            ],
        ));
        assert_eq!(
            listed(&s, 0x013000, 0x023000),
            vec![(0, 1800, title("news")), (1800, 3600, title("next"))]
        );
        // With nothing after it, to the recording's end.
        assert_eq!(
            listed(&s, 0x013000, 0x015000),
            vec![(0, 1200, title("news"))]
        );
    }

    #[test]
    fn an_undecided_duration_ends_at_an_event_before_the_recording() {
        let mut s = Schedule::new();
        s.feed(&section(
            0x8B,
            SERVICE,
            true,
            &[
                (1, 0x010000, 0xff_ffff, Some("news")),
                (2, 0x020000, 0x010000, Some("over")),
                (3, 0x032000, 0x010000, Some("next")),
            ],
        ));
        // From 03:10: the news ended at 02:00 and has no chapter.
        assert_eq!(
            listed(&s, 0x031000, 0x033000),
            vec![(600, 1200, title("next"))]
        );
    }

    #[test]
    fn a_gap_between_events_has_no_chapter() {
        let mut s = Schedule::new();
        s.feed(&section(
            0x8C,
            SERVICE,
            true,
            &[
                (1, 0x010000, 0x001000, Some("one")),
                (2, 0x012000, 0x001000, Some("two")),
            ],
        ));
        assert_eq!(
            listed(&s, 0x010000, 0x020000),
            vec![(0, 600, title("one")), (1200, 1800, title("two"))]
        );
    }

    #[test]
    fn what_cannot_be_placed_or_is_not_in_force_is_ignored() {
        let mut s = Schedule::new();
        // Another service, a next version, an undefined start, a zero
        // duration.
        s.feed(&section(
            0x8C,
            0x0066,
            true,
            &[(1, 0x010000, 0x010000, Some("other"))],
        ));
        s.feed(&section(
            0x8C,
            SERVICE,
            false,
            &[(2, 0x010000, 0x010000, Some("next"))],
        ));
        s.feed(&section(
            0x8C,
            SERVICE,
            true,
            &[(3, 0xff_ffff, 0x010000, Some("nvod"))],
        ));
        s.feed(&section(
            0x8C,
            SERVICE,
            true,
            &[(4, 0x010000, 0x000000, Some("empty"))],
        ));
        assert_eq!(listed(&s, 0x010000, 0x020000), vec![]);
        // A damaged section and other tables change nothing.
        let mut bad = section(0x8C, SERVICE, true, &[(5, 0x010000, 0x010000, Some("bad"))]);
        *bad.bytes.last_mut().unwrap() ^= 1;
        s.feed(&bad);
        let mut other = section(0x8C, SERVICE, true, &[(6, 0x010000, 0x010000, Some("sdt"))]);
        other.bytes[0] = 0x9F;
        s.feed(&other);
        s.feed(&TableBytes {
            packet_id: 0,
            message_id: 0,
            bytes: Vec::new(),
        });
        assert_eq!(listed(&s, 0x010000, 0x020000), vec![]);
    }

    #[test]
    fn a_short_section_does_not_panic() {
        let mut s = Schedule::new();
        for n in 0..32 {
            let t = section(0x8C, SERVICE, true, &[(1, 0x010000, 0x010000, Some("x"))]);
            s.feed(&TableBytes {
                bytes: t.bytes[..n].to_vec(),
                ..t
            });
        }
        assert_eq!(listed(&s, 0x010000, 0x020000), vec![]);
    }
}
