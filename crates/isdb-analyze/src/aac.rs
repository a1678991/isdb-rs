//! MPEG-2 AAC ADTS elementary streams (stream_type 0x0F) checked against
//! ARIB STD-B32 v3.11 Part 2 §5.2 while a capture is scanned.
//!
//! The ADTS frames are reassembled from the PES payloads of the PID
//! (frames may span PES packets; bytes before the first syncword and after
//! a loss are skipped with resynchronisation).  Per frame:
//!
//! * **header rules only**, from a header parse written here
//!   independently of the multiplexer's (`isdbt_mux::adts::AdtsHeader`), so
//!   that a symmetric encode/parse bug in `isdb mux` (or in its synthetic
//!   frames) cannot hide: ID, protection_absent, profile,
//!   sampling_frequency_index, channel_configuration,
//!   adts_buffer_fullness, number_of_raw_data_blocks_in_frame (§4.1,
//!   §5.2.1, §5.2.2 (1)/(2)).  The syncword search requires layer '00'
//!   (12 + 1 + 2 bits, as `isdbt_mux::adts::b32::Rule::Layer` expects), so
//!   a stream with another layer yields no frames (`aac.frames`).
//!
//! The raw_data_block is not parsed (that needs the ISO/IEC 13818-7 Annex
//! A tables, not included; `isdbt-mux` SPEC-NOTES "AAC ADTS:
//! raw_data_block not parsed"), so the crc_check value, coupling channel
//! elements (§5.2.2 (3)), element order (§5.2.3 (2)), PCE fields
//! (§5.2.3 (3)(d)), SBR fill payloads (§5.2.3 (4)(a)) and the LC tool set
//! are not checked.  Coded channels come from channel_configuration;
//! implicit SBR cannot be seen.
//!
//! The ADTS header layout is ISO/IEC 13818-7 §6.2.1 (not available
//! locally; from knowledge, as in `isdbt-mux`).

use std::collections::BTreeMap;

use isdb_ts::bits::BitReader;
use isdbt_mux::adts::b32::{self, Rule};

/// The ADTS fixed + variable header fields the rules need.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Header {
    /// ID ('1' MPEG-2).
    pub id: u8,
    /// layer.
    pub layer: u8,
    /// protection_absent.
    pub protection_absent: bool,
    /// profile (1 = LC).
    pub profile: u8,
    /// sampling_frequency_index.
    pub sampling_frequency_index: u8,
    /// channel_configuration.
    pub channel_configuration: u8,
    /// aac_frame_length.
    pub frame_length: usize,
    /// adts_buffer_fullness.
    pub buffer_fullness: u16,
    /// number_of_raw_data_blocks_in_frame.
    pub raw_data_blocks: u8,
}

/// Parse the 7 header bytes at the start of `b` (syncword 0xFFF required).
pub fn header(b: &[u8]) -> Option<Header> {
    if b.len() < 7 || b[0] != 0xFF || b[1] >> 4 != 0xF {
        return None;
    }
    // All 56 header bits are present, so none of the reads can fail.
    fields(&b[..7]).ok()
}

/// The fixed + variable header after the 12-bit syncword, MSB first.
fn fields(b: &[u8]) -> isdb_ts::Result<Header> {
    let mut r = BitReader::new(b, "adts_header");
    r.read(12)?; // syncword
    let id = r.read_u8(1)?;
    let layer = r.read_u8(2)?;
    let protection_absent = r.bool()?;
    let profile = r.read_u8(2)?;
    let sampling_frequency_index = r.read_u8(4)?;
    r.read(1)?; // private_bit
    let channel_configuration = r.read_u8(3)?;
    r.read(4)?; // original_copy, home, copyright_identification_bit/start
    let frame_length = r.read_usize(13)?;
    let buffer_fullness = r.read_u16(11)?;
    let raw_data_blocks = r.read_u8(2)?;
    Ok(Header {
        id,
        layer,
        protection_absent,
        profile,
        sampling_frequency_index,
        channel_configuration,
        frame_length,
        buffer_fullness,
        raw_data_blocks,
    })
}

/// Occurrences of one violated rule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuleCount {
    /// Frames.
    pub frames: u64,
    /// First frame index.
    pub first_frame: u64,
    /// First message.
    pub example: String,
}

/// ADTS reassembly and per-rule counters of one PID.
#[derive(Debug, Clone, Default)]
pub struct AdtsTrack {
    buf: Vec<u8>,
    started: bool,
    /// Frames checked.
    pub frames: u64,
    /// Bytes in those frames.
    pub bytes: u64,
    /// Samples per channel in those frames.
    pub samples: u64,
    /// Bytes skipped before the first frame (capture started mid-frame).
    pub lead_in: u64,
    /// Resynchronisations after the first frame (loss or bad length).
    pub resyncs: u64,
    /// Header of the first frame.
    pub first: Option<Header>,
    /// Frames whose sampling_frequency_index / channel_configuration
    /// differ from the first.
    pub parameter_changes: u64,
    /// Coded channels without LFE from the first frame's
    /// channel_configuration (`None` for 0, defined by a PCE, and 7).
    pub coded_channels: Option<u32>,
    /// Frames with crc_check.
    pub crc_frames: u64,
    /// adts_buffer_fullness range (excluding 0x7FF).
    pub fullness: Option<(u16, u16)>,
    /// Violated rules.
    pub rules: BTreeMap<Rule, RuleCount>,
}

