//! Receiver: synchronisation, channel estimation, TMCC decoding and the
//! inverse symbol-domain chain producing per-layer LLRs.
//!
//! Push complex baseband samples at 512/63 MHz (any chunking) into
//! [`Demodulator::push`]; it yields one [`DemodFrame`] per received OFDM
//! frame once locked.  Stages:
//!
//! 1. **Mode / GI / timing / fractional CFO** — guard-interval
//!    autocorrelation folded over several symbols ([`acquire::detect_mode`]).
//! 2. **Integer CFO** — continual-carrier coherence matched to the AC1 +
//!    band-edge positions ([`acquire::integer_cfo`]).
//! 3. **Segment kinds** — differential-only continual positions
//!    ([`acquire::detect_kinds`]).
//! 4. **SP phase** — symbol index mod 4 from scattered-pilot coherence
//!    ([`acquire::detect_sp_phase`]), used to validate frame candidates.
//! 5. **Frame sync** — DBPSK-decoded TMCC bits, sync word w0/w1 at B1–B16,
//!    confirmed by the (184,102) parity of the whole TMCC frame, which also
//!    gives the [`Params`].
//! 6. **Tracking** — per-symbol common-phase-error correction from the
//!    continual carriers, SP channel estimation, equalisation (coherent) or
//!    differential detection (DQPSK), frequency/time deinterleaving,
//!    max-log demapping, bit deinterleaving and the alignment FIFO of
//!    [`crate::latency`].

pub mod acquire;
pub mod chanest;

use crate::C32;
use crate::ac::{SeismicDecoded, SeismicWarning, is_seismic_config};
use crate::combine::layer_segments;
use crate::delay::{Fifo, MultiDelay};
use crate::frame::{FrameLayout, SegmentKind};
use crate::freq_interleave::{FrequencyInterleaver, RotationNumbering};
use crate::latency::{latency_frames, rx_alignment_symbols};
use crate::mapping::{BitDeinterleaver, Demapper, dqpsk_differential};
use crate::ofdm::{OfdmRx, carrier_bin};
use crate::time_interleave::rx_delays;
use crate::tmcc::{TmccDecoded, decode_frame, sync_word};
use acquire::{
    GUARDS, MODES, ModeGuess, detect_kinds, detect_mode, detect_sp_phase, integer_cfo,
    samples_needed, tmcc_soft,
};
use chanest::ChannelEstimator;
use isdbt_core::{GuardInterval, Layer, Mode, Modulation, Params, SYMBOLS_PER_FRAME};
use std::collections::VecDeque;

/// Receiver configuration.
#[derive(Debug, Clone)]
pub struct DemodConfig {
    /// Known mode (skip detection) or `None`.
    pub mode: Option<Mode>,
    /// Known guard interval or `None`.
    pub guard: Option<GuardInterval>,
    /// Carrier-rotation numbering of the frequency deinterleaver.
    pub rotation: RotationNumbering,
    /// Integer-CFO search range in carrier spacings (`None`: ±(N−K)/2).
    pub max_cfo_carriers: Option<usize>,
    /// FFT window advance into the guard interval, as a fraction of it.
    pub timing_backoff: f32,
    /// Symbols used for mode detection.
    pub detect_symbols: usize,
    /// Symbols used for integer-CFO and segment-kind detection.
    pub continual_symbols: usize,
    /// Scattered-pilot occurrences per side used by the time interpolation
    /// of the channel estimate (1 = linear interpolation between the two
    /// nearest SPs, robust to Doppler; 2 = least-squares over 4 SPs,
    /// ≈ 0.6 dB better MER on slowly varying channels).  The receiver's
    /// processing look-ahead is `4·w − 1` symbols.
    pub sp_time_window: usize,
}

impl Default for DemodConfig {
    fn default() -> Self {
        Self {
            mode: None,
            guard: None,
            rotation: RotationNumbering::default(),
            max_cfo_carriers: None,
            timing_backoff: 0.25,
            detect_symbols: 6,
            continual_symbols: 32,
            sp_time_window: 2,
        }
    }
}

/// Quality metrics of a frame.
#[derive(Debug, Clone, PartialEq)]
pub struct Metrics {
    /// Total carrier frequency offset in carrier spacings (estimate).
    pub cfo_carriers: f64,
    /// Integer part found by the continual-carrier search.
    pub cfo_integer: i64,
    /// Mode-detection correlation metric.
    pub gi_metric: f64,
    /// Modulation error ratio per layer in dB (DQPSK: on the differential
    /// product, which includes the 3 dB differential noise penalty).
    pub mer_db: [Option<f32>; 3],
    /// Noise variance per carrier (relative to unit-power data carriers).
    pub n0: f32,
    /// Accumulated common phase error over the frame (radians).
    pub cpe_drift: f32,
    /// Estimated timing drift in samples per OFDM symbol (sampling-clock
    /// offset ≈ drift / symbol length).
    pub timing_drift: f64,
    /// Total timing correction applied since lock (samples).
    pub timing_correction: f64,
}

