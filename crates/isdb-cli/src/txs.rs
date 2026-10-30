//! `isdb tx --system isdbs`: TS packets per TS → ISDB-S (ITU-R BO.1408-1,
//! ARIB STD-B20 system) → IQ file with its SigMF sidecar, or a radio.
//!
//! ```text
//! reader thread per file TS ──packets──► producer thread ──superframes (bounded)──► sink loop
//! ```
//!
//! The producer assembles each superframe's packets per TS (a TS file
//! through [`PacketStream`], or synthetic packets), runs
//! [`isdbs_phy::tx::Transmitter::superframe`] and the pulse shaper
//! ([`isdb_frontend::shape::Shaper`], BO.1408-1 Annex 1 §9) and queues the
//! samples ahead of the sink, as [`crate::producer`] does for ISDB-T.
//! On-air sequence: a pre-roll frame (the last frame of a null
//! superframe: a receiver's timing and carrier loops settle during its
//! first symbols, which would otherwise damage the first slots of the
//! first superframe, spread over its 8 frames by the interleaver), then
//! `--lead-superframes` null superframes (default 1: a receiver that
//! starts late or acquires during it still gets every data superframe),
//! the data superframes, then (unless `--no-flush` or stopped) one null
//! frame and the shaper's tail, so that the transmit and receive filters
//! do not cut the last data superframe. Nothing else needs draining: the
//! TMCC, the energy dispersal and the interleaver each span one
//! superframe.
//!
//! **Placement of a TS file in its slots** ([`plan_inputs`], `--ts-pacing`,
//! docs/plans/isdbs-next/pcr-pacing.md §7). A relative TS with N_r sent
//! slots per frame goes out at a constant packet rate, one packet every
//! 1 382 400 / (37·N_r) ticks of 27 MHz ([`SlotClock::isdbs`]; the uniform
//! per-TS grid a receiver outputs, `SPEC-NOTES.md` "ISDB-S TS timing
//! model"). A TS file that carries a PCR is placed by it
//! ([`crate::pace::SlotPacer`], [`Placement::Pcr`]): each packet in the
//! first free slot at or after the time its PCRs give it (H.222.0 §2.4.2.2
//! eq. 2-4), input null packets dropped, nulls in the slots left over, and
//! every PCR corrected by the time its packet moved (H.222.0 Annex D.0.4),
//! so the TS keeps its own rate. A regular file read with `--feed file` or
//! `loop` is probed first and refused when its mean rate exceeds its
//! slots' capacity (unless `--allow-overrate`); a pipe or stdin is decided
//! by its first PCR ([`Placement::FirstPcr`]), and so is every input read
//! with `--feed live`, within [`LIVE_SNIFF`] of wall time. Without a PCR
//! and with `--ts-pacing arrival`, the packets go into the slots in
//! arrival order, nulls included, unmodified ([`Placement::Arrival`]).
//!
//! The two waits: a file, a looped file and a pipe read with `--feed file`
//! are read as the slots need them, blocking for input
//! ([`WaitMode::Block`]); the producer runs ahead of air, bounded by its
//! sink (the FPGA path keeps about 1.2 s of codes queued, the Pluto's
//! kernel buffers), so a pipe whose writer stalls stalls the transmission.
//! A paced `--feed live` input waits for its packets only up to a deadline
//! on the wall clock ([`WaitMode::Live`], `pace.rs` "Live inputs"): due
//! time 0 (its first PCR) goes [`crate::pace::LIVE_LATENCY`] after it
//! arrived, a superframe waits for its packets until
//! [`crate::pace::LIVE_GRACE`] after that schedule, and then goes out as
//! it is, the packets that come later placed late, restamped by their lag
//! and counted (`deadline-misses`, `live-input lag` in the status lines).
//! With `--feed loop` a
//! paced file is joined seamlessly at each rewind: the reader thread tags
//! every packet with its loop pass ([`PacketStream::next_tagged`]) and the
//! pacer moves each pass's PCR, PTS and DTS on by the loop period and
//! renumbers its continuity counters (`pace.rs`, "Loop seams"; an arrival
//! order TS keeps its seams). Placement happens in [`TsInputs::take`], before
//! [`Modulate`], so the IQ and code paths carry the same packets;
//! `--placed-ts` writes them ([`PlacedTs`]).
//!
//! The samples are unit RMS (radios quantise them at `--backoff-db`); an
//! `--iq` file holds them at `--backoff-db` below full scale in every
//! format, cf32 included, because the ISDB-S receiver reads samples as a
//! fraction of full scale (its front end's input is 12-bit).
//!
//! To a radio whose FPGA has the transmit block (`--offload`,
//! [`isdb_radio::Offload::TxSymbols`]) the producer hands the symbols as
//! codes instead ([`Modulate::codes`], `Transmitter::superframe_codes`),
//! with the same pre-roll, lead and flush frame and no shaper: the block
//! shapes them (`isdb_frontend::shape::fixed`, the same arithmetic in the
//! FPGA), and its filter's tail drains on the zero symbols that end the
//! stream.

use crate::bands::Channel;
use crate::input::{
    InputRates, PROBE_BYTES, PROBE_SECONDS, RateShare, RateTarget, check_rates, is_regular_file,
    probe_rates,
};
use crate::pace::{SlotClock, SlotPacer, TaggedSource, WaitMode};
use crate::producer::{EndReason, FrameKind};
use crate::sink::{FileSink, IqFormat};
use crate::source::{
    Feed, Packet, PacketStream, check_feed, null_packet, path_opener, queue_depth,
};
use crate::symbols::OffloadArg;
use crate::tx::{TxArgs, describe_speed, ms_per, producer_speed, ratio};
use anyhow::{Context, Result, bail};
use clap::Args;
use isdb_frontend::shape::Shaper;
use isdb_frontend::shape::fixed::{TxDesign, TxParams};
use isdb_iq::sigmf::{Meta, Segment};
use isdb_radio::{C32, IqSink, RadioError, SymbolSink};
use isdb_ts::Pid;
use isdbs_core::combination::Combination;
use isdbs_core::frame::{FRAME_SYMBOLS, FRAMES};
use isdbs_core::tmcc::TmccInfo;
use isdbs_phy::Cplx;
use isdbs_phy::tx::{SUPERFRAME_SYMBOLS, Transmitter};
use isdbt_mux::pcrclock::{packet_pcr, packet_pid};
use serde_json::json;
use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, sync_channel};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};
use tracing::{info, warn};

/// `--system` of `isdb tx` and `isdb loopback`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum TxSystem {
    /// ISDB-T (ARIB STD-B31), terrestrial OFDM.
    Isdbt,
    /// ISDB-S (ITU-R BO.1408-1, ARIB STD-B20), BS and 110° CS: TS packets
    /// per TS_ID → superframes → IQ.
    Isdbs,
}

/// The ISDB-S output sample rate by default: ≥ (1 + 0.35) × 28.86 Mbaud
/// = 38.96 Msps (BO.1408-1 §9), the rate the receive front end is
/// validated at.
pub const DEFAULT_RATE: f64 = 40e6;

/// The analog bandwidth requested from a radio for ISDB-S by default: the
/// largest the Pluto's TX filter takes (0.2–40 MHz); the signal occupies
/// (1 + α)·R = 38.96 MHz.
pub const DEFAULT_BB_FILTER_HZ: u32 = 40_000_000;

/// The TS_ID of the default `--ts`. Not a placeholder: 0x4010 is BS-1
/// TS 0 on air (VERIFICATION.md "ISDB-S frame sync and TMCC": BS1 carries
/// 4010, 4011 and 4012). It stays the default for the synthetic loopback
/// and `--iq` runs; a bench broadcast names its own TS_ID
/// (`tools/isdbs-bench-tx.sh`: 0x4017, not on air).
pub const DEFAULT_TS_ID: u16 = 0x4010;

/// Seconds of one superframe: 319 488 symbols at 28.86 Mbaud (BO.1408-1
/// Appendix 1 Table 3), 11.07 ms.
pub fn superframe_seconds() -> f64 {
    SUPERFRAME_SYMBOLS as f64 / isdbs_phy::frontend::system().symbol_rate_hz
}

/// The arguments of a subcommand given on the command line or in the
/// environment, not by default: what the ISDB-T / ISDB-S option check
/// ([`check_system`]) and the ISDB-S defaults that differ from ISDB-T's
/// (`--bb-filter-hz`) look at.
#[derive(Debug, Clone, Default)]
pub struct Given(Vec<String>);

impl Given {
    /// The ids given in `m` (the subcommand's matches; none without).
    pub fn of(m: Option<&clap::ArgMatches>) -> Self {
        use clap::parser::ValueSource;
        let Some(m) = m else {
            return Self::default();
        };
        Self(
            m.ids()
                .filter(|id| {
                    matches!(
                        m.value_source(id.as_str()),
                        Some(ValueSource::CommandLine | ValueSource::EnvVariable)
                    )
                })
                .map(|id| id.as_str().to_owned())
                .collect(),
        )
    }

    /// True if argument `id` was given.
    pub fn has(&self, id: &str) -> bool {
        self.0.iter().any(|g| g == id)
    }
}

/// `isdb tx` options that only ISDB-T uses (argument ids; the flag is
/// `--` and the id with `-` for `_`).
pub const TX_ISDBT_ONLY: &[&str] = &[
    "mode",
    "gi",
    "partial",
    "layer_a",
    "layer_b",
    "layer_c",
    "a_ts",
    "b_ts",
    "c_ts",
    "mux_ts",
    "a_pids",
    "c_pids",
    "synthetic",
    "mux_config",
    "relay",
    "iq_in",
    "iq_in_format",
    "lead_frames",
    "seek_step",
];

/// `isdb tx` options that only ISDB-S uses.
pub const TX_ISDBS_ONLY: &[&str] = &[
    "combination",
    "ts",
    "rate",
    "lead_superframes",
    "ts_pacing",
    "offload",
    "placed_ts",
];

/// The flag of argument id `id`.
pub fn flag(id: &str) -> String {
    format!("--{}", id.replace('_', "-"))
}

/// Refuse the options of the other system: with `--system isdbs` those in
/// `isdbt_only`, with `isdbt` those in `isdbs_only`, naming the system.
pub fn check_system(
    system: TxSystem,
    given: &Given,
    isdbt_only: &[&str],
    isdbs_only: &[&str],
) -> Result<()> {
    let wrong = match system {
        TxSystem::Isdbs => isdbt_only,
        TxSystem::Isdbt => isdbs_only,
    };
    let bad: Vec<String> = wrong
        .iter()
        .filter(|id| given.has(id))
        .map(|id| flag(id))
        .collect();
    if bad.is_empty() {
        return Ok(());
    }
    let list = bad.join(", ");
    match system {
        TxSystem::Isdbs => bail!(
            "{list}: ISDB-T option(s), not used with --system isdbs (ISDB-S is laid out by --combination WORD:N and --ts ID[:SLOTS][=PATH])"
        ),
        TxSystem::Isdbt => bail!(
            "{list}: ISDB-S option(s), used with --system isdbs only (the default system is ISDB-T)"
        ),
    }
}

/// One `--ts ID[:SLOTS][=PATH]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TsArg {
    /// TS_ID.
    pub ts_id: u16,
    /// Sent slots per frame (all of them for a single TS when absent).
    pub slots: Option<u8>,
    /// Its TS file (`-` = stdin); synthetic packets when absent.
    pub path: Option<String>,
}

/// The ISDB-S options of `isdb tx` and `isdb loopback` (`--system isdbs`).
#[derive(Debug, Clone, Args)]
#[command(next_help_heading = "ISDB-S (--system isdbs)")]
pub struct SatArgs {
    /// Slot layout: a modulation-code combination of BO.1408-1 Table 1 and
    /// its slots per frame, dummy slots included (repeatable, at most 4,
    /// any order; the slots sum to 48, each in whole units). WORD (slots
    /// per unit, sent slots per unit): bpsk1/2 (4, 1), qpsk1/2 (2, 1),
    /// qpsk2/3 (3, 2), qpsk3/4 (4, 3), qpsk5/6 (6, 5), qpsk7/8 (8, 7),
    /// tc8psk2/3 (1, 1). Default tc8psk2/3:48 (the BS layout); the CS
    /// layout is qpsk3/4:48.
    #[arg(long, value_name = "WORD:N", value_parser = parse_combination)]
    pub combination: Vec<(Combination, u8)>,
    /// A TS (repeatable, at most 8, relative TS 0, 1, … in order): its
    /// TS_ID (0x4010, 4010h or decimal), its sent slots per frame (needed
    /// with more than one --ts; they sum to the layout's sent slots) and
    /// its packets: a TS file ("-" = stdin; --feed applies) or, without
    /// =PATH, synthetic packets (with file TSs they end when the files
    /// do). Default: 0x4010, synthetic, every sent slot.
    #[arg(long, value_name = "ID[:SLOTS][=PATH]", value_parser = parse_ts)]
    pub ts: Vec<TsArg>,
    /// Output sample rate (default 40M): at least (1 + 0.35) × 28.86 Mbaud
    /// = 38.961 Msps; a Pluto takes up to 61.44M.
    #[arg(long, value_name = "SPS", value_parser = crate::rxs::parse_rate)]
    pub rate: Option<f64>,
    /// Null superframes before the data (default 1), after the pre-roll
    /// frame: a receiver that starts late or acquires during them still
    /// gets every data superframe.
    #[arg(long, value_name = "N")]
    pub lead_superframes: Option<usize>,
    /// How a TS file's packets are placed in its slots. `auto`: by its PCR
    /// when it carries one (a regular file is probed first, a pipe or
    /// stdin is decided by its first PCR, --feed live by a PCR within its
    /// first second), else `arrival`. `pcr`: by its PCR, an input without
    /// one refused (--feed live: paced from its first PCR, nulls until it
    /// comes). With --feed live a paced TS waits for its packets until
    /// 0.5 s after the time its PCRs give them, then sends what came.
    /// Paced, each packet takes the first free slot at or after the time
    /// its PCRs give it, input null packets are dropped, the slots left
    /// over carry nulls and every PCR is corrected by the time its packet
    /// moved (H.222.0 Annex D.0.4), so the TS keeps its own rate and a
    /// receiver's output has accurate PCRs. `arrival`: every packet, nulls
    /// included, in the next slot as it comes, unmodified (a byte-exact
    /// relay; a TS slower than its slots then plays fast).
    #[arg(long, value_enum, default_value_t = TsPacing::Auto)]
    pub ts_pacing: TsPacing,
}

/// `--ts-pacing`: how the packets of a TS file are placed in its slots.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum TsPacing {
    /// By PCR when the input carries one, else in arrival order.
    Auto,
    /// By PCR; an input without one is refused.
    Pcr,
    /// In arrival order, byte-exact (the slots are filled as the packets
    /// come, input nulls included).
    Arrival,
}

/// The spelling of `c` for `--combination`: its name in lower case
/// without the space (`tc8psk2/3`, `qpsk3/4`, `bpsk1/2`).
pub fn combination_word(c: Combination) -> String {
    c.to_string().to_ascii_lowercase().replace(' ', "")
}

/// Parse `--combination WORD:N` ([`combination_word`], any case, spaces
/// ignored; N = 1..=48 slots).
pub fn parse_combination(s: &str) -> Result<(Combination, u8)> {
    let Some((w, n)) = s.split_once(':') else {
        bail!("--combination {s:?}: want WORD:N, e.g. tc8psk2/3:48");
    };
    let w: String = w
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect::<String>()
        .to_ascii_lowercase();
    let Some(c) = Combination::ALL
        .into_iter()
        .find(|&c| combination_word(c) == w)
    else {
        let words: Vec<String> = Combination::ALL
            .into_iter()
            .rev()
            .map(combination_word)
            .collect();
        bail!(
            "--combination {s:?}: {w:?} is not one of {}",
            words.join(", ")
        );
    };
    let n: u8 = n
        .trim()
        .parse()
        .with_context(|| format!("--combination {s:?}: slots"))?;
    if !(1..=48).contains(&n) {
        bail!("--combination {s:?}: {n} slots, want 1..=48");
    }
    Ok((c, n))
}

/// Parse `--ts ID[:SLOTS][=PATH]`.
pub fn parse_ts(s: &str) -> Result<TsArg> {
    let (spec, path) = match s.split_once('=') {
        Some((_, "")) => bail!("--ts {s:?}: an empty path after '='"),
        Some((spec, p)) => (spec, Some(p.to_owned())),
        None => (s, None),
    };
    let (id, slots) = match spec.split_once(':') {
        Some((id, n)) => {
            let n: u8 = n
                .trim()
                .parse()
                .with_context(|| format!("--ts {s:?}: slots"))?;
            if !(1..=48).contains(&n) {
                bail!("--ts {s:?}: {n} slots per frame, want 1..=48");
            }
            (id, Some(n))
        }
        None => (spec, None),
    };
    Ok(TsArg {
        ts_id: crate::rxs::parse_ts_id(id).with_context(|| format!("--ts {s:?}"))?,
        slots,
        path,
    })
}

