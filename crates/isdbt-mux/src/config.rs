//! Declarative description of one terrestrial transport stream (TOML via
//! serde).
//!
//! Values that ARIB derives from the transmission parameters are *not*
//! configured here: the terrestrial delivery system descriptor's
//! guard_interval / transmission_mode (B10 Tables 6-62/6-63) come from
//! [`isdbt_core::Params`], the frequency codes from the UHF channel list,
//! the partial reception descriptor from the services placed on layer A.
//! Every generated table can be modified for receiver experiments with the
//! common [`TableOpts`] fields (raw descriptors, header fields, byte
//! patches, whole transplanted sections).

use serde::{Deserialize, Serialize};

pub use crate::audio::AdtsCompliance;
use crate::error::{MuxError, Result};
pub use crate::signalling::{
    AudioComponentTypeCfg, AudioQualityIndicator, ComponentCopyControlCfg, ComponentTypeCfg,
    CopyControlCfg, RemoteControlKeyId, U4, VdcMode, VideoDecodeControlCfg,
};

/// Hierarchical layer name in the configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum LayerName {
    /// Layer A (partial-reception layer when partial reception is on).
    A,
    /// Layer B.
    B,
    /// Layer C.
    C,
}

impl LayerName {
    /// 0, 1, 2.
    pub fn index(self) -> usize {
        match self {
            LayerName::A => 0,
            LayerName::B => 1,
            LayerName::C => 2,
        }
    }
    /// The `isdbt_core` layer.
    pub fn layer(self) -> isdbt_core::Layer {
        isdbt_core::Layer::ALL[self.index()]
    }
}

/// Complete multiplex description.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// NIT network description.
    pub network: Network,
    /// Transport stream identity.
    pub transport: Transport,
    /// Stream clock (TOT/TDT and EIT "now").
    #[serde(default)]
    pub clock: Clock,
    /// PCR / PES timing.
    #[serde(default)]
    pub timing: Timing,
    /// Services (programs) in this TS.
    #[serde(default, rename = "service")]
    pub services: Vec<Service>,
    /// PAT options.
    #[serde(default)]
    pub pat: PatCfg,
    /// CAT content and options.
    #[serde(default)]
    pub cat: CatCfg,
    /// NIT (actual network) options.
    #[serde(default)]
    pub nit: NitCfg,
    /// SDT (actual stream) options.
    #[serde(default)]
    pub sdt: SdtCfg,
    /// EIT present/following (actual stream) options.
    #[serde(default)]
    pub eit: EitCfg,
    /// EIT schedule (actual stream) options.
    #[serde(default)]
    pub eit_schedule: EitScheduleCfg,
    /// TOT options.
    #[serde(default)]
    pub tot: TotCfg,
    /// TDT options (disabled by default: B10 Table 5-2 requires TDT *or* TOT).
    #[serde(default)]
    pub tdt: TdtCfg,
    /// BIT content and options.
    #[serde(default)]
    pub bit: BitCfg,
    /// Additional raw section streams (e.g. tables this crate does not type,
    /// transplanted from a capture), each on its own PID and layer.
    #[serde(default, rename = "raw_table")]
    pub raw_tables: Vec<RawTableCfg>,
}

/// NIT network data (B10 §5.2.4, terrestrial descriptors §6.2.31/6.2.32/6.2.42).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Network {
    /// network_id (B10 Annex N: terrestrial TV 0x7880–0x7FE8).
    pub network_id: u16,
    /// network_name_descriptor text.
    pub name: String,
    /// area_code of the terrestrial delivery system descriptor (12 bits).
    pub area_code: u16,
    /// remote_control_key_id of the TS information descriptor (B10
    /// §6.2.42), validated 1..=12 ([`RemoteControlKeyId`]).
    pub remote_control_key_id: RemoteControlKeyId,
    /// ts_name of the TS information descriptor (defaults to `name`).
    #[serde(default)]
    pub ts_name: Option<String>,
    /// UHF channels (13..62) of the network; each gives one frequency code
    /// (centre frequency in 1/7 MHz, B10 §6.2.31).
    #[serde(default)]
    pub channels: Vec<u32>,
    /// Additional raw frequency codes (1/7 MHz units).
    #[serde(default)]
    pub frequencies: Vec<u16>,
    /// additional_broadcasting_identification of the system management
    /// descriptor (B10 Table 6-34: "specified by the operation standard of
    /// service providers"; operational choice, default 0x01).
    #[serde(default = "default_abi")]
    pub additional_broadcasting_identification: u8,
    /// Emit the system management descriptor in the NIT network loop.
    #[serde(default = "yes")]
    pub system_management: bool,
    /// transmission_type loops of the TS information descriptor (B10
    /// §6.2.42: transmission_type_info is "defined by the operational
    /// guidelines of service providers"). The service_id lists are derived
    /// from the services on each layer. Default: none.
    #[serde(default)]
    pub transmission_types: Vec<TransmissionTypeCfg>,
    /// Emit the partial reception descriptor (services on layer A) when the
    /// transmission uses partial reception.
    #[serde(default = "yes")]
    pub partial_reception_descriptor: bool,
}

