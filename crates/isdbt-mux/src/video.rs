//! MPEG-2 video (ITU-T H.262 | ISO/IEC 13818-2) elementary stream parsing:
//! access-unit segmentation and PTS/DTS derivation.
//!
//! H.262 is not among the local specifications; the syntax used here is
//! cited from ITU-T H.262 (07/2000) from knowledge, and listed in
//! SPEC-NOTES.md:
//! * start codes (§6.2.1, Table 6-1): picture 0x00, slices 0x01–0xAF,
//!   user data 0xB2, sequence header 0xB3, extension 0xB5, sequence end
//!   0xB7, group of pictures 0xB8;
//! * sequence_header (§6.2.2.1), sequence_extension (§6.2.2.3),
//!   group_of_pictures_header (§6.2.2.6), picture_header (§6.2.3),
//!   picture_coding_extension (§6.2.3.1);
//! * display duration from progressive_sequence, picture_structure,
//!   repeat_first_field and top_field_first (§6.3.10, Table 6-18 style).
//!
//! What an access unit is comes from H.222.0 §2.1.1 ("access unit: a coded
//! representation of a presentation unit … In the case of video, an access
//! unit includes all the coded data for a picture, and any stuffing that
//! follows it, up to but not including the start of the next access
//! unit … if a picture is preceded by a group_start_code and/or a
//! sequence_header_code, the access unit begins with the first byte of the
//! first of these start codes … The sequence_end_code is part of the last
//! access unit"), and B32 Part 1 Attachment 2 §5.1 / §5.1.1 require one
//! frame or field pair per PES packet.

use isdb_si::values::{VideoComponentType, VideoEncodeFormat};

use crate::error::{MuxError, Result};

/// One access unit (coded frame or field pair with its headers).
#[derive(Debug, Clone)]
pub struct VideoAu {
    /// Byte offset in the ES.
    pub offset: usize,
    /// Length in bytes.
    pub len: usize,
    /// picture_coding_type of the first picture: 1 I, 2 P, 3 B.
    pub coding_type: u8,
    /// temporal_reference of the first picture.
    pub temporal_reference: u16,
    /// Contains a sequence_header.
    pub has_sequence_header: bool,
    /// Offset (within the AU) of the GOP header's 4th payload byte (closed_gop/broken_link), if any.
    pub gop_flags_at: Option<usize>,
    /// closed_gop of the GOP header in this AU.
    pub closed_gop: bool,
    /// Offsets (within the AU) of the first byte after each picture start code.
    pub picture_headers: Vec<usize>,
    /// Displayed duration in fields.
    pub fields: u32,
    /// Ends with sequence_end_code.
    pub has_sequence_end: bool,
}

/// Sequence-level parameters (first sequence header/extension).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SequenceInfo {
    /// horizontal_size (with extension bits).
    pub width: u32,
    /// vertical_size (with extension bits).
    pub height: u32,
    /// aspect_ratio_information.
    pub aspect_ratio: u8,
    /// frame_rate_code.
    pub frame_rate_code: u8,
    /// Frame rate numerator/denominator (with extension).
    pub frame_rate: (u64, u64),
    /// bit_rate in bit/s (400 bit/s units × value).
    pub bit_rate: u64,
    /// vbv_buffer_size in bits (16 384-bit units).
    pub vbv_buffer_bits: u64,
    /// profile_and_level_indication (0 if no sequence_extension: MPEG-1).
    pub profile_and_level: u8,
    /// progressive_sequence.
    pub progressive_sequence: bool,
    /// Whether a sequence_extension was present.
    pub mpeg2: bool,
}

impl SequenceInfo {
    /// Frame period in 1/54 MHz units (exact for every frame_rate_code).
    pub fn frame_period_54mhz(&self) -> u64 {
        let (n, d) = self.frame_rate;
        54_000_000 * d / n
    }

    /// Rmax[profile, level] of H.262 Table 8-13 (Main profile values; from
    /// knowledge, see SPEC-NOTES), bit/s.
    pub fn rmax(&self) -> u64 {
        if !self.mpeg2 {
            return 1_856_000; // ISO/IEC 11172-2 constrained parameters
        }
        match self.profile_and_level & 0x0F {
            4 => 80_000_000, // High
            6 => 60_000_000, // High-1440
            8 => 15_000_000, // Main
            10 => 4_000_000, // Low
            _ => 80_000_000,
        }
    }

