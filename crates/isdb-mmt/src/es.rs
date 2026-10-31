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

/// One AudioMuxElement (ISO/IEC 14496-3 §1.7.3, `muxConfigPresent` = 1,
/// the form of an MMT audio MFU) taken apart: its `AudioSpecificConfig`
/// (when the element carries a `StreamMuxConfig`) and the raw AAC frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LatmFrame<'a> {
    /// The `AudioSpecificConfig` bytes, realigned to a byte boundary
    /// (`None` when `useSameStreamMux` reuses the previous one).
    pub config: Option<Vec<u8>>,
    /// The `payload` of the one subframe: a raw AAC frame (what Matroska's
    /// `A_AAC` and MP4 store per sample).
    pub payload: std::borrow::Cow<'a, [u8]>,
}

/// Take one AudioMuxElement apart. The profile on air (BS4K/8K: version
/// 0, all streams on the same time framing, one subframe, one program,
/// one layer, frame length type 0 — a variable-length AAC frame) is the
/// one read; anything else is `None`.
pub fn latm_frame(element: &[u8]) -> Option<LatmFrame<'_>> {
    let mut r = Bits::new(element);
    let config = if r.bit()? {
        None
    } else {
        // StreamMuxConfig (Table 1.42).
        if r.bit()? {
            return None; // audioMuxVersion 1
        }
        let all_same = r.bit()?;
        let sub_frames = r.read(6)?;
        let programs = r.read(4)?;
        let layers = r.read(3)?;
        if !all_same || sub_frames != 0 || programs != 0 || layers != 0 {
            return None;
        }
        let start = r.pos;
        asc_length(&mut r)?;
        let config = r.bytes_from(start)?;
        if r.read(3)? != 0 {
            return None; // frameLengthType
        }
        r.read(8)?; // latmBufferFullness
        if r.bit()? {
            // otherDataPresent: otherDataLenBits (escaped).
            loop {
                let more = r.bit()?;
                r.read(8)?;
                if !more {
                    break;
                }
            }
        }
        if r.bit()? {
            r.read(8)?; // crcCheckSum
        }
        Some(config)
    };
    // PayloadLengthInfo: 8-bit chunks, 255 continuing.
    let mut len = 0usize;
    loop {
        let b = r.read(8)? as usize;
        len += b;
        if b != 255 {
            break;
        }
    }
    let payload = r.take(len)?;
    Some(LatmFrame { config, payload })
}

/// Walk an `AudioSpecificConfig` (§1.6.2.1) as far as an AAC object
/// carries it: object type, frequency, channels, `GASpecificConfig`
/// (§4.4.1), and an SBR/PS explicit extension's object type and
/// frequency.
fn asc_length(r: &mut Bits<'_>) -> Option<()> {
    let mut object = r.read(5)?;
    if object == 31 {
        object = 32 + r.read(6)?;
    }
    if r.read(4)? == 0xf {
        r.read(24)?;
    }
    let channels = r.read(4)?;
    if matches!(object, 5 | 29) {
        // Explicit SBR / PS: the extension frequency and the core object.
        if r.read(4)? == 0xf {
            r.read(24)?;
        }
        object = r.read(5)?;
    }
    if !matches!(object, 1 | 2 | 3 | 4 | 6 | 7) {
        return None;
    }
    r.bit()?; // frameLengthFlag
    if r.bit()? {
        r.read(14)?; // coreCoderDelay
    }
    let extension = r.bit()?;
    if channels == 0 {
        return None; // a program_config_element: not on air
    }
    if matches!(object, 6 | 20) {
        r.read(3)?;
    }
    if extension {
        return None; // the ER objects' extension fields
    }
    Some(())
}

/// A big-endian bit reader.
struct Bits<'a> {
    b: &'a [u8],
    pos: usize,
}

impl<'a> Bits<'a> {
    fn new(b: &'a [u8]) -> Self {
        Self { b, pos: 0 }
    }

