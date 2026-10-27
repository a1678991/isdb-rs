//! `kind = "ts-program"`: rebroadcast one program of a recorded transport
//! stream as a service of this multiplex.
//!
//! A TS of another network relayed as is cannot be registered by a
//! terrestrial receiver: its NIT/SDT describe that network. Here only the
//! program's elementary streams (and, with `ca = "keep"`, its ECM / EMM
//! PIDs) are taken over, and everything around them is this multiplexer's
//! own. The input must be unscrambled (transport_scrambling_control '00'):
//!
//! * **Selection** (source PMT, H.222.0 §2.4.4.8): elementary streams by
//!   stream_type ([`DEFAULT_STREAM_TYPES`]: video, audio, PES private data
//!   for captions / superimpose / data PES) and optionally component_tag;
//!   section streams (0x05, 0x0B–0x0D carousels) only with
//!   `include_carousels`. Scrambled packets (transport_scrambling_control
//!   ≠ '00') are refused at the probe.
//! * **Conditional access** (`ca`, [`CaMode`]): with `"keep"` (default) the
//!   source PMT's conditional_access_descriptors (H.222.0 §2.6.16, program
//!   and relayed ES loops) go into the generated PMT, the ECM PIDs they
//!   name are relayed packet by packet, and the source CAT's CA_descriptors
//!   go into the generated CAT with the EMM PIDs present in the source;
//!   every CA_PID follows the PID mapping ([`SourceCa`]). The payload is
//!   unscrambled, so receivers see CA signalling with a clear payload.
//!   `"drop"` relays none of it.
//! * **PSI/SI**: PAT/PMT/CAT/NIT/SDT/EIT/TOT/BIT are generated from the
//!   service configuration as for any service (terrestrial delivery, typed
//!   copy control). The source's copy control is read from its PMT program
//!   loop, relayed ES loops, SDT and EIT p/f (0xC1 and 0xDE,
//!   [`SourceCopyControl`]) and reported; with `recording_control =
//!   "source"` (the `--relay` default) the service's
//!   digital_copy_control_descriptor takes the source's
//!   digital_recording_control_data (precedence PMT program loop > ES
//!   loops > SDT > EIT) and the relayed ES keep their per-component 0xC1;
//!   an explicit value is used as given. ES descriptors are carried over
//!   *by type* through `isdb_si::Descriptor` when meaningful for the new
//!   PMT ([`carried_over`]: stream_identifier 0x52, video_decode_control
//!   0xC8, data_component 0xFD); the others (hierarchical 0xC0 with its
//!   source PID, unknown tags) are dropped and listed in the probe report.
//! * **PES bytes intact**: every source packet of a relayed PID becomes one
//!   output packet with the same payload bytes (same chunking), only the
//!   PTS/DTS fields (their 33-bit values; prefix and marker bits kept) are
//!   rewritten, PID renumbered by `pid_map`, continuity_counter renumbered,
//!   and the adaptation field rebuilt without PCR/OPCR (random_access_indicator
//!   kept where the field has room).
//! * **Timing** (H.222.0 §2.4.2.2): every source packet gets its arrival
//!   time interpolated from the program's PCR ([`crate::pcrclock`], PCR
//!   wraps unwrapped, discontinuities bridged). Mux time = that time + a
//!   constant `base` (+ one loop period per pass), and PTS/DTS are moved
//!   by the same constant, so each PES keeps the source's arrival→decode
//!   distance (the T-STD behaviour the source multiplexer produced). The
//!   new PCR is this multiplex's frame clock as for every service; a
//!   packet is sent in the first free slot at or after its time (or up to
//!   `max_early_ms` before it when the stream's T-STD model accepts it), so
//!   the ES leave at the source's rate. A due packet whose T-STD buffer
//!   would overflow waits up to [`RETIME_HOLD`] (the output slots are not
//!   the source's, see there).
//! * **Cut-in and loop**: a pass starts at the first video PES that begins
//!   with a sequence_header (an access point; with an open GOP the leading
//!   B-picture PES that reference the previous GOP are dropped whole) and
//!   ends with the last complete PES of each PID. With `at_eof = "loop"`
//!   the next pass is shifted by one period P = max(arrival span, per-PID
//!   PTS span + one PTS step), so arrival times and every PID's PTS stay
//!   monotonic (the approach of the media loops); `"end"` stops.

use std::collections::VecDeque;
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, sync_channel};

use isdb_si::Descriptor;
use isdb_si::tables::{EitBody, SdtBody, TdtBody, TotBody};
use isdb_si::values::{CodedValue, DigitalRecordingControl};
use isdb_ts::Pid;
use isdb_ts::cc::Counter;
use isdb_ts::demux::{Assembled, SectionDemux};
use isdb_ts::packet::{AdaptationFields, Packet as TsPacket};
use isdb_ts::pcr::BASE_MODULUS;
use isdb_ts::psi::{Cat, Pat, Pmt};
use isdb_ts::reader::StreamReader;
use isdb_ts::section::{Long, Short};

use crate::audio::AdtsEs;
use crate::config::{
    AtEof, CaMode, EventCfg, MediaClass, PidMap, PidPlan, RawDescriptor, StreamCfg, StreamKind,
    VdcMode, VideoDecodeControlCfg,
};
use crate::error::{MuxError, Result};
use crate::jst::{self, JstSeconds};
use crate::mux::Packet;
use crate::pcrclock::{Anchor, HZ, PcrClock, Timed, packet_pid};
use crate::playback::{AudioSlots, PlaybackStatus, Seg, Slot, Timeline, VideoThinning};
use crate::si::StreamInfo;
use crate::tstd::{Tstd, TstdParams};
use crate::video::{GOP_CLOSED, SequenceInfo, VideoEs};

/// stream_types relayed by default: MPEG-1/2 video 0x01/0x02, H.264 0x1B,
/// HEVC 0x24, MPEG-1/2 audio 0x03/0x04, AAC ADTS 0x0F, AAC LATM 0x11, and
/// PES private data 0x06 (ARIB captions / superimpose / data PES) —
/// H.222.0 Table 2-34, B32 Part 3 §3.10 (2).
pub const DEFAULT_STREAM_TYPES: &[u8] = &[0x01, 0x02, 0x1B, 0x24, 0x03, 0x04, 0x0F, 0x11, 0x06];
/// Section-carrying stream_types (private sections 0x05, DSM-CC 0x0B–0x0D,
/// i.e. data carousels): relayed packet by packet, only on request.
pub const CAROUSEL_STREAM_TYPES: &[u8] = &[0x05, 0x0B, 0x0C, 0x0D];
/// Seconds of PCR time measured by the probe by default.
pub const DEFAULT_PROBE_SECONDS: f64 = 30.0;
/// Upper bound of the bytes read by the probe.
const PROBE_MAX_BYTES: usize = 256 << 20;
/// Packets per batch from the reader thread.
const BATCH: usize = 2048;
/// How far past the current slot the source is read and timed. A PES is
/// sent only once complete, and its bytes span at most 1 s in a conformant
/// source (H.222.0 §2.4.2.6: "tdn(j) – t(i) ≤ 1 second"), so 1.2 s of
/// look-ahead has every PES complete before its first packet is due.
const LOOKAHEAD: u64 = HZ * 6 / 5;
/// Largest PTS gap the loop seam may add (H.222.0 §2.7.4 allows 0.7 s).
const MAX_SEAM_GAP: u64 = HZ / 2;
/// A due packet held back by its T-STD model is sent anyway once its PES
/// decodes within this time plus the time the rest of the PES needs.
pub(crate) const HOLD_MARGIN: u64 = HZ / 10;
/// After a loop seam, packets of the new pass whose source time is within
/// this window of its start may be held by the T-STD model: the pass head
/// was multiplexed at the file start for a buffer state the previous
/// pass's tail does not match. Elsewhere the source timing is kept as is.
const SEAM_WINDOW: u64 = 2 * HZ;
/// Outside the seam window, a due packet whose T-STD buffer would overflow
/// is held at most this long past its source time. The relay's slots are
/// not the source's: a packet leaves in the first free slot of its layer
/// at or after its time, so relayed packets arrive 0–1 ms later than the
/// source's own packets, unevenly, and their spacing shrinks by up to a
/// few slots. A source multiplexed with no headroom in MBn/EBn (such as
/// one made by this crate, whose scheduler fills the buffers to their
/// limits) then overflows the model by a packet or two at those points;
/// this short hold absorbs that re-timing jitter. A source whose own
/// timing overflows the model still does after the hold (the relay keeps
/// the source's pace instead of re-timing to hide it).
pub const RETIME_HOLD: u64 = HZ / 500;

/// Whether a source ES descriptor is carried over by type into the new PMT:
/// stream_identifier (B10 §6.2.16; links the ES to EIT component
/// descriptors and data broadcasting), video_decode_control (§6.2.30),
/// data_component (§6.2.20; tells a receiver which caption / data coding
/// the PES carries). CA (0x09) and copy control (0xC1) are carried as
/// bytes depending on `ca` / `recording_control` ([`RelayedEs`]);
/// everything else (hierarchy with source PIDs, unknown tags) is dropped.
pub fn carried_over(d: &Descriptor) -> bool {
    matches!(
        d,
        Descriptor::StreamIdentifier(_)
            | Descriptor::VideoDecodeControl(_)
            | Descriptor::DataComponent(_)
    )
}

/// How a relayed PID is carried.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EsMode {
    /// PES: assembled, PTS/DTS rebased, sent with the source chunking.
    Pes,
    /// Sections (carousels): packet by packet, unchanged payload.
    Packets,
}

/// Where a copy-control signal of the source program was found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum CopySignalPlace {
    /// The PMT program loop (B10 §6.2.23: "for the whole event").
    PmtProgram,
    /// The ES loop of a relayed elementary stream (source PID).
    PmtEs(Pid),
    /// The SDT service loop of the program (B10 §5.2.6).
    Sdt,
    /// The EIT present event of the program (B10 §5.2.7).
    EitPresent,
    /// The EIT following event of the program.
    EitFollowing,
}

impl CopySignalPlace {
    /// Short label: `PMT`, `PMT ES 0x0140`, `SDT`, `EIT present`, `EIT following`.
    pub fn label(&self) -> String {
        match self {
            CopySignalPlace::PmtProgram => "PMT".into(),
            CopySignalPlace::PmtEs(p) => format!("PMT ES {p:#06x}"),
            CopySignalPlace::Sdt => "SDT".into(),
            CopySignalPlace::EitPresent => "EIT present".into(),
            CopySignalPlace::EitFollowing => "EIT following".into(),
        }
    }
}

/// A copy-control signal of the source program.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum CopySignal {
    /// digital_copy_control_descriptor 0xC1 (B10 §6.2.23):
    /// digital_recording_control_data (Table 6-39).
    Recording(DigitalRecordingControl),
    /// content_availability_descriptor 0xDE (B10 §6.2.45).
    Availability(isdb_si::descriptor::ContentAvailability),
}

/// One copy-control signal and where it was found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceCopyFinding {
    /// Where.
    pub place: CopySignalPlace,
    /// What.
    pub signal: CopySignal,
}

impl SourceCopyFinding {
    /// One item for reports, e.g. `PMT copy-one-generation ('10')`.
    pub fn describe(&self) -> String {
        match &self.signal {
            CopySignal::Recording(v) => {
                format!("{} {} ('{:02b}')", self.place.label(), v.name(), v.code())
            }
            CopySignal::Availability(c) => format!(
                "{} content_availability (0xDE) copy_restriction_mode {}, image_constraint_token {}, retention_mode {}, retention_state {}, encryption_mode {}",
                self.place.label(),
                u8::from(c.copy_restriction_mode),
                u8::from(c.image_constraint_token),
                u8::from(c.retention_mode),
                c.retention_state,
                u8::from(c.encryption_mode)
            ),
        }
    }
}

/// The copy-control signalling of the relayed source program: every
/// digital_copy_control_descriptor (0xC1) and content_availability_descriptor
/// (0xDE) found in its PMT program loop, the ES loops of the relayed
/// streams, its SDT service loop and its EIT present/following events
/// (first version of each seen by the probe).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SourceCopyControl {
    /// Everything found, in PMT program, ES, SDT, EIT order.
    pub findings: Vec<SourceCopyFinding>,
}

impl SourceCopyControl {
    /// The digital_recording_control_data a relay with `recording_control =
    /// "source"` keeps, and where it comes from. Precedence: the PMT
    /// program loop (B10 §6.2.23: the program-loop descriptor applies to
    /// the whole event), else the most restrictive (largest code: '00'
    /// free < '01' provider-defined < '10' one generation < '11' never)
    /// of the relayed ES loops, else the SDT, else the EIT present event,
    /// else the following event. `None`: no 0xC1 anywhere.
    pub fn effective(&self) -> Option<(DigitalRecordingControl, CopySignalPlace)> {
        let pick = |want: &dyn Fn(&CopySignalPlace) -> bool| {
            self.findings
                .iter()
                .filter(|f| want(&f.place))
                .filter_map(|f| match f.signal {
                    CopySignal::Recording(v) => Some((v, f.place)),
                    CopySignal::Availability(_) => None,
                })
                .max_by_key(|(v, _)| v.code())
        };
        pick(&|p| *p == CopySignalPlace::PmtProgram)
            .or_else(|| pick(&|p| matches!(p, CopySignalPlace::PmtEs(_))))
            .or_else(|| pick(&|p| *p == CopySignalPlace::Sdt))
            .or_else(|| pick(&|p| *p == CopySignalPlace::EitPresent))
            .or_else(|| pick(&|p| *p == CopySignalPlace::EitFollowing))
    }

    /// The effective value for reports: `copy-one-generation (PMT)`, or
    /// `none signalled`.
    pub fn describe(&self) -> String {
        match self.effective() {
            Some((v, place)) => format!("{} ({})", v.name(), place.label()),
            None => "none signalled".into(),
        }
    }

    /// Every finding, `; `-separated (`none` when empty).
    pub fn findings_text(&self) -> String {
        if self.findings.is_empty() {
            return "none".into();
        }
        self.findings
            .iter()
            .map(SourceCopyFinding::describe)
            .collect::<Vec<_>>()
            .join("; ")
    }

    pub(crate) fn push_from<'a>(
        &mut self,
        place: CopySignalPlace,
        ds: impl IntoIterator<Item = &'a Descriptor>,
    ) {
        for d in ds {
            let signal = match d {
                Descriptor::DigitalCopyControl(c) => CopySignal::Recording(
                    DigitalRecordingControl::from_code(c.digital_recording_control_data),
                ),
                Descriptor::ContentAvailability(c) => CopySignal::Availability(c.clone()),
                _ => continue,
            };
            self.findings.push(SourceCopyFinding { place, signal });
        }
    }
}

/// Kind of a relayed conditional-access PID.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum CaPidKind {
    /// ECM: CA_PID of a PMT conditional_access_descriptor (H.222.0 §2.6.16).
    Ecm,
    /// EMM: CA_PID of a CAT CA_descriptor (H.222.0 §2.4.4.6).
    Emm,
}

/// An ECM or EMM PID relayed with `ca = "keep"`: its packets go out packet
/// by packet with unchanged payload, like a carousel.
#[derive(Debug, Clone, PartialEq)]
pub struct RelayedCaPid {
    /// Source PID.
    pub src_pid: Pid,
    /// Output PID (the CA_PID of the generated descriptor).
    pub pid: Pid,
    /// ECM or EMM.
    pub kind: CaPidKind,
    /// CA_system_ID of the descriptor that names it.
    pub ca_system_id: u16,
    /// Mean rate over the probe (bit/s of 188-byte packets).
    pub mean_bps: f64,
}

/// The conditional-access signalling of the source program and what the
/// relay does with it (`ca`, [`CaMode`]).
#[derive(Debug, Clone, Default)]
pub struct SourceCa {
    /// `keep` or `drop`.
    pub mode: CaMode,
    /// conditional_access_descriptors of the source PMT program loop, CA_PID
    /// remapped (empty with `drop`).
    pub program_descriptors: Vec<RawDescriptor>,
    /// CA_descriptors of the source CAT, CA_PID remapped (empty with
    /// `drop` or when the probed part has no CAT).
    pub cat_descriptors: Vec<RawDescriptor>,
    /// The source has a CAT in the probed part.
    pub source_has_cat: bool,
    /// ECM / EMM PIDs relayed.
    pub pids: Vec<RelayedCaPid>,
    /// free_CA_mode of the program in the source SDT, if seen.
    pub source_free_ca_mode: Option<bool>,
    /// Remarks for the report (descriptors whose CA_PID is not relayed …).
    pub remarks: Vec<String>,
}

impl SourceCa {
    /// True when the source signals conditional access anywhere (PMT, CAT).
    pub fn signalled(&self) -> bool {
        !self.program_descriptors.is_empty()
            || !self.cat_descriptors.is_empty()
            || !self.pids.is_empty()
    }
}

/// CA_system_ID and CA_PID of a raw conditional_access_descriptor body
/// (H.222.0 §2.6.16, Table 2-60).
fn ca_fields(data: &[u8]) -> Option<(u16, Pid)> {
    (data.len() >= 4).then(|| {
        (
            u16::from_be_bytes([data[0], data[1]]),
            Pid::from_wire(u16::from_be_bytes([data[2], data[3]])),
        )
    })
}

/// A conditional_access_descriptor with its CA_PID mapped by `map` (the 3
/// reserved bits and the private data kept).
fn remap_ca(data: &[u8], map: impl Fn(Pid) -> Pid) -> RawDescriptor {
    let mut b = data.to_vec();
    if let Some((_, pid)) = ca_fields(&b) {
        let np = map(pid).get();
        b[2] = (b[2] & 0xE0) | (np >> 8) as u8;
        b[3] = np as u8;
    }
    RawDescriptor {
        tag: 0x09,
        hex: hex(&b),
    }
}

