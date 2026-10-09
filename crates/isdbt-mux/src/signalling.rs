//! Typed configuration of the signalling values receivers act on: the video
//! decode control descriptor (B10 §6.2.30), the EIT component descriptors
//! (B10 §6.2.3 / §6.2.26), the digital copy control descriptor (B10
//! §6.2.23, user_defined per Annex F) and the remote control key of the TS
//! information descriptor (B10 §6.2.42).
//!
//! Every value is a named variant of an `isdb_si::values` enum whose code is
//! the one printed in the cited table; numbers are rejected with an error
//! naming the variants.  Raw bytes remain possible through the experiment
//! escape hatches (`descriptors`, `add_descriptors`, `byte_patches`), and
//! [`crate::config::Config::validate`] rejects a raw descriptor that
//! duplicates an active typed one.

use std::fmt;
use std::marker::PhantomData;

use isdb_si::descriptor::{ComponentCopyControl, DigitalCopyControl, VideoDecodeControl};
use isdb_si::values::{
    ApsControlData, AudioComponentType, AudioForDisabilities, AudioMode, CodedValue,
    CopyControlType, DigitalRecordingControl, MaximumBitrate, TransferCharacteristics,
    VideoComponentType, VideoEncodeFormat,
};
use serde::de::{self, Deserializer, MapAccess, Visitor};
use serde::{Deserialize, Serialize, Serializer};

use crate::adts::b32::QualityMode;
use crate::error::{MuxError, Result};

// ------------------------------------------------------------ named values

/// A code as the spec tables print it: bits for fields up to 5 bits
/// (`'10'`), hex otherwise (`0xb3`).
fn show_code<T: CodedValue>(c: u8) -> String {
    if T::BITS <= 5 {
        format!("'{:0w$b}'", c, w = T::BITS as usize)
    } else {
        format!("{c:#04x}")
    }
}

/// The variant list of `T` for error messages: `"copy-free" ('00'), …`.
pub fn variant_list<T: CodedValue>() -> String {
    T::NAMED
        .iter()
        .map(|v| format!("\"{}\" ({})", v.name(), show_code::<T>(v.code())))
        .collect::<Vec<_>>()
        .join(", ")
}

struct NamedVisitor<T>(PhantomData<T>);

impl<T: CodedValue> NamedVisitor<T> {
    fn number<E: de::Error>(v: i128) -> E {
        let hint = u8::try_from(v)
            .ok()
            .filter(|&c| u32::from(c) < (1u32 << T::BITS))
            .map(|c| {
                let x = T::from_code(c);
                if x.name() == "reserved" {
                    format!("; {} is reserved in {}", show_code::<T>(c), T::TABLE)
                } else {
                    format!("; {v} = {} is \"{}\"", show_code::<T>(c), x.name())
                }
            })
            .unwrap_or_default();
        E::custom(format!(
            "{} ({}) takes a named value, not a number{hint}; one of: {}",
            T::FIELD,
            T::TABLE,
            variant_list::<T>()
        ))
    }
}

impl<'de, T: CodedValue> Visitor<'de> for NamedVisitor<T> {
    type Value = T;
    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "one of {}", variant_list::<T>())
    }
    fn visit_str<E: de::Error>(self, s: &str) -> std::result::Result<T, E> {
        T::from_name(s).ok_or_else(|| {
            E::custom(format!(
                "unknown {} {s:?} ({}); one of: {}",
                T::FIELD,
                T::TABLE,
                variant_list::<T>()
            ))
        })
    }
    fn visit_i64<E: de::Error>(self, v: i64) -> std::result::Result<T, E> {
        Err(Self::number(v as i128))
    }
    fn visit_u64<E: de::Error>(self, v: u64) -> std::result::Result<T, E> {
        Err(Self::number(v as i128))
    }
}

/// serde newtype carrying a named [`CodedValue`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Named<T>(T);

impl<'de, T: CodedValue> Deserialize<'de> for Named<T> {
    fn deserialize<D: Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        d.deserialize_any(NamedVisitor(PhantomData)).map(Named)
    }
}

impl<T: CodedValue> Serialize for Named<T> {
    fn serialize<S: Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        if self.0.name() == "reserved" {
            return Err(serde::ser::Error::custom(format!(
                "{} {:#x} is reserved and has no name",
                T::FIELD,
                self.0.code()
            )));
        }
        s.serialize_str(self.0.name())
    }
}

/// `#[serde(with = "named")]` for a required [`CodedValue`] field.
pub mod named {
    use super::*;
    /// Serialize by name.
    pub fn serialize<T: CodedValue, S: Serializer>(
        v: &T,
        s: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        Named(*v).serialize(s)
    }
    /// Deserialize by name.
    pub fn deserialize<'de, T: CodedValue, D: Deserializer<'de>>(
        d: D,
    ) -> std::result::Result<T, D::Error> {
        Named::<T>::deserialize(d).map(|n| n.0)
    }
}

