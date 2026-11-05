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
//! Frames are spilled to a scratch file next to the output until the
//! end (the writer is two-pass), so the track list — configurations
//! learnt from the stream, captions aligned at the end — is complete
//! when the header is written. Block times are milliseconds from the
//! earliest PTS of any stream: the zero of a player's timeline, the same
//! as the TS form's sidecars.

use isdb_mkv::{Chapter, Kind, Track, Writer};
use std::collections::BTreeMap;
use std::fs::File;
use std::io::{self, BufWriter};
use std::path::{Path, PathBuf};

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
    out: PathBuf,
    scratch: PathBuf,
    finished: bool,
}

impl Drop for Files {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.scratch);
        if !self.finished {
            let _ = std::fs::remove_file(&self.out);
        }
    }
}

/// The Matroska sink.
pub struct Mkv {
    writer: Writer<BufWriter<File>, File>,
    files: Files,
    /// The streams' MPT descriptions, by asset packet id.
    decls: BTreeMap<u16, Decl>,
    /// The codec configuration learnt per stream.
    configs: BTreeMap<u16, Kind>,
    /// Each audio stream's `AudioSpecificConfig` in force.
    audio_config: BTreeMap<u16, Vec<u8>>,
    /// The parameter sets seen (the `hvcC`): VPS, SPS, PPS.
    vps: Option<Vec<u8>>,
    sps: Option<Vec<u8>>,
    pps: Option<Vec<u8>>,
    /// The track each stream's frames went to (provisional numbers,
    /// renumbered at the end): assigned at a stream's first frame.
    provisional: BTreeMap<u16, u64>,
    /// The data items per (asset, MPU), in arrival order.
    data: BTreeMap<(u16, u32), Vec<u8>>,
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
        let scratch = File::options()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(&scratch_path)?;
        Ok(Self {
            writer: Writer::new(out, scratch, app),
            files: Files {
                out: path.to_owned(),
                scratch: scratch_path,
                finished: false,
            },
            decls: BTreeMap::new(),
            configs: BTreeMap::new(),
            audio_config: BTreeMap::new(),
            vps: None,
            sps: None,
            pps: None,
            provisional: BTreeMap::new(),
            data: BTreeMap::new(),
        })
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
        let mut key = false;
        let (mut vps, mut sps, mut pps) = (None, None, None);
        let block = isdb_mkv::hevc::annex_b_to_length(annex_b, |nal| {
            match (nal[0] >> 1) & 0x3f {
                // Parameter sets stay in-band too (a decoder that
                // re-reads them mid-stream) — the AUD does not.
                32 => vps = vps.take().or_else(|| Some(nal.to_vec())),
                33 => sps = sps.take().or_else(|| Some(nal.to_vec())),
                34 => pps = pps.take().or_else(|| Some(nal.to_vec())),
                35 => return false,
                16..=23 => key = true,
                _ => {}
            }
            true
        });
        if self.vps.is_none() {
            self.vps = vps;
        }
        if self.sps.is_none() {
            self.sps = sps;
        }
        if self.pps.is_none() {
            self.pps = pps;
        }
        if !self.configs.contains_key(&pid)
            && let (Some(v), Some(s), Some(p)) = (&self.vps, &self.sps, &self.pps)
            && let Some(hvcc) = isdb_mkv::hevc::hvcc(&[v], &[s], &[p], &[])
            && let Some(sps) = isdb_mkv::hevc::parse_sps(s)
        {
            self.configs.insert(
                pid,
                Kind::Hevc {
                    hvcc,
                    width: sps.width,
                    height: sps.height,
                    frame_ns: None,
                },
            );
        }
        self.frame(pid, dts, pts, key, &block)
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
        if let Some(config) = &f.config
            && self.audio_config.get(&pid) != Some(config)
        {
            if let Some(first) = self.audio_config.get(&pid) {
                tracing::warn!(
                    pid,
                    from = ?isdb_mkv::asc_rate_channels(first),
                    to = ?isdb_mkv::asc_rate_channels(config),
                    "mkv: an audio stream changes configuration; its track keeps the first, \
                     so a player may not decode the frames from here on"
                );
            }
            self.audio_config.insert(pid, config.clone());
        }
        if let Some(config) = f.config
            && !self.configs.contains_key(&pid)
            && let Some((rate, channels)) = isdb_mkv::asc_rate_channels(&config)
        {
            self.configs.insert(
                pid,
                Kind::Aac {
                    config,
                    rate,
                    channels,
                },
            );
        }
        self.frame(pid, pts, pts, true, &f.payload)
    }

    /// Add a chapter, in the order a player lists them.
    pub fn chapter(&mut self, c: Chapter) {
        self.writer.add_chapter(c);
    }

    /// One item of an untimed asset.
    pub fn data(&mut self, pid: u16, mpu: u32, item: Option<u32>, bytes: &[u8]) {
        let b = self.data.entry((pid, mpu)).or_default();
        b.extend_from_slice(&item.unwrap_or(0).to_be_bytes());
        b.extend_from_slice(&u32::try_from(bytes.len()).unwrap_or(u32::MAX).to_be_bytes());
        b.extend_from_slice(bytes);
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
        // The tracks with a configuration — video, then the audio, each
        // in asset packet id order — and the frames renumbered to them
        // (the provisional numbers went by first frame); a stream with
        // no configuration is left out.
        let rank = |pid: &u16| match self.configs.get(pid) {
            Some(Kind::Hevc { .. }) => 0,
            Some(Kind::Aac { .. }) => 1,
            _ => 2,
        };
        let mut pids: Vec<u16> = self.provisional.keys().copied().collect();
        pids.sort_by_key(|p| (rank(p), *p));
        let mut map = vec![None; self.provisional.len()];
        let mut first_audio = true;
        let mut tracks = 0;
        for pid in pids {
            let Some(kind) = self.configs.get(&pid).cloned() else {
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
        for ((pid, mpu), bytes) in std::mem::take(&mut self.data) {
            self.writer.attach(
                &format!("{pid:04x}-{mpu}.mmtitems"),
                "application/octet-stream",
                &format!("MMT asset {pid:#06x}, package {mpu}: [item id u32][length u32][bytes]…"),
                bytes,
            );
        }
        let (out, _) = self.writer.finish()?;
        out.into_inner()
            .map_err(io::IntoInnerError::into_error)?
            .sync_all()?;
        self.files.finished = true;
        Ok((tracks, attachments))
    }
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