/// One elementary stream chosen from the source program.
#[derive(Debug, Clone)]
pub struct RelayedEs {
    /// Source PID.
    pub src_pid: Pid,
    /// Output PID (by role from the PID plan, or after `pid_map`).
    pub pid: Pid,
    /// stream_type.
    pub stream_type: u8,
    /// component_tag of its stream_identifier_descriptor.
    pub component_tag: Option<u8>,
    /// Role in the service ([`classify`]).
    pub role: Role,
    /// The classification rule that decided the role.
    pub role_reason: String,
    /// Position among the relayed streams of the same role (component_tag
    /// order, then PMT order): audio 0 is the main audio.
    pub rank: usize,
    /// Carried-over typed ES descriptors (stream_identifier included).
    pub descriptors: Vec<Descriptor>,
    /// Source ES descriptors not carried over (names).
    pub dropped_descriptors: Vec<String>,
    /// The source ES loop's conditional_access_descriptors, CA_PID
    /// remapped (empty with `ca = "drop"`).
    pub ca_descriptors: Vec<RawDescriptor>,
    /// The source ES loop's digital_copy_control_descriptors (per-component
    /// copy control, B10 §6.2.23), as is: carried when the service keeps
    /// the source's copy control (`recording_control = "source"`), dropped
    /// otherwise.
    pub copy_descriptors: Vec<RawDescriptor>,
    /// Carriage.
    pub mode: EsMode,
    /// Facts for the derived SI (video sequence / audio parameters).
    pub info: StreamInfo,
    /// T-STD buffers of the stream (video / audio), if known.
    pub tstd: Option<TstdParams>,
    /// Mean rate over the probe (bit/s of 188-byte packets).
    pub mean_bps: f64,
    /// The T-STD model (H.222.0 §2.4.2.3) run on the source's own timing
    /// over the probe: what the source multiplex does to a decoder with
    /// exactly the H.222.0 buffers (the relay keeps this timing).
    pub source_tstd: Option<crate::tstd::TstdViolations>,
    /// Arrival (first byte) to decoding time of the probed PES: min,
    /// median, max (s).
    pub source_lead_s: Option<(f64, f64, f64)>,
}

/// An event of the source EIT present/following of the program.
#[derive(Debug, Clone)]
pub struct SourceEvent {
    /// event_id.
    pub event_id: u16,
    /// Start (JST).
    pub start: JstSeconds,
    /// Duration (s).
    pub duration: u32,
    /// Event name (short_event_descriptor), decoded.
    pub name: String,
    /// Event text (short_event_descriptor), decoded.
    pub text: String,
    /// content_descriptor genres (level 1 << 4 | level 2).
    pub content: Vec<u8>,
}

impl RelayedEs {
    /// Role with its rank, e.g. `audio (main)`, `audio (secondary)`,
    /// `captions`, `captions #2`.
    pub fn role_label(&self) -> String {
        match (self.role, self.rank) {
            (Role::Audio, 0) => "audio (main)".into(),
            (Role::Audio, _) => "audio (secondary)".into(),
            (r, 0) => r.name().into(),
            (r, n) => format!("{} #{}", r.name(), n + 1),
        }
    }
}

/// What the start-up probe of a source TS found.
#[derive(Debug, Clone)]
pub struct Probe {
    /// Source path.
    pub path: PathBuf,
    /// program_number relayed.
    pub program: u16,
    /// How the program was chosen.
    pub chosen_by: String,
    /// Its SDT service name, if the source SDT names it.
    pub service_name: Option<String>,
    /// Every program of the source (PAT order).
    pub source_programs: Vec<SourceProgram>,
    /// Source PIDs present in the probed part and not relayed (other than
    /// the program's unselected elementary streams, see `skipped`), with
    /// the reason.
    pub dropped_pids: Vec<(Pid, String)>,
    /// Its PMT PID in the source.
    pub pmt_pid: Pid,
    /// Its PCR_PID in the source (the time base).
    pub pcr_pid: Pid,
    /// Elementary streams relayed.
    pub es: Vec<RelayedEs>,
    /// Elementary streams not relayed, with the reason.
    pub skipped: Vec<String>,
    /// Source program-level descriptors not carried as bytes (names):
    /// everything but the conditional_access_descriptors kept with
    /// `ca = "keep"` (copy control is regenerated from the typed
    /// configuration, see `source_copy_control`).
    pub dropped_program_descriptors: Vec<String>,
    /// The copy-control signalling of the source program (PMT, relayed ES
    /// loops, SDT, EIT p/f).
    pub source_copy_control: SourceCopyControl,
    /// The source's conditional-access signalling and what is relayed.
    pub ca: SourceCa,
    /// PCR time measured (s).
    pub span_s: f64,
    /// Mean rate of the relayed packets (bit/s).
    pub mean_bps: f64,
    /// Largest rate over a 1 s window (bit/s).
    pub peak_1s_bps: f64,
    /// The relayed packets over the whole file: the rate the relay needs
    /// (the capacity check and `--auto-layout` use it, not the probe's mean).
    pub rate: crate::relay_rate::RateProfile,
    /// Byte offsets of the file every 0.5 s of PCR time (for seeking).
    pub seek_index: std::sync::Arc<[crate::relay_rate::SeekPoint]>,
    /// Source EIT present event of the program.
    pub present: Option<SourceEvent>,
    /// Source EIT following event.
    pub following: Option<SourceEvent>,
    /// Source wall clock (TOT/TDT) at the relay's first access point.
    pub source_now: Option<JstSeconds>,
}

impl Probe {
    /// Multi-line human-readable report: the source program and how it was
    /// chosen, each source PID → role → output PID with its rate and
    /// descriptors, the streams and PIDs not relayed (with the reason),
    /// the measured rates.
    pub fn report(&self) -> String {
        let name = self
            .service_name
            .as_ref()
            .map(|n| format!(" \"{n}\""))
            .unwrap_or_default();
        let mut s = format!(
            "ts-program {}: program {:#06x}{name} ({}; PMT {:#06x}, PCR {:#06x}); {:.2} Mbit/s mean, {:.2} Mbit/s peak (1 s) over {:.1} s of PCR",
            self.path.display(),
            self.program,
            self.chosen_by,
            self.pmt_pid,
            self.pcr_pid,
            self.mean_bps / 1e6,
            self.peak_1s_bps / 1e6,
            self.span_s
        );
        s += &format!("\n  {}", self.rate.describe());
        if self.source_programs.len() > 1 {
            s += "\n  source programs:";
            for p in &self.source_programs {
                s += &format!("\n    {}", p.describe());
            }
        }
        if let Some(m) = self.ca.source_free_ca_mode {
            s += &format!("\n  source SDT free_CA_mode: {}", u8::from(m));
        }
        for e in &self.es {
            let timing = match (e.source_lead_s, e.source_tstd) {
                (Some((a, m, b)), Some(v)) => format!(
                    ", source arrival→decode {:.3}/{:.3}/{:.3} s (min/median/max), T-STD on the source timing: {}",
                    a,
                    m,
                    b,
                    if v.tb_overflow + v.buffer_overflow + v.underflow == 0 {
                        "ok".to_string()
                    } else {
                        format!(
                            "TB overflow {}, buffer overflow {}, underflow {} (packets of the probe)",
                            v.tb_overflow, v.buffer_overflow, v.underflow
                        )
                    }
                ),
                _ => String::new(),
            };
            s += &format!(
                "\n  relay {:#06x} -> {:#06x} {} ({}){} {:.3} Mbit/s, carried: [{}]{}{timing}",
                e.src_pid,
                e.pid,
                e.role_label(),
                e.role_reason,
                e.component_tag
                    .map(|t| format!(" component_tag {t:#04x}"))
                    .unwrap_or_default(),
                e.mean_bps / 1e6,
                e.descriptors
                    .iter()
                    .map(|d| d.name().to_string())
                    .chain(e.ca_descriptors.iter().map(|d| {
                        let b = parse_hex_lossy(&d.hex);
                        match ca_fields(&b) {
                            Some((sys, pid)) => format!(
                                "conditional_access_descriptor (CA_system_id {sys:#06x}, CA_PID {pid:#06x})"
                            ),
                            None => "conditional_access_descriptor".to_string(),
                        }
                    }))
                    .collect::<Vec<_>>()
                    .join(", "),
                if e.dropped_descriptors.is_empty() {
                    String::new()
                } else {
                    format!(", dropped: [{}]", e.dropped_descriptors.join(", "))
                }
            );
        }
        for k in &self.skipped {
            s += &format!("\n  not relayed: {k}");
        }
        for (p, why) in &self.dropped_pids {
            s += &format!("\n  dropped PID {p:#06x}: {why}");
        }
        if !self.dropped_program_descriptors.is_empty() {
            s += &format!(
                "\n  source program descriptors not carried: [{}]",
                self.dropped_program_descriptors.join(", ")
            );
        }
        s += &self.ca_report();
        s += &format!(
            "\n  source copy control: {} (found: {})",
            self.source_copy_control.describe(),
            self.source_copy_control.findings_text()
        );
        s
    }

    fn ca_report(&self) -> String {
        let ca = &self.ca;
        let mut s = String::new();
        let es_ca = self.es.iter().any(|e| !e.ca_descriptors.is_empty());
        if ca.mode == CaMode::Drop {
            return "\n  conditional access (ca = \"drop\"): source CA descriptors, ECM and EMM not relayed".into();
        }
        if !ca.signalled() && !es_ca {
            return s;
        }
        let list = |v: &[RawDescriptor]| {
            v.iter()
                .filter_map(|d| ca_fields(&parse_hex_lossy(&d.hex)))
                .map(|(sys, pid)| format!("CA_system_id {sys:#06x} CA_PID {pid:#06x}"))
                .collect::<Vec<_>>()
                .join(", ")
        };
        s += "\n  conditional access (ca = \"keep\"):";
        if !ca.program_descriptors.is_empty() {
            s += &format!(" PMT program loop [{}];", list(&ca.program_descriptors));
        }
        if es_ca {
            s += " ES loops (see the relayed streams);";
        }
        if ca.source_has_cat {
            s += &format!(" CAT [{}];", list(&ca.cat_descriptors));
        }
        for c in &ca.pids {
            s += &format!(
                " {} {:#06x} -> {:#06x} {:.3} Mbit/s;",
                match c.kind {
                    CaPidKind::Ecm => "ECM",
                    CaPidKind::Emm => "EMM",
                },
                c.src_pid,
                c.pid,
                c.mean_bps / 1e6
            );
        }
        for r in &ca.remarks {
            s += &format!(" {r};");
        }
        s += " the payload is unscrambled (transport_scrambling_control '00'): receivers see CA signalling with a clear payload";
        s
    }
}

fn desc_name(d: &isdb_ts::descriptor::Descriptor) -> String {
    match Descriptor::decode_body(d.tag, &d.data) {
        Ok(Descriptor::Unknown { tag, .. }) => format!("tag {tag:#04x}"),
        Ok(x) => format!("{} ({:#04x})", x.name(), d.tag),
        Err(_) => format!("tag {:#04x} (malformed)", d.tag),
    }
}

/// Payload of a raw packet (after header and adaptation field).
fn payload(p: &Packet) -> Option<&[u8]> {
    let afc = (p[3] >> 4) & 3;
    if afc & 1 == 0 {
        return None;
    }
    let start = if afc & 2 != 0 { 5 + p[4] as usize } else { 4 };
    (start <= 188).then(|| &p[start..])
}

/// ES bytes after the PES header (stream_ids with the optional header).
fn pes_es_start(pes: &[u8]) -> Option<usize> {
    if pes.len() < 9 || pes[..3] != [0, 0, 1] {
        return None;
    }
    if !isdb_ts::pes::has_optional_header(pes[3]) {
        return Some(6);
    }
    Some(9 + pes[8] as usize).filter(|&n| n <= pes.len())
}

/// The PES (first packet payload) starts an ES access point: its ES
/// begins with a sequence_header (H.262 start code 0xB3).
fn starts_with_sequence_header(pes: &[u8]) -> bool {
    pes_es_start(pes).is_some_and(|i| pes.len() >= i + 4 && pes[i..i + 4] == [0, 0, 1, 0xB3])
}

/// First picture_coding_type (1 I, 2 P, 3 B) in a video PES.
fn picture_coding_type(pes: &[u8]) -> Option<u8> {
    let i0 = pes_es_start(pes)?;
    let es = &pes[i0..];
    let mut i = 0;
    while i + 6 <= es.len() {
        if es[i] == 0 && es[i + 1] == 0 && es[i + 2] == 1 && es[i + 3] == 0x00 {
            return Some((es[i + 5] >> 3) & 7);
        }
        i += 1;
    }
    None
}

/// closed_gop of the first GOP header in a video PES (None: no GOP header).
fn closed_gop(pes: &[u8]) -> Option<bool> {
    let i0 = pes_es_start(pes)?;
    let es = &pes[i0..];
    let mut i = 0;
    while i + 8 <= es.len() {
        if es[i] == 0 && es[i + 1] == 0 && es[i + 2] == 1 {
            match es[i + 3] {
                0xB8 => return Some(es[i + 7] & GOP_CLOSED != 0),
                0x00 => return None, // picture before any GOP header
                _ => {}
            }
        }
        i += 1;
    }
    None
}

/// Length of the ADTS frame starting at `d[0]` (ISO/IEC 13818-7
/// adts_fixed_header syncword 0xFFF, layer '00'; aac_frame_length).
fn adts_len(d: &[u8]) -> Option<usize> {
    if d.len() < 7 || d[0] != 0xFF || d[1] & 0xF6 != 0xF0 {
        return None;
    }
    let n = ((d[3] as usize & 3) << 11) | (d[4] as usize) << 3 | (d[5] as usize) >> 5;
    (n >= 7).then_some(n)
}

/// The complete ADTS frames of `d` from its first syncword whose next two
/// frames also start with a syncword, up to the last complete frame.
fn adts_frames(d: &[u8]) -> Option<&[u8]> {
    let start = (0..d.len()).find(|&i| {
        let Some(a) = adts_len(&d[i..]) else {
            return false;
        };
        let Some(b) = d.get(i + a..).and_then(adts_len) else {
            return false;
        };
        d.get(i + a + b..).and_then(adts_len).is_some()
    })?;
    let mut end = start;
    while let Some(n) = adts_len(&d[end..]) {
        if end + n > d.len() {
            break;
        }
        end += n;
    }
    (end > start).then(|| &d[start..end])
}

fn read_ts(b: &[u8]) -> u64 {
    ((u64::from(b[0]) >> 1) & 7) << 30
        | u64::from(b[1]) << 22
        | (u64::from(b[2]) >> 1) << 15
        | u64::from(b[3]) << 7
        | u64::from(b[4]) >> 1
}

/// Write a 33-bit time stamp keeping the 4-bit prefix and marker bits
/// (H.222.0 Table 2-21 PTS/DTS layout).
fn write_ts(b: &mut [u8], v: u64) {
    let v = v % BASE_MODULUS;
    b[0] = (b[0] & 0xF1) | (((v >> 30) as u8 & 7) << 1);
    b[1] = (v >> 22) as u8;
    b[2] = (b[2] & 0x01) | (((v >> 15) as u8) << 1);
    b[3] = (v >> 7) as u8;
    b[4] = (b[4] & 0x01) | ((v as u8) << 1);
}

/// H.222.0 §2.4.2.3 T-STD buffers of an MPEG-2 video stream from its
/// sequence header (as for media files; see SPEC-NOTES "T-STD pacing").
pub fn video_tstd(seq: &SequenceInfo) -> TstdParams {
    let rmax = seq.rmax() as f64;
    let mbs = 0.004 * rmax / 8.0 + rmax / 750.0 / 8.0;
    let rbx = if seq.high_level() {
        (1.05 * seq.bit_rate as f64).min(rmax)
    } else {
        rmax
    };
    TstdParams {
        rx: 1.2 * rmax,
        mb: Some((mbs, rbx)),
        main: seq.vbv_buffer_bits as f64 / 8.0,
    }
}

fn read_probe_packets(path: &Path, probe_s: f64) -> Result<Vec<Packet>> {
    let f = std::fs::File::open(path).map_err(|e| MuxError::Media {
        path: path.display().to_string(),
        msg: e.to_string(),
    })?;
    let mut r = StreamReader::new(BufReader::with_capacity(1 << 20, f));
    let mut v: Vec<Packet> = Vec::new();
    // Stop at PROBE_MAX_BYTES, or once `probe_s` of PCR (of any PID) has passed.
    let (mut first, mut last): (Option<u64>, u64) = (None, 0);
    let limit = (probe_s * HZ as f64) as u64;
    let mut clock = PcrClock::<()>::new(None);
    while v.len() * 188 < PROBE_MAX_BYTES {
        let Some(p) = r.next_packet().map_err(MuxError::Io)? else {
            break;
        };
        let p = *p.packet();
        clock.push(&p, ());
        while let Some(x) = clock.pop() {
            first.get_or_insert(x.t);
            last = x.t;
        }
        v.push(p);
        if first.is_some_and(|f| last - f > limit) {
            break;
        }
    }
    if v.is_empty() {
        return Err(MuxError::Media {
            path: path.display().to_string(),
            msg: "no transport packets".into(),
        });
    }
    Ok(v)
}

/// Probe log entry of a relayed packet: arrival time, payload bytes, and
/// for a PES start its decoding time on the continuous time line (`None`
/// inside when it has no PTS/DTS or no PCR anchor yet).
type LoggedPacket = (u64, usize, Option<Option<i64>>);

fn event_of(e: &isdb_si::tables::EitEvent) -> Option<SourceEvent> {
    let start = jst::from_datetime(e.start_time.to_jst()?)?;
    let duration = e.duration.seconds()?;
    let mut ev = SourceEvent {
        event_id: e.event_id,
        start,
        duration,
        name: String::new(),
        text: String::new(),
        content: Vec::new(),
    };
    for d in &e.descriptors {
        match d {
            Descriptor::ShortEvent(s) => {
                ev.name = s.event_name.decode().text;
                ev.text = s.text.decode().text;
            }
            Descriptor::Content(c) => {
                ev.content = c
                    .items
                    .iter()
                    .map(|i| i.content_nibble_level_1 << 4 | (i.content_nibble_level_2 & 0xF))
                    .collect();
            }
            _ => {}
        }
    }
    Some(ev)
}

/// Role of a source elementary stream in the rebroadcast service
/// ([`classify`]); with `pid_map = "auto"` it picks the output PID from
/// the [`PidPlan`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Role {
    /// Video: stream_type 0x01/0x02 (MPEG-1/2), 0x1B (H.264), 0x24 (HEVC).
    Video,
    /// Audio: stream_type 0x03/0x04 (MPEG audio), 0x0F (AAC ADTS), 0x11
    /// (AAC LATM). The lowest component_tag (else the first in the PMT) is
    /// the main audio.
    Audio,
    /// ARIB caption (data_component_id 0x0008 PES related to the video).
    Captions,
    /// ARIB superimpose (data_component_id 0x0008 PES not related to it).
    Superimpose,
    /// Other PES private data (stream_type 0x06 without data_component_id
    /// 0x0008: data broadcasting PES).
    DataPes,
    /// Section streams: private sections 0x05, DSM-CC 0x0B–0x0D (data
    /// carousels); relayed only with `include_carousels`.
    Carousel,
    /// Any other stream_type; relayed only when listed in `stream_types`.
    Other,
}