/// One transmission_type loop of the TS information descriptor.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TransmissionTypeCfg {
    /// transmission_type_info byte.
    pub info: u8,
    /// Layer whose services are listed.
    pub layer: LayerName,
}

/// Transport stream identity.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Transport {
    /// transport_stream_id.
    pub transport_stream_id: u16,
    /// original_network_id (defaults to network_id; B10 §5.2.4 "the
    /// network_id and the original_network_id shall take the same value").
    #[serde(default)]
    pub original_network_id: Option<u16>,
}

/// Where the JST wall clock comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
#[non_exhaustive]
pub enum ClockSource {
    /// System clock at multiplexer creation, then advanced by the stream clock.
    #[default]
    System,
    /// `start` (JST), advanced by the stream clock.
    Fixed,
}

/// Stream clock configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Clock {
    /// Clock source.
    pub source: ClockSource,
    /// Start time in JST, `YYYY-MM-DDTHH:MM:SS` (required for `fixed`).
    pub start: Option<String>,
    /// PCR value (27 MHz ticks) of the first multiplex slot.
    pub start_pcr: u64,
}

impl Default for Clock {
    fn default() -> Self {
        Self {
            source: ClockSource::System,
            start: None,
            start_pcr: 0,
        }
    }
}

/// PCR, PES and scheduling parameters.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Timing {
    /// Delay from the first multiplex slot to the first DTS (ms).
    pub av_delay_ms: u32,
    /// Maximum delay of any byte through the T-STD (H.222.0 §2.4.2.6:
    /// "less than or equal to one second").
    pub max_std_delay_ms: u32,
    /// PCR packets per OFDM frame per service. Default: 1/2/4 for Mode
    /// 1/2/3 (B31 Part 2 §3.2.4 Table 3-5); must divide the layer's
    /// TSPs per frame and keep the interval ≤ 100 ms (H.222.0 §2.7.2).
    pub pcr_per_frame: Option<u32>,
    /// Section repetition is scheduled at `interval × margin` so that the
    /// measured interval stays within the configured maximum.
    pub interval_margin: f64,
    /// B10 §5.1.4 "4KB in 32 ms" per PID: maximum bytes of 188-byte
    /// packets of one section PID in any window of `si_window_ms`.
    pub si_max_bytes: u32,
    /// Window of the per-PID section rate limit (ms).
    pub si_window_ms: u32,
}

impl Default for Timing {
    fn default() -> Self {
        Self {
            av_delay_ms: 700,
            max_std_delay_ms: 1000,
            pcr_per_frame: None,
            interval_margin: 0.9,
            si_max_bytes: 4096,
            si_window_ms: 32,
        }
    }
}

/// A raw descriptor given as tag + hex body (the bytes after descriptor_length).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawDescriptor {
    /// descriptor_tag.
    pub tag: u8,
    /// Body bytes in hex (spaces/colons allowed).
    #[serde(default)]
    pub hex: String,
}

/// A raw descriptor added to one descriptor loop of a generated table.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AddDescriptor {
    /// Loop selector, per table: CAT `main`; PMT `program`, `es:<pid>`;
    /// NIT `network`, `ts`; SDT `service`, `service:<sid>`; EIT `event`,
    /// `event:<sid>`, `event:<sid>:<event_id>`; TOT `main`; BIT `first`,
    /// `broadcaster:<id>`.
    #[serde(rename = "loop")]
    pub target: String,
    /// descriptor_tag.
    pub tag: u8,
    /// Body bytes in hex.
    #[serde(default)]
    pub hex: String,
    /// Insert at the front of the loop instead of appending.
    #[serde(default)]
    pub first: bool,
}

/// Header-field overrides applied to every generated section of a table
/// (H.222.0 §2.4.4.10/§2.4.4.11 long form; short form uses only the first
/// four). CRC_32 is recomputed.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HeaderOverride {
    /// table_id.
    pub table_id: Option<u8>,
    /// section_syntax_indicator (patched into the encoded bytes).
    pub section_syntax_indicator: Option<bool>,
    /// The bit after section_syntax_indicator (private_indicator /
    /// reserved_future_use).
    pub private_indicator: Option<bool>,
    /// The 2 reserved bits before section_length.
    pub reserved1: Option<u8>,
    /// table_id_extension.
    pub table_id_extension: Option<u16>,
    /// The 2 reserved bits before version_number.
    pub reserved2: Option<u8>,
    /// version_number (fixes the version; automatic increments stop).
    pub version_number: Option<u8>,
    /// current_next_indicator.
    pub current_next_indicator: Option<bool>,
    /// section_number.
    pub section_number: Option<u8>,
    /// last_section_number.
    pub last_section_number: Option<u8>,
}

/// Byte-level patch of an encoded section.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BytePatch {
    /// Index of the section in the table's generated list (all if absent).
    #[serde(default)]
    pub section: Option<usize>,
    /// Byte offset from the table_id byte.
    pub offset: usize,
    /// Replacement bytes in hex.
    pub hex: String,
    /// Recompute CRC_32 afterwards (if the section has one).
    #[serde(default = "yes")]
    pub fix_crc: bool,
}

