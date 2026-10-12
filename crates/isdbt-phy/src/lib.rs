//! ISDB-T physical layer pipelines (ARIB STD-B31 v2.2, Fig. 3-2):
//!
//! ```text
//! TX: per-layer 188-byte TSPs ─► RS(204,188) ─► isdbt_fec::LayerEncoder
//!     ─► isdbt_ofdm::Modulator (mapping … IFFT/GI) ─► complex baseband
//! RX: complex baseband ─► isdbt_ofdm::Demodulator (sync … LLRs)
//!     ─► isdbt_fec::LayerDecoder ─► per-layer TSPs ─► §3.2.2 TS
//!     reproduction (multiplex frame with null TSPs)
//! ```
//!
//! Frame alignment (see `isdbt_ofdm::latency`): the modulator holds one
//! frame (Fig. 3-8 one-frame buffer; the call receiving input frame `k+1`
//! emits on-air frame `k`), the demodulator yields data frame `m − L` with
//! on-air frame `m` (`L = latency_frames`), and the FEC pair adds one more
//! frame (§3.6).  [`Receiver`] tags each output with the data-frame index it
//! carries; [`Transmitter::flush_frames`] filler frames after the last input
//! frame bring every input packet out of a receiver.
//!
//! Entry points: [`Transmitter`] (one OFDM frame per call),
//! [`Receiver`] / [`PipelinedReceiver`] (push samples, get [`RxFrame`]s),
//! [`FrameDecoder`] (LLRs → packets), [`broadcast_ts`] (the 204-byte
//! studio-to-transmitter TS with ISDB-T_information and IIP, B31 Part 2
//! §5.5) and [`iq`] (cf32 / cs8 sample formats).  The crate README describes the stream-boundary rules, the
//! `tx_file` / `rx_file` examples and how the pipelines are verified.
//!
//! # Example
//!
//! Software loopback of one data frame; every input packet comes out:
//!
//! ```
//! use isdbt_core::{CodeRate, GuardInterval, Layer, LayerConfig, Mode, Modulation, Params, TimeInterleave};
//! use isdbt_ofdm::DemodConfig;
//! use isdbt_phy::{Packet, Receiver, Transmitter, null_packet};
//!
//! let a = LayerConfig {
//!     segments: 13,
//!     modulation: Modulation::Qpsk,
//!     code_rate: CodeRate::R1_2,
//!     time_interleave: TimeInterleave(0),
//! };
//! let params = Params::new(Mode::M1, GuardInterval::G1_8, false, [Some(a), None, None])?;
//! let mut tx = Transmitter::new(params.clone());
//! tx.preroll()?;
//! let mut iq = Vec::new();
//! for _ in 0..tx.lead_frames() {
//!     iq.extend(tx.transmit_null_frame()?);
//! }
//! let sent: Vec<Packet> = (0..tx.tsps_per_frame(Layer::A))
//!     .map(|i| {
//!         let mut p = null_packet();
//!         p[1] = 0x01; // PID 0x100
//!         p[4..8].copy_from_slice(&(i as u32).to_be_bytes());
//!         p
//!     })
//!     .collect();
//! iq.extend(tx.transmit_frame([&sent, &[], &[]])?);
//! iq.extend(tx.finish()?);
//! for _ in 1..tx.flush_frames() {
//!     iq.extend(tx.transmit_null_frame()?);
//! }
//!
//! let mut rx = Receiver::new(DemodConfig::default());
//! let got: Vec<Packet> = rx
//!     .push(&iq)?
//!     .into_iter()
//!     .filter(|f| f.valid)
//!     .flat_map(|f| f.layers[0].clone().unwrap())
//!     .filter(|p| *p != null_packet())
//!     .collect();
//! assert_eq!(got, sent);
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

pub mod broadcast_ts;
pub mod iq;

use isdbt_core::{Layer, Params, TSP};
use isdbt_fec::layer::{LayerDecoder, LayerEncoder, RsStats, RsStatus};
use isdbt_fec::remux::{MultiplexPattern, null_tsp};
use isdbt_ofdm::ac::AcFrame;
use isdbt_ofdm::rx::RxState;
use isdbt_ofdm::tmcc::{TmccDecoded, TmccInfo};
use isdbt_ofdm::{C32, DemodConfig, DemodFrame, Demodulator, Modulator};
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::mpsc::{Receiver as Receiver_, SyncSender, channel, sync_channel};

