# isdb-remux — spec interpretation notes

Specs: ARIB STD-B60 v1.14-E1 (MH-EIT §7.3.3.9, MPT §7.3.3.1, MPU timestamp descriptor
§7.4.3.5, video component descriptor §7.4.3.19, NTP Chapter 3), H.265 from knowledge
(SPECS.md §1.2) and RFC 9559 (Matroska chapters §6.6, §20). Only the chapters of the
Matroska form have notes so far. "The BS17 recordings": clear TLV streams of services
0x0065, 0x00a1 and 0x00b5, 74 min each, with six event changes between them
(VERIFICATION.md, "Matroska chapters").

## event-time: an event's `start_time` on the media clock
- Spec: §7.3.3.9: "`start_time` … represents the start time of event by Japan
  Standard Time (JST) and Modified Julian Day (MJD)"; §7.4.3.5:
  "`mpu_presentation_time` … the presentation time of MPU by the NTP timestamp
  format of 64 bits"; Chapter 3 provides "Coordinated Universal Time (UTC)" in
  NTP format; §7.4.3.32: "Japan Standard Time (UTC+9 hours)".
- Interpretation: the MPU timestamp's NTP time is UTC, and a scheduled time has its
  place on the timeline at `start_time` − 9 h, counted in days from MJD 15020 (NTP's
  epoch, 1900-01-01). B60 does not say the two clocks are meant to be compared, nor
  that an event's first picture is presented at its `start_time`.
- Confirmation: on the BS17 recordings p/f section 0 turns over within 0.3 s of the
  scheduled time on the stream's NTP clock (some before, some after), and the picture cuts 2.665–2.845 s after
  it at all six changes, on three services. So the schedule's place is exact, and
  it is not the switch: see switch-point. Status: confirmed (live data) for the
  mapping; the lag's cause is not established.

## switch-point: where a programme really switches in the stream
- Spec: B60 defines the MPT's version but no relation between an MPT version, an
  event and the media: no clause says at which MPU a version takes effect, or how a
  receiver finds an event's first picture. Table 7-51 has the transfer
  characteristics that a change of the SPS's VUI goes with.
- Interpretation (from the recordings, not the spec): in the 10 s after an event's
  scheduled start or end, the switch is where a new coded video sequence starts (an
  end of sequence NAL unit: the next MPU; a changed SPS: its MPU); else the MPU after
  the first one listed by the first new MPT version, as the listed first was being
  sent when the version came; else nothing is seen and the schedule stands, counted
  in the report. A chapter starts at that MPU's first access unit in decode order, its
  key picture, so a seek lands in it. A version with no event near it is ignored.
- Confirmation: relative to the schedule on the BS17 recordings (presentation times):

  | Service, change | First MPU listed | Next MPU | Picture cut | Structure |
  |---|---|---|---|---|
  | 0x0065 02:24 | +2.181 | **+2.581** | +2.665 | none |
  | 0x0065 03:23 | **+2.250** | +2.651 | +2.668 | EOS and a new SPS at +2.250 |
  | 0x00a1 03:00 | +2.157 | **+2.691** | +2.709 | none |
  | 0x00a1 03:01 | +2.084 | **+2.618** | +2.702 | none |
  | 0x00b5 02:30 | +2.579 | **+2.712** | +2.713 | none |
  | 0x00b5 03:00 | +2.510 | **+2.777** | +2.845 | none |

  The next MPU starts 1–84 ms before the cut at all six. Its key picture is 21020 /
  180000 s (117 ms) after its presentation time, so up to 7 pictures of the new
  programme, leading pictures that a seek skips, fall before the chapter. At 0x0065's
  03:23, where the MPT also changes the asset list and the transfer characteristics
  (HLG to BT.2020), the new sequence and the end of the stereo asset 0xf311 come
  0.4 s before the cut, and 0xf310 goes from 5.1 to stereo at +2.873: a chapter
  cannot hold all of one structure, and the video's is taken. An MPT version also came
  at 02:59 on 0x00a1 with no event. Status: confirmed (live data) for these six; a new
  sequence seen once; **other broadcasters, overrunning events and a switch outside the
  window are unchecked** (a p/f turnover later than the schedule is not looked at).

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
