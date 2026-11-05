//! An MPEG-TS recorder of the decoded media with the stream's own clock
//! (ISO/IEC 13818-1): one program per recording, an elementary stream per
//! asset of the service's MPT — the HEVC video (Annex B, PES with PTS and
//! DTS, the PCR on its pid), every AAC audio (LOAS/LATM as it comes, PES
//! with PTS), and the untimed assets (data broadcasting `aapp`, …) as
//! private data — the timestamps resolved from the MPTs' descriptors by
//! [`crate::timing::au_times`].
//!
//! Stream pids are `0x0100 + component tag` (the MH-stream identifier
//! descriptor's; `0x0300 +` an index for an asset without one), so a
//! stream keeps its pid across recordings. The PMT goes out once the
//! first MPT names the service's assets, and again (a new version) when
//! the set changes; nothing is written before it.
//!
//! An untimed asset's MFU is one *item* (a file of its package): it
//! becomes one `private_stream_2` PES (stream type 06h, no PTS) whose data
//! starts with the 4-byte MPU sequence number (the package version), the
//! 4-byte item id (big-endian) and a flags byte — `01h` on the
//! continuation of an item split over PESs, `00h` otherwise. ffmpeg reads such a pid as
//! `bin_data`: a default `-c copy` remux skips it and mpv ignores it, but
//! `-map 0` into Matroska fails (Matroska has no codec for it) — add
//! `-dn`.
//!
//! A raw `.hevc`/`.latm` pair carries no timestamps at all: `ffmpeg -i
//! x.hevc -c copy out.ts` (or `.mkv`) fails on the B-pictures with
//! "first pts and dts value must be set". The recorder's file copies
//! without that (`ffmpeg -i x.ts -c copy out.mkv`), and keeps the audio
//! in step with the video.
//!
//! Access units: the MFU header's `sample_number` is 0 throughout on air
//! (the whole MPU is one sample), so the pictures split at the HEVC AU
//! boundaries (H.265 §7.4.2.4.4: an access unit delimiter, a parameter
//! set or prefix SEI after a picture's slices, or a first slice
//! segment); an audio MFU is one `AudioMuxElement`. The *n*-th AU of an
//! MPU takes the *n*-th row of its extended-timestamp descriptor, so a
//! stream is only written from the start of an MPU on.
//!
//! Lost packets (MMTP sequence gaps, [`Session::lost`]) cost only what
//! they took, the stream keeping its place in the MPU:
//!
//! - **video**: the picture under assembly is written as far as it came
//!   (the extractor hands over a slice cut at the gap — the decoder
//!   decodes it up to there and keeps it as a reference); the next
//!   picture that arrives is placed in its MPU by its slice's picture
//!   order count against the last picture's (POC steps one per frame, the
//!   descriptors' PTSs place each decode index), so a picture lost whole
//!   just leaves its slot empty. When the lost pictures include the MPU's
//!   first (its IRAP picture), the pictures up to the next IRAP are not
//!   written — they all reference it. The RASL pictures of an IRAP the
//!   stream starts at, or that follows a picture lost whole, are not
//!   written either: they reference pictures before it that the file
//!   does not have (H.265 NoRaslOutputFlag, as at a BLA picture);
//! - **audio**: one MFU per MMTP packet (every packet so far a single
//!   complete MFU), the lost packets are lost frames, and the stream
//!   steps over them;
//! - otherwise, or when a picture cannot be placed, the stream resumes at
//!   the next MPU.
//!
//! Captions (`stpp`, TTML documents) do not go into the TS: neither ffmpeg
//! nor mpv decodes TTML. Each caption asset becomes a cue list
//! ([`CaptionTrack`]) for SRT/WebVTT sidecars, on the TS's timeline. The MPT
//! names the documents' clock (B60 Table 9-7): with time control mode `0010`
//! a time code counts from the `reference_start_time` valid for the document's
//! MPU, which moves with every event, and the sum is a UTC time as
//! the video's presentation times are ([`Clock`]). Any other mode, and a
//! document that this would place far from its arrival (the reference moved
//! on while it travelled), is placed by its arrival on the NTP clock
//! (`crates/isdb-remux/SPEC-NOTES.md`, caption-clock).
//!
//! The file is for decoding, not a T-STD conforming stream: there is no
//! rate pacing; the PCR rides each picture at [`PCR_LEAD`] before its
//! DTS.

use crate::events::Schedule;
use crate::signal::AssetTiming;
use crate::timing::{Clock, TS_MASK, au_count, au_ntp, au_times, frame_ticks};
use isdb_mmt::es::{self, START_CODE};
use isdb_mmt::message::TableBytes;
use isdb_ts::crc::crc32;
use isdb_ts::pes::{PesBody, PesHeader, PesPacket, PtsDts};
use std::collections::BTreeMap;
use std::io::Write;

/// One TS packet, 188 bytes.
const TS_PACKET: usize = 188;
/// Its payload without the 4-byte header.
const TS_PAYLOAD: usize = TS_PACKET - 4;
/// The PAT/PMT repeat, in TS packets written.
const PSI_REPEAT: u64 = 1000;
/// How far the PCR runs ahead of the picture it rides (90 kHz): the
/// audio arrives on the TLV up to this much after the video of the same
/// time.
pub const PCR_LEAD: u64 = 90_000;

/// The PAT's pid.
pub const PAT_PID: u16 = 0;
/// The PMT's pid.
pub const PMT_PID: u16 = 0x0040;
/// A stream's pid: this + its component tag.
pub const STREAM_PID_BASE: u16 = 0x0100;
/// A stream without a component tag: this + its index.
const UNTAGGED_PID_BASE: u16 = 0x0300;
/// HEVC (H.222.0 Table 2-34).
pub const STREAM_TYPE_HEVC: u8 = 0x24;
/// MPEG-4 audio with the LATM transport syntax (LOAS, Table 2-34).
pub const STREAM_TYPE_LATM: u8 = 0x11;
/// PES packets with private data (Table 2-34).
pub const STREAM_TYPE_PRIVATE_PES: u8 = 0x06;
/// The PES stream ids (Table 2-22).
const VIDEO_SID: u8 = 0xe0;
const AUDIO_SID: u8 = 0xc0;
const PRIVATE_2_SID: u8 = 0xbf;
/// The largest data a bounded PES carries after its 6-byte start.
const PES_MAX_DATA: usize = 0xffff;
/// A data item PES's prefix: MPU sequence number, item id, flags.
const DATA_PREFIX: usize = 9;

/// What one output stream carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Video,
    Audio,
    Data,
}

/// One elementary stream of the TS.
#[derive(Debug)]
struct Stream {
    /// The TS pid.
    ts_pid: u16,
    kind: Kind,
    /// Its AU counting (video and audio).
    track: Track,
}

/// One elementary stream's AU counting within its MPU.
#[derive(Debug, Default)]
struct Track {
    /// The MPU in progress; `None` until the first MPU boundary (or
    /// again after a loss the stream could not step over).
    mpu: Option<u32>,
    /// The decode index of the next AU within `mpu`.
    index: usize,
    /// The last MPU seen (synchronised or not; `None` at the start and
    /// after such a loss).
    last: Option<u32>,
    /// Audio: the AUs lost since the last one taken.
    skip: Option<usize>,
    /// Video: pictures were lost; the next is placed by its POC.
    resync: bool,
}

impl Track {
    /// Take one unit of `mpu`: whether it starts or continues an MPU the
    /// stream is synchronised to.
    fn enter(&mut self, mpu: u32) -> bool {
        if self.last != Some(mpu) {
            // An MPU boundary seen in the stream (not the first unit
            // after the start or a loss, which may join an MPU midway).
            self.mpu = self.last.is_some().then_some(mpu);
            self.index = 0;
            self.last = Some(mpu);
        }
        self.mpu.is_some()
    }
}

/// What the recorder wrote and dropped.
#[derive(Debug, Clone, Copy, Default)]
pub struct Counts {
    /// Pictures written.
    pub video: u64,
    /// Audio frames written.
    pub audio: u64,
    /// Data items written (the untimed assets').
    pub data: u64,
    /// Elementary streams in the PMT (tracks of the Matroska form).
    pub streams: usize,
    /// Attachments of the Matroska form (data packages).
    pub attachments: usize,
    /// Chapters of the Matroska form (the MH-EIT's events).
    pub chapters: usize,
    /// Of them, those at their scheduled time, no switch of the stream seen.
    pub chapters_scheduled: usize,
    /// AUs skipped before the first MPU boundary or after a loss.
    pub unsynced: u64,
    /// AUs whose MPU the descriptors do not time.
    pub untimed: u64,
    /// Pictures placed by their POC after lost packets.
    pub placed: u64,
    /// Pictures not written after lost packets: up to the IRAP after a
    /// lost one, the RASL pictures of an IRAP after a discontinuity.
    pub skipped: u64,
    /// Audio frames stepped over (lost with their packets).
    pub audio_lost: u64,
    /// Pictures whose POC did not agree with their decode index (a check
    /// of the placement; 0 on a well-formed stream).
    pub misplaced: u64,
}

/// A picture under assembly.
#[derive(Debug)]
struct Picture {
    /// Its MPU.
    mpu: u32,
    /// Its Annex B bytes.
    bytes: Vec<u8>,
    /// A slice is in it.
    vcl: bool,
    /// Its first slice segment's NAL unit type and POC lsb (when the
    /// parameter sets are known).
    first: Option<(u8, Option<u32>)>,
}

/// The last picture timed: its MPU and decode index, PTS (90 kHz, 33
/// bits) and POC lsb — where a picture after a loss is placed from.
#[derive(Debug, Clone, Copy)]
struct Anchor {
    mpu: u32,
    index: usize,
    pts: u64,
    poc: u32,
}

