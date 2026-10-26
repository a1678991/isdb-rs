//! ARIB STD-B32 v3.11 Part 2 restrictions on the ADTS *headers* of MPEG-2
//! AAC streams (§5.2.1–5.2.3), the pass-or-refuse gate of the multiplexer
//! ([`conform`]), the constant-rate adts_buffer_fullness model
//! ([`cbr_fullness`]) and the audio quality modes of Attachment Chapter 3
//! Table 1 ([`QualityMode`]).
//!
//! Every [`Rule`] carries the clause and the quoted text it enforces.
//! Rules that rest on ISO/IEC 13818-7 semantics (not available locally)
//! say so in their clause string.
//!
//! Only header fields are checked.  The restrictions that need the
//! raw_data_block contents (crc_check value, coupling channel elements,
//! element order, PCE fields, SBR fill payloads, LC tool set) are not
//! checked: parsing raw_data_block needs the ISO/IEC 13818-7 Annex A
//! tables, which this crate does not include (SPEC-NOTES "AAC ADTS:
//! raw_data_block not parsed").  For the same reason no header is ever
//! rewritten: the Appendix 1 crc_check covers every header bit, so any
//! header change would need the CRC recomputed over element bits whose
//! boundaries are unknown without that parse.

use std::collections::BTreeMap;
use std::fmt;

use super::{
    AdtsHeader, BUFFER_BITS_PER_CHANNEL, HEADER_BYTES, HeaderError, VBR_FULLNESS, buffer_channels,
};

/// Sampling-frequency indices permitted for digital terrestrial television
/// by B32 Part 2 §5.2.2 (1): 0x3 (48 kHz), 0x4 (44.1 kHz), 0x5 (32 kHz),
/// 0x6 (24 kHz), 0x7 (22.05 kHz), 0x8 (16 kHz).  0x0 (96 kHz) is excluded
/// ("(Note 1) 0x0 (96kHz) can be used only for V-Low multimedia
/// broadcasting by connected segment system"); 0x6–0x8 are excluded only
/// for BS/wide-band CS (Note 2).
pub const TERRESTRIAL_SFI: std::ops::RangeInclusive<u8> = 0x3..=0x8;

/// Sampling-frequency indices at which B32 Part 2 §5.2.3 (4)(a) permits
/// SBR extension payloads in fill elements (24, 22.05, 16 kHz).  SBR is
/// signalled implicitly inside raw_data_block, so a header-only reader
/// cannot tell whether such a stream decodes at fs or 2 × fs.
pub const SBR_SFI: std::ops::RangeInclusive<u8> = 0x6..=0x8;

/// Severity of a violated rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Severity {
    /// The text implies a restriction without stating it as "shall" /
    /// "not permitted".
    Warning,
    /// A "shall" / "not permitted" / fixed value of the text.
    Error,
}

/// One header-level restriction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Rule {
    /// adts_fixed_header ID '1' (MPEG-2).
    Id,
    /// adts_fixed_header layer '00'.
    Layer,
    /// protection_absent '0'.
    ProtectionAbsent,
    /// profile 1 (LC).
    Profile,
    /// sampling_frequency_index in the terrestrial list.
    SamplingFrequency,
    /// At most 5.1 channels (channel_configuration 0–6).
    ChannelConfiguration,
    /// adts_buffer_fullness ≠ 0x7FF.
    BufferFullness,
    /// number_of_raw_data_blocks_in_frame = 0.
    RawDataBlocks,
    /// Every frame fits the 6144-bits-per-channel decoder buffer, so a
    /// constant-rate reservoir model exists (evaluated only when the model
    /// is computed: [`stream_fullness`], [`conform`] refusals).
    ConstantRate,
}

impl Rule {
    /// Every rule.
    pub const ALL: [Rule; 9] = [
        Rule::Id,
        Rule::Layer,
        Rule::ProtectionAbsent,
        Rule::Profile,
        Rule::SamplingFrequency,
        Rule::ChannelConfiguration,
        Rule::BufferFullness,
        Rule::RawDataBlocks,
        Rule::ConstantRate,
    ];

