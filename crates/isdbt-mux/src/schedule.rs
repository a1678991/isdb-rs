//! The multiplexer's timeline: one period of slots, which lane each slot
//! carries, and when each slot (and the PCR byte of its packet) arrives.
//!
//! The [`Multiplexer`](crate::Multiplexer) fills one packet per carried
//! slot and stamps PCR and PTS/DTS from the slot times, so a schedule is
//! all it needs to know about the transmission system:
//!
//! - **ISDB-T** ([`Schedule::ofdm`]): the period is an OFDM frame, the
//!   slots are the multiplex frame of ARIB STD-B31 §3.2.1
//!   ([`MultiplexPattern`]), lanes are the hierarchical layers A/B/C, and
//!   the unit is the 512/63 MHz model-receiver clock of §3.2.2: slot j
//!   starts at clock 408·j, the PCR byte (byte 10, the last bit of
//!   program_clock_reference_base, H.222.0 §2.4.2.2) 20 clocks later
//!   (2 clocks per byte), and units convert to 27 MHz ticks × 27·63/512 =
//!   × 1701/512.
//! - **ISDB-S** ([`Schedule::satellite`]): the period is a superframe of
//!   319 488 symbols (ITU-R BO.1408-1 Table 3), one lane (one relative TS
//!   of the TMCC information; one multiplexer per TS), the slot times from
//!   `isdbs_core::slots::slot_byte_symbol` (the position before the
//!   inter-frame interleaver), and units convert × 450/481 (28.860 Mbaud).
//!
//! Time of slot j of period k: `start_pcr + ⌊(k·period + start[j]) ·
//! num/den⌋` ticks ([`Schedule::slot_time`]).

use isdbs_core::TmccInfo;
use isdbs_core::frame::{
    FRAME_SYMBOLS, FRAMES, SLOTS, SUPERFRAME_SYMBOLS, SYMBOL_RATE, TICKS_PER_SYMBOL_DEN,
    TICKS_PER_SYMBOL_NUM,
};
use isdbs_core::slots::{slot_byte_symbol, ts_slots};
use isdbt_core::{Layer, Params};
use isdbt_fec::MultiplexPattern;

use crate::error::{MuxError, Result};

/// Model-receiver clocks per TSP period (B31 §3.2.2.2).
pub const OFDM_SLOT_CLOCKS: u64 = 408;
/// Clocks from the start of a TSP to the byte holding the last bit of the
/// PCR base (byte 10 of the packet, 2 clocks per byte).
pub const OFDM_PCR_BYTE_CLOCKS: u64 = 20;
/// 27 MHz ticks per model-receiver clock: 27 · 63 / 512 = 1701/512.
pub const OFDM_TICKS_PER_CLOCK: (u64, u64) = (1701, 512);
/// On-air byte of an ISDB-S slot holding the last bit of the PCR base:
/// packet byte 10, the sync byte not being sent (BO.1408-1 Table 3 item 3).
pub const SATELLITE_PCR_BYTE: usize = 9;

/// The transmission system a [`Schedule`] belongs to: what the SI derives
/// from it (the terrestrial delivery system descriptor, partial reception,
/// layer robustness) and what capacity messages can suggest.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum System {
    /// ISDB-T with these transmission parameters (lanes = layers A/B/C).
    Terrestrial(Params),
    /// ISDB-S: relative TS `relative_ts` of the TMCC information `info`
    /// (one lane).
    Satellite {
        /// The TMCC information (slot assignment).
        info: TmccInfo,
        /// The relative TS this multiplexer fills.
        relative_ts: usize,
    },
}

/// One period of the multiplex: the slots in transmission order, their
/// times, and the facts the multiplexer derives its checks and defaults
/// from. Build one with [`Schedule::ofdm`] or [`Schedule::satellite`];
/// the fields are public so that a caller can describe another timeline
/// ([`Schedule::validate`] states the rules).
#[derive(Debug, Clone, PartialEq)]
pub struct Schedule {
    /// The period's slots in transmission order: `Some(lane)` carried by
    /// this multiplexer (lane < 3: the layer index on ISDB-T), `None` not
    /// carried (an ISDB-T null TSP; on ISDB-S another TS's slot).
    pub slots: Vec<Option<usize>>,
    /// Start of each slot, in units from the period start (non-decreasing,
    /// below [`period`](Self::period)).
    pub start: Vec<u64>,
    /// Time of the byte holding the last bit of the PCR base (packet byte
    /// 10) of each slot's packet, in units from the period start.
    pub pcr_byte: Vec<u64>,
    /// Period length in units.
    pub period: u64,
    /// Units → 27 MHz ticks: × `num` / `den`.
    pub num: u64,
    /// See [`num`](Self::num).
    pub den: u64,
    /// TS rate of each lane in bit/s (188-byte packets; 0 for a lane
    /// without slots): the capacity the relay checks against.
    pub lane_bps: Vec<f64>,
    /// PCR packets per period and PCR service when `timing.pcr_per_frame`
    /// is not given (ISDB-T: B31 Part 2 Table 3-5, 1/2/4 for Mode 1/2/3).
    pub default_pcr_per_period: usize,
    /// Lane of PAT, NIT and CAT and, by default, of the other SI (ISDB-T:
    /// B31 Part 2 Table 3-1, [`crate::si::control_layer`]).
    pub control_lane: usize,
    /// The transmission system.
    pub system: System,
}

