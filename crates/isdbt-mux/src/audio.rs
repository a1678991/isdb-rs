//! MPEG-2 AAC ADTS elementary streams: frame list, timing, T-STD buffer
//! parameters, derived signalling (audio mode, sampling rate, quality
//! mode) and the ARIB STD-B32 Part 2 §5.2 compliance policy applied when a
//! file is loaded ([`AdtsCompliance`]).
//!
//! The ADTS header, CRC and the header-level B32 Part 2 rules are in
//! [`crate::adts`] (header syntax from ISO/IEC 13818-7, which is not
//! available locally — see that module and SPEC-NOTES "AAC ADTS").  No
//! raw_data_block is walked and no input header is rewritten.

use isdb_si::values::AudioMode;
use serde::{Deserialize, Serialize};

use crate::adts::b32::{self, QualityMode};
use crate::adts::{AdtsHeader, BUFFER_BITS_PER_CHANNEL, Crc16, HEADER_BITS, HEADER_BYTES};
use crate::error::{MuxError, Result};

pub use crate::adts::SAMPLING_FREQUENCIES;

/// What the multiplexer does with an ADTS file that violates ARIB STD-B32
/// Part 2 §5.2 (`adts_compliance` of an `aac-adts` stream).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AdtsCompliance {
    /// Send the file byte-identically if its headers satisfy every
    /// header-level rule; refuse it otherwise ([`b32::conform`]).  Nothing
    /// is rewritten: crc_check cannot be inserted into protection_absent
    /// '1' input, and a protected frame's header cannot be changed without
    /// invalidating its crc_check (both need a raw_data_block walk, not
    /// included).
    #[default]
    Conform,
    /// Send the frames unchanged; report every violation as a multiplexer
    /// warning.
    Warn,
    /// Send the frames unchanged without checking (experiments).
    PassThrough,
}

/// One ADTS frame.
#[derive(Debug, Clone, Copy)]
pub struct AdtsFrame {
    /// Byte offset.
    pub offset: usize,
    /// aac_frame_length.
    pub len: usize,
    /// Samples per channel in this frame.
    pub samples: u32,
}

/// A parsed ADTS stream.
#[derive(Debug, Clone)]
pub struct AdtsEs {
    /// The stream bytes (as loaded; never rewritten).
    pub data: Vec<u8>,
    /// Frames in order.
    pub frames: Vec<AdtsFrame>,
    /// Sampling frequency (Hz) of the first frame.
    pub sampling_frequency: u32,
    /// channel_configuration of the first frame.
    pub channel_configuration: u8,
    /// profile (0 Main, 1 LC, 2 SSR).
    pub profile: u8,
    /// Total samples per channel of one pass.
    pub total_samples: u64,
    /// Coded channels without LFE (NCC) from channel_configuration
    /// ([`crate::adts::buffer_channels`]: 1–5 → 1–5, 6 → 5); 0 (PCE, not
    /// read) counts as 1 and 7 as 7.
    pub coded_channels: u32,
    /// B32 Part 2 §5.2 header findings of the input file (`None` with
    /// [`AdtsCompliance::PassThrough`]).
    pub b32: Option<b32::Report>,
}