impl Role {
    /// Short name.
    pub fn name(self) -> &'static str {
        match self {
            Role::Video => "video",
            Role::Audio => "audio",
            Role::Captions => "captions",
            Role::Superimpose => "superimpose",
            Role::DataPes => "data PES",
            Role::Carousel => "data carousel",
            Role::Other => "other",
        }
    }

    /// Relayed when `stream_types` is not given.
    pub fn selected_by_default(self, include_carousels: bool) -> bool {
        match self {
            Role::Video | Role::Audio | Role::Captions | Role::Superimpose | Role::DataPes => true,
            Role::Carousel => include_carousels,
            Role::Other => false,
        }
    }

    /// The [`PidPlan`] entry of the role (data PES, carousels and other
    /// streams share `data`).
    pub fn plan(self, plan: &PidPlan) -> &[Pid] {
        match self {
            Role::Video => &plan.video.0,
            Role::Audio => &plan.audio.0,
            Role::Captions => &plan.captions.0,
            Role::Superimpose => &plan.superimpose.0,
            Role::DataPes | Role::Carousel | Role::Other => &plan.data.0,
        }
    }

    /// Streams of the same group share one plan entry.
    fn group(self) -> u8 {
        match self {
            Role::Video => 0,
            Role::Audio => 1,
            Role::Captions => 2,
            Role::Superimpose => 3,
            Role::DataPes | Role::Carousel | Role::Other => 4,
        }
    }
}

/// stream_types of [`Role::Video`].
pub const VIDEO_STREAM_TYPES: &[u8] = &[0x01, 0x02, 0x1B, 0x24];
/// stream_types of [`Role::Audio`].
pub const AUDIO_STREAM_TYPES: &[u8] = &[0x03, 0x04, 0x0F, 0x11];
/// data_component_id of the ARIB caption and superimpose coding (B24
/// Vol. 1 Part 3 §9.6 footnote 1: "shall be 0x0008"; B10 Annex J Table J-1).
pub const DATA_COMPONENT_CAPTION: u16 = 0x0008;

/// The first PES header seen on a PID of the source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PesHint {
    /// stream_id (H.222.0 Table 2-18).
    pub stream_id: u8,
    /// The PES carries a PTS (a "synchronous PES" in B24 Vol. 1 Part 3
    /// §3.1; without one it is an "asynchronous PES").
    pub has_pts: bool,
}

/// What [`classify`] decided for one elementary stream, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Classification {
    /// The role.
    pub role: Role,
    /// The rule that decided it (with its source).
    pub reason: String,
}

/// Classify a source elementary stream by stream_type, descriptors,
/// component_tag and (for captions vs superimpose) its first PES header.
///
/// * stream_type (H.222.0 Table 2-34, B32 Part 3 §3.10 (2)): video
///   [`VIDEO_STREAM_TYPES`], audio [`AUDIO_STREAM_TYPES`], carousels
///   [`CAROUSEL_STREAM_TYPES`].
/// * stream_type 0x06 with a data_component_descriptor (B10 §6.2.20) of
///   data_component_id 0x0008 is ARIB caption/superimpose coding (B24
///   Vol. 1 Part 3 §9.6). B24 Part 3 Ch. 4: characters "related to
///   contents of video" are caption, "all others" superimpose, and "when
///   transmitting and coding, these are not classified" — so the split
///   uses, in order: (1) component_tag 0x30–0x37 caption / 0x38–0x3F
///   superimpose (operational practice of ARIB TR-B14/TR-B15, **not
///   available locally — from knowledge**); (2) the Timing field of
///   additional_arib_caption_info (B24 Part 3 Table 9-15/9-16): '01'
///   program synchronous → caption, '00' asynchronous / '10' time
///   synchronous → superimpose; (3) the first PES: with a PTS
///   (synchronous PES) caption, without (asynchronous PES) superimpose;
///   (4) otherwise caption.
/// * Other stream_type 0x06: data PES. Anything else: [`Role::Other`].
pub fn classify(
    stream_type: u8,
    component_tag: Option<u8>,
    descriptors: &[Descriptor],
    pes: Option<PesHint>,
) -> Classification {
    let c = |role: Role, reason: String| Classification { role, reason };
    if VIDEO_STREAM_TYPES.contains(&stream_type) {
        return c(
            Role::Video,
            format!("stream_type {stream_type:#04x} (H.222.0 Table 2-34)"),
        );
    }
    if AUDIO_STREAM_TYPES.contains(&stream_type) {
        return c(
            Role::Audio,
            format!("stream_type {stream_type:#04x} (H.222.0 Table 2-34)"),
        );
    }
    if CAROUSEL_STREAM_TYPES.contains(&stream_type) {
        return c(
            Role::Carousel,
            format!("stream_type {stream_type:#04x}: sections (H.222.0 Table 2-34)"),
        );
    }
    if stream_type != 0x06 {
        return c(Role::Other, format!("stream_type {stream_type:#04x}"));
    }
    let dc = descriptors.iter().find_map(|d| match d {
        Descriptor::DataComponent(x) => Some(x),
        _ => None,
    });
    let Some(dc) = dc else {
        return c(
            Role::DataPes,
            "stream_type 0x06 PES private data without data_component_descriptor".into(),
        );
    };
    if dc.data_component_id != DATA_COMPONENT_CAPTION {
        return c(
            Role::DataPes,
            format!(
                "stream_type 0x06 PES private data, data_component_id {:#06x} (B10 Annex J)",
                dc.data_component_id
            ),
        );
    }
    let base = "stream_type 0x06 + data_component_id 0x0008 (B24 Vol.1 Part 3 §9.6)";
    match component_tag {
        Some(t @ 0x30..=0x37) => {
            return c(
                Role::Captions,
                format!(
                    "{base} + component_tag {t:#04x} in 0x30–0x37 [TR-B14 practice, from knowledge]"
                ),
            );
        }
        Some(t @ 0x38..=0x3F) => {
            return c(
                Role::Superimpose,
                format!(
                    "{base} + component_tag {t:#04x} in 0x38–0x3F [TR-B14 practice, from knowledge]"
                ),
            );
        }
        _ => {}
    }
    if let Some(&info) = dc.additional_data_component_info.first() {
        match info & 0b11 {
            0b01 => {
                return c(
                    Role::Captions,
                    format!("{base} + Timing '01' program synchronous (B24 Part 3 Table 9-16)"),
                );
            }
            0b00 => {
                return c(
                    Role::Superimpose,
                    format!("{base} + Timing '00' asynchronous (B24 Part 3 Table 9-16)"),
                );
            }
            0b10 => {
                return c(
                    Role::Superimpose,
                    format!("{base} + Timing '10' time synchronous (B24 Part 3 Table 9-16)"),
                );
            }
            _ => {}
        }
    }
    match pes {
        Some(h) if h.has_pts => c(
            Role::Captions,
            format!("{base} + PES with PTS: synchronous PES (B24 Part 3 §3.1)"),
        ),
        Some(_) => c(
            Role::Superimpose,
            format!("{base} + PES without PTS: asynchronous PES (B24 Part 3 §3.1)"),
        ),
        None => c(
            Role::Captions,
            format!("{base}, nothing distinguishes caption from superimpose: caption assumed"),
        ),
    }
}

/// A program of the source TS: its PAT entry, PMT and SDT name.
#[derive(Debug, Clone)]
pub struct SourceProgram {
    /// program_number (= service_id).
    pub number: u16,
    /// PMT PID.
    pub pmt_pid: Pid,
    /// SDT service name (service_descriptor), if the SDT was seen.
    pub service_name: Option<String>,
    /// SDT service_type.
    pub service_type: Option<u8>,
    /// Its elementary streams (PID, stream_type, role), `None` when no PMT
    /// was seen in the probed part.
    pub es: Option<Vec<(Pid, u8, Role)>>,
    /// Packets of its elementary streams in the probed part.
    pub es_packets: u64,
}

impl SourceProgram {
    /// One line: number, name, PMT, ES summary.
    pub fn describe(&self) -> String {
        let name = self
            .service_name
            .as_ref()
            .map(|n| format!(" \"{n}\""))
            .unwrap_or_default();
        let ty = self
            .service_type
            .map(|t| format!("service_type {t:#04x}, "))
            .unwrap_or_default();
        let es = match &self.es {
            None => "no PMT in the probed part".to_string(),
            Some(v) if v.is_empty() => "no elementary streams".to_string(),
            Some(v) => v
                .iter()
                .map(|(p, t, r)| format!("{} {p:#06x} ({t:#04x})", r.name()))
                .collect::<Vec<_>>()
                .join(", "),
        };
        format!(
            "{:#06x}{name} ({ty}PMT {:#06x}): {es}; {} ES packets in the probed part",
            self.number, self.pmt_pid, self.es_packets
        )
    }
}

/// Fold full-width ASCII (U+FF01–U+FF5E) and the ideographic space, drop
/// whitespace and lowercase, so `TV12` matches `ＴＶ１２テスト`.
pub fn normalize_name(s: &str) -> String {
    s.chars()
        .map(|c| match c as u32 {
            0xFF01..=0xFF5E => char::from_u32(c as u32 - 0xFEE0).unwrap_or(c),
            0x3000 => ' ',
            _ => c,
        })
        .filter(|c| !c.is_whitespace())
        .flat_map(char::to_lowercase)
        .collect()
}

/// PSI/SI of the probed part of a source.
struct Scan {
    /// PAT programs (number, PMT PID), in PAT order.
    programs: Vec<(u16, Pid)>,
    pmts: Vec<(u16, Pmt)>,
    /// SDT actual: service_id → (name, service_type).
    sdt: Vec<(u16, String, u8)>,
    /// EIT p/f actual: service_id → (present, following).
    eit: Vec<(u16, Option<SourceEvent>, Option<SourceEvent>)>,
    tot: Option<(u64, JstSeconds)>,
    pid_packets: Vec<u64>,
    pes_hint: Vec<Option<PesHint>>,
    /// CAT descriptor loop (first CAT seen).
    cat: Option<Vec<isdb_ts::descriptor::Descriptor>>,
    /// SDT actual: service_id → (free_CA_mode, service loop descriptors).
    sdt_loops: Vec<(u16, bool, Vec<Descriptor>)>,
    /// EIT p/f actual: (service_id, section_number) → first event's descriptors.
    eit_loops: Vec<(u16, u8, Vec<Descriptor>)>,
}

impl Scan {
    fn pmt(&self, program: u16) -> Option<&Pmt> {
        self.pmts.iter().find(|x| x.0 == program).map(|x| &x.1)
    }

    fn eit_entry(&mut self, sid: u16) -> &mut (u16, Option<SourceEvent>, Option<SourceEvent>) {
        if let Some(i) = self.eit.iter().position(|x| x.0 == sid) {
            return &mut self.eit[i];
        }
        self.eit.push((sid, None, None));
        self.eit.last_mut().expect("pushed")
    }

    fn program(&self, number: u16, pmt_pid: Pid) -> SourceProgram {
        let sdt = self.sdt.iter().find(|x| x.0 == number);
        let es = self.pmt(number).map(|m| {
            m.streams
                .iter()
                .map(|s| {
                    let typed: Vec<Descriptor> = s
                        .descriptors
                        .iter()
                        .filter_map(|d| Descriptor::decode_body(d.tag, &d.data).ok())
                        .collect();
                    let r = classify(
                        s.stream_type,
                        component_tag_of(&typed),
                        &typed,
                        self.pes_hint[usize::from(s.elementary_pid)],
                    );
                    (s.elementary_pid, s.stream_type, r.role)
                })
                .collect::<Vec<_>>()
        });
        let es_packets = es.as_ref().map_or(0, |v| {
            v.iter()
                .map(|(p, _, _)| self.pid_packets[usize::from(*p)])
                .sum()
        });
        SourceProgram {
            number,
            pmt_pid,
            service_name: sdt.map(|x| x.1.clone()),
            service_type: sdt.map(|x| x.2),
            es,
            es_packets,
        }
    }

    fn all_programs(&self) -> Vec<SourceProgram> {
        self.programs
            .iter()
            .map(|&(n, p)| self.program(n, p))
            .collect()
    }
}

fn component_tag_of(typed: &[Descriptor]) -> Option<u8> {
    typed.iter().find_map(|d| match d {
        Descriptor::StreamIdentifier(x) => Some(x.component_tag),
        _ => None,
    })
}

/// Pass over the probed packets: PAT, every PMT, SDT, EIT p/f, TOT/TDT,
/// packet counts and the first PES header of every PID.
fn scan(pkts: &[Packet]) -> Scan {
    let mut demux = SectionDemux::new();
    for pid in [0x0000, 0x0001, 0x0011, 0x0012, 0x0014] {
        demux.add_pid(Pid::new(pid).expect("Table 5-1 PID"));
    }
    let mut sc = Scan {
        programs: Vec::new(),
        pmts: Vec::new(),
        sdt: Vec::new(),
        eit: Vec::new(),
        tot: None,
        pid_packets: vec![0; 8192],
        pes_hint: vec![None; 8192],
        cat: None,
        sdt_loops: Vec::new(),
        eit_loops: Vec::new(),
    };
    let mut have_pat = false;
    for (i, p) in pkts.iter().enumerate() {
        let pid = usize::from(packet_pid(p));
        sc.pid_packets[pid] += 1;
        if sc.pes_hint[pid].is_none()
            && p[1] & 0x40 != 0
            && let Some(pl) = payload(p)
            && pl.len() >= 6
            && pl[..3] == [0, 0, 1]
        {
            let stream_id = pl[3];
            let has_pts =
                isdb_ts::pes::has_optional_header(stream_id) && pl.len() >= 8 && pl[7] & 0x80 != 0;
            sc.pes_hint[pid] = Some(PesHint { stream_id, has_pts });
        }
        for a in demux.push(i as u64, p) {
            let Assembled::Section(sec) = a else { continue };
            let tid = sec.data[0];
            match (sec.pid.get(), tid) {
                (0x0000, 0x00) if !have_pat => {
                    let Ok(pat) = Long::<Pat>::decode(&sec.data) else {
                        continue;
                    };
                    have_pat = true;
                    sc.programs = pat.body.programs().collect();
                    for &(_, pmt_pid) in &sc.programs {
                        demux.add_pid(pmt_pid);
                    }
                }
                (pid, 0x02) if sc.programs.iter().any(|x| x.1.get() == pid) => {
                    if let Ok(m) = Long::<Pmt>::decode(&sec.data) {
                        let n = m.header.table_id_extension;
                        if sc.programs.iter().any(|x| x.0 == n && x.1.get() == pid)
                            && sc.pmt(n).is_none()
                        {
                            sc.pmts.push((n, m.body));
                        }
                    }
                }
                (0x0011, 0x42) => {
                    let Ok(s) = Long::<SdtBody>::decode(&sec.data) else {
                        continue;
                    };
                    for svc in &s.body.services {
                        if !sc.sdt_loops.iter().any(|x| x.0 == svc.service_id) {
                            sc.sdt_loops.push((
                                svc.service_id,
                                svc.free_ca_mode,
                                svc.descriptors.clone(),
                            ));
                        }
                        if sc.sdt.iter().any(|x| x.0 == svc.service_id) {
                            continue;
                        }
                        if let Some(d) = svc.descriptors.iter().find_map(|d| match d {
                            Descriptor::Service(x) => Some(x),
                            _ => None,
                        }) {
                            sc.sdt.push((
                                svc.service_id,
                                d.service_name.decode().text,
                                d.service_type,
                            ));
                        }
                    }
                }
                (0x0012, 0x4E) => {
                    let Ok(e) = Long::<EitBody>::decode(&sec.data) else {
                        continue;
                    };
                    let ev = e.body.events.first().and_then(event_of);
                    let (sid, n) = (e.header.table_id_extension, e.header.section_number);
                    if n <= 1
                        && !sc.eit_loops.iter().any(|x| x.0 == sid && x.1 == n)
                        && let Some(first) = e.body.events.first()
                    {
                        sc.eit_loops.push((sid, n, first.descriptors.clone()));
                    }
                    let x = sc.eit_entry(e.header.table_id_extension);
                    match e.header.section_number {
                        0 if x.1.is_none() => x.1 = ev,
                        1 if x.2.is_none() => x.2 = ev,
                        _ => {}
                    }
                }
                (0x0001, 0x01) if sc.cat.is_none() => {
                    if let Ok(c) = Long::<Cat>::decode(&sec.data) {
                        sc.cat = Some(c.body.descriptors);
                    }
                }
                (0x0014, 0x73) if sc.tot.is_none() => {
                    if let Ok(t) = Short::<TotBody>::decode(&sec.data)
                        && let Some(dt) = t.body.jst_time.to_jst()
                    {
                        sc.tot = jst::from_datetime(dt).map(|j| (sec.end_packet, j));
                    }
                }
                (0x0014, 0x70) if sc.tot.is_none() => {
                    if let Ok(t) = Short::<TdtBody>::decode(&sec.data)
                        && let Some(dt) = t.body.jst_time.to_jst()
                    {
                        sc.tot = jst::from_datetime(dt).map(|j| (sec.end_packet, j));
                    }
                }
                _ => {}
            }
        }
    }
    sc
}

fn list_programs(v: &[SourceProgram]) -> String {
    v.iter()
        .map(|p| format!("\n    {}", p.describe()))
        .collect()
}

/// Choose the source program: `program` (= service_id), else
/// `service_name` against the SDT, else the only program (or the only one
/// whose elementary streams have packets). Returns (program, PMT PID, how).
fn select_program(sc: &Scan, st: &StreamCfg) -> std::result::Result<(u16, Pid, String), String> {
    if sc.programs.is_empty() {
        return Err("no PAT (or a PAT without programs) in the probed part".into());
    }
    let all = sc.all_programs();
    if let Some(n) = st.program {
        return sc
            .programs
            .iter()
            .find(|x| x.0 == n)
            .map(|&(n, p)| (n, p, "program given".to_string()))
            .ok_or_else(|| {
                format!(
                    "program {n:#06x} is not in the PAT; the recording's programs:{}",
                    list_programs(&all)
                )
            });
    }
    if let Some(want) = &st.service_name {
        if sc.sdt.is_empty() {
            return Err(format!(
                "service_name {want:?}: no SDT in the probed part; select with program = …:{}",
                list_programs(&all)
            ));
        }
        let w = normalize_name(want);
        let named: Vec<&SourceProgram> = all.iter().filter(|p| p.service_name.is_some()).collect();
        let exact: Vec<&&SourceProgram> = named
            .iter()
            .filter(|p| p.service_name.as_deref().map(normalize_name) == Some(w.clone()))
            .collect();
        let hits: Vec<&&SourceProgram> = if exact.is_empty() {
            named
                .iter()
                .filter(|p| {
                    p.service_name
                        .as_deref()
                        .is_some_and(|n| normalize_name(n).contains(&w))
                })
                .collect()
        } else {
            exact
        };
        return match hits.as_slice() {
            [p] => Ok((
                p.number,
                p.pmt_pid,
                format!("service_name {want:?} matches the SDT"),
            )),
            [] => Err(format!(
                "service_name {want:?} matches no SDT service of the PAT's programs:{}",
                list_programs(&all)
            )),
            _ => Err(format!(
                "service_name {want:?} is ambiguous ({} programs match); give program = …:{}",
                hits.len(),
                list_programs(&all)
            )),
        };
    }
    if let [(n, p)] = sc.programs.as_slice() {
        return Ok((*n, *p, "the only program in the PAT".into()));
    }
    let live: Vec<&SourceProgram> = all.iter().filter(|p| p.es_packets > 0).collect();
    match live.as_slice() {
        [p] => Ok((
            p.number,
            p.pmt_pid,
            "the only program whose elementary streams carry packets".into(),
        )),
        _ => Err(format!(
            "the recording has {} programs{}; choose one with program = … (or service_id) or service_name = … (CLI: --program / --service-name):{}",
            all.len(),
            if live.is_empty() {
                String::new()
            } else {
                format!(" ({} with elementary-stream packets)", live.len())
            },
            list_programs(&all)
        )),
    }
}