/// Options shared by every table. Spelled out in each table's struct so
/// that unknown keys are rejected.
#[derive(Debug, Clone, Default)]
pub struct TableOpts {
    /// Transmit this table.
    pub enabled: bool,
    /// Maximum repetition interval (ms); default from B10 Table 5-2.
    pub interval_ms: Option<u32>,
    /// Layer carrying the table.
    pub layer: Option<LayerName>,
    /// PID override (B10 Table 5-1 PIDs are the default).
    pub pid: Option<u16>,
    /// Initial version_number.
    pub version: u8,
    /// Raw descriptors added to the generated loops.
    pub add_descriptors: Vec<AddDescriptor>,
    /// Header field overrides.
    pub header: HeaderOverride,
    /// Byte patches.
    pub byte_patches: Vec<BytePatch>,
    /// Transmit these sections (hex) instead of the generated ones,
    /// byte-identical.
    pub replace_sections: Vec<String>,
    /// Transmit these sections (hex) after the generated ones.
    pub extra_sections: Vec<String>,
}

macro_rules! table_cfg {
    ($(#[$sm:meta])* $name:ident, enabled = $en:expr; $( $(#[$m:meta])* $f:ident : $t:ty = $d:expr ),* $(,)?) => {
        $(#[$sm])*
        #[derive(Debug, Clone, Serialize, Deserialize)]
        #[serde(deny_unknown_fields, default)]
        pub struct $name {
            /// Transmit this table.
            pub enabled: bool,
            /// Maximum repetition interval (ms); default from B10 Table 5-2.
            pub interval_ms: Option<u32>,
            /// Layer carrying the table (default: see SPEC-NOTES).
            pub layer: Option<LayerName>,
            /// PID override.
            pub pid: Option<u16>,
            /// Initial version_number.
            pub version: u8,
            /// Raw descriptors added to generated loops.
            pub add_descriptors: Vec<AddDescriptor>,
            /// Header field overrides.
            pub header: HeaderOverride,
            /// Byte patches of encoded sections.
            pub byte_patches: Vec<BytePatch>,
            /// Sections (hex) sent instead of the generated ones.
            pub replace_sections: Vec<String>,
            /// Sections (hex) sent after the generated ones.
            pub extra_sections: Vec<String>,
            $( $(#[$m])* pub $f: $t, )*
        }

        impl Default for $name {
            fn default() -> Self {
                Self {
                    enabled: $en,
                    interval_ms: None,
                    layer: None,
                    pid: None,
                    version: 0,
                    add_descriptors: Vec::new(),
                    header: HeaderOverride::default(),
                    byte_patches: Vec::new(),
                    replace_sections: Vec::new(),
                    extra_sections: Vec::new(),
                    $( $f: $d, )*
                }
            }
        }

        impl $name {
            /// The common options.
            pub fn opts(&self) -> TableOpts {
                TableOpts {
                    enabled: self.enabled,
                    interval_ms: self.interval_ms,
                    layer: self.layer,
                    pid: self.pid,
                    version: self.version,
                    add_descriptors: self.add_descriptors.clone(),
                    header: self.header.clone(),
                    byte_patches: self.byte_patches.clone(),
                    replace_sections: self.replace_sections.clone(),
                    extra_sections: self.extra_sections.clone(),
                }
            }
        }
    };
}

table_cfg!(
    /// PAT (H.222.0 §2.4.4.3); the network_PID entry is always generated.
    PatCfg, enabled = true;
);
table_cfg!(
    /// CAT (H.222.0 §2.4.4.6).
    CatCfg, enabled = true;
    /// CA descriptors (raw; tag 0x09 per H.222.0 §2.6.16).
    descriptors: Vec<RawDescriptor> = Vec::new(),
);
table_cfg!(
    /// NIT actual network (B10 §5.2.4).
    NitCfg, enabled = true;
);
table_cfg!(
    /// SDT actual stream (B10 §5.2.6).
    SdtCfg, enabled = true;
);
table_cfg!(
    /// EIT present/following actual (B10 §5.2.7, Part 2 Annex 1.4.1).
    EitCfg, enabled = true;
    /// running_status of the present event (B10 Annex Table 1-1).
    present_running_status: u8 = 4,
    /// running_status of the following event (Table 1-2: running not allowed).
    following_running_status: u8 = 1,
);
table_cfg!(
    /// EIT schedule actual (B10 §5.2.7, Part 2 Annex 1.4.2).
    EitScheduleCfg, enabled = false;
    /// Segment placement reference ([`EitScheduleReference`]): `"today"`
    /// (method 1, default) or `"month"` (method 2).
    reference: EitScheduleReference = EitScheduleReference::Today,
);
table_cfg!(
    /// TOT (B10 §5.2.9).
    TotCfg, enabled = true;
    /// Optional local time offset descriptor entries.
    local_time_offset: Vec<LocalTimeOffsetCfg> = Vec::new(),
);
table_cfg!(
    /// TDT (B10 §5.2.8).
    TdtCfg, enabled = false;
);
table_cfg!(
    /// BIT (B10 §5.2.13).
    BitCfg, enabled = true;
    /// broadcast_view_propriety.
    broadcast_view_propriety: bool = true,
    /// Raw first-loop descriptors.
    first_descriptors: Vec<RawDescriptor> = Vec::new(),
    /// Broadcaster loop.
    broadcasters: Vec<BroadcasterCfg> = Vec::new(),
);

/// Where EIT schedule segments are counted from (B10 Part 2 Annex 1.4.2,
/// `[eit_schedule] reference`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
#[non_exhaustive]
pub enum EitScheduleReference {
    /// From today 00:00 JST (method 1, default).
    #[default]
    Today,
    /// From the first day of the month 00:00 JST (method 2).
    Month,
}

/// A raw section stream.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawTableCfg {
    /// PID.
    pub pid: u16,
    /// Layer.
    pub layer: LayerName,
    /// Repetition interval (ms).
    pub interval_ms: u32,
    /// Sections in hex, sent in order every interval.
    pub sections: Vec<String>,
}

/// One entry of the local time offset descriptor (B10 §6.2.25).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LocalTimeOffsetCfg {
    /// country_code, e.g. "JPN".
    pub country_code: String,
    /// country_region_id (6 bits).
    #[serde(default)]
    pub country_region_id: u8,
    /// Offset polarity: false = local ahead of JST.
    #[serde(default)]
    pub polarity: bool,
    /// local_time_offset "HH:MM".
    pub offset: String,
    /// time_of_change JST.
    pub time_of_change: String,
    /// next_time_offset "HH:MM".
    pub next_offset: String,
}

/// One broadcaster of the BIT.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BroadcasterCfg {
    /// broadcaster_id.
    pub broadcaster_id: u8,
    /// broadcaster_name_descriptor (0xD8) text.
    #[serde(default)]
    pub name: Option<String>,
    /// extended_broadcaster_descriptor (0xCE), broadcaster_type 0x1.
    #[serde(default)]
    pub terrestrial: Option<TerrestrialBroadcasterCfg>,
    /// Raw descriptors.
    #[serde(default)]
    pub descriptors: Vec<RawDescriptor>,
}

/// Terrestrial TV broadcaster fields of the extended broadcaster descriptor.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TerrestrialBroadcasterCfg {
    /// terrestrial_broadcaster_id.
    pub terrestrial_broadcaster_id: u16,
    /// affiliation_id list.
    #[serde(default)]
    pub affiliation_ids: Vec<u8>,
    /// (original_network_id, broadcaster_id) pairs.
    #[serde(default)]
    pub broadcasters: Vec<(u16, u8)>,
}

