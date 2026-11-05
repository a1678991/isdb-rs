//! A Matroska writer (RFC 9559 over EBML, RFC 8794) for the media a
//! decoder of ISDB-S3 delivers: HEVC video (`V_MPEGH/ISO/HEVC`), AAC
//! audio (`A_AAC`, raw frames — what a LATM AudioMuxElement carries),
//! UTF-8 text subtitles (`S_TEXT/UTF8`), chapters and attachments.
//!
//! Two passes: [`Writer::frame`] spills each frame to a scratch file and
//! indexes it; [`Writer::finish`] writes the header with every track
//! added by then, the frames in decode order across tracks (`order`, the
//! decode time — a video's DTS — ties kept in arrival order; the block
//! time is the presentation time), a cluster at each video keyframe (and
//! at the latest every [`MAX_CLUSTER_MS`]), the chapters (RFC 9559 §6.6:
//! after the Info, before the Tracks), the Cues of the keyframes and the
//! attachments, and seeks back to fill in the Segment size, the
//! SeekHead and the Duration. So tracks — subtitles whose cue times are
//! only known at the end, a configuration learnt from the stream — can be
//! added at any time, the output is `Write + Seek` (a file, not a pipe),
//! and the scratch holds the media until the end. The [`hevc`] module
//! builds the `hvcC`.

pub mod hevc;

use std::io::{self, Read, Seek, SeekFrom, Write};

/// The longest a cluster runs (ms).
pub const MAX_CLUSTER_MS: i64 = 5_000;
/// The scratch's write buffer.
const SPILL_BUFFER: usize = 1 << 20;

mod id {
    pub const EBML: u32 = 0x1A45_DFA3;
    pub const EBML_VERSION: u32 = 0x4286;
    pub const EBML_READ_VERSION: u32 = 0x42F7;
    pub const EBML_MAX_ID_LENGTH: u32 = 0x42F2;
    pub const EBML_MAX_SIZE_LENGTH: u32 = 0x42F3;
    pub const DOC_TYPE: u32 = 0x4282;
    pub const DOC_TYPE_VERSION: u32 = 0x4287;
    pub const DOC_TYPE_READ_VERSION: u32 = 0x4285;
    pub const VOID: u32 = 0xEC;
    pub const SEGMENT: u32 = 0x1853_8067;
    pub const SEEK_HEAD: u32 = 0x114D_9B74;
    pub const SEEK: u32 = 0x4DBB;
    pub const SEEK_ID: u32 = 0x53AB;
    pub const SEEK_POSITION: u32 = 0x53AC;
    pub const INFO: u32 = 0x1549_A966;
    pub const TIMESTAMP_SCALE: u32 = 0x2A_D7B1;
    pub const DURATION: u32 = 0x4489;
    pub const MUXING_APP: u32 = 0x4D80;
    pub const WRITING_APP: u32 = 0x5741;
    pub const TITLE: u32 = 0x7BA9;
    pub const TRACKS: u32 = 0x1654_AE6B;
    pub const TRACK_ENTRY: u32 = 0xAE;
    pub const TRACK_NUMBER: u32 = 0xD7;
    pub const TRACK_UID: u32 = 0x73C5;
    pub const TRACK_TYPE: u32 = 0x83;
    pub const FLAG_DEFAULT: u32 = 0x88;
    pub const FLAG_LACING: u32 = 0x9C;
    pub const DEFAULT_DURATION: u32 = 0x23_E383;
    pub const NAME: u32 = 0x536E;
    pub const LANGUAGE: u32 = 0x22_B59C;
    pub const CODEC_ID: u32 = 0x86;
    pub const CODEC_PRIVATE: u32 = 0x63A2;
    pub const VIDEO: u32 = 0xE0;
    pub const PIXEL_WIDTH: u32 = 0xB0;
    pub const PIXEL_HEIGHT: u32 = 0xBA;
    pub const AUDIO: u32 = 0xE1;
    pub const SAMPLING_FREQUENCY: u32 = 0xB5;
    pub const CHANNELS: u32 = 0x9F;
    pub const CLUSTER: u32 = 0x1F43_B675;
    pub const TIMESTAMP: u32 = 0xE7;
    pub const SIMPLE_BLOCK: u32 = 0xA3;
    pub const BLOCK_GROUP: u32 = 0xA0;
    pub const BLOCK: u32 = 0xA1;
    pub const BLOCK_DURATION: u32 = 0x9B;
    pub const CUES: u32 = 0x1C53_BB6B;
    pub const CUE_POINT: u32 = 0xBB;
    pub const CUE_TIME: u32 = 0xB3;
    pub const CUE_TRACK_POSITIONS: u32 = 0xB7;
    pub const CUE_TRACK: u32 = 0xF7;
    pub const CUE_CLUSTER_POSITION: u32 = 0xF1;
    pub const ATTACHMENTS: u32 = 0x1941_A469;
    pub const ATTACHED_FILE: u32 = 0x61A7;
    pub const FILE_DESCRIPTION: u32 = 0x467E;
    pub const FILE_NAME: u32 = 0x466E;
    pub const FILE_MEDIA_TYPE: u32 = 0x4660;
    pub const FILE_DATA: u32 = 0x465C;
    pub const FILE_UID: u32 = 0x46AE;
    pub const CHAPTERS: u32 = 0x1043_A770;
    pub const EDITION_ENTRY: u32 = 0x45B9;
    pub const EDITION_UID: u32 = 0x45BC;
    pub const EDITION_FLAG_DEFAULT: u32 = 0x45DB;
    pub const CHAPTER_ATOM: u32 = 0xB6;
    pub const CHAPTER_UID: u32 = 0x73C4;
    pub const CHAPTER_TIME_START: u32 = 0x91;
    pub const CHAPTER_TIME_END: u32 = 0x92;
    pub const CHAPTER_DISPLAY: u32 = 0x80;
    pub const CHAP_STRING: u32 = 0x85;
    pub const CHAP_LANGUAGE: u32 = 0x437C;
}