/// Order of the streams within each role (component_tag, then PMT order)
/// → `rank`; with `pid_map = "auto"`, the output PIDs from `plan`.
fn rank_and_assign(es: &mut [RelayedEs], plan: Option<&PidPlan>) {
    let mut used: Vec<Pid> = Vec::new();
    let reserved: Vec<Pid> = plan.map(|p| p.all().collect()).unwrap_or_default();
    for group in 0..=4u8 {
        let mut idx: Vec<usize> = (0..es.len())
            .filter(|&i| es[i].role.group() == group)
            .collect();
        idx.sort_by_key(|&i| (es[i].component_tag.map_or(0x100, u16::from), i));
        for (n, &i) in idx.iter().enumerate() {
            es[i].rank = n;
        }
        let Some(plan) = plan else { continue };
        let Some(&first) = idx.first() else { continue };
        let list = es[first].role.plan(plan).to_vec();
        let mut next = *list.last().expect("validated non-empty");
        for (n, &i) in idx.iter().enumerate() {
            let pid = match list.get(n) {
                Some(&p) if !used.contains(&p) => p,
                _ => {
                    while next.get() < 0x1FFE {
                        next = Pid::new(next.get() + 1).expect("at most 0x1FFE");
                        if !used.contains(&next) && !reserved.contains(&next) {
                            break;
                        }
                    }
                    next
                }
            };
            used.push(pid);
            es[i].pid = pid;
        }
    }
}

/// Why a source PID that is not relayed is dropped.
fn drop_reason(sc: &Scan, program: u16, pmt: &Pmt, pmt_pid: Pid, pid: Pid, ca: CaMode) -> String {
    let ca_pid = |d: &isdb_ts::descriptor::Descriptor| match Descriptor::decode_body(d.tag, &d.data)
    {
        Ok(Descriptor::ConditionalAccess(c)) if c.ca_pid == pid => Some(c.ca_system_id),
        _ => None,
    };
    let si = match pid.get() {
        0x0000 => Some("PAT"),
        0x0001 => Some("CAT"),
        0x0010 => Some("NIT"),
        0x0011 => Some("SDT/BAT"),
        0x0012 | 0x0026 | 0x0027 => Some("EIT"),
        0x0013 => Some("RST"),
        0x0014 => Some("TDT/TOT"),
        0x0017 => Some("DCT"),
        0x001E => Some("DIT"),
        0x001F => Some("SIT"),
        0x0022 => Some("PCAT"),
        0x0023 | 0x0028 => Some("SDTT"),
        0x0024 => Some("BIT"),
        0x0025 => Some("NBIT/LDT"),
        0x0029 => Some("CDT"),
        _ => None,
    };
    if let Some(t) = si {
        let regenerated = matches!(
            pid.get(),
            0x0000 | 0x0001 | 0x0010 | 0x0011 | 0x0012 | 0x0014 | 0x0024 | 0x0026 | 0x0027
        );
        return format!(
            "source {t} (B10 Table 5-1){}",
            if regenerated {
                ": this multiplex generates its own"
            } else {
                ": not relayed"
            }
        );
    }
    if pid == Pid::NULL {
        return "null packets".into();
    }
    if pid == pmt_pid {
        return format!("PMT of program {program:#06x}: regenerated");
    }
    if pid == pmt.pcr_pid {
        return "PCR of the program: this service's PCR is generated on its pcr_pid".into();
    }
    if let Some(sys) = pmt
        .program_info
        .iter()
        .chain(pmt.streams.iter().flat_map(|s| s.descriptors.iter()))
        .find_map(ca_pid)
    {
        return format!(
            "ECM (CA_PID of a conditional_access_descriptor, CA_system_id {sys:#06x}): {}",
            if ca == CaMode::Drop {
                "ca = \"drop\""
            } else {
                "named only by an elementary stream that is not relayed"
            }
        );
    }
    if let Some(sys) = sc.cat.iter().flatten().find_map(ca_pid) {
        return format!(
            "EMM (CA_PID of a source CAT CA_descriptor, CA_system_id {sys:#06x}): ca = \"drop\""
        );
    }
    if let Some(&(n, _)) = sc.programs.iter().find(|x| x.1 == pid) {
        return format!("PMT of program {n:#06x}");
    }
    if let Some((n, _)) = sc
        .pmts
        .iter()
        .find(|(n, m)| *n != program && m.streams.iter().any(|s| s.elementary_pid == pid))
    {
        return format!("elementary stream of program {n:#06x}");
    }
    "not referenced by the program's PMT (e.g. EMM, another service)".into()
}

/// The source program `st` selects (`program` / `service_id`,
/// `service_name`, or the only one), from the probed part of `path`:
/// (program_number, SDT service name, how it was chosen). The same choice
/// [`probe`] makes, without the stream analysis.
pub fn select_source_program(path: &Path, st: &StreamCfg) -> Result<(u16, Option<String>, String)> {
    let pkts = read_probe_packets(path, st.probe_seconds.unwrap_or(DEFAULT_PROBE_SECONDS))?;
    let sc = scan(&pkts);
    let (n, _, how) = select_program(&sc, st).map_err(|m| MuxError::Media {
        path: path.display().to_string(),
        msg: m,
    })?;
    let name = sc.sdt.iter().find(|x| x.0 == n).map(|x| x.1.clone());
    Ok((n, name, how))
}