impl AdtsTrack {
    /// Payload of a PES packet start (after the PES header).
    pub fn push_start(&mut self, data: &[u8]) {
        self.started = true;
        self.push(data);
    }

    /// Continuation payload (ignored before the first PES start).
    pub fn push(&mut self, data: &[u8]) {
        if !self.started {
            return;
        }
        self.buf.extend_from_slice(data);
        self.drain();
    }

    fn drain(&mut self) {
        let mut pos = 0;
        loop {
            let skip_from = pos;
            while pos + 1 < self.buf.len()
                && !(self.buf[pos] == 0xFF && self.buf[pos + 1] & 0xF6 == 0xF0)
            {
                pos += 1;
            }
            if pos > skip_from {
                if self.frames == 0 {
                    self.lead_in += (pos - skip_from) as u64;
                } else {
                    self.resyncs += 1;
                }
            }
            let Some(h) = header(&self.buf[pos..]) else {
                break;
            };
            if h.frame_length < 7 + 2 * usize::from(!h.protection_absent) {
                pos += 1; // not a frame start
                continue;
            }
            // accept a frame only when the next syncword follows it (a
            // false sync or a frame cut by packet loss is skipped)
            let next = pos + h.frame_length;
            if next + 2 > self.buf.len() {
                break;
            }
            if !(self.buf[next] == 0xFF && self.buf[next + 1] & 0xF6 == 0xF0) {
                pos += 1;
                continue;
            }
            let frame = self.buf[pos..pos + h.frame_length].to_vec();
            pos += h.frame_length;
            self.frame(&frame, h);
        }
        self.buf.drain(..pos);
    }

    fn add(&mut self, rule: Rule, example: impl FnOnce() -> String) {
        let n = self.frames - 1;
        self.rules
            .entry(rule)
            .or_insert_with(|| RuleCount {
                frames: 0,
                first_frame: n,
                example: example(),
            })
            .frames += 1;
    }

    fn frame(&mut self, f: &[u8], h: Header) {
        self.frames += 1;
        self.bytes += f.len() as u64;
        self.samples += 1024 * (u64::from(h.raw_data_blocks) + 1);
        match self.first {
            None => {
                self.first = Some(h);
                self.coded_channels = isdbt_mux::adts::buffer_channels(h.channel_configuration);
            }
            Some(a)
                if a.sampling_frequency_index != h.sampling_frequency_index
                    || a.channel_configuration != h.channel_configuration =>
            {
                self.parameter_changes += 1
            }
            _ => {}
        }
        if h.id != 1 {
            self.add(Rule::Id, || "ID '0' (MPEG-4 ADTS)".into());
        }
        if h.protection_absent {
            self.add(Rule::ProtectionAbsent, || {
                "protection_absent '1' (no crc_check)".into()
            });
        } else {
            self.crc_frames += 1;
        }
        if h.profile != 1 {
            self.add(Rule::Profile, || format!("profile {}", h.profile));
        }
        if !b32::TERRESTRIAL_SFI.contains(&h.sampling_frequency_index) {
            self.add(Rule::SamplingFrequency, || {
                format!("sampling_frequency_index {:#x}", h.sampling_frequency_index)
            });
        }
        if h.channel_configuration > 6 {
            self.add(Rule::ChannelConfiguration, || {
                format!("channel_configuration {}", h.channel_configuration)
            });
        }
        if h.buffer_fullness == 0x7FF {
            self.add(Rule::BufferFullness, || {
                "adts_buffer_fullness 0x7FF (variable rate)".into()
            });
        } else {
            let (lo, hi) = self.fullness.unwrap_or((u16::MAX, 0));
            self.fullness = Some((lo.min(h.buffer_fullness), hi.max(h.buffer_fullness)));
        }
        if h.raw_data_blocks != 0 {
            self.add(Rule::RawDataBlocks, || {
                format!("number_of_raw_data_blocks_in_frame {}", h.raw_data_blocks)
            });
        }
    }

    /// Sampling frequency of the first frame (Hz).
    pub fn sampling_frequency(&self) -> Option<u32> {
        self.first.and_then(|h| {
            isdbt_mux::adts::SAMPLING_FREQUENCIES
                .get(h.sampling_frequency_index as usize)
                .copied()
        })
    }

    /// Mean bit rate of the frames (bit/s).
    pub fn bit_rate(&self) -> Option<f64> {
        let fs = self.sampling_frequency()?;
        (self.frames > 0).then(|| b32::bit_rate(self.bytes, self.samples, fs))
    }
}