/// One received OFDM frame.
#[derive(Debug, Clone)]
pub struct DemodFrame {
    /// Frames since lock (0 = first frame decoded).
    pub frame_number: u64,
    /// Sample index (in the pushed stream) of the start of this frame's
    /// symbol 0 guard interval.
    pub start_sample: u64,
    /// Decoded TMCC of this frame.
    pub tmcc: TmccDecoded,
    /// Parameters in force.
    pub params: Params,
    /// Per-layer LLRs (`nc·S·N·204`), aligned per [`crate::latency`]: with
    /// on-air frame `m` they belong to modulator input frame
    /// `m − latency_frames`.
    pub llr: [Option<Vec<f32>>; 3],
    /// False while the deinterleavers are priming (the first
    /// `latency_frames + 1` frames after lock).
    pub valid: bool,
    /// [`crate::latency::latency_frames`].
    pub latency_frames: usize,
    /// Hard-decided AC bits B0..B203 per AC carrier (ascending frequency;
    /// B0 set to 0).
    pub ac_bits: Vec<[u8; 204]>,
    /// Seismic-warning word of segment 0 if its configuration
    /// identification announces one (soft-combined over its AC carriers).
    pub seismic: Option<SeismicDecoded>,
    /// Metrics.
    pub metrics: Metrics,
}

/// Receiver status.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RxState {
    /// Looking for an ISDB-T signal.
    Searching,
    /// Mode known, synchronising.
    Acquiring,
    /// Frame-locked.
    Locked,
}

/// Half length of the resampling interpolator (taps per side).
const INTERP_HALF: usize = 12;
/// Fractional phases of the polyphase interpolator table.
const INTERP_PHASES: usize = 512;

/// Kaiser-windowed sinc polyphase table: `INTERP_PHASES + 1` rows of
/// `2·INTERP_HALF` taps for fractional delays 0..=1.
fn interp_table() -> &'static [Vec<f32>] {
    static T: std::sync::OnceLock<Vec<Vec<f32>>> = std::sync::OnceLock::new();
    T.get_or_init(|| {
        let beta = 8.0f64;
        let i0 = |v: f64| {
            let (mut sum, mut term) = (1.0, 1.0);
            for k in 1..40 {
                term *= (v / 2.0) * (v / 2.0) / (k * k) as f64;
                sum += term;
            }
            sum
        };
        let i0b = i0(beta);
        let h = INTERP_HALF as f64;
        (0..=INTERP_PHASES)
            .map(|ph| {
                let mu = ph as f64 / INTERP_PHASES as f64;
                // Taps for input offsets −(H−1)..=H relative to floor(p).
                let taps: Vec<f64> = (0..2 * INTERP_HALF)
                    .map(|t| {
                        let d = mu - (t as f64 - (h - 1.0));
                        let sinc = if d.abs() < 1e-12 {
                            1.0
                        } else {
                            (std::f64::consts::PI * d).sin() / (std::f64::consts::PI * d)
                        };
                        let u = d / h;
                        let w = if u.abs() >= 1.0 {
                            0.0
                        } else {
                            i0(beta * (1.0 - u * u).sqrt()) / i0b
                        };
                        sinc * w
                    })
                    .collect();
                let sum: f64 = taps.iter().sum();
                taps.iter().map(|&v| (v / sum) as f32).collect()
            })
            .collect()
    })
}

/// Time-domain symbol extraction (timing, sampling-rate correction, CFO
/// derotation, FFT).
struct Extractor {
    mode: Mode,
    n: usize,
    ng: usize,
    backoff: usize,
    /// Absolute input-sample position of the next symbol's GI start.
    pos: f64,
    /// Input samples per receiver sample (1 + sampling-clock offset).
    rate: f64,
    /// CFO in carrier spacings (total).
    cfo: f64,
    fft: OfdmRx,
    tmp: Vec<C32>,
}

impl Extractor {
    fn new(mode: Mode, guard: GuardInterval, start: u64, cfo: f64, backoff_frac: f32) -> Self {
        let n = mode.fft_size();
        let ng = guard.samples(mode);
        let backoff = ((ng as f32 * backoff_frac) as usize).min(ng);
        Self {
            mode,
            n,
            ng,
            backoff,
            pos: start as f64,
            rate: 1.0,
            cfo,
            fft: OfdmRx::new(mode),
            tmp: vec![C32::default(); n],
        }
    }

    /// GI start (rounded) of the next symbol.
    fn next_start(&self) -> u64 {
        self.pos.max(0.0).round() as u64
    }

    /// Absolute input sample index needed (exclusive) for the next symbol.
    fn needs(&self) -> u64 {
        (self.pos + (self.n + self.ng) as f64 * self.rate)
            .ceil()
            .max(0.0) as u64
            + INTERP_HALF as u64
            + 1
    }

    /// Move the timing of the following symbols by `c` samples.
    fn adjust(&mut self, c: f64) {
        self.pos += c;
    }