/// `#[serde(default, with = "named_opt")]` for an optional [`CodedValue`].
pub mod named_opt {
    use super::*;
    /// Serialize by name.
    pub fn serialize<T: CodedValue, S: Serializer>(
        v: &Option<T>,
        s: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        v.map(Named).serialize(s)
    }
    /// Deserialize by name.
    pub fn deserialize<'de, T: CodedValue, D: Deserializer<'de>>(
        d: D,
    ) -> std::result::Result<Option<T>, D::Error> {
        Ok(Option::<Named<T>>::deserialize(d)?.map(|n| n.0))
    }
}

// ------------------------------------------------- video decode control

/// How the video decode control descriptor is produced.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum VdcMode {
    /// Derived from the elementary stream (see [`VideoDecodeControlCfg`]);
    /// fields given in the table override the derived ones.
    #[default]
    Derived,
    /// Every field from the configuration; nothing derived.
    Explicit,
    /// No descriptor.
    Off,
}

/// video_decode_control_descriptor (tag 0xC8) in the PMT ES loop — ARIB
/// STD-B10 Part 2 §6.2.30, Table 6-59.
///
/// `mode = "derived"` (default) takes, for an `mpeg2-video` stream:
/// * `still_picture` = false — the ES is coded moving video (I/P/B GOPs),
///   not an "MPEG-I picture" still-picture component;
/// * `sequence_end_code` = whether the ES contains a sequence_end_code
///   (0x000001B7, H.262 Table 6-1);
/// * `video_encode_format` = from vertical_size and progressive_sequence
///   ([`crate::video::SequenceInfo::video_encode_format`], Table 6-60);
/// * `transfer_characteristics` = "bt709" ('00', Table 6-60-1: VUI 1/11/14)
///   — the H.262 default when no sequence_display_extension says otherwise
///   (H.262 §6.3.6, from knowledge; the extension is not parsed).
///
/// Any field given explicitly replaces the derived one.  For a `declared`
/// stream nothing can be derived, so `derived` emits nothing; `explicit`
/// emits the descriptor for `mpeg2-video` and `declared` streams.
/// `mode = "explicit"` requires `video_encode_format` and
/// `transfer_characteristics`; `still_picture` / `sequence_end_code`
/// default to false.  `mode = "off"` sends none (and admits a raw 0xC8).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VideoDecodeControlCfg {
    /// derived / explicit / off.
    #[serde(default)]
    pub mode: VdcMode,
    /// still_picture_flag.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub still_picture: Option<bool>,
    /// sequence_end_code_flag.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sequence_end_code: Option<bool>,
    /// video_encode_format (Table 6-60), e.g. "1080i".
    #[serde(default, with = "named_opt", skip_serializing_if = "Option::is_none")]
    pub video_encode_format: Option<VideoEncodeFormat>,
    /// transfer_characteristics (Table 6-60-1): "bt709", "pq", "hlg",
    /// "undefined".
    #[serde(default, with = "named_opt", skip_serializing_if = "Option::is_none")]
    pub transfer_characteristics: Option<TransferCharacteristics>,
}

/// What the ES gives the `derived` mode.
#[derive(Debug, Clone, Copy)]
pub struct VdcDerived {
    /// The ES contains sequence_end_code.
    pub sequence_end_code: bool,
    /// Table 6-60 format, if the scan format has one.
    pub video_encode_format: Option<VideoEncodeFormat>,
}

impl VideoDecodeControlCfg {
    /// Configuration errors independent of the media.
    pub fn check(&self, what: &str) -> Result<()> {
        match self.mode {
            VdcMode::Off
                if self.still_picture.is_some()
                    || self.sequence_end_code.is_some()
                    || self.video_encode_format.is_some()
                    || self.transfer_characteristics.is_some() =>
            {
                Err(MuxError::Config(format!(
                    "{what}: video_decode_control mode = \"off\" takes no field values"
                )))
            }
            VdcMode::Explicit
                if self.video_encode_format.is_none()
                    || self.transfer_characteristics.is_none() =>
            {
                Err(MuxError::Config(format!(
                    "{what}: video_decode_control mode = \"explicit\" needs video_encode_format ({}) and transfer_characteristics ({})",
                    variant_list::<VideoEncodeFormat>(),
                    variant_list::<TransferCharacteristics>()
                )))
            }
            _ => Ok(()),
        }
    }

    /// The descriptor to send, given what the ES offers (`None` for a
    /// stream without media).
    pub fn resolve(&self, derived: Option<VdcDerived>) -> Option<VideoDecodeControl> {
        let (still, seq_end, fmt, tc) = match self.mode {
            VdcMode::Off => return None,
            VdcMode::Explicit => (
                self.still_picture.unwrap_or(false),
                self.sequence_end_code.unwrap_or(false),
                self.video_encode_format?,
                self.transfer_characteristics?,
            ),
            VdcMode::Derived => {
                let d = derived?;
                (
                    self.still_picture.unwrap_or(false),
                    self.sequence_end_code.unwrap_or(d.sequence_end_code),
                    self.video_encode_format.or(d.video_encode_format)?,
                    self.transfer_characteristics
                        .unwrap_or(TransferCharacteristics::Bt709),
                )
            }
        };
        Some(VideoDecodeControl {
            still_picture_flag: still,
            sequence_end_code_flag: seq_end,
            video_encode_format: fmt.code(),
            transfer_characteristics: tc.code(),
        })
    }
}