/// Read the beginning of `path` and decide what `st` relays: program, ES,
/// roles and output PIDs, carried descriptors, stream parameters, rates,
/// source EIT p/f; then read the whole file once for the rate the relay
/// needs ([`crate::relay_rate`]).
pub fn probe(path: &Path, st: &StreamCfg) -> Result<Probe> {
    let probe_s = st.probe_seconds.unwrap_or(DEFAULT_PROBE_SECONDS);
    let pkts = read_probe_packets(path, probe_s)?;
    let err = |m: String| MuxError::Media {
        path: path.display().to_string(),
        msg: m,
    };

    // ---- pass 1: PAT, PMTs, SDT, EIT p/f, TOT/TDT, packet counts
    let sc = scan(&pkts);
    let (program, pmt_pid, chosen_by) = select_program(&sc, st).map_err(err)?;
    let pmt = sc.pmt(program).cloned().ok_or_else(|| {
        err(format!(
            "no PMT of program {program:#06x} on PID {pmt_pid:#06x} in the probed part"
        ))
    })?;
    if pmt.pcr_pid == Pid::NULL {
        return Err(err(format!(
            "program {program:#06x} has no PCR_PID: its packets cannot be timed"
        )));
    }
    let (present, following) = sc
        .eit
        .iter()
        .find(|x| x.0 == program)
        .map(|x| (x.1.clone(), x.2.clone()))
        .unwrap_or_default();
    let tot = sc.tot;
    let source = sc.all_programs();
    let service_name = sc.sdt.iter().find(|x| x.0 == program).map(|x| x.1.clone());

    // ---- selection and roles
    let types = st.stream_types.as_deref();
    let mut es = Vec::new();
    let mut skipped = Vec::new();
    let mut skipped_why: Vec<(Pid, String)> = Vec::new();
    // per relayed ES: its conditional_access_descriptor bodies (ca = "keep")
    let mut es_ca_raw: Vec<Vec<Vec<u8>>> = Vec::new();
    for s in &pmt.streams {
        let typed: Vec<Descriptor> = s
            .descriptors
            .iter()
            .filter_map(|d| Descriptor::decode_body(d.tag, &d.data).ok())
            .collect();
        let tag = component_tag_of(&typed);
        let class = classify(
            s.stream_type,
            tag,
            &typed,
            sc.pes_hint[usize::from(s.elementary_pid)],
        );
        let carousel = CAROUSEL_STREAM_TYPES.contains(&s.stream_type);
        let selected = match types {
            Some(t) => t.contains(&s.stream_type),
            None => class.role.selected_by_default(st.include_carousels),
        };
        let why = if !selected {
            Some(if carousel && types.is_none() {
                "section / carousel stream (include_carousels = true to relay)".to_string()
            } else {
                "stream_type not selected".to_string()
            })
        } else if let Some(want) = &st.component_tags
            && !tag.is_some_and(|t| want.contains(&t))
        {
            Some("component_tag not selected".to_string())
        } else {
            None
        };
        if let Some(why) = why {
            skipped.push(format!(
                "{:#06x} {} stream_type {:#04x}{}: {why}",
                s.elementary_pid,
                class.role.name(),
                s.stream_type,
                tag.map(|t| format!(" component_tag {t:#04x}"))
                    .unwrap_or_default()
            ));
            skipped_why.push((s.elementary_pid, why));
            continue;
        }
        let ca_keep = st.ca == CaMode::Keep;
        let (keep, drop): (Vec<_>, Vec<_>) = s.descriptors.iter().partition(|d| {
            Descriptor::decode_body(d.tag, &d.data)
                .ok()
                .is_some_and(|x| carried_over(&x))
        });
        let (special, drop): (Vec<_>, Vec<_>) = drop
            .into_iter()
            .partition(|d| d.tag == 0xC1 || (ca_keep && d.tag == 0x09));
        es_ca_raw.push(
            special
                .iter()
                .filter(|d| d.tag == 0x09)
                .map(|d| d.data.clone())
                .collect::<Vec<_>>(),
        );
        es.push(RelayedEs {
            src_pid: s.elementary_pid,
            pid: s.elementary_pid,
            stream_type: s.stream_type,
            component_tag: tag,
            role: class.role,
            role_reason: class.reason,
            rank: 0,
            descriptors: keep
                .iter()
                .filter_map(|d| Descriptor::decode_body(d.tag, &d.data).ok())
                .collect(),
            dropped_descriptors: drop.iter().map(|d| desc_name(d)).collect(),
            ca_descriptors: Vec::new(),
            copy_descriptors: special
                .iter()
                .filter(|d| d.tag == 0xC1)
                .map(|d| RawDescriptor {
                    tag: 0xC1,
                    hex: hex(&d.data),
                })
                .collect(),
            mode: if carousel {
                EsMode::Packets
            } else {
                EsMode::Pes
            },
            info: StreamInfo::None,
            tstd: None,
            mean_bps: 0.0,
            source_tstd: None,
            source_lead_s: None,
        });
    }
    if es.is_empty() {
        return Err(err(format!(
            "program {program:#06x}: no elementary stream selected ({})",
            skipped.join("; ")
        )));
    }

    // ---- conditional access (ca = "keep"): ECM PIDs of the program loop and
    // the relayed ES loops, EMM PIDs of the CAT present in the source
    let ca_keep = st.ca == CaMode::Keep;
    let mut ca_src: Vec<(Pid, CaPidKind, u16)> = Vec::new();
    let mut ca_remarks: Vec<String> = Vec::new();
    if ca_keep {
        let ecm = pmt
            .program_info
            .iter()
            .filter(|d| d.tag == 0x09)
            .map(|d| d.data.as_slice())
            .chain(es_ca_raw.iter().flatten().map(Vec::as_slice));
        for d in ecm {
            if let Some((sys, pid)) = ca_fields(d)
                && pid != Pid::NULL
                && !ca_src.iter().any(|c| c.0 == pid)
            {
                ca_src.push((pid, CaPidKind::Ecm, sys));
            }
        }
        for d in sc.cat.iter().flatten().filter(|d| d.tag == 0x09) {
            let Some((sys, pid)) = ca_fields(&d.data) else {
                continue;
            };
            if pid == Pid::NULL || ca_src.iter().any(|c| c.0 == pid) {
                continue;
            }
            if sc.pid_packets[usize::from(pid)] > 0 {
                ca_src.push((pid, CaPidKind::Emm, sys));
            } else {
                ca_remarks.push(format!(
                    "EMM PID {pid:#06x} (CA_system_id {sys:#06x}) has no packets in the probed part: CAT descriptor carried as is, nothing relayed"
                ));
            }
        }
    }

    // ---- output PIDs
    let default_plan = PidPlan::default();
    match &st.pid_map {
        PidMap::Auto => rank_and_assign(&mut es, Some(st.pids.as_ref().unwrap_or(&default_plan))),
        PidMap::Keep => rank_and_assign(&mut es, None),
        PidMap::List(list) => {
            rank_and_assign(&mut es, None);
            let summary = || {
                pmt.streams
                    .iter()
                    .map(|s| {
                        format!(
                            "{:#06x} (stream_type {:#04x})",
                            s.elementary_pid, s.stream_type
                        )
                    })
                    .collect::<Vec<_>>()
                    .join(", ")
            };
            for &[a, b] in list {
                if let Some(e) = es.iter_mut().find(|e| e.src_pid == a) {
                    e.pid = b;
                } else if ca_src.iter().any(|c| c.0 == a) {
                    // an ECM / EMM PID: mapped below
                } else if let Some((_, why)) = skipped_why.iter().find(|x| x.0 == a) {
                    return Err(err(format!(
                        "pid_map [{a:#06x}, {b:#06x}]: source PID {a:#06x} of program {program:#06x} is not relayed ({why}); select it or remove the entry"
                    )));
                } else {
                    return Err(err(format!(
                        "pid_map [{a:#06x}, {b:#06x}]: {a:#06x} is not an elementary stream of program {program:#06x} (its PMT lists {})",
                        summary()
                    )));
                }
            }
        }
    }
    let mut ca_pids: Vec<RelayedCaPid> = Vec::new();
    {
        let reserved: Vec<Pid> = match &st.pid_map {
            PidMap::Auto => st.pids.as_ref().unwrap_or(&default_plan).all().collect(),
            _ => Vec::new(),
        };
        let mut used: Vec<Pid> = es.iter().map(|e| e.pid).collect();
        for &(src, kind, sys) in &ca_src {
            let listed = match &st.pid_map {
                PidMap::List(l) => l.iter().find(|x| x[0] == src).map(|x| x[1]),
                _ => None,
            };
            let pid = match (listed, &st.pid_map) {
                (Some(p), _) => p,
                // the source PID, or the next free one when an output ES
                // PID or the role plan already has it
                (None, PidMap::Auto) => {
                    let mut p = src;
                    while used.contains(&p) || reserved.contains(&p) {
                        p = Pid::new(if p.get() >= 0x1FFE {
                            0x0030
                        } else {
                            p.get() + 1
                        })
                        .expect("at most 0x1FFE");
                    }
                    p
                }
                (None, _) => src,
            };
            used.push(pid);
            ca_pids.push(RelayedCaPid {
                src_pid: src,
                pid,
                kind,
                ca_system_id: sys,
                mean_bps: 0.0,
            });
        }
    }
    let out_pids: Vec<(Pid, Pid)> = es
        .iter()
        .map(|e| (e.pid, e.src_pid))
        .chain(ca_pids.iter().map(|c| (c.pid, c.src_pid)))
        .collect();
    for (i, a) in out_pids.iter().enumerate() {
        if let Some(b) = out_pids[..i].iter().find(|b| b.0 == a.0) {
            return Err(err(format!(
                "output PID {:#06x} used by source PIDs {:#06x} and {:#06x}; fix pid_map / pids",
                a.0, b.1, a.1
            )));
        }
    }
    let map_ca = |pid: Pid| {
        ca_pids
            .iter()
            .find(|c| c.src_pid == pid)
            .map_or(pid, |c| c.pid)
    };
    for (e, raw) in es.iter_mut().zip(&es_ca_raw) {
        e.ca_descriptors = raw.iter().map(|d| remap_ca(d, map_ca)).collect();
    }
    let mut ca = SourceCa {
        mode: st.ca,
        source_has_cat: sc.cat.is_some(),
        source_free_ca_mode: sc.sdt_loops.iter().find(|x| x.0 == program).map(|x| x.1),
        remarks: ca_remarks,
        ..Default::default()
    };
    if ca_keep {
        ca.program_descriptors = pmt
            .program_info
            .iter()
            .filter(|d| d.tag == 0x09)
            .map(|d| remap_ca(&d.data, map_ca))
            .collect();
        ca.cat_descriptors = sc
            .cat
            .iter()
            .flatten()
            .filter(|d| d.tag == 0x09)
            .map(|d| remap_ca(&d.data, map_ca))
            .collect();
    }
    let dropped_pids: Vec<(Pid, String)> = (0..8192u16)
        .map(|p| Pid::new(p).expect("13 bits"))
        .filter(|&p| {
            sc.pid_packets[usize::from(p)] > 0
                && !es.iter().any(|e| e.src_pid == p)
                && !ca_pids.iter().any(|c| c.src_pid == p)
                && !skipped_why.iter().any(|x| x.0 == p)
        })
        .map(|p| (p, drop_reason(&sc, program, &pmt, pmt_pid, p, st.ca)))
        .collect();

    // ---- pass 2: rates, scrambling, stream parameters, cut-in time
    let video = es
        .iter()
        .position(|e| e.stream_type == 0x02 || e.stream_type == 0x01);
    let mut idx = vec![usize::MAX; 8192];
    for (i, e) in es.iter().enumerate() {
        idx[usize::from(e.src_pid)] = i;
    }
    // ECM / EMM PIDs after the ES: counted (rate, scrambling) only
    for (k, c) in ca_pids.iter().enumerate() {
        idx[usize::from(c.src_pid)] = es.len() + k;
    }
    let mut clock = PcrClock::<usize>::new(Some(pmt.pcr_pid));
    for (i, p) in pkts.iter().enumerate() {
        clock.push(p, i);
    }
    clock.finish();
    let timed: Vec<Timed<usize>> = std::iter::from_fn(|| clock.pop()).collect();
    let mut sel: Vec<(u64, usize)> = Vec::new(); // (time, es)
    let mut scrambled = 0u64;
    let mut cut_t: Option<u64> = None;
    let mut tot_t: Option<u64> = None;
    let mut pes_buf: Vec<Vec<u8>> = vec![Vec::new(); es.len()];
    // per ES: (arrival, payload bytes, decoding time if the packet starts a PES)
    let mut plog: Vec<Vec<LoggedPacket>> = vec![Vec::new(); es.len()];
    let mut es_data: Vec<Vec<u8>> = vec![Vec::new(); es.len()];
    let mut collecting: Vec<bool> = vec![false; es.len()];
    for x in &timed {
        let p = &pkts[x.item];
        if tot.is_some_and(|(i, _)| i == x.item as u64) {
            tot_t = Some(x.t);
        }
        let ei = idx[usize::from(packet_pid(p))];
        if ei == usize::MAX {
            continue;
        }
        sel.push((x.t, ei));
        if p[3] & 0xC0 != 0 {
            scrambled += 1;
            continue;
        }
        if ei >= es.len() {
            continue; // ECM / EMM: sections, no PES analysis
        }
        let Some(pl) = payload(p) else { continue };
        let pusi = p[1] & 0x40 != 0;
        let start = pusi.then(|| {
            let ok =
                pl.len() >= 19 && pl[..3] == [0, 0, 1] && isdb_ts::pes::has_optional_header(pl[3]);
            let flags = if ok { pl[7] >> 6 } else { 0 };
            let ts = match flags {
                3 => Some(read_ts(&pl[14..19])),
                2 => Some(read_ts(&pl[9..14])),
                _ => None,
            };
            ts.zip(x.anchor).map(|(v, a)| a.map90(v))
        });
        plog[ei].push((x.t, pl.len(), start));
        if pusi && Some(ei) == video && cut_t.is_none() && starts_with_sequence_header(pl) {
            cut_t = Some(x.t);
        }
        // ES bytes of the first complete PES for video (from a sequence
        // header) and audio, for the stream parameters.
        let limit = if Some(ei) == video {
            6 << 20
        } else {
            256 << 10
        };
        if pusi {
            let done = std::mem::take(&mut pes_buf[ei]);
            if collecting[ei]
                && let Some(s) = pes_es_start(&done)
                && es_data[ei].len() < limit
            {
                es_data[ei].extend_from_slice(&done[s..]);
            }
            let start_ok = if Some(ei) == video {
                collecting[ei] || starts_with_sequence_header(pl)
            } else {
                true
            };
            collecting[ei] = start_ok;
        }
        if collecting[ei] {
            pes_buf[ei].extend_from_slice(pl);
        }
    }
    if scrambled > 0 {
        return Err(err(format!(
            "{scrambled} packets of the relayed PIDs are scrambled (transport_scrambling_control ≠ '00'); scrambled programs are refused: isdb does not descramble (no B-CAS / CAS support)"
        )));
    }
    let span = match (sel.first(), sel.last()) {
        (Some(a), Some(b)) if b.0 > a.0 => b.0 - a.0,
        _ => return Err(err("selected streams span no PCR time".into())),
    };
    let span_s = span as f64 / HZ as f64;
    let mut counts = vec![0u64; es.len() + ca_pids.len()];
    for &(_, e) in &sel {
        counts[e] += 1;
    }
    for (e, c) in es.iter_mut().zip(&counts) {
        e.mean_bps = *c as f64 * 1504.0 / span_s;
    }
    for (c, n) in ca_pids.iter_mut().zip(&counts[es.len()..]) {
        c.mean_bps = *n as f64 * 1504.0 / span_s;
    }
    ca.pids = ca_pids;
    let mean_bps = sel.len() as f64 * 1504.0 / span_s;
    let mut peak = 0f64;
    let mut j = 0;
    for i in 0..sel.len() {
        while sel[j].0 + HZ <= sel[i].0 {
            j += 1;
        }
        if sel[i].0 >= sel[0].0 + HZ {
            peak = peak.max((i - j + 1) as f64 * 1504.0);
        }
    }
    if span_s < 1.0 {
        peak = mean_bps;
    }
    let p = path.display().to_string();
    for (ei, e) in es.iter_mut().enumerate() {
        let data = std::mem::take(&mut es_data[ei]);
        match e.stream_type {
            0x01 | 0x02 if !data.is_empty() => {
                if let Ok(v) = VideoEs::parse(data, &p) {
                    e.tstd = Some(video_tstd(&v.seq));
                    e.info = StreamInfo::Video {
                        seq: v.seq,
                        has_sequence_end: false,
                    };
                }
            }
            0x0F if !data.is_empty() => {
                // PES need not start on an ADTS frame (data_alignment_indicator
                // 0; e.g. fixed 720-byte audio PES with frames straddling them):
                // start at the first syncword whose frame chain checks out.
                let Some(adts) = adts_frames(&data) else {
                    continue;
                };
                if let Ok(a) = AdtsEs::parse(adts.to_vec(), &p) {
                    e.tstd = Some(TstdParams {
                        rx: a.rx() as f64,
                        mb: None,
                        main: a.bs() as f64,
                    });
                    e.info = StreamInfo::Audio {
                        sampling_rate: a.sampling_rate_code(),
                        audio_mode: a.audio_mode(),
                        quality: a.quality_mode(),
                    };
                }
            }
            _ => {}
        }
    }
    // The T-STD model on the source's own timing.
    for (ei, e) in es.iter_mut().enumerate() {
        let Some(tp) = e.tstd else { continue };
        let log = &plog[ei];
        let Some(first) = log.iter().position(|x| x.2.is_some()) else {
            continue;
        };
        let t0 = log[first].0;
        let mut m = Tstd::new(tp, t0);
        let mut leads = Vec::new();
        let mut k = first;
        while k < log.len() {
            // one PES: its start packet and the packets up to the next start
            let end = log[k + 1..]
                .iter()
                .position(|x| x.2.is_some())
                .map_or(log.len(), |n| k + 1 + n);
            if end == log.len() {
                break; // the last PES may be cut by the probe window
            }
            let bytes: usize = log[k..end].iter().map(|x| x.1).sum();
            if let Some(Some(d)) = log[k].2
                && d > 0
            {
                m.add_au(d as u64, bytes);
                leads.push((d - log[k].0 as i64) as f64 / HZ as f64);
            }
            for x in &log[k..end] {
                m.push(x.0, x.1);
            }
            k = end;
        }
        e.source_tstd = Some(m.violations);
        if !leads.is_empty() {
            leads.sort_by(|a, b| a.total_cmp(b));
            e.source_lead_s = Some((leads[0], leads[leads.len() / 2], leads[leads.len() - 1]));
        }
    }
    let source_now = match (tot, tot_t, cut_t) {
        (Some((_, j)), Some(tt), Some(ct)) => {
            Some(j - ((tt as i64 - ct as i64) as f64 / HZ as f64).round() as i64)
        }
        (Some((_, j)), _, _) => Some(j),
        _ => None,
    };
    let typed = |v: &[isdb_ts::descriptor::Descriptor]| -> Vec<Descriptor> {
        v.iter()
            .filter(|d| d.tag == 0xC1 || d.tag == 0xDE)
            .filter_map(|d| Descriptor::decode_body(d.tag, &d.data).ok())
            .collect()
    };
    let mut source_copy_control = SourceCopyControl::default();
    source_copy_control.push_from(CopySignalPlace::PmtProgram, &typed(&pmt.program_info));
    for s in pmt
        .streams
        .iter()
        .filter(|s| es.iter().any(|e| e.src_pid == s.elementary_pid))
    {
        source_copy_control.push_from(
            CopySignalPlace::PmtEs(s.elementary_pid),
            &typed(&s.descriptors),
        );
    }
    if let Some((_, _, ds)) = sc.sdt_loops.iter().find(|x| x.0 == program) {
        source_copy_control.push_from(CopySignalPlace::Sdt, ds);
    }
    for (n, place) in [
        (0u8, CopySignalPlace::EitPresent),
        (1, CopySignalPlace::EitFollowing),
    ] {
        if let Some((_, _, ds)) = sc.eit_loops.iter().find(|x| x.0 == program && x.1 == n) {
            source_copy_control.push_from(place, ds);
        }
    }
    // ---- pass 3: the whole file, for the rate the relay needs
    let scan_pids: Vec<crate::relay_rate::ScanPid> = es
        .iter()
        .map(|e| crate::relay_rate::ScanPid {
            pid: e.src_pid,
            pes: e.mode == EsMode::Pes,
        })
        .chain(ca.pids.iter().map(|c| crate::relay_rate::ScanPid {
            pid: c.src_pid,
            pes: false,
        }))
        .collect();
    let scanned = crate::relay_rate::scan(path, pmt.pcr_pid, &scan_pids)?;
    Ok(Probe {
        path: path.to_path_buf(),
        program,
        chosen_by,
        service_name,
        source_programs: source,
        dropped_pids,
        pmt_pid,
        pcr_pid: pmt.pcr_pid,
        es,
        skipped,
        dropped_program_descriptors: pmt
            .program_info
            .iter()
            .filter(|d| !(ca_keep && d.tag == 0x09))
            .map(desc_name)
            .collect(),
        source_copy_control,
        ca,
        span_s,
        mean_bps,
        peak_1s_bps: peak,
        rate: scanned.profile,
        seek_index: scanned.index,
        present,
        following,
        source_now,
    })
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// Bytes of a hex string written by [`hex`].
fn parse_hex_lossy(s: &str) -> Vec<u8> {
    crate::config::parse_hex(s).unwrap_or_default()
}

/// The stream configuration a relayed ES takes in the service (so the PMT,
/// EIT component descriptors and validation treat it like any stream).
///
/// The ES loop gets the carried typed descriptors, the source's
/// conditional_access_descriptors kept with `ca = "keep"` (CA_PID
/// remapped) and, with `keep_copy_control` (the service keeps the source's
/// copy control, `recording_control = "source"`), the source's
/// per-component digital_copy_control_descriptors.
pub fn expanded_stream(
    st: &StreamCfg,
    e: &RelayedEs,
    relay: usize,
    index: usize,
    keep_copy_control: bool,
) -> Result<StreamCfg> {
    let mut x = st.clone();
    x.kind = StreamKind::TsProgram;
    x.pid = e.pid;
    x.stream_type = Some(e.stream_type);
    x.component_tag = e.component_tag;
    x.source = None;
    x.descriptors = Vec::new();
    let mut has_vdc = false;
    for d in &e.descriptors {
        if matches!(d, Descriptor::StreamIdentifier(_)) {
            continue; // generated from component_tag
        }
        has_vdc |= matches!(d, Descriptor::VideoDecodeControl(_));
        let mut v = Vec::new();
        d.encode(&mut v)?;
        x.descriptors.push(RawDescriptor {
            tag: v[0],
            hex: hex(&v[2..]),
        });
    }
    x.descriptors.extend(e.ca_descriptors.iter().cloned());
    if keep_copy_control {
        x.descriptors.extend(e.copy_descriptors.iter().cloned());
    }
    let video = x.media_class() == MediaClass::Video;
    x.video_decode_control = VideoDecodeControlCfg {
        mode: if video && !has_vdc {
            VdcMode::Derived
        } else {
            VdcMode::Off
        },
        ..Default::default()
    };
    x.eit_component = x.media_class() != MediaClass::Other && !matches!(e.info, StreamInfo::None);
    x.program = None;
    x.service_name = None;
    x.stream_types = None;
    x.component_tags = None;
    x.include_carousels = false;
    x.pid_map = PidMap::Auto;
    x.pids = None;
    x.eit_from_source = false;
    x.probe_seconds = None;
    x.max_early_ms = 0;
    x.at_eof = AtEof::Loop;
    x.rate_check = Default::default();
    x.ca = CaMode::Keep;
    x.relay_es = Some((relay, index));
    Ok(x)
}

/// Keep only the characters `isdb-text` can encode (SI strings).
fn encodable(s: &str) -> String {
    if isdb_text::encode(s).is_ok() {
        return s.to_string();
    }
    s.chars()
        .filter(|c| isdb_text::encode(&c.to_string()).is_ok())
        .collect()
}

fn hms(secs: u32) -> String {
    format!("{:02}:{:02}:{:02}", secs / 3600, secs / 60 % 60, secs % 60)
}

/// EIT events for the service from the source EIT present/following: the
/// present event placed so that the relay's first access point (the
/// multiplex start) is as far into it as in the source; the following
/// event after it at the source's distance. Names and texts are decoded
/// (isdb-text) and re-encoded by the SI builder.
pub fn source_events(p: &Probe) -> Vec<EventCfg> {
    let mut v = Vec::new();
    let now = p.source_now;
    let mut anchor: Option<(JstSeconds, i64)> = None; // (source start, offset_s from clock start)
    for e in [&p.present, &p.following].into_iter().flatten() {
        let off = match (anchor, now) {
            (Some((s0, o0)), _) => o0 + (e.start - s0),
            (None, Some(n)) => e.start - n,
            (None, None) => 0,
        };
        anchor.get_or_insert((e.start, off));
        if v.last().is_some_and(|l: &EventCfg| {
            l.start_offset_s.unwrap_or(0) + i64::from(jst::parse_hms(&l.duration).unwrap_or(0))
                > off
        }) {
            continue;
        }
        v.push(EventCfg {
            event_id: e.event_id,
            start: None,
            start_offset_s: Some(off),
            duration: hms(e.duration),
            name: encodable(&e.name),
            text: encodable(&e.text),
            content: e.content.clone(),
            descriptors: Vec::new(),
        });
    }
    v
}

// ---------------------------------------------------------------------------
// runtime
// ---------------------------------------------------------------------------

enum Chunk {
    /// Packets read after the seek of the generation given.
    Packets(u64, Vec<Packet>),
    /// End of file (of that generation's reading).
    Eof(u64),
    /// A read error: the relay stops with this message.
    Error(String),
}

/// A command to the reader thread.
enum ReaderCmd {
    /// Continue from byte `offset`; the packets that follow carry `epoch`.
    Seek { epoch: u64, offset: u64 },
}

/// The reader thread: the file's packets in batches, from the start again
/// after each end of file (`at_eof = "loop"`), or, after the end with
/// `"end"`, waiting for a seek. A seek restarts reading at a byte offset.
fn spawn_reader(
    path: &Path,
    at_eof: AtEof,
) -> Result<(Receiver<Chunk>, std::sync::mpsc::Sender<ReaderCmd>)> {
    use std::io::{Seek, SeekFrom};
    let open = |path: &Path, offset: u64| {
        std::fs::File::open(path)
            .and_then(|mut f| f.seek(SeekFrom::Start(offset)).map(|_| f))
            .map_err(|e| MuxError::Media {
                path: path.display().to_string(),
                msg: e.to_string(),
            })
    };
    let mut f = open(path, 0)?;
    let (tx, rx) = sync_channel::<Chunk>(8);
    let (ctl, cmds) = std::sync::mpsc::channel::<ReaderCmd>();
    let path = path.to_path_buf();
    std::thread::Builder::new()
        .name(format!("relay {}", path.display()))
        .spawn(move || {
            let mut epoch = 0u64;
            'file: loop {
                let mut r = StreamReader::new(BufReader::with_capacity(1 << 20, f));
                let mut batch = Vec::with_capacity(BATCH);
                loop {
                    if batch.is_empty()
                        && let Ok(ReaderCmd::Seek { epoch: g, offset }) = cmds.try_recv()
                    {
                        epoch = g;
                        match open(&path, offset) {
                            Ok(x) => f = x,
                            Err(e) => {
                                let _ = tx.send(Chunk::Error(format!("seeking: {e}")));
                                return;
                            }
                        }
                        continue 'file;
                    }
                    match r.next_packet() {
                        Ok(Some(p)) => {
                            batch.push(*p.packet());
                            if batch.len() == BATCH
                                && tx
                                    .send(Chunk::Packets(epoch, std::mem::take(&mut batch)))
                                    .is_err()
                            {
                                return;
                            }
                        }
                        Ok(None) => break,
                        Err(e) => {
                            if !batch.is_empty() {
                                let _ = tx.send(Chunk::Packets(epoch, std::mem::take(&mut batch)));
                            }
                            let _ = tx
                                .send(Chunk::Error(format!("{}: read error: {e}", path.display())));
                            return;
                        }
                    }
                }
                if !batch.is_empty() && tx.send(Chunk::Packets(epoch, batch)).is_err() {
                    return;
                }
                if tx.send(Chunk::Eof(epoch)).is_err() {
                    return;
                }
                let offset = if at_eof == AtEof::End {
                    // wait for a seek; none comes unless playback is controlled
                    match cmds.recv() {
                        Ok(ReaderCmd::Seek { epoch: g, offset }) => {
                            epoch = g;
                            offset
                        }
                        Err(_) => return,
                    }
                } else {
                    0
                };
                match open(&path, offset) {
                    Ok(x) => f = x,
                    Err(e) => {
                        let _ = tx.send(Chunk::Error(format!("reopening for the next pass: {e}")));
                        return;
                    }
                }
            }
        })
        .map_err(MuxError::Io)?;
    Ok((rx, ctl))
}

struct InChunk {
    len: usize,
    t: u64,
    rai: bool,
    pusi: bool,
}

struct Assembly {
    bytes: Vec<u8>,
    chunks: Vec<InChunk>,
    anchor: Option<Anchor>,
    damaged: bool,
}

/// One output packet's worth of a queued unit.
#[derive(Debug, Clone, Copy)]
struct OutChunk {
    len: usize,
    /// Content time (27 MHz: pass-local source time + pass offset) at which
    /// the source delivers it.
    src: i64,
    /// Mux time (27 MHz) at which it is due (set when the unit is prepared).
    at: u64,
    rai: bool,
    pusi: bool,
    /// Within [`SEAM_WINDOW`] after a loop seam: may be held by the T-STD.
    seam: bool,
}

/// What a queued unit is, for playback control.
#[derive(Debug, Clone, Copy, PartialEq)]
enum UnitKind {
    /// A PES as received; for video, its first picture_coding_type.
    Pes { picture: Option<u8> },
    /// One ADTS frame re-packetised as its own PES (playback control);
    /// `dur` ticks long; `copy`: a repetition made for slow motion.
    AudioFrame { dur: i64, copy: bool },
    /// One packet's payload (carousels, ECM / EMM).
    Packet,
}

/// A PES (or, for carousels, one packet's payload) ready to send.
#[derive(Clone)]
struct OutPes {
    bytes: Vec<u8>,
    chunks: Vec<OutChunk>,
    /// PTS / DTS in content time (27 MHz), written into `bytes` through
    /// the playback timeline when the unit is prepared.
    pts: Option<i64>,
    dts: Option<i64>,
    /// Mux decoding time (DTS, else PTS), once prepared.
    decode: Option<u64>,
    kind: UnitKind,
    /// The playback-control generation it was prepared for.
    prepared: Option<u64>,
    /// Thinning decision already taken (playback control).
    kept: bool,
}

impl OutPes {
    /// Decoding time in content time.
    fn decode_c(&self) -> Option<i64> {
        self.dts.or(self.pts)
    }
}