    /// Extract the next symbol's full N-bin spectrum from `buf` (buf[0] is
    /// absolute sample `base`).  Returns the (rounded) GI start.
    fn extract(&mut self, buf: &VecDeque<C32>, base: u64) -> u64 {
        let start = self.next_start();
        let off = (self.ng - self.backoff) as f64;
        let w = self.pos + off * self.rate;
        let two_pi = 2.0 * std::f64::consts::PI;
        let f = self.cfo / self.n as f64;
        let exact = self.rate == 1.0 && w.fract() == 0.0;
        if exact {
            let w0 = (w as u64 - base) as usize;
            for m in 0..self.n {
                self.tmp[m] = buf[w0 + m];
            }
        } else {
            let table = interp_table();
            for m in 0..self.n {
                let p = w + m as f64 * self.rate;
                let fl = p.floor();
                let ph = ((p - fl) * INTERP_PHASES as f64).round() as usize;
                let taps = &table[ph];
                let i0 = (fl as i64 - base as i64) - (INTERP_HALF as i64 - 1);
                let mut acc = C32::default();
                for (t, &h) in taps.iter().enumerate() {
                    let idx = i0 + t as i64;
                    if idx >= 0 && (idx as usize) < buf.len() {
                        acc += buf[idx as usize] * h;
                    }
                }
                self.tmp[m] = acc;
            }
        }
        // CFO derotation on the input time base.
        let ph0 = (-two_pi * f * w).rem_euclid(two_pi);
        let mut rot = C32::new(ph0.cos() as f32, ph0.sin() as f32);
        let step_ph = -two_pi * f * self.rate;
        let step = C32::new(step_ph.cos() as f32, step_ph.sin() as f32);
        for (m, v) in self.tmp.iter_mut().enumerate() {
            *v *= rot;
            rot *= step;
            if m % 1024 == 1023 {
                rot /= rot.norm();
            }
        }
        self.fft.transform(&mut self.tmp);
        // Undo the window advance (backoff): bin s × exp(+j2π s·backoff/N).
        let th = two_pi * self.backoff as f64 / self.n as f64;
        let n = self.n;
        for (half, s0) in [(0..n / 2, 0.0f64), (n / 2..n, -((n / 2) as f64))] {
            let mut r = C32::from_polar(1.0, (th * s0) as f32);
            let st = C32::from_polar(1.0, th as f32);
            for (i, b) in half.enumerate() {
                self.tmp[b] *= r;
                r *= st;
                if i % 1024 == 1023 {
                    r /= r.norm();
                }
            }
        }
        self.pos += (self.n + self.ng) as f64 * self.rate;
        start
    }

    /// Band carriers 0..K from the last extracted spectrum.
    fn carriers(&self) -> Vec<C32> {
        (0..self.mode.total_carriers())
            .map(|k| self.tmp[carrier_bin(self.mode, k)])
            .collect()
    }
}

struct Acq {
    guess: ModeGuess,
    ex: Extractor,
    first_start: u64,
    /// Full-bin spectra before the integer CFO is known.
    raw: Vec<Vec<C32>>,
    cfo_int: Option<i64>,
    /// Band-carrier spectra and their GI starts.
    spectra: Vec<Vec<C32>>,
    starts: Vec<u64>,
    kinds: Option<FrameLayout>,
    /// SP phase offset c (n mod 4 = (j + c) mod 4) if coherent segments exist.
    sp_phase: Option<(usize, f32)>,
    soft: Vec<f32>,
    next_candidate: usize,
}

/// Layer-specific receive pipeline.
struct LayerRx {
    layer: Layer,
    demapper: Demapper,
    s: usize,
    bitdi: BitDeinterleaver,
    align: Fifo<f32>,
    out: Vec<f32>,
    llr_tmp: Vec<f32>,
    mer_err: f64,
    mer_n: u64,
    first: usize,
    count: usize,
}

struct Track {
    params: Params,
    layout: FrameLayout,
    ex: Extractor,
    fi: FrequencyInterleaver,
    tdi: MultiDelay<(C32, f32)>,
    est: ChannelEstimator,
    layers: Vec<LayerRx>,
    seg_diff: Vec<bool>,
    /// Channel-estimation window and look-ahead.
    sp_w: usize,
    lookahead: u64,
    /// Ring of (symbol index j, GI start, spectrum).
    ring: VecDeque<(u64, u64, Vec<C32>)>,
    /// Continual carriers (TMCC, AC, CP, band edge).
    cont: Vec<usize>,
    /// Symbol index of frame start (j of n = 0).
    frame0: u64,
    next_j: u64,
    cpe: f32,
    cpe_frame_start: f32,
    /// Timing loop: drift estimate (samples/symbol), residual timing
    /// error (samples), correction applied to the current symbol, total
    /// correction since lock.
    drift: f64,
    tau: f64,
    last_corr: f64,
    timing_total: f64,
    n0: f32,
    nc: usize,
    latency: usize,
    frames_out: u64,
    bad_frames: u32,
    tmcc_soft: Vec<[f32; 3]>,
    ac_soft: Vec<Vec<f32>>,
    frame_start_sample: u64,
    cfo_int: i64,
    gi_metric: f64,
    // scratch
    data: Vec<(C32, f32)>,
    deint: Vec<(C32, f32)>,
}