/// One service (program).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Service {
    /// service_id = program_number.
    pub service_id: u16,
    /// service_type (B10 Table 6-25), e.g. 0x01 digital TV, 0xC0 data.
    #[serde(default = "default_service_type")]
    pub service_type: u8,
    /// Service provider name (service descriptor).
    #[serde(default)]
    pub provider: String,
    /// Service name (service descriptor).
    pub name: String,
    /// Layer carrying the service's elementary streams.
    pub layer: LayerName,
    /// PMT PID.
    pub pmt_pid: u16,
    /// PCR PID (0x1FFF: no PCR). May equal an elementary PID.
    #[serde(default = "default_no_pcr")]
    pub pcr_pid: u16,
    /// SDT running_status (B10 Table 5-6).
    #[serde(default)]
    pub running_status: u8,
    /// SDT/EIT free_CA_mode.
    #[serde(default)]
    pub free_ca_mode: bool,
    /// EIT_user_defined_flags (3 bits; '111' = unused).
    #[serde(default = "default_udf")]
    pub eit_user_defined_flags: u8,
    /// Generate EIT present/following for this service.
    #[serde(default = "yes")]
    pub eit_pf: bool,
    /// Include this service in the EIT schedule (when enabled).
    #[serde(default = "yes")]
    pub eit_schedule: bool,
    /// Typed digital copy control descriptor (B10 §6.2.23).
    #[serde(default)]
    pub copy_control: Option<CopyControlCfg>,
    /// Typed content availability descriptor (B10 §6.2.45).
    #[serde(default)]
    pub content_availability: Option<ContentAvailabilityCfg>,
    /// Raw PMT program_info descriptors.
    #[serde(default)]
    pub program_descriptors: Vec<RawDescriptor>,
    /// Raw SDT service-loop descriptors (after the service descriptor).
    #[serde(default)]
    pub sdt_descriptors: Vec<RawDescriptor>,
    /// PMT table options.
    #[serde(default)]
    pub pmt: PmtCfg,
    /// Elementary streams.
    #[serde(default, rename = "stream")]
    pub streams: Vec<StreamCfg>,
    /// Events for the EIT.
    #[serde(default, rename = "event")]
    pub events: Vec<EventCfg>,
}

table_cfg!(
    /// PMT (H.222.0 §2.4.4.8) of one service.
    PmtCfg, enabled = true;
);