/// Deserializer for `video_decode_control` that explains the former bool.
pub fn de_video_decode_control<'de, D: Deserializer<'de>>(
    d: D,
) -> std::result::Result<VideoDecodeControlCfg, D::Error> {
    struct V;
    impl<'de> Visitor<'de> for V {
        type Value = VideoDecodeControlCfg;
        fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("a table { mode = \"derived\" | \"explicit\" | \"off\", … }")
        }
        fn visit_bool<E: de::Error>(self, b: bool) -> std::result::Result<Self::Value, E> {
            Err(E::custom(format!(
                "video_decode_control is no longer a bool; write video_decode_control = {{ mode = \"{}\" }} \
                 (modes: \"derived\", \"explicit\", \"off\"; fields still_picture, sequence_end_code, \
                 video_encode_format, transfer_characteristics)",
                if b { "derived" } else { "off" }
            )))
        }
        fn visit_map<A: MapAccess<'de>>(self, m: A) -> std::result::Result<Self::Value, A::Error> {
            VideoDecodeControlCfg::deserialize(de::value::MapAccessDeserializer::new(m))
        }
    }
    d.deserialize_any(V)
}

// ------------------------------------------------------ EIT component type

/// component_type of the EIT component descriptor (video, tag 0x50, B10
/// §6.2.3 Table 6-5) or audio component descriptor (tag 0xC4, B10 §6.2.26
/// Table 6-43).
///
/// TOML: `"derived"` (default), a Table 6-5 name for video (e.g.
/// `"1080i-16:9-without-pan-vectors"`), or for audio a table
/// `{ audio_mode = "2/0", dialog_control = false, for_disabilities = "none" }`
/// whose missing fields are derived / default.  Unused on `declared` streams
/// (no media, so no EIT component descriptor is generated for them).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ComponentTypeCfg {
    /// Video: [`crate::video::SequenceInfo::component_type`]; audio: audio
    /// mode from the ADTS channel_configuration
    /// ([`crate::audio::AdtsEs::audio_mode`]), no dialog control, "none"
    /// for persons with disabilities.
    #[default]
    Derived,
    /// A Table 6-5 component_type for stream_content 0x1.
    Video(VideoComponentType),
    /// Table 6-43 fields.
    Audio(AudioComponentTypeCfg),
}

/// Audio component_type fields (B10 Table 6-43).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AudioComponentTypeCfg {
    /// b4–b0 audio mode, e.g. "2/0"; derived from the ES when absent.
    #[serde(default, with = "named_opt", skip_serializing_if = "Option::is_none")]
    pub audio_mode: Option<AudioMode>,
    /// b7 dialog control information included.
    #[serde(default)]
    pub dialog_control: bool,
    /// b6–b5 audio for a person with disabilities (default "none").
    #[serde(default, with = "named_opt", skip_serializing_if = "Option::is_none")]
    pub for_disabilities: Option<AudioForDisabilities>,
}

impl ComponentTypeCfg {
    /// Video component_type: configured, or `derived` from the ES.
    pub fn video(&self, derived: Option<VideoComponentType>) -> Option<VideoComponentType> {
        match self {
            ComponentTypeCfg::Video(v) => Some(*v),
            _ => derived,
        }
    }
    /// Audio component_type: configured fields over the derived audio mode.
    pub fn audio(&self, derived_mode: Option<AudioMode>) -> Option<AudioComponentType> {
        let a = match self {
            ComponentTypeCfg::Audio(a) => *a,
            _ => AudioComponentTypeCfg::default(),
        };
        Some(AudioComponentType {
            dialog_control: a.dialog_control,
            for_disabilities: a.for_disabilities.unwrap_or(AudioForDisabilities::None),
            mode: a.audio_mode.or(derived_mode)?,
        })
    }
}

impl Serialize for ComponentTypeCfg {
    fn serialize<S: Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        match self {
            ComponentTypeCfg::Derived => s.serialize_str("derived"),
            ComponentTypeCfg::Video(v) => Named(*v).serialize(s),
            ComponentTypeCfg::Audio(a) => a.serialize(s),
        }
    }
}