/// The recorder.
///
/// Fed by [`crate::Remux`]; it writes the TS to its output, or with
/// [`Session::with_mkv`] the Matroska file.
pub struct Session {
    out: Box<dyn Write + Send>,
    cc: BTreeMap<u16, u8>,
    /// TS packets written (the PSI repeat counter).
    sent: u64,
    /// The tracker's timing, refreshed at each MPT version.
    timing: BTreeMap<u16, AssetTiming>,
    /// Only this service's assets (`None`: the first service whose MPT
    /// arrives).
    service: Option<u16>,
    /// The output streams by asset packet id; the PMT's content.
    streams: BTreeMap<u16, Stream>,
    /// The asset packet id whose pictures are recorded (the first video
    /// asset; it carries the PCR).
    video: Option<u16>,
    /// The PMT's version (bumped as the streams change).
    pmt_version: u8,
    /// The picture under assembly.
    picture: Option<Picture>,
    /// The video's parameter sets as far as a slice's POC needs them.
    sps: Option<isdb_mkv::hevc::Sps>,
    pps: Option<isdb_mkv::hevc::Pps>,
    /// The SPS in force as sent (a change starts a new coded video sequence).
    sps_bytes: Option<Vec<u8>>,
    /// The last picture timed with a POC.
    anchor: Option<Anchor>,
    /// Video: an IRAP picture was lost; nothing until the next.
    need_irap: bool,
    /// Video: a picture since the last IRAP was lost whole.
    gop_broken: bool,
    /// Video: the RASL pictures of the last IRAP are not written.
    skip_rasl: bool,
    /// The last PCR written.
    pcr: Option<u64>,
    /// The earliest PTS written on any stream, unwrapped: where a
    /// player's timeline starts (the sidecar cues' zero).
    first_pts: Option<i64>,
    /// The last picture's PTS, unwrapped (the captions' arrival clock).
    last_pts: Option<i64>,
    /// The latest PTS written on any stream, unwrapped.
    latest_pts: Option<i64>,
    /// The first PTS written with its NTP time: the timeline's date.
    clock: Option<Clock>,
    /// The stream's NTP clock, as last read.
    ntp: Option<u64>,
    /// The MH-EIT's events (the Matroska form's chapters).
    schedule: Schedule,
    /// The files of `--split-events`, once finished.
    written: Vec<crate::mkv::Written>,
    /// The caption documents per asset: `(document, arrival)`, arrival
    /// the unwrapped PTS of the last picture (`None` before the first).
    captions: BTreeMap<u16, Vec<CaptionDoc>>,
    counts: Counts,
    /// The Matroska form (`--mkv`): the media go there instead of TS
    /// packets.
    mkv: Option<crate::mkv::Mkv>,
}

/// A caption document as it came.
#[derive(Debug)]
struct CaptionDoc {
    /// The TTML.
    bytes: Vec<u8>,
    /// The stream's NTP clock when it came: its arrival on the media's clock, as
    /// presentation times are, resolved at the end (it may precede the first
    /// picture, which dates the media's clock).
    ntp: Option<u64>,
    /// The last picture written when it came: its arrival without NTP packets,
    /// though that picture's presentation is some way ahead of its arrival.
    last_pts: Option<i64>,
    /// The asset's subtitle information valid for its MPU.
    info: Option<es::SubtitleInfo>,
}

/// How far a document's reference may place it from its arrival (ms): further, and
/// the reference is not the document's own. The recordings' are within 0.3 s; a
/// reference moves by a whole event.
const CAPTION_REFERENCE_TOLERANCE: i64 = 10_000;

/// One caption asset's cues for the sidecars: times in milliseconds from
/// the TS's earliest PTS (where a player's timeline starts).
#[derive(Debug, Clone)]
pub struct CaptionTrack {
    /// The asset packet id.
    pub pid: u16,
    /// The documents' `xml:lang`, when they name one.
    pub lang: Option<String>,
    /// The cues.
    pub cues: Vec<isdb_caption::Cue>,
    /// Documents that did not parse (or carried no TTML).
    pub rejected: u64,
}

impl Session {
    /// Open the recorder over `out` for `service`'s assets (`None`: the
    /// first service that appears). The PAT/PMT go out with the first
    /// MPT.
    ///
    /// # Errors
    ///
    /// None yet (the signature keeps the write's).
    pub fn new(out: Box<dyn Write + Send>, service: Option<u16>) -> std::io::Result<Self> {
        Ok(Self {
            out,
            cc: BTreeMap::new(),
            sent: 0,
            timing: BTreeMap::new(),
            service,
            streams: BTreeMap::new(),
            video: None,
            pmt_version: 0,
            picture: None,
            sps: None,
            pps: None,
            sps_bytes: None,
            anchor: None,
            need_irap: false,
            gop_broken: false,
            skip_rasl: false,
            pcr: None,
            first_pts: None,
            last_pts: None,
            latest_pts: None,
            clock: None,
            ntp: None,
            schedule: Schedule::new(),
            written: Vec::new(),
            captions: BTreeMap::new(),
            counts: Counts::default(),
            mkv: None,
        })
    }

    /// The recorder in its Matroska form: `out` unused, the media into
    /// `mkv`.
    #[must_use]
    pub fn with_mkv(mut self, mkv: crate::mkv::Mkv) -> Self {
        self.mkv = Some(mkv);
        self
    }

    /// Take the tracker's accumulated timing (whenever an MPT copy moved
    /// it on); a change of the service's asset set rewrites the PMT.
    ///
    /// # Errors
    ///
    /// The write's own.
    pub fn set_timing(&mut self, timing: &BTreeMap<u16, AssetTiming>) -> std::io::Result<()> {
        self.timing.clone_from(timing);
        if self.service.is_none() {
            self.service = timing.values().find_map(|t| t.service);
        }
        let Some(service) = self.service else {
            return Ok(());
        };
        // The service's assets as streams; a stream once in the PMT
        // stays (its pid keeps its meaning for the whole file).
        let mut changed = false;
        let mut untagged = self
            .streams
            .values()
            .filter(|s| s.ts_pid >= UNTAGGED_PID_BASE)
            .count();
        for (&pid, t) in timing.iter().filter(|(_, t)| t.service == Some(service)) {
            if self.streams.contains_key(&pid) {
                continue;
            }
            let kind = match &t.kind {
                b"hev1" | b"hvc1" => Kind::Video,
                b"mp4a" => Kind::Audio,
                // Captions go to the sidecars.
                b"stpp" => continue,
                _ => Kind::Data,
            };
            if kind == Kind::Video && self.video.is_some() {
                continue;
            }
            let ts_pid = match t.component_tag {
                Some(tag) if tag <= 0xff => STREAM_PID_BASE + tag,
                _ => {
                    untagged += 1;
                    UNTAGGED_PID_BASE + u16::try_from(untagged).unwrap_or(0xff)
                }
            };
            if self.streams.values().any(|s| s.ts_pid == ts_pid) {
                continue;
            }
            if kind == Kind::Video {
                self.video = Some(pid);
            }
            if let Some(m) = self.mkv.as_mut() {
                m.describe(
                    pid,
                    crate::mkv::Decl {
                        language: t.language.clone(),
                        name: match (kind, t.audio_component_type) {
                            (Kind::Audio, Some(ty)) => Some(audio_name(ty).to_owned()),
                            _ => None,
                        },
                    },
                );
            }
            self.streams.insert(
                pid,
                Stream {
                    ts_pid,
                    kind,
                    track: Track::default(),
                },
            );
            changed = true;
        }
        if changed && self.video.is_some() {
            if self.sent > 0 {
                self.pmt_version = (self.pmt_version + 1) & 0x1f;
            }
            self.counts.streams = self.streams.len();
            self.psi()?;
        }
        Ok(())
    }

    /// Whether `pid`'s MPU packets are recorded: an asset of the
    /// recorded service that is a stream or a caption track, or any
    /// asset of the service before the PMT is out (its first MFUs are
    /// still waiting for the clock then).
    #[must_use]
    pub fn records(&self, pid: u16) -> bool {
        let Some(t) = self.timing.get(&pid) else {
            return false;
        };
        match self.service {
            Some(s) if t.service != Some(s) => false,
            _ => self.streams.contains_key(&pid) || &t.kind == b"stpp" || !self.ready(),
        }
    }

    /// Whether the PMT is out (media waits for it; the Matroska form
    /// waits for the streams' declaration alike).
    fn ready(&self) -> bool {
        self.sent > 0
    }

    /// Packets of `pid` were lost; `units` the AUs they carried when
    /// known (one complete MFU per packet). The video's picture under
    /// assembly is written as it is (any slice in it cut at the gap) and
    /// the next is placed by its POC; an audio stream steps over the
    /// `units` frames. Otherwise (and without a picture to place from)
    /// the stream waits for the next MPU boundary it sees whole (the loss
    /// may have taken the head of the MPU after it).
    ///
    /// # Errors
    ///
    /// The write's own.
    pub fn lost(&mut self, pid: u16, units: Option<usize>) -> std::io::Result<()> {
        let video = self.video == Some(pid);
        if video {
            self.flush_picture()?;
        }
        let placeable = self.anchor.is_some() && self.sps.is_some() && self.pps.is_some();
        let Some(s) = self.streams.get_mut(&pid) else {
            return Ok(());
        };
        match (video, units) {
            (true, _) if placeable && s.track.mpu.is_some() => s.track.resync = true,
            (false, Some(n)) if s.track.mpu.is_some() => {
                s.track.skip = Some(s.track.skip.unwrap_or(0) + n);
            }
            _ => {
                s.track.mpu = None;
                s.track.last = None;
                s.track.skip = None;
                s.track.resync = false;
            }
        }
        Ok(())
    }

