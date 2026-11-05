//! The chapters of the Matroska form: a service's MH-EIT events (B60
//! §7.3.3.9, Table 7-18) on the recording's timeline.
//!
//! An event's `start_time` is JST and the media's clock is the NTP timestamp
//! of the MPT's descriptors, UTC (B60 Chapter 3), so the schedule has an exact
//! place on the timeline ([`crate::timing::Clock`]). The stream switches a little
//! after it, and a chapter starts where it really does, on the video's key picture
//! ([`Schedule::chapters`], `crates/isdb-remux/SPEC-NOTES.md`):
//!
//! 1. where a new coded video sequence starts in the 10 s after the
//!    scheduled time (an end of sequence NAL unit, a changed SPS);
//! 2. else at the first MPU sent wholly under the first new MPT version in the
//!    window: the second it lists, the first being already on its way;
//! 3. else, with neither seen, at the scheduled time (and counted).
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

use crate::signal::AssetTiming;
use crate::timing::{Clock, au_ntp};
use isdb_mkv::Chapter;
use isdb_mmt::desc::Descriptor;
use isdb_mmt::message::TableBytes;
use isdb_mmt::table::{Eit, Mpt, Sdt};
use std::cmp::Reverse;
use std::collections::BTreeMap;

/// 90 kHz ticks per second.
const TICKS: i64 = 90_000;
/// The MJD of 1900-01-01, NTP's epoch (B10 Annex C's date arithmetic).
const MJD_NTP_EPOCH: i64 = 15_020;
/// JST − UTC (B60 §7.4.3.32: "Japan Standard Time (UTC+9 hours)").
const JST_OFFSET: i64 = 9 * 3600;

/// How long after an event's scheduled time a switch of the stream counts as
/// its own (seconds). The recordings' are 2.1–2.6 s after it; other MPT versions
/// come a minute away.
pub(crate) const WINDOW: u64 = 10;

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
    /// The MPT version last seen, by the packet id it came on.
    versions: BTreeMap<u16, u8>,
    /// Per service, where each new MPT version takes effect (NTP timestamps).
    versions_in_force: BTreeMap<u16, Vec<u64>>,
    /// Per service, where a new coded video sequence starts (NTP timestamps).
    sequences: BTreeMap<u16, Vec<u64>>,
    /// Per service, the MH-service descriptor's provider and service names.
    names: BTreeMap<u16, (String, String)>,
}

/// An event in the recording, placed on the timeline and clipped to it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Aired {
    /// Where it starts (unwrapped PTS).
    pub start: i64,
    /// Where it stops (unwrapped PTS).
    pub stop: i64,
    /// Its name.
    pub title: Option<String>,
    /// The name's ISO 639-2 language.
    pub language: Option<String>,
    /// Its start is the scheduled time, no switch of the stream having been seen
    /// (not one clipped to the recording's start).
    pub scheduled: bool,
}

/// The chapters of a service.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Chapters {
    /// In time order.
    pub list: Vec<Chapter>,
    /// How many start at their scheduled time, no switch of the stream having
    /// been seen (one clipped to the recording's start is not counted).
    pub scheduled: usize,
}

impl Schedule {
    /// An empty schedule.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// One table of a control message: an MPT, or an MH-EIT section. Any
    /// other table, or one that does not check, changes nothing.
    pub fn feed(&mut self, t: &TableBytes) {
        match t.bytes.first() {
            Some(0x20) if t.message_id == 0 => self.mpt(t),
            Some(0x8B..=0x9B) => self.eit(t),
            Some(0x9F | 0xA0) => self.sdt(t),
            _ => {}
        }
    }

    /// An MH-SDT section (9Fh actual, A0h other): the services' names.
    fn sdt(&mut self, t: &TableBytes) {
        let Ok(sdt) = Sdt::parse(&t.bytes) else {
            return;
        };
        for service in &sdt.services {
            if let Some((_, provider, name)) = service.service() {
                self.names
                    .insert(service.service_id, (provider.to_owned(), name.to_owned()));
            }
        }
    }

    /// The channel name of `service`: the MH-service descriptor's service name,
    /// else its provider name, when they are not blank.
    #[must_use]
    pub fn channel(&self, service: u16) -> Option<&str> {
        let (provider, name) = self.names.get(&service)?;
        [name, provider]
            .into_iter()
            .map(|n| n.trim())
            .find(|n| !n.is_empty())
    }

    /// A new coded video sequence of `service` starts at the NTP timestamp
    /// `ntp` (its first access unit in decode order).
    pub fn sequence_start(&mut self, service: u16, ntp: u64) {
        self.sequences.entry(service).or_default().push(ntp);
    }