impl<'de> Deserialize<'de> for ComponentTypeCfg {
    fn deserialize<D: Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = ComponentTypeCfg;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(
                    "\"derived\", a B10 Table 6-5 video component_type name, or an audio table",
                )
            }
            fn visit_str<E: de::Error>(self, s: &str) -> std::result::Result<Self::Value, E> {
                if s == "derived" {
                    return Ok(ComponentTypeCfg::Derived);
                }
                NamedVisitor::<VideoComponentType>(PhantomData)
                    .visit_str(s)
                    .map(ComponentTypeCfg::Video)
                    .map_err(|_: E| {
                        E::custom(format!(
                            "unknown component_type {s:?}: \"derived\", a video component_type of B10 Table 6-5 \
                             (stream_content 0x1): {}, or for audio a table {{ audio_mode = …, dialog_control = …, \
                             for_disabilities = … }} (Table 6-43)",
                            variant_list::<VideoComponentType>()
                        ))
                    })
            }
            fn visit_i64<E: de::Error>(self, v: i64) -> std::result::Result<Self::Value, E> {
                self.visit_u64(v as u64)
            }
            fn visit_u64<E: de::Error>(self, v: u64) -> std::result::Result<Self::Value, E> {
                let hint = u8::try_from(v)
                    .ok()
                    .map(|c| match VideoComponentType::from_code(c) {
                        VideoComponentType::Reserved(_) => String::new(),
                        x => format!(" (video {v:#04x} is \"{}\")", x.name()),
                    })
                    .unwrap_or_default();
                Err(E::custom(format!(
                    "component_type takes \"derived\", a B10 Table 6-5 name or an audio table, not a number{hint}"
                )))
            }
            fn visit_map<A: MapAccess<'de>>(
                self,
                m: A,
            ) -> std::result::Result<Self::Value, A::Error> {
                AudioComponentTypeCfg::deserialize(de::value::MapAccessDeserializer::new(m))
                    .map(ComponentTypeCfg::Audio)
            }
        }
        d.deserialize_any(V)
    }
}

// ------------------------------------------------ audio quality indicator

/// quality_indicator of the audio component descriptor (tag 0xC4, B10
/// §6.2.26 Table 6-44: '01' "Mode 1", '10' "Mode 2", '11' "Mode 3",
/// "For detail, refer to ARIB STD-B32 Part 2" — the modes are defined in
/// B32 Part 2 Attachment Chapter 3 Table 1, see [`QualityMode`]).
///
/// TOML: `"derived"` (default) or `"mode-1"` / `"mode-2"` / `"mode-3"`.
/// `derived` applies [`QualityMode::derive`] to the stream as sent: sampling
/// frequency below 32 kHz → Mode 3 (Table 1's example); bit rate per
/// stereo pair ≥ 256 kbit/s → Mode 1 (the top of "192 to 256 kbps/stereo
/// or more"); ≥ 144 kbit/s (Note 3, ITU-R broadcast quality) → Mode 2;
/// otherwise Mode 3.  "The quality indicator shall be transmitted in
/// accordance with the quality of an AAC coding stream" — the bit rates
/// of Table 1 are "for reference purposes", so an explicit mode always
/// wins.  The bench `a.m2a` (≈ 189 kbit/s stereo, 48 kHz) derives Mode 2,
/// the value live broadcasters send for 192 kbit/s stereo.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AudioQualityIndicator {
    /// From the measured stream ([`QualityMode::derive`]).
    #[default]
    Derived,
    /// This mode.
    Mode(QualityMode),
}

impl AudioQualityIndicator {
    /// The mode to send, given the one derived from the stream.
    pub fn resolve(self, derived: QualityMode) -> QualityMode {
        match self {
            AudioQualityIndicator::Derived => derived,
            AudioQualityIndicator::Mode(m) => m,
        }
    }

    fn name(self) -> &'static str {
        match self {
            AudioQualityIndicator::Derived => "derived",
            AudioQualityIndicator::Mode(QualityMode::Mode1) => "mode-1",
            AudioQualityIndicator::Mode(QualityMode::Mode2) => "mode-2",
            AudioQualityIndicator::Mode(QualityMode::Mode3) => "mode-3",
        }
    }
}

const QUALITY_NAMES: &str = "\"derived\", \"mode-1\" ('01'), \"mode-2\" ('10'), \"mode-3\" ('11')";

impl Serialize for AudioQualityIndicator {
    fn serialize<S: Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        s.serialize_str(self.name())
    }
}