    /// One whole video MFU of `pid` (the NAL units with their 32-bit
    /// size prefixes, B60 §8.1.1), in decode order. The service's first
    /// video asset is the one recorded.
    ///
    /// # Errors
    ///
    /// The write's own.
    pub fn video_mfu(&mut self, pid: u16, mpu: u32, data: &[u8]) -> std::io::Result<()> {
        if self.video != Some(pid) || !self.ready() {
            return Ok(());
        }
        let Some(stream) = self.streams.get_mut(&pid) else {
            return Ok(());
        };
        if stream.track.last != Some(mpu) {
            // The MPU's last picture is complete.
            self.flush_picture()?;
        }
        let Some(stream) = self.streams.get_mut(&pid) else {
            return Ok(());
        };
        let synced = stream.track.mpu.is_some();
        if !stream.track.enter(mpu) {
            self.picture = None;
            return Ok(());
        }
        if !synced {
            // A (re)start at an MPU boundary: its IRAP's RASL pictures
            // reference pictures the file does not have.
            self.skip_rasl = true;
            self.need_irap = false;
            self.gop_broken = false;
        }
        for nal in es::nal_units(data).into_iter().flatten() {
            let Some(&first) = nal.first() else { continue };
            let ty = (first >> 1) & 0x3f;
            let vcl = ty < 32;
            let first_slice = vcl && nal.get(2).is_some_and(|b| b & 0x80 != 0);
            let has_vcl = self.picture.as_ref().is_some_and(|p| p.vcl);
            // H.265 §7.4.2.4.4: the first of these after a picture's
            // slices starts the next access unit.
            let starts = ty == 35
                || (has_vcl && (matches!(ty, 32..=34 | 39 | 41..=44 | 48..=55) || first_slice));
            if starts {
                self.flush_picture()?;
            }
            match ty {
                33 => {
                    if let Some(s) = isdb_mkv::hevc::parse_sps(nal) {
                        self.sps = Some(s);
                    }
                    if self.sps_bytes.as_deref() != Some(nal) {
                        if self.sps_bytes.is_some() {
                            self.note_sequence(pid, mpu);
                        }
                        self.sps_bytes = Some(nal.to_vec());
                    }
                }
                // End of sequence: the next picture starts a new one.
                36 => self.note_sequence(pid, mpu.wrapping_add(1)),
                34 => {
                    if let Some(p) = isdb_mkv::hevc::parse_pps(nal) {
                        self.pps = Some(p);
                    }
                }
                _ => {}
            }
            let poc = match (first_slice, &self.sps, &self.pps) {
                (true, Some(s), Some(p)) => isdb_mkv::hevc::first_slice_poc_lsb(nal, s, p),
                _ => None,
            };
            let p = self.picture.get_or_insert_with(|| Picture {
                mpu,
                bytes: Vec::new(),
                vcl: false,
                first: None,
            });
            p.bytes.extend_from_slice(&START_CODE);
            p.bytes.extend_from_slice(nal);
            p.vcl |= vcl;
            if first_slice && p.first.is_none() {
                p.first = Some((ty, poc));
            }
        }
        Ok(())
    }