/// The receiver.
pub struct Demodulator {
    cfg: DemodConfig,
    buf: VecDeque<C32>,
    base: u64,
    acq: Option<Box<Acq>>,
    track: Option<Box<Track>>,
}

impl std::fmt::Debug for Demodulator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Demodulator")
            .field("state", &self.state())
            .finish()
    }
}

impl Demodulator {
    /// New receiver. Out-of-range settings are clamped rather than
    /// trusted (the fields of [`DemodConfig`] are public): symbol counts
    /// and `sp_time_window` to at least 1, `timing_backoff` to 0..=1 (a
    /// non-finite value takes the default 0.25).
    pub fn new(mut cfg: DemodConfig) -> Self {
        cfg.detect_symbols = cfg.detect_symbols.max(1);
        cfg.continual_symbols = cfg.continual_symbols.max(1);
        cfg.sp_time_window = cfg.sp_time_window.max(1);
        cfg.timing_backoff = if cfg.timing_backoff.is_finite() {
            cfg.timing_backoff.clamp(0.0, 1.0)
        } else {
            DemodConfig::default().timing_backoff
        };
        Self {
            cfg,
            buf: VecDeque::new(),
            base: 0,
            acq: None,
            track: None,
        }
    }

    /// Current state.
    pub fn state(&self) -> RxState {
        if self.track.is_some() {
            RxState::Locked
        } else if self.acq.is_some() {
            RxState::Acquiring
        } else {
            RxState::Searching
        }
    }

    /// Parameters once locked.
    pub fn params(&self) -> Option<&Params> {
        self.track.as_ref().map(|t| &t.params)
    }

    /// Push samples; returns the frames completed by them.
    pub fn push(&mut self, samples: &[C32]) -> Vec<DemodFrame> {
        self.buf.extend(samples.iter().copied());
        let mut out = Vec::new();
        loop {
            let progressed = if self.track.is_some() {
                self.step_track(&mut out)
            } else if self.acq.is_some() {
                self.step_acquire()
            } else {
                self.step_search()
            };
            if !progressed {
                break;
            }
        }
        out
    }

    fn drop_before(&mut self, abs: u64) {
        if abs > self.base {
            let n = ((abs - self.base) as usize).min(self.buf.len());
            self.buf.drain(..n);
            self.base += n as u64;
        }
    }

    fn step_search(&mut self) -> bool {
        let modes: Vec<Mode> = self.cfg.mode.map_or(MODES.to_vec(), |m| vec![m]);
        let guards: Vec<GuardInterval> = self.cfg.guard.map_or(GUARDS.to_vec(), |g| vec![g]);
        let need = samples_needed(&modes, &guards, self.cfg.detect_symbols);
        if self.buf.len() < need {
            return false;
        }
        let window: Vec<C32> = self.buf.iter().take(need).copied().collect();
        match detect_mode(&window, &modes, &guards) {
            Some(g) if g.metric > 0.3 => {
                let start = self.base + g.offset as u64;
                let ex =
                    Extractor::new(g.mode, g.guard, start, g.cfo_frac, self.cfg.timing_backoff);
                self.acq = Some(Box::new(Acq {
                    guess: g,
                    ex,
                    first_start: start,
                    raw: Vec::new(),
                    cfo_int: None,
                    spectra: Vec::new(),
                    starts: Vec::new(),
                    kinds: None,
                    sp_phase: None,
                    soft: Vec::new(),
                    next_candidate: 0,
                }));
                self.drop_before(start);
            }
            _ => {
                // No signal: discard half the window and retry.
                self.drop_before(self.base + need as u64 / 2);
            }
        }
        true
    }