impl<'de> Deserialize<'de> for AudioQualityIndicator {
    fn deserialize<D: Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        struct V;
        impl Visitor<'_> for V {
            type Value = AudioQualityIndicator;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "one of {QUALITY_NAMES}")
            }
            fn visit_str<E: de::Error>(self, s: &str) -> std::result::Result<Self::Value, E> {
                Ok(match s {
                    "derived" => AudioQualityIndicator::Derived,
                    "mode-1" => AudioQualityIndicator::Mode(QualityMode::Mode1),
                    "mode-2" => AudioQualityIndicator::Mode(QualityMode::Mode2),
                    "mode-3" => AudioQualityIndicator::Mode(QualityMode::Mode3),
                    _ => {
                        return Err(E::custom(format!(
                            "unknown audio_quality_indicator {s:?} (B10 Table 6-44 / B32 Part 2 Attachment Ch. 3 Table 1); one of: {QUALITY_NAMES}"
                        )));
                    }
                })
            }
            fn visit_i64<E: de::Error>(self, v: i64) -> std::result::Result<Self::Value, E> {
                self.visit_u64(v as u64)
            }
            fn visit_u64<E: de::Error>(self, v: u64) -> std::result::Result<Self::Value, E> {
                let hint = u8::try_from(v)
                    .ok()
                    .and_then(QualityMode::from_code)
                    .filter(|_| v < 4)
                    .map(|m| {
                        format!(
                            "; {v} = '{v:02b}' is \"{}\"",
                            AudioQualityIndicator::Mode(m).name()
                        )
                    })
                    .unwrap_or_else(|| {
                        if v == 0 {
                            "; '00' is reserved (B10 Table 6-44)".into()
                        } else {
                            String::new()
                        }
                    });
                Err(E::custom(format!(
                    "audio_quality_indicator takes a named value, not a number{hint}; one of: {QUALITY_NAMES}"
                )))
            }
        }
        d.deserialize_any(V)
    }
}

// ----------------------------------------------------- digital copy control

/// A 4-bit value (0..=15), validated on load.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct U4(u8);

impl U4 {
    /// The value.
    pub fn get(self) -> u8 {
        self.0
    }
}

impl TryFrom<u8> for U4 {
    type Error = MuxError;
    fn try_from(v: u8) -> Result<Self> {
        if v <= 15 {
            Ok(U4(v))
        } else {
            Err(MuxError::Config(format!("{v} does not fit in 4 bits")))
        }
    }
}

impl<'de> Deserialize<'de> for U4 {
    fn deserialize<D: Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        let v = i64::deserialize(d)?;
        u8::try_from(v)
            .ok()
            .and_then(|v| U4::try_from(v).ok())
            .ok_or_else(|| {
                de::Error::custom(format!("user_defined {v} is not a 4-bit value (0..=15)"))
            })
    }
}

/// maximum_bitrate in Mbit/s, coded "rolling up in each 1/4Mbps" (B10
/// §6.2.23): 16.5 → 66.
pub mod mbps_opt {
    use super::*;
    /// Serialize as Mbit/s.
    pub fn serialize<S: Serializer>(
        v: &Option<MaximumBitrate>,
        s: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        v.map(|m| m.mbps()).serialize(s)
    }
    /// Deserialize Mbit/s (integer or float), 0 < rate ≤ 63.75.
    pub fn deserialize<'de, D: Deserializer<'de>>(
        d: D,
    ) -> std::result::Result<Option<MaximumBitrate>, D::Error> {
        let Some(x) = Option::<f64>::deserialize(d)? else {
            return Ok(None);
        };
        if !(x > 0.0 && x <= 63.75) {
            return Err(de::Error::custom(format!(
                "maximum_bitrate_mbps {x} outside (0, 63.75]: the field counts 1/4 Mbit/s in 8 bits (B10 §6.2.23)"
            )));
        }
        Ok(Some(MaximumBitrate((x * 4.0).ceil() as u8)))
    }
}

/// The digital_recording_control_data / user_defined / maximum_bitrate
/// fields shared by the descriptor and its per-component entries (see
/// [`CopyControlCfg`] for their TOML keys).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CopyControlFields {
    /// digital_recording_control_data (B10 Table 6-39).
    pub recording_control: DigitalRecordingControl,
    /// copy_control_type (Annex F Table F-2); `None` = '01'.
    pub copy_control_type: Option<CopyControlType>,
    /// APS_control_data (Annex F Table F-4); `None` = '00'.
    pub aps_control_data: Option<ApsControlData>,
    /// Raw user_defined experiment override.
    pub user_defined: Option<U4>,
    /// maximum_bitrate.
    pub maximum_bitrate: Option<MaximumBitrate>,
}

impl CopyControlFields {
    /// The 4-bit user_defined value: raw override, or copy_control_type
    /// (default '01') then APS_control_data (default '00') if '01', else
    /// reserved_future_use '11' (Annex F Table F-1; '1' per B10 §3.1).
    pub fn user_defined_code(&self, what: &str) -> Result<u8> {
        if let Some(u) = self.user_defined {
            if self.copy_control_type.is_some() || self.aps_control_data.is_some() {
                return Err(MuxError::Config(format!(
                    "{what}: user_defined (raw experiment override) conflicts with copy_control_type / \
                     aps_control_data; give one or the other"
                )));
            }
            return Ok(u.get());
        }
        let cct = self
            .copy_control_type
            .unwrap_or(CopyControlType::SerialOutputEncoded);
        let low = if cct == CopyControlType::SerialOutputEncoded {
            self.aps_control_data
                .unwrap_or(ApsControlData::CopyFree)
                .code()
        } else if self.aps_control_data.is_some() {
            return Err(MuxError::Config(format!(
                "{what}: aps_control_data exists only with copy_control_type = \"serial-output-encoded\" \
                 (B10 Annex F Table F-1); for \"{}\" the 2 bits are reserved_future_use",
                cct.name()
            )));
        } else {
            0b11
        };
        Ok((cct.code() << 2) | low)
    }
}