impl AdtsEs {
    /// Parse a complete ADTS stream without B32 checks; the stream must
    /// start with a syncword and keep sampling frequency and channel
    /// configuration constant.
    pub fn parse(data: Vec<u8>, path: &str) -> Result<Self> {
        let err = |msg: String| MuxError::Media {
            path: path.to_string(),
            msg,
        };
        let spans = b32::split_frames(&data).map_err(err)?;
        let mut frames = Vec::with_capacity(spans.len());
        let mut first: Option<AdtsHeader> = None;
        for &(pos, len) in &spans {
            let h =
                AdtsHeader::parse(&data[pos..]).map_err(|e| err(format!("{e} at byte {pos}")))?;
            if h.layer != 0 {
                return Err(err(format!(
                    "ADTS layer {} at byte {pos}, shall be '00'",
                    h.layer
                )));
            }
            if h.sampling_frequency().is_none() {
                return Err(err(format!(
                    "reserved sampling_frequency_index {} at byte {pos}",
                    h.sampling_frequency_index
                )));
            }
            match first {
                None => first = Some(h),
                Some(f)
                    if f.sampling_frequency_index != h.sampling_frequency_index
                        || f.channel_configuration != h.channel_configuration =>
                {
                    return Err(err(format!(
                        "sampling frequency / channel configuration change at byte {pos}"
                    )));
                }
                _ => {}
            }
            frames.push(AdtsFrame {
                offset: pos,
                len,
                samples: h.samples(),
            });
        }
        let h = first.ok_or_else(|| err("no ADTS frames".into()))?;
        let total_samples = frames.iter().map(|f| f.samples as u64).sum();
        let coded_channels = crate::adts::buffer_channels(h.channel_configuration).unwrap_or(
            match h.channel_configuration {
                0 => 1,
                c => c as u32,
            },
        );
        Ok(Self {
            data,
            frames,
            sampling_frequency: h.sampling_frequency().unwrap_or(0),
            channel_configuration: h.channel_configuration,
            profile: h.profile,
            total_samples,
            coded_channels,
            b32: None,
        })
    }

    /// Parse and apply the B32 Part 2 §5.2 policy.  Returns the stream and
    /// the warnings to report ([`AdtsCompliance::Warn`]: every violated
    /// rule, plus the [`b32::stream_fullness`] model when
    /// adts_buffer_fullness is 0x7FF; [`AdtsCompliance::Conform`]: any
    /// warning-level rule).  [`AdtsCompliance::Conform`] fails with the
    /// per-rule report, the reason and a re-encode suggestion when any
    /// header rule is violated.
    pub fn load(data: Vec<u8>, path: &str, policy: AdtsCompliance) -> Result<(Self, Vec<String>)> {
        let err = |msg: String| MuxError::Media {
            path: path.to_string(),
            msg,
        };
        match policy {
            AdtsCompliance::PassThrough => Ok((Self::parse(data, path)?, Vec::new())),
            AdtsCompliance::Warn => {
                let (report, _) = b32::check_stream(&data).map_err(err)?;
                let mut warnings: Vec<String> = report
                    .lines()
                    .into_iter()
                    .map(|l| format!("{path}: ARIB STD-B32 Part 2 §5.2 (adts_compliance = \"warn\", sent unchanged): {l}"))
                    .collect();
                if report.rules.contains_key(&b32::Rule::BufferFullness) {
                    warnings.push(match b32::stream_fullness(&data) {
                        Ok(m) => format!("{path}: for reference (not applied), {}", m.describe()),
                        Err(e) => {
                            format!("{path}: no constant-rate adts_buffer_fullness model: {e}")
                        }
                    });
                }
                let mut es = Self::parse(data, path)?;
                es.b32 = Some(report);
                Ok((es, warnings))
            }
            AdtsCompliance::Conform => {
                let c = b32::conform(&data).map_err(|r| err(r.to_string()))?;
                let warnings: Vec<String> = c
                    .input
                    .lines()
                    .into_iter()
                    .filter(|l| l.starts_with("warning"))
                    .map(|l| format!("{path}: ARIB STD-B32 Part 2 §5.2: {l}"))
                    .collect();
                let mut es = Self::parse(c.data, path)?;
                es.b32 = Some(c.input);
                Ok((es, warnings))
            }
        }
    }

    /// Full-bandwidth channels for the H.222.0 §2.4.2.3 buffer tables.
    pub fn channels(&self) -> u32 {
        match self.channel_configuration {
            0 | 1 => 1,
            2 => 2,
            3 => 3,
            4 => 4,
            5 | 6 => 5,
            7 => 7,
            _ => 2,
        }
    }

    /// Rxn of H.222.0 §2.4.2.3 for ADTS audio (bit/s).
    pub fn rx(&self) -> u64 {
        match self.channels() {
            1..=2 => 2_000_000,
            3..=8 => 5_529_600,
            9..=12 => 8_294_400,
            _ => 33_177_600,
        }
    }