    fn step_acquire(&mut self) -> bool {
        let base = self.base;
        let avail = base + self.buf.len() as u64;
        let acq = self.acq.as_mut().expect("acq");
        if acq.ex.needs() > avail {
            return false;
        }
        let mode = acq.guess.mode;
        if acq.cfo_int.is_none() {
            acq.ex.extract(&self.buf, base);
            acq.raw.push(acq.ex.tmp.clone());
            if acq.raw.len() >= self.cfg.continual_symbols {
                let n = mode.fft_size();
                let k = mode.total_carriers();
                let max = self.cfg.max_cfo_carriers.unwrap_or((n - k) / 2);
                let (d, _score) = integer_cfo(mode, &acq.raw, max);
                acq.cfo_int = Some(d);
                acq.raw.clear();
                acq.ex.cfo = acq.guess.cfo_frac + d as f64;
                acq.ex.pos = acq.first_start as f64;
            }
            return true;
        }
        // Give up after 3 frames without TMCC lock and search again.
        if acq.spectra.len() > 3 * SYMBOLS_PER_FRAME + self.cfg.continual_symbols {
            let keep = acq.ex.next_start();
            self.acq = None;
            self.drop_before(keep);
            return true;
        }
        let start = acq.ex.extract(&self.buf, base);
        acq.spectra.push(acq.ex.carriers());
        acq.starts.push(start);
        if acq.kinds.is_none() {
            if acq.spectra.len() >= self.cfg.continual_symbols {
                let (kinds, _) = detect_kinds(mode, &acq.spectra);
                let layout = FrameLayout::new(mode, kinds);
                acq.sp_phase = detect_sp_phase(&layout, &acq.spectra);
                for j in 1..acq.spectra.len() {
                    let s = tmcc_soft(&layout, &acq.spectra[j], &acq.spectra[j - 1], None);
                    acq.soft.push(s);
                }
                acq.kinds = Some(layout);
            }
            return true;
        }
        let layout = acq.kinds.as_ref().expect("layout");
        let j = acq.spectra.len() - 1;
        let s = tmcc_soft(layout, &acq.spectra[j], &acq.spectra[j - 1], None);
        acq.soft.push(s);
        // soft[j-1] holds the DBPSK value of symbol j (j ≥ 1).
        // Try frame-start candidates f with symbols f..f+203 available.
        while acq.next_candidate + SYMBOLS_PER_FRAME <= acq.spectra.len() {
            let f = acq.next_candidate;
            acq.next_candidate += 1;
            // The frame start must agree with the SP phase (n = 0 ⇒ phase 0).
            if let Some((c, conf)) = acq.sp_phase
                && conf > 0.6
                && !(f + c).is_multiple_of(4)
            {
                continue;
            }
            let bit = |n: usize| u8::from(acq.soft[f + n - 1] < 0.0);
            let sync: Vec<u8> = (1..17).map(bit).collect();
            if sync != sync_word(false) && sync != sync_word(true) {
                continue;
            }
            let mut bits = [0u8; 204];
            for (n, b) in bits.iter_mut().enumerate().skip(1) {
                *b = bit(n);
            }
            // B17–B19 are mixed over segment kinds here; they are outside
            // the parity-protected range, so the parity check is unaffected.
            let dec = decode_frame(&bits);
            if !dec.parity.is_ok() {
                continue;
            }
            let Ok(params) = dec.info.params(mode, acq.guess.guard) else {
                continue;
            };
            // Lock.
            let acq = self.acq.take().expect("acq");
            self.start_track(*acq, f, params);
            return true;
        }
        true
    }

    fn start_track(&mut self, acq: Acq, f: usize, params: Params) {
        let layout = FrameLayout::from_params(&params);
        let nc = params.mode().data_carriers_per_segment();
        let latency = latency_frames(&params);
        let layers = params
            .active_layers()
            .map(|(id, l)| {
                let (first, count) = layer_segments(&params, id).expect("active");
                let align_syms = rx_alignment_symbols(&params, id);
                let per_sym = nc * l.segments * l.modulation.bits_per_symbol();
                LayerRx {
                    layer: id,
                    demapper: Demapper::new(l.modulation),
                    s: l.modulation.bits_per_symbol(),
                    bitdi: BitDeinterleaver::new(l.modulation),
                    align: Fifo::new(align_syms * per_sym, 0.0),
                    out: Vec::with_capacity(per_sym * SYMBOLS_PER_FRAME),
                    llr_tmp: vec![0.0; per_sym],
                    mer_err: 0.0,
                    mer_n: 0,
                    first,
                    count,
                }
            })
            .collect();
        let seg_diff: Vec<bool> = (0..13).map(|s| params.segment_is_differential(s)).collect();
        let lookahead = chanest::lookahead(self.cfg.sp_time_window);
        // Processing restarts one symbol before the frame start: that
        // symbol only serves as the DQPSK reference of symbol 0 (it may be
        // noise if the signal began with this frame; it is excluded from
        // channel, noise and timing estimation).
        let first_j = f.saturating_sub(1);
        // Sampling-clock drift measured on the stored (uncorrected) symbols
        // of the locked frame; processing restarts from the buffered samples
        // with the timing and resampling rate corrected for it.
        let cont = continual_list(&layout);
        let n = params.mode().fft_size();
        let kc = (layout.k_total / 2) as f64;
        let drifts: Vec<f64> = acq.spectra[f..]
            .windows(2)
            .map(|w| timing_drift(&cont, kc, n, &w[1], &w[0]))
            .collect();
        let d0 = if drifts.is_empty() {
            0.0
        } else {
            drifts.iter().sum::<f64>() / drifts.len() as f64
        };
        let mut ex = acq.ex;
        let p = (ex.n + ex.ng) as f64;
        ex.rate = 1.0 + d0 / p;
        ex.pos = acq.starts[first_j] as f64 + first_j as f64 * d0;
        let fi = FrequencyInterleaver::with_numbering(&params, self.cfg.rotation);
        let tdi = MultiDelay::new(rx_delays(&params), (C32::default(), 0.0));
        let est = ChannelEstimator::new(&layout);
        let n_ac = layout.ac.len();
        let track = Track {
            tdi,
            fi,
            est,
            layers,
            seg_diff,
            ring: VecDeque::new(),
            sp_w: self.cfg.sp_time_window,
            lookahead: lookahead as u64,
            cont,
            frame0: f as u64,
            next_j: first_j as u64,
            cpe: 0.0,
            cpe_frame_start: 0.0,
            drift: d0,
            tau: 0.0,
            last_corr: d0,
            timing_total: 0.0,
            n0: 0.0,
            nc,
            latency,
            frames_out: 0,
            bad_frames: 0,
            tmcc_soft: vec![[0.0; 3]; SYMBOLS_PER_FRAME],
            ac_soft: vec![vec![0.0; SYMBOLS_PER_FRAME]; n_ac],
            frame_start_sample: 0,
            cfo_int: acq.cfo_int.unwrap_or(0),
            gi_metric: acq.guess.metric,
            data: vec![(C32::default(), 0.0); 13 * nc],
            deint: vec![(C32::default(), 0.0); 13 * nc],
            ex,
            layout,
            params,
        };
        self.track = Some(Box::new(track));
    }