/// The ISDB-S layout of the options: the TMCC information and the TSs,
/// defaults filled.
#[derive(Debug, Clone)]
pub struct Layout {
    /// The TMCC information sent ([`TmccInfo::assign`]).
    pub info: TmccInfo,
    /// The TSs, relative TS 0, 1, …, slots filled.
    pub ts: Vec<TsArg>,
}

impl Layout {
    /// The layout `sat` describes.
    pub fn of(sat: &SatArgs) -> Result<Self> {
        let combos = if sat.combination.is_empty() {
            vec![(Combination::Tc8psk2_3, 48)]
        } else {
            sat.combination.clone()
        };
        let mut ts = if sat.ts.is_empty() {
            vec![TsArg {
                ts_id: DEFAULT_TS_ID,
                slots: None,
                path: None,
            }]
        } else {
            sat.ts.clone()
        };
        // sent slots of whole units (a partial unit is refused below)
        let sent: usize = combos
            .iter()
            .map(|&(c, n)| {
                let (unit, valid) = c.slot_unit();
                usize::from(n) / unit * valid
            })
            .sum();
        if let [only] = ts.as_mut_slice()
            && only.slots.is_none()
        {
            only.slots = Some(u8::try_from(sent).context("sent slots")?);
        }
        if let Some(t) = ts.iter().find(|t| t.slots.is_none()) {
            bail!(
                "--ts {:#06x}: give its slots per frame (--ts ID:SLOTS) when there is more than one --ts; the layout sends {sent} slots per frame",
                t.ts_id
            );
        }
        if ts.iter().filter(|t| t.path.as_deref() == Some("-")).count() > 1 {
            bail!("only one --ts can read stdin");
        }
        let pairs: Vec<(u16, u8)> = ts.iter().map(|t| (t.ts_id, t.slots.unwrap_or(0))).collect();
        let info = TmccInfo::assign(&combos, &pairs).with_context(|| {
            format!(
                "ISDB-S layout {} with {}",
                combos
                    .iter()
                    .map(|&(c, n)| format!("{}:{n}", combination_word(c)))
                    .collect::<Vec<_>>()
                    .join(" "),
                pairs
                    .iter()
                    .map(|&(id, n)| format!("TS_ID {id:#06x} {n} slots"))
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        })?;
        Ok(Self { info, ts })
    }

    /// TS_ID of each relative TS.
    pub fn ts_ids(&self) -> Vec<u16> {
        self.ts.iter().map(|t| t.ts_id).collect()
    }

    /// True if no TS reads a file (the transmission never ends by itself).
    pub fn synthetic_only(&self) -> bool {
        self.ts.iter().all(|t| t.path.is_none())
    }

    /// `TC8PSK 2/3 ×48; TS_ID 0x4010 16 slots, …`.
    pub fn describe(&self) -> String {
        format!(
            "{}; {}",
            isdbs_phy::rx::describe_tmcc(&self.info),
            self.ts
                .iter()
                .map(|t| format!("TS_ID {:#06x} {} slots", t.ts_id, t.slots.unwrap_or(0)))
                .collect::<Vec<_>>()
                .join(", ")
        )
    }
}

/// Deterministic test packet `index` of superframe `superframe` of TS
/// `ts_id`: PID 0x0100, continuity counter `cc`, the TS_ID in bytes 4..6,
/// the superframe in 6..14 and the index in 14..18, then a xorshift
/// payload, so every packet of a run is distinct and says where it was
/// sent.
pub fn synthetic_packet(ts_id: u16, superframe: u64, index: u32, cc: u8) -> Packet {
    let mut p = [0u8; 188];
    p[0] = 0x47;
    p[1] = 0x01;
    p[2] = 0x00;
    p[3] = 0x10 | (cc & 15);
    p[4..6].copy_from_slice(&ts_id.to_be_bytes());
    p[6..14].copy_from_slice(&superframe.to_be_bytes());
    p[14..18].copy_from_slice(&index.to_be_bytes());
    let mut x = (superframe.wrapping_mul(0x9e37_79b9_7f4a_7c15)
        ^ (u64::from(index) << 20)
        ^ (u64::from(ts_id) << 48))
        | 1;
    for b in &mut p[18..] {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        *b = x.to_le_bytes()[3];
    }
    p
}

/// How the packets of one relative TS are placed in its slots, decided
/// before the transmission starts ([`plan_inputs`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Placement {
    /// Synthetic packets, or a TS file in arrival order: every packet,
    /// nulls included, in the next slot, unmodified.
    Arrival,
    /// By PCR ([`SlotPacer`]) on the PCRs of this PID, or of the first PID
    /// carrying one when `None`.
    Pcr(Option<Pid>),
    /// A pipe or stdin, or any `--feed live` input: by PCR if a PCR comes
    /// in its first [`SNIFF_PACKETS`] packets (on that PCR's PID; a live
    /// input: and within [`LIVE_SNIFF`]), else in arrival order.
    FirstPcr,
}

/// Packets of a pipe read to find its first PCR ([`Placement::FirstPcr`]):
/// 12 MB, more than 0.1 s (the longest PCR interval, H.222.0 §2.7.2) of
/// any TS up to 985 Mbit/s.
pub const SNIFF_PACKETS: usize = 65_536;

/// Wall time a `--feed live` input is given to show a PCR
/// ([`Placement::FirstPcr`]): ten times the longest PCR interval
/// (H.222.0 §2.7.2), for a writer that starts late.
pub const LIVE_SNIFF: Duration = Duration::from_secs(1);

/// How a paced TS read with `feed` waits for its input: up to a deadline
/// for `--feed live` ([`WaitMode::live`]), else blocking.
fn wait_mode(feed: Feed) -> WaitMode {
    if feed == Feed::Live {
        WaitMode::live()
    } else {
        WaitMode::Block
    }
}

/// A TS's slots as the [`RateTarget`] of the capacity check.
struct TsShare<'a> {
    ts_id: u16,
    path: &'a str,
    slots: usize,
    clock: SlotClock,
}

impl RateTarget for TsShare<'_> {
    fn shares(&self) -> Vec<RateShare> {
        vec![RateShare {
            index: 0,
            cap_bps: self.clock.bps(),
        }]
    }

    fn no_pcr(&self) {
        info!(
            target: "isdb::tx",
            ts_id = self.ts_id,
            probe_mb = PROBE_BYTES >> 20,
            "  TS_ID {:#06x} ({}): no PCR in the first {} MB — arrival order (every packet, nulls included, in the next slot)",
            self.ts_id,
            self.path,
            PROBE_BYTES >> 20
        );
    }

    fn report(&self, s: RateShare, pcr: Pid, r: &InputRates) {
        info!(
            target: "isdb::tx",
            ts_id = self.ts_id,
            pcr_pid = format_args!("{pcr:#06x}"),
            span_s = r.span_s,
            mean_bps = r.mean[s.index],
            peak_1s_bps = r.peak_1s[s.index],
            capacity_bps = s.cap_bps,
            "  TS_ID {:#06x} ({}): input rate (PCR 0x{pcr:04x}, first {:.1} s) mean {:.2} Mbit/s, peak {:.2} Mbit/s (1 s) — capacity {:.2} Mbit/s ({} slots/frame)",
            self.ts_id,
            self.path,
            r.span_s,
            r.mean[s.index] / 1e6,
            r.peak_1s[s.index] / 1e6,
            s.cap_bps / 1e6,
            self.slots
        );
    }

    fn peak_over(&self, s: RateShare, r: &InputRates) {
        warn!(
            target: "isdb::tx",
            ts_id = self.ts_id,
            peak_1s_bps = r.peak_1s[s.index],
            capacity_bps = s.cap_bps,
            "  TS_ID {:#06x}: 1-s peaks ({:.2} Mbit/s) exceed the capacity of its slots; the TS falls behind its PCR schedule during peaks (lag shown in the status lines)",
            self.ts_id,
            r.peak_1s[s.index] / 1e6
        );
    }

    fn over(&self, s: RateShare, r: &InputRates) -> String {
        // every sent slot carries one packet per frame, whatever its
        // modulation: the TS's rate per slot
        let per_slot = s.cap_bps / self.slots as f64;
        let need = (r.mean[s.index] / per_slot).ceil();
        format!(
            "TS_ID {:#06x} ({}): the input needs {:.2} Mbit/s (mean over {:.1} s) but its {} slots per frame carry {:.2} Mbit/s; it needs {need} sent slots per frame ({:.3} Mbit/s each)",
            self.ts_id,
            self.path,
            r.mean[s.index] / 1e6,
            r.span_s,
            self.slots,
            s.cap_bps / 1e6,
            per_slot / 1e6
        )
    }

    fn refusal(&self) -> String {
        "the file would go on air slower than real time; give the TS more slots (--ts ID:SLOTS), or pass --allow-overrate to send it anyway (it then lags its PCR schedule)".into()
    }
}

/// The slot clock of relative TS `t` (its sent slots per frame).
fn slot_clock(t: &TsArg) -> Result<SlotClock> {
    let n = usize::from(t.slots.unwrap_or(0));
    SlotClock::isdbs(n).with_context(|| format!("TS_ID {:#06x}: {n} slots per frame", t.ts_id))
}

/// Decide how each TS of `layout` is placed (pcr-pacing.md §7.3), and say
/// so: a regular file read with `--feed file` or `loop` is probed
/// ([`probe_rates`], its first [`PROBE_SECONDS`] s or [`PROBE_BYTES`]):
/// with a PCR it is paced, its mean rate above its slots' capacity refused
/// unless `allow_overrate` and a 1-s peak above it warned about; a pipe or
/// stdin with those feeds is decided by its first PCR, and so is any input
/// read with `--feed live` (within [`LIVE_SNIFF`]; it is then paced with a
/// deadline, [`WaitMode::Live`]). `TsPacing::Pcr` refuses a file without
/// PCR and paces a live input from its first PCR, whenever it comes;
/// `TsPacing::Arrival` places every TS in arrival order.
pub fn plan_inputs(
    layout: &Layout,
    feed: Feed,
    pacing: TsPacing,
    allow_overrate: bool,
) -> Result<Vec<Placement>> {
    let live_wait = format!(
        "each superframe waits for its packets until {:.1} s after the time their PCRs give them, the first PCR on air {:.1} s after it arrived; later packets are placed late",
        (crate::pace::LIVE_LATENCY + crate::pace::LIVE_GRACE).as_secs_f64(),
        crate::pace::LIVE_LATENCY.as_secs_f64()
    );
    let mut plan = Vec::with_capacity(layout.ts.len());
    for t in &layout.ts {
        let Some(path) = t.path.as_deref() else {
            plan.push(Placement::Arrival);
            continue;
        };
        let id = t.ts_id;
        let p = match (pacing, feed) {
            (TsPacing::Arrival, _) => {
                info!(
                    target: "isdb::tx",
                    ts_id = id,
                    "  TS_ID {id:#06x} ({path}): arrival order (--ts-pacing arrival: every packet, nulls included, in the next slot, unmodified)"
                );
                Placement::Arrival
            }
            (TsPacing::Pcr, Feed::Live) => {
                info!(
                    target: "isdb::tx",
                    ts_id = id,
                    "  TS_ID {id:#06x} ({path}): --feed live, paced by PCR (--ts-pacing pcr; nulls until its first PCR): {live_wait}"
                );
                Placement::Pcr(None)
            }
            (TsPacing::Auto, Feed::Live) => {
                info!(
                    target: "isdb::tx",
                    ts_id = id,
                    "  TS_ID {id:#06x} ({path}): --feed live: paced by PCR if one comes within {} s and its first {SNIFF_PACKETS} packets ({live_wait}), else arrival order (packets as they come, nulls padding when none is queued)",
                    LIVE_SNIFF.as_secs_f64()
                );
                Placement::FirstPcr
            }
            (_, Feed::File | Feed::Loop) if is_regular_file(path) => {
                let clock = slot_clock(t)?;
                let r = probe_rates(path, &|_| 0, PROBE_SECONDS, PROBE_BYTES)
                    .with_context(|| format!("probe the rate of {path}"))?;
                let share = TsShare {
                    ts_id: id,
                    path,
                    slots: usize::from(t.slots.unwrap_or(0)),
                    clock,
                };
                if check_rates(&r, &share, allow_overrate)? {
                    info!(
                        target: "isdb::tx",
                        ts_id = id,
                        "  TS_ID {id:#06x} ({path}): paced by PCR{}: each packet in the first free slot at or after its PCR time, input nulls dropped, PCRs corrected by the time their packet moved{}",
                        r.pcr_pid.map_or(String::new(), |p| format!(" (PID {p:#06x})")),
                        if feed == Feed::Loop {
                            "; looped seamlessly: each pass's PCR, PTS/DTS moved on by the loop period and its continuity counters renumbered"
                        } else {
                            ""
                        }
                    );
                    Placement::Pcr(r.pcr_pid)
                } else if pacing == TsPacing::Pcr {
                    bail!(
                        "--ts-pacing pcr: TS_ID {id:#06x} ({path}) has no PCR in its first {} MB",
                        PROBE_BYTES >> 20
                    );
                } else {
                    Placement::Arrival
                }
            }
            (TsPacing::Pcr, Feed::File | Feed::Loop) => {
                info!(
                    target: "isdb::tx",
                    ts_id = id,
                    "  TS_ID {id:#06x} ({path}): a pipe, paced by PCR (--ts-pacing pcr; no rate probe)"
                );
                Placement::Pcr(None)
            }
            (TsPacing::Auto, Feed::File | Feed::Loop) => {
                info!(
                    target: "isdb::tx",
                    ts_id = id,
                    "  TS_ID {id:#06x} ({path}): a pipe (no rate probe): paced by PCR if one comes in its first {SNIFF_PACKETS} packets, else arrival order"
                );
                Placement::FirstPcr
            }
        };
        plan.push(p);
    }
    Ok(plan)
}

/// A TS file's packets: those read ahead to find its first PCR, then its
/// stream; each with its loop pass ([`PacketStream::next_tagged`]) and
/// arrival ([`PacketStream::arrival`]).
struct Feeder {
    head: VecDeque<(Packet, u32, Option<Instant>)>,
    stream: PacketStream,
    /// Arrival of the packet last taken.
    arrived: Option<Instant>,
}

impl Feeder {
    fn new(stream: PacketStream) -> Self {
        Self {
            head: VecDeque::new(),
            stream,
            arrived: None,
        }
    }

    /// Read one packet ahead into the head (the first-PCR sniff); a live
    /// input waits at most `timeout`.
    fn read_ahead(&mut self, stop: &AtomicBool, timeout: Option<Duration>) -> Option<Packet> {
        let (p, pass) = match timeout {
            Some(t) => self.stream.next_tagged_timeout(stop, t),
            None => self.stream.next_tagged(stop),
        }?;
        self.head.push_back((p, pass, self.stream.arrival()));
        Some(p)
    }

    /// The first packet of the head.
    fn take_head(&mut self) -> Option<(Packet, u32)> {
        let (p, pass, at) = self.head.pop_front()?;
        self.arrived = at;
        Some((p, pass))
    }

    /// A packet taken from the stream (its arrival noted).
    fn note_stream(&mut self, q: Option<(Packet, u32)>) -> Option<(Packet, u32)> {
        if q.is_some() {
            self.arrived = self.stream.arrival();
        }
        q
    }

    /// The next packet, as [`PacketStream::next`].
    fn next_packet(&mut self, stop: &AtomicBool) -> Option<Packet> {
        match self.take_head() {
            Some((p, _)) => Some(p),
            None => self.stream.next(stop),
        }
    }

    fn ended(&self) -> bool {
        self.head.is_empty() && self.stream.ended()
    }
}

impl TaggedSource for Feeder {
    fn next(&mut self, stop: &AtomicBool) -> Option<(Packet, u32)> {
        match self.take_head() {
            Some(q) => Some(q),
            None => {
                let q = self.stream.next_tagged(stop);
                self.note_stream(q)
            }
        }
    }

    fn ended(&self) -> bool {
        Feeder::ended(self)
    }

    fn next_within(&mut self, stop: &AtomicBool, timeout: Duration) -> Option<(Packet, u32)> {
        match self.take_head() {
            Some(q) => Some(q),
            None => {
                let q = self.stream.next_tagged_timeout(stop, timeout);
                self.note_stream(q)
            }
        }
    }