    /// BSn of H.222.0 §2.4.2.3 for ADTS audio (bytes).
    pub fn bs(&self) -> u64 {
        match self.channels() {
            1..=2 => 3_584,
            3..=8 => 8_976,
            9..=12 => 12_804,
            _ => 51_216,
        }
    }

    /// Mean bit rate of the ADTS stream as sent (whole frames, bit/s).
    pub fn bit_rate(&self) -> f64 {
        let bytes: u64 = self.frames.iter().map(|f| f.len as u64).sum();
        b32::bit_rate(bytes, self.total_samples, self.sampling_frequency)
    }

    /// The quality mode B32 Part 2 Attachment Ch. 3 Table 1 assigns to this
    /// stream ([`QualityMode::derive`]).
    pub fn quality_mode(&self) -> QualityMode {
        QualityMode::derive(
            self.sampling_frequency,
            self.bit_rate(),
            self.coded_channels,
        )
    }

    /// sampling_rate code of the audio component descriptor (B10 Table 6-45).
    pub fn sampling_rate_code(&self) -> Option<u8> {
        Some(match self.sampling_frequency {
            16_000 => 0b001,
            22_050 => 0b010,
            24_000 => 0b011,
            32_000 => 0b101,
            44_100 => 0b110,
            48_000 => 0b111,
            _ => return None,
        })
    }

    /// Audio mode (component_type b4–b0, B10 Table 6-43) from
    /// channel_configuration (B32 Part 2 §5.2.3 (2) element mapping).
    pub fn audio_mode(&self) -> Option<AudioMode> {
        Some(match self.channel_configuration {
            1 => AudioMode::Mono,    // 1/0
            2 => AudioMode::Stereo,  // 2/0
            3 => AudioMode::M3_0,    // 3/0
            4 => AudioMode::M3_1,    // 3/1
            5 => AudioMode::M3_2,    // 3/2
            6 => AudioMode::M3_2Lfe, // 3/2 + LFE
            _ => return None,
        })
    }
}

/// Builds syntactically valid, silent AAC LC ADTS frames of an exact size
/// (synthetic media for tests and experiments): one CPE (stereo) or SCE
/// (channel_configuration 1) with max_sfb 0 — i.e. all spectral
/// coefficients zero — padded with fill elements (extension_type
/// EXT_FILL) to the requested length, then END.  Every header field is
/// settable, so frames violating any one B32 Part 2 header rule can be
/// built.
///
/// With protection_absent '0' the crc_check is computed per B32 Part 2
/// Appendix 1 ([`Crc16`]) from the element layout known by construction —
/// no raw_data_block walk: the 56 header bits, then the SCE / CPE from
/// its element_instance_tag (the bits after the 3-bit id_syn_ele) zero
/// padded to 192 bits, and for the CPE its second individual_channel_stream
/// zero padded to 128 bits; fill elements are not protected.  (That an
/// element's protected bits start after id_syn_ele was established during
/// development against broadcast frames and an independent decoder —
/// SPEC-NOTES "AAC ADTS"; no in-tree test re-checks it, because the
/// raw_data_block is not parsed.)
#[derive(Debug, Clone, Copy)]
pub struct SilentFrame {
    /// Header template; aac_frame_length is set by [`SilentFrame::build`],
    /// crc_check is computed when protection_absent is false.
    pub header: AdtsHeader,
    /// Total frame length in bytes.
    pub len: usize,
}

impl SilentFrame {
    /// LC, 48 kHz, stereo, ID '1', protection_absent '0' (crc_check
    /// computed), and the adts_buffer_fullness a constant-rate encoder
    /// writes for a stream of identical `len`-byte frames
    /// ([`SilentFrame::cbr_fullness`]), so a stream of these frames
    /// satisfies every header rule of B32 Part 2 §5.2 as built.
    pub fn stereo_48k(len: usize) -> Self {
        Self {
            header: AdtsHeader {
                id: 1,
                layer: 0,
                protection_absent: false,
                profile: 1,
                sampling_frequency_index: 3,
                private_bit: false,
                channel_configuration: 2,
                original_copy: false,
                home: false,
                copyright_identification_bit: false,
                copyright_identification_start: false,
                aac_frame_length: len as u16,
                adts_buffer_fullness: Self::cbr_fullness(len, 2),
                number_of_raw_data_blocks_in_frame: 0,
                crc_check: None,
            },
            len,
        }
    }