/// Counters of one relay.
#[derive(Debug, Clone, Copy, Default)]
pub struct RelayStats {
    /// Passes completed (loops).
    pub passes: u64,
    /// Packets sent.
    pub packets: u64,
    /// PES (or carousel packets) queued from the source.
    pub units_in: u64,
    /// PES dropped (damaged by CC errors / TEI / scrambling, or untimed).
    pub pes_dropped: u64,
    /// PES cut by the end of the file (dropped).
    pub eof_dropped: u64,
    /// Leading B-picture PES of an open GOP dropped at a cut-in.
    pub leading_b_dropped: u64,
    /// continuity_counter discontinuities in the source.
    pub cc_errors: u64,
    /// Source duplicate packets skipped (H.222.0 §2.4.3.3).
    pub duplicates: u64,
    /// Video pictures skipped by playback control (rate above 1).
    pub skipped_pictures: u64,
    /// Audio frames skipped by playback control (rate above 1).
    pub skipped_audio_frames: u64,
    /// Audio frames repeated by playback control (rate below 1).
    pub repeated_audio_frames: u64,
    /// Packets with transport_error_indicator.
    pub tei: u64,
    /// Scrambled packets.
    pub scrambled: u64,
    /// PES with an ESCR (not rewritten).
    pub escr: u64,
    /// PES whose last byte left after their decoding time.
    pub late_pes: u64,
    /// Largest delay of a packet behind its PCR-derived time (ticks).
    pub max_lateness: u64,
    /// Delay of the oldest due packet at the last slot (ticks).
    pub lag: u64,
    /// Slots in which a due packet was held back because its T-STD buffer
    /// would overflow ([`RETIME_HOLD`], or the hold after a loop seam).
    pub tstd_holds: u64,
    /// Relayed packets taken from the source in completed passes.
    pub source_packets: u64,
    /// PCR time covered by them (ticks).
    pub source_ticks: u64,
    /// Largest number of relayed packets in a 1 s window of PCR time.
    pub peak_1s_packets: u64,
    /// Largest number of relayed packets in a 100 ms window of PCR time.
    pub peak_100ms_packets: u64,
}

impl RelayStats {
    /// Mean source rate of the relayed packets (bit/s).
    pub fn mean_bps(&self) -> f64 {
        if self.source_ticks == 0 {
            0.0
        } else {
            self.source_packets as f64 * 1504.0 * HZ as f64 / self.source_ticks as f64
        }
    }
    /// Peak rate over 1 s windows (bit/s).
    pub fn peak_1s_bps(&self) -> f64 {
        self.peak_1s_packets as f64 * 1504.0
    }
    /// Peak rate over 100 ms windows (bit/s).
    pub fn peak_100ms_bps(&self) -> f64 {
        self.peak_100ms_packets as f64 * 15040.0
    }
}

pub(crate) struct RelayEs {
    pub(crate) pid: Pid,
    pub(crate) layer: usize,
    mode: EsMode,
    cc: Counter,
    pub(crate) tstd: Option<Tstd>,
    last_cc: Option<u8>,
    last_pkt: Option<Packet>,
    asm: Option<Assembly>,
    wait_pusi: bool,
    drop_leading_b: bool,
    pass_units: u64,
    queue: VecDeque<OutPes>,
    cur: Option<(OutPes, usize, usize)>, // unit, chunk index, byte position
    first_pts: Option<i64>,
    last_pts: Option<i64>,
    pts_step: i64,
    /// Latest decoding time (pass-local continuous ticks) queued this pass.
    last_decode: Option<i64>,
    /// AAC ADTS stream: re-packetised frame by frame under playback control.
    adts: Option<AdtsSplit>,
    /// Output audio frame periods (playback control).
    slots: AudioSlots,
    /// Mux decoding time of the last unit sent.
    last_out_decode: Option<i64>,
    /// Largest content time of this stream sent (playback control).
    c_sent: Option<i64>,
}

/// ADTS frames out of a PES stream whose PES need not start on a frame
/// (playback control): each frame becomes its own PES so that frames can
/// be skipped or repeated.
#[derive(Debug, Clone, Default)]
struct AdtsSplit {
    /// ES bytes not yet cut into frames.
    buf: Vec<u8>,
    /// Content arrival time of the source packet that carried each run of
    /// `buf` (start offset in `buf`, time).
    runs: Vec<(usize, i64)>,
    /// PTS (content time) of the first frame starting at or after `buf`
    /// offset `.0`, from the PES that carried it.
    pts: Option<(usize, i64)>,
    /// PTS of the last frame cut.
    last_pts: Option<i64>,
    /// stream_id of the PES.
    stream_id: u8,
}

/// Playback control of a relay ([`crate::playback`]).
struct Control {
    /// Bumped by every control change: prepared heads are prepared again.
    epoch: u64,
    /// Mux time of the pause, while paused.
    paused: Option<u64>,
    rate: f64,
    /// Largest content time sent (PTS, DTS, arrival).
    c_sent: Option<i64>,
    /// Largest mux decoding time sent.
    o_sent: Option<i64>,
    /// Largest content time read (arrival, PTS, DTS) in the current pass.
    c_read: i64,
    /// The next pass starts a new timeline segment at its access point,
    /// shown no earlier than this mux time (a seek, or a loop under control).
    anchor: Option<i64>,
    /// Units of the current pass decoded before this content time are
    /// dropped (they belong before its access point).
    floor: Option<i64>,
    /// Content time at the start of each pass, its pass offset and the file
    /// time where it starts reading (for the position shown).
    passes: Vec<(i64, i64, u64)>,
    /// File time where the current pass starts reading.
    pass_file_t: u64,
    /// Reader generation (seeks).
    reader_gen: u64,
    video: VideoThinning,
    /// Video frame period (ticks) and the output PTS of the last pictures
    /// kept: above rate 1 a picture is kept only a frame period away from
    /// them (a display shows one picture per frame period).
    frame: i64,
    shown: VecDeque<i64>,
}

/// The current playback position of a relay.
fn position(passes: &[(i64, i64, u64)], c: i64) -> f64 {
    let k = passes.partition_point(|p| p.0 <= c);
    let (_, off, file_t) = passes[k.saturating_sub(1)];
    (file_t as i64 + c - off) as f64 / HZ as f64
}

/// A running relay of one source program.
pub(crate) struct Relay {
    pub(crate) name: String,
    rx: Receiver<Chunk>,
    pcr_pid: Pid,
    at_eof: AtEof,
    pub(crate) es: Vec<RelayEs>,
    index: Vec<u16>, // source PID → es index + 1
    video: Option<usize>,
    start_mux: u64,
    pub(crate) early: u64,
    pub(crate) lead: u64,
    clock: PcrClock<Packet>,
    started: bool,
    base: Option<i64>,
    pass_offset: u64,
    period: Option<u64>,
    pass_first_t: Option<u64>,
    pass_last_t: u64,
    read_t: u64,
    reading_done: bool,
    win_1s: VecDeque<u64>,
    win_100ms: VecDeque<u64>,
    pass_packets: u64,
    pub(crate) stats: RelayStats,
    pub(crate) failure: Option<String>,
    /// Commands to the reader thread (seeks).
    reader: std::sync::mpsc::Sender<ReaderCmd>,
    /// Content time → mux time; set at the first access point. Without
    /// playback control it is the constant shift `base`.
    timeline: Option<Timeline>,
    /// Playback control, once enabled ([`Relay::enable_control`]).
    ctl: Option<Control>,
    /// Mux time of the latest slot seen.
    now: u64,
    /// Byte offsets of the file (seeks).
    seek_index: std::sync::Arc<[crate::relay_rate::SeekPoint]>,
    /// Length of the file (PCR ticks).
    duration: u64,
    /// stream_type of each relayed ES (AAC ADTS 0x0F is re-packetised under
    /// playback control).
    stream_types: Vec<u8>,
    /// Longest arrival→decoding lead of the source's PES (probe; ticks).
    max_lead: i64,
    /// Video frame period (ticks; 1/29.97 s when unknown).
    frame_period: i64,
}

/// Head of a relayed PID's queue.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Head {
    pub at: u64,
    pub len: usize,
    pub decode: Option<u64>,
    /// Bytes of the PES from this chunk to its end.
    pub rest: usize,
    /// May be held by the T-STD model (start of a pass after a seam).
    pub seam: bool,
}

impl Relay {
    /// Start relaying `probe` with each ES on `layers[i]`; the first access
    /// point goes out at mux time `start_mux`.
    pub(crate) fn start(
        probe: &Probe,
        layers: &[usize],
        at_eof: AtEof,
        start_mux: u64,
        early: u64,
        lead: u64,
    ) -> Result<Self> {
        let mut index = vec![0u16; 8192];
        let mut es = Vec::new();
        for (i, e) in probe.es.iter().enumerate() {
            index[usize::from(e.src_pid)] = i as u16 + 1;
            es.push(RelayEs {
                pid: e.pid,
                layer: layers[i],
                mode: e.mode,
                cc: Counter::new(0),
                tstd: e.tstd.map(|p| Tstd::new(p, start_mux.saturating_sub(HZ))),
                last_cc: None,
                last_pkt: None,
                asm: None,
                wait_pusi: true,
                drop_leading_b: false,
                pass_units: 0,
                queue: VecDeque::new(),
                cur: None,
                first_pts: None,
                last_pts: None,
                pts_step: 0,
                last_decode: None,
                adts: None,
                slots: AudioSlots::default(),
                last_out_decode: None,
                c_sent: None,
            });
        }
        // ECM / EMM PIDs (ca = "keep"): packet by packet, no T-STD model
        for c in &probe.ca.pids {
            let i = es.len();
            index[usize::from(c.src_pid)] = i as u16 + 1;
            es.push(RelayEs {
                pid: c.pid,
                layer: layers[i],
                mode: EsMode::Packets,
                cc: Counter::new(0),
                tstd: None,
                last_cc: None,
                last_pkt: None,
                asm: None,
                wait_pusi: true,
                drop_leading_b: false,
                pass_units: 0,
                queue: VecDeque::new(),
                cur: None,
                first_pts: None,
                last_pts: None,
                pts_step: 0,
                last_decode: None,
                adts: None,
                slots: AudioSlots::default(),
                last_out_decode: None,
                c_sent: None,
            });
        }
        let video = probe
            .es
            .iter()
            .position(|e| e.stream_type == 0x02 || e.stream_type == 0x01);
        let (rx, reader) = spawn_reader(&probe.path, at_eof)?;
        Ok(Self {
            name: probe.path.display().to_string(),
            rx,
            pcr_pid: probe.pcr_pid,
            at_eof,
            es,
            index,
            video,
            start_mux,
            early,
            lead,
            clock: PcrClock::new(Some(probe.pcr_pid)),
            started: false,
            base: None,
            pass_offset: 0,
            period: None,
            pass_first_t: None,
            pass_last_t: 0,
            read_t: 0,
            reading_done: false,
            win_1s: VecDeque::new(),
            win_100ms: VecDeque::new(),
            pass_packets: 0,
            stats: RelayStats::default(),
            failure: None,
            reader,
            timeline: None,
            ctl: None,
            now: start_mux,
            seek_index: probe.seek_index.clone(),
            duration: (probe.rate.span_s * HZ as f64) as u64,
            stream_types: probe.es.iter().map(|e| e.stream_type).collect(),
            max_lead: probe
                .es
                .iter()
                .filter_map(|e| e.source_lead_s.map(|l| l.2))
                .fold(None, |m: Option<f64>, l| Some(m.map_or(l, |m| m.max(l))))
                .map_or(HZ as i64 / 2, |l| (l * HZ as f64) as i64),
            frame_period: probe
                .es
                .iter()
                .find_map(|e| match &e.info {
                    StreamInfo::Video { seq, .. } => Some((seq.frame_period_54mhz() / 2) as i64),
                    _ => None,
                })
                .unwrap_or(900_900),
        })
    }

    /// Content time of pass-local time `t`.
    fn content(&self, t: u64) -> i64 {
        t as i64 + self.pass_offset as i64
    }

    /// True once the file ended (`at_eof = "end"`) and every PES is sent.
    pub(crate) fn finished(&self) -> bool {
        self.reading_done
            && self
                .es
                .iter()
                .all(|e| e.queue.is_empty() && e.cur.is_none())
    }

    /// Read and time the source until its packets due by the slot at mux
    /// time `t` (plus the early window and the look-ahead a PES needs to
    /// complete) are queued. Nothing is read while paused.
    pub(crate) fn fill(&mut self, t: u64) {
        self.now = t;
        if self.ctl.as_ref().is_some_and(|c| c.paused.is_some()) {
            return;
        }
        let until = (t + self.early + LOOKAHEAD) as i64;
        while !self.reading_done && self.read_more(until) {
            let epoch = self.ctl.as_ref().map_or(0, |c| c.reader_gen);
            match self.rx.recv() {
                Ok(Chunk::Packets(g, v)) => {
                    if g != epoch {
                        continue; // read before a seek
                    }
                    for p in v {
                        self.clock.push(&p, p);
                        while let Some(x) = self.clock.pop() {
                            self.process(x);
                        }
                    }
                }
                Ok(Chunk::Eof(g)) => {
                    if g == epoch {
                        self.end_pass();
                    }
                }
                Ok(Chunk::Error(m)) => {
                    self.failure = Some(m);
                    self.reading_done = true;
                }
                Err(_) => self.reading_done = true,
            }
        }
    }

    fn read_more(&self, until: i64) -> bool {
        let Some(tl) = &self.timeline else {
            return true; // up to the first access point
        };
        if !self.started && self.ctl.as_ref().is_some_and(|c| c.anchor.is_some()) {
            return true; // up to the access point of a seek
        }
        tl.map(self.content(self.read_t)) < until
    }

    fn damage(&mut self, ei: usize) {
        let e = &mut self.es[ei];
        if let Some(a) = e.asm.as_mut() {
            a.damaged = true;
        } else {
            e.wait_pusi = true;
        }
    }

    /// Rolling source-rate statistics of the relayed packets.
    fn count_rate(&mut self, t: u64) {
        self.pass_packets += 1;
        for (w, len, peak) in [
            (&mut self.win_1s, HZ, &mut self.stats.peak_1s_packets),
            (
                &mut self.win_100ms,
                HZ / 10,
                &mut self.stats.peak_100ms_packets,
            ),
        ] {
            while w.front().is_some_and(|&f| f + len <= t) {
                w.pop_front();
            }
            w.push_back(t);
            // count only full windows
            if t >= self.pass_first_t.unwrap_or(t) + len {
                *peak = (*peak).max(w.len() as u64);
            }
        }
    }

    fn start_pass(&mut self, t: u64) {
        self.started = true;
        self.pass_first_t = Some(t);
        if self.base.is_none() {
            // multiple of 300 ticks: the PTS/DTS shift is then exact in 90 kHz
            let b = self.start_mux as i64 - t as i64;
            let b = b.div_euclid(300) * 300;
            self.base = Some(b);
            self.timeline = Some(Timeline::new(0, b));
            if let Some(c) = self.ctl.as_mut() {
                c.passes.push((t as i64, 0, c.pass_file_t));
            }
            return;
        }
        let now = self.now as i64;
        let (Some(c), Some(tl)) = (self.ctl.as_mut(), self.timeline.as_mut()) else {
            return;
        };
        let Some(not_before) = c.anchor.take() else {
            return;
        };
        // A seek, or a loop under playback control: the new pass's content
        // times continue after everything read so far, and its access point
        // is shown as soon as possible but after everything already sent.
        let off = (c.c_read + HZ as i64 - t as i64).max(0);
        let off = off.div_euclid(300) * 300 + 300;
        self.pass_offset = off as u64;
        let c0 = off + t as i64;
        let o0 = not_before.max(now);
        tl.push(Seg {
            c0,
            o0: o0.div_euclid(300) * 300 + 300,
            rate: c.rate,
        });
        c.floor = Some(c0);
        c.passes.push((c0, off, c.pass_file_t));
        c.video.reset();
        c.epoch += 1;
    }

    fn process(&mut self, x: Timed<Packet>) {
        self.read_t = x.t;
        let p = x.item;
        let k = self.index[usize::from(packet_pid(&p))];
        if k == 0 {
            return;
        }
        let ei = k as usize - 1;
        if p[1] & 0x80 != 0 {
            self.stats.tei += 1;
            self.damage(ei);
            return;
        }
        let afc = (p[3] >> 4) & 3;
        if afc == 0 {
            return;
        }
        let has_payload = afc & 1 != 0;
        let cc = p[3] & 0x0F;
        let disc = afc & 2 != 0 && p[4] > 0 && p[5] & 0x80 != 0;
        if has_payload {
            let e = &mut self.es[ei];
            match e.last_cc {
                Some(l) if l == cc => {
                    if e.last_pkt == Some(p) {
                        self.stats.duplicates += 1;
                        return;
                    }
                    self.stats.cc_errors += 1;
                    self.damage(ei);
                }
                Some(l) if (l + 1) & 0x0F != cc && !disc => {
                    self.stats.cc_errors += 1;
                    self.damage(ei);
                }
                _ => {}
            }
            let e = &mut self.es[ei];
            e.last_cc = Some(cc);
            e.last_pkt = Some(p);
        } else {
            return;
        }
        if p[3] & 0xC0 != 0 {
            self.stats.scrambled += 1;
            self.damage(ei);
            return;
        }
        let Some(pl) = payload(&p) else { return };
        let rai = afc & 2 != 0 && p[4] > 0 && p[5] & 0x40 != 0;
        let pusi = p[1] & 0x40 != 0;
        if !self.started {
            let start = match self.video {
                Some(v) => v == ei && pusi && x.anchor.is_some() && starts_with_sequence_header(pl),
                None => x.anchor.is_some(),
            };
            if !start {
                return;
            }
            self.start_pass(x.t);
        }
        self.count_rate(x.t);
        let src = self.content(x.t);
        if let Some(c) = self.ctl.as_mut() {
            c.c_read = c.c_read.max(src);
        }
        let chunk = InChunk {
            len: pl.len(),
            t: x.t,
            rai,
            pusi,
        };
        if self.es[ei].mode == EsMode::Packets {
            self.pass_last_t = self.pass_last_t.max(x.t);
            self.stats.units_in += 1;
            self.es[ei].queue.push_back(OutPes {
                bytes: pl.to_vec(),
                chunks: vec![OutChunk {
                    len: chunk.len,
                    src,
                    at: 0,
                    rai,
                    pusi,
                    seam: false,
                }],
                pts: None,
                dts: None,
                decode: None,
                kind: UnitKind::Packet,
                prepared: None,
                kept: false,
            });
            return;
        }
        if pusi {
            self.finish_pes(ei, false);
            let e = &mut self.es[ei];
            e.asm = Some(Assembly {
                bytes: pl.to_vec(),
                chunks: vec![chunk],
                anchor: x.anchor,
                damaged: false,
            });
            e.wait_pusi = false;
        } else {
            let e = &mut self.es[ei];
            if e.wait_pusi {
                return;
            }
            let Some(a) = e.asm.as_mut() else { return };
            a.bytes.extend_from_slice(pl);
            a.chunks.push(chunk);
        }
        let complete = self.es[ei].asm.as_ref().is_some_and(|a| {
            a.bytes.len() >= 6 && {
                let n = u16::from_be_bytes([a.bytes[4], a.bytes[5]]) as usize;
                n != 0 && a.bytes.len() >= 6 + n
            }
        });
        if complete {
            self.finish_pes(ei, false);
        }
    }