    /// Whether the level is High or High-1440 (H.222.0 §2.4.2.3 MBS rule).
    pub fn high_level(&self) -> bool {
        self.mpeg2 && matches!(self.profile_and_level & 0x0F, 4 | 6)
    }

    /// video_encode_format of the video decode control descriptor (B10
    /// §6.2.30 Table 6-60) derived from vertical_size and
    /// progressive_sequence (sequence_header / sequence_extension):
    /// 1080 (or 1088 coded) lines → 1080/P or 1080/I, 720 progressive →
    /// 720/P, 480 → 480/P or 480/I, 240/120/180 progressive → 240/P, 120/P,
    /// 180/p.  `None` for any other format (no Table 6-60 entry).
    pub fn video_encode_format(&self) -> Option<VideoEncodeFormat> {
        use VideoEncodeFormat::*;
        Some(match (self.height, self.progressive_sequence) {
            (1080 | 1088, true) => P1080,
            (1080 | 1088, false) => I1080,
            (720, true) => P720,
            (480, true) => P480,
            (480, false) => I480,
            (240, true) => P240,
            (120, true) => P120,
            (180, true) => P180,
            _ => return None,
        })
    }

    /// component_type for stream_content 0x1 (B10 §6.2.3 Table 6-5) derived
    /// from the scan format (vertical_size, progressive_sequence) and
    /// aspect_ratio_information (H.262 Table 6-3, from knowledge: 2 = 4:3,
    /// 3 = 16:9, 4 = 2.21:1):
    /// * 4:3 → the "4:3 aspect ratio" row (0x?1);
    /// * 16:9 → "16:9 aspect ratio, without pan vectors" (0x?3) — the ES
    ///   carries no pan-scan signalling we inspect, so "with pan vectors"
    ///   is never derived (operational choice, SPEC-NOTES);
    /// * 2.21:1 → ">16:9 aspect ratio" (0x?4).
    ///
    /// `None` for formats without a Table 6-5 row or other aspect codes.
    pub fn component_type(&self) -> Option<VideoComponentType> {
        use VideoComponentType::*;
        #[derive(Clone, Copy)]
        enum A {
            R4x3,
            R16x9,
            Wide,
        }
        let a = match self.aspect_ratio {
            2 => A::R4x3,
            3 => A::R16x9,
            4 => A::Wide,
            _ => return None,
        };
        let pick = |x: [VideoComponentType; 3]| match a {
            A::R4x3 => x[0],
            A::R16x9 => x[1],
            A::Wide => x[2],
        };
        Some(match (self.height, self.progressive_sequence) {
            (480, false) => pick([V480i4x3, V480i16x9, V480iWide]),
            (480, true) => pick([V480p4x3, V480p16x9, V480pWide]),
            (1080 | 1088, false) => pick([V1080i4x3, V1080i16x9, V1080iWide]),
            (720, true) => pick([V720p4x3, V720p16x9, V720pWide]),
            (240, true) => pick([V240p4x3, V240p16x9, V240pWide]),
            (1080 | 1088, true) => pick([V1080p4x3, V1080p16x9, V1080pWide]),
            (180, true) => pick([V180p4x3, V180p16x9, V180pWide]),
            _ => return None,
        })
    }
}

/// A parsed video ES with per-AU timing.
#[derive(Debug, Clone)]
pub struct VideoEs {
    /// The ES bytes.
    pub data: Vec<u8>,
    /// Access units in decode order.
    pub aus: Vec<VideoAu>,
    /// Sequence parameters.
    pub seq: SequenceInfo,
    /// PTS of each AU relative to the loop start, 1/54 MHz units.
    pub pts54: Vec<u64>,
    /// DTS of each AU relative to the loop start, 1/54 MHz units.
    pub dts54: Vec<u64>,
    /// Total display duration of one pass, 1/54 MHz units.
    pub duration54: u64,
    /// Reorder delay added to every PTS, 1/54 MHz units (one frame period).
    pub reorder54: u64,
    /// The file contains a sequence_end_code.
    pub has_sequence_end: bool,
}