    /// adts_buffer_fullness of a constant-rate stream of identical
    /// `len`-byte frames with `channels` buffer channels: the reservoir
    /// stays full at 6144·NCC − 8·len bits, carried as
    /// ⌊reservoir / (32·NCC)⌋ (the [`b32::cbr_fullness`] model; 0 if a
    /// frame exceeds the buffer).
    pub fn cbr_fullness(len: usize, channels: u32) -> u16 {
        let buffer = (BUFFER_BITS_PER_CHANNEL * channels.max(1)) as usize;
        (buffer.saturating_sub(8 * len) / (32 * channels.max(1) as usize)) as u16
    }

    /// The frame bytes (`len` ≥ header + 13 bytes).
    ///
    /// # Panics
    ///
    /// When `len` is too small for the header and the audio element, or
    /// cannot be filled exactly with fill elements.
    pub fn build(&self) -> Vec<u8> {
        let mut h = self.header;
        h.aac_frame_length = self.len as u16;
        let hl = if h.protection_absent {
            HEADER_BYTES
        } else {
            HEADER_BYTES + 2
        };
        let mut w = BitWriter::default();
        let mono = h.channel_configuration == 1;
        // Appendix 1 regions of the audio element, in bits from the start
        // of raw_data_block: (start, end of element, protected length).
        let ics_bits = 8 + 11 + 3;
        let regions: &[(usize, usize, usize)] = if mono {
            &[(3, 3 + 4 + ics_bits, 192)]
        } else {
            let end = 3 + 4 + 1 + 2 * ics_bits;
            &[(3, end, 192), (3 + 4 + 1 + ics_bits, end, 128)]
        };
        // element + ics with max_sfb 0: id(3) tag(4) [common_window(1)]
        // global_gain(8) ics_info(11: reserved, ONLY_LONG, shape, max_sfb 0,
        // predictor 0) pulse/tns/gain flags(3) per channel
        if mono {
            w.put(0, 3);
            w.put(0, 4);
            w.put(100, 8);
            w.put(0, 11);
            w.put(0, 3);
        } else {
            w.put(1, 3);
            w.put(0, 4);
            w.put(0, 1); // common_window 0
            for _ in 0..2 {
                w.put(100, 8);
                w.put(0, 11);
                w.put(0, 3);
            }
        }
        // Fill exactly to `len`: the R bits between the audio element and
        // END are k fill elements of 7 + 8q bits each (id 3 + count 4 +
        // q bytes, or id 3 + count 15 (4) + esc_count (8) + q − 1 bytes for
        // q ≥ 15); R ≡ 7k (mod 8) because the frame is byte aligned.
        let budget = (self.len - hl) * 8;
        let r = budget
            .checked_sub(w.len() + 3)
            .expect("SilentFrame: len too small for the audio element");
        let mut k = (8 - r % 8) % 8;
        while 7 * k <= r && (r - 7 * k) / 8 > 270 * k {
            k += 8;
        }
        assert!(
            7 * k <= r,
            "SilentFrame: len {} cannot be filled exactly",
            self.len
        );
        let mut q_left = (r - 7 * k) / 8;
        for _ in 0..k {
            let q = q_left.min(270);
            q_left -= q;
            w.put(6, 3); // ID_FIL
            let payload = if q <= 14 {
                w.put(q as u32, 4);
                q
            } else {
                w.put(15, 4);
                w.put((q - 15) as u32, 8); // count = 15 + esc_count − 1 = q − 1
                q - 1
            };
            for i in 0..payload {
                // EXT_FILL '0000' + fill_nibble '0000', then fill_byte '10100101'
                w.put(if i == 0 { 0 } else { 0xA5 }, 8);
            }
        }
        w.put(7, 3); // END
        debug_assert_eq!(w.len(), budget);
        let raw = w.bytes();
        let mut f = h.encode_fixed_variable().to_vec();
        if !h.protection_absent {
            f.extend_from_slice(&[0, 0]);
        }
        f.extend_from_slice(&raw);
        if !h.protection_absent {
            let crc = crc_over_regions(&f, hl * 8, regions);
            f[7..9].copy_from_slice(&crc.to_be_bytes());
        }
        f
    }
}

