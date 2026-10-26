//! MPEG-2 AAC ADTS (Audio Data Transport Stream) frames: typed header,
//! the ADTS CRC-16 and the header-level ARIB STD-B32 Part 2 §5.2
//! restrictions ([`b32`]).
//!
//! This module works on the ADTS *headers* only.  It does not parse
//! raw_data_block (that needs the ISO/IEC 13818-7 Annex A Huffman and
//! scalefactor-band tables, which ARIB STD-B32 does not reproduce and this
//! crate does not include; see SPEC-NOTES "AAC ADTS: raw_data_block not
//! parsed").  Consequently it cannot verify or insert the crc_check of an
//! arbitrary frame: the CRC covers element bits whose boundaries are only
//! known by parsing raw_data_block.  [`Crc16`] is used where the element
//! layout is known by construction ([`crate::audio::SilentFrame`]).
//!
//! # Sources
//!
//! * ARIB STD-B32 v3.11 Part 2 (local): §4.1 (the ADTS configuration
//!   "adts_fixed_header / adts_variable_header / adts_error_check /
//!   raw_data_block" of the Notification), §5.2.1–5.2.3 (restrictions),
//!   Appendix 1 (which bits the ADTS CRC protects), Appendix 2 (decoder
//!   buffer sizes), Appendix 3 (meaning of adts_buffer_fullness).
//! * **ISO/IEC 13818-7 is not available locally.**  From knowledge (marked
//!   so in SPEC-NOTES "AAC ADTS"): the header field widths and order
//!   (§6.2.1 adts_fixed_header / adts_variable_header).
//! * The CRC algorithm itself is ISO/IEC 11172-3 §2.4.3.1 (cited by B32
//!   Part 2 Appendix 1; 11172-3 is not local either): CRC-16 with
//!   generator x¹⁶ + x¹⁵ + x² + 1 (0x8005), register preset to all ones,
//!   bits fed MSB first, no final inversion.
//!
//! # Header layout (ISO/IEC 13818-7 §6.2.1, from knowledge)
//!
//! adts_fixed_header: syncword 0xFFF (12), ID (1), layer (2),
//! protection_absent (1), profile (2), sampling_frequency_index (4),
//! private_bit (1), channel_configuration (3), original_copy (1), home (1);
//! adts_variable_header: copyright_identification_bit (1),
//! copyright_identification_start (1), aac_frame_length (13, the whole
//! frame including headers), adts_buffer_fullness (11),
//! number_of_raw_data_blocks_in_frame (2).  With protection_absent '0' and
//! one raw_data_block, adts_error_check is crc_check (16).  A frame carries
//! 1024 × (number_of_raw_data_blocks_in_frame + 1) samples per channel.

pub mod b32;

use std::fmt;

/// sampling_frequency_index → Hz (ISO/IEC 13818-7 Table 35, from
/// knowledge; the B32 Part 2 §5.2.2 (1) list "0x0 (96kHz) … 0x3 (48kHz),
/// 0x4 (44.1kHz), 0x5 (32kHz), 0x6 (24kHz), 0x7 (22.05kHz), 0x8 (16kHz)"
/// agrees on every value it names).
pub const SAMPLING_FREQUENCIES: [u32; 12] = [
    96_000, 88_200, 64_000, 48_000, 44_100, 32_000, 24_000, 22_050, 16_000, 12_000, 11_025, 8_000,
];

/// Bytes of adts_fixed_header + adts_variable_header.
pub const HEADER_BYTES: usize = 7;
/// Bits covered by the CRC from the headers (B32 Part 2 Appendix 1 "all
/// bits of the headers": the 56 bits of the fixed and variable header,
/// not the crc_check itself).
pub const HEADER_BITS: usize = 56;
/// adts_buffer_fullness value signalling a variable-rate stream (B32 Part 2
/// §5.2.2 (2): "Use of 0x7FF (indicating variable rate) is not permitted").
pub const VBR_FULLNESS: u16 = 0x7FF;
/// Decoder input buffer per channel in bits: B32 Part 2 Appendix 3 (1)(a),
/// ISO/IEC 13818-7:2003 "amount of remained equivalent buffer per channel
/// (6,144 bits per channel)".
pub const BUFFER_BITS_PER_CHANNEL: u32 = 6144;

/// Channels with a decoder input buffer (NCC: SCE / CPE channels; the LFE
/// has none) implied by channel_configuration, for the
/// [`b32::cbr_fullness`] model: 1–5 → 1–5, 6 (5.1) → 5.  `None` for 0 (the
/// configuration is in a program_config_element, which is not read
/// without a raw_data_block walk) and 7 (more than 5.1 channels, outside
/// B32 Part 2 §5.2.1).  The element list per configuration is the
/// "Coding mode stipulated as default in the AAC Standard" table of B32
/// Part 2 §5.2.3 (2) (1: SCE; 2: CPE; 3: SCE CPE; 4: SCE CPE SCE;
/// 5: SCE CPE CPE; 6: SCE CPE CPE LFE).
pub fn buffer_channels(channel_configuration: u8) -> Option<u32> {
    match channel_configuration {
        1..=5 => Some(u32::from(channel_configuration)),
        6 => Some(5),
        _ => None,
    }
}