fn start_codes(data: &[u8]) -> Vec<(usize, u8)> {
    let mut v = Vec::new();
    let mut i = 0;
    while i + 3 < data.len() {
        if data[i + 2] > 1 {
            i += 3;
        } else if data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 1 {
            v.push((i, data[i + 3]));
            i += 4;
        } else {
            i += 1;
        }
    }
    v
}

struct Bits<'a> {
    b: &'a [u8],
    pos: usize,
}

impl Bits<'_> {
    fn read(&mut self, n: usize) -> Option<u32> {
        let mut v = 0u32;
        for _ in 0..n {
            let byte = *self.b.get(self.pos / 8)?;
            v = (v << 1) | ((byte >> (7 - self.pos % 8)) & 1) as u32;
            self.pos += 1;
        }
        Some(v)
    }
}

/// closed_gop in byte 3 of the group_of_pictures_header after its start
/// code (H.262 §6.2.2.6: time_code 25 bits, closed_gop 1, broken_link 1).
pub const GOP_CLOSED: u8 = 0x40;
/// broken_link in the same byte.
pub const GOP_BROKEN_LINK: u8 = 0x20;

const FRAME_RATES: [(u64, u64); 9] = [
    (0, 1),
    (24_000, 1001),
    (24, 1),
    (25, 1),
    (30_000, 1001),
    (30, 1),
    (50, 1),
    (60_000, 1001),
    (60, 1),
];