    /// Short identifier (`aac.<name>` in `isdb ts validate`).
    pub fn name(self) -> &'static str {
        match self {
            Rule::Id => "id",
            Rule::Layer => "layer",
            Rule::ProtectionAbsent => "protection_absent",
            Rule::Profile => "profile",
            Rule::SamplingFrequency => "sampling_frequency_index",
            Rule::ChannelConfiguration => "channel_configuration",
            Rule::BufferFullness => "adts_buffer_fullness",
            Rule::RawDataBlocks => "number_of_raw_data_blocks_in_frame",
            Rule::ConstantRate => "constant_rate",
        }
    }

    /// Clause (ARIB STD-B32 v3.11 Part 2 unless stated).
    pub fn clause(self) -> &'static str {
        match self {
            Rule::Id => {
                "B32 Part 2 §4.1 / §5.2 (ADTS of ISO/IEC 13818-7: ID '1'; 13818-7 from knowledge)"
            }
            Rule::Layer => {
                "B32 Part 2 §4.1 (ADTS of ISO/IEC 13818-7: layer '00'; 13818-7 from knowledge)"
            }
            Rule::ProtectionAbsent => "B32 Part 2 §5.2.2 (1)",
            Rule::Profile => "B32 Part 2 §5.2.1, §5.2.2 (1)",
            Rule::SamplingFrequency => "B32 Part 2 §5.2.2 (1)",
            Rule::ChannelConfiguration => "B32 Part 2 §5.2.1, §5.2.3 (2)",
            Rule::BufferFullness => "B32 Part 2 §5.2.2 (2)",
            Rule::RawDataBlocks => "B32 Part 2 §5.2.2 (2)",
            Rule::ConstantRate => "B32 Part 2 §5.2.2 (2), Description 1 (3), Appendix 3 (1)(a)",
        }
    }

    /// The text the rule enforces.
    pub fn quote(self) -> &'static str {
        match self {
            Rule::Id => {
                "ADTS fixed header \"consists of synchronization and audio coded information defined in ISO/IEC 13818-7\" (13818-7: ID \"set to '1'\")"
            }
            Rule::Layer => {
                "ADTS fixed header \"consists of synchronization and audio coded information defined in ISO/IEC 13818-7\" (13818-7: layer '00')"
            }
            Rule::ProtectionAbsent => "protection_absent ‘0’ (CRC error check is always presented)",
            Rule::Profile => "Profile: Low Complexity (LC) profile; profile 1 (LC profile)",
            Rule::SamplingFrequency => {
                "sampling_frequency_index 0x0 (96kHz) (Note 1), 0x3 (48kHz), 0x4 (44.1kHz), 0x5 (32kHz), 0x6 (24kHz), 0x7 (22.05kHz), 0x8 (16kHz); Note 1: 0x0 only for V-Low multimedia broadcasting"
            }
            Rule::ChannelConfiguration => "Max. number of coded channels: 5.1 channels per ADTS",
            Rule::BufferFullness => {
                "adts_buffer_fullness: Use of 0x7FF (indicating variable rate) is not permitted."
            }
            Rule::RawDataBlocks => {
                "number_of_raw_data_blocks_in_frame: 0 (number of raw_data_blocks per frame = 1)"
            }
            Rule::ConstantRate => {
                "To maintain the average bitrate fixed, 0x7FF (indicating variable bitrate) is prohibited for use as adts_buffer_fullness value; adts_buffer_fullness is the \"amount of remained equivalent buffer per channel (6,144 bits per channel)\""
            }
        }
    }

    /// Severity (every header rule is a "shall" / fixed value).
    pub fn severity(self) -> Severity {
        Severity::Error
    }
}

impl fmt::Display for Rule {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// One violated rule in one frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Violation {
    /// Rule.
    pub rule: Rule,
    /// What was found.
    pub message: String,
}

/// The checks of one frame.
#[derive(Debug, Clone)]
pub struct FrameCheck {
    /// Parsed header.
    pub header: AdtsHeader,
    /// Violations.
    pub violations: Vec<Violation>,
}

fn v(rule: Rule, message: String) -> Violation {
    Violation { rule, message }
}