/// An EBML element under construction (its children appended to `body`).
#[derive(Debug, Default, Clone)]
pub struct Element {
    id: u32,
    body: Vec<u8>,
}

impl Element {
    fn new(id: u32) -> Self {
        Self {
            id,
            body: Vec::new(),
        }
    }

    fn uint(mut self, id: u32, v: u64) -> Self {
        let bytes = v.to_be_bytes();
        let skip = bytes.iter().take_while(|&&b| b == 0).count().min(7);
        put_id(&mut self.body, id);
        put_size(&mut self.body, (8 - skip) as u64);
        self.body.extend_from_slice(&bytes[skip..]);
        self
    }

    fn float(mut self, id: u32, v: f64) -> Self {
        put_id(&mut self.body, id);
        put_size(&mut self.body, 8);
        self.body.extend_from_slice(&v.to_be_bytes());
        self
    }

    fn bytes(mut self, id: u32, v: &[u8]) -> Self {
        put_id(&mut self.body, id);
        put_size(&mut self.body, v.len() as u64);
        self.body.extend_from_slice(v);
        self
    }

    fn string(self, id: u32, v: &str) -> Self {
        self.bytes(id, v.as_bytes())
    }

    fn child(mut self, e: &Element) -> Self {
        e.encode_into(&mut self.body);
        self
    }

    fn encode_into(&self, out: &mut Vec<u8>) {
        put_id(out, self.id);
        put_size(out, self.body.len() as u64);
        out.extend_from_slice(&self.body);
    }

    fn encode(&self) -> Vec<u8> {
        let mut v = Vec::with_capacity(self.body.len() + 12);
        self.encode_into(&mut v);
        v
    }
}

fn put_id(out: &mut Vec<u8>, id: u32) {
    let b = id.to_be_bytes();
    let skip = b.iter().take_while(|&&x| x == 0).count();
    out.extend_from_slice(&b[skip..]);
}

/// The shortest data size VINT (never the all-ones "unknown" pattern).
fn put_size(out: &mut Vec<u8>, n: u64) {
    let mut len = 1;
    while len < 8 && n >= (1u64 << (7 * len)) - 1 {
        len += 1;
    }
    put_size_len(out, n, len);
}

/// A data size VINT of exactly `len` bytes.
fn put_size_len(out: &mut Vec<u8>, n: u64, len: usize) {
    let v = n | 1u64 << (7 * len);
    out.extend_from_slice(&v.to_be_bytes()[8 - len..]);
}

/// What a track carries.
#[derive(Debug, Clone, PartialEq)]
pub enum Kind {
    /// HEVC: the `hvcC` and the picture size.
    Hevc {
        /// `HEVCDecoderConfigurationRecord`.
        hvcc: Vec<u8>,
        /// Luma width.
        width: u32,
        /// Luma height.
        height: u32,
        /// One frame's duration (ns), when constant.
        frame_ns: Option<u64>,
    },
    /// AAC: the `AudioSpecificConfig`, the rate and channels.
    Aac {
        /// `AudioSpecificConfig`.
        config: Vec<u8>,
        /// Sampling frequency.
        rate: u32,
        /// Channel count.
        channels: u8,
    },
    /// UTF-8 text subtitles (each block one cue's text, with a duration).
    Text,
}