    /// The decode index within `mpu` of the picture of POC lsb `poc`,
    /// from the anchor's: the index whose descriptor PTS lies the POC
    /// difference in frames from the anchor's PTS.
    fn place(&self, pid: u16, mpu: u32, poc: u32) -> Option<usize> {
        let a = self.anchor?;
        let t = self.timing.get(&pid)?;
        let range = 1i64 << self.sps.as_ref()?.log2_max_poc_lsb;
        let mut frames = (i64::from(poc) - i64::from(a.poc)).rem_euclid(range);
        if frames >= range / 2 {
            frames -= range;
        }
        let frame = frame_ticks(t, mpu)?;
        #[allow(
            clippy::cast_precision_loss,
            reason = "PTS differences, far below 2^52"
        )]
        (0..au_count(t, mpu)?).find(|&i| {
            au_times(t, mpu, i).is_some_and(|(pts, _)| {
                (pts_diff(pts, a.pts) as f64 - frames as f64 * frame).abs() < frame / 2.0
            })
        })
    }

    /// Write the picture under assembly at its MPU's next decode index
    /// (after lost packets, at the index its POC places it).
    fn flush_picture(&mut self) -> std::io::Result<()> {
        let Some(Picture {
            mpu,
            bytes,
            vcl: true,
            first,
        }) = self.picture.take()
        else {
            return Ok(());
        };
        let Some(pid) = self.video else {
            return Ok(());
        };
        let Some(stream) = self.streams.get(&pid) else {
            return Ok(());
        };
        let (ty, poc) = first.unwrap_or((0, None));
        let mut index = stream.track.index;
        if stream.track.resync {
            let placed = poc.and_then(|poc| self.place(pid, mpu, poc));
            let anchor = self.anchor;
            let Some(stream) = self.streams.get_mut(&pid) else {
                return Ok(());
            };
            stream.track.resync = false;
            match (placed, anchor) {
                (Some(i), Some(a)) if a.mpu != mpu || i >= index => {
                    tracing::debug!(pid, mpu, from = index, to = i, "picture placed by its POC");
                    let tail_lost = a.mpu != mpu
                        && (mpu != a.mpu.wrapping_add(1)
                            || self
                                .timing
                                .get(&pid)
                                .and_then(|t| au_count(t, a.mpu))
                                .is_none_or(|n| a.index + 1 < n));
                    if a.mpu != mpu && i > 0 {
                        // The MPU's first picture, its IRAP, is gone: so
                        // is everything up to the next.
                        self.need_irap = true;
                        self.gop_broken = true;
                    } else if tail_lost || (a.mpu == mpu && i > a.index + 1) {
                        // Pictures lost whole: what references them
                        // decodes with errors, the RASL pictures of the
                        // next IRAP are not written.
                        self.gop_broken = true;
                    }
                    index = i;
                    self.counts.placed += 1;
                }
                _ => {
                    // Not placed: the next MPU boundary resynchronises.
                    tracing::debug!(pid, mpu, ?poc, "picture after a loss not placed");
                    stream.track.mpu = None;
                    stream.track.last = Some(mpu);
                    self.counts.unsynced += 1;
                    return Ok(());
                }
            }
        } else if let Some(poc) = poc
            && self.anchor.is_some()
            && self.place(pid, mpu, poc) != Some(index)
        {
            tracing::debug!(
                pid,
                mpu,
                index,
                poc,
                "picture's POC disagrees with its index"
            );
            self.counts.misplaced += 1;
        }
        let Some(stream) = self.streams.get_mut(&pid) else {
            return Ok(());
        };
        stream.track.index = index + 1;
        let ts_pid = stream.ts_pid;
        let Some((pts, dts)) = self.timing.get(&pid).and_then(|t| au_times(t, mpu, index)) else {
            tracing::debug!(pid, mpu, index, "picture untimed");
            self.counts.untimed += 1;
            return Ok(());
        };
        if let Some(poc) = poc {
            self.anchor = Some(Anchor {
                mpu,
                index,
                pts,
                poc,
            });
        }
        // What a loss left undecodable is not written.
        if (16..=23).contains(&ty) {
            self.need_irap = false;
            self.skip_rasl |= self.gop_broken;
            self.gop_broken = false;
        } else if self.need_irap || (self.skip_rasl && matches!(ty, 8 | 9)) {
            self.counts.skipped += 1;
            return Ok(());
        } else if ty <= 5 {
            // A trailing picture: the IRAP's leading pictures are over.
            self.skip_rasl = false;
        }
        let pcr = dts.wrapping_sub(PCR_LEAD) & TS_MASK;
        self.pcr = Some(pcr);
        let pts_unwrapped = unwrap(self.last_pts.or(self.first_pts), pts);
        self.note_written(pid, mpu, index, pts_unwrapped);
        self.last_pts = Some(pts_unwrapped);
        self.counts.video += 1;
        if let Some(m) = self.mkv.as_mut() {
            let dts_unwrapped = pts_unwrapped - (pts.wrapping_sub(dts) & TS_MASK) as i64;
            return m.picture(pid, pts_unwrapped, dts_unwrapped, &bytes);
        }
        self.pes(
            ts_pid,
            VIDEO_SID,
            PtsDts::pts_and_dts(pts, dts),
            Some(pcr),
            &bytes,
        )
    }

    /// One whole audio MFU of `pid` (one `AudioMuxElement`), written as a
    /// LOAS frame on the asset's own stream.
    ///
    /// # Errors
    ///
    /// The write's own.
    pub fn audio_mfu(&mut self, pid: u16, mpu: u32, data: &[u8]) -> std::io::Result<()> {
        if !self.ready() {
            return Ok(());
        }
        let Some(stream) = self.streams.get_mut(&pid).filter(|s| s.kind == Kind::Audio) else {
            return Ok(());
        };
        if let Some(n) = stream.track.skip.take() {
            // Step over the `n` lost frames from the last one taken.
            let timing = self.timing.get(&pid);
            let at = stream.track.mpu.and_then(|mut m| {
                let mut i = stream.track.index + n;
                loop {
                    let count = au_count(timing?, m).filter(|&c| c > 0)?;
                    if i < count {
                        return (m == mpu).then_some(i);
                    }
                    i -= count;
                    m = m.wrapping_add(1);
                }
            });
            stream.track.last = Some(mpu);
            stream.track.mpu = at.map(|_| mpu);
            stream.track.index = at.unwrap_or(0);
            if at.is_some() {
                self.counts.audio_lost += n as u64;
            } else {
                tracing::debug!(
                    pid,
                    mpu,
                    lost = n,
                    "audio frames lost: resuming at the next MPU"
                );
            }
        }
        if !stream.track.enter(mpu) {
            self.counts.unsynced += 1;
            return Ok(());
        }
        let index = stream.track.index;
        stream.track.index += 1;
        let ts_pid = stream.ts_pid;
        let Some((pts, _)) = self.timing.get(&pid).and_then(|t| au_times(t, mpu, index)) else {
            tracing::debug!(pid, mpu, index, "audio frame untimed");
            self.counts.untimed += 1;
            return Ok(());
        };
        let mut frame = Vec::with_capacity(data.len() + 3);
        if !es::loas_frame(data, &mut frame) {
            return Ok(());
        }
        self.counts.audio += 1;
        let pts_unwrapped = unwrap(self.last_pts.or(self.first_pts), pts);
        self.note_written(pid, mpu, index, pts_unwrapped);
        if let Some(m) = self.mkv.as_mut() {
            return m.audio(pid, pts_unwrapped, data);
        }
        self.pes(ts_pid, AUDIO_SID, PtsDts::pts_only(pts), None, &frame)
    }

    /// One whole MFU of an untimed asset `pid` (one item of its package):
    /// a `private_stream_2` PES prefixed by the MPU sequence number and
    /// the item id (see the module documentation).
    ///
    /// # Errors
    ///
    /// The write's own.
    pub fn data_mfu(
        &mut self,
        pid: u16,
        mpu: u32,
        item: Option<u32>,
        data: &[u8],
    ) -> std::io::Result<()> {
        if !self.ready() {
            return Ok(());
        }
        let Some(stream) = self.streams.get(&pid).filter(|s| s.kind == Kind::Data) else {
            return Ok(());
        };
        let ts_pid = stream.ts_pid;
        if let Some(m) = self.mkv.as_mut() {
            m.data(pid, mpu, item, data, self.latest_pts);
            self.counts.data += 1;
            return Ok(());
        }
        let item = item.unwrap_or(0);
        let mut rest = data;
        let mut first = true;
        loop {
            let take = rest.len().min(PES_MAX_DATA - DATA_PREFIX);
            let (chunk, tail) = rest.split_at(take);
            let mut body = Vec::with_capacity(DATA_PREFIX + take);
            body.extend_from_slice(&mpu.to_be_bytes());
            body.extend_from_slice(&item.to_be_bytes());
            body.push(u8::from(!first));
            body.extend_from_slice(chunk);
            let packet = PesPacket {
                stream_id: PRIVATE_2_SID,
                unbounded: false,
                body: PesBody::Data(body),
            };
            let bytes = packet
                .encode()
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
            self.packetise(ts_pid, None, &bytes)?;
            rest = tail;
            first = false;
            if rest.is_empty() {
                break;
            }
        }
        self.counts.data += 1;
        Ok(())
    }

    /// Access unit `index` of `mpu` of `pid` is written, at the unwrapped
    /// `pts`: the timeline's span, and its date from the first one.
    fn note_written(&mut self, pid: u16, mpu: u32, index: usize, pts: i64) {
        self.first_pts = Some(self.first_pts.map_or(pts, |f| f.min(pts)));
        self.latest_pts = Some(self.latest_pts.map_or(pts, |l| l.max(pts)));
        if self.clock.is_none() {
            self.clock = self
                .timing
                .get(&pid)
                .and_then(|t| au_ntp(t, mpu, index))
                .map(|ntp| Clock { ticks: pts, ntp });
        }
    }

    /// A new coded video sequence starts at `mpu`: for the chapters.
    fn note_sequence(&mut self, pid: u16, mpu: u32) {
        if self.mkv.is_none() {
            return;
        }
        let at = self.timing.get(&pid).and_then(|t| au_ntp(t, mpu, 0));
        if let (Some(service), Some(ntp)) = (self.service, at) {
            self.schedule.sequence_start(service, ntp);
        }
    }

    /// The files of `--split-events`, after [`Session::finish`].
    pub fn take_written(&mut self) -> Vec<crate::mkv::Written> {
        std::mem::take(&mut self.written)
    }

    /// One table of a control message: the Matroska form keeps the MH-EIT's
    /// events and the MPT versions for its chapters.
    pub fn table(&mut self, t: &TableBytes) {
        if self.mkv.is_some() {
            self.schedule.feed(t);
        }
    }

    /// The stream's NTP clock read `ntp` (B60 §3.2).
    pub fn ntp(&mut self, ntp: u64) {
        self.ntp = Some(ntp);
    }

    /// One whole caption MFU of `pid`, of MPU `mpu` (B60 Table 9-1): its TTML
    /// document is kept, with its arrival and clock, for [`Session::finish`].
    pub fn caption_mfu(&mut self, pid: u16, mpu: u32, data: &[u8]) {
        if let Some(sub) = es::Subtitle::parse(data)
            && sub.data_type == 0
        {
            let doc = CaptionDoc {
                bytes: sub.data.to_vec(),
                ntp: self.ntp,
                last_pts: self.last_pts,
                info: self.timing.get(&pid).and_then(|t| t.subtitle_at(mpu)),
            };
            self.captions.entry(pid).or_default().push(doc);
        }
    }

    /// Write the last picture and flush the output; the caption tracks.
    ///
    /// # Errors
    ///
    /// The write's own.
    pub fn finish(&mut self) -> std::io::Result<(Counts, Vec<CaptionTrack>)> {
        self.flush_picture()?;
        self.out.flush()?;
        let tracks: Vec<CaptionTrack> = std::mem::take(&mut self.captions)
            .into_iter()
            .map(|(pid, docs)| self.caption_track(pid, &docs))
            .collect();
        let span = (self.service, self.clock, self.first_pts, self.latest_pts);
        if let Some(m) = self.mkv.take_if(|m| m.splits()) {
            // With nothing written there is no file; the sink removes its scratch.
            if let (Some(service), Some(clock), Some(origin), Some(end)) = span {
                let aired = self.schedule.aired(service, clock, origin, end);
                let channel = self
                    .schedule
                    .channel(service)
                    .map_or_else(|| format!("service-{service:04x}"), str::to_owned);
                let parts = crate::split::plan(&aired, origin, end);
                self.written = m.finish_parts(&tracks, self.first_pts, &parts, clock, &channel)?;
                self.counts.streams = self.written.iter().map(|w| w.tracks).max().unwrap_or(0);
                self.counts.attachments = self.written.iter().map(|w| w.attachments).sum();
                self.counts.chapters_scheduled = aired.iter().filter(|e| e.scheduled).count();
            }
        }
        if let Some(mut m) = self.mkv.take() {
            if let (Some(service), Some(clock), Some(origin), Some(end)) = span {
                let chapters = self.schedule.chapters(service, clock, origin, end);
                self.counts.chapters = chapters.list.len();
                self.counts.chapters_scheduled = chapters.scheduled;
                for c in chapters.list {
                    m.chapter(c);
                }
            }
            let (n, attachments) = m.finish(&tracks, self.first_pts)?;
            self.counts.streams = n;
            self.counts.attachments = attachments;
        }
        Ok((self.counts, tracks))
    }

    /// The cues of one caption asset on the TS timeline.
    fn caption_track(&self, pid: u16, docs: &[CaptionDoc]) -> CaptionTrack {
        let mut rejected = 0;
        let parsed: Vec<_> = docs
            .iter()
            .filter_map(|doc| match isdb_caption::parse(&doc.bytes) {
                Ok(d) => Some((d, doc)),
                Err(e) => {
                    tracing::debug!(pid, error = %e, "caption document rejected");
                    rejected += 1;
                    None
                }
            })
            .collect();
        // Milliseconds from the start of the TS's timeline.
        let origin = self.first_pts.unwrap_or(0);
        let ms = |pts: i64| (pts - origin) / 90;
        let mut timeline = isdb_caption::Timeline::new();
        let mut by_arrival = 0;
        for (d, doc) in &parsed {
            let arrival = match (self.clock, doc.ntp) {
                (Some(clock), Some(ntp)) => Some(ms(clock.ticks_of(ntp))),
                _ => doc.last_pts.map(ms),
            };
            let first = d.blocks.iter().find_map(|b| b.begin);
            // The time code's origin: where the first block's time places it.
            let reference = doc
                .info
                .and_then(|i| i.reference_start_time)
                .zip(self.clock);
            let placed = |t: i64, (r, c): (u64, Clock)| ms(c.ticks_after(r, t));
            let reference = reference.filter(|&rc| match (first, arrival) {
                (Some(f), Some(a)) => {
                    let near = (placed(f, rc) - a).abs() <= CAPTION_REFERENCE_TOLERANCE;
                    by_arrival += u32::from(!near);
                    near
                }
                _ => true,
            });
            if let Some(rc) = reference {
                timeline.push(d, |t| Some(placed(t, rc)), arrival);
            } else {
                // The first block at the arrival, the rest as far after it as
                // the document says.
                timeline.push(d, |t| Some(arrival? + t - first?), arrival);
            }
        }
        if by_arrival > 0 {
            tracing::debug!(
                pid,
                by_arrival,
                "caption documents far from their reference: placed by their arrival"
            );
        }
        let modes: std::collections::BTreeSet<u8> = docs
            .iter()
            .filter_map(|d| d.info.map(|i| i.time_control))
            .filter(|&m| {
                !matches!(
                    m,
                    es::SubtitleInfo::TMD_REFERENCE | es::SubtitleInfo::TMD_NONE
                )
            })
            .collect();
        for mode in modes {
            tracing::warn!(
                pid,
                mode,
                "captions: time control mode (B60 Table 9-7) not implemented; \
                 its documents are placed by their arrival"
            );
        }
        let end = self.last_pts.map(ms);
        let cues = timeline
            .finish(end)
            .into_iter()
            .filter(|c| c.end > 0)
            .map(|mut c| {
                c.start = c.start.max(0);
                c
            })
            .collect();
        CaptionTrack {
            pid,
            lang: parsed.iter().find_map(|(d, _)| d.lang.clone()),
            cues,
            rejected,
        }
    }

    /// The PAT and the PMT (the Matroska form only marks the streams
    /// declared).
    fn psi(&mut self) -> std::io::Result<()> {
        if self.mkv.is_some() {
            self.sent = self.sent.max(1);
            return Ok(());
        }
        // PAT: the service's program → the PMT.
        let program = self.service.unwrap_or(1).max(1);
        let mut pat = vec![0x00, 0x00, 0x00, 0x00, 0x01, 0xc1, 0x00, 0x00];
        pat.extend(program.to_be_bytes());
        pat.extend((0xe000 | PMT_PID).to_be_bytes());
        self.section(PAT_PID, pat)?;
        // PMT: the PCR on the video, a stream per asset in pid order, no
        // descriptors.
        let pcr_pid = self
            .video
            .and_then(|v| self.streams.get(&v))
            .map_or(0x1fff, |s| s.ts_pid);
        let mut pmt = vec![0x02, 0x00, 0x00];
        pmt.extend(program.to_be_bytes());
        pmt.extend([0xc1 | (self.pmt_version << 1), 0x00, 0x00]);
        pmt.extend((0xe000 | pcr_pid).to_be_bytes());
        pmt.extend(0xf000u16.to_be_bytes());
        let mut streams: Vec<_> = self.streams.values().map(|s| (s.ts_pid, s.kind)).collect();
        streams.sort_unstable_by_key(|&(ts_pid, _)| ts_pid);
        for (ts_pid, kind) in streams {
            pmt.push(match kind {
                Kind::Video => STREAM_TYPE_HEVC,
                Kind::Audio => STREAM_TYPE_LATM,
                Kind::Data => STREAM_TYPE_PRIVATE_PES,
            });
            pmt.extend((0xe000 | ts_pid).to_be_bytes());
            pmt.extend(0xf000u16.to_be_bytes());
        }
        self.section(PMT_PID, pmt)
    }

    /// One PSI section in one packet: `s` is the section from its
    /// `table_id` with a placeholder length; the length and CRC are
    /// filled in here.
    #[allow(
        clippy::cast_possible_truncation,
        reason = "a section within one packet"
    )]
    fn section(&mut self, pid: u16, mut s: Vec<u8>) -> std::io::Result<()> {
        let len = s.len() - 3 + 4;
        s[1] = 0xb0 | (len >> 8) as u8;
        s[2] = len as u8;
        s.extend_from_slice(&crc32(&s).to_be_bytes());
        let mut p = Vec::with_capacity(TS_PACKET);
        p.extend_from_slice(&[
            0x47,
            0x40 | (pid >> 8) as u8,
            pid as u8,
            0x10 | self.next_cc(pid),
        ]);
        p.push(0x00); // pointer_field
        p.extend_from_slice(&s);
        p.resize(TS_PACKET, 0xff);
        self.write_packet(&p)
    }

    fn next_cc(&mut self, pid: u16) -> u8 {
        let e = self.cc.entry(pid).or_insert(0);
        let c = *e;
        *e = (c + 1) & 0x0f;
        c
    }

    fn write_packet(&mut self, p: &[u8]) -> std::io::Result<()> {
        self.out.write_all(p)?;
        self.sent += 1;
        if self.sent.is_multiple_of(PSI_REPEAT) {
            self.psi()?;
        }
        Ok(())
    }

    /// Encode one PES with its header (13818-1 §2.4.3) and packetise it.
    fn pes(
        &mut self,
        pid: u16,
        sid: u8,
        pts_dts: PtsDts,
        pcr: Option<u64>,
        au: &[u8],
    ) -> std::io::Result<()> {
        let header = PesHeader {
            marker_bits: 0b10,
            scrambling_control: 0,
            priority: false,
            data_alignment_indicator: true,
            copyright: false,
            original_or_copy: false,
            pts_dts,
            es_rate: None,
            escr: None,
            trick_mode: None,
            additional_copy_info: None,
            previous_pes_packet_crc: None,
            extension: None,
            stuffing: Vec::new(),
        };
        // A picture can exceed PES_packet_length's 16 bits: the video's
        // are unbounded (§2.4.3.7 allows it on a video stream).
        let packet = PesPacket {
            stream_id: sid,
            unbounded: sid == VIDEO_SID,
            body: PesBody::Header {
                header,
                data: au.to_vec(),
            },
        };
        let bytes = packet
            .encode()
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        self.packetise(pid, pcr, &bytes)
    }

    /// One encoded PES into TS packets of `pid`: the PCR, when given, in
    /// the adaptation field of the first; the last stuffed through its
    /// adaptation field.
    #[allow(
        clippy::cast_possible_truncation,
        reason = "the adaptation field lengths are < 184"
    )]
    fn packetise(&mut self, pid: u16, pcr: Option<u64>, bytes: &[u8]) -> std::io::Result<()> {
        let mut left = bytes;
        let mut first = true;
        while !left.is_empty() {
            let pcr_here = first.then_some(pcr).flatten();
            // The adaptation field's fixed bytes: length + flags (+ PCR).
            let fixed = if pcr_here.is_some() { 8 } else { 0 };
            let take = left.len().min(TS_PAYLOAD - fixed);
            // Stuffing fills what the payload leaves; an adaptation field
            // of length 0 is the single byte of a 183-byte payload.
            let af = TS_PAYLOAD - take;
            let (chunk, rest) = left.split_at(take);
            left = rest;
            let mut p = Vec::with_capacity(TS_PACKET);
            let pusi = if first { 0x40 } else { 0x00 };
            let afc = if af > 0 { 0x30 } else { 0x10 };
            p.extend_from_slice(&[
                0x47,
                pusi | (pid >> 8) as u8,
                pid as u8,
                afc | self.next_cc(pid),
            ]);
            if af > 0 {
                p.push((af - 1) as u8);
                if af > 1 {
                    p.push(if pcr_here.is_some() { 0x10 } else { 0x00 });
                    if let Some(pcr) = pcr_here {
                        p.extend_from_slice(&pcr_bytes(pcr));
                    }
                    p.resize(4 + af, 0xff);
                }
            }
            p.extend_from_slice(chunk);
            self.write_packet(&p)?;
            first = false;
        }
        Ok(())
    }
}