/// Check the header of one frame (only the headers are read; the
/// raw_data_block is not walked).
pub fn check_frame(frame: &[u8]) -> Result<FrameCheck, HeaderError> {
    let h = AdtsHeader::parse(frame)?;
    let mut out = Vec::new();
    if h.id != 1 {
        out.push(v(Rule::Id, "ID '0' (MPEG-4 ADTS)".into()));
    }
    if h.layer != 0 {
        out.push(v(
            Rule::Layer,
            format!("layer '{:02b}', shall be '00'", h.layer),
        ));
    }
    if h.protection_absent {
        out.push(v(
            Rule::ProtectionAbsent,
            "protection_absent '1' (no crc_check)".into(),
        ));
    }
    if h.profile != 1 {
        let name = ["Main", "LC", "SSR", "reserved"][h.profile as usize];
        out.push(v(Rule::Profile, format!("profile {} ({name})", h.profile)));
    }
    if !TERRESTRIAL_SFI.contains(&h.sampling_frequency_index) {
        let hz = h
            .sampling_frequency()
            .map_or("reserved".to_string(), |f| format!("{f} Hz"));
        out.push(v(
            Rule::SamplingFrequency,
            format!(
                "sampling_frequency_index {:#x} ({hz}) not permitted for terrestrial television",
                h.sampling_frequency_index
            ),
        ));
    }
    if h.channel_configuration > 6 {
        out.push(v(
            Rule::ChannelConfiguration,
            format!(
                "channel_configuration {} (more than 5.1 channels)",
                h.channel_configuration
            ),
        ));
    }
    if h.adts_buffer_fullness == VBR_FULLNESS {
        out.push(v(
            Rule::BufferFullness,
            "adts_buffer_fullness 0x7FF (variable rate)".into(),
        ));
    }
    if h.number_of_raw_data_blocks_in_frame != 0 {
        out.push(v(
            Rule::RawDataBlocks,
            format!(
                "number_of_raw_data_blocks_in_frame {} ({} raw_data_blocks)",
                h.number_of_raw_data_blocks_in_frame,
                h.number_of_raw_data_blocks_in_frame + 1
            ),
        ));
    }
    Ok(FrameCheck {
        header: h,
        violations: out,
    })
}

/// Occurrences of one rule over a stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tally {
    /// Frames violating it.
    pub frames: usize,
    /// First frame index.
    pub first_frame: usize,
    /// First message.
    pub example: String,
}

/// Per-rule summary of a stream.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Report {
    /// Frames checked.
    pub frames: usize,
    /// Violated rules.
    pub rules: BTreeMap<Rule, Tally>,
}

impl Report {
    /// Add a frame's violations.
    pub fn add(&mut self, frame: usize, vs: &[Violation]) {
        for x in vs {
            self.rules
                .entry(x.rule)
                .or_insert_with(|| Tally {
                    frames: 0,
                    first_frame: frame,
                    example: x.message.clone(),
                })
                .frames += 1;
        }
    }

    /// Violated error-severity rules (each one makes [`conform`] refuse).
    pub fn blocking(&self) -> impl Iterator<Item = (&Rule, &Tally)> {
        self.rules
            .iter()
            .filter(|(r, _)| r.severity() == Severity::Error)
    }

    /// One line per rule: `<severity>: <name> (<clause>): N/M frames,
    /// first #i: …`.
    pub fn lines(&self) -> Vec<String> {
        self.rules
            .iter()
            .map(|(r, t)| {
                format!(
                    "{}: {} ({}): {}/{} frames, first #{}: {}",
                    match r.severity() {
                        Severity::Error => "error",
                        Severity::Warning => "warning",
                    },
                    r.name(),
                    r.clause(),
                    t.frames,
                    self.frames,
                    t.first_frame,
                    t.example,
                )
            })
            .collect()
    }
}

/// Split an ADTS byte stream into frames (offset, length).  The stream
/// must start with a syncword; a truncated last frame is dropped.
pub fn split_frames(data: &[u8]) -> Result<Vec<(usize, usize)>, String> {
    let mut out = Vec::new();
    let mut pos = 0;
    while pos + HEADER_BYTES <= data.len() {
        let h = AdtsHeader::parse(&data[pos..]).map_err(|e| format!("{e} at byte {pos}"))?;
        let len = h.aac_frame_length as usize;
        if len < h.header_len() {
            return Err(format!("aac_frame_length {len} at byte {pos}"));
        }
        if pos + len > data.len() {
            if out.is_empty() {
                return Err(format!(
                    "aac_frame_length {len} at byte {pos} beyond the end"
                ));
            }
            break;
        }
        out.push((pos, len));
        pos += len;
    }
    if out.is_empty() {
        return Err("no ADTS frames".into());
    }
    Ok(out)
}

/// Check the header of every frame of an ADTS stream.
pub fn check_stream(data: &[u8]) -> Result<(Report, Vec<FrameCheck>), String> {
    let frames = split_frames(data)?;
    let mut rep = Report {
        frames: frames.len(),
        ..Default::default()
    };
    let mut checks = Vec::with_capacity(frames.len());
    for (i, &(o, l)) in frames.iter().enumerate() {
        let c = check_frame(&data[o..o + l]).map_err(|e| format!("frame {i}: {e}"))?;
        rep.add(i, &c.violations);
        checks.push(c);
    }
    Ok((rep, checks))
}