/// One track's description.
#[derive(Debug, Clone, PartialEq)]
pub struct Track {
    /// What it carries.
    pub kind: Kind,
    /// ISO 639-2 language (`jpn`), when known.
    pub language: Option<String>,
    /// A human-readable name.
    pub name: Option<String>,
    /// Whether a player picks it by default.
    pub default: bool,
}

/// One chapter of the segment's timeline (RFC 9559 §20.2): a simple chapter
/// of the one, default edition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chapter {
    /// Its start (ns, not scaled by the `TimestampScale`).
    pub start_ns: u64,
    /// Its end, excluded (ns), when known; at least `start_ns`.
    pub end_ns: Option<u64>,
    /// Its name; a chapter without one has no `ChapterDisplay`.
    pub title: Option<String>,
    /// The title's ISO 639-2 language (`jpn`), when known.
    pub language: Option<String>,
}

/// One spilled frame.
#[derive(Debug, Clone, Copy)]
struct Indexed {
    /// The write order: the decode time (ms).
    order: i64,
    /// The block time: the presentation time (ms).
    time: i64,
    track: u64,
    keyframe: bool,
    /// A text block's duration (ms).
    duration: Option<i64>,
    /// Where its bytes are in the scratch.
    at: u64,
    len: u32,
}

/// The writer: `W` the output, `S` the scratch the frames wait in.
pub struct Writer<W: Write + Seek, S: Read + Write + Seek> {
    out: W,
    scratch: S,
    /// The scratch writes not yet written (a buffer of its own: the
    /// scratch is read back too).
    pending: Vec<u8>,
    scratch_len: u64,
    tracks: Vec<Track>,
    title: Option<String>,
    app: String,
    index: Vec<Indexed>,
    attachments: Vec<(String, String, String, Vec<u8>)>,
    chapters: Vec<Chapter>,
}

impl<W: Write + Seek, S: Read + Write + Seek> Writer<W, S> {
    /// A writer over `out` with `scratch` for the frames until
    /// [`Writer::finish`]; `app` names the writing application.
    pub fn new(out: W, scratch: S, app: &str) -> Self {
        Self {
            out,
            scratch,
            pending: Vec::with_capacity(SPILL_BUFFER),
            scratch_len: 0,
            tracks: Vec::new(),
            title: None,
            app: app.to_owned(),
            index: Vec::new(),
            attachments: Vec::new(),
            chapters: Vec::new(),
        }
    }

    /// The segment's title.
    pub fn set_title(&mut self, title: &str) {
        self.title = Some(title.to_owned());
    }

    /// Add a track; its number (from 1).
    pub fn add_track(&mut self, t: Track) -> u64 {
        self.tracks.push(t);
        self.tracks.len() as u64
    }

    /// Rebase every frame given so far: `t ↦ (t − origin) / divisor`,
    /// rounded to the nearest (frames given in another unit — 90 kHz
    /// ticks — become ms with `divisor` 90).
    pub fn rebase(&mut self, origin: i64, divisor: i64) {
        let f = |t: i64| (t - origin + divisor / 2).div_euclid(divisor);
        for x in &mut self.index {
            x.order = f(x.order);
            x.time = f(x.time);
        }
    }

    /// Renumber the frames given so far (`map[old - 1]` the new number,
    /// `None` drops the frame).
    pub fn renumber(&mut self, map: &[Option<u64>]) {
        self.index.retain_mut(|x| {
            match usize::try_from(x.track)
                .ok()
                .and_then(|t| map.get(t.checked_sub(1)?))
                .copied()
                .flatten()
            {
                Some(n) => {
                    x.track = n;
                    true
                }
                None => false,
            }
        });
    }

    /// Attach a file: name, media type, description.
    pub fn attach(&mut self, name: &str, media_type: &str, description: &str, data: Vec<u8>) {
        self.attachments.push((
            name.to_owned(),
            media_type.to_owned(),
            description.to_owned(),
            data,
        ));
    }

    /// Add a chapter, in the order a player lists them.
    pub fn add_chapter(&mut self, c: Chapter) {
        self.chapters.push(c);
    }