impl VideoEs {
    /// Parse a complete ES.
    pub fn parse(data: Vec<u8>, path: &str) -> Result<Self> {
        let err = |msg: String| MuxError::Media {
            path: path.to_string(),
            msg,
        };
        let codes = start_codes(&data);
        let mut seq: Option<SequenceInfo> = None;
        let mut aus: Vec<VideoAu> = Vec::new();
        // State of the AU being built.
        let mut cur: Option<VideoAu> = None;
        let mut cur_has_slices = false;
        let mut pending_field: bool = false; // current AU holds a first field awaiting its pair
        let mut last_ext_target = 0u8; // 1 after sequence header, 2 after picture header
        let mut has_seq_end = false;

        let finish = |cur: &mut Option<VideoAu>, end: usize, aus: &mut Vec<VideoAu>| {
            if let Some(mut a) = cur.take() {
                a.len = end - a.offset;
                aus.push(a);
            }
        };

        for &(pos, code) in &codes {
            let payload = &data[pos + 4..];
            let starts_au = match code {
                0xB3 | 0xB8 => cur_has_slices,
                0x00 => cur_has_slices && !pending_field,
                _ => false,
            };
            if starts_au || (cur.is_none() && matches!(code, 0xB3 | 0xB8 | 0x00)) {
                finish(&mut cur, pos, &mut aus);
                cur = Some(VideoAu {
                    offset: if aus.is_empty() { 0 } else { pos },
                    len: 0,
                    coding_type: 0,
                    temporal_reference: 0,
                    has_sequence_header: false,
                    gop_flags_at: None,
                    closed_gop: false,
                    picture_headers: Vec::new(),
                    fields: 0,
                    has_sequence_end: false,
                });
                cur_has_slices = false;
                pending_field = false;
            }
            let Some(au) = cur.as_mut() else { continue };
            match code {
                0xB3 => {
                    au.has_sequence_header = true;
                    last_ext_target = 1;
                    let mut b = Bits { b: payload, pos: 0 };
                    let rd = |b: &mut Bits<'_>, n| {
                        b.read(n)
                            .ok_or_else(|| err("truncated sequence_header".into()))
                    };
                    let w = rd(&mut b, 12)?;
                    let h = rd(&mut b, 12)?;
                    let ar = rd(&mut b, 4)? as u8;
                    let frc = rd(&mut b, 4)? as u8;
                    let br = rd(&mut b, 18)?;
                    rd(&mut b, 1)?;
                    let vbv = rd(&mut b, 10)?;
                    if seq.is_none() {
                        if frc == 0 || frc > 8 {
                            return Err(err(format!("frame_rate_code {frc} not supported")));
                        }
                        seq = Some(SequenceInfo {
                            width: w,
                            height: h,
                            aspect_ratio: ar,
                            frame_rate_code: frc,
                            frame_rate: FRAME_RATES[frc as usize],
                            bit_rate: br as u64 * 400,
                            vbv_buffer_bits: vbv as u64 * 16_384,
                            profile_and_level: 0,
                            progressive_sequence: false,
                            mpeg2: false,
                        });
                    }
                }
                0xB5 => {
                    let mut b = Bits { b: payload, pos: 0 };
                    let id = b.read(4).unwrap_or(0);
                    if id == 1 && last_ext_target == 1 {
                        // sequence_extension
                        let rd = |b: &mut Bits<'_>, n| {
                            b.read(n)
                                .ok_or_else(|| err("truncated sequence_extension".into()))
                        };
                        let pl = rd(&mut b, 8)? as u8;
                        let prog = rd(&mut b, 1)? == 1;
                        let _chroma = rd(&mut b, 2)?;
                        let hx = rd(&mut b, 2)?;
                        let vx = rd(&mut b, 2)?;
                        let brx = rd(&mut b, 12)?;
                        rd(&mut b, 1)?;
                        let vbvx = rd(&mut b, 8)?;
                        let _low_delay = rd(&mut b, 1)?;
                        let frn = rd(&mut b, 2)? as u64;
                        let frd = rd(&mut b, 5)? as u64;
                        if let Some(s) = seq.as_mut()
                            && !s.mpeg2
                        {
                            s.mpeg2 = true;
                            s.profile_and_level = pl;
                            s.progressive_sequence = prog;
                            s.width |= hx << 12;
                            s.height |= vx << 12;
                            s.bit_rate = (((brx as u64) << 18) | (s.bit_rate / 400)) * 400;
                            s.vbv_buffer_bits =
                                (((vbvx as u64) << 10) | (s.vbv_buffer_bits / 16_384)) * 16_384;
                            s.frame_rate = (s.frame_rate.0 * (frn + 1), s.frame_rate.1 * (frd + 1));
                        }
                    } else if id == 8 && last_ext_target == 2 {
                        // picture_coding_extension
                        let rd = |b: &mut Bits<'_>, n| {
                            b.read(n)
                                .ok_or_else(|| err("truncated picture_coding_extension".into()))
                        };
                        rd(&mut b, 16)?; // f_codes
                        rd(&mut b, 2)?; // intra_dc_precision
                        let structure = rd(&mut b, 2)?;
                        let tff = rd(&mut b, 1)? == 1;
                        rd(&mut b, 5)?; // frame_pred_frame_dct .. alternate_scan
                        let rff = rd(&mut b, 1)? == 1;
                        rd(&mut b, 1)?; // chroma_420_type
                        let _progressive_frame = rd(&mut b, 1)? == 1;
                        let prog_seq = seq.map(|s| s.progressive_sequence).unwrap_or(false);
                        if structure == 3 {
                            au.fields = if prog_seq {
                                2 * (1 + u32::from(rff) * (1 + u32::from(tff)))
                            } else {
                                2 + u32::from(rff)
                            };
                            pending_field = false;
                        } else if au.picture_headers.len() == 1 {
                            // first field of a pair
                            au.fields = 2;
                            pending_field = true;
                        } else {
                            pending_field = false;
                        }
                    }
                }
                0xB8 => {
                    last_ext_target = 0;
                    // group_of_pictures_header: time_code (25 bits), then
                    // closed_gop and broken_link: payload byte 3 bits 6 / 5
                    // (byte 3 bit 7 is the last time_code bit).
                    if payload.len() >= 4 {
                        au.gop_flags_at = Some(pos + 4 + 3 - au.offset);
                        au.closed_gop = payload[3] & GOP_CLOSED != 0;
                    }
                }
                0x00 => {
                    last_ext_target = 2;
                    let mut b = Bits { b: payload, pos: 0 };
                    let tr = b
                        .read(10)
                        .ok_or_else(|| err("truncated picture_header".into()))?
                        as u16;
                    let pct = b
                        .read(3)
                        .ok_or_else(|| err("truncated picture_header".into()))?
                        as u8;
                    if au.picture_headers.is_empty() {
                        au.temporal_reference = tr;
                        au.coding_type = pct;
                        au.fields = 2; // MPEG-1 / no extension default
                    }
                    au.picture_headers.push(pos + 4 - au.offset);
                }
                0x01..=0xAF => cur_has_slices = true,
                0xB7 => {
                    au.has_sequence_end = true;
                    has_seq_end = true;
                    let end = pos + 4;
                    // sequence_end_code is part of the last access unit
                    finish(&mut cur, end, &mut aus);
                    cur_has_slices = false;
                    pending_field = false;
                }
                _ => {}
            }
        }
        finish(&mut cur, data.len(), &mut aus);
        let seq = seq.ok_or_else(|| err("no sequence_header found".into()))?;
        if aus.is_empty() {
            return Err(err("no pictures".into()));
        }
        if aus.iter().any(|a| a.coding_type == 0 || a.coding_type > 3) {
            return Err(err("picture with invalid picture_coding_type".into()));
        }
        if !aus[0].has_sequence_header {
            return Err(err("stream does not start with a sequence_header".into()));
        }
        let mut es = VideoEs {
            data,
            aus,
            seq,
            pts54: Vec::new(),
            dts54: Vec::new(),
            duration54: 0,
            reorder54: 0,
            has_sequence_end: has_seq_end,
        };
        es.compute_timestamps();
        Ok(es)
    }

    /// PTS/DTS derivation (see module docs and SPEC-NOTES):
    /// display order within a GOP from temporal_reference; PTS =
    /// display start + one frame period of reorder delay; DTS = PTS for
    /// B-pictures, and for I/P-pictures the PTS of the previous I/P-picture
    /// in decode order (the first one gets DTS 0).
    fn compute_timestamps(&mut self) {
        let field = self.seq.frame_period_54mhz() / 2;
        let reorder = 2 * field;
        let n = self.aus.len();
        let mut pts = vec![0u64; n];
        // Group boundaries: AU with a GOP header or sequence header.
        let mut groups: Vec<(usize, usize)> = Vec::new();
        let mut s = 0;
        for i in 1..=n {
            if i == n || self.aus[i].gop_flags_at.is_some() || self.aus[i].has_sequence_header {
                groups.push((s, i));
                s = i;
            }
        }
        let mut base = 0u64;
        for (a, b) in groups {
            let mut order: Vec<usize> = (a..b).collect();
            order.sort_by_key(|&i| (self.aus[i].temporal_reference, i));
            let mut t = base;
            for &i in &order {
                pts[i] = t + reorder;
                t += self.aus[i].fields as u64 * field;
            }
            base = t;
        }
        let mut dts = vec![0u64; n];
        let mut prev_anchor_pts: Option<u64> = None;
        for i in 0..n {
            if self.aus[i].coding_type == 3 {
                dts[i] = pts[i];
            } else {
                dts[i] = prev_anchor_pts.unwrap_or(0);
                prev_anchor_pts = Some(pts[i]);
            }
        }
        self.pts54 = pts;
        self.dts54 = dts;
        self.duration54 = base;
        self.reorder54 = reorder;
    }

    /// AU bytes with the configured rewrites applied.
    pub fn au_bytes(&self, i: usize, force_vbv_ffff: bool, set_broken_link: bool) -> Vec<u8> {
        let a = &self.aus[i];
        let mut v = self.data[a.offset..a.offset + a.len].to_vec();
        if force_vbv_ffff {
            for &h in &a.picture_headers {
                // picture_header: temporal_reference(10) picture_coding_type(3) vbv_delay(16)
                if h + 4 <= v.len() {
                    let mut w = u32::from_be_bytes([v[h], v[h + 1], v[h + 2], v[h + 3]]);
                    w |= 0xFFFF << 3;
                    v[h..h + 4].copy_from_slice(&w.to_be_bytes());
                }
            }
        }
        if set_broken_link
            && let Some(g) = a.gop_flags_at
            && !a.closed_gop
            && g < v.len()
        {
            v[g] |= GOP_BROKEN_LINK;
        }
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a tiny synthetic ES: sequence header + extension, GOP, and
    /// pictures with given (tr, type).
    pub(crate) fn synth(pics: &[(u16, u8)], gop_every: usize) -> Vec<u8> {
        let mut v = Vec::new();
        for (k, &(tr, ty)) in pics.iter().enumerate() {
            if k % gop_every == 0 {
                // sequence_header 1920x1080, aspect 3, frc 4, bitrate 20 Mbps, vbv 488
                v.extend_from_slice(&[0, 0, 1, 0xB3]);
                let w: u32 = 1920;
                let h: u32 = 1080;
                v.extend_from_slice(&[
                    (w >> 4) as u8,
                    ((w & 0xF) << 4 | h >> 8) as u8,
                    h as u8,
                    0x34,
                ]);
                let br = 50_000u32; // ×400 = 20 Mbps
                let vbv = 488u32;
                let x: u32 = br << 14 | 1 << 13 | vbv << 3; // bit_rate(18) marker vbv(10) cpf(1) + 2 load flags
                v.extend_from_slice(&x.to_be_bytes());
                // sequence_extension: id 1, profile/level 0x44, progressive 0, chroma 01
                v.extend_from_slice(&[0, 0, 1, 0xB5, 0x14, 0x42, 0x00, 0x01, 0x00, 0x00]);
                // GOP header (marker bit set), closed_gop=1
                v.extend_from_slice(&[0, 0, 1, 0xB8, 0x00, 0x08, 0x00, 0x40]);
            }
            v.extend_from_slice(&[0, 0, 1, 0x00]);
            let w: u32 = (tr as u32) << 22 | (ty as u32) << 19 | 0x1234 << 3;
            v.extend_from_slice(&w.to_be_bytes());
            // picture_coding_extension: frame picture, tff=1, rff=0
            v.extend_from_slice(&[0, 0, 1, 0xB5, 0x8F, 0xFF, 0xF3, 0x80, 0x80]);
            // one slice with some bytes
            v.extend_from_slice(&[0, 0, 1, 0x01, 0x12, 0x34, 0x56, 0x78, 0x9A]);
        }
        v
    }

    #[test]
    fn ibbp_timestamps() {
        // coding order I2 B0 B1 P5 B3 B4, closed GOP
        let es = VideoEs::parse(
            synth(&[(2, 1), (0, 3), (1, 3), (5, 2), (3, 3), (4, 3)], 100),
            "t",
        )
        .unwrap();
        assert_eq!(es.aus.len(), 6);
        assert_eq!(es.seq.width, 1920);
        assert_eq!(es.seq.height, 1080);
        assert_eq!(es.seq.bit_rate, 20_000_000);
        let f = es.seq.frame_period_54mhz();
        assert_eq!(f, 1_801_800);
        let p: Vec<u64> = es.pts54.iter().map(|x| x / f).collect();
        let d: Vec<u64> = es.dts54.iter().map(|x| x / f).collect();
        assert_eq!(p, vec![3, 1, 2, 6, 4, 5]);
        assert_eq!(d, vec![0, 1, 2, 3, 4, 5]);
        for i in 0..6 {
            assert!(es.dts54[i] <= es.pts54[i]);
            if i > 0 {
                assert!(es.dts54[i] > es.dts54[i - 1]);
            }
        }
        assert_eq!(es.duration54, 6 * f);
        // AU 0 includes the sequence header
        assert!(es.aus[0].has_sequence_header && es.aus[0].offset == 0);
        let total: usize = es.aus.iter().map(|a| a.len).sum();
        assert_eq!(total, es.data.len());
    }

    #[test]
    fn gop_flags_follow_the_25_bit_time_code() {
        // closed GOP: no rewrite; open GOP: broken_link (0x20) set, the
        // time_code's last bit (0x80) untouched.
        let mut es = VideoEs::parse(synth(&[(0, 1), (1, 2)], 100), "t").unwrap();
        assert!(es.aus[0].closed_gop);
        let g = es.aus[0].gop_flags_at.unwrap();
        assert_eq!(es.au_bytes(0, false, true)[g], 0x40);
        es.data[g] = 0x80; // open GOP, time_code pictures LSB = 1
        let es = VideoEs::parse(es.data.clone(), "t").unwrap();
        assert!(!es.aus[0].closed_gop);
        assert_eq!(es.au_bytes(0, false, true)[g], 0x80 | GOP_BROKEN_LINK);
    }

    #[test]
    fn vbv_rewrite() {
        let es = VideoEs::parse(synth(&[(0, 1), (1, 2)], 100), "t").unwrap();
        let b = es.au_bytes(1, true, false);
        let h = es.aus[1].picture_headers[0];
        let w = u32::from_be_bytes([b[h], b[h + 1], b[h + 2], b[h + 3]]);
        assert_eq!((w >> 3) & 0xFFFF, 0xFFFF);
        assert_eq!(w >> 22, 1);
        assert_eq!((w >> 19) & 7, 2);
    }
}