/// Result of the constant-rate decoder-buffer model.
#[derive(Debug, Clone, PartialEq)]
pub struct FullnessModel {
    /// The constant rate of the model, bits per frame (≥ the mean).
    pub rate_frame_bits: u64,
    /// Mean frame length of the stream (bits).
    pub mean_frame_bits: f64,
    /// Channels with a decoder buffer (NCC; SCE 1, CPE 2, LFE 0; from
    /// channel_configuration, [`super::buffer_channels`]).
    pub channels: u32,
    /// Largest reservoir the decoder buffer allows:
    /// 6144 × NCC − rate_frame_bits.
    pub max_reservoir_bits: u64,
    /// Bits per pass a constant-rate encoder would have spent on fill
    /// because its reservoir was full (not present in the stream; 0 when
    /// the stream is constant-rate at `rate_frame_bits`).
    pub clamped_bits_per_pass: u64,
    /// Reservoir after each frame (bits).
    pub reservoir: Vec<u64>,
    /// adts_buffer_fullness per frame.
    pub values: Vec<u16>,
}

/// The periodic reservoir trajectory at `rate` bits/frame: start full,
/// `r ← min(r + rate − bits, max)` per frame, repeat passes until the
/// state at the start of a pass repeats.  `None` if `r` goes negative
/// (the decoder buffer would underflow).
fn reservoir_orbit(frame_bits: &[u64], rate: u64, max: u64) -> Option<(Vec<u64>, u64)> {
    let mut start = max;
    for _ in 0..10_000 {
        let mut r = i128::from(start);
        let mut out = Vec::with_capacity(frame_bits.len());
        let mut clamped = 0u64;
        for &b in frame_bits {
            r += i128::from(rate) - i128::from(b);
            if r < 0 {
                return None;
            }
            if r > i128::from(max) {
                clamped += (r - i128::from(max)) as u64;
                r = i128::from(max);
            }
            out.push(r as u64);
        }
        if r as u64 == start {
            return Some((out, clamped));
        }
        start = r as u64;
    }
    None
}

/// adts_buffer_fullness of every frame from a constant-bit-rate
/// bit-reservoir model.
///
/// Definition used (B32 Part 2 Appendix 3 (1)(a) quoting ISO/IEC
/// 13818-7:2003: adts_buffer_fullness is the "amount of remained equivalent
/// buffer per channel (6,144 bits per channel)"; the recurrence and the
/// scaling are ISO/IEC 13818-7 from knowledge): with a constant rate of
/// `R` bits per frame, the encoder's bit reservoir after frame *n* is
/// `r[n] = r[n−1] + R − bits[n]` (`bits` = the whole ADTS frame),
/// `0 ≤ r[n] ≤ 6144·NCC − R` (a constant-rate encoder spends what would
/// exceed the maximum on fill), and the field carries `⌊r[n] / (32·NCC)⌋`.
/// Confirmed on live data (SPEC-NOTES "AAC ADTS"): the live full-seg and
/// one-seg encoders follow exactly this recurrence (to the truncation)
/// with R = 4096 / 2048 bits and reach the maximum (12288 − 4096)/64 = 128
/// and (12288 − 2048)/64 = 160.
///
/// `R` is the smallest integer rate ≥ the stream's mean frame length for
/// which the reservoir never goes negative (the decoder buffer never
/// underflows); for a stream that is constant-rate at its mean (the live
/// encoders) that is the mean and nothing is clamped.  A variable-rate
/// stream (e.g. an ABR encode) needs `R` somewhat above its mean; the
/// fill such an encoder would have added is not part of the stream, so the
/// delivered rate stays the mean.
/// The trajectory is the periodic one (start full, repeat the pass until
/// it repeats), so the values continue seamlessly when the file loops.
/// Fails only if a single frame exceeds the 6144·NCC-bit buffer.
pub fn cbr_fullness(frame_bits: &[u64], channels: u32) -> Result<FullnessModel, String> {
    let n = frame_bits.len() as u64;
    if n == 0 || channels == 0 {
        return Err("no frames / no channels".into());
    }
    let total: u64 = frame_bits.iter().sum();
    let buffer = u64::from(BUFFER_BITS_PER_CHANNEL * channels);
    let largest = *frame_bits.iter().max().unwrap_or(&0);
    if largest > buffer {
        return Err(format!(
            "a frame of {largest} bits exceeds the decoder buffer of 6144 × {channels} channels = {buffer} bits"
        ));
    }
    let feasible = |rate: u64| {
        if rate > buffer {
            return None;
        }
        reservoir_orbit(frame_bits, rate, buffer - rate)
    };
    // the largest frame is always feasible (r never decreases)
    let (mut lo, mut hi) = (total.div_ceil(n), largest.max(total.div_ceil(n)));
    if feasible(lo).is_some() {
        hi = lo;
    }
    while lo < hi {
        let mid = (lo + hi) / 2;
        if feasible(mid).is_some() {
            hi = mid;
        } else {
            lo = mid + 1;
        }
    }
    let rate = hi;
    let (reservoir, clamped) = feasible(rate).ok_or_else(|| {
        format!("no constant rate up to the largest frame ({largest} bits) keeps the decoder buffer from underflowing")
    })?;
    let div = 32 * u64::from(channels);
    let values = reservoir
        .iter()
        .map(|&r| {
            let f = r / div;
            debug_assert!(f < u64::from(VBR_FULLNESS));
            f as u16
        })
        .collect();
    Ok(FullnessModel {
        rate_frame_bits: rate,
        mean_frame_bits: total as f64 / n as f64,
        channels,
        max_reservoir_bits: buffer - rate,
        clamped_bits_per_pass: clamped,
        reservoir,
        values,
    })
}

