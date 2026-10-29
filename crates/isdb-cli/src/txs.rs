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
//! The samples are unit RMS (radios quantise them at `--backoff-db`); an
//! `--iq` file holds them at `--backoff-db` below full scale in every
//! format, cf32 included, because the ISDB-S receiver reads samples as a
//! fraction of full scale (its front end's input is 12-bit).

use crate::bands::Channel;
use crate::producer::{EndReason, FrameKind};
use crate::sink::{FileSink, IqFormat};
use crate::source::{
    Feed, Packet, PacketStream, check_feed, null_packet, path_opener, queue_depth,
};
use crate::tx::{TxArgs, describe_speed, ms_per, producer_speed, ratio};
use anyhow::{Context, Result, bail};
use clap::Args;
use isdb_frontend::shape::Shaper;
use isdb_iq::sigmf::{Meta, Segment};
use isdb_radio::{C32, IqSink};
use isdbs_core::combination::Combination;
use isdbs_core::frame::{FRAME_SYMBOLS, FRAMES};
use isdbs_core::tmcc::TmccInfo;
use isdbs_phy::Cplx;
use isdbs_phy::tx::{SUPERFRAME_SYMBOLS, Transmitter};
use serde_json::json;
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

/// The TS_ID of the default `--ts` (a placeholder, not a station's).
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
    "allow_overrate",
    "iq_in",
    "iq_in_format",
    "lead_frames",
    "seek_step",
];

/// `isdb tx` options that only ISDB-S uses.
pub const TX_ISDBS_ONLY: &[&str] = &["combination", "ts", "rate", "lead_superframes"];

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

/// Where a TS's packets come from.
enum Source {
    /// [`synthetic_packet`]s.
    Synthetic,
    /// A TS file (or FIFO, stdin) read by its own thread.
    File(PacketStream),
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
}