/// Why a header could not be read.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum HeaderError {
    /// Fewer bytes than the header needs.
    Short,
    /// The first 12 bits are not 0xFFF.
    NoSync,
}

impl fmt::Display for HeaderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            HeaderError::Short => f.write_str("truncated ADTS header"),
            HeaderError::NoSync => f.write_str("no ADTS syncword 0xFFF"),
        }
    }
}

/// Every field of adts_fixed_header + adts_variable_header (+ crc_check).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AdtsHeader {
    /// ID: '1' MPEG-2 (ISO/IEC 13818-7), '0' MPEG-4 (ISO/IEC 14496-3 ADTS).
    pub id: u8,
    /// layer, '00'.
    pub layer: u8,
    /// protection_absent: '1' = no adts_error_check.
    pub protection_absent: bool,
    /// profile: 0 Main, 1 LC, 2 SSR, 3 reserved.
    pub profile: u8,
    /// sampling_frequency_index.
    pub sampling_frequency_index: u8,
    /// private_bit.
    pub private_bit: bool,
    /// channel_configuration.
    pub channel_configuration: u8,
    /// original_copy.
    pub original_copy: bool,
    /// home.
    pub home: bool,
    /// copyright_identification_bit.
    pub copyright_identification_bit: bool,
    /// copyright_identification_start.
    pub copyright_identification_start: bool,
    /// aac_frame_length: bytes of the whole frame including the headers.
    pub aac_frame_length: u16,
    /// adts_buffer_fullness (0x7FF: variable rate).
    pub adts_buffer_fullness: u16,
    /// number_of_raw_data_blocks_in_frame (frame carries this + 1 blocks).
    pub number_of_raw_data_blocks_in_frame: u8,
    /// crc_check when protection_absent is '0' and the frame has one
    /// raw_data_block (with more blocks the error check also carries
    /// raw_data_block_position fields; `None` then).
    pub crc_check: Option<u16>,
}

impl AdtsHeader {
    /// Parse the headers at the start of `b` (and crc_check if present).
    pub fn parse(b: &[u8]) -> Result<Self, HeaderError> {
        if b.len() < HEADER_BYTES {
            return Err(HeaderError::Short);
        }
        if b[0] != 0xFF || b[1] & 0xF0 != 0xF0 {
            return Err(HeaderError::NoSync);
        }
        let protection_absent = b[1] & 1 != 0;
        let blocks = b[6] & 3;
        let crc_check = if !protection_absent && blocks == 0 {
            if b.len() < HEADER_BYTES + 2 {
                return Err(HeaderError::Short);
            }
            Some(u16::from_be_bytes([b[7], b[8]]))
        } else {
            None
        };
        Ok(Self {
            id: (b[1] >> 3) & 1,
            layer: (b[1] >> 1) & 3,
            protection_absent,
            profile: b[2] >> 6,
            sampling_frequency_index: (b[2] >> 2) & 0xF,
            private_bit: b[2] & 2 != 0,
            channel_configuration: ((b[2] & 1) << 2) | (b[3] >> 6),
            original_copy: b[3] & 0x20 != 0,
            home: b[3] & 0x10 != 0,
            copyright_identification_bit: b[3] & 0x08 != 0,
            copyright_identification_start: b[3] & 0x04 != 0,
            aac_frame_length: (u16::from(b[3] & 3) << 11)
                | (u16::from(b[4]) << 3)
                | u16::from(b[5] >> 5),
            adts_buffer_fullness: (u16::from(b[5] & 0x1F) << 6) | u16::from(b[6] >> 2),
            number_of_raw_data_blocks_in_frame: blocks,
            crc_check,
        })
    }

    /// The 7 header bytes (fixed + variable header, without crc_check).
    pub fn encode_fixed_variable(&self) -> [u8; HEADER_BYTES] {
        let l = self.aac_frame_length & 0x1FFF;
        let f = self.adts_buffer_fullness & 0x7FF;
        [
            0xFF,
            0xF0 | (self.id & 1) << 3 | (self.layer & 3) << 1 | u8::from(self.protection_absent),
            (self.profile & 3) << 6
                | (self.sampling_frequency_index & 0xF) << 2
                | u8::from(self.private_bit) << 1
                | (self.channel_configuration >> 2) & 1,
            (self.channel_configuration & 3) << 6
                | u8::from(self.original_copy) << 5
                | u8::from(self.home) << 4
                | u8::from(self.copyright_identification_bit) << 3
                | u8::from(self.copyright_identification_start) << 2
                | (l >> 11) as u8,
            (l >> 3) as u8,
            ((l & 7) as u8) << 5 | (f >> 6) as u8,
            ((f & 0x3F) as u8) << 2 | (self.number_of_raw_data_blocks_in_frame & 3),
        ]
    }