/// The [`cbr_fullness`] model of a whole ADTS stream, from its headers
/// only: buffer channels from the first frame's channel_configuration
/// ([`super::buffer_channels`]; 0 and 7 cannot be determined without a
/// PCE / are outside B32 and fail), and frame lengths as they are *with*
/// crc_check (a frame without one counts 2 bytes more, the length it
/// would have once re-encoded with protection_absent '0').  These are the
/// adts_buffer_fullness values a constant-rate encoder would have written;
/// the stream is not changed.
pub fn stream_fullness(data: &[u8]) -> Result<FullnessModel, String> {
    let frames = split_frames(data)?;
    let first = AdtsHeader::parse(data).map_err(|e| e.to_string())?;
    let channels = buffer_channels(first.channel_configuration).ok_or_else(|| {
        format!(
            "channel_configuration {}: the buffer channels {}",
            first.channel_configuration,
            if first.channel_configuration == 0 {
                "are defined by a program_config_element, which is not read (no raw_data_block walk)"
            } else {
                "are not defined for more than 5.1 channels"
            }
        )
    })?;
    let mut bits = Vec::with_capacity(frames.len());
    for &(o, l) in &frames {
        let h = AdtsHeader::parse(&data[o..]).map_err(|e| format!("{e} at byte {o}"))?;
        let protected_len = l - h.header_len() + HEADER_BYTES + 2;
        bits.push(protected_len as u64 * 8);
    }
    cbr_fullness(&bits, channels)
}

impl FullnessModel {
    /// One line describing the model.
    pub fn describe(&self) -> String {
        format!(
            "constant-rate reservoir model: R = {} bits/frame (mean {:.1}), {} buffer channels, reservoir ≤ {} bits, fill a CBR encoder would add {} bits/pass, adts_buffer_fullness {}..={}",
            self.rate_frame_bits,
            self.mean_frame_bits,
            self.channels,
            self.max_reservoir_bits,
            self.clamped_bits_per_pass,
            self.values.iter().min().unwrap_or(&0),
            self.values.iter().max().unwrap_or(&0)
        )
    }
}

/// What [`conform`] produced.
#[derive(Debug, Clone)]
pub struct Conformed {
    /// The stream, byte-identical to the input.
    pub data: Vec<u8>,
    /// Header checks of the input (no violation).
    pub input: Report,
}

/// Why [`conform`] refused.
#[derive(Debug, Clone)]
pub struct Refusal {
    /// Violations of the input.
    pub input: Report,
    /// Explanation.
    pub reason: String,
    /// The [`stream_fullness`] model, when adts_buffer_fullness is 0x7FF
    /// and the model could be computed: the values a constant-rate encoder
    /// would have written (informative; nothing is rewritten).
    pub fullness: Option<Box<FullnessModel>>,
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "{}", self.reason)?;
        for l in self.input.lines() {
            writeln!(f, "  {l}")?;
        }
        if let Some(m) = &self.fullness {
            writeln!(f, "  for reference, {}", m.describe())?;
        }
        write!(f, "  {REENCODE_HINT}")
    }
}

