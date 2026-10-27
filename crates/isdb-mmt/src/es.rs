//! Elementary streams from MFUs, one per asset type (ARIB STD-B60 v1.14
//! §8, §9):
//!
//! - **HEVC** (`hev1`, §8.1.1): each MFU is a NAL unit preceded by its
//!   size as a 32-bit unsigned integer ("by eliminating byte start code,
//!   the size of NAL unit … represented by 32 bits … is added to
//!   immediately before NAL unit"); VPS, SPS and PPS are MFUs of their
//!   own. The output is an Annex B byte stream: each NAL unit after a
//!   4-byte start code `00 00 00 01`. The start code itself and the NAL
//!   unit header layout (type = bits 1–6 of the first byte) are from
//!   ITU-T H.265 Annex B / §7.3.1.2, not local (SPEC-NOTES
//!   "hevc-annex-b", from knowledge).
//! - **MPEG-4 audio** (`mp4a`, §8.2.1): each MFU is an AudioMuxElement()
//!   with "synchronization byte and length information removed from
//!   AudioSyncStream()"; the receiver adds them back. The output is a LOAS
//!   stream: 11-bit syncword `0x2B7`, 13-bit audioMuxLengthBytes, then the
//!   element (ISO/IEC 14496-3 §1.7.2, not local: SPEC-NOTES "loas", from
//!   knowledge).
//! - **Captions** (`stpp`, §9.2.2): each MFU has the header of Table 9-1;
//!   subsamples of data type 0 (TTML documents) are written as text, the
//!   others (PNG, SVG, audio, fonts) as their own files by the caller.
//! - anything else: the MFU bytes back to back.

/// Annex B start code (H.265 Annex B, from knowledge).
pub const START_CODE: [u8; 4] = [0, 0, 0, 1];

/// The NAL units of a length-prefixed MFU (§8.1.1): normally one; more
/// than one are accepted. `None` when a length does not fit.
pub fn nal_units(mfu: &[u8]) -> Option<Vec<&[u8]>> {
    let mut out = Vec::new();
    let mut p = 0;
    while p < mfu.len() {
        let n = u32::from_be_bytes(mfu.get(p..p + 4)?.try_into().ok()?) as usize;
        out.push(mfu.get(p + 4..p + 4 + n)?);
        p += 4 + n;
    }
    Some(out)
}

/// The HEVC NAL unit type (H.265 §7.3.1.2: forbidden bit, then 6 bits of
/// type; from knowledge).
pub fn hevc_nal_type(nal: &[u8]) -> Option<u8> {
    nal.first().map(|b| (b >> 1) & 0x3F)
}

/// The HEVC NAL unit types this crate names (H.265 Table 7-1, from
/// knowledge): VPS 32, SPS 33, PPS 34, AUD 35, SEI 39/40, IRAP 16–23.
pub fn hevc_nal_name(t: u8) -> &'static str {
    match t {
        0..=9 => "slice (non-IRAP)",
        16..=21 => "slice (IRAP)",
        32 => "VPS",
        33 => "SPS",
        34 => "PPS",
        35 => "AUD",
        36 => "EOS",
        37 => "EOB",
        38 => "FD",
        39 | 40 => "SEI",
        _ => "other",
    }
}

/// Append one length-prefixed HEVC MFU to an Annex B stream. Returns the
/// NAL unit types written, or `None` (nothing written) when the lengths do
/// not fit.
pub fn hevc_annex_b(mfu: &[u8], out: &mut Vec<u8>) -> Option<Vec<u8>> {
    let nals = nal_units(mfu)?;
    let mut types = Vec::with_capacity(nals.len());
    for n in nals {
        out.extend_from_slice(&START_CODE);
        out.extend_from_slice(n);
        types.extend(hevc_nal_type(n));
    }
    Some(types)
}

/// Largest AudioMuxElement a LOAS frame can carry (13-bit length).
pub const MAX_LOAS: usize = 0x1FFF;

