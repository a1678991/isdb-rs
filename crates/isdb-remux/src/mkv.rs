//! The Matroska form of the recorder ([`crate::ts::Session`] with
//! `--mkv`): every track of the service in one file, through
//! `isdb-mkv`'s writer —
//!
//! - the HEVC video (`V_MPEGH/ISO/HEVC`, its `hvcC` from the stream's
//!   first VPS/SPS/PPS; pictures as 4-byte length-prefixed NAL units,
//!   the access unit delimiters dropped, IRAP pictures keyframes);
//! - every AAC audio (`A_AAC`: the raw frame of each LATM AudioMuxElement,
//!   its `AudioSpecificConfig` the `CodecPrivate` — not re-encoded), the
//!   MPT's language, named by its channels;
//! - the captions (`S_TEXT/UTF8`, one block per cue);
//! - a chapter per event of the MH-EIT, at the stream's real switch
//!   ([`crate::events`]);
//! - the untimed assets (data broadcasting): one attachment per asset
//!   and package version, `PID-MPU.mmtitems`, its items each as
//!   `[item id u32][length u32][bytes]`.
//!
//! With `--split-events` the same frames make one file per event
//! ([`Mkv::finish_parts`]).
//!
//! Frames are spilled to a scratch file next to the output until the
//! end (the writer is two-pass), so the track list — configurations
//! learnt from the stream, captions aligned at the end — is complete
//! when the header is written. Block times are milliseconds from the
//! earliest PTS of any stream: the zero of a player's timeline, the same
//! as the TS form's sidecars.

use crate::split::{NO_EVENT, Part, Template, start_time};
use crate::timing::Clock;
use isdb_mkv::{Chapter, Frame, Kind, Track, Writer};
use std::collections::BTreeMap;
use std::fs::File;
use std::io::{self, BufWriter, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

/// 90 kHz ticks per second.
const TICKS: i64 = 90_000;
/// A key picture counts as a part's start this near it (ticks): the descriptors'
/// time and the PTS round apart.
const KEY_TOLERANCE: i64 = 2;

/// One stream's description from the MPT.
#[derive(Debug, Clone, Default)]
pub struct Decl {
    /// The MPT's language (ISO 639-2) when it names one.
    pub language: Option<String>,
    /// A track name (`5.1ch`, `stereo`, …).
    pub name: Option<String>,
}

/// The files of a sink: the scratch always goes at the end, the output
/// too unless the file was finished (a failed or interrupted run leaves
/// neither a half-written file nor its scratch).
struct Files {
    out: Option<PathBuf>,
    scratch: PathBuf,
    finished: bool,
}

impl Drop for Files {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.scratch);
        if !self.finished
            && let Some(out) = &self.out
        {
            let _ = std::fs::remove_file(out);
        }
    }
}

/// A part's file until it is finished: a failed run leaves whole files only.
struct PartFile {
    path: PathBuf,
    finished: bool,
}