/// B32 Part 2 Appendix 1 crc_check of a frame whose protected element
/// regions are known: the 56 header bits, then for each region
/// `(start, end, protected)` (bits relative to `raw_start`) the bits
/// `[start, min(end, start + protected))`, zero padded to `protected`.
fn crc_over_regions(frame: &[u8], raw_start: usize, regions: &[(usize, usize, usize)]) -> u16 {
    let mut c = Crc16::new();
    c.bits(frame, 0, HEADER_BITS);
    for &(start, end, protected) in regions {
        let have = (end - start).min(protected);
        c.bits(frame, raw_start + start, have);
        c.zeros(protected - have);
    }
    c.value()
}

#[derive(Default)]
struct BitWriter {
    bytes: Vec<u8>,
    bits: usize,
}

impl BitWriter {
    fn put(&mut self, v: u32, n: u32) {
        for k in (0..n).rev() {
            if self.bits.is_multiple_of(8) {
                self.bytes.push(0);
            }
            if v >> k & 1 != 0 {
                *self.bytes.last_mut().unwrap() |= 0x80 >> (self.bits % 8);
            }
            self.bits += 1;
        }
    }
    fn len(&self) -> usize {
        self.bits
    }
    fn bytes(self) -> Vec<u8> {
        self.bytes
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adts::VBR_FULLNESS;
    use crate::adts::b32::{Rule, check_frame, conform};

    /// Synthetic silent ADTS frame (LC, 48 kHz, stereo, crc_check, CBR
    /// fullness).
    fn frame(len: usize) -> Vec<u8> {
        SilentFrame::stereo_48k(len).build()
    }

    #[test]
    fn parse_frames() {
        let mut d = frame(300);
        d.extend(frame(400));
        let a = AdtsEs::parse(d, "t").unwrap();
        assert_eq!(a.frames.len(), 2);
        assert_eq!(a.frames[1].offset, 300);
        assert_eq!(a.sampling_frequency, 48_000);
        assert_eq!(a.channel_configuration, 2);
        assert_eq!(a.coded_channels, 2);
        assert_eq!(a.sampling_rate_code(), Some(0b111));
        assert_eq!(a.audio_mode(), Some(AudioMode::Stereo));
        assert_eq!(a.total_samples, 2048);
        // coded channels from channel_configuration (5.1 → 5, LFE excluded)
        for (cc, n) in [(1u8, 1u32), (3, 3), (5, 5), (6, 5)] {
            let mut f = SilentFrame::stereo_48k(300);
            f.header.channel_configuration = cc;
            assert_eq!(AdtsEs::parse(f.build(), "t").unwrap().coded_channels, n);
        }
    }

    /// Independent CRC-16 (ISO/IEC 11172-3 §2.4.3.1: x¹⁶ + x¹⁵ + x² + 1,
    /// preset all ones, MSB first, no final inversion) over a bit list.
    fn crc16_of_bits(bits: &[bool]) -> u16 {
        let mut reg: u32 = 0xFFFF;
        for &b in bits {
            let fb = ((reg >> 15) & 1 == 1) ^ b;
            reg = (reg << 1) & 0xFFFF;
            if fb {
                reg ^= 0x8005;
            }
        }
        reg as u16
    }

    fn bit_at(d: &[u8], i: usize) -> bool {
        d[i / 8] & (0x80 >> (i % 8)) != 0
    }

    /// The silent-frame crc_check equals the Appendix 1 CRC computed here
    /// from the coverage rule and the element syntax written out field by
    /// field (no shared code with `SilentFrame::build` beyond the frame
    /// bytes).
    #[test]
    fn silent_frame_crc_matches_appendix_1() {
        let mut ascii = Vec::new();
        for c in b"123456789" {
            ascii.extend((0..8).map(|k| c & (0x80 >> k) != 0));
        }
        // catalogued check value of this parameter set (CRC-16/CMS)
        assert_eq!(crc16_of_bits(&ascii), 0xAEE7);
        // individual_channel_stream with max_sfb 0: global_gain 8,
        // ics_info (ics_reserved_bit 1, window_sequence 2, window_shape 1,
        // max_sfb 6, predictor_data_present 1), section / scale factor
        // data empty, pulse / tns / gain_control flags 1 + 1 + 1
        let ics = 8 + (1 + 2 + 1 + 6 + 1) + 3;
        for mono in [false, true] {
            for len in [22usize, 23, 25, 31, 35, 100, 200, 384, 386, 1500] {
                let mut s = SilentFrame::stereo_48k(len);
                if mono {
                    s.header.channel_configuration = 1;
                }
                let f = s.build();
                assert_eq!(f.len(), len);
                let raw = 9 * 8;
                // SCE: tag 4 + ics; CPE: tag 4 + common_window 1 + 2 ics
                let (id, body) = if mono {
                    (0b000, 4 + ics)
                } else {
                    (0b001, 4 + 1 + 2 * ics)
                };
                let id_bits: Vec<bool> = (0..3).map(|k| bit_at(&f, raw + k)).collect();
                assert_eq!(
                    id_bits,
                    (0..3).map(|k| id >> (2 - k) & 1 == 1).collect::<Vec<_>>()
                );
                // the next element is a FIL ('110') or END ('111')
                let next = raw + 3 + body;
                assert!(bit_at(&f, next) && bit_at(&f, next + 1), "{len}");
                let mut cover: Vec<bool> = (0..56).map(|i| bit_at(&f, i)).collect();
                let elem: Vec<bool> = (raw + 3..next).map(|i| bit_at(&f, i)).collect();
                cover.extend(&elem);
                cover.extend(std::iter::repeat_n(false, 192 - elem.len()));
                if !mono {
                    let second: Vec<bool> = (next - ics..next).map(|i| bit_at(&f, i)).collect();
                    cover.extend(&second);
                    cover.extend(std::iter::repeat_n(false, 128 - second.len()));
                }
                let crc = u16::from_be_bytes([f[7], f[8]]);
                assert_eq!(crc, crc16_of_bits(&cover), "mono {mono} len {len}");
                // the zero padding matters: the fill bits after the element
                // would give another value
                let unpadded: Vec<bool> = (0..56)
                    .chain(raw + 3..raw + 3 + 192)
                    .map(|i| i < len * 8 && bit_at(&f, i))
                    .collect();
                if len > 40 {
                    assert_ne!(
                        crc16_of_bits(&unpadded[..56 + 192]),
                        crc16_of_bits(&cover[..56 + 192])
                    );
                }
                let c = check_frame(&f).unwrap();
                assert!(c.violations.is_empty(), "{len}: {:?}", c.violations);
                assert_eq!(c.header.crc_check, Some(crc));
            }
        }
        // the CRC covers the header: another original_copy, another crc_check
        let mut s = SilentFrame::stereo_48k(200);
        let a = s.build();
        s.header.original_copy = true;
        assert_ne!(a[7..9], s.build()[7..9]);
    }

    /// One frame per B32 Part 2 header rule, each violating exactly that rule.
    #[test]
    fn each_rule_detected() {
        let rules_of = |f: SilentFrame| -> Vec<Rule> {
            check_frame(&f.build())
                .unwrap()
                .violations
                .iter()
                .map(|v| v.rule)
                .collect()
        };
        let base = SilentFrame::stereo_48k(200);
        assert_eq!(rules_of(base), vec![]);
        let with = |m: &dyn Fn(&mut AdtsHeader)| {
            let mut f = base;
            m(&mut f.header);
            rules_of(f)
        };
        assert_eq!(
            with(&|h| h.protection_absent = true),
            vec![Rule::ProtectionAbsent]
        );
        assert_eq!(
            with(&|h| h.adts_buffer_fullness = VBR_FULLNESS),
            vec![Rule::BufferFullness]
        );
        assert_eq!(with(&|h| h.id = 0), vec![Rule::Id]);
        assert_eq!(with(&|h| h.layer = 1), vec![Rule::Layer]);
        assert_eq!(with(&|h| h.profile = 0), vec![Rule::Profile]);
        assert_eq!(with(&|h| h.profile = 2), vec![Rule::Profile]);
        for sfi in [0u8, 1, 2, 9, 10, 11] {
            assert_eq!(
                with(&|h| h.sampling_frequency_index = sfi),
                vec![Rule::SamplingFrequency],
                "sfi {sfi}"
            );
        }
        for sfi in 3u8..=8 {
            assert_eq!(
                with(&|h| h.sampling_frequency_index = sfi),
                vec![],
                "sfi {sfi}"
            );
        }
        assert_eq!(
            with(&|h| h.channel_configuration = 7),
            vec![Rule::ChannelConfiguration]
        );
        for cc in 0u8..=6 {
            assert_eq!(with(&|h| h.channel_configuration = cc), vec![], "cc {cc}");
        }
        assert_eq!(
            with(&|h| {
                h.number_of_raw_data_blocks_in_frame = 1;
                h.protection_absent = true;
            }),
            vec![Rule::ProtectionAbsent, Rule::RawDataBlocks]
        );
    }

    fn stream(lens: &[usize], n: usize, edit: impl Fn(usize, &mut SilentFrame)) -> Vec<u8> {
        let mut d = Vec::new();
        for (i, &l) in lens.iter().cycle().take(n).enumerate() {
            let mut f = SilentFrame::stereo_48k(l);
            edit(i, &mut f);
            d.extend(f.build());
        }
        d
    }

    const LENS: [usize; 8] = [300, 420, 380, 512, 610, 333, 470, 401];

    /// Conforming input passes byte-identically; nothing is ever rewritten.
    #[test]
    fn conform_passes_conforming_input_unchanged() {
        let d = stream(&LENS, 64, |i, f| f.header.original_copy = i % 3 == 0);
        let c = conform(&d).unwrap();
        assert_eq!(c.data, d);
        assert!(c.input.rules.is_empty());
        let (es, w) = AdtsEs::load(d.clone(), "x.aac", AdtsCompliance::Conform).unwrap();
        assert_eq!(es.data, d);
        assert!(w.is_empty(), "{w:?}");
    }

    /// protection_absent '1': refused (crc_check cannot be inserted), with
    /// the constant-rate fullness model for reference.
    #[test]
    fn conform_refuses_unprotected_input() {
        let d = stream(&LENS, 64, |_, f| {
            f.header.protection_absent = true;
            f.header.adts_buffer_fullness = VBR_FULLNESS;
        });
        let e = conform(&d).unwrap_err();
        assert!(e.input.rules.contains_key(&Rule::ProtectionAbsent));
        assert!(e.input.rules.contains_key(&Rule::BufferFullness));
        assert!(
            e.reason.contains("crc_check cannot be inserted"),
            "{}",
            e.reason
        );
        assert!(!e.reason.contains("would invalidate"), "{}", e.reason);
        let m = e.fullness.as_ref().unwrap();
        assert_eq!(m.channels, 2);
        // modelled on the lengths with crc_check (+2 bytes)
        let mean = LENS.iter().map(|&l| (l + 2) as f64 * 8.0).sum::<f64>() / 8.0;
        assert!((m.mean_frame_bits - mean).abs() < 1e-9);
        let msg = e.to_string();
        assert!(
            msg.contains("13818-7")
                && msg.contains("for reference")
                && msg.contains("protection_absent '0'"),
            "{msg}"
        );
        assert!(!msg.contains("ffmpeg"), "{msg}");
        let err = AdtsEs::load(d.clone(), "x.aac", AdtsCompliance::Conform).unwrap_err();
        assert!(err.to_string().contains("x.aac"));
        // without a crc_check but with a constant-rate fullness: still refused
        let d = stream(&LENS, 8, |_, f| f.header.protection_absent = true);
        let e = conform(&d).unwrap_err();
        assert_eq!(
            e.input.rules.keys().collect::<Vec<_>>(),
            [&Rule::ProtectionAbsent]
        );
        assert!(e.fullness.is_none());
    }

    /// Protected input that would need a header rewrite is refused: the
    /// rewrite would invalidate crc_check.
    #[test]
    fn conform_refuses_protected_input_needing_a_header_rewrite() {
        let d = stream(&LENS, 16, |_, f| {
            f.header.adts_buffer_fullness = VBR_FULLNESS
        });
        let e = conform(&d).unwrap_err();
        assert!(
            e.reason.contains("would invalidate the crc_check"),
            "{}",
            e.reason
        );
        assert!(!e.reason.contains("cannot be inserted"), "{}", e.reason);
        let m = e.fullness.as_ref().unwrap();
        let bits: Vec<u64> = LENS
            .iter()
            .cycle()
            .take(16)
            .map(|&l| l as u64 * 8)
            .collect();
        assert_eq!(m.values, b32::cbr_fullness(&bits, 2).unwrap().values);
        let d = stream(&LENS, 16, |_, f| f.header.id = 0);
        let e = conform(&d).unwrap_err();
        assert!(
            e.reason.contains("would invalidate the crc_check"),
            "{}",
            e.reason
        );
        assert!(e.fullness.is_none());
        // channel_configuration 0: no buffer channels without a PCE
        let d = stream(&LENS, 16, |_, f| {
            f.header.adts_buffer_fullness = VBR_FULLNESS;
            f.header.channel_configuration = 0;
        });
        let e = conform(&d).unwrap_err();
        assert!(e.fullness.is_none());
        assert!(
            e.input.rules[&Rule::ConstantRate]
                .example
                .contains("program_config_element"),
            "{e}"
        );
    }

    #[test]
    fn conform_refuses_unfixable_and_warn_passes() {
        let d = stream(&[300], 10, |i, f| {
            if i == 4 {
                f.header.profile = 0;
            }
        });
        let e = conform(&d).unwrap_err();
        assert_eq!(e.input.rules[&Rule::Profile].first_frame, 4);
        let msg = e.to_string();
        assert!(
            msg.contains("profile") && msg.contains("§5.2.2 (1)") && msg.contains("re-encod"),
            "{msg}"
        );
        // warn: unchanged bytes, warnings per rule, fullness model for 0x7FF
        let d = stream(&[300], 10, |i, f| {
            f.header.protection_absent = true;
            f.header.adts_buffer_fullness = VBR_FULLNESS;
            if i == 4 {
                f.header.profile = 0;
            }
        });
        let (es, w) = AdtsEs::load(d.clone(), "x.aac", AdtsCompliance::Warn).unwrap();
        assert_eq!(es.data, d);
        for want in [
            "profile",
            "protection_absent",
            "adts_buffer_fullness",
            "for reference",
        ] {
            assert!(w.iter().any(|l| l.contains(want)), "{want}: {w:?}");
        }
        let (es, w) = AdtsEs::load(d.clone(), "x.aac", AdtsCompliance::PassThrough).unwrap();
        assert_eq!(es.data, d);
        assert!(w.is_empty() && es.b32.is_none());
    }

    /// Identical frames: the model's value is the silent frame's default.
    #[test]
    fn silent_frame_fullness_is_the_cbr_value() {
        for len in [200usize, 386, 768, 1536] {
            let d = frame(len).repeat(20);
            let m = b32::stream_fullness(&d).unwrap();
            assert_eq!(
                m.values,
                vec![SilentFrame::cbr_fullness(len, 2); 20],
                "{len}"
            );
            assert_ne!(SilentFrame::cbr_fullness(len, 2), VBR_FULLNESS);
        }
    }
}