/// Append one AudioMuxElement as a LOAS AudioSyncStream frame
/// (syncword 0x2B7, 13-bit length; ISO/IEC 14496-3, from knowledge).
/// `false` (nothing written) when it is longer than 8191 bytes.
pub fn loas_frame(element: &[u8], out: &mut Vec<u8>) -> bool {
    let Ok(n) = u16::try_from(element.len()) else {
        return false;
    };
    if usize::from(n) > MAX_LOAS {
        return false;
    }
    let [hi, lo] = n.to_be_bytes();
    out.push(0x56);
    out.push(0xE0 | hi);
    out.push(lo);
    out.extend_from_slice(element);
    true
}

/// A caption / superimposition MFU (Table 9-1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Subtitle<'a> {
    /// `subtitle_tag`.
    pub tag: u8,
    /// `subtitle_sequence_number`.
    pub sequence_number: u8,
    /// `subsample_number`.
    pub subsample_number: u8,
    /// `last_subsample_number`.
    pub last_subsample_number: u8,
    /// `data_type` (Table 9-2: 0 TTML, 1 PNG, 2 SVG, 3 AIFF-C, 4 MP3,
    /// 5 AAC, 6 SVG font, 7 WOFF).
    pub data_type: u8,
    /// The data bytes.
    pub data: &'a [u8],
}

impl<'a> Subtitle<'a> {
    /// Parse a caption MFU.
    pub fn parse(b: &'a [u8]) -> Option<Subtitle<'a>> {
        let f = *b.get(4)?;
        let data_type = f >> 4;
        let long_len = f & 0x08 != 0;
        let info_list = f & 0x04 != 0;
        let (size, mut p) = if long_len {
            (
                u32::from_be_bytes(b.get(5..9)?.try_into().ok()?) as usize,
                9,
            )
        } else {
            (usize::from(u16::from_be_bytes([*b.get(5)?, *b.get(6)?])), 7)
        };
        let subsample_number = *b.get(2)?;
        let last = *b.get(3)?;
        if subsample_number == 0 && last > 0 && info_list {
            p += usize::from(last) * (1 + if long_len { 4 } else { 2 });
        }
        Some(Subtitle {
            tag: b[0],
            sequence_number: b[1],
            subsample_number,
            last_subsample_number: last,
            data_type,
            data: b.get(p..p + size)?,
        })
    }

    /// A file extension for the data type (Table 9-2).
    pub fn extension(&self) -> &'static str {
        match self.data_type {
            0 => "ttml",
            1 => "png",
            2 | 6 => "svg",
            3 => "aifc",
            4 => "mp3",
            5 => "aac",
            7 => "woff",
            _ => "bin",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn annex_b_from_length_prefixed_nals() {
        // a VPS (type 32: first byte 0x40) and a PPS (34: 0x44)
        let mut mfu = vec![0, 0, 0, 3, 0x40, 0x01, 0x0C];
        mfu.extend([0, 0, 0, 2, 0x44, 0x01]);
        let mut out = Vec::new();
        assert_eq!(hevc_annex_b(&mfu, &mut out), Some(vec![32, 34]));
        assert_eq!(out, [0, 0, 0, 1, 0x40, 0x01, 0x0C, 0, 0, 0, 1, 0x44, 0x01]);
        // a length beyond the MFU writes nothing
        let mut out = Vec::new();
        assert_eq!(hevc_annex_b(&[0, 0, 0, 9, 1], &mut out), None);
        assert!(out.is_empty());
    }

    #[test]
    fn loas_header() {
        let mut out = Vec::new();
        assert!(loas_frame(&[0xAB; 0x123], &mut out));
        assert_eq!(&out[..3], &[0x56, 0xE1, 0x23]);
        // syncword 0x2B7 in the first 11 bits
        assert_eq!((u16::from_be_bytes([out[0], out[1]]) >> 5), 0x2B7);
    }

    #[test]
    fn subtitle_mfu() {
        // tag 0x30, seq 4, subsample 0 of 0, TTML, 16-bit size 5
        let b = [0x30, 4, 0, 0, 0x00, 0, 5, b'<', b't', b't', b'/', b'>'];
        let s = Subtitle::parse(&b).unwrap();
        assert_eq!((s.tag, s.data_type, s.extension()), (0x30, 0, "ttml"));
        assert_eq!(s.data, b"<tt/>");
    }
}