    fn arrival(&self) -> Option<Instant> {
        self.arrived
    }
}

/// Where a TS's packets come from.
enum Source {
    /// [`synthetic_packet`]s.
    Synthetic,
    /// A TS file (or FIFO, stdin) read by its own thread, in arrival order.
    File(Feeder),
    /// A TS file placed by its PCR.
    Paced {
        stream: Feeder,
        pacer: Box<SlotPacer>,
    },
    /// A pipe whose first PCR decides between `Paced` and `File` at the
    /// first [`TsInputs::take`]. `live`: a `--feed live` input, given this
    /// much wall time to show its PCR ([`LIVE_SNIFF`]) and paced with
    /// [`WaitMode::Live`].
    FirstPcr {
        stream: Feeder,
        slots: SlotClock,
        live: Option<Duration>,
    },
}

impl Source {
    fn feeder(&self) -> Option<&Feeder> {
        match self {
            Self::Synthetic => None,
            Self::File(f) | Self::Paced { stream: f, .. } | Self::FirstPcr { stream: f, .. } => {
                Some(f)
            }
        }
    }

    /// Every input packet handed out (`None` for synthetic packets).
    fn ended(&self) -> Option<bool> {
        match self {
            Self::Synthetic => None,
            Self::File(f) | Self::FirstPcr { stream: f, .. } => Some(f.ended()),
            Self::Paced { pacer, .. } => Some(pacer.drained()),
        }
    }

    /// Resolve a `FirstPcr`: read up to [`SNIFF_PACKETS`] packets (kept
    /// for the TS; a live input: for at most its wall time) until one
    /// carries a PCR, then pace on its PID, or keep the arrival order
    /// without one.
    fn resolve(&mut self, ts_id: u16, stop: &AtomicBool) {
        let Self::FirstPcr { stream, live, .. } = self else {
            return;
        };
        let until = live.and_then(|d| Instant::now().checked_add(d));
        let mut pcr = None;
        while pcr.is_none() && stream.head.len() < SNIFF_PACKETS {
            let left = until.map(|u| u.saturating_duration_since(Instant::now()));
            let Some(p) = stream.read_ahead(stop, left) else {
                break; // ended, stopped or out of time
            };
            pcr = packet_pcr(&p).map(|_| packet_pid(&p));
        }
        let n = stream.head.len();
        let Self::FirstPcr {
            stream,
            slots,
            live,
        } = std::mem::replace(self, Self::Synthetic)
        else {
            unreachable!("matched above");
        };
        *self = match pcr {
            Some(pid) => {
                let mode = if live.is_some() {
                    WaitMode::live()
                } else {
                    WaitMode::Block
                };
                info!(
                    target: "isdb::tx",
                    ts_id,
                    "  TS_ID {ts_id:#06x}: first PCR (PID {pid:#06x}) in packet {n}: paced by PCR{}",
                    if live.is_some() { " (live)" } else { "" }
                );
                Self::Paced {
                    stream,
                    pacer: Box::new(SlotPacer::new(slots, Some(pid), mode)),
                }
            }
            None => {
                if !stop.load(Ordering::Relaxed) {
                    let within = live.map_or(String::new(), |d| {
                        format!(" (read within {} s)", d.as_secs_f64())
                    });
                    info!(
                        target: "isdb::tx",
                        ts_id,
                        "  TS_ID {ts_id:#06x}: no PCR in the first {n} packets{within}: arrival order"
                    );
                }
                Self::File(stream)
            }
        };
    }
}

/// The transport_stream_id of the PAT section that starts in `p` (PID
/// 0x0000, payload_unit_start_indicator '1', table_id 0x00 after the
/// pointer_field; H.222.0 §2.4.4.2, §2.4.4.3, Table 2-30), else `None`.
fn pat_ts_id(p: &Packet) -> Option<u16> {
    let v = isdb_ts::packet::View::parse(p).ok()?;
    if v.header.pid != Pid::PAT || !v.header.payload_unit_start_indicator {
        return None;
    }
    let data = v.payload?;
    match data.get(1 + usize::from(*data.first()?)..)? {
        [0x00, _, _, hi, lo, ..] => Some(u16::from_be_bytes([*hi, *lo])),
        _ => None,
    }
}

/// Compare the first PAT among `packets` (the input of the TS `ts_id`,
/// read from `name`) with the TS_ID and warn when they differ: a receiver
/// selects the TS by the TMCC's TS_ID and then reads the PAT's, which on
/// air are equal (VERIFICATION.md "ISDB-S main signal to TS"). True once
/// a PAT has been seen, so that the caller warns at most once per TS.
fn check_pat(ts_id: u16, name: &str, packets: &[Packet]) -> bool {
    let Some(pat) = packets.iter().find_map(pat_ts_id) else {
        return false;
    };
    if pat != ts_id {
        warn!(
            target: "isdb::tx",
            ts_id,
            pat_ts_id = pat,
            "TS_ID {ts_id:#06x} <- {name}: its PAT says transport_stream_id {pat:#06x}, the TMCC {ts_id:#06x}; a receiver tuned to {ts_id:#06x} finds a TS that names another (give --ts {pat:#06x}=…, or multiplex it with transport_stream_id = {ts_id:#06x})"
        );
    }
    true
}

/// One superframe's packets: exactly the count of each relative TS.
#[derive(Debug, Clone)]
pub struct SfPackets {
    /// Per relative TS.
    pub ts: Vec<Vec<Packet>>,
    /// How many of them came from the input (the rest are null padding).
    pub real: Vec<usize>,
}

/// The inputs of the TSs.
pub struct TsInputs {
    ts_ids: Vec<u16>,
    sources: Vec<Source>,
    names: Vec<String>,
    feed: Feed,
    /// Superframes taken so far (the synthetic packets' superframe field).
    superframe: u64,
    /// Packets per TS so far (the synthetic continuity counters).
    sent: Vec<u64>,
    /// Per TS: its first PAT has been compared with its TS_ID
    /// ([`check_pat`]).
    pat_checked: Vec<bool>,
}

impl TsInputs {
    /// Open the inputs of `layout` with `feed`, placed as `plan` says (one
    /// entry per TS, [`plan_inputs`]); `per_superframe` packets of each
    /// relative TS size the read-ahead queues.
    pub fn open(
        layout: &Layout,
        feed: Feed,
        per_superframe: &[usize],
        plan: &[Placement],
    ) -> Result<Self> {
        let mut sources = Vec::new();
        let mut names = Vec::new();
        for ((t, &n), &place) in layout.ts.iter().zip(per_superframe).zip(plan) {
            match &t.path {
                Some(p) => {
                    check_feed(p, feed)?;
                    let stream = Feeder::new(PacketStream::spawn(
                        p,
                        path_opener(p),
                        feed,
                        queue_depth(n, superframe_seconds()),
                    )?);
                    sources.push(match place {
                        Placement::Arrival => Source::File(stream),
                        Placement::Pcr(pid) => Source::Paced {
                            stream,
                            pacer: Box::new(SlotPacer::new(slot_clock(t)?, pid, wait_mode(feed))),
                        },
                        Placement::FirstPcr => Source::FirstPcr {
                            stream,
                            slots: slot_clock(t)?,
                            live: (feed == Feed::Live).then_some(LIVE_SNIFF),
                        },
                    });
                    names.push(p.clone());
                }
                None => {
                    sources.push(Source::Synthetic);
                    names.push("synthetic".into());
                }
            }
        }
        Ok(Self {
            ts_ids: layout.ts_ids(),
            sent: vec![0; sources.len()],
            pat_checked: vec![false; sources.len()],
            sources,
            names,
            feed,
            superframe: 0,
        })
    }

    /// In-memory streams for the TSs (tests): `None` = synthetic; paced
    /// streams on 48 slots per frame.
    #[cfg(test)]
    fn from_streams(
        ts_ids: Vec<u16>,
        streams: Vec<Option<(PacketStream, Placement)>>,
        feed: Feed,
    ) -> Self {
        let names = streams
            .iter()
            .map(|s| s.as_ref().map_or("synthetic".into(), |s| s.0.name.clone()))
            .collect();
        let slots = SlotClock::isdbs(48).expect("48 slots");
        let sources: Vec<Source> = streams
            .into_iter()
            .map(|s| match s {
                None => Source::Synthetic,
                Some((s, place)) => {
                    let stream = Feeder::new(s);
                    match place {
                        Placement::Arrival => Source::File(stream),
                        Placement::Pcr(pid) => Source::Paced {
                            stream,
                            pacer: Box::new(SlotPacer::new(slots, pid, wait_mode(feed))),
                        },
                        Placement::FirstPcr => Source::FirstPcr {
                            stream,
                            slots,
                            live: (feed == Feed::Live).then_some(LIVE_SNIFF),
                        },
                    }
                }
            })
            .collect();
        Self {
            ts_ids,
            sent: vec![0; sources.len()],
            pat_checked: vec![false; sources.len()],
            sources,
            names,
            feed,
            superframe: 0,
        }
    }

    /// Give the live inputs still looking for their first PCR `d` of wall
    /// time instead of [`LIVE_SNIFF`] (tests).
    #[cfg(test)]
    fn set_live_sniff(&mut self, d: Duration) {
        for s in &mut self.sources {
            if let Source::FirstPcr { live: Some(l), .. } = s {
                *l = d;
            }
        }
    }

    /// Packets for one superframe, `counts[r]` of relative TS r: synthetic
    /// packets; a file TS's next packets in arrival order, padded with
    /// null packets; or a paced TS's next `counts[r]` slots
    /// ([`SlotPacer::fill`]). `None` when stopped, or once every file TS
    /// has ended (a paced one: every packet handed out) and none gave a
    /// packet (synthetic TSs then end with them; with synthetic TSs only,
    /// never).
    pub fn take(&mut self, counts: &[usize], stop: &AtomicBool) -> Option<SfPackets> {
        let sf = self.superframe;
        let mut ts = Vec::with_capacity(counts.len());
        let mut real = Vec::with_capacity(counts.len());
        let mut file_real = 0;
        for (r, src) in self.sources.iter_mut().enumerate() {
            let n = counts[r];
            src.resolve(self.ts_ids[r], stop);
            let (v, k) = match src {
                Source::Synthetic => {
                    let mut v = Vec::with_capacity(n);
                    for i in 0..n {
                        // a TS takes at most 48 × 8 packets per superframe
                        let index = u32::try_from(i).unwrap_or(u32::MAX);
                        let cc = u8::try_from(self.sent[r] & 15).unwrap_or(0);
                        v.push(synthetic_packet(self.ts_ids[r], sf, index, cc));
                        self.sent[r] += 1;
                    }
                    (v, n)
                }
                // `resolve` leaves no FirstPcr
                Source::File(s) | Source::FirstPcr { stream: s, .. } => {
                    let mut v = Vec::with_capacity(n);
                    while v.len() < n {
                        match s.next_packet(stop) {
                            Some(p) => v.push(p),
                            None => break,
                        }
                    }
                    let k = v.len();
                    file_real += k;
                    v.resize(n, null_packet());
                    (v, k)
                }
                Source::Paced { stream, pacer } => {
                    let (v, k) = pacer.fill(n, stream, stop);
                    file_real += k;
                    (v, k)
                }
            };
            if !self.pat_checked[r] && k > 0 {
                self.pat_checked[r] = check_pat(self.ts_ids[r], &self.names[r], &v);
            }
            real.push(k);
            ts.push(v);
        }
        let ended: Vec<bool> = self.sources.iter().filter_map(Source::ended).collect();
        let all_ended = !ended.is_empty() && ended.iter().all(|&e| e);
        if (all_ended && file_real == 0) || stop.load(Ordering::Relaxed) {
            return None;
        }
        self.superframe += 1;
        Some(SfPackets { ts, real })
    }

    /// `TS_ID 0x4010 <- a.ts (paced by PCR), TS_ID 0x4011 <- synthetic
    /// (feed File)`; a live pacer is `(paced by PCR, live)`.
    pub fn describe(&self) -> String {
        let v: Vec<String> = self
            .ts_ids
            .iter()
            .zip(&self.names)
            .zip(&self.sources)
            .map(|((id, n), s)| {
                let how = match s {
                    Source::Synthetic | Source::File(_) => "",
                    Source::Paced { pacer, .. } if pacer.is_live() => " (paced by PCR, live)",
                    Source::Paced { .. } => " (paced by PCR)",
                    Source::FirstPcr { .. } => " (paced by PCR if one comes)",
                };
                format!("TS_ID {id:#06x} <- {n}{how}")
            })
            .collect();
        format!("{} (feed {:?})", v.join(", "), self.feed)
    }

    /// Reader counters and the pacers' status for the status lines (may
    /// be empty).
    pub fn status(&self) -> String {
        let (mut skipped, mut rewinds) = (0, 0);
        for f in self.sources.iter().filter_map(Source::feeder) {
            skipped += f.stream.stats.skipped_bytes.load(Ordering::Relaxed);
            rewinds += f.stream.stats.rewinds.load(Ordering::Relaxed);
        }
        let mut s = String::new();
        if skipped > 0 {
            s += &format!(" resync-skipped {skipped} B");
        }
        if rewinds > 0 {
            s += &format!(" rewinds {rewinds}");
        }
        for (id, src) in self.ts_ids.iter().zip(&self.sources) {
            if let Source::Paced { pacer, .. } = src {
                s += &pacer.status().replacen(" | ", &format!(" | {id:#06x} "), 1);
            }
        }
        s
    }
}

/// A produced superframe's signal: IQ samples or symbol codes.
#[derive(Debug, Clone, PartialEq)]
pub enum Payload {
    /// IQ samples at the output rate, unit RMS (442 810 or 442 811 per
    /// superframe at 40 Msps).
    Samples(Vec<C32>),
    /// One TX boundary code per symbol (`isdb_frontend::txformat`: 0 the
    /// zero symbol, 8 + k the point e^{jkπ/4}), 319 488 per superframe.
    Codes(Vec<u8>),
}

impl Payload {
    /// Its air time: samples at `fs`, or symbols at the ISDB-S symbol
    /// rate.
    pub fn seconds(&self, fs: f64) -> f64 {
        match self {
            Self::Samples(x) => x.len() as f64 / fs,
            Self::Codes(c) => c.len() as f64 / isdbs_phy::frontend::system().symbol_rate_hz,
        }
    }

    /// The samples; an error for codes.
    pub fn into_samples(self) -> Result<Vec<C32>> {
        match self {
            Self::Samples(x) => Ok(x),
            Self::Codes(_) => bail!("the producer made symbol codes, not samples"),
        }
    }
}

/// What the producer makes of the transmitter's symbols, and what it has
/// made so far for the next item.
pub struct Modulate(Mod);

enum Mod {
    /// IQ samples, shaped on the CPU.
    Shaped { shaper: Shaper, out: Vec<C32> },
    /// The symbols as codes.
    Codes { out: Vec<u8> },
}

impl Modulate {
    /// IQ samples at the shaper's rate ([`Shaper`], BO.1408-1 Annex 1 §9).
    pub fn shaped(shaper: Shaper) -> Self {
        Self(Mod::Shaped {
            shaper,
            out: Vec::new(),
        })
    }

    /// TX boundary codes for a radio's FPGA transmit block (no shaping
    /// here).
    pub fn codes() -> Self {
        Self(Mod::Codes { out: Vec::new() })
    }

    /// Append the symbols `range` of the transmitter's next null
    /// superframe.
    fn null(&mut self, tx: &mut Transmitter, range: std::ops::Range<usize>) {
        match &mut self.0 {
            Mod::Shaped { shaper, out } => {
                let s = tx.null_superframe();
                shaper.push(&to_c32(&s.symbols[range]), out);
            }
            Mod::Codes { out } => {
                let s = tx.null_superframe_codes();
                out.extend_from_slice(&s.codes[range]);
            }
        }
    }

    /// Append the superframe of `ts`'s packets.
    fn data(&mut self, tx: &mut Transmitter, ts: &[&[Packet]]) -> Result<()> {
        match &mut self.0 {
            Mod::Shaped { shaper, out } => {
                let s = tx.superframe(ts)?;
                shaper.push(&to_c32(&s.symbols), out);
            }
            Mod::Codes { out } => {
                let s = tx.superframe_codes(ts)?;
                out.extend_from_slice(&s.codes);
            }
        }
        Ok(())
    }

    /// The end of the stream: the shaper's tail (codes need none: the
    /// block's filter drains on the zero symbols after them).
    fn finish(&mut self) {
        if let Mod::Shaped { shaper, out } = &mut self.0 {
            shaper.finish(out);
        }
    }