    /// The pass-local continuous (PTS, DTS) of a PES header.
    fn pes_times(bytes: &[u8], anchor: Anchor, escr: &mut u64) -> (Option<i64>, Option<i64>) {
        if bytes.len() < 9
            || !isdb_ts::pes::has_optional_header(bytes[3])
            || bytes[6] & 0xC0 != 0x80
        {
            return (None, None);
        }
        let flags = bytes[7] >> 6;
        if bytes[7] & 0x20 != 0 {
            *escr += 1;
        }
        let one = |off: usize| -> Option<i64> {
            (bytes.len() >= off + 5).then(|| anchor.map90(read_ts(&bytes[off..off + 5])))
        };
        let pts = if flags & 2 != 0 { one(9) } else { None };
        let dts = if flags == 3 { one(14) } else { None };
        (pts, dts)
    }

    /// Write mux times (27 MHz) as the PTS / DTS of a PES header laid out
    /// as [`Self::pes_times`] reads it (33-bit values; prefix and marker
    /// bits kept).
    fn write_times(bytes: &mut [u8], pts: Option<i64>, dts: Option<i64>) {
        let v = |x: i64| x.div_euclid(300).rem_euclid(BASE_MODULUS as i64) as u64;
        if let Some(p) = pts
            && bytes.len() >= 14
        {
            write_ts(&mut bytes[9..14], v(p));
        }
        if let Some(d) = dts
            && bytes.len() >= 19
        {
            write_ts(&mut bytes[14..19], v(d));
        }
    }

    fn finish_pes(&mut self, ei: usize, at_eof: bool) {
        let Some(a) = self.es[ei].asm.take() else {
            return;
        };
        let len = if a.bytes.len() >= 6 {
            u16::from_be_bytes([a.bytes[4], a.bytes[5]]) as usize
        } else {
            0
        };
        let incomplete = (len != 0 && a.bytes.len() < 6 + len) || (len == 0 && at_eof);
        if incomplete && at_eof {
            self.stats.eof_dropped += 1;
            return;
        }
        if a.damaged || incomplete || a.bytes.len() < 6 || a.bytes[..3] != [0, 0, 1] {
            self.stats.pes_dropped += 1;
            return;
        }
        let Some(anchor) = a.anchor else {
            self.stats.pes_dropped += 1;
            return;
        };
        let is_video = Some(ei) == self.video;
        if is_video {
            let e = &mut self.es[ei];
            if e.pass_units == 0 && closed_gop(&a.bytes) == Some(false) {
                e.drop_leading_b = true;
            } else if e.drop_leading_b && e.pass_units > 0 {
                match picture_coding_type(&a.bytes) {
                    Some(3) => {
                        self.stats.leading_b_dropped += 1;
                        return;
                    }
                    Some(_) => e.drop_leading_b = false,
                    None => {}
                }
            }
        }
        let (pts, dts) = Self::pes_times(&a.bytes, anchor, &mut self.stats.escr);
        let off = self.pass_offset as i64;
        // under playback control, a new pass starts at its access point:
        // units decoded before it are dropped
        if let Some(c) = self.ctl.as_mut() {
            if let (Some(f), Some(d)) = (c.floor, dts.or(pts))
                && d + off < f
            {
                self.stats.pes_dropped += 1;
                return;
            }
            for v in [pts, dts].into_iter().flatten() {
                c.c_read = c.c_read.max(v + off);
            }
        }
        let e = &mut self.es[ei];
        if let Some(p) = pts {
            if let Some(l) = e.last_pts
                && p > l
            {
                e.pts_step = p - l;
            }
            e.first_pts.get_or_insert(p);
            e.last_pts = Some(e.last_pts.map_or(p, |l| l.max(p)));
        }
        if let Some(d) = dts.or(pts) {
            e.last_decode = Some(e.last_decode.map_or(d, |l| l.max(d)));
        }
        let seam_end = if self.stats.passes > 0 && self.ctl.is_none() {
            self.pass_first_t.unwrap_or(0) + SEAM_WINDOW
        } else {
            0
        };
        let mut chunks = Vec::with_capacity(a.chunks.len());
        for c in &a.chunks {
            chunks.push(OutChunk {
                len: c.len,
                src: c.t as i64 + off,
                at: 0,
                rai: c.rai,
                pusi: c.pusi,
                seam: c.t < seam_end,
            });
            self.pass_last_t = self.pass_last_t.max(c.t);
        }
        e.pass_units += 1;
        self.stats.units_in += 1;
        let unit = OutPes {
            kind: UnitKind::Pes {
                picture: if is_video {
                    picture_coding_type(&a.bytes)
                } else {
                    None
                },
            },
            bytes: a.bytes,
            chunks,
            pts: pts.map(|v| v + off),
            dts: dts.map(|v| v + off),
            decode: None,
            prepared: None,
            kept: false,
        };
        if self.ctl.is_some() && self.stream_types[ei] == 0x0F {
            self.split_adts(ei, unit);
        } else {
            self.es[ei].queue.push_back(unit);
        }
    }

    /// Cut an AAC PES into one PES per ADTS frame (playback control). The
    /// PES's PTS belongs to the first frame starting in it (H.222.0
    /// §2.4.3.7); the frames after it are one frame period apart.
    fn split_adts(&mut self, ei: usize, u: OutPes) {
        let e = &mut self.es[ei];
        let Some(es0) = pes_es_start(&u.bytes) else {
            return;
        };
        let sp = e.adts.get_or_insert_with(AdtsSplit::default);
        sp.stream_id = u.bytes[3];
        let start = sp.buf.len();
        if let Some(p) = u.pts {
            sp.pts = Some((start, p));
        }
        // content arrival time of each source packet's bytes
        let mut pos = 0usize;
        for c in &u.chunks {
            let end = pos + c.len;
            if end > es0 {
                sp.runs.push((start + pos.saturating_sub(es0), c.src));
            }
            pos = end;
        }
        sp.buf.extend_from_slice(&u.bytes[es0.min(u.bytes.len())..]);
        loop {
            let sp = e.adts.as_mut().expect("set above");
            // resynchronise on a syncword
            let Some(s) = (0..sp.buf.len()).find(|&i| adts_len(&sp.buf[i..]).is_some()) else {
                let keep = sp.buf.len().min(6);
                let cut = sp.buf.len() - keep;
                Self::consume(sp, cut);
                return;
            };
            Self::consume(sp, s);
            let Some(n) = adts_len(&sp.buf) else { return };
            if sp.buf.len() < n {
                return; // the rest comes with the next PES
            }
            if sp.buf.len() >= n + 2 && adts_len(&sp.buf[n..]).is_none() {
                Self::consume(sp, 1); // a false syncword
                continue;
            }
            let fs = [
                96000, 88200, 64000, 48000, 44100, 32000, 24000, 22050, 16000, 12000, 11025, 8000,
            ]
            .get(((sp.buf[2] >> 2) & 0x0F) as usize)
            .copied()
            .unwrap_or(48000);
            let dur = (1024 * HZ as i64 + fs / 2) / fs;
            let pts = match sp.pts {
                Some((0, p)) => {
                    sp.pts = None;
                    Some(p)
                }
                _ => sp.last_pts.map(|l| l + dur),
            };
            let src = sp
                .runs
                .iter()
                .take_while(|r| r.0 == 0 || r.0 < n)
                .last()
                .map_or(0, |r| r.1);
            let frame = sp.buf[..n].to_vec();
            let stream_id = sp.stream_id;
            Self::consume(sp, n);
            let Some(pts) = pts else { continue };
            sp.last_pts = Some(pts);
            // PES: header with PTS only, data_alignment_indicator = 1
            let mut b = vec![0, 0, 1, stream_id, 0, 0, 0x84, 0x80, 5, 0x21, 0, 1, 0, 1];
            let plen = (b.len() - 6 + frame.len()) as u16;
            b[4..6].copy_from_slice(&plen.to_be_bytes());
            b.extend_from_slice(&frame);
            let chunks = b
                .chunks(184)
                .enumerate()
                .map(|(k, c)| OutChunk {
                    len: c.len(),
                    src,
                    at: 0,
                    rai: false,
                    pusi: k == 0,
                    seam: false,
                })
                .collect();
            e.queue.push_back(OutPes {
                bytes: b,
                chunks,
                pts: Some(pts),
                dts: None,
                decode: None,
                kind: UnitKind::AudioFrame { dur, copy: false },
                prepared: None,
                kept: false,
            });
        }
    }

    /// Drop the first `n` bytes of a splitter's buffer.
    fn consume(sp: &mut AdtsSplit, n: usize) {
        if n == 0 {
            return;
        }
        sp.buf.drain(..n);
        // the run covering the new first byte starts at 0
        let k = sp.runs.partition_point(|r| r.0 <= n);
        sp.runs.drain(..k.saturating_sub(1));
        for r in &mut sp.runs {
            r.0 = r.0.saturating_sub(n);
        }
        if let Some((at, p)) = sp.pts {
            sp.pts = (at >= n).then(|| (at - n, p)).or(Some((0, p)));
        }
    }

    /// Per-pass state back to the start of a pass.
    fn reset_pass(&mut self) {
        self.win_1s.clear();
        self.win_100ms.clear();
        self.clock = PcrClock::new(Some(self.pcr_pid));
        self.started = false;
        self.pass_first_t = None;
        self.pass_last_t = 0;
        self.read_t = 0;
        for e in &mut self.es {
            e.last_cc = None;
            e.last_pkt = None;
            e.asm = None;
            e.wait_pusi = true;
            e.drop_leading_b = false;
            e.pass_units = 0;
            e.first_pts = None;
            e.last_pts = None;
            e.last_decode = None;
            e.adts = None;
        }
    }

    fn end_pass(&mut self) {
        self.clock.finish();
        while let Some(x) = self.clock.pop() {
            self.process(x);
        }
        for ei in 0..self.es.len() {
            self.finish_pes(ei, true);
        }
        if !self.started {
            self.failure = Some(format!(
                "{}: no access point (video PES starting with a sequence_header, timed by PCR {:#06x}) in the whole file",
                self.name, self.pcr_pid
            ));
            self.reading_done = true;
            return;
        }
        self.stats.passes += 1;
        self.stats.source_ticks += self.pass_last_t - self.pass_first_t.unwrap_or(0);
        self.stats.source_packets += std::mem::take(&mut self.pass_packets);
        self.win_1s.clear();
        self.win_100ms.clear();
        if let Some(c) = self.ctl.as_mut() {
            // under playback control the next pass is anchored like a seek
            if self.at_eof == AtEof::End {
                self.reading_done = true;
                return;
            }
            let tl = self.timeline.as_ref().expect("started");
            c.anchor = Some(tl.map(c.c_read));
            c.pass_file_t = 0;
            self.reset_pass();
            return;
        }
        if self.period.is_none() {
            // Loop period P (pass-local ticks): the next pass starts
            // arriving after the last packet of this one, after the last
            // decoding time of this one (the decoder buffers are empty
            // again, so the next pass's first PES — multiplexed at the
            // file start into whatever buffer state the source had — cannot
            // overflow them), and every PID's PTS continue after its last
            // one plus one PTS step.
            // The drain wait is capped so that no PID's PTS jump exceeds
            // MAX_SEAM_GAP (H.222.0 §2.7.4: PTS at most 0.7 s apart); what
            // the cap leaves is absorbed by the T-STD hold at emission.
            let first = self.pass_first_t.unwrap_or(0) as i64;
            let ms1 = HZ as i64 / 1000;
            let mut p = self.pass_last_t as i64 - first + ms1;
            let mut drain = p;
            let mut min_span = i64::MAX;
            for e in &self.es {
                if let (Some(a), Some(b)) = (e.first_pts, e.last_pts) {
                    p = p.max(b - a + e.pts_step.max(1));
                    min_span = min_span.min(b - a);
                }
                if let Some(d) = e.last_decode {
                    drain = drain.max(d - first + ms1);
                }
            }
            if min_span != i64::MAX {
                drain = drain.min(min_span + MAX_SEAM_GAP as i64);
            }
            p = p.max(drain);
            self.period = Some((p.max(1) as u64).div_ceil(300) * 300);
        }
        if self.at_eof == AtEof::End {
            self.reading_done = true;
            return;
        }
        // next pass: same file, same pass-local times, one period later
        self.pass_offset += self.period.unwrap_or(0);
        self.reset_pass();
    }

    /// Map the head unit of `ei` onto the timeline (PTS/DTS written, due
    /// times set), applying playback control: skipped video pictures and
    /// audio frames are dropped here, repeated audio frames copied. False
    /// when nothing can start now (empty queue, paused, no timeline yet).
    fn prepare(&mut self, ei: usize) -> bool {
        let video = self.video;
        let Self {
            es,
            timeline,
            ctl,
            stats,
            ..
        } = self;
        let Some(tl) = timeline.as_ref() else {
            return false;
        };
        let epoch = ctl.as_ref().map_or(0, |c| c.epoch);
        if ctl.as_ref().is_some_and(|c| c.paused.is_some()) {
            return false;
        }
        let e = &mut es[ei];
        loop {
            let Some(u) = e.queue.front_mut() else {
                return false;
            };
            if u.prepared == Some(epoch) {
                return true;
            }
            match u.kind {
                UnitKind::Packet => {
                    for c in &mut u.chunks {
                        c.at = tl.map(c.src).max(0) as u64;
                    }
                }
                UnitKind::Pes { picture } => {
                    let Some(d) = u.decode_c() else {
                        for c in &mut u.chunks {
                            c.at = tl.map(c.src).max(0) as u64;
                        }
                        u.prepared = Some(epoch);
                        return true;
                    };
                    if let Some(c) = ctl.as_mut() {
                        if Some(ei) == video && !u.kept {
                            let rate = tl.rate_at(d);
                            let shown = u.pts.map(|p| tl.map(p));
                            let spaced = rate <= 1.0
                                || shown.is_none_or(|p| {
                                    c.shown.iter().all(|&k| (p - k).abs() >= c.frame * 9 / 10)
                                });
                            if !c.video.decide(picture, u.bytes.len(), rate, spaced) {
                                e.queue.pop_front();
                                stats.skipped_pictures += 1;
                                continue;
                            }
                            u.kept = true;
                            if let Some(p) = shown {
                                if c.shown.len() >= 8 {
                                    c.shown.pop_front();
                                }
                                c.shown.push_back(p);
                            }
                        }
                        if e.last_out_decode.is_some_and(|l| tl.map(d) <= l) {
                            // behind what was sent (a seek or rate change)
                            e.queue.pop_front();
                            stats.pes_dropped += 1;
                            continue;
                        }
                    }
                    let out = tl.map(d);
                    let (pts, dts) = (u.pts.map(|v| tl.map(v)), u.dts.map(|v| tl.map(v)));
                    Self::write_times(&mut u.bytes, pts, dts);
                    for c in &mut u.chunks {
                        c.at = (out - (d - c.src)).max(0) as u64;
                    }
                    u.decode = Some(out.max(0) as u64);
                }
                UnitKind::AudioFrame { dur, copy } => {
                    if copy {
                        e.queue.pop_front(); // a repetition prepared for an older timeline
                        continue;
                    }
                    let c = u.pts.expect("audio frames carry a PTS");
                    match e.slots.peek(tl.map(c), tl.map(c + dur), dur) {
                        Slot::Skip => {
                            e.queue.pop_front();
                            stats.skipped_audio_frames += 1;
                            continue;
                        }
                        Slot::Emit { pts, again } => {
                            let set = |u: &mut OutPes| {
                                Self::write_times(&mut u.bytes, Some(pts), None);
                                for ch in &mut u.chunks {
                                    ch.at = (pts - (c - ch.src)).max(0) as u64;
                                }
                                u.decode = Some(pts.max(0) as u64);
                                u.prepared = Some(epoch);
                            };
                            if again {
                                let mut cp = u.clone();
                                cp.kind = UnitKind::AudioFrame { dur, copy: true };
                                set(&mut cp);
                                u.prepared = None;
                                e.queue.push_front(cp);
                                stats.repeated_audio_frames += 1;
                            } else {
                                set(u);
                            }
                            return true;
                        }
                    }
                }
            }
            u.prepared = Some(epoch);
            return true;
        }
    }

    /// Next chunk of `ei` to send, if any can go now.
    pub(crate) fn head(&mut self, ei: usize) -> Option<Head> {
        if self.es[ei].cur.is_none() && !self.prepare(ei) {
            return None;
        }
        let e = &self.es[ei];
        match &e.cur {
            Some((u, i, pos)) => Some(Head {
                at: u.chunks[*i].at,
                len: u.chunks[*i].len,
                decode: u.decode,
                rest: u.bytes.len() - pos,
                seam: u.chunks[*i].seam,
            }),
            None => e.queue.front().map(|u| Head {
                at: u.chunks[0].at,
                len: u.chunks[0].len,
                decode: u.decode,
                rest: u.bytes.len(),
                seam: u.chunks[0].seam,
            }),
        }
    }

    /// Send the next chunk of `ei` in the slot at mux time `t`.
    pub(crate) fn emit(&mut self, ei: usize, t: u64) -> Result<Packet> {
        let e = &mut self.es[ei];
        if e.cur.is_none() {
            let u = e
                .queue
                .pop_front()
                .ok_or_else(|| MuxError::Config("relay: emit on an empty queue".into()))?;
            if let (Some(ts), Some(d), EsMode::Pes) = (e.tstd.as_mut(), u.decode, e.mode) {
                ts.add_au(d, u.bytes.len());
            }
            if let Some(c) = self.ctl.as_mut() {
                if let Some(d) = u.decode {
                    let d = d as i64;
                    e.last_out_decode = Some(d);
                    c.o_sent = Some(c.o_sent.map_or(d, |o| o.max(d)));
                    if let UnitKind::AudioFrame { dur, .. } = u.kind {
                        e.slots.commit(d, dur);
                    }
                }
                let content = [u.pts, u.dts, u.chunks.last().map(|c| c.src)]
                    .into_iter()
                    .flatten()
                    .max();
                if let Some(v) = content {
                    c.c_sent = Some(c.c_sent.map_or(v, |o| o.max(v)));
                    e.c_sent = Some(e.c_sent.map_or(v, |o| o.max(v)));
                }
            }
            e.cur = Some((u, 0, 0));
        }
        let (u, i, pos) = e.cur.as_mut().expect("current unit");
        let c = u.chunks[*i];
        let data = &u.bytes[*pos..*pos + c.len];
        let cc = e.cc.next(true);
        let pkt = if c.len == 184 {
            TsPacket::with_payload(e.pid, c.pusi, cc, data)?
        } else {
            let fields = (c.rai && 184 - c.len >= 2).then(|| AdaptationFields {
                random_access_indicator: true,
                ..Default::default()
            });
            TsPacket::with_adaptation(e.pid, c.pusi, cc, fields, data)?
        };
        if let Some(ts) = e.tstd.as_mut() {
            ts.push(t, c.len);
        }
        self.stats.packets += 1;
        self.stats.max_lateness = self.stats.max_lateness.max(t.saturating_sub(c.at));
        *pos += c.len;
        *i += 1;
        if *i == u.chunks.len() {
            if u.decode.is_some_and(|d| t > d) && e.tstd.is_some() {
                self.stats.late_pes += 1;
            }
            e.cur = None;
        }
        Ok(pkt.encode()?)
    }