/// What an input needs to pass [`conform`] (B32 Part 2 §5.2.1–5.2.2).
pub const REENCODE_HINT: &str = "re-encode to MPEG-2 AAC LC (ADTS ID '1'), 48 kHz, ≤ 5.1 channels, constant bit rate, \
     with an encoder that writes the ADTS crc_check (protection_absent '0') and a constant-rate \
     adts_buffer_fullness (not 0x7FF) itself; this crate cannot insert crc_check (it needs the \
     ISO/IEC 13818-7 tables, not included) and does not rewrite headers of protected frames";

/// The multiplexer's ARIB STD-B32 Part 2 §5.2 gate: an ADTS stream whose
/// headers satisfy every [`Rule`] is returned byte-identically; anything
/// else is refused with the per-rule report and an explanation.
///
/// Nothing is rewritten, because nothing can be rewritten correctly
/// without walking raw_data_block (Appendix 1: crc_check covers "all bits
/// of the headers" and element bits whose boundaries need the ISO/IEC
/// 13818-7 Annex A tables, not included):
///
/// * protection_absent '1': crc_check cannot be inserted;
/// * protected frames with adts_buffer_fullness 0x7FF or ID '0': changing
///   the header would invalidate the transmitted crc_check, which cannot
///   be recomputed.  For 0x7FF the refusal carries the [`stream_fullness`]
///   model (the values a constant-rate encoder would have written);
/// * every other violation needs re-encoding anyway.
pub fn conform(data: &[u8]) -> Result<Conformed, Refusal> {
    let refuse = |input: Report, reason: String| Refusal {
        input,
        reason,
        fullness: None,
    };
    let (mut input, _) = check_stream(data).map_err(|e| refuse(Report::default(), e))?;
    if input.blocking().next().is_none() {
        return Ok(Conformed {
            data: data.to_vec(),
            input,
        });
    }
    let has = |r: Rule| input.rules.contains_key(&r);
    let header_only = [Rule::ProtectionAbsent, Rule::BufferFullness, Rule::Id];
    let only_header_fixes = input.rules.keys().all(|r| header_only.contains(r));
    let mut reason = String::from("the ADTS stream violates ARIB STD-B32 Part 2 §5.2");
    if has(Rule::ProtectionAbsent) {
        reason.push_str(
            "; protection_absent '1': crc_check cannot be inserted, because its coverage (B32 Part 2 Appendix 1: header bits plus the first 192 / 128 bits of each SCE, CPE, CCE, LFE and all of PCE / DSE) needs the raw_data_block element boundaries, i.e. the ISO/IEC 13818-7 Huffman and scalefactor-band tables, which are not included",
        );
    }
    // some frame carries a crc_check
    let protected = input
        .rules
        .get(&Rule::ProtectionAbsent)
        .is_none_or(|t| t.frames < input.frames);
    if protected && (has(Rule::BufferFullness) || has(Rule::Id)) {
        reason.push_str(
            "; rewriting adts_buffer_fullness / ID of a frame with crc_check would invalidate the crc_check (Appendix 1 covers all header bits), and recomputing it needs the same element boundaries",
        );
    }
    if !only_header_fixes {
        reason.push_str("; the other violations need re-encoding in any case");
    }
    reason.push(':');
    let mut fullness = None;
    if has(Rule::BufferFullness) {
        match stream_fullness(data) {
            Ok(m) => fullness = Some(Box::new(m)),
            Err(e) => {
                input.rules.insert(
                    Rule::ConstantRate,
                    Tally {
                        frames: input.frames,
                        first_frame: 0,
                        example: e,
                    },
                );
            }
        }
    }
    Err(Refusal {
        input,
        reason,
        fullness,
    })
}

/// Audio quality mode of the audio component descriptor's
/// quality_indicator (B10 Table 6-44, defined by B32 Part 2 Attachment
/// Chapter 3 Table 1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum QualityMode {
    /// '01' Mode 1: "Audio quality equivalent to B mode available in BS
    /// analog television broadcasting"; reference bitrate "192 to 256
    /// kbps/stereo or more".
    Mode1,
    /// '10' Mode 2: "Audio quality (Note 3) other than mode 1 that is not
    /// classified in mode 3"; Note 3: ITU-R broadcast quality, reference
    /// bitrate "144 kbps/stereo or more".
    Mode2,
    /// '11' Mode 3: "Mode with limited audio quality compared to modes 1
    /// and 2" ("ex. Sampling frequency below 32 kHz").
    Mode3,
}