/// `check` identifier of a rule in `isdb ts validate`.
pub fn check_name(rule: Rule) -> &'static str {
    match rule {
        Rule::Id => "aac.id",
        Rule::Layer => "aac.layer",
        Rule::ProtectionAbsent => "aac.protection_absent",
        Rule::Profile => "aac.profile",
        Rule::SamplingFrequency => "aac.sampling_frequency_index",
        Rule::ChannelConfiguration => "aac.channel_configuration",
        Rule::BufferFullness => "aac.adts_buffer_fullness",
        Rule::RawDataBlocks => "aac.number_of_raw_data_blocks_in_frame",
        Rule::ConstantRate => "aac.constant_rate",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use isdbt_mux::adts::AdtsHeader;

    /// The independent header parse agrees with the multiplexer's encoder
    /// on every field position.
    #[test]
    fn header_fields_match_encoder() {
        let h = AdtsHeader {
            id: 1,
            layer: 0,
            protection_absent: false,
            profile: 2,
            sampling_frequency_index: 0xB,
            private_bit: false,
            channel_configuration: 5,
            original_copy: true,
            home: true,
            copyright_identification_bit: false,
            copyright_identification_start: true,
            aac_frame_length: 0x1234,
            adts_buffer_fullness: 0x5A5,
            number_of_raw_data_blocks_in_frame: 3,
            crc_check: None,
        };
        let x = header(&h.encode_fixed_variable()).unwrap();
        assert_eq!(
            (
                x.id,
                x.layer,
                x.protection_absent,
                x.profile,
                x.sampling_frequency_index,
                x.channel_configuration
            ),
            (1, 0, false, 2, 0xB, 5)
        );
        assert_eq!(
            (x.frame_length, x.buffer_fullness, x.raw_data_blocks),
            (0x1234, 0x5A5, 3)
        );
        // a first header of FFmpeg `aac` encoder output (no CRC, fullness 0x7FF)
        let a = header(&[0xFF, 0xF9, 0x4C, 0x80, 0x36, 0x5F, 0xFC]).unwrap();
        assert_eq!(
            (
                a.id,
                a.protection_absent,
                a.profile,
                a.sampling_frequency_index,
                a.channel_configuration
            ),
            (1, true, 1, 3, 2)
        );
        assert_eq!(
            (a.frame_length, a.buffer_fullness, a.raw_data_blocks),
            (434, 0x7FF, 0)
        );
    }

    #[test]
    fn reassembly_across_pes_and_resync() {
        // frames without crc_check and with 0x7FF: two violated rules each
        let f = |len| {
            let mut s = isdbt_mux::audio::SilentFrame::stereo_48k(len);
            s.header.protection_absent = true;
            s.header.adts_buffer_fullness = 0x7FF;
            s.build()
        };
        let mut es = Vec::new();
        for l in [300, 310, 320, 330] {
            es.extend(f(l));
        }
        let mut t = AdtsTrack::default();
        t.push(&es[..50]); // before any PES start: ignored
        t.push_start(&es[100..400]); // mid-frame start
        t.push(&es[400..]);
        // frames 1 and 2 (frame 3 waits for a following syncword)
        assert_eq!(t.frames, 2);
        assert_eq!(t.lead_in, 200); // 300 - 100
        assert_eq!(t.rules[&Rule::ProtectionAbsent].frames, 2);
        assert_eq!(t.rules[&Rule::BufferFullness].first_frame, 0);
        // frame 0 complete, frame 1 cut by a loss (es[400..700] missing):
        // frame 0 is checked (frame 1's syncword follows), the cut frame 1
        // is skipped by a resync to frame 3 (which waits for a successor)
        let mut t = AdtsTrack::default();
        t.push_start(&es[..400]);
        t.push(&es[700..]);
        assert_eq!((t.frames, t.resyncs), (1, 1));
        assert!(
            t.rules
                .keys()
                .all(|r| matches!(r, Rule::ProtectionAbsent | Rule::BufferFullness))
        );
        assert_eq!(t.coded_channels, Some(2));
    }

    /// Header rules only; coded channels from channel_configuration.
    #[test]
    fn header_rules_and_channels() {
        let es = |edit: &dyn Fn(&mut isdbt_mux::adts::AdtsHeader)| {
            let mut out = Vec::new();
            for _ in 0..3 {
                let mut s = isdbt_mux::audio::SilentFrame::stereo_48k(200);
                edit(&mut s.header);
                out.extend(s.build());
            }
            let mut t = AdtsTrack::default();
            t.push_start(&out);
            t
        };
        let t = es(&|_| {});
        assert_eq!((t.frames, t.crc_frames), (2, 2));
        assert!(t.rules.is_empty(), "{:?}", t.rules);
        // layer ≠ '00' is not taken for a syncword
        assert_eq!(es(&|h| h.layer = 2).frames, 0);
        for (cc, n) in [(1u8, Some(1u32)), (6, Some(5)), (0, None), (7, None)] {
            assert_eq!(
                es(&|h| h.channel_configuration = cc).coded_channels,
                n,
                "cc {cc}"
            );
        }
    }
}