/// Content availability descriptor fields.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContentAvailabilityCfg {
    /// copy_restriction_mode.
    #[serde(default)]
    pub copy_restriction_mode: bool,
    /// image_constraint_token.
    #[serde(default = "yes")]
    pub image_constraint_token: bool,
    /// retention_mode.
    #[serde(default)]
    pub retention_mode: bool,
    /// retention_state (3 bits).
    #[serde(default = "seven")]
    pub retention_state: u8,
    /// encryption_mode.
    #[serde(default)]
    pub encryption_mode: bool,
    /// Put it in the PMT program loop.
    #[serde(default = "yes")]
    pub in_pmt: bool,
    /// Put it in every EIT event of the service.
    #[serde(default)]
    pub in_eit: bool,
}

/// Kind of elementary stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum StreamKind {
    /// ITU-T H.262 | ISO/IEC 13818-2 video elementary stream file.
    Mpeg2Video,
    /// ISO/IEC 13818-7 AAC in ADTS frames.
    AacAdts,
    /// Listed in the PMT only; no packets are sent.
    Declared,
    /// One program of a recorded transport stream, rebroadcast as this
    /// service's elementary streams ([`crate::relay`]): PES bytes kept,
    /// PTS/DTS rebased onto this multiplex, PSI/SI generated here.
    TsProgram,
}

/// What a `ts-program` stream does at the end of its file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
#[non_exhaustive]
pub enum AtEof {
    /// Start again at the first random access point, timestamps continuing
    /// (default).
    #[default]
    Loop,
    /// Stop relaying; the multiplex then carries PSI/SI and nulls
    /// ([`crate::Multiplexer::relays_finished`] turns true).
    End,
}

/// What a `ts-program` relay does with the source program's conditional
/// access signalling (`ca`). The relayed payload is unscrambled in either
/// case (transport_scrambling_control '00'; scrambled input is refused).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
#[non_exhaustive]
pub enum CaMode {
    /// Carry it (default): the source PMT's conditional_access_descriptors
    /// (H.222.0 §2.6.16; program and ES loops) go into the generated PMT,
    /// the ECM PIDs they point at are relayed, and the source CAT's
    /// CA_descriptors go into the generated CAT (with the EMM PIDs that are
    /// present in the source). CA_PIDs follow the PID mapping. Receivers
    /// see CA signalling with a clear payload.
    #[default]
    Keep,
    /// Drop it: no CA descriptors, ECM or EMM from the source.
    Drop,
}

impl CaMode {
    /// True for the default (`"keep"`).
    pub fn is_keep(&self) -> bool {
        *self == CaMode::Keep
    }
}

/// What a `ts-program` stream does when its rate exceeds the capacity of
/// its layer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
#[non_exhaustive]
pub enum RateCheck {
    /// Refuse the configuration, listing layer layouts that fit (default).
    #[default]
    Error,
    /// Warn and relay anyway (the stream falls behind its PCR schedule).
    Warn,
}