/// Mode 1 is unambiguous from this rate per stereo pair (upper end of
/// Table 1's "192 to 256 kbps/stereo or more").
pub const MODE1_SURE_BPS: f64 = 256_000.0;
/// Lower end of the Mode 1 reference range.
pub const MODE1_MIN_BPS: f64 = 192_000.0;
/// Mode 2 reference bitrate (Table 1 Note 3).
pub const MODE2_MIN_BPS: f64 = 144_000.0;

impl QualityMode {
    /// The 2-bit quality_indicator.
    pub fn code(self) -> u8 {
        match self {
            QualityMode::Mode1 => 0b01,
            QualityMode::Mode2 => 0b10,
            QualityMode::Mode3 => 0b11,
        }
    }

    /// From the 2-bit code ('00' is reserved).
    pub fn from_code(c: u8) -> Option<Self> {
        match c & 3 {
            0b01 => Some(QualityMode::Mode1),
            0b10 => Some(QualityMode::Mode2),
            0b11 => Some(QualityMode::Mode3),
            _ => None,
        }
    }

    /// "Mode 1" …
    pub fn name(self) -> &'static str {
        match self {
            QualityMode::Mode1 => "Mode 1",
            QualityMode::Mode2 => "Mode 2",
            QualityMode::Mode3 => "Mode 3",
        }
    }

    /// The mode Table 1 assigns, conservatively, to a stream:
    /// sampling frequency below 32 kHz → Mode 3 (the table's example);
    /// rate per stereo pair ≥ 256 kbit/s → Mode 1 (the whole "192 to 256
    /// kbps/stereo" range is reached); ≥ 144 kbit/s → Mode 2 (Note 3's
    /// ITU-R reference); below → Mode 3.  `bps` is the ADTS stream rate
    /// (whole frames) and `channels` the coded channels without LFE; the
    /// rate per stereo pair is `bps × 2 / channels`.  The table's
    /// bitrates are "for reference purposes" — an explicit mode overrides
    /// this.
    pub fn derive(sampling_frequency: u32, bps: f64, channels: u32) -> Self {
        let per_pair = per_stereo_pair(bps, channels);
        if sampling_frequency < 32_000 {
            QualityMode::Mode3
        } else if per_pair >= MODE1_SURE_BPS {
            QualityMode::Mode1
        } else if per_pair >= MODE2_MIN_BPS {
            QualityMode::Mode2
        } else {
            QualityMode::Mode3
        }
    }

    /// Why `self`, signalled for such a stream, contradicts Table 1
    /// (`None` if it does not): Mode 1 below the 192 kbit/s/stereo
    /// reference range, Mode 2 below the 144 kbit/s/stereo reference, or
    /// Mode 1/2 with a sampling frequency below 32 kHz.
    pub fn contradiction(self, sampling_frequency: u32, bps: f64, channels: u32) -> Option<String> {
        let pp = per_stereo_pair(bps, channels) / 1000.0;
        match self {
            QualityMode::Mode1 | QualityMode::Mode2 if sampling_frequency < 32_000 => {
                Some(format!(
                    "{} signalled at a {sampling_frequency} Hz sampling frequency (Table 1 cites \"Sampling frequency below 32 kHz\" as Mode 3)",
                    self.name()
                ))
            }
            QualityMode::Mode1 if pp < MODE1_MIN_BPS / 1000.0 => Some(format!(
                "Mode 1 signalled at {pp:.1} kbit/s per stereo pair, below Table 1's \"192 to 256 kbps/stereo or more\""
            )),
            QualityMode::Mode2 if pp < MODE2_MIN_BPS / 1000.0 => Some(format!(
                "Mode 2 signalled at {pp:.1} kbit/s per stereo pair, below Note 3's \"144 kbps/stereo or more\""
            )),
            _ => None,
        }
    }
}

/// Bit rate normalised to a stereo pair.
pub fn per_stereo_pair(bps: f64, channels: u32) -> f64 {
    bps * 2.0 / f64::from(channels.max(1))
}