    fn step_track(&mut self, out: &mut Vec<DemodFrame>) -> bool {
        let base = self.base;
        let avail = base + self.buf.len() as u64;
        let t = self.track.as_mut().expect("track");
        if t.ex.needs() > avail {
            return false;
        }
        let start = t.ex.extract(&self.buf, base);
        let spec = t.ex.carriers();
        let j = t.next_j;
        t.next_j += 1;
        if let Some(fr) = t.push_symbol(j, start, spec) {
            // Lock supervision: re-acquire after 4 consecutive frames with
            // uncorrectable TMCC, or when a valid TMCC announces different
            // current parameters (parameter switching, §3.15.6.2).
            if fr.tmcc.parity.is_ok() {
                t.bad_frames = 0;
            } else {
                t.bad_frames += 1;
            }
            let changed = fr.tmcc.parity.is_ok()
                && fr
                    .tmcc
                    .info
                    .params(t.params.mode(), t.params.guard())
                    .is_ok_and(|p| p != t.params);
            let lost = t.bad_frames >= 4;
            out.push(fr);
            if changed || lost {
                let keep = t.ex.next_start();
                self.track = None;
                self.drop_before(keep);
                return true;
            }
        }
        // Keep the sample buffer short: drop everything before the next symbol.
        let keep = t.ex.next_start().saturating_sub(t.ex.ng as u64);
        self.drop_before(keep);
        true
    }
}

impl Track {
    fn push_symbol(&mut self, j: u64, start: u64, mut y: Vec<C32>) -> Option<DemodFrame> {
        // Common phase error from the continual carriers (squared DBPSK products).
        if let Some((_, _, prev)) = self.ring.back() {
            let rot = C32::from_polar(1.0, -self.cpe);
            let mut acc = C32::default();
            for &k in &self.cont {
                let p = y[k] * rot * prev[k].conj();
                acc += p * p;
            }
            let d = acc.arg() / 2.0;
            self.cpe += d;
        }
        let rot = C32::from_polar(1.0, -self.cpe);
        for v in y.iter_mut() {
            *v *= rot;
        }
        // Timing drift (sampling-clock offset) tracking (not across the
        // pre-roll symbol).
        if let Some((_, _, prev)) = self.ring.back().filter(|e| e.0 >= self.frame0) {
            let kc = (self.layout.k_total / 2) as f64;
            let delta = timing_drift(&self.cont, kc, self.ex.n, &y, prev);
            self.drift += (delta + self.last_corr - self.drift) / 32.0;
            self.tau += delta;
            // Step limited so the squared-phase measurement never wraps.
            let c = (self.drift + self.tau / 16.0).clamp(self.drift - 0.1, self.drift + 0.1);
            self.last_corr = c;
            self.timing_total += c;
            // The extractor already advanced by P·(rate − 1) beyond nominal
            // for the next symbol; resample at the new rate and shift by the
            // rest of the correction.
            let p = (self.ex.n + self.ex.ng) as f64;
            let already = (self.ex.rate - 1.0) * p;
            self.ex.rate = 1.0 + self.drift / p;
            self.ex.adjust(c - already);
        }
        self.ring.push_back((j, start, y));
        if self.ring.len() > 2 * self.lookahead as usize + 1 {
            self.ring.pop_front();
        }
        // Process symbol j − lookahead (SP time interpolation).
        if j < self.lookahead {
            return None;
        }
        let t = j - self.lookahead;
        if t < self.frame0 {
            return None;
        }
        self.process(t)
    }

    fn ring_get(&self, j: u64) -> Option<&(u64, u64, Vec<C32>)> {
        let first = self.ring.front()?.0;
        if j < first {
            return None;
        }
        self.ring.get((j - first) as usize)
    }