    /// What was made since the last take.
    fn take(&mut self) -> Payload {
        match &mut self.0 {
            Mod::Shaped { out, .. } => Payload::Samples(std::mem::take(out)),
            Mod::Codes { out } => Payload::Codes(std::mem::take(out)),
        }
    }
}

/// One produced superframe (or the flush: a null frame, and the shaper's
/// tail for samples).
pub struct ProducedSf {
    /// Role.
    pub kind: FrameKind,
    /// Its signal.
    pub payload: Payload,
    /// Input packets per relative TS.
    pub real: Vec<usize>,
    /// Null padding per relative TS.
    pub nulls: Vec<usize>,
    /// The packets per relative TS, when recording (for the loopback's
    /// verifier); never for the flush, which is no whole superframe.
    pub packets: Option<Arc<[Vec<Packet>]>>,
    /// Input counters as of this superframe.
    pub input_status: String,
    /// Time spent assembling its packets from the input.
    pub input_time: Duration,
    /// Time spent in the transmitter (and the shaper).
    pub modulate_time: Duration,
}

/// `--placed-ts TEMPLATE`: one file per relative TS that receives the
/// packets of every data superframe as they were placed in the TS's slots
/// (null packets included) and handed to the transmitter: the TS a
/// receiver outputs for that TS_ID.
#[derive(Debug)]
pub struct PlacedTs {
    files: Vec<(String, std::io::BufWriter<std::fs::File>)>,
}

impl PlacedTs {
    /// Create the files of `template` for `ts_ids`: `{ts_id}` is replaced
    /// by the TS_ID in 4 hex digits (as `isdb rx --out` does); a template
    /// without it names one file and takes only one TS.
    pub fn create(template: &str, ts_ids: &[u16]) -> Result<Self> {
        if ts_ids.len() > 1 && !template.contains("{ts_id}") {
            bail!(
                "--placed-ts {template:?} names one file for {} TSs: put {{ts_id}} in the path",
                ts_ids.len()
            );
        }
        let mut files = Vec::with_capacity(ts_ids.len());
        for id in ts_ids {
            let path = template.replace("{ts_id}", &format!("{id:04x}"));
            let f = std::fs::File::create(&path).with_context(|| format!("create {path}"))?;
            files.push((path, std::io::BufWriter::new(f)));
        }
        Ok(Self { files })
    }

    /// The paths written.
    pub fn paths(&self) -> Vec<String> {
        self.files.iter().map(|(p, _)| p.clone()).collect()
    }

    /// Append one superframe's packets, per relative TS.
    fn write(&mut self, ts: &[Vec<Packet>]) -> Result<()> {
        use std::io::Write;
        for ((path, f), packets) in self.files.iter_mut().zip(ts) {
            for p in packets {
                f.write_all(p).with_context(|| format!("write {path}"))?;
            }
        }
        Ok(())
    }

    /// Flush the files.
    fn finish(&mut self) -> Result<()> {
        use std::io::Write;
        for (path, f) in &mut self.files {
            f.flush().with_context(|| format!("write {path}"))?;
        }
        Ok(())
    }
}

/// Producer settings.
#[derive(Debug)]
pub struct SatConfig {
    /// Null superframes before the data.
    pub lead: usize,
    /// Append the flush (a null frame and the shaper's tail).
    pub flush: bool,
    /// Bound on the lead and data superframes; `None` = unbounded.
    pub max_superframes: Option<u64>,
    /// Pace production to real time.
    pub pace: bool,
    /// Attach each superframe's packets ([`ProducedSf::packets`]).
    pub record: bool,
    /// Superframes queued ahead of the sink.
    pub depth: usize,
    /// Write each data superframe's packets here (`--placed-ts`).
    pub placed: Option<PlacedTs>,
}

/// Superframes covering `seconds` of air time (rounded up, at least 1).
pub fn superframes_for_seconds(seconds: f64) -> u64 {
    // f64 -> u64 saturates: a superframe count, at least 1
    #[allow(clippy::cast_possible_truncation)]
    let n = (seconds / superframe_seconds()).ceil().max(1.0) as u64;
    n
}

/// Symbols to the shaper's `f32` input.
fn to_c32(z: &[Cplx]) -> Vec<C32> {
    // unit-modulus constellation points, rounded to f32 on purpose
    #[allow(clippy::cast_possible_truncation)]
    let v = z
        .iter()
        .map(|c| C32::new(c.re as f32, c.im as f32))
        .collect();
    v
}

/// Start the producer thread over `tx` and `m` (fresh, built by the
/// caller so that their errors come before any output). Fails only when
/// the OS refuses the thread.
pub fn spawn(
    mut cfg: SatConfig,
    mut tx: Transmitter,
    mut m: Modulate,
    mut input: TsInputs,
    stop: Arc<AtomicBool>,
) -> Result<(Receiver<ProducedSf>, JoinHandle<Result<EndReason>>)> {
    let (out, rx) = sync_channel::<ProducedSf>(cfg.depth.max(1));
    let h = std::thread::Builder::new()
        .name("tx-producer".into())
        .spawn(move || -> Result<EndReason> {
            let counts: Vec<usize> = (0..tx.ts_count())
                .map(|r| tx.packets_per_superframe(r))
                .collect();
            let sf_s = superframe_seconds();
            let data_limit = cfg
                .max_superframes
                .map(|t| t.saturating_sub(cfg.lead as u64));
            let t0 = Instant::now();
            let pace = |index: u64| {
                if cfg.pace {
                    let due = t0 + Duration::from_secs_f64(index as f64 * sf_s);
                    let now = Instant::now();
                    if due > now {
                        std::thread::sleep(due - now);
                    }
                }
            };
            let ended = |stop: &AtomicBool| {
                if stop.load(Ordering::Relaxed) {
                    EndReason::Stopped
                } else {
                    EndReason::SinkClosed
                }
            };
            // The pre-roll: the last frame of a null superframe, ahead of the
            // first item. A receiver's loops settle during its first
            // symbols, which would otherwise be the start of the first
            // superframe (whose first slots, spread over its 8 frames by
            // the interleaver, would not decode). The inner coder runs on
            // from it into the next superframe, as on air.
            m.null(&mut tx, (FRAMES - 1) * FRAME_SYMBOLS..SUPERFRAME_SYMBOLS);
            let mut index = 0u64;
            for _ in 0..cfg.lead {
                if stop.load(Ordering::Relaxed) {
                    return Ok(EndReason::Stopped);
                }
                pace(index);
                let tm = Instant::now();
                m.null(&mut tx, 0..SUPERFRAME_SYMBOLS);
                let f = ProducedSf {
                    kind: FrameKind::Lead,
                    payload: m.take(),
                    real: vec![0; counts.len()],
                    nulls: vec![0; counts.len()],
                    packets: cfg
                        .record
                        .then(|| counts.iter().map(|&n| vec![null_packet(); n]).collect()),
                    input_status: String::new(),
                    input_time: Duration::ZERO,
                    modulate_time: tm.elapsed(),
                };
                if out.send(f).is_err() {
                    return Ok(ended(&stop));
                }
                index += 1;
            }
            let mut data = 0u64;
            let reason = loop {
                if data_limit.is_some_and(|l| data >= l) {
                    break EndReason::Limit;
                }
                if stop.load(Ordering::Relaxed) {
                    return Ok(EndReason::Stopped);
                }
                pace(index);
                let ti = Instant::now();
                let Some(p) = input.take(&counts, &stop) else {
                    if stop.load(Ordering::Relaxed) {
                        return Ok(EndReason::Stopped);
                    }
                    break EndReason::InputEnded;
                };
                let it = ti.elapsed();
                let tm = Instant::now();
                let refs: Vec<&[Packet]> = p.ts.iter().map(Vec::as_slice).collect();
                m.data(&mut tx, &refs)?;
                if let Some(w) = cfg.placed.as_mut() {
                    w.write(&p.ts)?;
                }
                let payload = m.take();
                let mt = tm.elapsed();
                let f = ProducedSf {
                    kind: FrameKind::Data,
                    payload,
                    nulls: counts.iter().zip(&p.real).map(|(n, r)| n - r).collect(),
                    real: p.real,
                    packets: cfg.record.then(|| Arc::from(p.ts)),
                    input_status: input.status(),
                    input_time: it,
                    modulate_time: mt,
                };
                if out.send(f).is_err() {
                    return Ok(ended(&stop));
                }
                index += 1;
                data += 1;
            };
            if let Some(w) = cfg.placed.as_mut() {
                w.finish()?;
            }
            if cfg.flush {
                if stop.load(Ordering::Relaxed) {
                    return Ok(EndReason::Stopped);
                }
                pace(index);
                let tm = Instant::now();
                m.null(&mut tx, 0..FRAME_SYMBOLS);
                m.finish();
                let f = ProducedSf {
                    kind: FrameKind::Flush,
                    payload: m.take(),
                    real: vec![0; counts.len()],
                    nulls: vec![0; counts.len()],
                    packets: None,
                    input_status: String::new(),
                    input_time: Duration::ZERO,
                    modulate_time: tm.elapsed(),
                };
                if out.send(f).is_err() {
                    return Ok(ended(&stop));
                }
            }
            Ok(reason)
        })
        .context("spawn the producer thread")?;
    Ok((rx, h))
}

/// Where `tx --system isdbs` sends its superframes.
pub enum TxSink {
    /// IQ samples: an `--iq` file or a radio.
    Iq(Box<dyn IqSink>),
    /// Symbol codes: a radio's FPGA transmit block.
    Codes(Box<dyn SymbolSink>),
}

impl TxSink {
    /// The producer's modulation for this sink: `shaper`'s samples, or the
    /// codes.
    pub fn modulate(&self, shaper: Shaper) -> Modulate {
        match self {
            Self::Iq(_) => Modulate::shaped(shaper),
            Self::Codes(_) => Modulate::codes(),
        }
    }

    fn write(&mut self, p: &Payload, stop: &AtomicBool) -> Result<()> {
        match (self, p) {
            (Self::Iq(s), Payload::Samples(x)) => s.write(x, stop)?,
            (Self::Codes(s), Payload::Codes(c)) => s.write(c, stop)?,
            (Self::Iq(_), Payload::Codes(_)) => bail!("symbol codes for a sink of IQ samples"),
            (Self::Codes(_), Payload::Samples(_)) => {
                bail!("IQ samples for a sink of symbol codes")
            }
        }
        Ok(())
    }

    fn status(&mut self) -> String {
        match self {
            Self::Iq(s) => s.status(),
            Self::Codes(s) => s.status(),
        }
    }

    fn finish(&mut self, aborted: bool) -> isdb_radio::Result<String> {
        match self {
            Self::Iq(s) => s.finish(aborted),
            Self::Codes(s) => s.finish(aborted),
        }
    }

    fn realtime(&self) -> bool {
        match self {
            Self::Iq(s) => s.realtime(),
            Self::Codes(s) => s.realtime(),
        }
    }
}

/// Outcome of an ISDB-S transmission.
#[derive(Debug, Clone, Default)]
pub struct SatReport {
    /// Superframes handed to the sink (the flush counted as one).
    pub items: u64,
    /// Lead superframes.
    pub lead: u64,
    /// Data superframes.
    pub data: u64,
    /// Flush items (0 or 1).
    pub flush: u64,
    /// IQ samples handed to the sink.
    pub samples: u64,
    /// Symbol codes handed to the sink.
    pub codes: u64,
    /// Air time handed to the sink, seconds.
    pub on_air_s: f64,
    /// Input packets per relative TS.
    pub real: Vec<u64>,
    /// Null padding per relative TS in data superframes.
    pub nulls: Vec<u64>,
    /// Why the producer ended (None if the sink failed first).
    pub end: Option<EndReason>,
    /// Sink summary.
    pub sink: String,
    /// The input's last status ([`TsInputs::status`]: reader counters,
    /// the pacers' lag and counters).
    pub input: String,
}