/// ADTS stream bit rate: Σ frame bits × fs / samples.
pub fn bit_rate(total_bytes: u64, total_samples: u64, sampling_frequency: u32) -> f64 {
    if total_samples == 0 {
        return 0.0;
    }
    total_bytes as f64 * 8.0 * f64::from(sampling_frequency) / total_samples as f64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quality_derivation_table_1() {
        // an encoder at ≈ 189 kbit/s stereo 48 kHz → Mode 2
        assert_eq!(
            QualityMode::derive(48_000, 189_100.0, 2),
            QualityMode::Mode2
        );
        // live broadcasters 192 kbit/s stereo → Mode 2 (and Mode 2 is not contradicted)
        assert_eq!(
            QualityMode::derive(48_000, 192_000.0, 2),
            QualityMode::Mode2
        );
        assert_eq!(QualityMode::Mode2.contradiction(48_000, 192_000.0, 2), None);
        assert_eq!(QualityMode::Mode1.contradiction(48_000, 192_000.0, 2), None);
        assert_eq!(
            QualityMode::derive(48_000, 256_000.0, 2),
            QualityMode::Mode1
        );
        assert_eq!(
            QualityMode::derive(48_000, 128_000.0, 2),
            QualityMode::Mode3
        );
        assert_eq!(
            QualityMode::derive(24_000, 300_000.0, 2),
            QualityMode::Mode3
        );
        // 5.0 channels at 640 kbit/s = 256 per pair
        assert_eq!(
            QualityMode::derive(48_000, 640_000.0, 5),
            QualityMode::Mode1
        );
        assert!(
            QualityMode::Mode1
                .contradiction(48_000, 189_000.0, 2)
                .is_some()
        );
        assert!(
            QualityMode::Mode2
                .contradiction(48_000, 120_000.0, 2)
                .is_some()
        );
        assert!(
            QualityMode::Mode2
                .contradiction(24_000, 48_000.0, 2)
                .is_some()
        );
        assert_eq!(QualityMode::Mode3.contradiction(24_000, 48_000.0, 2), None);
        for m in [QualityMode::Mode1, QualityMode::Mode2, QualityMode::Mode3] {
            assert_eq!(QualityMode::from_code(m.code()), Some(m));
        }
        assert_eq!(QualityMode::Mode2.code(), 0b10);
        assert_eq!(QualityMode::from_code(0), None);
    }

    #[test]
    fn fullness_model_recurrence() {
        // constant 4096-bit frames (192 kbit/s stereo): R = 4096, the
        // reservoir stays full: (12288 − 4096) / 64 = 128, as live broadcasters
        let m = cbr_fullness(&[4096; 10], 2).unwrap();
        assert_eq!(m.rate_frame_bits, 4096);
        assert_eq!(m.max_reservoir_bits, 12288 - 4096);
        assert_eq!(m.values, vec![128; 10]);
        assert_eq!(m.clamped_bits_per_pass, 0);
        // alternating 3072 / 5120 (mean 4096): periodic 8192 / 7168
        let m = cbr_fullness(&[3072, 5120, 3072, 5120], 2).unwrap();
        assert_eq!(m.rate_frame_bits, 4096);
        assert_eq!(m.reservoir, vec![8192, 7168, 8192, 7168]);
        assert_eq!(m.values, vec![128, 112, 128, 112]);
        // variable rate: ten 3000-bit frames then two 9000-bit frames
        // (mean 4000) underflow at R = 4000; the smallest feasible R
        // satisfies (12288 − R) + 2R − 18000 = 0 → R = 5712, ending empty
        let mut bits = vec![3000u64; 10];
        bits.extend([9000, 9000]);
        let m = cbr_fullness(&bits, 2).unwrap();
        assert_eq!(m.rate_frame_bits, 5712);
        assert_eq!(*m.reservoir.last().unwrap(), 0);
        assert!(m.clamped_bits_per_pass > 0);
        // a single frame larger than the buffer
        let mut bits = vec![1000u64; 50];
        bits.push(13_000);
        assert!(cbr_fullness(&bits, 2).is_err());
        // never 0x7FF, never above (6144·NCC − R) / (32·NCC)
        let dev = [300i64, -200, 100, -400, 250, -50, 0, 0, 1200, -1200];
        let bits: Vec<u64> = (0..400)
            .map(|i| (4096 + dev[i % dev.len()] + (i % 7) as i64) as u64)
            .collect();
        let m = cbr_fullness(&bits, 2).unwrap();
        let cap = ((12288 - m.rate_frame_bits) / 64) as u16;
        assert!(m.values.iter().all(|&f| f != VBR_FULLNESS && f <= cap));
    }
}
