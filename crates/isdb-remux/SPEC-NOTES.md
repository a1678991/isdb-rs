# isdb-remux — spec interpretation notes

Specs: ARIB STD-B60 v1.14-E1 (MH-EIT §7.3.3.9, MPU timestamp descriptor
§7.4.3.5, NTP Chapter 3) and RFC 9559 (Matroska chapters §6.6, §20). Only the
chapters of the Matroska form have notes so far. "The BS17 recording": a clear
TLV stream of service 0x0065 (NHK BS P4K), 74 min across two event changes
(VERIFICATION.md, "Matroska chapters").

## event-time: an event's `start_time` on the media clock
- Spec: §7.3.3.9: "`start_time` … represents the start time of event by Japan
  Standard Time (JST) and Modified Julian Day (MJD)"; §7.4.3.5:
  "`mpu_presentation_time` … the presentation time of MPU by the NTP timestamp
  format of 64 bits"; Chapter 3 provides "Coordinated Universal Time (UTC)" in
  NTP format; §7.4.3.32: "Japan Standard Time (UTC+9 hours)".
- Interpretation: the MPU timestamp's NTP time is UTC, and an event starts on
  the timeline at the access unit whose presentation time is `start_time` − 9 h,
  counted in days from MJD 15020 (NTP's epoch, 1900-01-01). B60 does not say that
  the two clocks are meant to be compared; nothing there says an event's first
  picture is presented at exactly its `start_time`.
- Confirmation: on the BS17 recording the p/f table turns over 0.2–0.3 s before
  the mark on the stream's NTP clock, and the picture cuts to the next programme
  2.665 s and 2.668 s after the two marks, an hour apart. So the mapping is exact
  and a chapter marks the **scheduled** time: the picture follows it by a
  constant the tables do not state (cause not established; one service measured,
  other services may differ). Nothing is corrected for it. Status: confirmed
  (live data) for the mapping; the lag is an observation.

## event-merge: which table's description of an event a chapter takes
- Spec: §7.3.3.9 divides the MH-EIT into present/next (8Bh) and schedule
  (8Ch–9Bh) tables, and `event_id` is "uniquely assigned in one service". It says
  nothing on how a receiver combines them, or what an event dropped from its
  table means.
- Interpretation: descriptions accumulate per `event_id` over the whole run, not
  from the last tables held. A name comes from any table with one. The time of a
  p/f section outranks a schedule's, whose copy may lag; within a rank the last
  section read wins. An event whose start lies inside an event read more recently
  was replaced and has no chapter. Any other overlap ends the earlier chapter at
  the next one's start. An undecided duration (all bits '1') runs to the next
  event or the recording's end. An undefined `start_time` has no place on the
  timeline and no chapter.
- Confirmation: on the BS17 recording the extended tables (94h–9Bh) repeat every
  event's time with no MH-short event descriptor, and p/f section 0 drops each
  event as the next one starts, as this assumes. No event changed its time or was
  replaced there, so **the rank and replacement rules have only synthetic tests**.
  An event replaced by one starting after it keeps a chapter up to that start.
  Status: confirmed (live data) for the merge of names and accumulation; pending
  real-world check for the rank and replacement rules.

## chapter-placement: where the Chapters element goes
- Spec: RFC 9559 §6.6: "The Chapters element SHOULD be placed before the first
  Tracks element and after the first Info element", while §25.3.1's optimum layout
  lists Tracks before Chapters.
- Interpretation: §6.6's normative order (Info, Chapters, Tracks), one default,
  non-ordered edition. §20.1.3 uses only `ChapterTimeStart` of a simple chapter as
  a mark, and the RFC sets no rule for gaps between sibling chapters, so a time no
  event covers simply has no chapter.
- Confirmation: ffprobe and mpv read the chapters of the BS17 recording, and mpv
  seeks to them. Status: confirmed (live data).