/// A track name for an audio `component_type` (B60 Table 7-31 / B24).
fn audio_name(ty: u8) -> &'static str {
    match ty {
        0x01 => "mono",
        0x02 => "dual mono",
        0x03 => "stereo",
        0x04 => "2/1",
        0x05 => "3/0",
        0x06 => "2/2",
        0x07 => "3/1",
        0x08 => "3/2",
        0x09 => "5.1ch",
        0x0a => "3/3.1",
        0x0b => "2/0/0-2/0/2-0.1",
        0x0c => "5/2.1",
        0x0d => "3/2/2.1",
        0x0e => "2/0/0-3/0/2-0.1",
        0x0f => "0/2/0-3/0/2-0.1",
        0x10 => "2/0/0-3/2/3-0.2",
        0x11 => "22.2ch",
        _ => "audio",
    }
}

/// `a − b` on the 33-bit ring, the nearest way round.
fn pts_diff(a: u64, b: u64) -> i64 {
    const RING: i64 = 1 << 33;
    let d = i64::try_from(a.wrapping_sub(b) & TS_MASK).unwrap_or(0);
    if d >= RING / 2 { d - RING } else { d }
}

/// A 33-bit PTS unwrapped against the previous unwrapped one (the
/// nearest of its 2^33 aliases).
fn unwrap(prev: Option<i64>, pts: u64) -> i64 {
    const RING: i64 = 1 << 33;
    let pts = i64::try_from(pts).unwrap_or(0);
    let Some(prev) = prev else { return pts };
    let k = (prev - pts + RING / 2).div_euclid(RING);
    pts + k * RING
}