    // ---- playback control

    /// Under playback control.
    pub(crate) fn controlled(&self) -> bool {
        self.ctl.is_some()
    }

    /// Enable playback control (before the first frame).
    pub(crate) fn enable_control(&mut self) {
        self.ctl.get_or_insert(Control {
            epoch: 0,
            paused: None,
            rate: 1.0,
            c_sent: None,
            o_sent: None,
            c_read: 0,
            anchor: None,
            floor: None,
            passes: Vec::new(),
            pass_file_t: 0,
            reader_gen: 0,
            video: VideoThinning::default(),
            frame: self.frame_period,
            shown: VecDeque::new(),
        });
    }

    /// Content time from which a control change applies: after everything
    /// already sent (the receiver's buffers keep their timing).
    fn boundary(&self) -> Option<i64> {
        let c = self.ctl.as_ref()?;
        c.c_sent
            .or_else(|| self.timeline.as_ref().map(|t| t.last().c0))
    }

    /// Pause at mux time `now` (the unit being sent completes).
    pub(crate) fn pause(&mut self, now: u64) -> bool {
        let Some(c) = self.ctl.as_mut() else {
            return false;
        };
        if c.paused.is_none() {
            c.paused = Some(now);
            c.epoch += 1;
        }
        true
    }

    /// Resume at mux time `now`: everything not yet sent is shown that much
    /// later. The delay applies from the stream that is least far ahead
    /// (audio is sent closer to its decoding than video), so that every
    /// stream's next unit is delayed.
    pub(crate) fn resume(&mut self, now: u64) -> bool {
        let b = self
            .es
            .iter()
            .filter(|e| e.mode == EsMode::Pes)
            .filter_map(|e| e.c_sent)
            .min()
            .or_else(|| self.boundary());
        let Some(c) = self.ctl.as_mut() else {
            return false;
        };
        let Some(p) = c.paused.take() else {
            return true;
        };
        let d = now.saturating_sub(p) as i64;
        if let (Some(tl), Some(b)) = (self.timeline.as_mut(), b) {
            tl.change(b, d.div_euclid(300) * 300, Some(c.rate));
        }
        c.video.reset();
        c.epoch += 1;
        true
    }

    /// Play `rate` times faster than real time from what is sent next.
    /// What follows starts no sooner than the source's longest lead from
    /// now: the receiver's buffers then hold less than the source keeps in
    /// them (after fast play, or at the start), and the units at the new
    /// rate need that time to arrive (a short still picture instead of
    /// late PES).
    pub(crate) fn set_rate(&mut self, rate: f64) -> bool {
        let b = self.boundary();
        let (now, lead) = (self.now as i64, self.max_lead);
        let Some(c) = self.ctl.as_mut() else {
            return false;
        };
        c.rate = rate;
        if c.paused.is_none()
            && let (Some(tl), Some(b)) = (self.timeline.as_mut(), b)
        {
            let gap = (now + lead - tl.map(b)).max(0);
            tl.change(b, gap.div_euclid(300) * 300, Some(rate));
        }
        c.video.reset();
        c.epoch += 1;
        true
    }

    /// Jump `delta_s` seconds from the position shown; returns the file
    /// position (s) reading continues from.
    pub(crate) fn seek(&mut self, delta_s: f64) -> Option<f64> {
        let pos = self.playback_status()?.position_s;
        let dur = self.duration as f64 / HZ as f64;
        let target = (pos + delta_s).clamp(0.0, (dur - 1.0).max(0.0));
        let p = {
            let k = self
                .seek_index
                .partition_point(|x| x.t as f64 <= target * HZ as f64);
            *self.seek_index.get(k.saturating_sub(1))?
        };
        let now = self.now as i64;
        let c = self.ctl.as_mut()?;
        c.reader_gen += 1;
        self.reader
            .send(ReaderCmd::Seek {
                epoch: c.reader_gen,
                offset: p.offset,
            })
            .ok()?;
        c.anchor = Some(c.o_sent.unwrap_or(now).max(now));
        c.pass_file_t = p.t;
        c.epoch += 1;
        // what is queued is not sent; the unit being sent completes
        for e in &mut self.es {
            e.queue.clear();
        }
        self.reading_done = false;
        self.pass_packets = 0;
        self.reset_pass();
        Some(p.t as f64 / HZ as f64)
    }

    /// Position shown, rate and pause state (under playback control).
    pub(crate) fn playback_status(&self) -> Option<PlaybackStatus> {
        let c = self.ctl.as_ref()?;
        let dur = self.duration as f64 / HZ as f64;
        let position_s = match (&self.timeline, c.passes.is_empty()) {
            (Some(tl), false) => {
                let shown = tl.inverse(c.paused.unwrap_or(self.now) as i64);
                let shown = c.c_sent.map_or(shown, |s| shown.min(s));
                position(&c.passes, shown).clamp(0.0, dur)
            }
            _ => 0.0,
        };
        Some(PlaybackStatus {
            position_s,
            duration_s: dur,
            rate: c.rate,
            paused: c.paused.is_some(),
        })
    }

    /// One-line status.
    pub(crate) fn status(&self) -> String {
        let s = &self.stats;
        let mut rates = String::new();
        if s.peak_1s_packets > 0 {
            rates = format!(
                ", source peak {:.2} Mbit/s (1 s) {:.2} (100 ms)",
                s.peak_1s_bps() / 1e6,
                s.peak_100ms_bps() / 1e6
            );
            // completed passes, else the pass so far
            let partial = RelayStats {
                source_packets: self.pass_packets,
                source_ticks: self
                    .pass_last_t
                    .saturating_sub(self.pass_first_t.unwrap_or(0)),
                ..*s
            };
            let m = if s.source_ticks > 0 { s } else { &partial };
            if m.source_ticks > 0 {
                rates += &format!(", mean {:.2} Mbit/s", m.mean_bps() / 1e6);
            }
            if let Some(p) = self.period {
                rates += &format!(", loop period {:.3} s", p as f64 / HZ as f64);
            }
        }
        format!(
            "relay {}: {}, late PES {}, lag {:.3} s (max {:.3} s), T-STD holds {}{rates}, dropped PES {} damaged + {} leading B + {} cut at EOF, cc-errors {}",
            Path::new(&self.name)
                .file_name()
                .map(|f| f.to_string_lossy().to_string())
                .unwrap_or_default(),
            if self.finished() {
                format!("ended after {} pass(es)", s.passes)
            } else {
                format!("pass {}", s.passes + 1)
            },
            s.late_pes,
            s.lag as f64 / HZ as f64,
            s.max_lateness as f64 / HZ as f64,
            s.tstd_holds,
            s.pes_dropped,
            s.leading_b_dropped,
            s.eof_dropped,
            s.cc_errors,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spec_table_2_21_timestamp_rewrite_keeps_prefix_and_markers() {
        // '0011' PTS prefix, all marker bits '1'
        let mut b = [0x31, 0x00, 0x01, 0x00, 0x01];
        assert_eq!(read_ts(&b), 0);
        let v = 0x1_2345_6789 & (BASE_MODULUS - 1);
        write_ts(&mut b, v);
        assert_eq!(read_ts(&b), v);
        assert_eq!(b[0] >> 4, 0b0011);
        assert_eq!(b[0] & 1, 1);
        assert_eq!(b[2] & 1, 1);
        assert_eq!(b[4] & 1, 1);
        write_ts(&mut b, BASE_MODULUS + 5); // wraps
        assert_eq!(read_ts(&b), 5);
    }

    #[test]
    fn video_pes_markers() {
        let mut pes = vec![0, 0, 1, 0xE0, 0, 0, 0x80, 0x80, 5, 0x21, 0, 1, 0, 1];
        pes.extend_from_slice(&[0, 0, 1, 0xB3, 1, 2, 3, 4]);
        pes.extend_from_slice(&[0, 0, 1, 0xB8, 0, 8, 0, 0x80]); // open GOP
        pes.extend_from_slice(&[0, 0, 1, 0x00, 0x00, 0x08 | (1 << 3), 0, 0]);
        assert!(starts_with_sequence_header(&pes));
        assert_eq!(closed_gop(&pes), Some(false));
        assert_eq!(picture_coding_type(&pes), Some(1));
    }

    fn dc(id: u16, info: &[u8]) -> Descriptor {
        Descriptor::DataComponent(isdb_si::descriptor::DataComponent {
            data_component_id: id,
            additional_data_component_info: info.to_vec(),
        })
    }

    #[test]
    fn classification_rules() {
        let role = |st, tag, d: &[Descriptor], pes| classify(st, tag, d, pes).role;
        let sync = Some(PesHint {
            stream_id: 0xBD,
            has_pts: true,
        });
        let asynchronous = Some(PesHint {
            stream_id: 0xBF,
            has_pts: false,
        });
        // stream_type (H.222.0 Table 2-34)
        for t in [0x01, 0x02, 0x1B, 0x24] {
            assert_eq!(role(t, None, &[], None), Role::Video);
        }
        for t in [0x03, 0x04, 0x0F, 0x11] {
            assert_eq!(role(t, Some(0x10), &[], None), Role::Audio);
        }
        for t in [0x05, 0x0B, 0x0C, 0x0D] {
            assert_eq!(role(t, Some(0x40), &[], None), Role::Carousel);
        }
        assert_eq!(role(0x07, None, &[], None), Role::Other);
        // 0x06 without / with another data_component_id: data PES
        assert_eq!(role(0x06, Some(0x30), &[], sync), Role::DataPes);
        assert_eq!(role(0x06, None, &[dc(0x000C, &[])], sync), Role::DataPes);
        // 0x0008: (1) component_tag ranges win over everything else
        let cap = [dc(0x0008, &[0x3C])]; // Timing '00' would say superimpose
        assert_eq!(role(0x06, Some(0x30), &cap, asynchronous), Role::Captions);
        assert_eq!(role(0x06, Some(0x37), &cap, asynchronous), Role::Captions);
        assert_eq!(
            role(0x06, Some(0x38), &[dc(0x0008, &[0x3D])], sync),
            Role::Superimpose
        );
        assert_eq!(
            role(0x06, Some(0x3F), &[dc(0x0008, &[0x3D])], sync),
            Role::Superimpose
        );
        // (2) Timing of additional_arib_caption_info (B24 Table 9-16)
        assert_eq!(
            role(0x06, Some(0x87), &[dc(0x0008, &[0x3D])], asynchronous),
            Role::Captions
        );
        assert_eq!(
            role(0x06, None, &[dc(0x0008, &[0x3C])], sync),
            Role::Superimpose
        );
        assert_eq!(
            role(0x06, None, &[dc(0x0008, &[0x3E])], sync),
            Role::Superimpose
        );
        // (3) synchronous / asynchronous PES; (4) caption by default
        assert_eq!(
            role(0x06, None, &[dc(0x0008, &[0x3F])], sync),
            Role::Captions
        );
        assert_eq!(
            role(0x06, None, &[dc(0x0008, &[])], asynchronous),
            Role::Superimpose
        );
        assert_eq!(role(0x06, None, &[dc(0x0008, &[])], sync), Role::Captions);
        assert_eq!(role(0x06, None, &[dc(0x0008, &[])], None), Role::Captions);
        let r = classify(0x06, Some(0x30), &cap, None).reason;
        assert!(r.contains("from knowledge") && r.contains("0x0008"), "{r}");
    }

    #[test]
    fn names_fold_full_width_and_case() {
        assert_eq!(normalize_name("ＴＶ１２テスト"), "tv12テスト");
        assert_eq!(normalize_name(" TV 12\u{3000}"), "tv12");
        assert!(normalize_name("ＴＶ１２テスト").contains(&normalize_name("Tv12")));
    }

    fn es(src: u16, role: Role, tag: Option<u8>) -> RelayedEs {
        let src = Pid::new(src).unwrap();
        RelayedEs {
            src_pid: src,
            pid: src,
            stream_type: 0,
            component_tag: tag,
            role,
            role_reason: String::new(),
            rank: 0,
            descriptors: Vec::new(),
            dropped_descriptors: Vec::new(),
            ca_descriptors: Vec::new(),
            copy_descriptors: Vec::new(),
            mode: EsMode::Pes,
            info: StreamInfo::None,
            tstd: None,
            mean_bps: 0.0,
            source_tstd: None,
            source_lead_s: None,
        }
    }

    /// H.222.0 Table 2-60: CA_PID is the 13 bits after 3 reserved bits;
    /// the remap keeps the reserved bits and the private data.
    #[test]
    fn spec_table_2_60_ca_pid_remap_keeps_reserved_bits() {
        let body = [0x00, 0x05, 0xE1, 0x21, 0xAB];
        assert_eq!(ca_fields(&body), Some((0x0005, Pid::new(0x0121).unwrap())));
        let r = remap_ca(&body, |p| {
            if p == Pid::new(0x0121).unwrap() {
                Pid::new(0x0901).unwrap()
            } else {
                p
            }
        });
        assert_eq!(r.tag, 0x09);
        assert_eq!(r.hex, "0005e901ab");
        let null = remap_ca(&[0x00, 0x05, 0xFF, 0xFF], |_| Pid::new(0x0901).unwrap());
        assert_eq!(null.hex, "0005e901", "every CA_PID goes through the map");
        assert_eq!(ca_fields(&[0x00, 0x05, 0xE1]), None);
    }

    /// Precedence of the source copy control: PMT program loop > relayed
    /// ES loops (most restrictive) > SDT > EIT present > EIT following.
    #[test]
    fn source_copy_control_precedence() {
        use DigitalRecordingControl::*;
        let f = |place, v| SourceCopyFinding {
            place,
            signal: CopySignal::Recording(v),
        };
        let mut c = SourceCopyControl {
            findings: vec![
                f(CopySignalPlace::EitFollowing, CopyNever),
                f(CopySignalPlace::EitPresent, CopyOneGeneration),
            ],
        };
        assert_eq!(
            c.effective(),
            Some((CopyOneGeneration, CopySignalPlace::EitPresent))
        );
        c.findings.push(f(CopySignalPlace::Sdt, CopyFree));
        assert_eq!(c.effective(), Some((CopyFree, CopySignalPlace::Sdt)));
        c.findings.push(f(
            CopySignalPlace::PmtEs(Pid::new(0x0140).unwrap()),
            CopyOneGeneration,
        ));
        c.findings.push(f(
            CopySignalPlace::PmtEs(Pid::new(0x0141).unwrap()),
            CopyNever,
        ));
        assert_eq!(
            c.effective(),
            Some((CopyNever, CopySignalPlace::PmtEs(Pid::new(0x0141).unwrap())))
        );
        c.findings.push(f(CopySignalPlace::PmtProgram, CopyFree));
        assert_eq!(c.effective(), Some((CopyFree, CopySignalPlace::PmtProgram)));
        assert_eq!(c.describe(), "copy-free (PMT)");
        assert!(
            c.findings_text()
                .contains("PMT ES 0x0141 copy-never ('11')"),
            "{}",
            c.findings_text()
        );
        assert_eq!(SourceCopyControl::default().describe(), "none signalled");
    }

    #[test]
    fn pid_plan_by_role_and_component_tag() {
        // PMT order deliberately not the component_tag order
        let mut v = vec![
            es(0x0200, Role::Audio, Some(0x11)),
            es(0x0901, Role::Video, Some(0x00)),
            es(0x0310, Role::Audio, Some(0x10)),
            es(0x0777, Role::Captions, Some(0x30)),
            es(0x0123, Role::Superimpose, Some(0x38)),
            es(0x0124, Role::Superimpose, None),
            es(0x0500, Role::DataPes, None),
            es(0x0501, Role::Carousel, Some(0x40)),
        ];
        rank_and_assign(&mut v, Some(&PidPlan::default()));
        let got: Vec<(u16, u16, String)> = v
            .iter()
            .map(|e| (e.src_pid.get(), e.pid.get(), e.role_label()))
            .collect();
        assert_eq!(
            got,
            vec![
                (0x0200, 0x0113, "audio (secondary)".into()),
                (0x0901, 0x0111, "video".into()),
                (0x0310, 0x0112, "audio (main)".into()),
                (0x0777, 0x0130, "captions".into()),
                (0x0123, 0x0138, "superimpose".into()),
                (0x0124, 0x0139, "superimpose #2".into()),
                // tagged before untagged within the data group
                (0x0500, 0x0141, "data PES #2".into()),
                (0x0501, 0x0140, "data carousel".into()),
            ]
        );
        // an explicit list per role, extension skipping the plan's PIDs
        let plan = PidPlan {
            video: crate::config::PidList(vec![Pid::new(0x0100).unwrap()]),
            audio: crate::config::PidList(vec![
                Pid::new(0x0110).unwrap(),
                Pid::new(0x0101).unwrap(),
            ]),
            ..PidPlan::default()
        };
        let mut v = vec![
            es(1, Role::Video, None),
            es(2, Role::Video, None),
            es(3, Role::Audio, None),
            es(4, Role::Audio, None),
            es(5, Role::Audio, None),
        ];
        rank_and_assign(&mut v, Some(&plan));
        let pids: Vec<u16> = v.iter().map(|e| e.pid.get()).collect();
        // video #2 → 0x0101 is the plan's second audio PID: skipped → 0x0102
        assert_eq!(pids, vec![0x0100, 0x0102, 0x0110, 0x0101, 0x0103]);
        // no plan (keep / explicit list): PIDs untouched, ranks set
        let mut v = vec![
            es(7, Role::Audio, Some(0x11)),
            es(8, Role::Audio, Some(0x10)),
        ];
        rank_and_assign(&mut v, None);
        assert_eq!((v[0].pid.get(), v[0].rank, v[1].rank), (7, 1, 0));
    }
}