    /// Bytes before the first raw_data_block: 7, or 9 with crc_check.
    /// (With several raw_data_blocks and protection, the header error check
    /// is 2 × number_of_raw_data_blocks_in_frame + 2 bytes.)
    pub fn header_len(&self) -> usize {
        match (
            self.protection_absent,
            self.number_of_raw_data_blocks_in_frame,
        ) {
            (true, _) => HEADER_BYTES,
            (false, n) => HEADER_BYTES + 2 * n as usize + 2,
        }
    }

    /// Sampling frequency in Hz (`None` for the reserved indices 0xC–0xF).
    pub fn sampling_frequency(&self) -> Option<u32> {
        SAMPLING_FREQUENCIES
            .get(self.sampling_frequency_index as usize)
            .copied()
    }

    /// Samples per channel carried by the frame.
    pub fn samples(&self) -> u32 {
        1024 * (u32::from(self.number_of_raw_data_blocks_in_frame) + 1)
    }
}

/// CRC-16 of ISO/IEC 11172-3 §2.4.3.1 (generator 0x8005, preset 0xFFFF,
/// MSB first, no final inversion), fed bit by bit.
///
/// The ADTS crc_check is this CRC over the bits B32 Part 2 Appendix 1
/// lists: "all bits of the headers" (the 56 bits of the fixed and variable
/// header), then the "first 192 bits of any single_channel_element (SCE),
/// channel_pair_element (CPE), coupling_channel_element (CCE), low
/// frequency enhancement channel (LFE)", "the first 128 bits of the second
/// individual_channel_stream in the channel_pair_element", and "all
/// information in any program configuration element or data element";
/// "for any element where the specified protection length of 128 or 192
/// bits exceeds its actual length, the element is zero padded".  Feeding
/// those bits needs the element boundaries of the raw_data_block, which
/// this crate only knows for frames it builds itself
/// ([`crate::audio::SilentFrame`]).
#[derive(Debug, Clone, Copy)]
pub struct Crc16(u16);

impl Default for Crc16 {
    fn default() -> Self {
        Self(0xFFFF)
    }
}

impl Crc16 {
    /// Preset register.
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed one bit.
    pub fn bit(&mut self, b: bool) {
        let top = self.0 & 0x8000 != 0;
        self.0 <<= 1;
        if top != b {
            self.0 ^= 0x8005;
        }
    }

    /// Feed bits `[start, start + len)` of `data` (bit 0 = MSB of byte 0);
    /// bits beyond the end of `data` are fed as '0'.
    pub fn bits(&mut self, data: &[u8], start: usize, len: usize) {
        for i in start..start + len {
            let b = data.get(i / 8).is_some_and(|&x| x & (0x80 >> (i % 8)) != 0);
            self.bit(b);
        }
    }

    /// Feed `n` zero bits.
    pub fn zeros(&mut self, n: usize) {
        for _ in 0..n {
            self.bit(false);
        }
    }

    /// Current value.
    pub fn value(&self) -> u16 {
        self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_roundtrip_every_field() {
        let h = AdtsHeader {
            id: 1,
            layer: 0,
            protection_absent: false,
            profile: 1,
            sampling_frequency_index: 3,
            private_bit: true,
            channel_configuration: 6,
            original_copy: true,
            home: false,
            copyright_identification_bit: true,
            copyright_identification_start: false,
            aac_frame_length: 0x1ABC,
            adts_buffer_fullness: 0x5A5,
            number_of_raw_data_blocks_in_frame: 0,
            crc_check: Some(0xBEEF),
        };
        let mut b = h.encode_fixed_variable().to_vec();
        b.extend_from_slice(&0xBEEFu16.to_be_bytes());
        assert_eq!(AdtsHeader::parse(&b).unwrap(), h);
        assert_eq!(h.header_len(), 9);
        // a first header of FFmpeg `aac` encoder output: ff f9 4c 80 36 5f fc
        let a = AdtsHeader::parse(&[0xFF, 0xF9, 0x4C, 0x80, 0x36, 0x5F, 0xFC]).unwrap();
        assert_eq!(
            (
                a.id,
                a.protection_absent,
                a.profile,
                a.sampling_frequency_index
            ),
            (1, true, 1, 3)
        );
        assert_eq!(a.channel_configuration, 2);
        assert_eq!(a.aac_frame_length, 434);
        assert_eq!(a.adts_buffer_fullness, VBR_FULLNESS);
        assert_eq!(a.header_len(), 7);
        assert_eq!(
            AdtsHeader::parse(&[0xFF, 0xE9, 0, 0, 0, 0, 0]),
            Err(HeaderError::NoSync)
        );
    }

    #[test]
    fn crc16_known_answer() {
        // ISO/IEC 11172-3 CRC-16 (poly 0x8005, preset 0xFFFF, no
        // reflection, no final xor) is the "CRC-16/CMS" parameter set,
        // whose catalogued check value over "123456789" is 0xAEE7.
        let mut c = Crc16::new();
        c.bits(b"123456789", 0, 72);
        assert_eq!(c.value(), 0xAEE7);
    }
}