/// One elementary stream.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StreamCfg {
    /// Kind.
    pub kind: StreamKind,
    /// elementary_PID (not used by `ts-program`, whose PIDs come from the
    /// source PMT and `pid_map`).
    #[serde(default)]
    pub pid: u16,
    /// stream_type (H.222.0 Table 2-34); default 0x02 video, 0x0F AAC ADTS.
    #[serde(default)]
    pub stream_type: Option<u8>,
    /// component_tag → stream_identifier_descriptor (B10 §6.2.16).
    #[serde(default)]
    pub component_tag: Option<u8>,
    /// Source file (relative to the config file).
    #[serde(default)]
    pub source: Option<String>,
    /// Layer carrying this stream (default: the service's layer); for
    /// hierarchical (HQ/LQ) services whose components sit on different layers.
    #[serde(default)]
    pub layer: Option<LayerName>,
    /// Hierarchical transmission descriptor (B10 §6.2.22) in the PMT ES loop.
    #[serde(default)]
    pub hierarchical: Option<HierarchicalCfg>,
    /// Raw ES_info descriptors.
    #[serde(default)]
    pub descriptors: Vec<RawDescriptor>,
    /// Video decode control descriptor (B10 §6.2.30) in the PMT ES loop:
    /// `{ mode = "derived" | "explicit" | "off", still_picture,
    /// sequence_end_code, video_encode_format, transfer_characteristics }`
    /// ([`VideoDecodeControlCfg`]; default derived).
    #[serde(
        default,
        deserialize_with = "crate::signalling::de_video_decode_control"
    )]
    pub video_decode_control: VideoDecodeControlCfg,
    /// AAC frames per PES packet.
    #[serde(default = "one")]
    pub frames_per_pes: u32,
    /// Rewrite vbv_delay to 0xFFFF (B32 Part 1 §5.1.1: "vbv_delay shall
    /// always be set to 0xFFFF").
    #[serde(default = "yes")]
    pub force_vbv_delay_ffff: bool,
    /// Set broken_link in the first GOP header of an open GOP at each
    /// (re)start of the file.
    #[serde(default = "yes")]
    pub mark_broken_link: bool,
    /// Generate EIT component descriptors (0x50 video / 0xC4 audio) from the ES.
    #[serde(default = "yes")]
    pub eit_component: bool,
    /// component_type of the EIT component descriptor: "derived" (default),
    /// a B10 Table 6-5 name (video) or an audio table (Table 6-43)
    /// ([`ComponentTypeCfg`]).
    #[serde(default)]
    pub component_type: ComponentTypeCfg,
    /// quality_indicator of the EIT audio component descriptor (B10
    /// Table 6-44, modes defined by B32 Part 2 Attachment Ch. 3 Table 1):
    /// `"derived"` (default, from the measured bit rate, channels and
    /// sampling frequency) or `"mode-1"` / `"mode-2"` / `"mode-3"`
    /// ([`AudioQualityIndicator`]).
    #[serde(default)]
    pub audio_quality_indicator: AudioQualityIndicator,
    /// What to do with an `aac-adts` file whose headers violate the B32
    /// Part 2 §5.2 ADTS restrictions: `"conform"` (default: send a
    /// conforming file unchanged, refuse anything else — crc_check is
    /// never inserted and a protected header is never rewritten),
    /// `"warn"` (send unchanged, report) or `"pass-through"` (send
    /// unchanged, no check) ([`AdtsCompliance`]).
    #[serde(default)]
    pub adts_compliance: AdtsCompliance,
    /// `ts-program`: program_number in the source TS's PAT; `service_id` is
    /// accepted as an alias (B10 §5.2.6: "The service_id is the same as the
    /// program_number"). Default: the source's only program, or the only
    /// one whose elementary streams carry packets; otherwise the probe
    /// refuses with the list of programs.
    #[serde(default, alias = "service_id")]
    pub program: Option<u16>,
    /// `ts-program`: select the source program by its SDT service name
    /// (service_descriptor; exact match first, then a unique substring,
    /// full-width ASCII and case folded). Exclusive with `program`.
    #[serde(default)]
    pub service_name: Option<String>,
    /// `ts-program`: relay the elementary streams with these stream_types
    /// (default: MPEG-1/2 and H.264/HEVC video 0x01/0x02/0x1B/0x24,
    /// MPEG audio / AAC 0x03/0x04/0x0F/0x11, PES private data 0x06 —
    /// captions, superimpose, data PES).
    #[serde(default)]
    pub stream_types: Option<Vec<u8>>,
    /// `ts-program`: relay only elementary streams whose
    /// stream_identifier_descriptor carries one of these component_tags.
    #[serde(default)]
    pub component_tags: Option<Vec<u8>>,
    /// `ts-program`: also relay section-carrying streams (stream_type 0x05
    /// private sections, 0x0B–0x0D DSM-CC, i.e. data carousels), sent
    /// packet by packet. Off by default.
    #[serde(default)]
    pub include_carousels: bool,
    /// `ts-program`: output PIDs of the relayed streams. `"auto"` (default):
    /// by stream role (video, audio, captions, superimpose, data; see
    /// [`crate::relay::Role`]) from `pids`; `"keep"`: the source PIDs;
    /// `[[source_pid, output_pid], …]`: explicit, every source PID must be
    /// a relayed elementary stream of the program, unlisted relayed
    /// streams keep their source PID.
    #[serde(default)]
    pub pid_map: PidMap,
    /// `ts-program` with `pid_map = "auto"`: the PID plan per role
    /// (default [`PidPlan::default`]: video 0x0111, audio 0x0112,
    /// captions 0x0130, superimpose 0x0138, data 0x0140; an operational
    /// choice, see SPEC-NOTES).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pids: Option<PidPlan>,
    /// `ts-program`: the source's conditional access signalling, `"keep"`
    /// (default) or `"drop"` ([`CaMode`]).
    #[serde(default, skip_serializing_if = "CaMode::is_keep")]
    pub ca: CaMode,
    /// `ts-program`: `"loop"` (default) or `"end"` at the end of the file.
    #[serde(default)]
    pub at_eof: AtEof,
    /// `ts-program`: take the service's EIT present/following events (name,
    /// text, genre, duration) from the source EIT p/f of the program,
    /// re-encoded; the configured events are used when this is off or the
    /// source has none.
    #[serde(default)]
    pub eit_from_source: bool,
    /// `ts-program`: `"error"` (default) or `"warn"` when the measured rate
    /// exceeds the layer capacity.
    #[serde(default)]
    pub rate_check: RateCheck,
    /// `ts-program`: a packet may be sent up to this long before its
    /// PCR-derived time when the H.222.0 T-STD model of its stream accepts
    /// it (smooths rate peaks into idle slots); 0 = strictly at the
    /// source's pace.
    #[serde(default)]
    pub max_early_ms: u32,
    /// `ts-program`: seconds of the source's PCR time measured at start for
    /// the rate check and stream parameters (default 30).
    #[serde(default)]
    pub probe_seconds: Option<f64>,
    /// Set by the multiplexer on the streams it expands a `ts-program`
    /// into: (relay index, elementary stream index).
    #[serde(skip)]
    pub relay_es: Option<(usize, usize)>,
}