/// One component entry of the digital copy control descriptor (B10 Table
/// 6-38 inner loop); fields as in [`CopyControlCfg`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ComponentCopyControlCfg {
    /// component_tag of the elementary stream.
    pub component_tag: u8,
    /// digital_recording_control_data (B10 Table 6-39).
    #[serde(with = "named")]
    pub recording_control: DigitalRecordingControl,
    /// copy_control_type (Annex F Table F-2), default '01'.
    #[serde(default, with = "named_opt", skip_serializing_if = "Option::is_none")]
    pub copy_control_type: Option<CopyControlType>,
    /// APS_control_data (Annex F Table F-4), default '00'.
    #[serde(default, with = "named_opt", skip_serializing_if = "Option::is_none")]
    pub aps_control_data: Option<ApsControlData>,
    /// Raw user_defined experiment override.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_defined: Option<U4>,
    /// maximum_bitrate in Mbit/s.
    #[serde(default, with = "mbps_opt", skip_serializing_if = "Option::is_none")]
    pub maximum_bitrate_mbps: Option<MaximumBitrate>,
}

impl ComponentCopyControlCfg {
    /// The shared fields.
    pub fn fields(&self) -> CopyControlFields {
        CopyControlFields {
            recording_control: self.recording_control,
            copy_control_type: self.copy_control_type,
            aps_control_data: self.aps_control_data,
            user_defined: self.user_defined,
            maximum_bitrate: self.maximum_bitrate_mbps,
        }
    }
}

/// The service-level digital_recording_control_data of a
/// [`CopyControlCfg`]: a named value of B10 Table 6-39, or `"source"`
/// (only for a service with a `ts-program` stream): the recording mode the
/// relayed source program signals, so that a relay never downgrades the
/// source's copy restriction (resolved by
/// [`crate::Multiplexer::new`] from [`crate::relay::Probe::source_copy_control`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordingControlCfg {
    /// A fixed value.
    Value(DigitalRecordingControl),
    /// The relayed source program's value (`recording_control = "source"`).
    Source,
}

impl RecordingControlCfg {
    /// The TOML name of [`RecordingControlCfg::Source`].
    pub const SOURCE: &'static str = "source";

    /// The fixed value; `None` while still [`RecordingControlCfg::Source`].
    pub fn value(self) -> Option<DigitalRecordingControl> {
        match self {
            RecordingControlCfg::Value(v) => Some(v),
            RecordingControlCfg::Source => None,
        }
    }
}

impl From<DigitalRecordingControl> for RecordingControlCfg {
    fn from(v: DigitalRecordingControl) -> Self {
        RecordingControlCfg::Value(v)
    }
}

impl<'de> Deserialize<'de> for RecordingControlCfg {
    fn deserialize<D: Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = RecordingControlCfg;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(
                    f,
                    "\"{}\" or one of {}",
                    RecordingControlCfg::SOURCE,
                    variant_list::<DigitalRecordingControl>()
                )
            }
            fn visit_str<E: de::Error>(self, s: &str) -> std::result::Result<Self::Value, E> {
                if s == RecordingControlCfg::SOURCE {
                    return Ok(RecordingControlCfg::Source);
                }
                NamedVisitor::<DigitalRecordingControl>(PhantomData)
                    .visit_str(s)
                    .map(RecordingControlCfg::Value)
            }
            fn visit_i64<E: de::Error>(self, v: i64) -> std::result::Result<Self::Value, E> {
                NamedVisitor::<DigitalRecordingControl>(PhantomData)
                    .visit_i64(v)
                    .map(RecordingControlCfg::Value)
            }
            fn visit_u64<E: de::Error>(self, v: u64) -> std::result::Result<Self::Value, E> {
                NamedVisitor::<DigitalRecordingControl>(PhantomData)
                    .visit_u64(v)
                    .map(RecordingControlCfg::Value)
            }
        }
        d.deserialize_any(V)
    }
}

impl Serialize for RecordingControlCfg {
    fn serialize<S: Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        match self {
            RecordingControlCfg::Value(v) => Named(*v).serialize(s),
            RecordingControlCfg::Source => s.serialize_str(RecordingControlCfg::SOURCE),
        }
    }
}