    /// The new coded video sequences noted for `service`.
    #[cfg(test)]
    pub(crate) fn sequence_count(&self, service: u16) -> usize {
        self.sequences.get(&service).map_or(0, Vec::len)
    }

    /// An MPT: every copy is the same until the version moves, and the first
    /// version seen is no change.
    fn mpt(&mut self, t: &TableBytes) {
        let Some(&version) = t.bytes.get(1) else {
            return;
        };
        if self.versions.get(&t.packet_id) == Some(&version) {
            return;
        }
        let Some(mpt) = Mpt::parse(&t.bytes) else {
            return;
        };
        if self.versions.insert(t.packet_id, version).is_none() {
            return;
        }
        if let (Some(service), Some(ntp)) = (mpt.service_id(), first_whole_mpu(&mpt)) {
            self.versions_in_force.entry(service).or_default().push(ntp);
        }
    }

    /// An MH-EIT section (table id 8Bh p/f, 8Ch–9Bh schedule), when in force.
    fn eit(&mut self, t: &TableBytes) {
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

    /// Where the stream of `service` really switches for a change scheduled
    /// at the NTP second `seconds`, when a switch is seen.
    fn switch_after(&self, service: u16, seconds: u32) -> Option<u64> {
        let scheduled = u64::from(seconds) << 32;
        let earliest = |from: &BTreeMap<u16, Vec<u64>>| {
            from.get(&service)?
                .iter()
                .map(|&ntp| ntp.wrapping_sub(scheduled))
                .filter(|&after| after < WINDOW << 32)
                .min()
                .map(|after| scheduled.wrapping_add(after))
        };
        earliest(&self.sequences).or_else(|| earliest(&self.versions_in_force))
    }

    /// The chapters of `service` on the timeline from the unwrapped PTS
    /// `origin` to `end`, in time order: each event in that span, clipped to
    /// it, with `clock` placing the event's time and the stream's switches.
    #[must_use]
    pub fn chapters(&self, service: u16, clock: Clock, origin: i64, end: i64) -> Chapters {
        let mut chapters = Chapters::default();
        for e in self.aired(service, clock, origin, end) {
            chapters.scheduled += usize::from(e.scheduled);
            chapters.list.push(Chapter {
                start_ns: nanoseconds(e.start - origin),
                end_ns: Some(nanoseconds(e.stop - origin)),
                title: e.title,
                language: e.language,
            });
        }
        chapters
    }

    /// The events of `service` in the span from the unwrapped PTS `origin` to
    /// `end`, in time order and clipped to it, placed as a chapter is.
    #[must_use]
    pub fn aired(&self, service: u16, clock: Clock, origin: i64, end: i64) -> Vec<Aired> {
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
        // A scheduled time as the stream's, and whether a switch was seen.
        let real = |seconds: u32, scheduled: i64| {
            self.switch_after(service, seconds)
                .map_or((scheduled, false), |ntp| (clock.ticks_of(ntp), true))
        };
        let starts: Vec<(i64, bool)> = kept.iter().map(|p| real(p.entry.start, p.start)).collect();
        let mut aired = Vec::new();
        for (i, p) in kept.iter().enumerate() {
            let (at, switched) = starts[i];
            let start = at.max(origin);
            let own_end = p
                .entry
                .duration
                .zip(p.stop)
                .map(|(d, stop)| real(p.entry.start.wrapping_add(d), stop).0);
            let stop = [own_end, starts.get(i + 1).map(|s| s.0)]
                .into_iter()
                .flatten()
                .fold(end, std::cmp::Ord::min);
            if stop > start {
                aired.push(Aired {
                    start,
                    stop,
                    title: p.entry.name.clone(),
                    language: p.entry.language.clone(),
                    scheduled: !switched && at > origin,
                });
            }
        }
        aired
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

/// Where an MPT version takes effect on the video: the key picture (the first
/// access unit in decode order) of the second MPU the version lists, or its
/// presentation time when the descriptors do not time the picture. The first MPU
/// listed was being sent when the version came (B60 does not define this:
/// `crates/isdb-remux/SPEC-NOTES.md`).
fn first_whole_mpu(mpt: &Mpt) -> Option<u64> {
    let video = mpt
        .assets
        .iter()
        .find(|a| matches!(&a.asset_type, b"hev1" | b"hvc1"))?;
    let row = video.descriptors.iter().find_map(|d| match d {
        Descriptor::MpuTimestamp(rows) => rows.get(1).or_else(|| rows.first()),
        _ => None,
    })?;
    let mut timing = AssetTiming::default();
    for d in &video.descriptors {
        timing.add(d);
    }
    au_ntp(&timing, row.mpu_sequence_number, 0).or(Some(row.presentation_time))
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

/// 90 kHz ticks from the origin as nanoseconds, rounded to the millisecond as
/// the blocks' times are ([`isdb_mkv::Writer::rebase`]): a chapter at a key picture
/// is at its block's time, where a seek lands on it and not a picture group early.
fn nanoseconds(ticks: i64) -> u64 {
    u64::try_from((ticks.max(0) + 45) / 90).unwrap_or(0) * 1_000_000
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
            .list
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
        let c = s
            .chapters(SERVICE, clock, 5_000_000, 5_000_000 + 3_600 * TICKS)
            .list;
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
        let c = s.chapters(SERVICE, clock, origin, end).list;
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

    /// An MPT of `service` at `version`: a video asset with its MPU timestamps
    /// `(mpu, seconds after the NTP second base)`, and, with `timed`, the on-air
    /// row of one access unit per MPU (key picture 21020/180000 s after the MPU's
    /// presentation time).
    fn mpt(service: u16, version: u8, base: u32, rows: &[(u32, u32)], timed: bool) -> TableBytes {
        let mut d = vec![0x00, 0x01, u8::try_from(rows.len() * 12).unwrap()];
        for &(mpu, after) in rows {
            d.extend(mpu.to_be_bytes());
            d.extend((u64::from(base + after) << 32).to_be_bytes());
        }
        if timed {
            let mut e = vec![0b0000_0011];
            e.extend(180_000u32.to_be_bytes());
            e.extend(3003u16.to_be_bytes());
            for &(mpu, _) in rows {
                e.extend(mpu.to_be_bytes());
                e.extend([0, 0x23, 0x32, 1, 0x75, 0x4e]);
            }
            d.extend([0x80, 0x26, u8::try_from(e.len()).unwrap()]);
            d.extend(e);
        }
        let mut t = vec![0x20, version, 0, 0, 0xFC, 2];
        t.extend(service.to_be_bytes());
        t.extend([0, 0, 1, 0, 0, 0, 0, 0, 0]);
        t.extend(*b"hev1");
        t.extend([0xFE, 0]);
        t.extend(u16::try_from(d.len()).unwrap().to_be_bytes());
        t.extend(d);
        let len = u16::try_from(t.len() - 4).unwrap();
        t[2..4].copy_from_slice(&len.to_be_bytes());
        TableBytes {
            packet_id: 0xff02,
            message_id: 0,
            bytes: t,
        }
    }

    /// Two events meeting at 02:00:00, recorded from 01:59:00 to 02:01:00.
    fn two_events() -> Schedule {
        let mut s = Schedule::new();
        s.feed(&section(
            0x8C,
            SERVICE,
            true,
            &[
                (1, 0x010000, 0x010000, Some("one")),
                (2, 0x020000, 0x010000, Some("two")),
            ],
        ));
        s
    }

    /// `(start ms, end ms)` of its chapters, and how many are at the schedule.
    fn millis(s: &Schedule) -> (Vec<(u64, u64)>, usize) {
        let (clock, origin, end) = span(0x015900, 0x020100);
        let c = s.chapters(SERVICE, clock, origin, end);
        let list = c
            .list
            .iter()
            .map(|c| (c.start_ns / 1_000_000, c.end_ns.unwrap() / 1_000_000))
            .collect();
        (list, c.scheduled)
    }

    #[test]
    fn with_no_switch_seen_a_chapter_is_at_the_schedule_and_counted() {
        let (list, scheduled) = millis(&two_events());
        assert_eq!(list, vec![(0, 60_000), (60_000, 120_000)]);
        assert_eq!(scheduled, 1, "the clipped first chapter does not count");
    }

    #[test]
    fn a_new_mpt_version_moves_the_change_to_its_first_whole_mpus_key_picture() {
        let mut s = two_events();
        let n = at(0x020000);
        s.feed(&mpt(SERVICE, 1, n, &[(10, 0)], true));
        s.feed(&mpt(SERVICE, 2, n, &[(20, 2), (21, 3), (22, 4)], true));
        // A copy of the same version is no second change.
        s.feed(&mpt(SERVICE, 2, n, &[(21, 3), (22, 4)], true));
        // 60 s + MPU 21's 3 s + 21020/180000 s, to the millisecond.
        let (list, scheduled) = millis(&s);
        assert_eq!(list, vec![(0, 63_117), (63_117, 120_000)]);
        assert_eq!(scheduled, 0);
    }

    #[test]
    fn an_untimed_picture_falls_back_to_the_mpus_presentation_time() {
        let mut s = two_events();
        let n = at(0x020000);
        s.feed(&mpt(SERVICE, 1, n, &[(10, 0)], false));
        s.feed(&mpt(SERVICE, 2, n, &[(20, 2), (21, 3)], false));
        assert_eq!(millis(&s).0, vec![(0, 63_000), (63_000, 120_000)]);
    }

    #[test]
    fn a_new_video_sequence_outranks_the_mpt_version() {
        let mut s = two_events();
        let n = at(0x020000);
        s.feed(&mpt(SERVICE, 1, n, &[(10, 0)], false));
        s.feed(&mpt(SERVICE, 2, n, &[(20, 2), (21, 3)], false));
        s.sequence_start(SERVICE, (u64::from(n) << 32) + (5u64 << 31));
        assert_eq!(millis(&s).0, vec![(0, 62_500), (62_500, 120_000)]);
    }

    #[test]
    fn a_switch_outside_the_window_or_of_another_service_is_not_the_events() {
        let mut s = two_events();
        let n = at(0x020000);
        // The first version seen is no change.
        s.feed(&mpt(SERVICE, 1, n, &[(10, 2), (11, 3)], false));
        // A version with no event: its second MPU a minute before, and one
        // past the window. (BS-TBS 4K's 02:59 one.)
        s.feed(&mpt(SERVICE, 2, n - 60, &[(20, 2), (21, 3)], false));
        s.feed(&mpt(SERVICE, 3, n, &[(30, 10), (31, 11)], false));
        s.sequence_start(0x0066, u64::from(n + 1) << 32);
        s.feed(&mpt(0x0066, 1, n, &[(40, 2), (41, 3)], false));
        let (list, scheduled) = millis(&s);
        assert_eq!(list, vec![(0, 60_000), (60_000, 120_000)]);
        assert_eq!(scheduled, 1);
    }

    #[test]
    fn an_events_end_with_nothing_after_it_moves_too() {
        let mut s = Schedule::new();
        s.feed(&section(
            0x8C,
            SERVICE,
            true,
            &[(1, 0x020000, 0x000030, Some("short"))],
        ));
        let n = at(0x020030);
        s.feed(&mpt(SERVICE, 1, n, &[(10, 0)], false));
        s.feed(&mpt(SERVICE, 2, n, &[(20, 2), (21, 3)], false));
        assert_eq!(millis(&s).0, vec![(60_000, 93_000)]);
    }

    #[test]
    fn a_malformed_mpt_changes_nothing() {
        let mut s = two_events();
        let whole = mpt(SERVICE, 2, at(0x020000), &[(20, 2), (21, 3)], true);
        s.feed(&mpt(SERVICE, 1, at(0x020000), &[(10, 0)], false));
        for n in 0..whole.bytes.len() {
            s.feed(&TableBytes {
                bytes: whole.bytes[..n].to_vec(),
                ..whole.clone()
            });
        }
        assert_eq!(millis(&s).1, 1);
    }

    /// An MH-SDT section with one service's provider and service names.
    fn sdt(service: u16, provider: &str, name: &str) -> TableBytes {
        let mut d = vec![1, u8::try_from(provider.len()).unwrap()];
        d.extend(provider.as_bytes());
        d.push(u8::try_from(name.len()).unwrap());
        d.extend(name.as_bytes());
        let mut descriptors = vec![0x80, 0x19, u8::try_from(d.len()).unwrap()];
        descriptors.extend(d);
        let mut s = vec![0xA0, 0xf0, 0, 0xb1, 0x10, 0xc1, 0, 0, 0x00, 0x0b, 0xff];
        s.extend(service.to_be_bytes());
        s.push(0xff);
        s.extend(u16::try_from(descriptors.len()).unwrap().to_be_bytes());
        s.extend(descriptors);
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

    #[test]
    fn the_channel_is_the_service_name_else_the_provider() {
        let mut s = Schedule::new();
        s.feed(&sdt(SERVICE, "ＮＨＫ", "ＮＨＫ　ＢＳＰ４Ｋ"));
        assert_eq!(s.channel(SERVICE), Some("ＮＨＫ　ＢＳＰ４Ｋ"));
        // Blank on air (an ideographic space): the provider, else none.
        s.feed(&sdt(0x0066, "局", "　"));
        assert_eq!(s.channel(0x0066), Some("局"));
        s.feed(&sdt(0x00e7, "　", "　"));
        assert_eq!(s.channel(0x00e7), None);
        assert_eq!(s.channel(0x0097), None);
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