    /// One frame of `track`: `order` its decode time and `time` its
    /// presentation time (ms; equal but for a reordered video), `duration`
    /// a text block's.
    ///
    /// # Errors
    ///
    /// The scratch's write.
    pub fn frame(
        &mut self,
        track: u64,
        order: i64,
        time: i64,
        keyframe: bool,
        duration: Option<i64>,
        data: &[u8],
    ) -> io::Result<()> {
        let Ok(len) = u32::try_from(data.len()) else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "a frame over 4 GiB",
            ));
        };
        self.pending.extend_from_slice(data);
        if self.pending.len() >= SPILL_BUFFER {
            self.scratch.write_all(&self.pending)?;
            self.pending.clear();
        }
        self.index.push(Indexed {
            order,
            time,
            track,
            keyframe,
            duration,
            at: self.scratch_len,
            len,
        });
        self.scratch_len += u64::from(len);
        Ok(())
    }

    /// Write the file; the output and the scratch back.
    ///
    /// # Errors
    ///
    /// The writes' and the scratch reads' own.
    pub fn finish(mut self) -> io::Result<(W, S)> {
        self.scratch.write_all(&self.pending)?;
        self.pending = Vec::new();
        self.scratch.flush()?;
        // Decode order across the tracks; the sort is stable, so a
        // track's own frames keep their arrival order on equal times.
        self.index.sort_by_key(|f| f.order);
        let video = self
            .tracks
            .iter()
            .position(|t| matches!(t.kind, Kind::Hevc { .. }))
            .map(|i| i as u64 + 1);
        // The header.
        let ebml = Element::new(id::EBML)
            .uint(id::EBML_VERSION, 1)
            .uint(id::EBML_READ_VERSION, 1)
            .uint(id::EBML_MAX_ID_LENGTH, 4)
            .uint(id::EBML_MAX_SIZE_LENGTH, 8)
            .string(id::DOC_TYPE, "matroska")
            .uint(id::DOC_TYPE_VERSION, 4)
            .uint(id::DOC_TYPE_READ_VERSION, 2);
        self.out.write_all(&ebml.encode())?;
        let mut seg = Vec::new();
        put_id(&mut seg, id::SEGMENT);
        seg.extend_from_slice(&[0x01, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff]);
        self.out.write_all(&seg)?;
        let segment = self.out.stream_position()?;
        // The SeekHead's room (rewritten at the end).
        const ROOM: usize = 160;
        self.out.write_all(&void(ROOM))?;
        let mut positions = Vec::new();
        let first = self.index.iter().map(|f| f.time).min().unwrap_or(0);
        let last = self
            .index
            .iter()
            .map(|f| f.time + f.duration.unwrap_or(0))
            .max()
            .unwrap_or(0);
        let mut info = Element::new(id::INFO)
            .uint(id::TIMESTAMP_SCALE, 1_000_000)
            .string(id::MUXING_APP, &self.app)
            .string(id::WRITING_APP, &self.app);
        if let Some(t) = &self.title {
            info = info.string(id::TITLE, t);
        }
        #[allow(clippy::cast_precision_loss, reason = "a duration in ms")]
        let info = info.float(id::DURATION, (last - first).max(0) as f64);
        positions.push((id::INFO, self.out.stream_position()? - segment));
        self.out.write_all(&info.encode())?;
        if !self.chapters.is_empty() {
            positions.push((id::CHAPTERS, self.out.stream_position()? - segment));
            self.out.write_all(&chapters(&self.chapters).encode())?;
        }
        positions.push((id::TRACKS, self.out.stream_position()? - segment));
        let mut tracks = Element::new(id::TRACKS);
        for (i, t) in self.tracks.iter().enumerate() {
            tracks = tracks.child(&track_entry(i as u64 + 1, t));
        }
        self.out.write_all(&tracks.encode())?;
        // The clusters.
        let mut cues = Vec::new();
        // The open cluster: its timestamp, file position, whether it holds
        // a video block yet.
        let mut cluster: Option<(i64, u64, bool)> = None;
        let mut body = Vec::new();
        let mut buf = Vec::new();
        let mut first_cluster = None;
        for f in std::mem::take(&mut self.index) {
            let key = f.keyframe && Some(f.track) == video;
            // A video keyframe starts a cluster (a seek point) unless the
            // open one has no video yet.
            let open = match cluster {
                None => true,
                Some((start, _, has_video)) => {
                    (key && has_video)
                        || f.time - start >= MAX_CLUSTER_MS
                        || f.time - start > i64::from(i16::MAX)
                        || f.time - start < i64::from(i16::MIN)
                }
            };
            if open {
                write_cluster(&mut self.out, &mut body)?;
                let pos = self.out.stream_position()? - segment;
                first_cluster.get_or_insert(pos);
                cluster = Some((f.time, pos, false));
                let e =
                    Element::new(0).uint(id::TIMESTAMP, u64::try_from(f.time.max(0)).unwrap_or(0));
                body.extend_from_slice(&e.body);
            }
            let (start, pos, has_video) = cluster.as_mut().expect("opened above");
            if Some(f.track) == video {
                if key {
                    cues.push((f.time, f.track, *pos));
                }
                *has_video = true;
            }
            let start = *start;
            buf.resize(f.len as usize, 0);
            self.scratch.seek(SeekFrom::Start(f.at))?;
            self.scratch.read_exact(&mut buf)?;
            let mut block = Vec::with_capacity(buf.len() + 4);
            put_size(&mut block, f.track);
            block.extend_from_slice(&i16::try_from(f.time - start).unwrap_or(0).to_be_bytes());
            match f.duration {
                None => {
                    block.push(if f.keyframe { 0x80 } else { 0x00 });
                    block.extend_from_slice(&buf);
                    put_id(&mut body, id::SIMPLE_BLOCK);
                    put_size(&mut body, block.len() as u64);
                    body.extend_from_slice(&block);
                }
                Some(d) => {
                    block.push(0x00);
                    block.extend_from_slice(&buf);
                    Element::new(id::BLOCK_GROUP)
                        .bytes(id::BLOCK, &block)
                        .uint(id::BLOCK_DURATION, u64::try_from(d.max(0)).unwrap_or(0))
                        .encode_into(&mut body);
                }
            }
        }
        write_cluster(&mut self.out, &mut body)?;
        if let Some(pos) = first_cluster {
            positions.push((id::CLUSTER, pos));
        }
        if !cues.is_empty() {
            positions.push((id::CUES, self.out.stream_position()? - segment));
            let mut e = Element::new(id::CUES);
            for (time, track, pos) in cues {
                e = e.child(
                    &Element::new(id::CUE_POINT)
                        .uint(id::CUE_TIME, u64::try_from(time.max(0)).unwrap_or(0))
                        .child(
                            &Element::new(id::CUE_TRACK_POSITIONS)
                                .uint(id::CUE_TRACK, track)
                                .uint(id::CUE_CLUSTER_POSITION, pos),
                        ),
                );
            }
            self.out.write_all(&e.encode())?;
        }
        if !self.attachments.is_empty() {
            positions.push((id::ATTACHMENTS, self.out.stream_position()? - segment));
            let mut a = Element::new(id::ATTACHMENTS);
            for (i, (name, media, desc, data)) in self.attachments.iter().enumerate() {
                let mut f = Element::new(id::ATTACHED_FILE);
                if !desc.is_empty() {
                    f = f.string(id::FILE_DESCRIPTION, desc);
                }
                a = a.child(
                    &f.string(id::FILE_NAME, name)
                        .string(id::FILE_MEDIA_TYPE, media)
                        .bytes(id::FILE_DATA, data)
                        .uint(id::FILE_UID, i as u64 + 1),
                );
            }
            self.out.write_all(&a.encode())?;
        }
        let end = self.out.stream_position()?;
        // The Segment's size, then the SeekHead in its room.
        let mut size = Vec::new();
        put_size_len(&mut size, end - segment, 8);
        self.out.seek(SeekFrom::Start(segment - 8))?;
        self.out.write_all(&size)?;
        let mut sh = Element::new(id::SEEK_HEAD);
        for (eid, pos) in positions {
            let mut idb = Vec::new();
            put_id(&mut idb, eid);
            sh = sh.child(
                &Element::new(id::SEEK)
                    .bytes(id::SEEK_ID, &idb)
                    .uint(id::SEEK_POSITION, pos),
            );
        }
        let mut bytes = sh.encode();
        if bytes.len() + 2 <= ROOM {
            bytes.extend_from_slice(&void(ROOM - bytes.len()));
            self.out.seek(SeekFrom::Start(segment))?;
            self.out.write_all(&bytes)?;
        }
        self.out.seek(SeekFrom::Start(end))?;
        self.out.flush()?;
        Ok((self.out, self.scratch))
    }
}