    fn process(&mut self, t: u64) -> Option<DemodFrame> {
        let n = ((t - self.frame0) % SYMBOLS_PER_FRAME as u64) as usize;
        let phase = n % 4;
        let (_, start, y) = self.ring_get(t).expect("current").clone();
        let y_prev = self.ring_get(t.wrapping_sub(1)).map(|e| e.2.clone());
        if n == 0 {
            self.frame_start_sample = start;
            self.cpe_frame_start = self.cpe;
            for l in &mut self.layers {
                l.out.clear();
                l.mer_err = 0.0;
                l.mer_n = 0;
            }
        }
        // Noise estimate from continual carriers.
        if let Some(yp) = y_prev.as_ref().filter(|_| t > self.frame0) {
            let mut e = 0f32;
            let mut cnt = 0;
            for &k in &self.cont {
                let p = y[k] * yp[k].conj();
                let s = if p.re >= 0.0 { 1.0 } else { -1.0 };
                e += (y[k] - yp[k] * s).norm_sqr();
                cnt += 1;
            }
            let n0 = e / (2.0 * cnt as f32);
            self.n0 = if self.n0 == 0.0 {
                n0
            } else {
                0.95 * self.n0 + 0.05 * n0
            };
            // TMCC / AC soft DBPSK values.
            if n >= 1 {
                self.tmcc_soft[n] = [
                    tmcc_soft(&self.layout, &y, yp, None),
                    tmcc_soft(&self.layout, &y, yp, Some(SegmentKind::Coherent)),
                    tmcc_soft(&self.layout, &y, yp, Some(SegmentKind::Differential)),
                ];
                for (i, a) in self.layout.ac.iter().enumerate() {
                    self.ac_soft[i][n] = (y[a.k as usize] * yp[a.k as usize].conj()).re;
                }
            }
        }
        let n0 = self.n0.max(1e-9);
        // Channel estimate for coherent segments.
        if !self.est.is_empty() {
            let ring = &self.ring;
            let frame0 = self.frame0;
            let first = ring.front().expect("ring").0;
            let get = |d: i32| -> Option<&[C32]> {
                let jj = t as i64 + d as i64;
                if jj < first.max(frame0) as i64 {
                    return None;
                }
                ring.get((jj as u64 - first) as usize)
                    .map(|e| e.2.as_slice())
            };
            self.est.estimate(get, phase, self.sp_w);
        }
        // Data carriers in data-segment order.
        let nc = self.nc;
        for (q, &k) in self.layout.data[phase].iter().enumerate() {
            let k = k as usize;
            let seg = q / nc;
            self.data[q] = if self.seg_diff[seg] {
                match &y_prev {
                    Some(yp) => dqpsk_differential(y[k], yp[k], n0),
                    None => (C32::default(), 0.0),
                }
            } else {
                let h = self.est.h[k];
                let hn = h.norm_sqr().max(1e-12);
                (y[k] / h, hn / n0)
            };
        }
        self.fi.deinterleave(&self.data, &mut self.deint);
        // MER per layer (before time deinterleaving; positions keep their layer).
        for l in &mut self.layers {
            for q in l.first * nc..(l.first + l.count) * nc {
                let (z, w) = self.deint[q];
                if w <= 0.0 {
                    continue;
                }
                let zz = if l.demapper.modulation() == Modulation::Dqpsk {
                    // Phase-only error of the differential product.
                    let m = z.norm();
                    if m <= 0.0 {
                        continue;
                    }
                    z / m
                } else {
                    z
                };
                let ideal = l.demapper.nearest(zz);
                l.mer_err += (zz - ideal).norm_sqr() as f64;
                l.mer_n += 1;
            }
        }
        // Time deinterleaving.
        let input = self.deint.clone();
        self.tdi.push(&input, &mut self.deint);
        // Per layer: demap, bit-deinterleave, align.
        for l in &mut self.layers {
            let s = l.s;
            for (idx, q) in (l.first * nc..(l.first + l.count) * nc).enumerate() {
                let (z, w) = self.deint[q];
                l.demapper
                    .demap(z, w, &mut l.llr_tmp[idx * s..(idx + 1) * s]);
            }
            l.bitdi.process(&mut l.llr_tmp);
            l.align.process(&mut l.llr_tmp);
            l.out.extend_from_slice(&l.llr_tmp);
        }
        if n == SYMBOLS_PER_FRAME - 1 {
            return Some(self.finish_frame());
        }
        None
    }