/// A 188-byte transport stream packet.
pub type Packet = [u8; TSP];

/// Pipeline error.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// Channel-coding error.
    #[error(transparent)]
    Fec(#[from] isdbt_fec::FecError),
    /// Symbol-domain error.
    #[error(transparent)]
    Ofdm(#[from] isdbt_ofdm::Error),
    /// Input does not match the configured layers.
    #[error("{0}")]
    Input(String),
    /// Broadcasting-TS additional information (B31 Part 2 §5.5) that
    /// cannot be encoded or does not parse ([`broadcast_ts`]).
    #[error("broadcast TS: {0}")]
    BroadcastTs(String),
    /// A [`PipelinedReceiver`] worker thread panicked (its frames after
    /// the panic are missing).
    #[error("receiver worker thread {0} panicked")]
    Worker(&'static str),
}

/// Result alias.
pub type Result<T> = std::result::Result<T, Error>;

/// ISDB-T transmitter: one call per OFDM frame.
pub struct Transmitter {
    params: Params,
    pattern: MultiplexPattern,
    encoders: [Option<LayerEncoder>; 3],
    modulator: Modulator,
    tmcc: TmccInfo,
    frames: u64,
}

impl Transmitter {
    /// Transmitter for `params`; TMCC carries `params` as current and next
    /// information (§3.15.6).
    pub fn new(params: Params) -> Self {
        let encoders = std::array::from_fn(|i| {
            params.layers()[i]
                .as_ref()
                .map(|l| LayerEncoder::new(params.mode(), l))
        });
        Self {
            pattern: MultiplexPattern::new(&params),
            modulator: Modulator::new(params.clone()),
            tmcc: TmccInfo::from_params(&params),
            encoders,
            params,
            frames: 0,
        }
    }

    /// Parameters.
    pub fn params(&self) -> &Params {
        &self.params
    }

    /// Override the TMCC information (e.g. emergency-alarm flag, countdown).
    pub fn set_tmcc(&mut self, tmcc: TmccInfo) {
        self.tmcc = tmcc;
    }

    /// Output scaling applied by the modulator.
    pub fn set_gain(&mut self, gain: f32) {
        self.modulator.set_gain(gain);
    }

    /// TSPs consumed per frame by `layer` (Table 3-3 × N); 0 if unused.
    pub fn tsps_per_frame(&self, layer: Layer) -> usize {
        self.encoders[layer.index()]
            .as_ref()
            .map_or(0, |e| e.tsps_per_frame())
    }

    /// §3.2 multiplex-frame arrangement for these parameters.
    pub fn pattern(&self) -> &MultiplexPattern {
        &self.pattern
    }

    /// Frames transmitted so far.
    pub fn frames(&self) -> u64 {
        self.frames
    }

    /// Modulate one OFDM frame from exactly [`Self::tsps_per_frame`] packets
    /// per active layer; returns `samples_per_frame` complex samples at
    /// 512/63 MHz.
    pub fn transmit_frame(&mut self, layers: [&[Packet]; 3]) -> Result<Vec<C32>> {
        let mut bits: [Vec<u8>; 3] = Default::default();
        for (i, enc) in self.encoders.iter_mut().enumerate() {
            match enc {
                Some(e) => bits[i] = e.encode_frame_188(layers[i])?,
                None if !layers[i].is_empty() => {
                    return Err(Error::Input(format!(
                        "layer {} unused but {} packets given",
                        Layer::ALL[i],
                        layers[i].len()
                    )));
                }
                None => {}
            }
        }
        let ac = AcFrame::stuffing(self.modulator.layout());
        let out = self
            .modulator
            .modulate_frame([&bits[0], &bits[1], &bits[2]], &self.tmcc, &ac)?;
        self.frames += 1;
        Ok(out)
    }

    /// Modulate one OFDM frame from a re-multiplexed TS frame of
    /// `pattern().len()` packets (§3.2.1): slot *j* goes to the layer the
    /// §3.2.2 model receiver assigns, null slots are discarded (§3.4).
    pub fn transmit_multiplex_frame(&mut self, frame: &[Packet]) -> Result<Vec<C32>> {
        let [a, b, c] = self.pattern.divide(frame)?;
        self.transmit_frame([&a, &b, &c])
    }

    /// Bring the transmitter to steady state before the first emitted
    /// frame: modulate and discard [`Self::preroll_frames`] null frames.
    ///
    /// Without it the first on-air frames carry the start-up fill of the
    /// delay lines instead of transport packets, so a receiver locked from
    /// the start decodes fill (uncorrectable RS words, garbage "packets")
    /// before the first real frame.  The symbol-domain fill
    /// (`isdbt_ofdm`) is pseudo-random, but the FEC encoder's byte
    /// interleaver / delay-adjustment fill is not energy-dispersed: its long
    /// runs of zero bits map to identical carriers and still give ~37 dB
    /// PAPR in on-air frames 0–1 (Mode 3, I = 2), which an 8-bit DAC
    /// clips.  After the pre-roll every emitted symbol is ordinary
    /// OFDM carrying null packets until real input arrives.
    pub fn preroll(&mut self) -> Result<()> {
        for _ in 0..self.preroll_frames() {
            self.transmit_null_frame()?;
        }
        Ok(())
    }

    /// Frames discarded by [`Self::preroll`]: the end-to-end delay
    /// `1 + L` plus one frame for the bit interleaver's carry-over into the
    /// next frame (§3.9.2, up to 120 carrier symbols).
    pub fn preroll_frames(&self) -> usize {
        isdbt_ofdm::latency::end_to_end_frames(&self.params) + 1
    }

    /// Modulate one frame whose every layer carries null packets
    /// (H.222.0 §2.4.3.3): lead-in, flush and stuffing frames.
    pub fn transmit_null_frame(&mut self) -> Result<Vec<C32>> {
        let nulls: [Vec<Packet>; 3] =
            std::array::from_fn(|i| vec![null_tsp(); self.tsps_per_frame(Layer::ALL[i])]);
        self.transmit_frame([&nulls[0], &nulls[1], &nulls[2]])
    }

    /// End of stream for the modulator: emit the frame it still buffers
    /// (Fig. 3-8 one-frame buffer) by feeding one null frame.  After this
    /// the last input frame has gone on air, but its packets are still in
    /// the time/byte interleavers; see [`Self::flush_frames`].
    pub fn finish(&mut self) -> Result<Vec<C32>> {
        self.transmit_null_frame()
    }

    /// Null frames to transmit after the last input frame so a receiver
    /// outputs all of its packets: the modulator buffer (1), the time
    /// interleaving `L` (Table 3-12), the FEC delay (1 frame, §3.6) and one
    /// more frame because a receiver completes an on-air frame only after
    /// the first symbols of the next one (channel-estimation look-ahead of
    /// `isdbt_ofdm::DemodConfig::sp_time_window`).
    pub fn flush_frames(&self) -> usize {
        isdbt_ofdm::latency::end_to_end_frames(&self.params) + isdbt_fec::layer::LATENCY_FRAMES + 1
    }

    /// Null frames to transmit before the first input frame so that a
    /// receiver started together with the transmitter outputs every input
    /// packet.  A receiver locking at on-air frame `s` primes its
    /// deinterleavers for `L + 1` frames and the FEC decoder for one more,
    /// so its first valid output is data frame `s + 1`; locking at the very
    /// first emitted frame (`s = −1`) loses nothing, and one frame of
    /// acquisition delay (the usual case with an arbitrary sample offset)
    /// is covered by this two-frame lead-in.
    pub fn lead_frames(&self) -> usize {
        2
    }
}

/// One frame of receiver output.
#[derive(Debug, Clone)]
pub struct RxFrame {
    /// Data-frame index of these packets, in on-air frame numbering
    /// relative to the on-air frame at which the receiver locked (= 0).
    /// With the `isdbt_ofdm::latency` contract, on-air frame `m` yields the
    /// LLRs of data frame `m − L` and the FEC decoder adds one frame, so
    /// `tx_frame = frame_number − L − 1`; warm-up outputs are negative.
    /// The transmitter's absolute frame number is `tx_frame` plus the
    /// (unknown to the receiver) on-air index of the lock frame.
    pub tx_frame: i64,
    /// True once all deinterleavers are primed; packets of invalid frames
    /// are warm-up filler.
    pub valid: bool,
    /// Decoded packets per layer (parity stripped). Packets whose RS word
    /// was uncorrectable carry transport_error_indicator = 1 (H.222.0
    /// §2.4.3.3).
    pub layers: [Option<Vec<Packet>>; 3],
    /// RS statistics per layer.
    pub rs: [Option<RsStats>; 3],
    /// Single TS reproduced as by the §3.2.2 model receiver: the multiplex
    /// frame with null TSPs in the slots of no layer.
    pub ts: Vec<Packet>,
    /// TMCC decoded with the on-air frame that completed this output.
    pub tmcc: TmccDecoded,
    /// Parameters in force.
    pub params: Params,
    /// Demodulator metrics of that on-air frame.
    pub metrics: isdbt_ofdm::rx::Metrics,
    /// Sample index (in the pushed stream) where that on-air frame starts.
    pub start_sample: u64,
}

/// Channel-decoding half of the receiver: demodulated frames (LLRs) →
/// RS-corrected packets and the §3.2.2 reproduced TS.  [`Receiver`] runs it
/// inline; [`PipelinedReceiver`] runs it on its own thread.
#[derive(Default)]
pub struct FrameDecoder {
    chain: Option<RxChain>,
}

struct RxChain {
    params: Params,
    pattern: MultiplexPattern,
    decoders: [Option<LayerDecoder>; 3],
    prev_valid: bool,
}

impl FrameDecoder {
    /// New decoder (configures itself from each frame's TMCC parameters).
    pub fn new() -> Self {
        Self::default()
    }

    /// Decode one demodulated frame.
    pub fn decode(&mut self, f: DemodFrame) -> Result<RxFrame> {
        if self.chain.as_ref().is_none_or(|c| c.params != f.params) {
            let p = f.params.clone();
            self.chain = Some(RxChain {
                decoders: std::array::from_fn(|i| {
                    p.layers()[i]
                        .as_ref()
                        .map(|l| LayerDecoder::new(p.mode(), l))
                }),
                pattern: MultiplexPattern::new(&p),
                params: p,
                prev_valid: false,
            });
        }
        let chain = self.chain.as_mut().expect("chain set above");
        let mut layers: [Option<Vec<Packet>>; 3] = Default::default();
        let mut rs: [Option<RsStats>; 3] = Default::default();
        for (i, dec) in chain.decoders.iter_mut().enumerate() {
            let (Some(dec), Some(llr)) = (dec.as_mut(), f.llr[i].as_ref()) else {
                continue;
            };
            let d = dec.decode_frame(llr)?;
            let mut pkts = d.tsps_188();
            for (p, s) in pkts.iter_mut().zip(&d.status) {
                if *s == RsStatus::Uncorrectable {
                    p[1] |= 0x80;
                }
            }
            layers[i] = Some(pkts);
            rs[i] = Some(d.stats);
        }
        let empty: Vec<Packet> = Vec::new();
        let refs = std::array::from_fn(|i| layers[i].as_deref().unwrap_or(&empty));
        let ts = chain.pattern.reproduce_ts(refs)?;
        // The FEC pair adds one frame (§3.6): output is valid once both this
        // and the previous LLR frame were.
        let valid = f.valid && chain.prev_valid;
        chain.prev_valid = f.valid;
        Ok(RxFrame {
            tx_frame: f.frame_number as i64
                - f.latency_frames as i64
                - isdbt_fec::layer::LATENCY_FRAMES as i64,
            valid,
            layers,
            rs,
            ts,
            tmcc: f.tmcc,
            params: f.params,
            metrics: f.metrics,
            start_sample: f.start_sample,
        })
    }
}

/// ISDB-T receiver: push samples, get decoded frames.
pub struct Receiver {
    demod: Demodulator,
    decoder: FrameDecoder,
}

impl Receiver {
    /// Receiver with the given demodulator configuration.
    pub fn new(cfg: DemodConfig) -> Self {
        Self {
            demod: Demodulator::new(cfg),
            decoder: FrameDecoder::new(),
        }
    }

    /// Parameters currently decoded from TMCC, if locked.
    pub fn params(&self) -> Option<&Params> {
        self.demod.params()
    }

    /// Synchronisation state.
    pub fn state(&self) -> RxState {
        self.demod.state()
    }

    /// Feed complex baseband at 512/63 MHz (any chunking).
    pub fn push(&mut self, samples: &[C32]) -> Result<Vec<RxFrame>> {
        self.demod
            .push(samples)
            .into_iter()
            .map(|f| self.decoder.decode(f))
            .collect()
    }
}

/// Receiver running demodulation and channel decoding on two worker
/// threads connected by bounded queues, for real-time input (the two
/// halves take comparable time, so the pipeline roughly doubles
/// throughput).  Samples go in with [`Self::push`]; decoded frames come out
/// of [`Self::try_recv`]; [`Self::finish`] drains.
pub struct PipelinedReceiver {
    samples: Option<SyncSender<Vec<C32>>>,
    frames: Receiver_<Result<RxFrame>>,
    state: Arc<AtomicU8>,
    workers: Vec<(&'static str, std::thread::JoinHandle<()>)>,
}

impl PipelinedReceiver {
    /// Start the workers; `depth` sample chunks may be queued.
    ///
    /// # Errors
    ///
    /// The operating system's error when a worker thread cannot be
    /// spawned.
    pub fn new(cfg: DemodConfig, depth: usize) -> std::io::Result<Self> {
        let (s_tx, s_rx) = sync_channel::<Vec<C32>>(depth.max(1));
        let (d_tx, d_rx) = sync_channel::<DemodFrame>(2);
        // Unbounded output: a caller blocked in `push` while not draining
        // frames must not deadlock the pipeline (frames are drained after
        // every push, so this stays short).
        let (f_tx, f_rx) = channel::<Result<RxFrame>>();
        let state = Arc::new(AtomicU8::new(0));
        let st = state.clone();
        let demod = std::thread::Builder::new()
            .name("rx-demod".into())
            .spawn(move || {
                let mut demod = Demodulator::new(cfg);
                for chunk in s_rx {
                    for f in demod.push(&chunk) {
                        if d_tx.send(f).is_err() {
                            return;
                        }
                    }
                    st.store(demod.state() as u8, Ordering::Relaxed);
                }
            })?;
        let decode = std::thread::Builder::new()
            .name("rx-decode".into())
            .spawn(move || {
                let mut dec = FrameDecoder::new();
                for f in d_rx {
                    if f_tx.send(dec.decode(f)).is_err() {
                        return;
                    }
                }
            })?;
        Ok(Self {
            samples: Some(s_tx),
            frames: f_rx,
            state,
            workers: vec![("rx-demod", demod), ("rx-decode", decode)],
        })
    }

    /// Queue samples (blocks while the queue is full).
    pub fn push(&mut self, samples: Vec<C32>) {
        if let Some(s) = &self.samples {
            // A send error means the workers ended (the output side was
            // dropped or a worker panicked); decode errors themselves are
            // delivered as `Err` items through `try_recv` / `finish`.
            let _ = s.send(samples);
        }
    }

    /// Synchronisation state of the demodulator (as of its last chunk).
    pub fn state(&self) -> RxState {
        match self.state.load(Ordering::Relaxed) {
            x if x == RxState::Locked as u8 => RxState::Locked,
            x if x == RxState::Acquiring as u8 => RxState::Acquiring,
            _ => RxState::Searching,
        }
    }

    /// A decoded frame if one is ready.
    pub fn try_recv(&mut self) -> Option<Result<RxFrame>> {
        self.frames.try_recv().ok()
    }

    /// Close the input and return every remaining frame, followed by an
    /// [`Error::Worker`] item for each worker thread that panicked.
    pub fn finish(mut self) -> Vec<Result<RxFrame>> {
        self.samples = None;
        let mut out: Vec<_> = self.frames.iter().collect();
        for (name, w) in self.workers.drain(..) {
            if w.join().is_err() {
                out.push(Err(Error::Worker(name)));
            }
        }
        out
    }
}

/// A null packet (PID 0x1FFF), the filler for unused multiplex slots.
pub fn null_packet() -> Packet {
    null_tsp()
}

// Compile the README's code blocks as doctests.
#[cfg(doctest)]
#[doc = include_str!("../README.md")]
struct ReadmeDoctests;