/// Write one Cluster of `body` (its Timestamp and blocks) and clear it.
fn write_cluster(out: &mut impl Write, body: &mut Vec<u8>) -> io::Result<()> {
    if body.is_empty() {
        return Ok(());
    }
    let mut head = Vec::with_capacity(12);
    put_id(&mut head, id::CLUSTER);
    put_size(&mut head, body.len() as u64);
    out.write_all(&head)?;
    out.write_all(body)?;
    body.clear();
    Ok(())
}

/// A Void element spanning exactly `n` bytes (n ≥ 2).
fn void(n: usize) -> Vec<u8> {
    let mut v = Vec::with_capacity(n);
    put_id(&mut v, id::VOID);
    // A 1-byte size up to 126 data bytes, else an 8-byte one.
    if n - 2 <= 126 {
        put_size_len(&mut v, (n - 2) as u64, 1);
    } else {
        put_size_len(&mut v, (n - 9) as u64, 8);
    }
    v.resize(n, 0);
    v
}

/// The Chapters of one default edition (UIDs from 1; RFC 9559 §5.1.7 forbids
/// 0). The language is always written: an absent one would read as `eng`.
fn chapters(list: &[Chapter]) -> Element {
    let mut edition = Element::new(id::EDITION_ENTRY)
        .uint(id::EDITION_UID, 1)
        .uint(id::EDITION_FLAG_DEFAULT, 1);
    for (i, c) in list.iter().enumerate() {
        let mut atom = Element::new(id::CHAPTER_ATOM)
            .uint(id::CHAPTER_UID, i as u64 + 1)
            .uint(id::CHAPTER_TIME_START, c.start_ns);
        if let Some(end) = c.end_ns {
            atom = atom.uint(id::CHAPTER_TIME_END, end);
        }
        if let Some(title) = &c.title {
            atom = atom.child(
                &Element::new(id::CHAPTER_DISPLAY)
                    .string(id::CHAP_STRING, title)
                    .string(id::CHAP_LANGUAGE, c.language.as_deref().unwrap_or("und")),
            );
        }
        edition = edition.child(&atom);
    }
    Element::new(id::CHAPTERS).child(&edition)
}