    fn finish_frame(&mut self) -> DemodFrame {
        let mut bits = [0u8; 204];
        let mut seg_type = [[0u8; 3]; 2];
        for n in 1..SYMBOLS_PER_FRAME {
            bits[n] = u8::from(self.tmcc_soft[n][0] < 0.0);
            if (17..20).contains(&n) {
                seg_type[0][n - 17] = u8::from(self.tmcc_soft[n][1] < 0.0);
                seg_type[1][n - 17] = u8::from(self.tmcc_soft[n][2] < 0.0);
            }
        }
        // Report the segment-type bits of segment 0's kind.
        let k0 = usize::from(self.layout.kinds[0] == SegmentKind::Differential);
        bits[17..20].copy_from_slice(&seg_type[k0]);
        let tmcc = decode_frame(&bits);
        let ac_bits: Vec<[u8; 204]> = self
            .ac_soft
            .iter()
            .map(|s| {
                let mut b = [0u8; 204];
                for n in 1..SYMBOLS_PER_FRAME {
                    b[n] = u8::from(s[n] < 0.0);
                }
                b
            })
            .collect();
        // Seismic word: soft-combine the AC carriers of segment 0.
        let mut comb = [0f32; 204];
        for (i, a) in self.layout.ac.iter().enumerate() {
            if a.seg == 0 {
                for (c, s) in comb.iter_mut().zip(&self.ac_soft[i]).skip(1) {
                    *c += s;
                }
            }
        }
        let mut sw = [0u8; 204];
        for n in 1..SYMBOLS_PER_FRAME {
            sw[n] = u8::from(comb[n] < 0.0);
        }
        let seismic = is_seismic_config([sw[1], sw[2], sw[3]]).then(|| SeismicWarning::decode(&sw));
        let mut llr = [None, None, None];
        let mut mer = [None; 3];
        for l in &mut self.layers {
            let v = std::mem::take(&mut l.out);
            llr[l.layer.index()] = Some(v);
            if l.mer_n > 0 {
                mer[l.layer.index()] = Some((-10.0 * (l.mer_err / l.mer_n as f64).log10()) as f32);
            }
        }
        let frame_number = self.frames_out;
        self.frames_out += 1;
        DemodFrame {
            frame_number,
            start_sample: self.frame_start_sample,
            tmcc,
            params: self.params.clone(),
            llr,
            valid: frame_number > self.latency as u64,
            latency_frames: self.latency,
            ac_bits,
            seismic,
            metrics: Metrics {
                cfo_carriers: self.ex.cfo,
                cfo_integer: self.cfo_int,
                gi_metric: self.gi_metric,
                mer_db: mer,
                n0: self.n0,
                cpe_drift: self.cpe - self.cpe_frame_start,
                timing_drift: self.drift,
                timing_correction: self.timing_total,
            },
        }
    }
}

/// Continual carriers of a layout: TMCC, AC, differential CPs, band edge.
fn continual_list(layout: &FrameLayout) -> Vec<usize> {
    layout
        .tmcc
        .iter()
        .map(|t| t.k as usize)
        .chain(layout.ac.iter().map(|a| a.k as usize))
        .chain(layout.cp.iter().map(|&k| k as usize))
        .chain(std::iter::once(layout.edge as usize))
        .collect()
}

/// Timing shift (samples) of spectrum `y` relative to `prev` from the
/// phase ramp of the squared differential products of the continual
/// carriers `cont` (squaring removes the DBPSK sign): a delay of δ samples
/// rotates carrier k by −2π(k−Kc)δ/N.  Weighted least-squares slope with
/// intercept (the intercept absorbs the common phase).
fn timing_drift(cont: &[usize], kc: f64, n: usize, y: &[C32], prev: &[C32]) -> f64 {
    let (mut sw, mut sx, mut sp, mut sxx, mut sxp) = (0f64, 0f64, 0f64, 0f64, 0f64);
    for &k in cont {
        let d = y[k] * prev[k].conj();
        let q = d * d;
        let w = q.norm() as f64;
        if w <= 0.0 {
            continue;
        }
        let x = k as f64 - kc;
        let ph = q.arg() as f64 / 2.0;
        sw += w;
        sx += w * x;
        sp += w * ph;
        sxx += w * x * x;
        sxp += w * x * ph;
    }
    if sw <= 0.0 {
        return 0.0;
    }
    let den = sxx - sx * sx / sw;
    if den <= 0.0 {
        return 0.0;
    }
    let slope = (sxp - sx * sp / sw) / den;
    -slope * n as f64 / (2.0 * std::f64::consts::PI)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Degenerate public settings are clamped: no panic on any input.
    #[test]
    fn degenerate_config_is_clamped() {
        let cfg = DemodConfig {
            detect_symbols: 0,
            continual_symbols: 0,
            sp_time_window: 0,
            timing_backoff: f32::NAN,
            max_cfo_carriers: Some(usize::MAX / 4),
            ..DemodConfig::default()
        };
        let mut d = Demodulator::new(cfg);
        let mut x = 1u32;
        let noise: Vec<C32> = (0..200_000)
            .map(|_| {
                x = x.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                C32::new(
                    (x >> 16) as f32 / 65536.0 - 0.5,
                    (x & 0xFFFF) as f32 / 65536.0 - 0.5,
                )
            })
            .collect();
        let _ = d.push(&noise);
        assert!(d.cfg.timing_backoff.is_finite());
        assert_eq!(d.cfg.sp_time_window, 1);
    }
}