impl TsInputs {
    /// Open the inputs of `layout` with `feed` (`per_superframe` packets of
    /// each relative TS size the read-ahead queues).
    pub fn open(layout: &Layout, feed: Feed, per_superframe: &[usize]) -> Result<Self> {
        let mut sources = Vec::new();
        let mut names = Vec::new();
        for (t, &n) in layout.ts.iter().zip(per_superframe) {
            match &t.path {
                Some(p) => {
                    check_feed(p, feed)?;
                    sources.push(Source::File(PacketStream::spawn(
                        p,
                        path_opener(p),
                        feed,
                        queue_depth(n, superframe_seconds()),
                    )?));
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
            sources,
            names,
            feed,
            superframe: 0,
        })
    }

    /// In-memory streams for the TSs (tests): `None` = synthetic.
    #[cfg(test)]
    fn from_streams(ts_ids: Vec<u16>, streams: Vec<Option<PacketStream>>, feed: Feed) -> Self {
        let names = streams
            .iter()
            .map(|s| s.as_ref().map_or("synthetic".into(), |s| s.name.clone()))
            .collect();
        let sources: Vec<Source> = streams
            .into_iter()
            .map(|s| s.map_or(Source::Synthetic, Source::File))
            .collect();
        Self {
            ts_ids,
            sent: vec![0; sources.len()],
            sources,
            names,
            feed,
            superframe: 0,
        }
    }

    /// Packets for one superframe, `counts[r]` of relative TS r (file TSs
    /// padded with null packets). `None` when stopped, or once every file
    /// TS has ended and none gave a packet (synthetic TSs then end with
    /// them; with synthetic TSs only, never).
    pub fn take(&mut self, counts: &[usize], stop: &AtomicBool) -> Option<SfPackets> {
        let sf = self.superframe;
        let mut ts = Vec::with_capacity(counts.len());
        let mut real = Vec::with_capacity(counts.len());
        let mut file_real = 0;
        for (r, src) in self.sources.iter_mut().enumerate() {
            let n = counts[r];
            let mut v = Vec::with_capacity(n);
            match src {
                Source::Synthetic => {
                    for i in 0..n {
                        // a TS takes at most 48 × 8 packets per superframe
                        let index = u32::try_from(i).unwrap_or(u32::MAX);
                        let cc = u8::try_from(self.sent[r] & 15).unwrap_or(0);
                        v.push(synthetic_packet(self.ts_ids[r], sf, index, cc));
                        self.sent[r] += 1;
                    }
                }
                Source::File(s) => {
                    while v.len() < n {
                        match s.next(stop) {
                            Some(p) => v.push(p),
                            None => break,
                        }
                    }
                    file_real += v.len();
                }
            }
            real.push(v.len());
            v.resize(n, null_packet());
            ts.push(v);
        }
        let mut files = self.sources.iter().filter_map(|s| match s {
            Source::File(f) => Some(f),
            Source::Synthetic => None,
        });
        let any_file = files.clone().next().is_some();
        let all_ended = files.all(PacketStream::ended);
        if (any_file && all_ended && file_real == 0) || stop.load(Ordering::Relaxed) {
            return None;
        }
        self.superframe += 1;
        Some(SfPackets { ts, real })
    }

    /// `TS_ID 0x4010 <- a.ts, TS_ID 0x4011 <- synthetic (feed File)`.
    pub fn describe(&self) -> String {
        let v: Vec<String> = self
            .ts_ids
            .iter()
            .zip(&self.names)
            .map(|(id, n)| format!("TS_ID {id:#06x} <- {n}"))
            .collect();
        format!("{} (feed {:?})", v.join(", "), self.feed)
    }

    /// Reader counters for the status lines (may be empty).
    pub fn status(&self) -> String {
        let (mut skipped, mut rewinds) = (0, 0);
        for s in &self.sources {
            if let Source::File(f) = s {
                skipped += f.stats.skipped_bytes.load(Ordering::Relaxed);
                rewinds += f.stats.rewinds.load(Ordering::Relaxed);
            }
        }
        let mut s = String::new();
        if skipped > 0 {
            s += &format!(" resync-skipped {skipped} B");
        }
        if rewinds > 0 {
            s += &format!(" rewinds {rewinds}");
        }
        s
    }
}

/// One produced superframe (or the flush: a null frame and the shaper's
/// tail).
pub struct ProducedSf {
    /// Role.
    pub kind: FrameKind,
    /// Its samples at the output rate, unit RMS (442 810 or 442 811 per
    /// superframe at 40 Msps).
    pub samples: Vec<C32>,
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
    /// Time spent in the transmitter and the shaper.
    pub modulate_time: Duration,
}

/// Producer settings.
#[derive(Debug, Clone)]
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

/// Start the producer thread over `tx` and `shaper` (fresh, built by the
/// caller so that their errors come before any output). Fails only when
/// the OS refuses the thread.
pub fn spawn(
    cfg: SatConfig,
    mut tx: Transmitter,
    mut shaper: Shaper,
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
            let mut preroll = Vec::new();
            let s = tx.null_superframe();
            shaper.push(
                &to_c32(&s.symbols[(FRAMES - 1) * FRAME_SYMBOLS..]),
                &mut preroll,
            );
            let mut index = 0u64;
            for _ in 0..cfg.lead {
                if stop.load(Ordering::Relaxed) {
                    return Ok(EndReason::Stopped);
                }
                pace(index);
                let tm = Instant::now();
                let s = tx.null_superframe();
                let mut samples = std::mem::take(&mut preroll);
                shaper.push(&to_c32(&s.symbols), &mut samples);
                let f = ProducedSf {
                    kind: FrameKind::Lead,
                    samples,
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
                let s = tx.superframe(&refs)?;
                let mut samples = std::mem::take(&mut preroll);
                shaper.push(&to_c32(&s.symbols), &mut samples);
                let mt = tm.elapsed();
                let f = ProducedSf {
                    kind: FrameKind::Data,
                    samples,
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
            if cfg.flush {
                if stop.load(Ordering::Relaxed) {
                    return Ok(EndReason::Stopped);
                }
                pace(index);
                let tm = Instant::now();
                let s = tx.null_superframe();
                let mut samples = std::mem::take(&mut preroll);
                shaper.push(&to_c32(&s.symbols[..FRAME_SYMBOLS]), &mut samples);
                shaper.finish(&mut samples);
                let f = ProducedSf {
                    kind: FrameKind::Flush,
                    samples,
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
    /// Samples handed to the sink.
    pub samples: u64,
    /// Input packets per relative TS.
    pub real: Vec<u64>,
    /// Null padding per relative TS in data superframes.
    pub nulls: Vec<u64>,
    /// Why the producer ended (None if the sink failed first).
    pub end: Option<EndReason>,
    /// Sink summary.
    pub sink: String,
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
    sink: &mut dyn IqSink,
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
        if let Err(e) = sink.write(&f.samples, stop) {
            aborted = true;
            err = Some(e);
            break;
        }
        rep.items += 1;
        rep.samples += f.samples.len() as u64;
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
            let on_air = rep.samples as f64 / fs;
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
                    "the producer runs below real time ({:.2}×): the radio underruns. Per superframe: input {:.1} ms + modulation and shaping {:.1} ms for a {:.2} ms superframe{}",
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
        return Err(e.into());
    }
    rep.end = Some(end?);
    rep.sink = fin?;
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
    if a.radio.is_some() {
        warn!(
            target: "isdb::tx",
            sample_rate_hz = rate,
            "ISDB-S needs ≥ 38.96 Msps: a Pluto over Gigabit Ethernet streams about 65 MB/s (16 Msps of cs12), so this transmission underruns (the radio counts it) and a HackRF refuses the rate; continuous ISDB-S transmission needs a modulator in the radio's FPGA"
        );
    }
    let bb = if given.has("bb_filter_hz") {
        a.tune.bb_filter_hz
    } else {
        DEFAULT_BB_FILTER_HZ
    };
    let stop = crate::stop::flag();
    let mut sink = crate::tx::open_sink(
        &a,
        sf_s,
        endless,
        seconds,
        |spec| {
            let t = a.tune.satellite_tuning(rate)?;
            Ok((a.tune.config(&t, bb, spec, &a.gain), t))
        },
        |path| {
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
        },
    )?;
    let lead = a.sat.lead_superframes.unwrap_or(1);
    let cfg = SatConfig {
        lead,
        flush: !a.no_flush,
        max_superframes: (seconds > 0.0).then(|| superframes_for_seconds(seconds)),
        pace: a.realtime || crate::producer::default_pace(a.input.feed, sink.realtime()),
        record: false,
        depth: a.queue_frames,
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
        if cfg.flush {
            "a null frame and the shaper's tail"
        } else {
            "none"
        },
        if cfg.pace { ", paced to real time" } else { "" }
    );
    let input = TsInputs::open(&layout, a.input.feed, &counts)?;
    info!(target: "isdb::tx", "input: {}", input.describe());
    let ts_ids = layout.ts_ids();
    let (items, worker) = spawn(cfg, tx, shaper, input, stop.clone())?;
    let rep = drive(
        &ts_ids,
        rate,
        items,
        worker,
        sink.as_mut(),
        a.status_secs,
        &stop,
    )?;
    crate::source::check_not_scrambled()?;
    let end = rep.end.unwrap_or(EndReason::Stopped);
    info!(
        target: "isdb::tx",
        end = ?end,
        superframes = rep.items,
        on_air_s = rep.samples as f64 / rate,
        lead_superframes = rep.lead,
        data_superframes = rep.data,
        flush = rep.flush,
        samples = rep.samples,
        "done ({end:?}): {} superframes = {:.3} s on air ({} lead, {} data, {}), {} samples",
        rep.items,
        rep.samples as f64 / rate,
        rep.lead,
        rep.data,
        if rep.flush > 0 { "flush" } else { "no flush" },
        rep.samples
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
    info!(target: "isdb::tx", "{}", rep.sink);
    Ok(())
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
        let mut inp = TsInputs::from_streams(vec![0x4010, 0x4011], vec![Some(s), None], Feed::File);
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
        let input = TsInputs::open(&layout, Feed::File, &[288]).unwrap();
        let cfg = SatConfig {
            lead: 1,
            flush: true,
            max_superframes: Some(3),
            pace: false,
            record: true,
            depth: 2,
        };
        let (rx, h) = spawn(cfg, tx, shaper, input, Arc::new(AtomicBool::new(false))).unwrap();
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
        // the samples of the pre-roll frame, 3 superframes, the flush frame
        // and the tail, exactly; the pre-roll goes out with the first item
        let total: usize = items.iter().map(|f| f.samples.len()).sum();
        let symbols = 3 * SUPERFRAME_SYMBOLS + 2 * FRAME_SYMBOLS + 2 * reference.span() - 1;
        assert_eq!(total as u64, reference.samples_for(symbols as u64));
        let first = reference.samples_for((FRAME_SYMBOLS + SUPERFRAME_SYMBOLS) as u64);
        assert_eq!(items[0].samples.len() as u64, first);
        assert!(
            items[1..3]
                .iter()
                .all(|f| (442_810..=442_811).contains(&f.samples.len()))
        );
        // unit RMS
        let p: f64 = items[1]
            .samples
            .iter()
            .map(|c| f64::from(c.norm_sqr()))
            .sum::<f64>()
            / items[1].samples.len() as f64;
        assert!((p - 1.0).abs() < 0.01, "power {p}");
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
}