fn track_entry(number: u64, t: &Track) -> Element {
    let (ty, codec) = match &t.kind {
        Kind::Hevc { .. } => (1, "V_MPEGH/ISO/HEVC"),
        Kind::Aac { .. } => (2, "A_AAC"),
        Kind::Text => (17, "S_TEXT/UTF8"),
    };
    let mut e = Element::new(id::TRACK_ENTRY)
        .uint(id::TRACK_NUMBER, number)
        .uint(id::TRACK_UID, number)
        .uint(id::TRACK_TYPE, ty)
        .uint(id::FLAG_DEFAULT, u64::from(t.default))
        .uint(id::FLAG_LACING, 0)
        .string(id::CODEC_ID, codec)
        .string(id::LANGUAGE, t.language.as_deref().unwrap_or("und"));
    if let Some(n) = &t.name {
        e = e.string(id::NAME, n);
    }
    match &t.kind {
        Kind::Hevc {
            hvcc,
            width,
            height,
            frame_ns,
        } => {
            e = e.bytes(id::CODEC_PRIVATE, hvcc);
            if let Some(ns) = frame_ns {
                e = e.uint(id::DEFAULT_DURATION, *ns);
            }
            e = e.child(
                &Element::new(id::VIDEO)
                    .uint(id::PIXEL_WIDTH, u64::from(*width))
                    .uint(id::PIXEL_HEIGHT, u64::from(*height)),
            );
        }
        Kind::Aac {
            config,
            rate,
            channels,
        } => {
            e = e.bytes(id::CODEC_PRIVATE, config).child(
                &Element::new(id::AUDIO)
                    .float(id::SAMPLING_FREQUENCY, f64::from(*rate))
                    .uint(id::CHANNELS, u64::from(*channels)),
            );
        }
        Kind::Text => {}
    }
    e
}