impl StreamCfg {
    /// Video, audio or other, for the EIT component descriptor choice:
    /// from the kind, or for relayed streams from the stream_type.
    pub fn media_class(&self) -> MediaClass {
        match self.kind {
            StreamKind::Mpeg2Video => MediaClass::Video,
            StreamKind::AacAdts => MediaClass::Audio,
            StreamKind::Declared => MediaClass::Other,
            StreamKind::TsProgram => match self.stream_type {
                Some(0x01 | 0x02 | 0x1B | 0x24) => MediaClass::Video,
                Some(0x03 | 0x04 | 0x0F | 0x11) => MediaClass::Audio,
                _ => MediaClass::Other,
            },
        }
    }
}

/// Coarse class of an elementary stream ([`StreamCfg::media_class`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaClass {
    /// Video.
    Video,
    /// Audio.
    Audio,
    /// Anything else (data, captions, declared).
    Other,
}

/// Output PIDs of a `ts-program` relay (`pid_map`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum PidMap {
    /// By stream role from the [`PidPlan`] (TOML `"auto"`, the default).
    #[default]
    Auto,
    /// The source PIDs (TOML `"keep"`).
    Keep,
    /// `[[source_pid, output_pid], …]`; relayed streams not listed keep
    /// their source PID.
    List(Vec<[u16; 2]>),
}

impl PidMap {
    /// True for the default (`"auto"`).
    pub fn is_auto(&self) -> bool {
        matches!(self, PidMap::Auto)
    }
}

impl Serialize for PidMap {
    fn serialize<S: serde::Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        match self {
            PidMap::Auto => s.serialize_str("auto"),
            PidMap::Keep => s.serialize_str("keep"),
            PidMap::List(v) => v.serialize(s),
        }
    }
}

impl<'de> Deserialize<'de> for PidMap {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        struct V;
        impl<'de> serde::de::Visitor<'de> for V {
            type Value = PidMap;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("\"auto\", \"keep\" or a list of [source_pid, output_pid] pairs")
            }
            fn visit_str<E: serde::de::Error>(self, v: &str) -> std::result::Result<PidMap, E> {
                match v {
                    "auto" => Ok(PidMap::Auto),
                    "keep" => Ok(PidMap::Keep),
                    _ => Err(E::invalid_value(serde::de::Unexpected::Str(v), &self)),
                }
            }
            fn visit_seq<A: serde::de::SeqAccess<'de>>(
                self,
                seq: A,
            ) -> std::result::Result<PidMap, A::Error> {
                let v: Vec<[u16; 2]> =
                    Deserialize::deserialize(serde::de::value::SeqAccessDeserializer::new(seq))?;
                Ok(PidMap::List(v))
            }
        }
        d.deserialize_any(V)
    }
}

/// One PID or a list of PIDs (TOML `0x0112` or `[0x0112, 0x0113]`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PidList(pub Vec<u16>);

impl Serialize for PidList {
    fn serialize<S: serde::Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        match self.0.as_slice() {
            [one] => s.serialize_u16(*one),
            v => v.serialize(s),
        }
    }
}

impl<'de> Deserialize<'de> for PidList {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        struct V;
        impl<'de> serde::de::Visitor<'de> for V {
            type Value = PidList;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("a PID or a list of PIDs")
            }
            fn visit_i64<E: serde::de::Error>(self, v: i64) -> std::result::Result<PidList, E> {
                u16::try_from(v)
                    .map(|p| PidList(vec![p]))
                    .map_err(|_| E::invalid_value(serde::de::Unexpected::Signed(v), &self))
            }
            fn visit_u64<E: serde::de::Error>(self, v: u64) -> std::result::Result<PidList, E> {
                u16::try_from(v)
                    .map(|p| PidList(vec![p]))
                    .map_err(|_| E::invalid_value(serde::de::Unexpected::Unsigned(v), &self))
            }
            fn visit_seq<A: serde::de::SeqAccess<'de>>(
                self,
                seq: A,
            ) -> std::result::Result<PidList, A::Error> {
                let v: Vec<u16> =
                    Deserialize::deserialize(serde::de::value::SeqAccessDeserializer::new(seq))?;
                Ok(PidList(v))
            }
        }
        d.deserialize_any(V)
    }
}

/// Output PIDs per stream role for `pid_map = "auto"`: the streams of a
/// role, in component_tag order (then PMT order), take the role's PIDs in
/// turn; streams beyond the list take the next free PIDs after its last
/// entry. Defaults: the PIDs terrestrial broadcasters use for these
/// components — an operational choice, not a spec value (SPEC-NOTES).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct PidPlan {
    /// Video (stream_type 0x01/0x02/0x1B/0x24). Default 0x0111.
    pub video: PidList,
    /// Audio, main first (stream_type 0x03/0x04/0x0F/0x11). Default 0x0112
    /// (a second audio stream gets 0x0113).
    pub audio: PidList,
    /// Captions (PES private data, data_component_id 0x0008). Default 0x0130.
    pub captions: PidList,
    /// Superimpose (PES private data, data_component_id 0x0008). Default 0x0138.
    pub superimpose: PidList,
    /// Other data: data PES, carousels (with `include_carousels`), streams
    /// selected by `stream_types` outside the roles above. Default 0x0140.
    pub data: PidList,
}