impl Drop for PartFile {
    fn drop(&mut self) {
        if !self.finished {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

/// The base writer's output: the file of a single-file run, none when the sink
/// only spools for [`Mkv::finish_parts`].
enum Out {
    File(BufWriter<File>),
    Spool,
}

impl Write for Out {
    fn write(&mut self, b: &[u8]) -> io::Result<usize> {
        match self {
            Self::File(f) => f.write(b),
            Self::Spool => Ok(b.len()),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match self {
            Self::File(f) => f.flush(),
            Self::Spool => Ok(()),
        }
    }
}

impl Seek for Out {
    fn seek(&mut self, to: SeekFrom) -> io::Result<u64> {
        match self {
            Self::File(f) => f.seek(to),
            Self::Spool => Ok(0),
        }
    }
}

/// Where `--split-events` puts its files.
struct Split {
    dir: PathBuf,
    template: Template,
    /// Another handle on the scratch, for the parts.
    scratch: File,
}

/// One file written by [`Mkv::finish_parts`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Written {
    /// Where.
    pub path: PathBuf,
    /// Its event's name, when an event covers it.
    pub event: Option<String>,
    /// Its first picture's presentation time (NTP timestamp).
    pub first_frame: u64,
    /// Its tracks.
    pub tracks: usize,
    /// Its attachments.
    pub attachments: usize,
    /// Its pictures and audio frames.
    pub frames: usize,
}

/// A data stream's items in arrival order: `[item id u32][length u32][bytes]`,
/// with each item's arrival (unwrapped PTS) and offset.
#[derive(Default)]
struct Items {
    bytes: Vec<u8>,
    marks: Vec<(i64, usize)>,
}

impl Items {
    /// The items that arrived from `from` to before `to`.
    fn between(&self, from: i64, to: i64) -> &[u8] {
        let offset = |t: i64| {
            let i = self.marks.partition_point(|m| m.0 < t);
            self.marks.get(i).map_or(self.bytes.len(), |m| m.1)
        };
        &self.bytes[offset(from)..offset(to)]
    }
}

/// The Matroska sink.
pub struct Mkv {
    writer: Writer<Out, File>,
    files: Files,
    split: Option<Split>,
    /// The streams' MPT descriptions, by asset packet id.
    decls: BTreeMap<u16, Decl>,
    /// The codec configurations learnt per stream, each with the time of its
    /// first frame (a picture's DTS, an audio frame's PTS).
    configs: BTreeMap<u16, Vec<(i64, Kind)>>,
    /// The parameter sets in force (the `hvcC`): VPS, SPS, PPS.
    vps: Option<Vec<u8>>,
    sps: Option<Vec<u8>>,
    pps: Option<Vec<u8>>,
    /// The track each stream's frames went to (provisional numbers,
    /// renumbered at the end): assigned at a stream's first frame.
    provisional: BTreeMap<u16, u64>,
    /// The data items per (asset, MPU), in arrival order.
    data: BTreeMap<(u16, u32), Items>,
}

impl Mkv {
    /// A sink writing `path` (its scratch `PATH.part`, removed at the
    /// end; both removed when the sink is dropped unfinished).
    ///
    /// # Errors
    ///
    /// The files' creation.
    pub fn create(path: &Path, app: &str) -> io::Result<Self> {
        let out = BufWriter::new(File::create(path)?);
        let mut scratch_path = path.as_os_str().to_owned();
        scratch_path.push(".part");
        let scratch_path = PathBuf::from(scratch_path);
        let scratch = open_scratch(&scratch_path)?;
        Ok(Self::with(
            Writer::new(Out::File(out), scratch, app),
            Files {
                out: Some(path.to_owned()),
                scratch: scratch_path,
                finished: false,
            },
            None,
        ))
    }

    /// A sink writing one file per part into `dir`, named by `template`
    /// ([`Mkv::finish_parts`]); its scratch is in `dir` too, removed at the end.
    ///
    /// # Errors
    ///
    /// The scratch's creation.
    pub fn create_split(dir: &Path, template: Template, app: &str) -> io::Result<Self> {
        let scratch_path = dir.join(format!("isdb-remux-{}.part", std::process::id()));
        let scratch = open_scratch(&scratch_path)?;
        let handle = scratch.try_clone()?;
        Ok(Self::with(
            Writer::new(Out::Spool, scratch, app),
            Files {
                out: None,
                scratch: scratch_path,
                finished: false,
            },
            Some(Split {
                dir: dir.to_owned(),
                template,
                scratch: handle,
            }),
        ))
    }

    fn with(writer: Writer<Out, File>, files: Files, split: Option<Split>) -> Self {
        Self {
            writer,
            files,
            split,
            decls: BTreeMap::new(),
            configs: BTreeMap::new(),
            vps: None,
            sps: None,
            pps: None,
            provisional: BTreeMap::new(),
            data: BTreeMap::new(),
        }
    }

    /// Whether this sink writes one file per part.
    #[must_use]
    pub fn splits(&self) -> bool {
        self.split.is_some()
    }

    /// Describe a stream (from the MPT).
    pub fn describe(&mut self, pid: u16, decl: Decl) {
        self.decls.insert(pid, decl);
    }

    /// The writer's track for `pid` (provisional: in order of first
    /// frame; the header lists them so).
    fn track(&mut self, pid: u16) -> u64 {
        let n = self.provisional.len() as u64 + 1;
        *self.provisional.entry(pid).or_insert(n)
    }

    /// One picture of `pid` (Annex B), with its unwrapped PTS / DTS.
    ///
    /// # Errors
    ///
    /// The scratch's write.
    pub fn picture(&mut self, pid: u16, pts: i64, dts: i64, annex_b: &[u8]) -> io::Result<()> {
        let (mut key, mut rasl) = (false, false);
        let (mut vps, mut sps, mut pps) = (None, None, None);
        let block = isdb_mkv::hevc::annex_b_to_length(annex_b, |nal| {
            match (nal[0] >> 1) & 0x3f {
                // Parameter sets stay in-band too (a decoder that
                // re-reads them mid-stream) — the AUD does not.
                32 => vps = vps.take().or_else(|| Some(nal.to_vec())),
                33 => sps = sps.take().or_else(|| Some(nal.to_vec())),
                34 => pps = pps.take().or_else(|| Some(nal.to_vec())),
                35 => return false,
                8 | 9 => rasl = true,
                16..=23 => key = true,
                _ => {}
            }
            true
        });
        // A new VPS or SPS is a new configuration; a PPS only goes with one.
        let new_sequence = (vps.is_some() && vps != self.vps) || (sps.is_some() && sps != self.sps);
        self.vps = vps.or_else(|| self.vps.take());
        self.sps = sps.or_else(|| self.sps.take());
        self.pps = pps.or_else(|| self.pps.take());
        if (new_sequence || !self.configs.contains_key(&pid))
            && let (Some(v), Some(s), Some(p)) = (&self.vps, &self.sps, &self.pps)
            && let Some(hvcc) = isdb_mkv::hevc::hvcc(&[v], &[s], &[p], &[])
            && let Some(sps) = isdb_mkv::hevc::parse_sps(s)
        {
            self.configs.entry(pid).or_default().push((
                dts,
                Kind::Hevc {
                    hvcc,
                    width: sps.width,
                    height: sps.height,
                    frame_ns: None,
                },
            ));
        }
        self.frame(pid, dts, pts, key, &block)?;
        if rasl && !block.is_empty() {
            self.writer.mark_needs_previous();
        }
        Ok(())
    }

    /// One audio AudioMuxElement of `pid` with its unwrapped PTS.
    ///
    /// # Errors
    ///
    /// The scratch's write.
    pub fn audio(&mut self, pid: u16, pts: i64, element: &[u8]) -> io::Result<()> {
        let Some(f) = isdb_mmt::es::latm_frame(element) else {
            return Ok(());
        };
        if let Some(config) = f.config
            && !self
                .configs
                .get(&pid)
                .and_then(|c| c.last())
                .is_some_and(|(_, k)| matches!(k, Kind::Aac { config: c, .. } if *c == config))
            && let Some((rate, channels)) = isdb_mkv::asc_rate_channels(&config)
        {
            self.configs.entry(pid).or_default().push((
                pts,
                Kind::Aac {
                    config,
                    rate,
                    channels,
                },
            ));
        }
        self.frame(pid, pts, pts, true, &f.payload)
    }

    /// Add a chapter, in the order a player lists them.
    pub fn chapter(&mut self, c: Chapter) {
        self.writer.add_chapter(c);
    }

    /// One item of an untimed asset, arriving at the unwrapped PTS `arrival`
    /// (`None` before the first picture).
    pub fn data(
        &mut self,
        pid: u16,
        mpu: u32,
        item: Option<u32>,
        bytes: &[u8],
        arrival: Option<i64>,
    ) {
        let d = self.data.entry((pid, mpu)).or_default();
        d.marks.push((arrival.unwrap_or(i64::MIN), d.bytes.len()));
        d.bytes.extend_from_slice(&item.unwrap_or(0).to_be_bytes());
        d.bytes
            .extend_from_slice(&u32::try_from(bytes.len()).unwrap_or(u32::MAX).to_be_bytes());
        d.bytes.extend_from_slice(bytes);
    }

    fn frame(&mut self, pid: u16, dts: i64, pts: i64, key: bool, block: &[u8]) -> io::Result<()> {
        if block.is_empty() {
            return Ok(());
        }
        // Times stay in 90 kHz ticks until the end (the origin, the
        // earliest PTS of all, is known then).
        let track = self.track(pid);
        self.writer.frame(track, dts, pts, key, None, block)
    }

    /// Warn of the audio configuration changes not in `cuts`: the frames from
    /// there on go on under a track that declares the earlier configuration.
    fn warn_audio_changes(&self, cuts: &BTreeMap<u16, Vec<i64>>) {
        for (&pid, list) in &self.configs {
            for pair in list.windows(2) {
                let (Kind::Aac { config: from, .. }, Kind::Aac { config: to, .. }) =
                    (&pair[0].1, &pair[1].1)
                else {
                    continue;
                };
                if !cuts.get(&pid).is_some_and(|c| c.contains(&pair[1].0)) {
                    tracing::warn!(
                        pid,
                        from = ?isdb_mkv::asc_rate_channels(from),
                        to = ?isdb_mkv::asc_rate_channels(to),
                        "mkv: an audio stream changes configuration; its track keeps the \
                         earlier one, so a player may not decode the frames from here on"
                    );
                }
            }
        }
    }

    /// Add the tracks (with their configuration), the captions and the
    /// data attachments, and write the file. Captions are in ms from
    /// `ts_origin` (the recorder's earliest PTS, 90 kHz unwrapped). The
    /// track and attachment counts.
    ///
    /// # Errors
    ///
    /// The writes'.
    pub fn finish(
        mut self,
        captions: &[crate::ts::CaptionTrack],
        ts_origin: Option<i64>,
    ) -> io::Result<(usize, usize)> {
        self.warn_audio_changes(&BTreeMap::new());
        // The tracks with a configuration — video, then the audio, each
        // in asset packet id order — and the frames renumbered to them
        // (the provisional numbers went by first frame); a stream with
        // no configuration is left out.
        let rank = |pid: &u16| rank(self.configs.get(pid).and_then(|c| c.first()).map(|c| &c.1));
        let mut pids: Vec<u16> = self.provisional.keys().copied().collect();
        pids.sort_by_key(|p| (rank(p), *p));
        let mut map = vec![None; self.provisional.len()];
        let mut first_audio = true;
        let mut tracks = 0;
        for pid in pids {
            let Some(kind) = self
                .configs
                .get(&pid)
                .and_then(|c| c.first())
                .map(|c| c.1.clone())
            else {
                tracing::warn!(
                    pid,
                    "mkv: no codec configuration seen; the stream is left out"
                );
                continue;
            };
            let slot = usize::try_from(self.provisional[&pid] - 1).unwrap_or(usize::MAX);
            let d = self.decls.get(&pid).cloned().unwrap_or_default();
            let default = match &kind {
                Kind::Aac { .. } => std::mem::take(&mut first_audio),
                _ => true,
            };
            let n = self.writer.add_track(Track {
                kind,
                language: d.language,
                name: d.name,
                default,
            });
            if let Some(m) = map.get_mut(slot) {
                *m = Some(n);
            }
            tracks += 1;
        }
        self.writer.renumber(&map);
        // 90 kHz ticks to ms from the recorder's origin (the captions'
        // zero too).
        self.writer.rebase(ts_origin.unwrap_or(0), 90);
        for c in captions.iter().filter(|c| !c.cues.is_empty()) {
            let n = self.writer.add_track(Track {
                kind: Kind::Text,
                language: c.lang.as_deref().map(iso639_2),
                name: Some("captions".into()),
                default: false,
            });
            tracks += 1;
            for cue in &c.cues {
                self.writer.frame(
                    n,
                    cue.start,
                    cue.start,
                    true,
                    Some(cue.end - cue.start),
                    cue.lines.join("\n").as_bytes(),
                )?;
            }
        }
        let attachments = self.data.len();
        for ((pid, mpu), d) in std::mem::take(&mut self.data) {
            self.writer.attach(
                &format!("{pid:04x}-{mpu}.mmtitems"),
                "application/octet-stream",
                &attachment_description(pid, mpu),
                d.bytes,
            );
        }
        let (out, _) = self.writer.finish()?;
        if let Out::File(f) = out {
            f.into_inner()
                .map_err(io::IntoInnerError::into_error)?
                .sync_all()?;
        }
        self.files.finished = true;
        Ok((tracks, attachments))
    }

    /// Write one file per part with frames, from the frames spooled: the video
    /// cut at each part's key picture, leaving out the RASL pictures after it,
    /// which need pictures in the file before; each audio stream at the part's
    /// first picture, or at a change of its configuration near it, so that a track
    /// declares what its frames are. Each file has its own tracks and timeline,
    /// the event's name for its title, the captions shown in its span and the data
    /// items that arrived in it. `ts_origin` as [`Mkv::finish`]. A file already
    /// there is not overwritten: the name takes a `-N`.
    ///
    /// # Errors
    ///
    /// The writes'. The files finished by then stay; the one being written goes.
    pub fn finish_parts(
        mut self,
        captions: &[crate::ts::CaptionTrack],
        ts_origin: Option<i64>,
        parts: &[Part],
        clock: Clock,
        broadcaster: &str,
    ) -> io::Result<Vec<Written>> {
        let split = self
            .split
            .take()
            .ok_or_else(|| io::Error::other("not a split sink"))?;
        let by_track: BTreeMap<u64, u16> = self.provisional.iter().map(|(&p, &t)| (t, p)).collect();
        let video = self.configs.iter().find_map(|(pid, c)| {
            matches!(c.first(), Some((_, Kind::Hevc { .. }))).then(|| self.provisional.get(pid))?
        });
        // A copy: each part's writer takes the writer mutably.
        let frames = self.writer.frames().to_vec();
        let starts = video_cuts(&frames, video.copied(), parts);
        let (assigned, firsts) = assign_video(&frames, video.copied(), &starts);
        // Where each part's span starts: its first picture.
        let spans: Vec<i64> = parts
            .iter()
            .enumerate()
            .map(|(k, p)| {
                if k == 0 {
                    i64::MIN
                } else {
                    firsts[k].unwrap_or(p.start)
                }
            })
            .collect();
        let mut audio_cuts: BTreeMap<u16, Vec<i64>> = BTreeMap::new();
        for (&pid, list) in &self.configs {
            if matches!(list.first(), Some((_, Kind::Aac { .. }))) {
                let changes: Vec<i64> = list.iter().skip(1).map(|c| c.0).collect();
                audio_cuts.insert(pid, cuts_of_audio(&spans, &changes));
            }
        }
        self.warn_audio_changes(&audio_cuts);
        let assigned = assign_audio(&frames, &by_track, &audio_cuts, assigned);
        let mut written = Vec::new();
        for (k, part) in parts.iter().enumerate() {
            let in_part: Vec<&Frame> = frames
                .iter()
                .zip(&assigned)
                .filter(|(_, a)| **a == Some(k))
                .map(|(f, _)| f)
                .collect();
            let Some(origin) = in_part.iter().map(|f| f.time).min() else {
                continue;
            };
            let first_frame = firsts[k].unwrap_or(origin);
            let count = in_part.len();
            // Each present stream, with the configuration at its first frame.
            let mut present: Vec<(u16, u64, Kind)> = Vec::new();
            for (&track, &pid) in &by_track {
                let Some(first) = in_part.iter().find(|f| f.track == track) else {
                    continue;
                };
                let tick = if Some(track) == video.copied() {
                    first.order
                } else {
                    first.time
                };
                if let Some(kind) = self.configs.get(&pid).and_then(|c| config_at(c, tick)) {
                    present.push((pid, track, kind.clone()));
                } else {
                    tracing::warn!(
                        pid,
                        "mkv: no codec configuration seen; the stream is left out"
                    );
                }
            }
            drop(in_part);
            present.sort_by_key(|(pid, _, kind)| (rank(Some(kind)), *pid));
            // The file.
            let start = start_time(clock.ntp_of(first_frame));
            let event = part.title.as_deref().unwrap_or(NO_EVENT);
            let name = split.template.render(&start, event, broadcaster);
            let (file, path) = create_new(&split.dir, &name)?;
            let mut guard = PartFile {
                path,
                finished: false,
            };
            let mut position = 0;
            let mut out = self.writer.part(
                BufWriter::new(file),
                split.scratch.try_clone()?,
                |_: &Frame| {
                    let keep = assigned[position] == Some(k);
                    position += 1;
                    keep
                },
            )?;
            // Tracks: their numbers from 1, video first.
            let mut map = vec![None; self.provisional.len()];
            let mut first_audio = true;
            for (pid, track, kind) in present {
                let d = self.decls.get(&pid).cloned().unwrap_or_default();
                let (default, name) = match &kind {
                    Kind::Aac { channels, .. } => (
                        std::mem::take(&mut first_audio),
                        Some(channel_name(*channels)),
                    ),
                    _ => (true, d.name),
                };
                let n = out.add_track(Track {
                    kind,
                    language: d.language,
                    name,
                    default,
                });
                if let Some(m) = usize::try_from(track - 1).ok().and_then(|t| map.get_mut(t)) {
                    *m = Some(n);
                }
            }
            let mut tracks = map.iter().flatten().count();
            out.renumber(&map);
            out.rebase(origin, 90);
            let from = spans[k];
            let to = spans.get(k + 1).copied().unwrap_or(i64::MAX);
            tracks += add_captions(&mut out, captions, ts_origin, origin, from, to)?;
            let mut attachments = 0;
            for (&(pid, mpu), d) in &self.data {
                let bytes = d.between(from, to);
                if !bytes.is_empty() {
                    out.attach(
                        &format!("{pid:04x}-{mpu}.mmtitems"),
                        "application/octet-stream",
                        &attachment_description(pid, mpu),
                        bytes.to_vec(),
                    );
                    attachments += 1;
                }
            }
            if let Some(title) = &part.title {
                out.set_title(title);
            }
            let (file, _) = out.finish()?;
            file.into_inner()
                .map_err(io::IntoInnerError::into_error)?
                .sync_all()?;
            guard.finished = true;
            written.push(Written {
                path: guard.path.clone(),
                event: part.title.clone(),
                first_frame: clock.ntp_of(first_frame),
                tracks,
                attachments,
                frames: count,
            });
        }
        Ok(written)
    }
}

/// Video before audio before anything else.
fn rank(kind: Option<&Kind>) -> u8 {
    match kind {
        Some(Kind::Hevc { .. }) => 0,
        Some(Kind::Aac { .. }) => 1,
        _ => 2,
    }
}

fn open_scratch(path: &Path) -> io::Result<File> {
    File::options()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .open(path)
}

fn attachment_description(pid: u16, mpu: u32) -> String {
    format!("MMT asset {pid:#06x}, package {mpu}: [item id u32][length u32][bytes]…")
}

/// A new file `name` in `dir`, or `-2`, `-3` … before its extension when taken.
fn create_new(dir: &Path, name: &str) -> io::Result<(File, PathBuf)> {
    let path = dir.join(name);
    let stem = path.file_stem().map(std::ffi::OsStr::to_os_string);
    let extension = path.extension().map(std::ffi::OsStr::to_os_string);
    let mut candidate = path;
    for n in 2u32.. {
        match File::options()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&candidate)
        {
            Ok(f) => return Ok((f, candidate)),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e),
        }
        let mut next = stem.clone().unwrap_or_default();
        next.push(format!("-{n}"));
        if let Some(x) = &extension {
            next.push(".");
            next.push(x);
        }
        candidate = dir.join(next);
    }
    Err(io::Error::other("no free file name"))
}

/// Each part's first decode time in the video: `i64::MIN` for the first, then the
/// first key picture at or after the part's start, so a cut is always a place a
/// decoder can start (`i64::MAX` when none follows).
fn video_cuts(frames: &[Frame], video: Option<u64>, parts: &[Part]) -> Vec<i64> {
    let keys: Vec<&Frame> = frames
        .iter()
        .filter(|f| Some(f.track) == video && f.keyframe)
        .collect();
    parts
        .iter()
        .enumerate()
        .map(|(k, p)| {
            if k == 0 {
                return i64::MIN;
            }
            keys.iter()
                .find(|f| f.time >= p.start - KEY_TOLERANCE)
                .map_or(i64::MAX, |f| f.order)
        })
        .collect()
}

/// Which part each picture goes to (`None` for what is left out or is not a
/// picture), and each part's first picture's presentation time. A RASL picture of
/// a part's first key picture is left out; the first part has none, the recorder
/// not having written them.
fn assign_video(
    frames: &[Frame],
    video: Option<u64>,
    cuts: &[i64],
) -> (Vec<Option<usize>>, Vec<Option<i64>>) {
    let mut assigned = vec![None; frames.len()];
    let mut firsts = vec![None::<i64>; cuts.len()];
    let mut k = 0;
    let mut keys = 0;
    for (f, slot) in frames.iter().zip(&mut assigned) {
        if Some(f.track) != video {
            continue;
        }
        while k + 1 < cuts.len() && f.order >= cuts[k + 1] {
            k += 1;
            keys = 0;
        }
        keys += u32::from(f.keyframe);
        if k > 0 && f.needs_previous && keys <= 1 {
            continue;
        }
        *slot = Some(k);
        firsts[k] = Some(firsts[k].map_or(f.time, |t: i64| t.min(f.time)));
    }
    (assigned, firsts)
}

/// One audio stream's cuts: each part's span start, or the change of the
/// stream's configuration nearest it, in the window a programme's change takes.
fn cuts_of_audio(spans: &[i64], changes: &[i64]) -> Vec<i64> {
    let near = i64::try_from(crate::events::WINDOW).unwrap_or(0) * TICKS;
    let mut cuts = Vec::with_capacity(spans.len());
    let mut at_least = i64::MIN;
    for &span in spans {
        let change = changes
            .iter()
            .copied()
            .filter(|c| span != i64::MIN && (c - span).abs() <= near)
            .min_by_key(|c| (c - span).abs());
        let cut = change.unwrap_or(span).max(at_least);
        cuts.push(cut);
        at_least = cut;
    }
    cuts
}

/// `assigned` with each audio frame's part, by its stream's cuts.
fn assign_audio(
    frames: &[Frame],
    by_track: &BTreeMap<u64, u16>,
    cuts: &BTreeMap<u16, Vec<i64>>,
    mut assigned: Vec<Option<usize>>,
) -> Vec<Option<usize>> {
    for (f, slot) in frames.iter().zip(&mut assigned) {
        if let Some(c) = by_track.get(&f.track).and_then(|p| cuts.get(p)) {
            *slot = c.partition_point(|&t| t <= f.time).checked_sub(1);
        }
    }
    assigned
}

/// The configuration in force at `tick`: the last at or before it, else the first
/// (a frame can precede what the parameter sets of its access unit came with).
fn config_at(list: &[(i64, Kind)], tick: i64) -> Option<&Kind> {
    list.iter()
        .rev()
        .find(|c| c.0 <= tick)
        .or_else(|| list.first())
        .map(|c| &c.1)
}

/// A track name from the frames' own channel count (the MPT's follows it at a
/// switch, by a moment).
fn channel_name(channels: u8) -> String {
    match channels {
        1 => "mono".to_owned(),
        2 => "stereo".to_owned(),
        6 => "5.1ch".to_owned(),
        8 => "7.1ch".to_owned(),
        n => format!("{n}ch"),
    }
}

/// The caption tracks in the span `from..to` (unwrapped PTS) as text tracks of
/// `out`, whose zero is `origin`: a cue crossing the span's edge is clipped to it.
/// The tracks added.
fn add_captions<O: Write + Seek>(
    out: &mut Writer<O, File>,
    captions: &[crate::ts::CaptionTrack],
    ts_origin: Option<i64>,
    origin: i64,
    from: i64,
    to: i64,
) -> io::Result<usize> {
    let mut added = 0;
    let zero = ts_origin.unwrap_or(0);
    for c in captions {
        let cues: Vec<(i64, i64, String)> = c
            .cues
            .iter()
            .filter_map(|cue| {
                let start = (zero + cue.start * 90).max(from).max(origin);
                let end = (zero + cue.end * 90).min(to);
                (end > start).then(|| {
                    (
                        (start - origin) / 90,
                        (end - origin) / 90,
                        cue.lines.join("\n"),
                    )
                })
            })
            .collect();
        if cues.is_empty() {
            continue;
        }
        let n = out.add_track(Track {
            kind: Kind::Text,
            language: c.lang.as_deref().map(iso639_2),
            name: Some("captions".into()),
            default: false,
        });
        added += 1;
        for (start, end, text) in cues {
            out.frame(n, start, start, true, Some(end - start), text.as_bytes())?;
        }
    }
    Ok(added)
}

/// An `xml:lang` (`ja`) as Matroska's ISO 639-2 (`jpn`).
fn iso639_2(lang: &str) -> String {
    match lang {
        "ja" => "jpn",
        "en" => "eng",
        "zh" => "chi",
        "ko" => "kor",
        other => other,
    }
    .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use isdb_mkv::hevc;

    /// A temporary directory removed at the end.
    struct Dir(PathBuf);

    impl Dir {
        fn new(tag: &str) -> Self {
            let d = std::env::temp_dir().join(format!("isdb-remux-{tag}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&d);
            std::fs::create_dir_all(&d).unwrap();
            Self(d)
        }

        fn names(&self) -> Vec<String> {
            let mut v: Vec<String> = std::fs::read_dir(&self.0)
                .unwrap()
                .map(|e| e.unwrap().file_name().into_string().unwrap())
                .collect();
            v.sort();
            v
        }
    }

    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn hex(s: &str) -> Vec<u8> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect()
    }

    /// A picture in Annex B: an IRAP with real parameter sets, or a slice.
    fn annex_b(ty: u8) -> Vec<u8> {
        let mut nals = Vec::new();
        if (16..=23).contains(&ty) {
            nals.push(hex(
                "40010c06ffff022000000300b00000030000030099000018824090",
            ));
            nals.push(hex("420106022000000300b000000300000300990000a001e020021c4d8d18826490a594e0284244826ca500000303e90000ea60803e9c00814bc00004c4b4000004c5ec5c00004c4b4000004c5ec5c00004c4b4000004c5ec5c00004c4b4000004c5ec440"));
            nals.push(hex("4401c0f7c0cc90"));
        }
        nals.push(vec![ty << 1, 0x01, 0x80, 0x55, ty]);
        let mut out = Vec::new();
        for n in nals {
            out.extend([0, 0, 0, 1]);
            out.extend(n);
        }
        out
    }

    /// An AudioMuxElement with the 2-byte `AudioSpecificConfig` `asc`.
    fn element(asc: u16, payload: &[u8]) -> Vec<u8> {
        let mut bits = String::from("0010000000000000");
        bits += &format!("{:013b}", asc >> 3);
        bits += "000000"; // GASpecificConfig 0 0 0, frameLengthType 0
        bits += "01101010"; // latmBufferFullness
        bits += "00"; // no other data, no CRC
        bits += &format!("{:08b}", payload.len());
        for b in payload {
            bits += &format!("{b:08b}");
        }
        while bits.len() % 8 != 0 {
            bits.push('0');
        }
        (0..bits.len())
            .step_by(8)
            .map(|i| u8::from_str_radix(&bits[i..i + 8], 2).unwrap())
            .collect()
    }

    /// A CodecPrivate of a 2-byte `AudioSpecificConfig`, as written.
    fn codec_private(asc: [u8; 2]) -> [u8; 5] {
        [0x63, 0xa2, 0x82, asc[0], asc[1]]
    }

    fn has(file: &[u8], needle: &[u8]) -> bool {
        file.windows(needle.len()).any(|w| w == needle)
    }

    /// Two events meeting at tick 90 000 (the second's key picture), the audio
    /// going from 5.1 to stereo a little after it, as on air.
    #[test]
    fn a_part_per_event_with_its_own_configuration() {
        assert!(hevc::parse_sps(&hex("420106022000000300b000000300000300990000a001e020021c4d8d18826490a594e0284244826ca500000303e90000ea60803e9c00814bc00004c4b4000004c5ec5c00004c4b4000004c5ec5c00004c4b4000004c5ec5c00004c4b4000004c5ec440")).is_some());
        let dir = Dir::new("split");
        let template: Template = "${START_TIME}-${EVENT_NAME}-${BROADCASTER_NAME}.mkv"
            .parse()
            .unwrap();
        let mut m = Mkv::create_split(&dir.0, template, "test").unwrap();
        const V: u16 = 0xf300;
        const A: u16 = 0xf310;
        // The first event's pictures: key at 0, trailing every 9000.
        for (i, pts) in [
            0i64, 9000, 18_000, 27_000, 36_000, 45_000, 54_000, 63_000, 72_000,
        ]
        .into_iter()
        .enumerate()
        {
            let ty = if i == 0 { 21 } else { 1 };
            m.picture(V, pts, pts - 1000, &annex_b(ty)).unwrap();
        }
        // The next: its key picture, a RASL shown before it, then trailing.
        m.picture(V, 90_000, 80_000, &annex_b(21)).unwrap();
        m.picture(V, 81_000, 81_000, &annex_b(9)).unwrap();
        m.picture(V, 99_000, 82_000, &annex_b(1)).unwrap();
        m.picture(V, 108_000, 83_000, &annex_b(1)).unwrap();
        // A later key picture's RASL stays: its references are in the file.
        m.picture(V, 126_000, 84_000, &annex_b(21)).unwrap();
        m.picture(V, 117_000, 85_000, &annex_b(9)).unwrap();
        // Audio: 5.1 from 0 until 100 000, past the second event's start, then
        // stereo.
        let (surround, stereo) = (0x11b0u16, 0x1190u16);
        for pts in (0..130_000).step_by(10_000) {
            let asc = if pts < 100_000 { surround } else { stereo };
            m.audio(A, pts, &element(asc, &[0xa5, 0x5a])).unwrap();
        }
        m.data(0xf340, 7, Some(1), b"early", Some(1_000));
        m.data(0xf340, 7, Some(2), b"late!", Some(95_000));
        let parts = [
            Part {
                start: 0,
                title: Some("first".to_owned()),
            },
            Part {
                start: 90_000,
                title: Some("a/second".to_owned()),
            },
        ];
        // PTS 0 is 2026-11-05 02:24:00 JST.
        let clock = Clock {
            ticks: 0,
            ntp: ((61_348 - 15_020) * 86_400 + 17 * 3600 + 24 * 60) << 32,
        };
        // A cue crossing the cut, and one in each file.
        let cue = |start, end, text: &str| isdb_caption::Cue {
            start,
            end,
            lines: vec![text.to_owned()],
            paragraphs: Vec::new(),
        };
        let captions = [crate::ts::CaptionTrack {
            pid: 0xf330,
            lang: Some("ja".to_owned()),
            cues: vec![
                cue(100, 200, "in-first"),
                cue(900, 1100, "crossing"),
                cue(1200, 1300, "in-second"),
            ],
            rejected: 0,
        }];
        let w = m
            .finish_parts(&captions, Some(0), &parts, clock, "局")
            .unwrap();
        assert_eq!(w.len(), 2);
        assert_eq!(
            dir.names(),
            vec![
                "2026-11-05_02-24-00.000-first-局.mkv".to_owned(),
                "2026-11-05_02-24-01.000-a_second-局.mkv".to_owned()
            ],
            "the scratch is gone, the name is safe, the time is the first frame's"
        );
        let first = std::fs::read(&w[0].path).unwrap();
        let second = std::fs::read(&w[1].path).unwrap();
        // 9 pictures + 10 audio frames (0 .. 90 000); then 4 pictures of the 6
        // and... the audio keeps its 5.1 until its change, 100 000.
        assert_eq!(w[0].frames, 9 + 10);
        assert_eq!(w[1].frames, 5 + 3, "one RASL left out, the later one kept");
        assert!(has(&first, &codec_private([0x11, 0xb0])));
        assert!(!has(&first, &codec_private([0x11, 0x90])));
        assert!(has(&second, &codec_private([0x11, 0x90])));
        assert!(!has(&second, &codec_private([0x11, 0xb0])));
        assert!(has(&first, b"5.1ch") && has(&second, b"stereo"));
        assert!(has(&first, b"first") && has(&second, b"a/second"), "titles");
        // Each file has the cues in its span, the crossing one in both, clipped.
        assert!(has(&first, b"in-first") && !has(&first, b"in-second"));
        assert!(has(&second, b"in-second") && !has(&second, b"in-first"));
        assert!(has(&first, b"crossing") && has(&second, b"crossing"));
        assert_eq!(w[0].tracks, 3);
        // Each file's data items are those that arrived in it.
        assert!(has(&first, b"early") && !has(&first, b"late!"));
        assert!(has(&second, b"late!") && !has(&second, b"early"));
        assert_eq!(w[0].attachments, 1);
        assert_eq!(w[1].attachments, 1);
    }

    #[test]
    fn a_name_already_taken_is_not_overwritten() {
        let dir = Dir::new("taken");
        std::fs::write(dir.0.join("x.mkv"), b"keep").unwrap();
        let (_, p) = create_new(&dir.0, "x.mkv").unwrap();
        assert_eq!(p.file_name().unwrap(), "x-2.mkv");
        let (_, p) = create_new(&dir.0, "x.mkv").unwrap();
        assert_eq!(p.file_name().unwrap(), "x-3.mkv");
        assert_eq!(std::fs::read(dir.0.join("x.mkv")).unwrap(), b"keep");
    }

    #[test]
    fn audio_cuts_follow_a_change_near_the_span() {
        let second = TICKS;
        let spans = [i64::MIN, 100 * second, 200 * second];
        // No change: at the spans.
        assert_eq!(cuts_of_audio(&spans, &[]), spans);
        // Just after one, just before another; one far from any is not a cut.
        let changes = [100 * second + 1, 150 * second, 200 * second - 1];
        assert_eq!(
            cuts_of_audio(&spans, &changes),
            vec![i64::MIN, 100 * second + 1, 200 * second - 1]
        );
    }

    #[test]
    fn items_are_taken_by_arrival() {
        let mut d = Items::default();
        for (arrival, offset, n) in [(i64::MIN, 0, 1), (5, 1, 2), (5, 3, 1), (9, 4, 3)] {
            d.marks.push((arrival, offset));
            d.bytes.extend(std::iter::repeat_n(0u8, n));
        }
        assert_eq!(d.between(i64::MIN, 5).len(), 1);
        assert_eq!(d.between(5, 9).len(), 3);
        assert_eq!(d.between(9, i64::MAX).len(), 3);
        assert_eq!(d.between(10, i64::MAX).len(), 0);
    }
}