/// digital_copy_control_descriptor (tag 0xC1) — ARIB STD-B10 Part 2 §6.2.23,
/// Table 6-38, with placement.
///
/// TOML: `copy_control = { recording_control = "copy-one-generation",
/// copy_control_type = "serial-output-encoded", aps_control_data =
/// "copy-free", maximum_bitrate_mbps = 16.5, in_pmt = true, in_sdt = true,
/// in_eit = false }`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CopyControlCfg {
    /// digital_recording_control_data (B10 Table 6-39): "copy-free" ('00'),
    /// "provider-defined" ('01'), "copy-one-generation" ('10'),
    /// "copy-never" ('11'), or "source" (a relayed `ts-program` service:
    /// the source program's value, see [`RecordingControlCfg`]).
    pub recording_control: RecordingControlCfg,
    /// copy_control_type: upper 2 bits of user_defined (B10 Annex F
    /// (informative) Table F-2; understood to be normative in the ARIB
    /// operational guidelines TR-B14/TR-B15, not available locally).
    /// Default "serial-output-encoded" ('01'), as live broadcasters send and
    /// a Sony BDZ-series recorder requires (bench 2026-10-08: '00' is refused).
    #[serde(default, with = "named_opt", skip_serializing_if = "Option::is_none")]
    pub copy_control_type: Option<CopyControlType>,
    /// APS_control_data (Annex F Table F-4), only with copy_control_type
    /// '01'; default "copy-free" ('00').
    #[serde(default, with = "named_opt", skip_serializing_if = "Option::is_none")]
    pub aps_control_data: Option<ApsControlData>,
    /// Experiment override: the raw 4-bit user_defined field, replacing
    /// copy_control_type / aps_control_data (which must then be absent).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_defined: Option<U4>,
    /// maximum_bitrate in Mbit/s (sent as ⌈rate × 4⌉ quarter-Mbit/s units,
    /// B10 §6.2.23 "rolling up in each 1/4Mbps").
    #[serde(default, with = "mbps_opt", skip_serializing_if = "Option::is_none")]
    pub maximum_bitrate_mbps: Option<MaximumBitrate>,
    /// Per-component loop (component_control_flag = 1).  B10: "When this
    /// descriptor is transmitted by PMT, component control flag should
    /// always be '0'", so it is rejected together with `in_pmt`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub component_control: Vec<ComponentCopyControlCfg>,
    /// Put it in the PMT program loop.
    #[serde(default = "yes")]
    pub in_pmt: bool,
    /// Put it in every EIT event of the service.
    #[serde(default = "yes")]
    pub in_eit: bool,
    /// Put it in the SDT service loop.
    #[serde(default)]
    pub in_sdt: bool,
}

fn yes() -> bool {
    true
}

impl CopyControlCfg {
    /// The descriptor-level fields; an error while `recording_control` is
    /// still `"source"` (it is resolved when the relayed source is probed).
    pub fn fields(&self, what: &str) -> Result<CopyControlFields> {
        let recording_control = self.recording_control.value().ok_or_else(|| {
            MuxError::Config(format!(
                "{what}: recording_control = \"source\" needs a ts-program stream in the service \
                 (the relayed source program provides the value)"
            ))
        })?;
        Ok(CopyControlFields {
            recording_control,
            copy_control_type: self.copy_control_type,
            aps_control_data: self.aps_control_data,
            user_defined: self.user_defined,
            maximum_bitrate: self.maximum_bitrate_mbps,
        })
    }

    /// Configuration errors (`recording_control = "source"` is accepted
    /// here; [`crate::config::Config::validate`] checks that the service
    /// relays a `ts-program`).
    pub fn check(&self, what: &str) -> Result<()> {
        let mut f = self.clone();
        if f.recording_control == RecordingControlCfg::Source {
            f.recording_control = DigitalRecordingControl::CopyFree.into();
        }
        f.fields(what)?.user_defined_code(what)?;
        for c in &self.component_control {
            c.fields()
                .user_defined_code(&format!("{what} component {:#04x}", c.component_tag))?;
        }
        if self.in_pmt && !self.component_control.is_empty() {
            return Err(MuxError::Config(format!(
                "{what}: copy_control.component_control with in_pmt = true; B10 §6.2.23: \"When this \
                 descriptor is transmitted by PMT, component control flag should always be '0'\""
            )));
        }
        Ok(())
    }

    /// The descriptor (`with_components`: include the component loop).
    pub fn descriptor(&self, with_components: bool, what: &str) -> Result<DigitalCopyControl> {
        let components = if with_components && !self.component_control.is_empty() {
            Some(
                self.component_control
                    .iter()
                    .map(|c| {
                        let f = c.fields();
                        Ok(ComponentCopyControl {
                            component_tag: c.component_tag,
                            digital_recording_control_data: f.recording_control.code(),
                            reserved_future_use: true,
                            user_defined: f.user_defined_code(what)?,
                            maximum_bitrate: f.maximum_bitrate.map(|m| m.0),
                        })
                    })
                    .collect::<Result<Vec<_>>>()?,
            )
        } else {
            None
        };
        let f = self.fields(what)?;
        Ok(DigitalCopyControl {
            digital_recording_control_data: f.recording_control.code(),
            user_defined: f.user_defined_code(what)?,
            maximum_bitrate: f.maximum_bitrate.map(|m| m.0),
            component_control: components,
        })
    }
}

// ------------------------------------------------------ remote control key