/// The PCR field: the 33-bit base at 90 kHz, 6 reserved bits, a zero
/// extension.
fn pcr_bytes(base: u64) -> [u8; 6] {
    let b = base & TS_MASK;
    #[allow(clippy::cast_possible_truncation, reason = "byte slices of the base")]
    [
        (b >> 25) as u8,
        (b >> 17) as u8,
        (b >> 9) as u8,
        (b >> 1) as u8,
        ((b & 1) as u8) << 7 | 0x7e,
        0x00,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    /// A `Write` the test reads back.
    #[derive(Clone, Default)]
    struct Shared(Arc<Mutex<Vec<u8>>>);
    impl Write for Shared {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    fn packets(out: &Shared) -> Vec<[u8; TS_PACKET]> {
        let b = out.0.lock().unwrap();
        assert_eq!(b.len() % TS_PACKET, 0);
        b.chunks(TS_PACKET).map(|c| c.try_into().unwrap()).collect()
    }

    /// A service of a video (tag 0), two audio (16, 17), a caption (48)
    /// and a data asset (64).
    fn timing() -> BTreeMap<u16, AssetTiming> {
        [
            (0xf300, *b"hev1", 0),
            (0xf310, *b"mp4a", 16),
            (0xf311, *b"mp4a", 17),
            (0xf330, *b"stpp", 48),
            (0xf340, *b"aapp", 64),
        ]
        .into_iter()
        .map(|(pid, kind, tag)| {
            (
                pid,
                AssetTiming {
                    kind,
                    component_tag: Some(tag),
                    service: Some(0x65),
                    ..AssetTiming::default()
                },
            )
        })
        .collect()
    }

    /// The payload bytes of `pid`'s packets.
    fn payload(ps: &[[u8; TS_PACKET]], pid: u16) -> Vec<u8> {
        let mut out = Vec::new();
        for p in ps
            .iter()
            .filter(|p| u16::from_be_bytes([p[1] & 0x1f, p[2]]) == pid)
        {
            let start = match p[3] & 0x30 {
                0x10 => 4,
                0x30 => 5 + usize::from(p[4]),
                _ => continue,
            };
            out.extend_from_slice(&p[start..]);
        }
        out
    }

    #[test]
    fn pmt_lists_the_service_streams_once_known() {
        let out = Shared::default();
        let mut s = Session::new(Box::new(out.clone()), None).unwrap();
        assert!(packets(&out).is_empty(), "nothing before the MPT");
        s.set_timing(&timing()).unwrap();
        let ps = packets(&out);
        assert_eq!(ps.len(), 2);
        for (p, table_id) in ps.iter().zip([0x00u8, 0x02]) {
            assert_eq!(p[3] & 0x30, 0x10, "payload only");
            assert_eq!(p[4], 0, "pointer_field");
            assert_eq!(p[5], table_id);
            let len = usize::from(u16::from_be_bytes([p[6], p[7]]) & 0x0fff);
            assert_eq!(crc32(&p[5..5 + 3 + len]), 0, "the CRC covers the section");
        }
        // PMT: program 0x65, PCR on 0x100, then (type, pid) per stream.
        let pmt = &ps[1][5..];
        assert_eq!(u16::from_be_bytes([pmt[3], pmt[4]]), 0x65);
        assert_eq!(u16::from_be_bytes([pmt[8], pmt[9]]) & 0x1fff, 0x100);
        let len = usize::from(u16::from_be_bytes([pmt[1], pmt[2]]) & 0x0fff);
        let streams: Vec<(u8, u16)> = pmt[12..3 + len - 4]
            .chunks(5)
            .map(|e| (e[0], u16::from_be_bytes([e[1], e[2]]) & 0x1fff))
            .collect();
        assert_eq!(
            streams,
            vec![(0x24, 0x100), (0x11, 0x110), (0x11, 0x111), (0x06, 0x140)],
            "the caption is a sidecar"
        );
        // The same set again: no new PMT.
        s.set_timing(&timing()).unwrap();
        assert_eq!(packets(&out).len(), 2);
    }

    #[test]
    fn data_items_are_private_stream_2() {
        let out = Shared::default();
        let mut s = Session::new(Box::new(out.clone()), None).unwrap();
        s.set_timing(&timing()).unwrap();
        let item = vec![0x5au8; 70_000];
        // An item id with its top bit set keeps it.
        s.data_mfu(0xf340, 7, Some(0xd1ed_bf3b), &item).unwrap();
        s.data_mfu(0xf999, 7, Some(3), &item).unwrap();
        let bytes = payload(&packets(&out), 0x140);
        // Two PESs: 65526 + 4474 bytes of the item after the prefixes.
        let first = &bytes[..6 + 0xffff];
        assert_eq!(&first[..4], &[0, 0, 1, 0xbf]);
        assert_eq!(u16::from_be_bytes([first[4], first[5]]), 0xffff);
        assert_eq!(&first[6..15], &[0, 0, 0, 7, 0xd1, 0xed, 0xbf, 0x3b, 0]);
        let second = &bytes[6 + 0xffff..];
        assert_eq!(&second[..4], &[0, 0, 1, 0xbf]);
        assert_eq!(
            &second[6..15],
            &[0, 0, 0, 7, 0xd1, 0xed, 0xbf, 0x3b, 1],
            "a continuation"
        );
        let n = usize::from(u16::from_be_bytes([second[4], second[5]]));
        assert_eq!(n - DATA_PREFIX + 0xffff - DATA_PREFIX, item.len());
        assert_eq!(s.counts.data, 1, "an unknown asset writes nothing");
    }

    #[test]
    fn pes_packetisation_sizes() {
        for n in [1usize, 160, 170, 175, 176, 177, 182, 183, 184, 1000, 70_000] {
            let out = Shared::default();
            let mut s = Session::new(Box::new(out.clone()), None).unwrap();
            let au = vec![0xa5u8; n];
            s.pes(
                0x100,
                VIDEO_SID,
                PtsDts::pts_and_dts(9000, 6000),
                Some(3000),
                &au,
            )
            .unwrap();
            let ps = packets(&out);
            let bytes = payload(&ps, 0x100);
            assert_eq!(&bytes[bytes.len() - n..], au.as_slice(), "n {n}");
            assert_eq!(ps[0][5] & 0x10, 0x10, "PCR flag, n {n}");
        }
    }

    /// `timing()` with the descriptors' clock: the video's on-air row
    /// (the 8-picture cycle ×4) and the AAC's (4 frames) for every MPU.
    fn timed() -> BTreeMap<u16, AssetTiming> {
        use isdb_mmt::desc::MpuExtended;
        let mut t = timing();
        let v = t.get_mut(&0xf300).unwrap();
        v.timescale = Some(180_000);
        v.mpu_times.insert(100, 1000 << 32);
        let cycle = [30030u16, 15014, 6006, 0, 3004, 9008, 3004, 6006];
        v.rows.insert(
            100,
            MpuExtended {
                mpu_sequence_number: 100,
                leap_indicator: 0,
                decoding_time_offset: 9010,
                access_units: cycle
                    .iter()
                    .cycle()
                    .take(32)
                    .map(|&d| (d, Some(3003)))
                    .collect(),
            },
        );
        let a = t.get_mut(&0xf310).unwrap();
        a.timescale = Some(180_000);
        a.mpu_times.insert(8, 1000 << 32);
        a.rows.insert(
            8,
            MpuExtended {
                mpu_sequence_number: 8,
                leap_indicator: 0,
                decoding_time_offset: 0,
                access_units: vec![(0, Some(3840)); 4],
            },
        );
        t
    }

    /// The length-prefixed NAL units of one picture's MFU: an AUD, the
    /// BS17 parameter sets ahead of an IRAP, and a slice of NAL type `ty`
    /// whose header carries `poc` (pps 0, slice type B).
    fn picture(ty: u8, poc: u16) -> Vec<u8> {
        let hex = |s: &str| -> Vec<u8> {
            (0..s.len())
                .step_by(2)
                .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
                .collect()
        };
        let mut nals = vec![vec![0x46, 0x01, 0x50]];
        if (16..=23).contains(&ty) {
            nals.push(hex(
                "40010c06ffff022000000300b00000030000030099000018824090",
            ));
            nals.push(hex("420106022000000300b000000300000300990000a001e020021c4d8d18826490a594e0284244826ca500000303e90000ea60803e9c00814bc00004c4b4000004c5ec5c00004c4b4000004c5ec5c00004c4b4000004c5ec5c00004c4b4000004c5ec440"));
            nals.push(hex("4401c076"));
        }
        // first_slice_segment_in_pic 1, [no_output_of_prior_pics 0],
        // pps id ue 0, slice type ue 0, the 16-bit POC lsb, 1s after.
        let mut bits: Vec<u8> = vec![1];
        if (16..=23).contains(&ty) {
            bits.push(0);
        }
        bits.extend([1, 1]);
        bits.extend((0..16).rev().map(|k| u8::from(poc >> k & 1 == 1)));
        while !bits.len().is_multiple_of(8) {
            bits.push(1);
        }
        let mut slice = vec![ty << 1, 0x01];
        slice.extend(
            bits.chunks(8)
                .map(|c| c.iter().fold(0u8, |a, &b| a << 1 | b)),
        );
        slice.extend([0xff; 8]);
        nals.push(slice);
        let mut mfu = Vec::new();
        for n in nals {
            mfu.extend(u32::try_from(n.len()).unwrap().to_be_bytes());
            mfu.extend(n);
        }
        mfu
    }

    /// The decode-order POC of picture `i` of MPU `mpu` (display frames
    /// `7 3 1 0 2 5 4 6` per cycle, 32 per MPU) and its NAL type: the
    /// CRA, its seven RASL_R, then TRAIL_R.
    fn pic_of(mpu: u32, i: usize) -> (u8, u16) {
        const DISPLAY: [u32; 8] = [7, 3, 1, 0, 2, 5, 4, 6];
        let frame = mpu * 32 + u32::try_from(i / 8 * 8).unwrap() + DISPLAY[i % 8];
        let poc = u16::try_from((40_000 + frame) & 0xffff).unwrap();
        let ty = match i {
            0 => 21,
            1..=7 => 9,
            _ => 1,
        };
        (ty, poc)
    }

    fn feed(s: &mut Session, mpu: u32, pics: impl IntoIterator<Item = usize>) {
        for i in pics {
            let (ty, poc) = pic_of(mpu, i);
            s.video_mfu(0xf300, mpu, &picture(ty, poc)).unwrap();
        }
    }

    #[test]
    fn video_loss_places_by_poc_and_drops_what_cannot_decode() {
        let mut s = Session::new(Box::new(std::io::sink()), None).unwrap();
        s.set_timing(&timed()).unwrap();
        // Joined mid-MPU: nothing until MPU 100; its RASL pictures
        // reference what came before the recording.
        feed(&mut s, 99, [31]);
        feed(&mut s, 100, 0..32);
        // MPU 101: picture 11 lost whole, picture 20 cut short (its
        // slice arrives truncated, then the gap).
        feed(&mut s, 101, 0..11);
        s.lost(0xf300, None).unwrap();
        feed(&mut s, 101, 12..20);
        let (ty, poc) = pic_of(101, 20);
        let whole = picture(ty, poc);
        let cut = crate::reassemble::cut_nal_units(&whole[..whole.len() - 4]).unwrap();
        assert_eq!(
            cut.len(),
            whole.len() - 4,
            "the slice's length cut to what came"
        );
        s.video_mfu(0xf300, 101, &cut).unwrap();
        s.lost(0xf300, None).unwrap();
        feed(&mut s, 101, 21..32);
        // MPU 102: the CRA's RASL pictures reference picture 11.
        feed(&mut s, 102, 0..32);
        // MPU 103: only a slice cut short; MPU 104's RASL pictures are
        // whole again.
        feed(&mut s, 103, 0..5);
        s.lost(0xf300, None).unwrap();
        feed(&mut s, 103, 5..32);
        feed(&mut s, 104, 0..9);
        let (c, _) = s.finish().unwrap();
        assert_eq!(c.placed, 3);
        assert_eq!(c.misplaced, 0, "every later picture's POC agrees");
        assert_eq!(c.skipped, 14, "the RASL pictures of MPUs 100 and 102");
        assert_eq!(c.video, 25 + 31 + 25 + 32 + 9);
        assert_eq!(c.untimed, 0);
    }

    #[test]
    fn video_loss_of_an_irap_waits_for_the_next() {
        let mut s = Session::new(Box::new(std::io::sink()), None).unwrap();
        s.set_timing(&timed()).unwrap();
        feed(&mut s, 99, [31]);
        feed(&mut s, 100, 0..30);
        // Pictures 30, 31 of MPU 100 and 0 of MPU 101 lost.
        s.lost(0xf300, None).unwrap();
        feed(&mut s, 101, 1..32);
        feed(&mut s, 102, 0..9);
        let (c, _) = s.finish().unwrap();
        assert_eq!(c.placed, 1);
        assert_eq!(c.misplaced, 0);
        // MPU 100's and MPU 102's RASL pictures, MPU 101 but its first.
        assert_eq!(c.skipped, 7 + 31 + 7);
        assert_eq!(c.video, 23 + 2);
    }

    #[test]
    fn audio_loss_steps_over_the_lost_frames() {
        let mut s = Session::new(Box::new(std::io::sink()), None).unwrap();
        s.set_timing(&timed()).unwrap();
        let frame = [0x20u8, 0x00, 0x11];
        let au = |s: &mut Session, mpu: u32| s.audio_mfu(0xf310, mpu, &frame).unwrap();
        au(&mut s, 7);
        for _ in 0..4 {
            au(&mut s, 8);
        }
        // Frames 0 and 1 of MPU 9 lost.
        s.lost(0xf310, Some(2)).unwrap();
        au(&mut s, 9);
        assert_eq!(s.streams[&0xf310].track.index, 3, "the frame took index 2");
        au(&mut s, 9);
        // Three lost across the boundary: MPU 10's frames 0–2.
        s.lost(0xf310, Some(3)).unwrap();
        au(&mut s, 10);
        assert_eq!(s.streams[&0xf310].track.index, 4);
        // A count that does not fit (frame 0 of MPU 11 expected, MPU 12
        // came): the next MPU boundary resynchronises.
        s.lost(0xf310, Some(1)).unwrap();
        au(&mut s, 12);
        au(&mut s, 13);
        assert_eq!(s.counts.audio, 4 + 2 + 1 + 1);
        assert_eq!(s.counts.audio_lost, 5);
        assert_eq!(s.counts.unsynced, 2);
    }

    /// An MH-EIT section of service 0x65 with one event named `prog`.
    fn eit_section(table_id: u8, id: u16, mjd: u16, hms: [u8; 3], duration: [u8; 3]) -> TableBytes {
        let name = b"prog";
        let name_len = u8::try_from(name.len()).unwrap();
        let mut d = vec![0xF0, 0x01, 0, 6 + name_len];
        d.extend(b"jpn");
        d.push(name_len);
        d.extend(name);
        d.extend([0, 0]);
        let mut s = vec![table_id, 0xf0, 0, 0x00, 0x65, 0xc1, 0, 0];
        s.extend([0xb1, 0x10, 0, 0x0b, 0, table_id]);
        s.extend(id.to_be_bytes());
        s.extend(mjd.to_be_bytes());
        s.extend(hms);
        s.extend(duration);
        s.extend(u16::try_from(d.len()).unwrap().to_be_bytes());
        s.extend(d);
        s[2] = u8::try_from(s.len() - 3 + 4).unwrap();
        s.extend(crc32(&s).to_be_bytes());
        TableBytes {
            packet_id: 0x8000,
            message_id: 0x8000,
            bytes: s,
        }
    }

    /// The Matroska form end to end: the descriptors' NTP clock places the
    /// EIT's JST on the timeline, the recorded service's events only.
    #[test]
    fn mkv_chapters_from_the_eit_on_the_media_clock() {
        const DAY: u16 = 61_349; // 2026-11-05
        // The first MPU's PT: 02:21:26 JST, 17:21:26 UTC of the day before.
        let pt = (u64::from(DAY) - 15_020) * 86_400 + 2 * 3600 + 21 * 60 + 26 - 9 * 3600;
        let mut timing = timed();
        for (pid, mpu) in [(0xf300, 100), (0xf310, 8)] {
            let t = timing.get_mut(&pid).unwrap();
            t.mpu_times.insert(mpu, pt << 32);
        }
        let dir = std::env::temp_dir().join(format!("isdb-remux-chapters-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("out.mkv");
        let mkv = crate::mkv::Mkv::create(&path, "test").unwrap();
        let mut s = Session::new(Box::new(std::io::sink()), None)
            .unwrap()
            .with_mkv(mkv);
        s.set_timing(&timing).unwrap();
        // Running since 02:00; ends and the next starts 1 s in (the two MPUs
        // written span ≈ 1.07 s); one starts after them; another service's.
        let hms = |m, s| [0x02, m, s];
        s.table(&eit_section(0x8B, 1, DAY, hms(0x00, 0x00), [0, 0x21, 0x27]));
        s.table(&eit_section(0x8B, 2, DAY, hms(0x21, 0x27), [0, 0x30, 0]));
        s.table(&eit_section(0x8C, 3, DAY, hms(0x51, 0x27), [0, 0x30, 0]));
        let mut other = eit_section(0x8B, 4, DAY, hms(0x00, 0x00), [1, 0, 0]);
        other.bytes[4] = 0x66;
        let at = other.bytes.len() - 4;
        let crc = crc32(&other.bytes[..at]).to_be_bytes();
        other.bytes[at..].copy_from_slice(&crc);
        s.table(&other);
        feed(&mut s, 100, 0..32);
        feed(&mut s, 101, 0..32);
        let (c, _) = s.finish().unwrap();
        assert_eq!(c.chapters, 2);
        let file = std::fs::read(&path).unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
        assert!(file.windows(4).any(|w| w == [0x10, 0x43, 0xa7, 0x70]));
        assert_eq!(file.windows(4).filter(|w| *w == b"prog").count(), 2);
    }

    /// An end of sequence and a changed SPS each start a new video sequence on
    /// the chapters' clock; the first SPS does not.
    #[test]
    fn a_new_video_sequence_reaches_the_chapters() {
        let dir = std::env::temp_dir().join(format!("isdb-remux-seq-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mkv = crate::mkv::Mkv::create(&dir.join("out.mkv"), "test").unwrap();
        let mut s = Session::new(Box::new(std::io::sink()), None)
            .unwrap()
            .with_mkv(mkv);
        s.set_timing(&timed()).unwrap();
        let nal = |bytes: &[u8]| {
            let mut mfu = u32::try_from(bytes.len()).unwrap().to_be_bytes().to_vec();
            mfu.extend(bytes);
            mfu
        };
        let sequences = |s: &Session| s.schedule.sequence_count(0x65);
        // The first MPU may be joined midway: a stream starts at the next.
        feed(&mut s, 100, 0..1);
        s.video_mfu(0xf300, 101, &nal(&[33 << 1, 1, 0xaa])).unwrap();
        assert_eq!(sequences(&s), 0, "the first SPS");
        s.video_mfu(0xf300, 101, &nal(&[33 << 1, 1, 0xaa])).unwrap();
        assert_eq!(sequences(&s), 0, "the same SPS");
        s.video_mfu(0xf300, 101, &nal(&[33 << 1, 1, 0xbb])).unwrap();
        assert_eq!(sequences(&s), 1, "a changed SPS");
        s.video_mfu(0xf300, 101, &nal(&[36 << 1, 1])).unwrap();
        assert_eq!(sequences(&s), 2, "an end of sequence");
        drop(s);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A data item's arrival never goes back, though pictures come in decode
    /// order, their PTSs reordered: a cut selects items by a search over arrivals.
    #[test]
    fn data_arrivals_never_go_back_over_reordered_pictures() {
        let dir = std::env::temp_dir().join(format!("isdb-remux-arrival-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mkv = crate::mkv::Mkv::create(&dir.join("out.mkv"), "test").unwrap();
        let mut s = Session::new(Box::new(std::io::sink()), None)
            .unwrap()
            .with_mkv(mkv);
        s.set_timing(&timed()).unwrap();
        feed(&mut s, 100, 0..1);
        // Trailing pictures, whose display order is not their decode order.
        for i in 0..32 {
            feed(&mut s, 101, i..=i);
            s.data_mfu(0xf340, 7, Some(0), b"x").unwrap();
        }
        let arrivals = s.mkv.as_ref().unwrap().arrivals();
        assert!(arrivals.len() > 8, "{arrivals:?}");
        assert!(arrivals.is_sorted(), "{arrivals:?}");
        drop(s);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A caption MFU (B60 Table 9-1) of the document `ttml`.
    fn caption_mfu(ttml: &str) -> Vec<u8> {
        let mut b = vec![0x30, 0, 0, 0, 0x00];
        b.extend(u16::try_from(ttml.len()).unwrap().to_be_bytes());
        b.extend(ttml.as_bytes());
        b
    }

    /// A document shown from `begin` to `end`.
    fn closed(begin: &str, end: &str, text: &str) -> String {
        format!(
            r#"<tt xmlns="http://www.w3.org/ns/ttml"><body><div begin="{begin}" end="{end}"><p>{text}</p></div></body></tt>"#
        )
    }

    /// A document left open, with no time.
    fn open(text: &str) -> String {
        format!(
            r#"<tt xmlns="http://www.w3.org/ns/ttml"><body><div><p>{text}</p></div></body></tt>"#
        )
    }

    /// Subtitle information (Table 9-3) with this clock, from `start_mpu`.
    fn subtitle_info(
        time_control: u8,
        reference: Option<u64>,
        start_mpu: Option<u32>,
    ) -> es::SubtitleInfo {
        es::SubtitleInfo {
            tag: 0x30,
            version: 0,
            language: *b"jpn",
            kind: 0,
            format: 0,
            operation_mode: 1,
            time_control,
            display_mode: 0b1010,
            resolution: 1,
            compression: 0,
            start_mpu,
            reference_start_time: reference,
            leap_indicator: reference.map(|_| 0),
        }
    }

    /// `timed()` with the caption asset's clock.
    fn with_captions(time_control: u8, reference: Option<u64>) -> BTreeMap<u16, AssetTiming> {
        let mut t = timed();
        t.get_mut(&0xf330).unwrap().subtitles = vec![subtitle_info(time_control, reference, None)];
        t
    }

    /// A recorder that has written its first picture: PTS 21020 / 180000 s (116 ms
    /// after the MPU's presentation time) after NTP second 1000, the timeline's zero.
    fn recording(time_control: u8, reference: Option<u64>) -> Session {
        let mut s = Session::new(Box::new(std::io::sink()), None).unwrap();
        s.set_timing(&with_captions(time_control, reference))
            .unwrap();
        first_picture(&mut s);
        s
    }

    /// The first picture of [`recording`].
    fn first_picture(s: &mut Session) {
        // Joined mid-MPU: it starts at the next.
        feed(s, 99, [31]);
        feed(s, 100, 0..2);
        s.flush_picture().unwrap();
    }

    /// The cues of the only caption asset: `(start, end, text)`, ms.
    fn cues(mut s: Session) -> Vec<(i64, i64, String)> {
        let (_, tracks) = s.finish().unwrap();
        assert_eq!(tracks.len(), 1);
        tracks[0]
            .cues
            .iter()
            .map(|c| (c.start, c.end, c.lines.join("/")))
            .collect()
    }

    /// B60 Table 9-7, `0010`: each event's time codes count from its own reference,
    /// so the same time code in two events is two places on the timeline.
    #[test]
    fn spec_b60_table_9_7_each_event_counts_from_its_reference() {
        let mut s = recording(0b0010, Some(1000 << 32));
        s.ntp(1005 << 32);
        s.caption_mfu(
            0xf330,
            1,
            &caption_mfu(&closed("00:00:05", "00:00:07", "first")),
        );
        s.set_timing(&with_captions(0b0010, Some(1100 << 32)))
            .unwrap();
        s.ntp(1105 << 32);
        s.caption_mfu(
            0xf330,
            1,
            &caption_mfu(&closed("00:00:05", "00:00:07", "second")),
        );
        assert_eq!(
            cues(s),
            vec![
                (4_883, 6_883, "first".to_owned()),
                (104_883, 106_883, "second".to_owned()),
            ]
        );
    }

    /// The MPT's new reference comes before the event's switch: the old event's
    /// last document, read against it, would land far ahead in the next event.
    #[test]
    fn a_document_far_from_its_reference_is_placed_by_its_arrival() {
        let mut s = recording(0b0010, Some(1100 << 32));
        s.ntp(1099 << 32);
        s.caption_mfu(
            0xf330,
            1,
            &caption_mfu(&closed("00:01:39", "00:01:41", "old")),
        );
        assert_eq!(cues(s), vec![(98_883, 100_883, "old".to_owned())]);
    }

    /// A document may come a little before its time, as on air: not far enough to
    /// doubt the reference.
    #[test]
    fn a_document_ahead_of_its_time_keeps_it() {
        let mut s = recording(0b0010, Some(1000 << 32));
        s.ntp((1005 << 32) - (1 << 30));
        s.caption_mfu(
            0xf330,
            1,
            &caption_mfu(&closed("00:00:05", "00:00:07", "soon")),
        );
        assert_eq!(cues(s), vec![(4_883, 6_883, "soon".to_owned())]);
    }

    /// B60 Table 9-7, `1111`: presented when it comes; a time code, if any, says
    /// only how long. An open document ends at the next.
    #[test]
    fn spec_b60_table_9_7_without_time_control_a_document_shows_on_arrival() {
        let mut s = recording(0b1111, None);
        s.ntp(1010 << 32);
        s.caption_mfu(
            0xf330,
            1,
            &caption_mfu(&closed("05:00:00", "05:00:02", "closed")),
        );
        s.ntp(1020 << 32);
        s.caption_mfu(0xf330, 1, &caption_mfu(&open("open")));
        s.ntp(1023 << 32);
        s.caption_mfu(0xf330, 1, &caption_mfu(&open("next")));
        let shown = cues(s);
        assert_eq!(shown[0], (9_883, 11_883, "closed".to_owned()));
        assert_eq!(shown[1], (19_883, 22_883, "open".to_owned()));
        assert_eq!(shown.len(), 2, "the last opens after the last picture");
    }

    /// A stream without NTP packets still times a document, by the picture then.
    #[test]
    fn without_the_ntp_clock_arrival_is_the_last_picture() {
        let mut s = recording(0b1111, None);
        s.caption_mfu(
            0xf330,
            1,
            &caption_mfu(&closed("00:00:00", "00:00:02", "x")),
        );
        let last = s.last_pts.unwrap() - s.first_pts.unwrap();
        assert_eq!(
            cues(s),
            vec![(last / 90, last / 90 + 2_000, "x".to_owned())]
        );
    }

    /// A document may come before the first picture dates the media's clock: its
    /// arrival is still known, and a cue running into the recording is kept.
    #[test]
    fn a_document_before_the_first_picture_keeps_its_arrival() {
        let mut s = Session::new(Box::new(std::io::sink()), None).unwrap();
        s.set_timing(&with_captions(0b1111, None)).unwrap();
        s.ntp(999 << 32);
        s.caption_mfu(
            0xf330,
            1,
            &caption_mfu(&closed("00:00:00", "00:00:05", "early")),
        );
        first_picture(&mut s);
        assert_eq!(cues(s), vec![(0, 3_884, "early".to_owned())]);
    }

    /// B60 Table 9-3: information with a start MPU governs from that MPU, however
    /// soon it is announced: the documents before go by the reference before it,
    /// even one too near the new reference for the arrival to tell.
    #[test]
    fn spec_b60_table_9_3_a_reference_waits_for_its_start_mpu() {
        let mut t = timed();
        t.get_mut(&0xf330).unwrap().subtitles = vec![
            subtitle_info(0b0010, Some(1000 << 32), None),
            subtitle_info(0b0010, Some(1005 << 32), Some(10)),
        ];
        let mut s = Session::new(Box::new(std::io::sink()), None).unwrap();
        s.set_timing(&t).unwrap();
        first_picture(&mut s);
        s.ntp(1005 << 32);
        s.caption_mfu(
            0xf330,
            9,
            &caption_mfu(&closed("00:00:05", "00:00:07", "old")),
        );
        s.ntp(1010 << 32);
        s.caption_mfu(
            0xf330,
            10,
            &caption_mfu(&closed("00:00:05", "00:00:07", "new")),
        );
        assert_eq!(
            cues(s),
            vec![
                (4_883, 6_883, "old".to_owned()),
                (9_883, 11_883, "new".to_owned()),
            ]
        );
    }

    /// Another service's caption asset in the same stream is not recorded.
    #[test]
    fn another_services_captions_are_not_recorded() {
        let mut t = timed();
        t.insert(
            0xf331,
            AssetTiming {
                kind: *b"stpp",
                service: Some(0x66),
                ..AssetTiming::default()
            },
        );
        let mut s = Session::new(Box::new(std::io::sink()), Some(0x65)).unwrap();
        s.set_timing(&t).unwrap();
        assert!(s.records(0xf330));
        assert!(!s.records(0xf331));
    }

    /// The TS form never reads the events.
    #[test]
    fn ts_form_keeps_no_events() {
        let mut s = Session::new(Box::new(std::io::sink()), None).unwrap();
        s.set_timing(&timed()).unwrap();
        s.table(&eit_section(
            0x8B,
            1,
            15020,
            [0x09, 0x00, 0x00],
            [0, 0x30, 0],
        ));
        feed(&mut s, 100, 0..32);
        let (c, _) = s.finish().unwrap();
        assert_eq!(c.chapters, 0);
    }

    #[test]
    fn unwrap_follows_the_33_bit_ring() {
        let ring = 1i64 << 33;
        assert_eq!(unwrap(None, 5), 5);
        assert_eq!(unwrap(Some(ring - 10), 5), ring + 5);
        assert_eq!(unwrap(Some(ring + 5), (ring - 10) as u64), ring - 10);
    }
}