    fn bit(&mut self) -> Option<bool> {
        let byte = *self.b.get(self.pos / 8)?;
        let v = byte >> (7 - self.pos % 8) & 1;
        self.pos += 1;
        Some(v == 1)
    }

    fn read(&mut self, n: u32) -> Option<u32> {
        let mut v = 0u32;
        for _ in 0..n {
            v = v << 1 | u32::from(self.bit()?);
        }
        Some(v)
    }

    /// The bits from `start` to here, left-aligned into bytes (the last
    /// zero-padded).
    fn bytes_from(&self, start: usize) -> Option<Vec<u8>> {
        let mut r = Bits {
            b: self.b,
            pos: start,
        };
        let n = self.pos - start;
        let mut out = vec![0u8; n.div_ceil(8)];
        for i in 0..n {
            if r.bit()? {
                out[i / 8] |= 0x80 >> (i % 8);
            }
        }
        Some(out)
    }

    /// `n` bytes from here: borrowed when byte-aligned, shifted when not.
    fn take(&mut self, n: usize) -> Option<std::borrow::Cow<'a, [u8]>> {
        if self.pos.is_multiple_of(8) {
            let s = self.b.get(self.pos / 8..self.pos / 8 + n)?;
            self.pos += n * 8;
            return Some(std::borrow::Cow::Borrowed(s));
        }
        let start = self.pos;
        self.pos += n * 8;
        if self.pos > self.b.len() * 8 {
            return None;
        }
        self.bytes_from(start).map(std::borrow::Cow::Owned)
    }
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

    /// An AudioMuxElement as BS17 sends it: StreamMuxConfig (AAC-LC 48 kHz
    /// stereo, CRC present), payload length 255+255+160 = 670, the payload
    /// shifted by 5 bits, 3 bits of padding.
    fn element(payload: &[u8]) -> Vec<u8> {
        // 0 0 1 000000 0000 000 | ASC 00010 0011 0010 | 0 0 0 | 000 |
        // fullness 8 | 0 | 1 | crc 8 | lengths | payload | pad
        let mut bits = String::from("0010000000000000"); // header, 16 bits
        bits += "0001000110010"; // ASC: AAC-LC, 48 kHz, 2 channels
        bits += "000000"; // GASpecificConfig 0 0 0, frameLengthType 0
        bits += "01101010"; // latmBufferFullness
        bits += "01"; // otherDataPresent 0, crcCheckPresent 1
        bits += "00000001"; // crcCheckSum
        let mut n = payload.len();
        while n >= 255 {
            bits += "11111111";
            n -= 255;
        }
        bits += &format!("{n:08b}");
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

    #[test]
    fn latm_element_taken_apart() {
        let payload: Vec<u8> = (0..670u32).map(|i| (i * 7 + 3).to_be_bytes()[3]).collect();
        let e = element(&payload);
        // The real element's first bytes: 20 00 11 90 0d 48 0f ff fd ...
        assert_eq!(
            &e[..9],
            &[0x20, 0x00, 0x11, 0x90, 0x0d, 0x48, 0x0f, 0xff, 0xfd]
        );
        let f = latm_frame(&e).unwrap();
        assert_eq!(
            f.config.as_deref(),
            Some(&[0x11, 0x90][..]),
            "AAC-LC 48 kHz 2ch"
        );
        assert_eq!(f.payload.as_ref(), payload.as_slice());
        // useSameStreamMux: no config, the lengths right after the bit:
        // 1 | 00000011 | 01010101 01100110 01110111 | 0000000.
        let same = [0x81, 0xaa, 0xb3, 0x3b, 0x80];
        let f = latm_frame(&same).unwrap();
        assert_eq!(f.config, None);
        assert_eq!(f.payload.as_ref(), &[0x55, 0x66, 0x77]);
        // A 5.1 config (channel configuration 6) reads too; version 1 and
        // two subframes do not.
        assert!(latm_frame(&[0x40, 0, 0]).is_none());
        assert!(latm_frame(&[0x20, 0x04, 0x11]).is_none());
    }

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