/// remote_control_key_id of the TS information descriptor (tag 0xCD) — ARIB
/// STD-B10 Part 2 §6.2.42, Table 6-82: "the recommended remote control key
/// number to which the applicable TS shall be assigned" (8 bits,
/// "Specified and operated by the company", Part 1 Table 7-1).
///
/// B10 states no range.  isdbt-mux accepts 1..=12, the one-touch keys of
/// terrestrial receivers (operational practice of ARIB TR-B14, not available
/// locally); receivers number the TS's services as 3-digit channels
/// key·10 + service index (e.g. key 9 → 091, 092, …; TR-B14, from
/// knowledge).  For an out-of-range experiment patch the NIT bytes
/// (`[nit] byte_patches`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct RemoteControlKeyId(u8);

impl RemoteControlKeyId {
    /// Accepted range.
    pub const RANGE: std::ops::RangeInclusive<u8> = 1..=12;
    /// The key number.
    pub fn get(self) -> u8 {
        self.0
    }
}

impl RemoteControlKeyId {
    fn check(v: i64) -> std::result::Result<Self, String> {
        match u8::try_from(v) {
            Ok(k) if Self::RANGE.contains(&k) => Ok(Self(k)),
            _ => Err(format!(
                "remote_control_key_id {v} outside 1..=12: B10 §6.2.42 leaves the 8-bit key number to the \
                 operator; isdbt-mux restricts it to the 12 one-touch keys of terrestrial receivers \
                 (TR-B14 practice; the receiver shows the services as channel key·10 + index). \
                 For an out-of-range experiment use [nit] byte_patches"
            )),
        }
    }
}

impl TryFrom<i64> for RemoteControlKeyId {
    type Error = MuxError;
    fn try_from(v: i64) -> Result<Self> {
        Self::check(v).map_err(MuxError::Config)
    }
}

impl<'de> Deserialize<'de> for RemoteControlKeyId {
    fn deserialize<D: Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        let v = i64::deserialize(d)?;
        RemoteControlKeyId::check(v).map_err(de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copy_control_user_defined() {
        let f = |cct, aps, ud: Option<u8>| CopyControlFields {
            recording_control: DigitalRecordingControl::CopyOneGeneration,
            copy_control_type: cct,
            aps_control_data: aps,
            user_defined: ud.map(|u| U4::try_from(u).unwrap()),
            maximum_bitrate: None,
        };
        // default = '01' + APS '00' = 0b0100 (live broadcasters)
        assert_eq!(f(None, None, None).user_defined_code("t").unwrap(), 0b0100);
        assert_eq!(
            f(None, Some(ApsControlData::PseudoSyncBurst4), None)
                .user_defined_code("t")
                .unwrap(),
            0b0111
        );
        assert_eq!(
            f(Some(CopyControlType::Undefined00), None, None)
                .user_defined_code("t")
                .unwrap(),
            0b0011
        );
        assert_eq!(f(None, None, Some(0)).user_defined_code("t").unwrap(), 0);
        assert!(
            f(Some(CopyControlType::Undefined00), None, Some(0))
                .user_defined_code("t")
                .is_err()
        );
        assert!(
            f(
                Some(CopyControlType::Undefined10),
                Some(ApsControlData::CopyFree),
                None
            )
            .user_defined_code("t")
            .is_err()
        );
        assert!(U4::try_from(16).is_err());
    }

    #[test]
    fn remote_key_range() {
        assert!(RemoteControlKeyId::try_from(0).is_err());
        assert_eq!(RemoteControlKeyId::try_from(1).unwrap().get(), 1);
        assert_eq!(RemoteControlKeyId::try_from(12).unwrap().get(), 12);
        let e = RemoteControlKeyId::try_from(13).unwrap_err().to_string();
        assert!(e.contains("1..=12"), "{e}");
        assert!(RemoteControlKeyId::try_from(300).is_err());
    }

    #[test]
    fn vdc_resolution() {
        let d = VdcDerived {
            sequence_end_code: false,
            video_encode_format: Some(VideoEncodeFormat::I1080),
        };
        let derived = VideoDecodeControlCfg::default();
        let v = derived.resolve(Some(d)).unwrap();
        let mut b = Vec::new();
        isdb_si::descriptor::DescriptorBody::encode(&v, &mut b).unwrap();
        assert_eq!(b, [0x04]); // 0 0 0001 00
        assert_eq!(derived.resolve(None), None);
        let over = VideoDecodeControlCfg {
            sequence_end_code: Some(true),
            transfer_characteristics: Some(TransferCharacteristics::Undefined),
            ..Default::default()
        };
        let v = over.resolve(Some(d)).unwrap();
        let mut b = Vec::new();
        isdb_si::descriptor::DescriptorBody::encode(&v, &mut b).unwrap();
        assert_eq!(b, [0x47]); // byte live HD services send
        let explicit = VideoDecodeControlCfg {
            mode: VdcMode::Explicit,
            ..Default::default()
        };
        assert!(explicit.check("t").is_err());
        let off = VideoDecodeControlCfg {
            mode: VdcMode::Off,
            still_picture: Some(true),
            ..Default::default()
        };
        assert!(off.check("t").is_err());
    }
}