/// The rate and channel count an `AudioSpecificConfig` names (object
/// type, frequency index or the explicit 24-bit rate, channel
/// configuration; configuration 7 is 7.1 = 8 channels).
#[must_use]
pub fn asc_rate_channels(asc: &[u8]) -> Option<(u32, u8)> {
    const RATES: [u32; 13] = [
        96000, 88200, 64000, 48000, 44100, 32000, 24000, 22050, 16000, 12000, 11025, 8000, 7350,
    ];
    let bits = |from: usize, n: usize| -> Option<u32> {
        let mut v = 0u32;
        for i in from..from + n {
            let b = (asc.get(i / 8)? >> (7 - i % 8)) & 1;
            v = v << 1 | u32::from(b);
        }
        Some(v)
    };
    let mut p = 5;
    if bits(0, 5)? == 31 {
        p += 6;
    }
    let fi = bits(p, 4)?;
    p += 4;
    let rate = if fi == 15 {
        let r = bits(p, 24)?;
        p += 24;
        r
    } else {
        *RATES.get(fi as usize)?
    };
    let ch = match bits(p, 4)? {
        7 => 8,
        c @ 1..=6 => u8::try_from(c).ok()?,
        _ => return None,
    };
    Some((rate, ch))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn vints() {
        let mut v = Vec::new();
        put_size(&mut v, 0);
        put_size(&mut v, 126);
        put_size(&mut v, 127);
        put_size(&mut v, 16382);
        assert_eq!(v, [0x80, 0xfe, 0x40, 0x7f, 0x7f, 0xfe]);
        assert_eq!(void(2), [0xec, 0x80]);
        assert_eq!(void(160).len(), 160);
        assert_eq!(void(128).len(), 128);
    }

    #[test]
    fn asc() {
        assert_eq!(asc_rate_channels(&[0x11, 0x90]), Some((48000, 2)));
        assert_eq!(asc_rate_channels(&[0x11, 0xb0]), Some((48000, 6)));
    }

    /// An element's (id, data start, data end) at `i`.
    fn element_at(out: &[u8], i: usize) -> (u32, usize, usize) {
        let id_len = out[i].leading_zeros() as usize + 1;
        let id = out[i..i + id_len]
            .iter()
            .fold(0u32, |a, b| a << 8 | u32::from(*b));
        let s = i + id_len;
        let lead = out[s].leading_zeros() as usize + 1;
        let mut size = usize::from(out[s]) & (0xffusize >> lead);
        for b in &out[s + 1..s + lead] {
            size = size << 8 | usize::from(*b);
        }
        (id, s + lead, s + lead + size)
    }

    /// The clusters' timestamps and each block's (track, absolute time)
    /// in file order, walking the Segment's top-level elements.
    fn blocks(out: &[u8]) -> (Vec<i64>, Vec<(u8, i64)>) {
        let (mut clusters, mut blocks) = (Vec::new(), Vec::new());
        let (_, _, ebml_end) = element_at(out, 0);
        let (seg, mut i, seg_end) = element_at(out, ebml_end);
        assert_eq!(seg, id::SEGMENT);
        assert_eq!(seg_end, out.len(), "the Segment size covers the file");
        while i < seg_end {
            let (eid, data, end) = element_at(out, i);
            if eid == id::CLUSTER {
                let (tid, td, te) = element_at(out, data);
                assert_eq!(tid, id::TIMESTAMP);
                let ts = out[td..te].iter().fold(0i64, |a, b| a << 8 | i64::from(*b));
                clusters.push(ts);
                let mut k = te;
                while k < end {
                    let (bid, bd, be) = element_at(out, k);
                    assert_eq!(bid, id::SIMPLE_BLOCK);
                    let rel = i16::from_be_bytes([out[bd + 1], out[bd + 2]]);
                    blocks.push((out[bd] & 0x7f, ts + i64::from(rel)));
                    k = be;
                }
            }
            i = end;
        }
        (clusters, blocks)
    }

    /// The top-level elements of the Segment: (id, data start, data end).
    fn top_level(out: &[u8]) -> Vec<(u32, usize, usize)> {
        let (_, _, ebml_end) = element_at(out, 0);
        let (_, mut i, seg_end) = element_at(out, ebml_end);
        let mut v = Vec::new();
        while i < seg_end {
            let e = element_at(out, i);
            v.push(e);
            i = e.2;
        }
        v
    }

    /// The direct children of the master element whose data is `from..to`.
    fn children(out: &[u8], from: usize, to: usize) -> Vec<(u32, usize, usize)> {
        let mut v = Vec::new();
        let mut i = from;
        while i < to {
            let e = element_at(out, i);
            v.push(e);
            i = e.2;
        }
        v
    }

    fn uint_at(out: &[u8], e: (u32, usize, usize)) -> u64 {
        out[e.1..e.2].iter().fold(0, |a, b| a << 8 | u64::from(*b))
    }

    #[test]
    fn chapters_follow_the_info_and_are_in_the_seek_head() {
        let mut w = Writer::new(Cursor::new(Vec::new()), Cursor::new(Vec::new()), "test");
        let a = w.add_track(Track {
            kind: Kind::Text,
            language: None,
            name: None,
            default: false,
        });
        w.frame(a, 0, 0, true, Some(10), b"x").unwrap();
        w.add_chapter(Chapter {
            start_ns: 0,
            end_ns: Some(153_000_000_000),
            title: Some("番組".into()),
            language: Some("jpn".into()),
        });
        w.add_chapter(Chapter {
            start_ns: 153_000_000_000,
            end_ns: None,
            title: None,
            language: None,
        });
        let (out, _) = w.finish().unwrap();
        let out = out.into_inner();
        let top = top_level(&out);
        let ids: Vec<u32> = top.iter().map(|e| e.0).collect();
        assert_eq!(
            ids[..5],
            [id::SEEK_HEAD, id::VOID, id::INFO, id::CHAPTERS, id::TRACKS]
        );
        // The SeekHead names the Chapters' position from the Segment's data.
        let (_, _, ebml_end) = element_at(&out, 0);
        let (_, segment, _) = element_at(&out, ebml_end);
        let at = top[3].1 - 1 - 4 - segment;
        let seeks = children(&out, top[0].1, top[0].2);
        let chapter_seek = seeks
            .iter()
            .map(|s| children(&out, s.1, s.2))
            .find(|c| out[c[0].1..c[0].2] == [0x10, 0x43, 0xa7, 0x70])
            .expect("a Seek of the Chapters");
        assert_eq!(usize::try_from(uint_at(&out, chapter_seek[1])).unwrap(), at);
        // One default edition, two atoms.
        let edition = children(&out, top[3].1, top[3].2);
        assert_eq!(edition.len(), 1);
        assert_eq!(edition[0].0, id::EDITION_ENTRY);
        let fields = children(&out, edition[0].1, edition[0].2);
        assert_eq!(
            fields.iter().map(|e| e.0).collect::<Vec<_>>(),
            [
                id::EDITION_UID,
                id::EDITION_FLAG_DEFAULT,
                id::CHAPTER_ATOM,
                id::CHAPTER_ATOM
            ]
        );
        assert_eq!(uint_at(&out, fields[1]), 1);
        let first = children(&out, fields[2].1, fields[2].2);
        assert_eq!(first[0].0, id::CHAPTER_UID);
        assert_eq!(uint_at(&out, first[0]), 1);
        assert_eq!(first[1].0, id::CHAPTER_TIME_START);
        assert_eq!(uint_at(&out, first[1]), 0);
        assert_eq!(first[2].0, id::CHAPTER_TIME_END);
        assert_eq!(uint_at(&out, first[2]), 153_000_000_000);
        assert_eq!(first[3].0, id::CHAPTER_DISPLAY);
        let display = children(&out, first[3].1, first[3].2);
        assert_eq!(&out[display[0].1..display[0].2], "番組".as_bytes());
        assert_eq!(&out[display[1].1..display[1].2], b"jpn");
        // No end, no title: only the mandatory fields.
        let second = children(&out, fields[3].1, fields[3].2);
        assert_eq!(
            second.iter().map(|e| e.0).collect::<Vec<_>>(),
            [id::CHAPTER_UID, id::CHAPTER_TIME_START]
        );
        assert_eq!(uint_at(&out, second[0]), 2);
    }

    #[test]
    fn no_chapters_no_element() {
        let w = Writer::new(Cursor::new(Vec::new()), Cursor::new(Vec::new()), "test");
        let (out, _) = w.finish().unwrap();
        let out = out.into_inner();
        assert!(top_level(&out).iter().all(|e| e.0 != id::CHAPTERS));
    }

    #[test]
    fn writes_decode_order_with_presentation_times() {
        let mut w = Writer::new(Cursor::new(Vec::new()), Cursor::new(Vec::new()), "test");
        let a = w.add_track(Track {
            kind: Kind::Aac {
                config: vec![0x11, 0x90],
                rate: 48000,
                channels: 2,
            },
            language: None,
            name: Some("stereo".into()),
            default: true,
        });
        // Audio fed first, video after: the file interleaves them.
        for i in 0..10 {
            w.frame(a, i * 100, i * 100, true, None, &[0xbb; 5])
                .unwrap();
        }
        // A track added after frames of another: allowed.
        let v = w.add_track(Track {
            kind: Kind::Hevc {
                hvcc: vec![1],
                width: 16,
                height: 16,
                frame_ns: None,
            },
            language: Some("jpn".into()),
            name: None,
            default: true,
        });
        // I P B (decode order; presentation 0, 200, 100) ×3.
        for g in 0..3 {
            let base = g * 300;
            w.frame(v, base, base, true, None, &[0xaa; 10]).unwrap();
            w.frame(v, base + 1, base + 200, false, None, &[0xaa; 10])
                .unwrap();
            w.frame(v, base + 2, base + 100, false, None, &[0xaa; 10])
                .unwrap();
        }
        w.attach("x.bin", "application/octet-stream", "", vec![1, 2, 3]);
        let (out, _) = w.finish().unwrap();
        let out = out.into_inner();
        assert_eq!(&out[..4], &[0x1a, 0x45, 0xdf, 0xa3]);
        let (clusters, blocks) = blocks(&out);
        assert_eq!(clusters, vec![0, 300, 600], "a cluster per video keyframe");
        let video: Vec<i64> = blocks.iter().filter(|b| b.0 == 2).map(|b| b.1).collect();
        assert_eq!(
            video,
            vec![0, 200, 100, 300, 500, 400, 600, 800, 700],
            "decode order, PTS stamps"
        );
        let audio: Vec<i64> = blocks.iter().filter(|b| b.0 == 1).map(|b| b.1).collect();
        assert_eq!(audio, (0..10).map(|i| i * 100).collect::<Vec<_>>());
        // The SeekHead replaced the Void at the segment start.
        let seg = out
            .windows(4)
            .position(|w| w == [0x18, 0x53, 0x80, 0x67])
            .unwrap();
        assert_eq!(&out[seg + 12..seg + 16], &[0x11, 0x4d, 0x9b, 0x74]);
    }
}