impl Schedule {
    /// The schedule of ISDB-T `params`: one OFDM frame of the multiplex
    /// frame's slots ([`MultiplexPattern::new`], B31 §3.2.1), slot j at
    /// clock 408·j of the 512/63 MHz model receiver (§3.2.2), its PCR byte
    /// at 408·j + 20, period `samples_per_frame` clocks, × 1701/512 to
    /// ticks; lane l = layer l at [`crate::capacity::layer_bps`].
    pub fn ofdm(params: &Params) -> Self {
        Self::from_pattern(params, &MultiplexPattern::new(params))
    }

    /// [`ofdm`](Self::ofdm) from an already derived pattern of `params`.
    pub(crate) fn from_pattern(params: &Params, pattern: &MultiplexPattern) -> Self {
        let n = pattern.len() as u64;
        Self {
            slots: pattern
                .slots()
                .iter()
                .map(|s| s.map(Layer::index))
                .collect(),
            start: (0..n).map(|j| j * OFDM_SLOT_CLOCKS).collect(),
            pcr_byte: (0..n)
                .map(|j| j * OFDM_SLOT_CLOCKS + OFDM_PCR_BYTE_CLOCKS)
                .collect(),
            period: params.samples_per_frame() as u64,
            num: OFDM_TICKS_PER_CLOCK.0,
            den: OFDM_TICKS_PER_CLOCK.1,
            lane_bps: Layer::ALL
                .iter()
                .map(|&l| crate::capacity::layer_bps(params, l))
                .collect(),
            default_pcr_per_period: params.mode().scale(),
            control_lane: crate::si::control_layer(params),
            system: System::Terrestrial(params.clone()),
        }
    }

    /// The schedule of relative TS `relative_ts` of the ISDB-S TMCC
    /// information `info`: one superframe (BO.1408-1 Table 3: 8 frames of
    /// 39 936 symbols) of the sent slots in transmission order, frame by
    /// frame and slot by slot (Fig. 9; dummy slots are not sent, §8), lane
    /// 0 for the TS's own slots and `None` for the other TSs'; units are
    /// symbols at 28.860 Mbaud (MIC Ordinance No. 87 Art. 52 ¶3), × 450/481
    /// to ticks. A slot starts at the symbol that carries its first on-air
    /// byte and its PCR byte is on-air byte 9 (packet byte 10, the sync
    /// byte is not sent), both from `isdbs_core::slots::slot_byte_symbol`:
    /// the position before the inter-frame interleaver (SPEC-NOTES
    /// "ISDB-S timing model"). The lane carries 8 · n_r · 1 504 bits per
    /// superframe (52.17 Mbit/s for 48 TC8PSK slots); one PCR per
    /// superframe (≈ 11.07 ms) by default; PAT/NIT/CAT on lane 0.
    ///
    /// # Errors
    ///
    /// [`MuxError::Schedule`] when `info` does not validate or gives the TS
    /// no slot.
    pub fn satellite(info: &TmccInfo, relative_ts: usize) -> Result<Self> {
        let bad = |what: String| MuxError::Schedule { what };
        info.validate()
            .map_err(|e| bad(format!("TMCC information: {e}")))?;
        let n = ts_slots(info, relative_ts);
        if n == 0 {
            return Err(bad(format!(
                "relative TS {relative_ts} has no slot in the TMCC information"
            )));
        }
        let comb = info.slot_combinations();
        let (mut slots, mut start, mut pcr_byte) = (Vec::new(), Vec::new(), Vec::new());
        for f in 0..FRAMES {
            for s in (0..SLOTS).filter(|&s| comb[s].is_some()) {
                let at = |byte| {
                    slot_byte_symbol(info, s, byte)
                        .map(|x| (f * FRAME_SYMBOLS + x) as u64)
                        .ok_or_else(|| bad(format!("slot {s} has no symbols")))
                };
                start.push(at(0)?);
                pcr_byte.push(at(SATELLITE_PCR_BYTE)?);
                let own = usize::from(info.relative_ts[s] & 7) == relative_ts;
                slots.push(own.then_some(0));
            }
        }
        let bits = (FRAMES * n * 188 * 8) as f64;
        Ok(Self {
            slots,
            start,
            pcr_byte,
            period: SUPERFRAME_SYMBOLS as u64,
            num: TICKS_PER_SYMBOL_NUM,
            den: TICKS_PER_SYMBOL_DEN,
            lane_bps: vec![bits * SYMBOL_RATE as f64 / SUPERFRAME_SYMBOLS as f64],
            default_pcr_per_period: 1,
            control_lane: 0,
            system: System::Satellite {
                info: info.clone(),
                relative_ts,
            },
        })
    }