impl Default for PidPlan {
    fn default() -> Self {
        Self {
            video: PidList(vec![0x0111]),
            audio: PidList(vec![0x0112]),
            captions: PidList(vec![0x0130]),
            superimpose: PidList(vec![0x0138]),
            data: PidList(vec![0x0140]),
        }
    }
}

impl PidPlan {
    /// Every PID of the plan.
    pub fn all(&self) -> impl Iterator<Item = u16> + '_ {
        [
            &self.video,
            &self.audio,
            &self.captions,
            &self.superimpose,
            &self.data,
        ]
        .into_iter()
        .flat_map(|l| l.0.iter().copied())
    }
}

/// Hierarchical transmission descriptor fields (B10 §6.2.22).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HierarchicalCfg {
    /// reference_PID: the elementary stream of the other quality level.
    pub reference_pid: u16,
    /// quality_level ('1' high). Default: derived from the transmission
    /// parameters — the stream on the less robust layer (B31 Part 2
    /// Table 3-2 ranking) is the high-quality one.
    #[serde(default)]
    pub high_quality: Option<bool>,
}

/// One event (EIT).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EventCfg {
    /// event_id.
    pub event_id: u16,
    /// Start in JST `YYYY-MM-DDTHH:MM:SS`; or use `start_offset_s`.
    #[serde(default)]
    pub start: Option<String>,
    /// Start relative to the stream clock start (s). If neither is given:
    /// previous event's end, or the start time floored to the hour.
    #[serde(default)]
    pub start_offset_s: Option<i64>,
    /// Duration `HH:MM:SS`.
    pub duration: String,
    /// Event name (short event descriptor).
    pub name: String,
    /// Event text (short event descriptor).
    #[serde(default)]
    pub text: String,
    /// Genre bytes content_nibble_level_1 << 4 | level_2 (content descriptor).
    #[serde(default)]
    pub content: Vec<u8>,
    /// Raw event descriptors.
    #[serde(default)]
    pub descriptors: Vec<RawDescriptor>,
}

fn yes() -> bool {
    true
}
fn one() -> u32 {
    1
}
fn seven() -> u8 {
    7
}
fn default_abi() -> u8 {
    0x01
}
fn default_service_type() -> u8 {
    0x01
}
fn default_no_pcr() -> u16 {
    0x1FFF
}
fn default_udf() -> u8 {
    0x7
}

impl Config {
    /// Parse TOML text and [`Config::validate`] it.
    pub fn from_toml(text: &str) -> Result<Self> {
        let c: Self = toml::from_str(text).map_err(|e| MuxError::Config(e.to_string()))?;
        c.validate()?;
        Ok(c)
    }

    /// Serialise to TOML.
    pub fn to_toml(&self) -> Result<String> {
        toml::to_string_pretty(self).map_err(|e| MuxError::Config(e.to_string()))
    }

    /// original_network_id (defaults to network_id).
    pub fn original_network_id(&self) -> u16 {
        self.transport
            .original_network_id
            .unwrap_or(self.network.network_id)
    }
}

/// Decode a hex string (whitespace, ':' and '_' ignored).
pub fn parse_hex(s: &str) -> Result<Vec<u8>> {
    let digits: Vec<u8> = s
        .bytes()
        .filter(|b| !b.is_ascii_whitespace() && *b != b':' && *b != b'_')
        .collect();
    if !digits.len().is_multiple_of(2) {
        return Err(MuxError::Config(format!(
            "hex string has an odd number of digits: {s:?}"
        )));
    }
    digits
        .chunks(2)
        .map(|c| {
            let h =
                std::str::from_utf8(c).map_err(|_| MuxError::Config(format!("bad hex {s:?}")))?;
            u8::from_str_radix(h, 16)
                .map_err(|_| MuxError::Config(format!("bad hex digits {h:?} in {s:?}")))
        })
        .collect()
}

/// The example configuration shipped as `examples/bench.toml`.
pub const EXAMPLE_TOML: &str = include_str!("../examples/bench.toml");

#[cfg(test)]
mod tests {
    use super::*;

    /// `[eit_schedule] reference` is one of the B10 Part 2 Annex 1.4.2
    /// methods; any other string is refused when parsing.
    #[test]
    fn eit_schedule_reference_is_validated() {
        let base = EXAMPLE_TOML;
        let with = |r: &str| {
            base.replace(
                "[eit_schedule]\n",
                &format!("[eit_schedule]\nreference = \"{r}\"\n"),
            )
        };
        let c = Config::from_toml(&with("month")).unwrap();
        assert_eq!(c.eit_schedule.reference, EitScheduleReference::Month);
        let c = Config::from_toml(&with("today")).unwrap();
        assert_eq!(c.eit_schedule.reference, EitScheduleReference::Today);
        let e = Config::from_toml(&with("week")).unwrap_err().to_string();
        assert!(e.contains("week") && e.contains("today"), "{e}");
    }
}