/// `0x4010 0.0% 0x4011 12.5%`, or `-` before any data.
fn null_fill(ts_ids: &[u16], real: &[u64], nulls: &[u64]) -> String {
    if real.iter().chain(nulls).all(|&n| n == 0) {
        return "-".into();
    }
    ts_ids
        .iter()
        .zip(real.iter().zip(nulls))
        .map(|(id, (&r, &n))| format!("{id:#06x} {:5.1}%", ratio(n, r + n)))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Hand the produced superframes to `sink` with status lines until the
/// producer ends, the sink fails or a stop is requested.
pub fn drive(
    ts_ids: &[u16],
    fs: f64,
    items: Receiver<ProducedSf>,
    worker: JoinHandle<Result<EndReason>>,
    sink: &mut TxSink,
    status_secs: f64,
    stop: &AtomicBool,
) -> Result<SatReport> {
    let sf_s = superframe_seconds();
    let n = ts_ids.len();
    let mut rep = SatReport {
        real: vec![0; n],
        nulls: vec![0; n],
        ..SatReport::default()
    };
    let (mut iv_real, mut iv_nulls) = (vec![0u64; n], vec![0u64; n]);
    let (mut iv_items, mut iv_input, mut iv_mod) = (0u64, Duration::ZERO, Duration::ZERO);
    let realtime_sink = sink.realtime();
    let t0 = Instant::now();
    let mut next_status = t0 + Duration::from_secs_f64(status_secs.max(0.0));
    let mut aborted = false;
    let mut err = None;
    let mut input_status = String::new();
    for f in items.iter() {
        if stop.load(Ordering::Relaxed) {
            aborted = true;
            break;
        }
        if let Err(e) = sink.write(&f.payload, stop) {
            aborted = true;
            err = Some(e);
            break;
        }
        rep.items += 1;
        match &f.payload {
            Payload::Samples(x) => rep.samples += x.len() as u64,
            Payload::Codes(c) => rep.codes += c.len() as u64,
        }
        rep.on_air_s += f.payload.seconds(fs);
        iv_items += 1;
        iv_input += f.input_time;
        iv_mod += f.modulate_time;
        if !f.input_status.is_empty() {
            input_status = f.input_status;
        }
        match f.kind {
            FrameKind::Lead => rep.lead += 1,
            FrameKind::Flush => rep.flush += 1,
            FrameKind::Data => {
                rep.data += 1;
                for r in 0..n {
                    rep.real[r] += f.real[r] as u64;
                    rep.nulls[r] += f.nulls[r] as u64;
                    iv_real[r] += f.real[r] as u64;
                    iv_nulls[r] += f.nulls[r] as u64;
                }
            }
        }
        if status_secs > 0.0 && Instant::now() >= next_status {
            next_status += Duration::from_secs_f64(status_secs);
            let speed = producer_speed(iv_items, iv_input + iv_mod, sf_s);
            let on_air = rep.on_air_s;
            info!(
                target: "isdb::tx",
                on_air_s = on_air,
                superframes = rep.items,
                data_superframes = rep.data,
                speed = speed.unwrap_or(f64::NAN),
                "{on_air:7.2} s on air, {} superframes ({} data) | null fill {} (total {}){input_status} | {}{}",
                rep.items,
                rep.data,
                null_fill(ts_ids, &iv_real, &iv_nulls),
                null_fill(ts_ids, &rep.real, &rep.nulls),
                describe_speed(speed, iv_items, iv_input, iv_mod, "superframe"),
                sink.status()
            );
            if realtime_sink && speed.is_some_and(|x| x < 1.0) {
                warn!(
                    target: "isdb::tx",
                    speed = speed.unwrap_or(0.0),
                    input_ms = ms_per(iv_input, iv_items),
                    modulation_ms = ms_per(iv_mod, iv_items),
                    superframe_ms = sf_s * 1e3,
                    "the producer runs below real time ({:.2}×): the radio underruns. Per superframe: input {:.1} ms + modulation {:.1} ms for a {:.2} ms superframe{}",
                    speed.unwrap_or(0.0),
                    ms_per(iv_input, iv_items),
                    ms_per(iv_mod, iv_items),
                    sf_s * 1e3,
                    if cfg!(debug_assertions) {
                        " — this is a debug build; use `cargo build --release -p isdb-cli`"
                    } else {
                        ""
                    }
                );
            }
            iv_real.fill(0);
            iv_nulls.fill(0);
            (iv_items, iv_input, iv_mod) = (0, Duration::ZERO, Duration::ZERO);
        }
    }
    if stop.load(Ordering::Relaxed) {
        aborted = true;
    }
    drop(items); // unblocks the producer if it is waiting on the queue
    let joined = worker.join();
    // The sink (a radio) is stopped before anything else, also when the
    // producer panicked; its panic is then re-raised.
    let fin = sink.finish(aborted);
    let end = joined.unwrap_or_else(|p| std::panic::resume_unwind(p));
    if let Some(e) = err {
        if let Ok(s) = &fin {
            info!(target: "isdb::tx", "{s}");
        }
        return Err(e);
    }
    rep.end = Some(end?);
    rep.sink = fin?;
    rep.input = input_status;
    Ok(rep)
}

/// The SigMF metadata of an ISDB-S `--iq` file: its format and rate,
/// `isdb:system` "ISDB-S", the layout in `core:description`, and one
/// capture segment at 0 with the centre frequency of `--channel` /
/// `--freq` when given (a satellite channel's IF, with its channel fields
/// as `isdb capture` writes them).
pub fn metadata(
    format: IqFormat,
    rate: f64,
    backoff_db: f32,
    layout: &Layout,
    channel: Option<Channel>,
    freq: Option<u64>,
) -> Result<Meta> {
    let sf = match format {
        IqFormat::Cf32 => isdb_iq::SampleFormat::Cf32,
        IqFormat::Cs8 => isdb_iq::SampleFormat::Cs8,
    };
    let mut m = Meta::new(sf, rate);
    m.recorder = Some(format!("isdb tx {}", env!("CARGO_PKG_VERSION")));
    m.description = Some(format!(
        "isdb tx ISDB-S: {}; RMS {backoff_db} dB below full scale",
        layout.describe()
    ));
    m.global_extra.insert("isdb:system".into(), json!("ISDB-S"));
    let mut extra = serde_json::Map::new();
    let frequency = match (channel, freq) {
        (Some(c), _) => {
            extra.insert("isdb:channel".into(), json!(c.to_string()));
            extra.insert("isdb:channel_rf_hz".into(), json!(c.rf_hz()?));
            if let (Some(lo), Some(pol)) = (c.lnb_lo_hz(), c.polarisation()) {
                extra.insert("isdb:lnb_lo_hz".into(), json!(lo));
                extra.insert("isdb:polarisation".into(), json!(pol.name()));
            }
            Some(c.tune_hz()? as f64)
        }
        (None, Some(f)) => Some(f as f64),
        (None, None) => None,
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0.0, |d| d.as_secs_f64());
    m.captures.push(Segment {
        sample_start: 0,
        frequency,
        datetime: Some(isdb_iq::iso8601_utc(now)),
        extra,
    });
    Ok(m)
}

/// `isdb tx --system isdbs`.
pub fn run(a: TxArgs, given: &Given) -> Result<()> {
    let layout = Layout::of(&a.sat)?;
    let rate = a.sat.rate.unwrap_or(DEFAULT_RATE);
    let sys = isdbs_phy::frontend::system();
    let shaper = Shaper::new(rate, sys.symbol_rate_hz, sys.roll_off)
        .with_context(|| format!("--rate {rate}"))?;
    let tx = Transmitter::new(layout.info.clone())?;
    let sf_s = superframe_seconds();
    let seconds = crate::tx::resolve_seconds(a.seconds)?;
    let endless = layout.synthetic_only() || a.input.feed == Feed::Loop;
    let counts: Vec<usize> = (0..tx.ts_count())
        .map(|r| tx.packets_per_superframe(r))
        .collect();
    info!(
        target: "isdb::tx",
        system = "ISDB-S",
        "ISDB-S (ITU-R BO.1408-1): {}",
        layout.describe()
    );
    for (t, &n) in layout.ts.iter().zip(&counts) {
        let bps = n as f64 * 188.0 * 8.0 / sf_s;
        info!(
            target: "isdb::tx",
            ts_id = t.ts_id,
            slots = t.slots.unwrap_or(0),
            packets_per_superframe = n,
            bps,
            "  TS_ID {:#06x}: {} slots/frame, {n} TSP/superframe ({:.2} Mbit/s)",
            t.ts_id,
            t.slots.unwrap_or(0),
            bps / 1e6
        );
    }
    info!(
        target: "isdb::tx",
        superframe_s = sf_s,
        samples_per_superframe = SUPERFRAME_SYMBOLS as f64 * shaper.sps(),
        sample_rate_hz = rate,
        "per superframe: {FRAMES} frames, {SUPERFRAME_SYMBOLS} symbols = {:.3} ms, {:.2} samples at {:.3} Msps ({:.4} samples/symbol, RRC roll-off {})",
        sf_s * 1e3,
        SUPERFRAME_SYMBOLS as f64 * shaper.sps(),
        rate / 1e6,
        shaper.sps(),
        sys.roll_off
    );
    // the placement of each TS file, before anything is opened or written
    // (a dry run shows it; an input over its slots' capacity is refused)
    let plan = plan_inputs(
        &layout,
        a.input.feed,
        a.sat.ts_pacing,
        a.input.allow_overrate,
    )?;
    let bb = if given.has("bb_filter_hz") {
        a.tune.bb_filter_hz
    } else {
        DEFAULT_BB_FILTER_HZ
    };
    let stop = crate::stop::flag();
    let mut sink = open_sink(&a, rate, bb, sf_s, endless, seconds, |path| {
        // the data file first: a sidecar is never left without one
        let stream = crate::tx::is_stream_path(path);
        let sink = FileSink::create(path, a.iq_format, a.backoff_db)?.with_cf32_backoff();
        if stream {
            info!(
                target: "isdb::tx",
                "--iq {path}: a stream, no SigMF sidecar written (`isdb rx --iq` needs one to read ISDB-S from a file)"
            );
        } else {
            let meta = isdb_iq::meta_path_for(std::path::Path::new(path));
            let m = metadata(
                a.iq_format,
                rate,
                a.backoff_db,
                &layout,
                a.tune.channel,
                a.tune.freq,
            )?;
            std::fs::write(&meta, m.to_json())
                .with_context(|| format!("write {}", meta.display()))?;
            info!(target: "isdb::tx", sidecar = %meta.display(), "SigMF metadata: {}", meta.display());
        }
        Ok(sink)
    })?;
    let codes = matches!(sink, TxSink::Codes(_));
    if codes {
        let link = isdb_frontend::txformat::link_rate(sys.symbol_rate_hz);
        info!(
            target: "isdb::tx",
            link_bytes_per_s = link,
            "TX path: symbol codes to the radio's FPGA transmit block ({SUPERFRAME_SYMBOLS} per superframe, {:.2} MB/s), which shapes them at {:.3} Msps",
            link / 1e6,
            rate / 1e6
        );
    }
    let lead = a.sat.lead_superframes.unwrap_or(1);
    let placed = match &a.placed_ts {
        Some(t) => {
            let w = PlacedTs::create(t, &layout.ts_ids())?;
            info!(
                target: "isdb::tx",
                "--placed-ts: the packets of each data superframe as placed in the slots -> {}",
                w.paths().join(", ")
            );
            Some(w)
        }
        None => None,
    };
    let cfg = SatConfig {
        lead,
        flush: !a.no_flush,
        max_superframes: (seconds > 0.0).then(|| superframes_for_seconds(seconds)),
        pace: a.realtime || crate::producer::default_pace(a.input.feed, sink.realtime()),
        record: false,
        depth: a.queue_frames,
        placed,
    };
    info!(
        target: "isdb::tx",
        lead_superframes = cfg.lead,
        paced = cfg.pace,
        "duration: {} | lead-in {} superframes, flush {}{}",
        cfg.max_superframes.map_or_else(
            || if endless {
                "unbounded (the input never ends); stop with Ctrl-C".into()
            } else {
                "unbounded (until the input ends); stop with Ctrl-C".into()
            },
            |n| format!("{seconds} s ({n} superframes)")
        ),
        cfg.lead,
        match (cfg.flush, codes) {
            (false, _) => "none",
            (true, false) => "a null frame and the shaper's tail",
            (true, true) => "a null frame (the block's filter drains on the zero symbols after it)",
        },
        if cfg.pace { ", paced to real time" } else { "" }
    );
    let input = TsInputs::open(&layout, a.input.feed, &counts, &plan)?;
    info!(target: "isdb::tx", "input: {}", input.describe());
    let ts_ids = layout.ts_ids();
    let modulate = sink.modulate(shaper);
    let (items, worker) = spawn(cfg, tx, modulate, input, stop.clone())?;
    let rep = drive(
        &ts_ids,
        rate,
        items,
        worker,
        &mut sink,
        a.status_secs,
        &stop,
    )?;
    crate::source::check_not_scrambled()?;
    let end = rep.end.unwrap_or(EndReason::Stopped);
    info!(
        target: "isdb::tx",
        end = ?end,
        superframes = rep.items,
        on_air_s = rep.on_air_s,
        lead_superframes = rep.lead,
        data_superframes = rep.data,
        flush = rep.flush,
        samples = rep.samples,
        codes = rep.codes,
        "done ({end:?}): {} superframes = {:.3} s on air ({} lead, {} data, {}), {}",
        rep.items,
        rep.on_air_s,
        rep.lead,
        rep.data,
        if rep.flush > 0 { "flush" } else { "no flush" },
        if codes {
            format!("{} symbol codes", rep.codes)
        } else {
            format!("{} samples", rep.samples)
        }
    );
    for (r, id) in ts_ids.iter().enumerate() {
        let (real, nulls) = (rep.real[r], rep.nulls[r]);
        info!(
            target: "isdb::tx",
            ts_id = id,
            input_packets = real,
            null_packets = nulls,
            null_fill_pct = ratio(nulls, real + nulls),
            "  TS_ID {id:#06x}: {real} input packets, {nulls} null padding ({:.1}% null fill)",
            ratio(nulls, real + nulls)
        );
    }
    if !rep.input.is_empty() {
        info!(
            target: "isdb::tx",
            "  input at the end: {}",
            rep.input.trim_start().trim_start_matches("| ")
        );
    }
    info!(target: "isdb::tx", "{}", rep.sink);
    Ok(())
}

/// The warning of the IQ path to a radio: the rate limit.
fn warn_iq_radio(rate: f64) {
    warn!(
        target: "isdb::tx",
        sample_rate_hz = rate,
        "ISDB-S needs ≥ 38.96 Msps: a Pluto over Gigabit Ethernet streams about 65 MB/s (16 Msps of cs12), so this IQ transmission underruns (the radio counts it) and a HackRF refuses the rate; continuous ISDB-S transmission needs the radio's FPGA transmit block (--offload)"
    );
}

/// The parameters of the FPGA transmit block for ISDB-S at `rate` with
/// the level `backoff_db` below the 12-bit full scale in its banks
/// (`isdb_frontend::shape::fixed`: 512 phases × 24 taps).
pub fn block_params(rate: f64, backoff_db: f32) -> Result<TxParams> {
    let mut d = TxDesign::for_system(isdbs_phy::frontend::system(), rate);
    d.backoff_db = f64::from(backoff_db);
    TxParams::design(&d).with_context(|| {
        format!("the FPGA transmit block's parameters (--rate {rate}, --backoff-db {backoff_db})")
    })
}

/// `TX format 1, 512 × 24 taps`, the transmit blocks among `offloads`, or
/// `none`.
fn describe_tx_blocks(offloads: &[isdb_radio::Offload]) -> String {
    let v: Vec<String> = offloads
        .iter()
        .filter_map(|o| match o {
            isdb_radio::Offload::TxSymbols {
                version,
                phases,
                taps,
            } => Some(format!("TX format {version}, {phases} × {taps} taps")),
            _ => None,
        })
        .collect();
    if v.is_empty() {
        "none".into()
    } else {
        v.join(", ")
    }
}

/// How `tx --system isdbs` reaches a radio.
enum Plan {
    /// Symbol codes to the FPGA transmit block.
    Codes,
    /// IQ samples shaped on the CPU.
    Iq,
    /// Not decided: a dry run of `--offload auto`, which opens nothing.
    Either,
}

/// The sink of `tx --system isdbs`: the `--iq` file (`file` creates it),
/// or `--radio` through the `--rf-enable` / `--dry-run` gate, sent IQ
/// samples or, through its FPGA transmit block (`--offload`), symbol
/// codes. `--offload auto|fpga` with `--rf-enable` probes the radio first
/// (read-only); a dry run never opens it, so `auto` describes both paths
/// and `fpga` the codes'.
fn open_sink(
    a: &TxArgs,
    rate: f64,
    bb: u32,
    sf_s: f64,
    endless: bool,
    seconds: f64,
    file: impl FnOnce(&str) -> Result<FileSink>,
) -> Result<TxSink> {
    let radio_cfg = |spec: &isdb_radio::DeviceSpec| {
        let t = a.tune.satellite_tuning(rate)?;
        Ok((a.tune.config(&t, bb, spec, &a.gain), t))
    };
    let iq =
        |file| crate::tx::open_sink(a, sf_s, endless, seconds, radio_cfg, file).map(TxSink::Iq);
    let spec = match (&a.radio, a.offload) {
        (None, OffloadArg::Fpga) => bail!(
            "--offload fpga: the FPGA transmit block is a radio's (--radio); --iq always gets the CPU's samples (--offload cpu or auto)"
        ),
        (None, _) => return iq(file),
        (Some(_), OffloadArg::Cpu) => {
            warn_iq_radio(rate);
            return iq(file);
        }
        (Some(spec), _) => spec,
    };
    let p = block_params(rate, a.backoff_db)?;
    let backend = isdb_radio::backend(&spec.backend)?;
    let (cfg, tuning) = radio_cfg(spec)?;
    let opts = crate::tx::tx_options(a, sf_s);
    let plan = match (a.offload, a.rf_enable && !a.dry_run) {
        (OffloadArg::Fpga, false) => Plan::Codes,
        (_, false) => Plan::Either,
        (offload, true) => {
            let offloads = backend
                .tx_offloads(&cfg)
                .with_context(|| format!("probe radio {spec} for an FPGA transmit block"))?;
            match (offloads.iter().any(|o| o.runs_tx(&p)), offload) {
                (true, _) => {
                    info!(
                        target: "isdb::tx",
                        "the radio has a compatible FPGA transmit block ({})",
                        describe_tx_blocks(&offloads)
                    );
                    Plan::Codes
                }
                (false, OffloadArg::Fpga) => bail!(
                    "--offload fpga: the radio has no compatible FPGA transmit block (found: {}; needed: TX format {}, {} × {} taps)",
                    describe_tx_blocks(&offloads),
                    isdb_frontend::txformat::FORMAT_VERSION,
                    p.phases(),
                    p.taps()
                ),
                (false, _) => {
                    info!(
                        target: "isdb::tx",
                        "the radio has no compatible FPGA transmit block (found: {}): IQ samples shaped on the CPU",
                        describe_tx_blocks(&offloads)
                    );
                    Plan::Iq
                }
            }
        }
    };
    match plan {
        Plan::Iq => {
            warn_iq_radio(rate);
            iq(file)
        }
        Plan::Codes => {
            let settings = backend.describe_tx_symbols(&cfg, &opts, &p)?;
            crate::tx::gate(a, backend, &tuning, &settings, endless, seconds)?;
            let (sink, info) = backend
                .open_tx_symbols(&cfg, &opts, &p)
                .with_context(|| format!("open radio {spec} for TX of symbol codes"))?;
            info!(target: "isdb::tx", "opened: {info}");
            Ok(TxSink::Codes(sink))
        }
        Plan::Either => match backend.describe_tx_symbols(&cfg, &opts, &p) {
            Ok(codes) => {
                // the IQ path's own lines (device and tuning are the same)
                let samples: Vec<String> = backend
                    .describe_tx(&cfg, &opts)?
                    .lines()
                    .filter(|l| !codes.lines().any(|c| c == *l))
                    .map(str::to_owned)
                    .collect();
                let settings = format!(
                    "{codes}\n  --offload auto: the FPGA path above if the radio reports a compatible transmit block when it is opened (a read-only probe, which a dry run does not make); otherwise IQ samples shaped on the CPU, which a Pluto cannot stream at this rate (it underruns), with\n{}",
                    samples.join("\n")
                );
                crate::tx::gate(a, backend, &tuning, &settings, endless, seconds)?;
                bail!("--offload auto decides when the radio is opened: not transmitting")
            }
            // the backend has no transmit block: the IQ path's dry run
            Err(RadioError::Unsupported { .. }) => {
                warn_iq_radio(rate);
                iq(file)
            }
            Err(e) => Err(e.into()),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::is_null;
    use crate::source::tests::pkt;
    use clap::{CommandFactory, FromArgMatches, Parser};
    use std::sync::mpsc::sync_channel;

    #[derive(Parser)]
    struct T {
        #[command(flatten)]
        a: TxArgs,
    }

    /// `isdb tx` arguments and what was given.
    fn parse(args: &[&str]) -> Result<(TxArgs, Given), clap::Error> {
        let mut v = vec!["tx"];
        v.extend_from_slice(args);
        let m = T::command().try_get_matches_from(v)?;
        let t = T::from_arg_matches(&m)?;
        Ok((t.a, Given::of(Some(&m))))
    }

    #[test]
    fn combination_and_ts_syntax() {
        assert_eq!(
            parse_combination("tc8psk2/3:48").unwrap(),
            (Combination::Tc8psk2_3, 48)
        );
        assert_eq!(
            parse_combination("QPSK 3/4:44").unwrap(),
            (Combination::Qpsk3_4, 44)
        );
        assert_eq!(
            parse_combination("bpsk1/2: 4").unwrap(),
            (Combination::Bpsk1_2, 4)
        );
        for c in Combination::ALL {
            assert_eq!(parse_combination(&format!("{c}:8")).unwrap(), (c, 8));
        }
        let e = parse_combination("8psk2/3:48").unwrap_err().to_string();
        assert!(e.contains("bpsk1/2, qpsk1/2, qpsk2/3"), "{e}");
        assert!(parse_combination("tc8psk2/3").is_err());
        assert!(parse_combination("tc8psk2/3:0").is_err());
        assert!(parse_combination("tc8psk2/3:49").is_err());
        assert_eq!(
            parse_ts("0x4010").unwrap(),
            TsArg {
                ts_id: 0x4010,
                slots: None,
                path: None
            }
        );
        assert_eq!(
            parse_ts("4011h:24=a:b.ts").unwrap(),
            TsArg {
                ts_id: 0x4011,
                slots: Some(24),
                path: Some("a:b.ts".into())
            }
        );
        assert_eq!(parse_ts("16400=-").unwrap().path.as_deref(), Some("-"));
        assert!(parse_ts("0x4010=").is_err());
        assert!(parse_ts("0x4010:0").is_err());
        assert!(parse_ts("0x10000").is_err());
        assert!(parse_ts("zz:3").is_err());
    }

    #[test]
    fn layouts_and_their_errors() {
        let sat = |args: &[&str]| {
            let mut v = vec!["--system", "isdbs", "--iq", "x"];
            v.extend_from_slice(args);
            parse(&v).unwrap().0.sat
        };
        // default: the BS combination, one synthetic TS with every slot
        let l = Layout::of(&sat(&[])).unwrap();
        assert_eq!(l.ts_ids(), [DEFAULT_TS_ID]);
        assert_eq!(l.ts[0].slots, Some(48));
        assert!(l.synthetic_only());
        assert_eq!(l.describe(), "TC8PSK 2/3 ×48; TS_ID 0x4010 48 slots");
        // the CS layout sends 36 of 48 slots
        let l = Layout::of(&sat(&["--combination", "qpsk3/4:48", "--ts", "7"])).unwrap();
        assert_eq!(l.ts[0].slots, Some(36));
        // a mix with two TSs
        let l = Layout::of(&sat(&[
            "--combination",
            "bpsk1/2:4",
            "--combination",
            "tc8psk2/3:44",
            "--ts",
            "0x4010:30=a.ts",
            "--ts",
            "0x4011:15",
        ]))
        .unwrap();
        assert_eq!(
            isdbs_phy::rx::describe_tmcc(&l.info),
            "TC8PSK 2/3 ×44 + BPSK 1/2 ×4"
        );
        assert!(!l.synthetic_only());
        // slots needed with two TSs, and they must sum to the sent slots
        let e = Layout::of(&sat(&["--ts", "0x4010", "--ts", "0x4011:24"]))
            .unwrap_err()
            .to_string();
        assert!(
            e.contains("give its slots per frame") && e.contains("48"),
            "{e}"
        );
        let e = format!(
            "{:#}",
            Layout::of(&sat(&["--ts", "0x4010:24", "--ts", "0x4011:16"])).unwrap_err()
        );
        assert!(
            e.contains("the TSs take 40 slots, the combinations send 48"),
            "{e}"
        );
        // whole units, a slot sum of 48, distinct TS_IDs, two stdins
        assert!(
            Layout::of(&sat(&[
                "--combination",
                "qpsk3/4:46",
                "--combination",
                "tc8psk2/3:2"
            ]))
            .is_err()
        );
        assert!(Layout::of(&sat(&["--combination", "qpsk3/4:44"])).is_err());
        assert!(Layout::of(&sat(&["--ts", "0x4010:24", "--ts", "0x4010:24"])).is_err());
        assert!(
            Layout::of(&sat(&["--ts", "1:24=-", "--ts", "2:24=-"]))
                .unwrap_err()
                .to_string()
                .contains("stdin")
        );
    }

    #[test]
    fn options_of_the_other_system_are_refused() {
        let check = |args: &[&str]| -> Result<()> {
            let (a, g) = parse(args).unwrap();
            check_system(a.system, &g, TX_ISDBT_ONLY, TX_ISDBS_ONLY)
        };
        check(&["--iq", "x", "--synthetic"]).unwrap();
        check(&["--iq", "x", "--system", "isdbs", "--ts", "0x4010"]).unwrap();
        // defaults are not "given": ISDB-S with the ISDB-T defaults is fine
        check(&["--iq", "x", "--system", "isdbs"]).unwrap();
        for args in [
            &["--mode", "1"][..],
            &["--layer-b", "none"],
            &["--b-ts", "b.ts"],
            &["--synthetic"],
            &["--lead-frames", "3"],
            &["--relay", "r.ts"],
            &["--iq-in", "x.cf32"],
        ] {
            let mut v = vec!["--iq", "x", "--system", "isdbs"];
            v.extend_from_slice(args);
            let e = check(&v).unwrap_err().to_string();
            assert!(
                e.contains(args[0]) && e.contains("ISDB-T option") && e.contains("--system isdbs"),
                "{e}"
            );
        }
        // an explicit ISDB-T default is still an ISDB-T option
        let e = check(&["--iq", "x", "--system", "isdbs", "--mode", "3", "--gi", "8"])
            .unwrap_err()
            .to_string();
        assert!(e.starts_with("--mode, --gi: ISDB-T option(s)"), "{e}");
        for args in [
            &["--combination", "qpsk3/4:48"][..],
            &["--ts", "0x4010"],
            &["--rate", "40M"],
            &["--lead-superframes", "2"],
            &["--offload", "cpu"],
        ] {
            let mut v = vec!["--iq", "x", "--synthetic"];
            v.extend_from_slice(args);
            let e = check(&v).unwrap_err().to_string();
            assert!(
                e.contains(args[0]) && e.contains("ISDB-S option") && e.contains("--system isdbs"),
                "{e}"
            );
        }
        // every id of the lists is an argument whose flag `flag` spells
        let cmd = T::command();
        for id in TX_ISDBT_ONLY
            .iter()
            .chain(TX_ISDBS_ONLY)
            .chain(["bb_filter_hz"].iter())
        {
            let arg = cmd
                .get_arguments()
                .find(|x| x.get_id() == *id)
                .unwrap_or_else(|| panic!("no argument {id}"));
            assert_eq!(format!("--{}", arg.get_long().unwrap()), flag(id));
        }
    }

    #[test]
    fn bb_filter_default_is_the_given_value_or_40_mhz() {
        let (_, g) = parse(&["--iq", "x", "--system", "isdbs"]).unwrap();
        assert!(!g.has("bb_filter_hz"));
        let (a, g) = parse(&[
            "--iq",
            "x",
            "--system",
            "isdbs",
            "--bb-filter-hz",
            "30000000",
        ])
        .unwrap();
        assert!(g.has("bb_filter_hz"));
        assert_eq!(a.tune.bb_filter_hz, 30_000_000);
    }

    #[test]
    fn synthetic_packets_are_distinct_and_say_where_they_were_sent() {
        let a = synthetic_packet(0x4010, 7, 3, 9);
        assert_eq!(&a[..4], &[0x47, 0x01, 0x00, 0x19]);
        assert_eq!(&a[4..6], &[0x40, 0x10]);
        assert_eq!(u64::from_be_bytes(a[6..14].try_into().unwrap()), 7);
        assert_eq!(u32::from_be_bytes(a[14..18].try_into().unwrap()), 3);
        assert_ne!(a, synthetic_packet(0x4011, 7, 3, 9));
        assert_ne!(a[18..], synthetic_packet(0x4010, 8, 3, 9)[18..]);
        assert_ne!(a[18..], synthetic_packet(0x4010, 7, 4, 9)[18..]);
        assert!(!is_null(&a));
    }

    #[test]
    fn inputs_pad_with_nulls_and_end_with_the_files() {
        let (tx, rx) = sync_channel(64);
        let s = PacketStream::from_receiver("a.ts", rx, Feed::File);
        let mut inp = TsInputs::from_streams(
            vec![0x4010, 0x4011],
            vec![Some((s, Placement::Arrival)), None],
            Feed::File,
        );
        let stop = AtomicBool::new(false);
        for n in 0..5 {
            tx.send(pkt(0x0111, n)).unwrap();
        }
        drop(tx);
        let p = inp.take(&[4, 3], &stop).unwrap();
        assert_eq!(p.real, [4, 3]);
        assert_eq!(p.ts[0], (0..4).map(|n| pkt(0x0111, n)).collect::<Vec<_>>());
        assert_eq!(p.ts[1][2], synthetic_packet(0x4011, 0, 2, 2));
        // the file runs dry: padded with nulls, the synthetic TS goes on
        let p = inp.take(&[4, 3], &stop).unwrap();
        assert_eq!(p.real, [1, 3]);
        assert!(p.ts[0][1..].iter().all(is_null));
        assert_eq!(p.ts[1][0], synthetic_packet(0x4011, 1, 0, 3));
        // every file ended without a packet: the end, synthetic TSs too
        assert!(inp.take(&[4, 3], &stop).is_none());
        assert!(
            inp.describe()
                .contains("TS_ID 0x4010 <- a.ts, TS_ID 0x4011 <- synthetic")
        );
        // synthetic TSs alone never end; a stop does
        let mut syn = TsInputs::from_streams(vec![1], vec![None], Feed::File);
        for _ in 0..3 {
            assert!(syn.take(&[2], &stop).is_some());
        }
        stop.store(true, Ordering::SeqCst);
        assert!(syn.take(&[2], &stop).is_none());
    }

    /// AF-only PCR packet (H.222.0 Table 2-6) on `pid`.
    fn pcr_only(pid: u16, ticks: u64) -> Packet {
        let mut p = [0xFFu8; 188];
        let [hi, lo] = pid.to_be_bytes();
        p[..6].copy_from_slice(&[0x47, hi, lo, 0x20, 183, 0x10]);
        p[6..12].copy_from_slice(&isdb_ts::pcr::Pcr::from_ticks(ticks).encode().unwrap());
        p
    }

    /// A CBR TS of `n` packets at `tpp` ticks of 27 MHz each (packet i at
    /// `pcr0` + i·tpp, H.222.0 §2.4.2.2): an AF-only PCR (PID 0x01FF)
    /// every 199 packets (≤ 30 ms at ≥ 9.98 Mbit/s) from packet 0, PAT and
    /// PMT (program 1, PCR_PID 0x01FF, a private stream on 0x0111) every
    /// 332 packets, data on 0x0111 in every third packet, nulls elsewhere.
    fn cbr_ts(n: u64, tpp: u64, pcr0: u64) -> Vec<Packet> {
        use isdb_ts::cc::Counter;
        use isdb_ts::packetize::{Packing, packetize_sections};
        use isdb_ts::psi::{Pat, PatEntry, Pmt, PmtStream};
        let pmt_pid = Pid::new(0x01F0).unwrap();
        let es = Pid::new(0x0111).unwrap();
        let pat = Pat::section(0x4010, 0, vec![PatEntry::new(1, pmt_pid)])
            .encode()
            .unwrap();
        let pmt = Pmt::section(
            1,
            0,
            Pmt::new(
                Pid::new(0x01FF).unwrap(),
                vec![],
                vec![PmtStream::new(0x06, es, vec![])],
            ),
        )
        .encode()
        .unwrap();
        let (mut c_pat, mut c_pmt, mut c_es) = (Counter::default(), Counter::default(), 0u8);
        let one = |pid, sec: &Vec<u8>, c: &mut Counter| {
            let v = packetize_sections(pid, std::slice::from_ref(sec), Packing::OnePerPacket, c)
                .unwrap();
            assert_eq!(v.len(), 1);
            v[0]
        };
        (0..n)
            .map(|i| match i {
                i if i % 199 == 0 => pcr_only(0x01FF, pcr0 + i * tpp),
                i if i % 332 == 1 => one(Pid::PAT, &pat, &mut c_pat),
                i if i % 332 == 2 => one(pmt_pid, &pmt, &mut c_pmt),
                i if i % 3 == 0 => {
                    let mut p = pkt(0x0111, u32::try_from(i).unwrap());
                    p[3] = 0x10 | c_es;
                    c_es = (c_es + 1) & 15;
                    p
                }
                _ => null_packet(),
            })
            .collect()
    }

    /// The PAT's transport_stream_id is found after a pointer_field and
    /// an adaptation field, only in a PID 0 packet that starts a section
    /// with table_id 0x00; `check_pat` is done once a PAT has been seen
    /// (matching or not) and waits for one otherwise.
    #[test]
    fn the_first_pat_names_its_transport_stream_id() {
        let ts = cbr_ts(400, 300, 0);
        assert_eq!(ts.iter().find_map(pat_ts_id), Some(0x4010));
        let pat = ts[1];
        assert_eq!(pat_ts_id(&pat), Some(0x4010));
        // pointer_field 2, then the section
        let mut p = pat;
        p[4] = 2;
        p[7..].copy_from_slice(&pat[5..186]);
        assert_eq!(pat_ts_id(&p), Some(0x4010));
        // an adaptation field of 3 bytes before the payload
        let mut p = pat;
        p[3] = 0x30 | (pat[3] & 15);
        p[4..8].copy_from_slice(&[2, 0, 0xFF, 0]);
        p[8..].copy_from_slice(&pat[5..185]);
        assert_eq!(pat_ts_id(&p), Some(0x4010));
        // not a PAT: another PID, no section start, another table_id
        let mut p = pat;
        p[2] = 0x01;
        assert_eq!(pat_ts_id(&p), None);
        let mut p = pat;
        p[1] &= !0x40;
        assert_eq!(pat_ts_id(&p), None);
        let mut p = pat;
        p[5] = 0x02;
        assert_eq!(pat_ts_id(&p), None);
        assert!(check_pat(0x4010, "a.ts", &ts));
        assert!(check_pat(0x4017, "a.ts", &ts), "a mismatch is checked too");
        assert!(!check_pat(0x4010, "a.ts", &ts[2..300]), "no PAT yet");
    }

    /// A paced TS spreads its packets over the superframe's slots: the
    /// first PAT is found among them all (here after the first `real`
    /// slots: 1 Mbit/s in 26 Mbit/s of slots), in the superframe that
    /// carries it, and the TS is not checked again; a synthetic TS is
    /// never checked.
    #[test]
    fn the_pat_of_a_paced_input_is_checked_once() {
        let d = scratch("pat");
        let path = d.join("cbr.ts");
        std::fs::write(&path, cbr_ts(665, 40_610, 5_000).concat()).unwrap();
        let layout = layout_of(&[format!("0x4017:24={}", path.display()), "0x4011:24".into()]);
        let plan = plan_inputs(&layout, Feed::File, TsPacing::Auto, false).unwrap();
        assert!(matches!(plan[0], Placement::Pcr(_)), "{plan:?}");
        let mut inp = TsInputs::open(&layout, Feed::File, &[192, 192], &plan).unwrap();
        let stop = AtomicBool::new(false);
        let sf = inp.take(&[192, 192], &stop).unwrap();
        let at = sf.ts[0].iter().position(|p| pat_ts_id(p).is_some());
        assert!(at.is_some_and(|i| i >= sf.real[0]), "{at:?} {:?}", sf.real);
        assert_eq!(inp.pat_checked, [true, false]);
        inp.take(&[192, 192], &stop).unwrap();
        assert_eq!(inp.pat_checked, [true, false]);
        std::fs::remove_dir_all(&d).unwrap();
    }

    /// A fresh directory under the system's temporary directory.
    fn scratch(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("isdb-txs-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// The layout of `ts` (`--ts` arguments), the BS combination.
    fn layout_of(ts: &[String]) -> Layout {
        let mut v = vec!["--system", "isdbs", "--iq", "x"];
        for t in ts {
            v.extend(["--ts", t.as_str()]);
        }
        Layout::of(&parse(&v).unwrap().0.sat).unwrap()
    }

    /// Every superframe of `inp` until it ends: the packets per relative
    /// TS, concatenated, and the superframes taken.
    fn drain(inp: &mut TsInputs, counts: &[usize]) -> (Vec<Vec<Packet>>, usize) {
        let stop = AtomicBool::new(false);
        let mut out = vec![Vec::new(); counts.len()];
        let mut n = 0;
        while let Some(p) = inp.take(counts, &stop) {
            for (o, v) in out.iter_mut().zip(p.ts) {
                o.extend(v);
            }
            n += 1;
        }
        (out, n)
    }

    #[test]
    fn plan_paces_files_with_a_pcr_and_checks_their_rate() {
        let d = scratch("plan");
        let write = |name: &str, p: &[Packet]| {
            let path = d.join(name);
            std::fs::write(&path, p.concat()).unwrap();
            path.display().to_string()
        };
        // 1 s at 9.999 Mbit/s: 3.4 Mbit/s without the nulls
        let cbr = write("cbr.ts", &cbr_ts(6649, 4061, 0));
        let plain = write(
            "plain.ts",
            &(0..500).map(|i| pkt(0x0111, i)).collect::<Vec<_>>(),
        );
        let one = |path: &str| layout_of(&[format!("0x4010={path}")]);
        let plan = |l: &Layout, feed, pacing, allow| plan_inputs(l, feed, pacing, allow);
        let pcr = Pid::new(0x01FF);
        assert_eq!(
            plan(&one(&cbr), Feed::File, TsPacing::Auto, false).unwrap(),
            [Placement::Pcr(pcr)]
        );
        assert_eq!(
            plan(&one(&cbr), Feed::Loop, TsPacing::Pcr, false).unwrap(),
            [Placement::Pcr(pcr)]
        );
        assert_eq!(
            plan(&one(&cbr), Feed::File, TsPacing::Arrival, false).unwrap(),
            [Placement::Arrival]
        );
        // no PCR: arrival order, or refused with `pcr`
        assert_eq!(
            plan(&one(&plain), Feed::File, TsPacing::Auto, false).unwrap(),
            [Placement::Arrival]
        );
        let e = plan(&one(&plain), Feed::File, TsPacing::Pcr, false)
            .unwrap_err()
            .to_string();
        assert!(e.contains("has no PCR in its first 128 MB"), "{e}");
        // a pipe (stdin): decided by its first PCR, or paced with `pcr`
        assert_eq!(
            plan(&one("-"), Feed::File, TsPacing::Auto, false).unwrap(),
            [Placement::FirstPcr]
        );
        assert_eq!(
            plan(&one("-"), Feed::File, TsPacing::Pcr, false).unwrap(),
            [Placement::Pcr(None)]
        );
        // --feed live: decided by its first PCR (no probe), or paced from
        // whenever it comes with `pcr`; `arrival` stays arrival
        for path in [&cbr, &plain, "-"] {
            assert_eq!(
                plan(&one(path), Feed::Live, TsPacing::Auto, false).unwrap(),
                [Placement::FirstPcr]
            );
            assert_eq!(
                plan(&one(path), Feed::Live, TsPacing::Pcr, false).unwrap(),
                [Placement::Pcr(None)]
            );
            assert_eq!(
                plan(&one(path), Feed::Live, TsPacing::Arrival, false).unwrap(),
                [Placement::Arrival]
            );
        }
        // 3.4 Mbit/s into 2 slots (2.17 Mbit/s): refused, naming the slots
        // it needs, unless --allow-overrate; synthetic TSs are not planned
        let two = layout_of(&[format!("0x4010:2={cbr}"), "0x4011:46".into()]);
        let e = plan(&two, Feed::File, TsPacing::Auto, false)
            .unwrap_err()
            .to_string();
        assert!(
            e.contains("TS_ID 0x4010") && e.contains("its 2 slots per frame carry 2.17 Mbit/s"),
            "{e}"
        );
        assert!(
            e.contains("it needs 4 sent slots per frame") && e.contains("--allow-overrate"),
            "{e}"
        );
        assert_eq!(
            plan(&two, Feed::File, TsPacing::Auto, true).unwrap(),
            [Placement::Pcr(pcr), Placement::Arrival]
        );
        std::fs::remove_dir_all(&d).unwrap();
    }

    /// `--ts-pacing arrival` (and a file without PCR under `auto`) is the
    /// old rule: every input packet, nulls included and PCRs untouched, in
    /// the next slot, then null padding.
    #[test]
    fn arrival_reproduces_the_input_bytes() {
        let d = scratch("arrival");
        let input = cbr_ts(1000, 4061, 77);
        let path = d.join("cbr.ts");
        std::fs::write(&path, input.concat()).unwrap();
        let layout = layout_of(&[format!("0x4010={}", path.display())]);
        let plan = plan_inputs(&layout, Feed::File, TsPacing::Arrival, false).unwrap();
        let mut inp = TsInputs::open(&layout, Feed::File, &[384], &plan).unwrap();
        assert!(!inp.describe().contains("paced"));
        let (out, n) = drain(&mut inp, &[384]);
        assert_eq!(n, 3, "1000 packets in 384-packet superframes");
        assert_eq!(out[0][..1000], input[..]);
        assert!(out[0][1000..].iter().all(|p| *p == null_packet()));
        assert!(!inp.status().contains("PCR lag"));
        std::fs::remove_dir_all(&d).unwrap();
    }

    /// A pipe is decided by its first PCR: the packets read ahead to find
    /// it are kept (before the first PCR they take the first slots); a
    /// pipe without PCR keeps today's arrival order.
    #[test]
    fn a_pipe_is_paced_from_its_first_pcr() {
        let stop = AtomicBool::new(false);
        let (tx, rx) = sync_channel(4096);
        let mut input: Vec<Packet> = (0..5).map(|i| pkt(0x0111, i)).collect();
        input.extend(cbr_ts(2000, 4061, 0));
        for p in &input {
            tx.send(*p).unwrap();
        }
        drop(tx);
        let s = PacketStream::from_receiver("-", rx, Feed::File);
        let mut inp = TsInputs::from_streams(
            vec![0x4010],
            vec![Some((s, Placement::FirstPcr))],
            Feed::File,
        );
        assert!(inp.describe().contains("<- - (paced by PCR if one comes)"));
        let (out, _) = drain(&mut inp, &[384]);
        assert!(inp.describe().contains("<- - (paced by PCR)"));
        let placed: Vec<Packet> = out[0].iter().filter(|p| !is_null(p)).copied().collect();
        let want = input.iter().filter(|p| !is_null(p)).count();
        assert_eq!(placed.len(), want);
        assert_eq!(placed[..5], input[..5], "read ahead, placed first");
        assert_eq!(out[0][..5], input[..5]);
        // 2000 packets at 4061 ticks: 0.3 s of slots, not 2005 slots
        assert!(
            out[0].len() >= 2000 * 4061 * 37 / 28_800,
            "{}",
            out[0].len()
        );
        assert!(
            inp.status().contains(" | 0x4010 PCR lag 0.00 s"),
            "{}",
            inp.status()
        );
        // no PCR: arrival order, the packets read ahead first
        let (tx, rx) = sync_channel(64);
        let plain: Vec<Packet> = (0..50).map(|i| pkt(0x0111, i)).collect();
        for p in &plain {
            tx.send(*p).unwrap();
        }
        drop(tx);
        let s = PacketStream::from_receiver("-", rx, Feed::File);
        let mut inp = TsInputs::from_streams(
            vec![0x4010],
            vec![Some((s, Placement::FirstPcr))],
            Feed::File,
        );
        let p = inp.take(&[40], &stop).unwrap();
        assert_eq!((p.ts[0].as_slice(), p.real[0]), (&plain[..40], 40));
        let p = inp.take(&[40], &stop).unwrap();
        assert_eq!(p.ts[0][..10], plain[40..]);
        assert_eq!(p.real, [10]);
        assert!(inp.take(&[40], &stop).is_none());
        assert!(!inp.describe().contains("paced"));
    }

    /// `--feed live` with a PCR: paced live (the anchor `LIVE_LATENCY`
    /// after the first PCR, so the first superframes are nulls); at the
    /// writer's end every packet has gone out at its own slot and the
    /// status reports the live input. Everything is queued and the sender
    /// closed, so nothing waits on the wall clock.
    #[test]
    fn a_live_input_is_paced_from_its_first_pcr() {
        let input = cbr_ts(3000, 4061, 0);
        for place in [Placement::FirstPcr, Placement::Pcr(None)] {
            let (tx, rx) = sync_channel(4096);
            for p in &input {
                tx.send(*p).unwrap();
            }
            drop(tx);
            let s = PacketStream::from_receiver("fifo", rx, Feed::Live);
            let mut inp = TsInputs::from_streams(vec![0x4010], vec![Some((s, place))], Feed::Live);
            let (out, _) = drain(&mut inp, &[384]);
            assert!(
                inp.describe()
                    .contains("<- fifo (paced by PCR, live) (feed Live)"),
                "{}",
                inp.describe()
            );
            // ⌈0.3 s / one superframe⌉ = 28 superframes of nulls first
            assert!(out[0][..28 * 384].iter().all(is_null));
            assert_eq!(out[0][28 * 384], input[0], "the first PCR, Δ = 0");
            let placed = out[0].iter().filter(|p| !is_null(p)).count();
            assert_eq!(placed, input.iter().filter(|p| !is_null(p)).count());
            let st = inp.status();
            assert!(
                st.contains(" | 0x4010 PCR lag 0.00 s (max 0.00)")
                    && st.ends_with(" deadline-misses 0"),
                "{st}"
            );
        }
    }

    /// `--feed live` without a PCR in its first second (here: in the
    /// sniff's wall time, shortened) or before the writer closes: today's
    /// arrival order, the packets read while looking kept in front.
    #[test]
    fn live_without_pcr_falls_back() {
        let stop = AtomicBool::new(false);
        let plain: Vec<Packet> = (0..50).map(|i| pkt(0x0111, i)).collect();
        // the writer closes: the end of input decides
        let (tx, rx) = sync_channel(64);
        for p in &plain {
            tx.send(*p).unwrap();
        }
        drop(tx);
        let s = PacketStream::from_receiver("fifo", rx, Feed::Live);
        let mut inp = TsInputs::from_streams(
            vec![0x4010],
            vec![Some((s, Placement::FirstPcr))],
            Feed::Live,
        );
        let p = inp.take(&[40], &stop).unwrap();
        assert_eq!((p.ts[0].as_slice(), p.real[0]), (&plain[..40], 40));
        assert!(!inp.describe().contains("paced"));
        // the writer stays open: the sniff's time runs out (shortened from
        // LIVE_SNIFF); then the live feed pads with nulls, never blocks
        let (tx, rx) = sync_channel(64);
        for p in &plain {
            tx.send(*p).unwrap();
        }
        let s = PacketStream::from_receiver("fifo", rx, Feed::Live);
        let mut inp = TsInputs::from_streams(
            vec![0x4010],
            vec![Some((s, Placement::FirstPcr))],
            Feed::Live,
        );
        inp.set_live_sniff(Duration::from_millis(20));
        let p = inp.take(&[40], &stop).unwrap();
        assert_eq!(p.ts[0], plain[..40]);
        let p = inp.take(&[40], &stop).unwrap();
        assert_eq!((&p.ts[0][..10], p.real[0]), (&plain[40..], 10));
        assert!(p.ts[0][10..].iter().all(is_null));
        assert!(!inp.describe().contains("paced"));
        tx.send(pkt(0x0111, 99)).unwrap();
        let p = inp.take(&[40], &stop).unwrap();
        assert_eq!((p.ts[0][0], p.real[0]), (pkt(0x0111, 99), 1));
        drop(tx);
        assert!(inp.take(&[40], &stop).is_none());
    }

    /// The paced TS of a CBR file, as `--placed-ts` writes it, is the
    /// constant-rate TS of its slots: `isdb ts validate` finds no PCR
    /// fault and no repetition fault, and `inspect` measures the slots'
    /// 52.17 Mbit/s within 1 ppm from the restamped PCRs.
    #[test]
    fn placed_ts_of_a_cbr_file_validates() {
        use isdb_analyze::validate::{Severity, validate};
        let d = scratch("placed");
        // 3 s at 9.999 Mbit/s (4 061 ticks per packet), PCRs from 1 000 000
        let n = 3 * 27_000_000 / 4061;
        let input = cbr_ts(n, 4061, 1_000_000);
        let path = d.join("cbr.ts");
        std::fs::write(&path, input.concat()).unwrap();
        let layout = layout_of(&[format!("0x4010={}", path.display())]);
        let plan = plan_inputs(&layout, Feed::File, TsPacing::Auto, false).unwrap();
        assert_eq!(plan, [Placement::Pcr(Pid::new(0x01FF))]);
        let mut inp = TsInputs::open(&layout, Feed::File, &[384], &plan).unwrap();
        assert!(
            inp.describe().contains("(paced by PCR)"),
            "{}",
            inp.describe()
        );
        let template = d.join("placed-{ts_id}.ts").display().to_string();
        let mut w = PlacedTs::create(&template, &[0x4010]).unwrap();
        assert!(w.paths()[0].ends_with("placed-4010.ts"));
        let stop = AtomicBool::new(false);
        let mut sfs = 0;
        while let Some(p) = inp.take(&[384], &stop) {
            w.write(&p.ts).unwrap();
            sfs += 1;
        }
        w.finish().unwrap();
        // 3 s of PCR time in 11.07 ms superframes
        assert!((271..=272).contains(&sfs), "{sfs} superframes");
        let st = inp.status();
        assert!(st.contains(" | 0x4010 PCR lag 0.00 s (max 0.00)"), "{st}");
        assert!(st.contains("(cap 52.17)") && !st.contains("OVER"), "{st}");
        let bytes = std::fs::read(d.join("placed-4010.ts")).unwrap();
        let packets: Vec<Packet> = bytes.as_chunks::<188>().0.to_vec();
        assert_eq!(packets.len(), sfs * 384);
        // every non-null input packet once, in order; only PCRs changed
        let got: Vec<&Packet> = packets.iter().filter(|p| !is_null(p)).collect();
        let want: Vec<&Packet> = input.iter().filter(|p| !is_null(p)).collect();
        assert_eq!(got.len(), want.len());
        for (g, w) in got.iter().zip(&want) {
            if crate::source::pid(w) != 0x01FF {
                assert_eq!(g, w);
            }
        }
        let cap = isdb_analyze::scan_packets(&packets, &Default::default());
        // `isdb ts validate --expect-rate 52170000:1`
        let opts = isdb_analyze::validate::ValidateOptions {
            expect: isdb_analyze::validate::Expectations {
                rate: Some(isdb_analyze::validate::RateExpectation {
                    bps: 52_170_000.0,
                    ppm: 1.0,
                }),
                ..Default::default()
            },
            nominal_bps: None,
        };
        let rep = validate(&cap, &opts);
        assert!(
            rep.findings
                .iter()
                .any(|f| f.check == "pcr.rate" && f.severity == Severity::Info),
            "{:?}",
            rep.findings
        );
        for f in &rep.findings {
            assert!(
                !(f.check.starts_with("pcr.") && f.severity > Severity::Info),
                "{f:?}"
            );
            assert_ne!(f.check, "tables.frequency", "{f:?}");
        }
        let ins = isdb_analyze::inspect::inspect(&cap, &Default::default());
        let rate = ins.rate_bps.unwrap();
        assert!((rate / 52_170_000.0 - 1.0).abs() < 1e-6, "{rate} bit/s");
        let pcr = &ins.pcr[0];
        assert!(pcr.fit_max_ns.unwrap() < 40.0, "{pcr:?}");
        std::fs::remove_dir_all(&d).unwrap();
    }

    /// `--placed-ts` needs `{ts_id}` with more than one TS.
    #[test]
    fn placed_ts_template() {
        let e = PlacedTs::create("/nonexistent/p.ts", &[1, 2])
            .unwrap_err()
            .to_string();
        assert!(e.contains("put {ts_id} in the path"), "{e}");
        let d = scratch("template");
        let one = d.join("p.ts").display().to_string();
        assert_eq!(PlacedTs::create(&one, &[0x4010]).unwrap().paths(), [one]);
        std::fs::remove_dir_all(&d).unwrap();
    }

    /// A paced TS file gives the CPU path (IQ samples) and the FPGA path
    /// (symbol codes) the same packets: placement happens before
    /// `Modulate`.
    #[test]
    fn paced_inputs_feed_both_paths_the_same_packets() {
        let d = scratch("paths");
        let path = d.join("cbr.ts");
        // 0.1 s of a 9.999 Mbit/s TS: 9 superframes of slots
        std::fs::write(&path, cbr_ts(665, 4061, 5_000).concat()).unwrap();
        let layout = layout_of(&[format!("0x4010={}", path.display())]);
        let plan = plan_inputs(&layout, Feed::File, TsPacing::Auto, false).unwrap();
        let sys = isdbs_phy::frontend::system();
        type Item = (Vec<Vec<Packet>>, Vec<usize>, bool);
        let run = |m: Modulate| {
            let cfg = SatConfig {
                lead: 1,
                flush: true,
                max_superframes: Some(5),
                pace: false,
                record: true,
                depth: 2,
                placed: None,
            };
            let input = TsInputs::open(&layout, Feed::File, &[384], &plan).unwrap();
            let tx = Transmitter::new(layout.info.clone()).unwrap();
            let (rx, h) = spawn(cfg, tx, m, input, Arc::new(AtomicBool::new(false))).unwrap();
            let items: Vec<Item> = rx
                .iter()
                .filter(|f| f.kind == FrameKind::Data)
                .map(|f| {
                    let codes = matches!(f.payload, Payload::Codes(_));
                    (f.packets.as_ref().unwrap().to_vec(), f.real.clone(), codes)
                })
                .collect();
            assert_eq!(h.join().unwrap().unwrap(), EndReason::Limit);
            items
        };
        let cpu = run(Modulate::shaped(
            Shaper::new(40e6, sys.symbol_rate_hz, sys.roll_off).unwrap(),
        ));
        let fpga = run(Modulate::codes());
        assert_eq!(cpu.len(), 4);
        assert!(cpu.iter().all(|x| !x.2) && fpga.iter().all(|x| x.2));
        let packets = |v: &[Item]| {
            v.iter()
                .map(|x| (x.0.clone(), x.1.clone()))
                .collect::<Vec<_>>()
        };
        assert_eq!(packets(&cpu), packets(&fpga));
        // paced: about 3.4 / 52.17 of each superframe is input
        for (_, real, _) in &cpu {
            assert!((15..=35).contains(&real[0]), "{real:?}");
        }
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn producer_sequence_and_sample_counts() {
        let sat = parse(&[
            "--system",
            "isdbs",
            "--iq",
            "x",
            "--combination",
            "qpsk3/4:48",
        ])
        .unwrap()
        .0
        .sat;
        let layout = Layout::of(&sat).unwrap();
        let tx = Transmitter::new(layout.info.clone()).unwrap();
        let sys = isdbs_phy::frontend::system();
        let shaper = Shaper::new(40e6, sys.symbol_rate_hz, sys.roll_off).unwrap();
        let reference = shaper.clone();
        let input = TsInputs::open(&layout, Feed::File, &[288], &[Placement::Arrival]).unwrap();
        let cfg = SatConfig {
            lead: 1,
            flush: true,
            max_superframes: Some(3),
            pace: false,
            record: true,
            depth: 2,
            placed: None,
        };
        let (rx, h) = spawn(
            cfg,
            tx,
            Modulate::shaped(shaper),
            input,
            Arc::new(AtomicBool::new(false)),
        )
        .unwrap();
        let items: Vec<ProducedSf> = rx.iter().collect();
        assert_eq!(h.join().unwrap().unwrap(), EndReason::Limit);
        let kinds: Vec<FrameKind> = items.iter().map(|f| f.kind).collect();
        assert_eq!(
            kinds,
            [
                FrameKind::Lead,
                FrameKind::Data,
                FrameKind::Data,
                FrameKind::Flush
            ]
        );
        let samples = |f: &ProducedSf| match &f.payload {
            Payload::Samples(x) => x.clone(),
            Payload::Codes(_) => panic!("codes from the shaped path"),
        };
        // the samples of the pre-roll frame, 3 superframes, the flush frame
        // and the tail, exactly; the pre-roll goes out with the first item
        let total: usize = items.iter().map(|f| samples(f).len()).sum();
        let symbols = 3 * SUPERFRAME_SYMBOLS + 2 * FRAME_SYMBOLS + 2 * reference.span() - 1;
        assert_eq!(total as u64, reference.samples_for(symbols as u64));
        let first = reference.samples_for((FRAME_SYMBOLS + SUPERFRAME_SYMBOLS) as u64);
        assert_eq!(samples(&items[0]).len() as u64, first);
        assert!(
            items[1..3]
                .iter()
                .all(|f| (442_810..=442_811).contains(&samples(f).len()))
        );
        // unit RMS
        let x = samples(&items[1]);
        let p: f64 = x.iter().map(|c| f64::from(c.norm_sqr())).sum::<f64>() / x.len() as f64;
        assert!((p - 1.0).abs() < 0.01, "power {p}");
        assert!((items[1].payload.seconds(40e6) - superframe_seconds()).abs() < 1e-7);
        assert_eq!(
            (items[2].real.clone(), items[2].nulls.clone()),
            (vec![288], vec![0])
        );
        // packets recorded: nulls in the lead, the synthetic data (input
        // superframe 1), none for the flush
        let lead = items[0].packets.as_ref().unwrap();
        assert!(lead.iter().flatten().all(is_null) && lead[0].len() == 288);
        let d = items[2].packets.as_ref().unwrap();
        // continuity counter (288 + 5) mod 16 = 5
        assert_eq!(d[0][5], synthetic_packet(DEFAULT_TS_ID, 1, 5, 5));
        assert!(items[3].packets.is_none());
        assert_eq!(
            (items[0].real.clone(), items[3].real.clone()),
            (vec![0], vec![0])
        );
    }

    #[test]
    fn sidecar_names_the_system_and_the_channel() {
        let sat = parse(&["--system", "isdbs", "--iq", "x"]).unwrap().0.sat;
        let layout = Layout::of(&sat).unwrap();
        let m = metadata(
            IqFormat::Cf32,
            40e6,
            10.0,
            &layout,
            Some(Channel::Bs(1)),
            None,
        )
        .unwrap();
        let back = Meta::from_json(&m.to_json()).unwrap();
        assert_eq!(back.format, isdb_iq::SampleFormat::Cf32);
        assert_eq!(back.sample_rate, 40e6);
        assert_eq!(back.global_extra["isdb:system"], json!("ISDB-S"));
        let d = back.description.unwrap();
        assert!(
            d.starts_with("isdb tx ISDB-S: TC8PSK 2/3 ×48; TS_ID 0x4010 48 slots"),
            "{d}"
        );
        assert!(back.recorder.unwrap().starts_with("isdb tx "));
        assert_eq!(back.captures.len(), 1);
        let c = &back.captures[0];
        assert_eq!((c.sample_start, c.frequency), (0, Some(1_049_480_000.0)));
        assert_eq!(c.extra["isdb:channel"], json!("BS1"));
        assert_eq!(c.extra["isdb:polarisation"], json!("right"));
        let m = metadata(
            IqFormat::Cs8,
            61.44e6,
            12.0,
            &layout,
            None,
            Some(1_200_000_000),
        )
        .unwrap();
        assert_eq!(m.format, isdb_iq::SampleFormat::Cs8);
        assert_eq!(m.captures[0].frequency, Some(1.2e9));
        assert!(m.captures[0].extra.is_empty());
        let m = metadata(IqFormat::Cf32, 40e6, 10.0, &layout, None, None).unwrap();
        assert_eq!(m.captures[0].frequency, None);
    }

    /// The BS layout's producer over synthetic packets: `superframes`
    /// lead and data superframes, the flush, packets recorded.
    fn bs_producer(
        m: Modulate,
        superframes: u64,
        depth: usize,
    ) -> (Receiver<ProducedSf>, JoinHandle<Result<EndReason>>) {
        let sat = parse(&["--system", "isdbs", "--iq", "x"]).unwrap().0.sat;
        let layout = Layout::of(&sat).unwrap();
        let tx = Transmitter::new(layout.info.clone()).unwrap();
        let input = TsInputs::open(&layout, Feed::File, &[384], &[Placement::Arrival]).unwrap();
        let cfg = SatConfig {
            lead: 1,
            flush: true,
            max_superframes: Some(superframes),
            pace: false,
            record: true,
            depth,
            placed: None,
        };
        spawn(cfg, tx, m, input, Arc::new(AtomicBool::new(false))).unwrap()
    }

    fn codes_of(f: &ProducedSf) -> &[u8] {
        match &f.payload {
            Payload::Codes(c) => c,
            Payload::Samples(_) => panic!("samples from the code path"),
        }
    }

    /// The code path carries the transmitter's symbols, in the sample
    /// path's sequence (pre-roll frame with the lead, the data, one null
    /// frame), as TX boundary codes, and nothing else.
    #[test]
    fn the_code_path_carries_the_transmitters_symbols() {
        let (rx, h) = bs_producer(Modulate::codes(), 3, 2);
        let items: Vec<ProducedSf> = rx.iter().collect();
        assert_eq!(h.join().unwrap().unwrap(), EndReason::Limit);
        let kinds: Vec<FrameKind> = items.iter().map(|f| f.kind).collect();
        assert_eq!(
            kinds,
            [
                FrameKind::Lead,
                FrameKind::Data,
                FrameKind::Data,
                FrameKind::Flush
            ]
        );
        let lens: Vec<usize> = items.iter().map(|f| codes_of(f).len()).collect();
        assert_eq!(
            lens,
            [
                FRAME_SYMBOLS + SUPERFRAME_SYMBOLS,
                SUPERFRAME_SYMBOLS,
                SUPERFRAME_SYMBOLS,
                FRAME_SYMBOLS
            ]
        );
        assert!((items[1].payload.seconds(40e6) - superframe_seconds()).abs() < 1e-12);
        // the same superframes from a second transmitter, as points
        let sat = parse(&["--system", "isdbs", "--iq", "x"]).unwrap().0.sat;
        let mut tx = Transmitter::new(Layout::of(&sat).unwrap().info).unwrap();
        let mut want: Vec<Cplx> =
            tx.null_superframe().symbols[(FRAMES - 1) * FRAME_SYMBOLS..].to_vec();
        want.extend(tx.null_superframe().symbols);
        for f in &items[1..3] {
            let p = f.packets.as_ref().unwrap();
            let refs: Vec<&[Packet]> = p.iter().map(Vec::as_slice).collect();
            want.extend(tx.superframe(&refs).unwrap().symbols);
        }
        want.extend(&tx.null_superframe().symbols[..FRAME_SYMBOLS]);
        let got: Vec<u8> = items.iter().flat_map(|f| codes_of(f).to_vec()).collect();
        assert_eq!(got.len(), want.len());
        for (k, (&c, z)) in got.iter().zip(&want).enumerate() {
            assert!(
                isdb_frontend::txformat::angle(c).is_some(),
                "symbol {k}: code {c} is no point"
            );
            let d = isdb_frontend::txformat::point(c) - z;
            assert!(d.norm() < 1e-12, "symbol {k}: code {c} for {z}");
        }
    }

    /// The code path into a radio's queued sink (a mock consuming DMA
    /// samples of eight codes): what reaches the radio unpacks to the
    /// produced codes, and the report counts codes and their air time.
    #[test]
    fn codes_reach_the_radio_packed() {
        use isdb_radio::mock::MockRadio;
        use isdb_radio::{CodeEncoder, Cs12, QueuedSink, TxOptions};
        let (rx, h) = bs_producer(Modulate::codes(), 3, 2);
        let produced: Vec<u8> = rx.iter().flat_map(|f| codes_of(&f).to_vec()).collect();
        h.join().unwrap().unwrap();
        let radio = MockRadio::<Cs12>::new(50e6);
        let sent = radio.sent.clone();
        let opts = TxOptions {
            prefill_frames: 2,
            frame_seconds: superframe_seconds(),
            backoff_db: 10.0,
        };
        let mut sink = TxSink::Codes(Box::new(
            QueuedSink::with_encoder(radio, &opts, CodeEncoder::new()).unwrap(),
        ));
        assert!(sink.realtime());
        let (items, worker) = bs_producer(
            sink.modulate(isdb_frontend::shape::Shaper::new(40e6, 28.86e6, 0.35).unwrap()),
            3,
            2,
        );
        let rep = drive(
            &[DEFAULT_TS_ID],
            40e6,
            items,
            worker,
            &mut sink,
            0.0,
            &AtomicBool::new(false),
        )
        .unwrap();
        assert_eq!((rep.items, rep.lead, rep.data, rep.flush), (4, 1, 2, 1));
        assert_eq!((rep.samples, rep.codes), (0, produced.len() as u64));
        let rs = isdbs_phy::frontend::system().symbol_rate_hz;
        assert!((rep.on_air_s - produced.len() as f64 / rs).abs() < 1e-9);
        assert!(rep.sink.contains("queue drained"), "{}", rep.sink);
        assert!(
            rep.sink
                .contains(&format!("{} symbol codes in", produced.len())),
            "{}",
            rep.sink
        );
        let sent = sent.lock().unwrap();
        let words: Vec<u32> = sent
            .as_chunks::<2>()
            .0
            .iter()
            .map(|&c| isdb_frontend::txformat::from_components(c))
            .collect();
        assert_eq!(isdb_frontend::txformat::unpack(&words), produced);
        // a payload of the other kind is refused
        let mut iq = TxSink::Iq(Box::new(isdb_radio::mock::MemorySink::default()));
        let e = iq
            .write(&Payload::Codes(vec![8]), &AtomicBool::new(false))
            .unwrap_err()
            .to_string();
        assert!(e.contains("symbol codes for a sink of IQ samples"), "{e}");
    }

    /// `--offload`: auto by default; `fpga` with `--iq` is refused before
    /// anything is written; `cpu` and `auto` with `--iq` are the CPU's
    /// samples (here the dry run's refusal); with `--radio` and
    /// `--dry-run` the device is not opened (`fpga` describes the codes,
    /// `auto` both paths), and the gate refuses.
    #[test]
    fn offload_choices() {
        let (a, _) = parse(&["--system", "isdbs", "--iq", "x"]).unwrap();
        assert_eq!(a.offload, OffloadArg::Auto);
        let open = |args: &[&str]| -> Result<TxSink> {
            let (a, _) = parse(args).unwrap();
            open_sink(
                &a,
                40e6,
                40_000_000,
                superframe_seconds(),
                true,
                1.0,
                |_| panic!("no file is created"),
            )
        };
        let e = open(&["--system", "isdbs", "--iq", "x", "--offload", "fpga"])
            .map(|_| ())
            .unwrap_err()
            .to_string();
        assert!(
            e.starts_with("--offload fpga: the FPGA transmit block is a radio's"),
            "{e}"
        );
        for offload in ["cpu", "auto"] {
            let e = open(&[
                "--system",
                "isdbs",
                "--iq",
                "x",
                "--offload",
                offload,
                "--dry-run",
            ])
            .map(|_| ())
            .unwrap_err()
            .to_string();
            assert_eq!(e, "dry run: nothing written", "{offload}");
        }
        if cfg!(feature = "pluto") {
            for offload in ["fpga", "auto"] {
                let e = open(&[
                    "--system",
                    "isdbs",
                    "--radio",
                    "pluto:uri=ip:192.0.2.1",
                    "--channel",
                    "BS1",
                    "--freq-unrestricted",
                    "--offload",
                    offload,
                    "--dry-run",
                ])
                .map(|_| ())
                .unwrap_err()
                .to_string();
                assert!(e.starts_with("dry run, not transmitting"), "{offload}: {e}");
            }
        }
        // the parameters of the block: the FPGA's geometry at the backoff
        let p = block_params(40e6, 10.0).unwrap();
        assert_eq!(p.id_word(), 0xB5D1_1918);
        assert!(block_params(30e6, 10.0).is_err());
    }

    /// The producer's speed on both paths, release build: `cargo test
    /// --release -p isdb-cli --bin isdb producer_speed_of_both_paths --
    /// --ignored --nocapture`. Busy time (input and modulation per
    /// superframe, as the status lines report it) and wall time with a
    /// consumer that only drops the items; the BS layout, synthetic
    /// packets.
    #[test]
    #[ignore = "a measurement; run in release"]
    fn producer_speed_of_both_paths() {
        let n = 200;
        let sys = isdbs_phy::frontend::system();
        for (name, m) in [
            (
                "IQ samples (shaper)",
                Modulate::shaped(Shaper::new(40e6, sys.symbol_rate_hz, sys.roll_off).unwrap()),
            ),
            ("symbol codes", Modulate::codes()),
        ] {
            let t = Instant::now();
            let (rx, h) = bs_producer(m, n, 8);
            let (mut items, mut busy) = (0u64, Duration::ZERO);
            for f in rx.iter().filter(|f| f.kind == FrameKind::Data) {
                busy += f.input_time + f.modulate_time;
                items += 1;
            }
            h.join().unwrap().unwrap();
            let wall = t.elapsed().as_secs_f64();
            let speed = producer_speed(items, busy, superframe_seconds()).unwrap();
            println!(
                "{name}: {items} data superframes, {:.2} ms each, {speed:.2}× real time (wall {:.2}× with the lead and flush)",
                busy.as_secs_f64() * 1e3 / items as f64,
                (n + 1) as f64 * superframe_seconds() / wall
            );
        }
    }
}