    /// The rules the multiplexer relies on: as many `start` and `pcr_byte`
    /// entries as slots, at least one slot, starts non-decreasing and
    /// inside the period, lanes (and the control lane) below 3 with a
    /// `lane_bps` entry each, `den` > 0, and a PCR default ≥ 1.
    ///
    /// # Errors
    ///
    /// [`MuxError::Schedule`] naming the first rule broken.
    pub fn validate(&self) -> Result<()> {
        let bad = |what: String| Err(MuxError::Schedule { what });
        let n = self.slots.len();
        if n == 0 {
            return bad("no slot".into());
        }
        if self.start.len() != n || self.pcr_byte.len() != n {
            return bad(format!(
                "{n} slots but {} starts and {} PCR byte times",
                self.start.len(),
                self.pcr_byte.len()
            ));
        }
        if self.den == 0 || self.num == 0 {
            return bad(format!("unit ratio {}/{}", self.num, self.den));
        }
        if self.start.windows(2).any(|w| w[0] > w[1]) {
            return bad("slot starts decrease".into());
        }
        if self.start.iter().any(|&s| s >= self.period) {
            return bad(format!(
                "a slot starts at or after the period {}",
                self.period
            ));
        }
        let lanes = self.lane_bps.len();
        if lanes > 3 {
            return bad(format!("{lanes} lanes, at most 3"));
        }
        if let Some(l) = self.slots.iter().flatten().find(|&&l| l >= lanes) {
            return bad(format!("slot of lane {l}, {lanes} lanes"));
        }
        if self.control_lane >= lanes {
            return bad(format!("control lane {}, {lanes} lanes", self.control_lane));
        }
        if self.default_pcr_per_period == 0 {
            return bad("no PCR per period by default".into());
        }
        Ok(())
    }

    /// Lanes: the entries of [`lane_bps`](Self::lane_bps).
    pub fn lanes(&self) -> usize {
        self.lane_bps.len()
    }

    /// Slots per period carried on `lane` (ISDB-T: the layer's
    /// `tsp_per_frame`, B31 Table 3-3).
    pub fn lane_slots(&self, lane: usize) -> usize {
        self.slots.iter().filter(|&&s| s == Some(lane)).count()
    }

    /// True when `lane` has slots (ISDB-T: the layer has segments).
    pub fn lane_active(&self, lane: usize) -> bool {
        self.slots.contains(&Some(lane))
    }

    /// `u` units in 27 MHz ticks, rounded down: ⌊u · num / den⌋, split so
    /// that it cannot overflow (the quotient and remainder of `u` by `den`
    /// scale separately and the parts add exactly).
    pub fn units_to_ticks(&self, u: u64) -> u64 {
        (u / self.den) * self.num + (u % self.den) * self.num / self.den
    }

    /// 27 MHz ticks from the stream start to the first byte of slot `j` of
    /// period `k` (add the multiplexer's `clock.start_pcr`).
    pub fn slot_time(&self, k: u64, j: usize) -> u64 {
        self.units_to_ticks(k * self.period + self.start[j])
    }

    /// 27 MHz ticks from the stream start to the PCR byte of slot `j` of
    /// period `k`: the PCR value a packet in that slot carries (minus
    /// `clock.start_pcr`).
    pub fn pcr_time(&self, k: u64, j: usize) -> u64 {
        self.units_to_ticks(k * self.period + self.pcr_byte[j])
    }

    /// Period length in 27 MHz ticks, rounded down (exact on ISDB-T:
    /// `Params::frame_ticks_27mhz`).
    pub fn period_ticks(&self) -> u64 {
        self.units_to_ticks(self.period)
    }

    /// Period length in seconds (ISDB-T: `Params::frame_seconds`, the same
    /// float).
    pub fn period_seconds(&self) -> f64 {
        match &self.system {
            System::Terrestrial(p) => p.frame_seconds(),
            System::Satellite { .. } => {
                self.period as f64 * self.num as f64 / self.den as f64 / 27e6
            }
        }
    }

    /// The ISDB-T parameters, `None` for another system.
    pub fn params(&self) -> Option<&Params> {
        match &self.system {
            System::Terrestrial(p) => Some(p),
            System::Satellite { .. } => None,
        }
    }

    /// The name of `lane` in messages: "layer A" on ISDB-T, "relative TS
    /// r" on ISDB-S.
    pub fn lane_name(&self, lane: usize) -> String {
        match &self.system {
            System::Terrestrial(_) => format!("layer {}", Layer::ALL[lane.min(2)]),
            System::Satellite { relative_ts, .. } => format!("relative TS {relative_ts}"),
        }
    }

    /// The name of the period in messages: "frame" (OFDM frame) on ISDB-T,
    /// "superframe" on ISDB-S.
    pub fn period_name(&self) -> &'static str {
        match &self.system {
            System::Terrestrial(_) => "frame",
            System::Satellite { .. } => "superframe",
        }
    }

    /// The lane as an ISDB-T layer, for the errors that name one
    /// ([`MuxError::InactiveLayer`], [`MuxError::OverCapacity`]).
    pub(crate) fn layer(lane: usize) -> Layer {
        Layer::ALL[lane.min(2)]
    }
}
