//! The device-neutral transmit path of a radio fed with buffers of integer
//! components: [`QueuedSink`] over a [`RadioTx`], converting with an
//! [`Encode`]r.
//!
//! A backend implements [`RadioTx`] for its device (start a stream that
//! consumes a bounded queue of interleaved buffers on its own thread,
//! stop it, report whether the device still streams, and optionally the
//! hooks around the stream) and returns a [`QueuedSink`] from
//! [`crate::Backend::open_tx`] (with a [`Quantizer`] of unit-RMS samples:
//! an [`IqSink`]) or [`crate::Backend::open_tx_symbols`] (with a
//! [`crate::CodeEncoder`] of ISDB-S symbol codes: a [`SymbolSink`]). The
//! sink logic — start after the prefill, back-pressure, stall detection,
//! drain on finish, the stop flag and the underrun counters — is shared by
//! every such backend and both encoders, and tested against
//! [`crate::mock::MockRadio`].

use crate::{
    C32, Encode, IntFormat, IqSink, Quantizer, RadioError, Result, StreamProblem, SymbolSink,
    TxOptions,
};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{SyncSender, TrySendError};
use std::time::{Duration, Instant};
use tracing::{debug, info, warn};

/// Live counters of a TX stream, shared with the thread feeding the device.
pub trait StreamCounters: Send + Sync {
    /// Buffers handed to the device.
    fn buffers(&self) -> u64;
    /// Buffers sent as zeros because nothing was queued.
    fn underruns(&self) -> u64;
    /// The stream ended (queue closed and empty, or stop requested).
    fn finished(&self) -> bool;
}

/// A started TX stream with components of type `C`.
pub struct TxStream<C> {
    /// Queue of interleaved buffers (one component per channel per
    /// sample, I first, any length). Dropping it ends the stream once the
    /// device has sent what is queued.
    pub sender: SyncSender<Vec<C>>,
    /// Live counters.
    pub counters: Arc<dyn StreamCounters>,
}

impl<C> std::fmt::Debug for TxStream<C> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TxStream")
            .field("buffers", &self.counters.buffers())
            .field("underruns", &self.counters.underruns())
            .field("finished", &self.counters.finished())
            .finish_non_exhaustive()
    }
}

/// The buffer component of radio `R`.
type Component<R> = <<R as RadioTx>::Format as IntFormat>::Component;

/// A transmitting radio fed with buffers of interleaved integer
/// components: I/Q in the sample format [`RadioTx::Format`], or anything
/// else an [`Encode`]r packs into the same components (the symbol codes
/// of an FPGA transmit block).
pub trait RadioTx {
    /// The device's IQ sample format; its component is the element of the
    /// stream's buffers, whatever they carry.
    type Format: IntFormat;
    /// Start streaming with up to `depth` buffers queued ahead.
    fn start_tx(&mut self, depth: usize) -> Result<TxStream<Component<Self>>>;
    /// Stop streaming (without waiting for queued buffers).
    fn stop(&mut self) -> Result<()>;
    /// True while the device reports streaming.
    fn is_streaming(&mut self) -> bool;
    /// Components the device takes ahead of real time once it streams
    /// (its kernel queue and the buffer being filled): [`QueuedSink`]
    /// converts that much, and at least [`TxOptions::prefill_frames`]
    /// frames, before it starts the stream, so that the device's queue
    /// fills from converted frames instead of running dry while it fills.
    /// Default 0.
    fn ahead(&self) -> usize {
        0
    }
    /// Called once the stream runs and the prefill is queued (default:
    /// nothing): a device that must be told to start consuming does it
    /// here.
    fn started(&mut self, counters: &dyn StreamCounters) -> Result<()> {
        let _ = counters;
        Ok(())
    }
    /// The device's fragment of a status line (default empty). Called once
    /// per status line, so it may read device registers.
    fn status(&mut self) -> String {
        String::new()
    }
    /// The device's fragment of the summary (default empty), read after
    /// the queue drained (at once when aborted) and before
    /// [`RadioTx::stop`], while the device still streams.
    fn summary(&mut self) -> String {
        String::new()
    }
}

/// Radio sink: converts what is written with an [`Encode`]r and queues it
/// for the radio's stream. With the default encoder, a [`Quantizer`] into
/// the radio's sample format, it is an [`IqSink`]
/// ([`QueuedSink::new`]); with a [`crate::CodeEncoder`] it is a
/// [`SymbolSink`] ([`QueuedSink::with_encoder`]).
///
/// * Streaming starts once [`TxOptions::prefill_frames`] frames, and at
///   least [`RadioTx::ahead`] components, are converted, so the device
///   never starts on an empty queue; a shorter transmission is started by
///   `finish`.
/// * The queue holds the prefill plus the frame being converted; a full
///   queue blocks `write` (back-pressure paces the producer) until the
///   device consumes, the stop flag is set (the frame is dropped) or no
///   buffer is transferred for max(4 frames, 2 s) (an error: the device
///   stalled).
/// * `finish` queues what the encoder still holds, waits for the queue to
///   drain (unless aborted; at most one frame more than the queue holds,
///   plus 2 s), takes the device's summary ([`RadioTx::summary`]), stops
///   the device and fails if it still reports streaming.
pub struct QueuedSink<R, E = Quantizer<<R as RadioTx>::Format>>
where
    R: RadioTx,
    E: Encode<Component = Component<R>>,
{
    radio: R,
    encoder: E,
    prefill: usize,
    pending: Vec<Vec<Component<R>>>,
    /// Components in `pending`.
    pending_components: usize,
    stream: Option<TxStream<Component<R>>>,
    counters: Option<Arc<dyn StreamCounters>>,
    /// Buffers the stream's queue holds.
    depth: usize,
    frame_s: f64,
    stall: Duration,
    last_underruns: u64,
}

impl<R: RadioTx, E: Encode<Component = Component<R>>> std::fmt::Debug for QueuedSink<R, E> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("QueuedSink")
            .field("encoder", &self.encoder.name())
            .field("prefill", &self.prefill)
            .field("pending", &self.pending.len())
            .field("stream", &self.stream)
            .field("frame_s", &self.frame_s)
            .finish_non_exhaustive()
    }
}

impl<R: RadioTx> QueuedSink<R> {
    /// New sink over a configured radio, quantising unit-RMS samples into
    /// its format at [`TxOptions::backoff_db`]; fails if `opts` does not
    /// pass [`TxOptions::validate`].
    pub fn new(radio: R, opts: &TxOptions) -> Result<Self> {
        let q = Quantizer::new(opts.backoff_db);
        Self::with_encoder(radio, opts, q)
    }
}

impl<R: RadioTx, E: Encode<Component = Component<R>>> QueuedSink<R, E> {
    /// New sink over a configured radio converting with `encoder`; fails
    /// if `opts` does not pass [`TxOptions::validate`].
    pub fn with_encoder(radio: R, opts: &TxOptions, encoder: E) -> Result<Self> {
        opts.validate()?;
        let frame_s = opts.frame_seconds;
        let prefill = opts.prefill_frames.max(1);
        Ok(Self {
            radio,
            encoder,
            prefill,
            pending: Vec::new(),
            pending_components: 0,
            stream: None,
            counters: None,
            depth: prefill + 1,
            frame_s,
            stall: Duration::from_secs_f64((frame_s * 4.0).max(2.0)),
            last_underruns: 0,
        })
    }

    /// The radio.
    pub fn radio(&self) -> &R {
        &self.radio
    }

    /// The radio, mutably.
    pub fn radio_mut(&mut self) -> &mut R {
        &mut self.radio
    }

    /// The encoder.
    pub fn encoder(&self) -> &E {
        &self.encoder
    }

    fn start(&mut self) -> Result<()> {
        // Queue: the prefill (and what the device takes ahead) plus one
        // frame being converted.
        let depth = self.pending.len().max(self.prefill) + 1;
        let stream = self.radio.start_tx(depth)?;
        self.counters = Some(stream.counters.clone());
        self.depth = depth;
        info!(
            prefill_frames = self.pending.len(),
            queue = depth,
            format = self.encoder.name(),
            "radio TX stream started with {} frame(s) converted ({}), queue of {} buffers",
            self.pending.len(),
            self.encoder.name(),
            depth
        );
        // Kept as soon as it runs, so that finish() stops it even when
        // queueing the prefill fails.
        let stream = self.stream.insert(stream);
        for b in std::mem::take(&mut self.pending) {
            Self::send(stream, self.stall, b, &AtomicBool::new(false))?;
        }
        self.pending_components = 0;
        let counters = stream.counters.clone();
        self.radio.started(counters.as_ref())
    }

    /// Queue `b` on `stream`, waiting while the queue is full (up to
    /// `stall` without progress of the device).
    fn send(
        stream: &TxStream<Component<R>>,
        stall: Duration,
        mut b: Vec<Component<R>>,
        stop: &AtomicBool,
    ) -> Result<()> {
        let TxStream { sender, counters } = stream;
        let mut last_progress = Instant::now();
        let mut last_buffers = counters.buffers();
        loop {
            match sender.try_send(b) {
                Ok(()) => return Ok(()),
                Err(TrySendError::Disconnected(_)) => {
                    return Err(stream_error(StreamProblem::Closed));
                }
                Err(TrySendError::Full(back)) => b = back,
            }
            if stop.load(Ordering::Relaxed) {
                debug!(
                    components = b.len(),
                    "frame of {} components dropped: stopping",
                    b.len()
                );
                return Ok(()); // dropped: we are stopping anyway
            }
            if counters.finished() {
                return Err(stream_error(StreamProblem::Ended));
            }
            let n = counters.buffers();
            if n != last_buffers {
                last_buffers = n;
                last_progress = Instant::now();
            } else if last_progress.elapsed() > stall {
                return Err(stream_error(StreamProblem::Stalled {
                    seconds: stall.as_secs_f64(),
                }));
            }
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    /// Queue one converted buffer (nothing for an empty one), starting the
    /// stream once the prefill is reached.
    fn queue(&mut self, b: Vec<Component<R>>, stop: &AtomicBool) -> Result<()> {
        if b.is_empty() {
            return Ok(());
        }
        if let Some(stream) = &self.stream {
            return Self::send(stream, self.stall, b, stop);
        }
        self.pending_components += b.len();
        self.pending.push(b);
        if self.pending.len() >= self.prefill && self.pending_components >= self.radio.ahead() {
            self.start()?;
        }
        Ok(())
    }

    /// Convert `x` and queue it.
    fn put(&mut self, x: &[E::Input], stop: &AtomicBool) -> Result<()> {
        let mut b = Vec::new();
        self.encoder.encode(x, &mut b);
        self.queue(b, stop)
    }

    fn underruns(&self) -> u64 {
        self.counters.as_ref().map_or(0, |c| c.underruns())
    }

    fn status_line(&mut self) -> String {
        let u = self.underruns();
        let d = u - self.last_underruns;
        self.last_underruns = u;
        let buffers = self.counters.as_ref().map_or(0, |c| c.buffers());
        if d > 0 {
            warn!(
                underruns = u,
                new_underruns = d,
                buffers,
                "radio underran {d} time(s) since the last status ({u} in {buffers} buffers): zeros went on air"
            );
        }
        format!(
            " | buffers {buffers} underruns {u} (+{d}) | {}{}",
            self.encoder.describe(),
            self.radio.status()
        )
    }

    fn finish_stream(&mut self, aborted: bool) -> Result<String> {
        let mut drained = "stopped immediately".to_string();
        if !aborted {
            // A partial unit the encoder holds goes out last.
            let mut tail = Vec::new();
            self.encoder.flush(&mut tail);
            self.queue(tail, &AtomicBool::new(false))?;
            if self.stream.is_none() && !self.pending.is_empty() {
                // Short transmission: never reached the prefill.
                self.start()?;
            }
        }
        if let Some(TxStream { sender, counters }) = self.stream.take()
            && !aborted
        {
            drop(sender); // the device ends the stream once the queue is empty
            let limit = Duration::from_secs_f64(self.frame_s * (self.depth + 1) as f64 + 2.0);
            let t = Instant::now();
            while !counters.finished() && t.elapsed() < limit {
                std::thread::sleep(Duration::from_millis(2));
            }
            drained = if counters.finished() {
                format!("queue drained in {:.2} s", t.elapsed().as_secs_f64())
            } else {
                warn!(
                    limit_s = limit.as_secs_f64(),
                    buffers = counters.buffers(),
                    "radio TX queue did not drain in {:.1} s: stopping with samples queued",
                    limit.as_secs_f64()
                );
                "queue did not drain in time; stopped".into()
            };
        }
        let device = self.radio.summary();
        self.radio.stop()?;
        let streaming = self.radio.is_streaming();
        debug!(
            drained,
            underruns = self.underruns(),
            aborted,
            streaming,
            "radio TX stream stopped ({drained}): {} underrun(s){}",
            self.underruns(),
            if streaming {
                "; the device still reports streaming"
            } else {
                ""
            }
        );
        let summary = format!(
            "radio: {drained}; underruns {}; {}{device}; device stopped streaming: {}",
            self.underruns(),
            self.encoder.describe(),
            if streaming { "NO" } else { "yes" }
        );
        if streaming {
            return Err(RadioError::StillStreaming { summary });
        }
        Ok(summary)
    }
}

impl<R: RadioTx, E: Encode<Input = C32, Component = Component<R>>> IqSink for QueuedSink<R, E> {
    fn write(&mut self, samples: &[C32], stop: &AtomicBool) -> Result<()> {
        self.put(samples, stop)
    }

    fn status(&mut self) -> String {
        self.status_line()
    }

    fn finish(&mut self, aborted: bool) -> Result<String> {
        self.finish_stream(aborted)
    }

    fn realtime(&self) -> bool {
        true
    }
}

impl<R: RadioTx, E: Encode<Input = u8, Component = Component<R>>> SymbolSink for QueuedSink<R, E> {
    fn write(&mut self, codes: &[u8], stop: &AtomicBool) -> Result<()> {
        self.put(codes, stop)
    }

    fn status(&mut self) -> String {
        self.status_line()
    }

    fn finish(&mut self, aborted: bool) -> Result<String> {
        self.finish_stream(aborted)
    }

    fn realtime(&self) -> bool {
        true
    }
}

/// A [`RadioError::Stream`] of the TX queue.
fn stream_error(problem: StreamProblem) -> RadioError {
    RadioError::Stream {
        stream: "radio",
        problem,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mock::MockRadio;
    use crate::{CodeEncoder, Cs8, Cs12};
    use isdb_frontend::txformat;

    fn frame(n: usize, v: f32) -> Vec<C32> {
        vec![C32::new(v, -v); n]
    }

    /// `8·n` symbol codes (`n` DMA samples), zero symbols among the points.
    fn codes(n: usize) -> Vec<u8> {
        (0..8 * n)
            .map(|i| match i % 9 {
                8 => txformat::ZERO,
                k => txformat::point_code(u8::try_from(k).unwrap()),
            })
            .collect()
    }

    fn opts(backoff_db: f32, prefill: usize, frame_s: f64) -> TxOptions {
        TxOptions {
            prefill_frames: prefill,
            frame_seconds: frame_s,
            backoff_db,
        }
    }

    fn sink(rate: f64, backoff_db: f32, prefill: usize, frame_s: f64) -> QueuedSink<MockRadio> {
        QueuedSink::new(MockRadio::new(rate), &opts(backoff_db, prefill, frame_s)).unwrap()
    }

    /// One encoder's side of the shared sink tests: IQ frames through a
    /// quantiser ([`Iq`]), or symbol codes through the code encoder
    /// ([`Codes`]). `n` counts radio samples (for codes, DMA samples of
    /// eight codes).
    trait Kind {
        type F: IntFormat;
        type E: Encode<Component = <Self::F as IntFormat>::Component>;
        fn sink(rate: f64, prefill: usize, frame_s: f64) -> Sink<Self>;
        fn write(s: &mut Sink<Self>, n: usize, stop: &AtomicBool) -> Result<()>;
        fn status(s: &mut Sink<Self>) -> String;
        fn finish(s: &mut Sink<Self>, aborted: bool) -> Result<String>;
        /// Whether `sent` is what writes of `frames` frames of `n` became.
        fn check(sent: &[<Self::F as IntFormat>::Component], frames: usize, n: usize);
    }

    type Sink<K> = QueuedSink<MockRadio<<K as Kind>::F>, <K as Kind>::E>;

    struct Iq;
    struct Codes;

    impl Kind for Iq {
        type F = Cs8;
        type E = Quantizer<Cs8>;
        fn sink(rate: f64, prefill: usize, frame_s: f64) -> Sink<Self> {
            QueuedSink::new(MockRadio::new(rate), &opts(0.0, prefill, frame_s)).unwrap()
        }
        fn write(s: &mut Sink<Self>, n: usize, stop: &AtomicBool) -> Result<()> {
            IqSink::write(s, &frame(n, 0.1), stop)
        }
        fn status(s: &mut Sink<Self>) -> String {
            IqSink::status(s)
        }
        fn finish(s: &mut Sink<Self>, aborted: bool) -> Result<String> {
            IqSink::finish(s, aborted)
        }
        fn check(sent: &[i8], frames: usize, n: usize) {
            assert_eq!(sent.len(), frames * n * 2);
            // 0.1 · 127 = 12.7 → 13
            assert!(sent.chunks(2).all(|c| c == [13, -13]));
        }
    }

    impl Kind for Codes {
        type F = Cs12;
        type E = CodeEncoder;
        fn sink(rate: f64, prefill: usize, frame_s: f64) -> Sink<Self> {
            QueuedSink::with_encoder(
                MockRadio::new(rate),
                &opts(10.0, prefill, frame_s),
                CodeEncoder::new(),
            )
            .unwrap()
        }
        fn write(s: &mut Sink<Self>, n: usize, stop: &AtomicBool) -> Result<()> {
            SymbolSink::write(s, &codes(n), stop)
        }
        fn status(s: &mut Sink<Self>) -> String {
            SymbolSink::status(s)
        }
        fn finish(s: &mut Sink<Self>, aborted: bool) -> Result<String> {
            SymbolSink::finish(s, aborted)
        }
        fn check(sent: &[i16], frames: usize, n: usize) {
            assert_eq!(sent.len(), frames * n * 2);
            let back = unpack(sent);
            let one = codes(n);
            assert!(back.chunks(one.len()).all(|c| c == one));
        }
    }

    /// The codes of interleaved I/Q components (two per DMA sample).
    fn unpack(c: &[i16]) -> Vec<u8> {
        let words: Vec<u32> = c
            .as_chunks::<2>()
            .0
            .iter()
            .map(|&w| txformat::from_components(w))
            .collect();
        txformat::unpack(&words)
    }

    #[test]
    fn options_are_validated() {
        let ok = opts(10.0, 0, 0.05);
        // A prefill of 0 counts as 1.
        assert_eq!(
            QueuedSink::new(MockRadio::<Cs8>::new(1e6), &ok)
                .unwrap()
                .prefill,
            1
        );
        for bad in [
            TxOptions {
                frame_seconds: 0.0,
                ..ok
            },
            TxOptions {
                frame_seconds: -1.0,
                ..ok
            },
            TxOptions {
                frame_seconds: f64::NAN,
                ..ok
            },
            TxOptions {
                frame_seconds: f64::INFINITY,
                ..ok
            },
            TxOptions {
                backoff_db: -0.5,
                ..ok
            },
            TxOptions {
                backoff_db: 41.0,
                ..ok
            },
            TxOptions {
                backoff_db: f32::NAN,
                ..ok
            },
        ] {
            assert!(bad.validate().is_err(), "{bad:?}");
            assert!(
                QueuedSink::new(MockRadio::<Cs8>::new(1e6), &bad).is_err(),
                "{bad:?}"
            );
            assert!(
                QueuedSink::with_encoder(MockRadio::<Cs12>::new(1e6), &bad, CodeEncoder::new())
                    .is_err(),
                "{bad:?}"
            );
        }
        let e = TxOptions {
            backoff_db: 41.0,
            ..ok
        }
        .validate()
        .unwrap_err()
        .to_string();
        assert_eq!(e, "backoff 41 dB outside 0..40");
        let bad = TxOptions {
            backoff_db: 41.0,
            ..ok
        };
        assert!(matches!(
            bad.validate(),
            Err(RadioError::Backoff { db }) if db == 41.0
        ));
        let bad = TxOptions {
            frame_seconds: 0.0,
            ..ok
        };
        assert!(matches!(
            bad.validate(),
            Err(RadioError::Duration {
                what: "frame duration",
                ..
            })
        ));
        assert!(matches!(
            crate::RxOptions {
                queue_seconds: -1.0
            }
            .validate(),
            Err(RadioError::Duration {
                what: "RX queue",
                ..
            })
        ));
    }

    fn stopped_radio_is_a_stream_error<K: Kind>() {
        let mut s = K::sink(1e6, 1, 0.05);
        let stop = AtomicBool::new(false);
        K::write(&mut s, 1000, &stop).unwrap();
        s.radio_mut().stop().unwrap();
        // The device's queue is gone: the next frame cannot be queued.
        assert!(matches!(
            K::write(&mut s, 1000, &stop),
            Err(RadioError::Stream {
                stream: "radio",
                problem: StreamProblem::Closed
            })
        ));
    }

    #[test]
    fn a_stopped_radio_is_a_stream_error() {
        stopped_radio_is_a_stream_error::<Iq>();
        stopped_radio_is_a_stream_error::<Codes>();
    }

    /// A radio that never stops streaming (and never consumes).
    struct Stuck(Option<std::sync::mpsc::Receiver<Vec<i8>>>);

    impl RadioTx for Stuck {
        type Format = Cs8;
        fn start_tx(&mut self, depth: usize) -> Result<TxStream<i8>> {
            let (sender, receiver) = std::sync::mpsc::sync_channel(depth);
            self.0 = Some(receiver);
            Ok(TxStream {
                sender,
                counters: Arc::new(crate::mock::MockCounters::default()),
            })
        }
        fn stop(&mut self) -> Result<()> {
            Ok(())
        }
        fn is_streaming(&mut self) -> bool {
            true
        }
    }

    #[test]
    fn a_radio_still_streaming_after_finish_is_an_error() {
        let mut s = QueuedSink::new(Stuck(None), &opts(0.0, 1, 0.05)).unwrap();
        s.write(&frame(10, 0.1), &AtomicBool::new(false)).unwrap();
        match s.finish(true) {
            Err(RadioError::StillStreaming { summary }) => {
                assert!(
                    summary.contains("device stopped streaming: NO"),
                    "{summary}"
                );
            }
            other => panic!("{other:?}"),
        }
    }

    fn prefills_streams_and_drains<K: Kind>() {
        // 4 "frames" of 100k samples at 2 Msps: 0.2 s of air time.
        let mut s = K::sink(2e6, 2, 0.05);
        let stop = AtomicBool::new(false);
        K::write(&mut s, 100_000, &stop).unwrap();
        assert_eq!(s.radio().starts, 0, "waits for the prefill");
        for _ in 0..3 {
            K::write(&mut s, 100_000, &stop).unwrap();
        }
        assert_eq!(s.radio().starts, 1);
        let summary = K::finish(&mut s, false).unwrap();
        assert!(summary.contains("queue drained"), "{summary}");
        assert!(
            summary.contains("device stopped streaming: yes"),
            "{summary}"
        );
        K::check(&s.radio().sent.lock().unwrap(), 4, 100_000);
        assert!(!s.radio_mut().is_streaming());
    }

    #[test]
    fn queued_sink_prefills_streams_and_drains() {
        prefills_streams_and_drains::<Iq>();
        prefills_streams_and_drains::<Codes>();
    }

    fn short_transmission_and_abort<K: Kind>() {
        let mut s = K::sink(2e6, 4, 0.05);
        let stop = AtomicBool::new(false);
        K::write(&mut s, 1000, &stop).unwrap();
        // Below the prefill: finish still transmits it.
        assert!(
            K::finish(&mut s, false)
                .unwrap()
                .contains("stopped streaming: yes")
        );
        K::check(&s.radio().sent.lock().unwrap(), 1, 1000);

        let mut s = K::sink(5e5, 1, 0.05);
        for _ in 0..3 {
            K::write(&mut s, 100_000, &stop).unwrap();
        }
        // Abort: stops without waiting for the 3 s queued.
        let t = Instant::now();
        let summary = K::finish(&mut s, true).unwrap();
        assert!(t.elapsed() < Duration::from_millis(500));
        assert!(summary.contains("stopped immediately"));
        assert!(!s.radio_mut().is_streaming());
    }

    #[test]
    fn queued_sink_short_transmission_and_abort() {
        short_transmission_and_abort::<Iq>();
        short_transmission_and_abort::<Codes>();
    }

    fn counts_underruns<K: Kind>() {
        let mut s = K::sink(4e6, 1, 0.01);
        let stop = AtomicBool::new(false);
        K::write(&mut s, 10_000, &stop).unwrap();
        std::thread::sleep(Duration::from_millis(100));
        let st = K::status(&mut s);
        assert!(!st.contains("underruns 0 "), "{st}");
        K::finish(&mut s, false).unwrap();
    }

    #[test]
    fn queued_sink_counts_underruns() {
        counts_underruns::<Iq>();
        counts_underruns::<Codes>();
    }

    fn stop_flag_releases_back_pressure<K: Kind>() {
        // A full queue blocks `write` until the radio consumes (10 frames
        // of 0.2 s here would take ≈ 2 s); with the stop flag set `write`
        // returns at once and drops the frame.
        let mut s = K::sink(5e5, 1, 0.05);
        let stop = AtomicBool::new(false);
        for _ in 0..3 {
            K::write(&mut s, 100_000, &stop).unwrap();
        }
        stop.store(true, Ordering::SeqCst);
        let t = Instant::now();
        for _ in 0..10 {
            K::write(&mut s, 100_000, &stop).unwrap();
        }
        assert!(t.elapsed() < Duration::from_millis(500));
        assert!(
            K::finish(&mut s, true)
                .unwrap()
                .contains("stopped immediately")
        );
    }

    #[test]
    fn queued_sink_stop_flag_releases_back_pressure() {
        stop_flag_releases_back_pressure::<Iq>();
        stop_flag_releases_back_pressure::<Codes>();
    }

    fn back_pressure_paces_the_writer<K: Kind>() {
        // 8 frames of 50k samples at 1 Msps (0.4 s of air time) through a
        // queue of 2: the writes wait for the radio, which takes a transfer
        // of 131 072 samples (2.6 frames) every 131 ms, so the last write
        // returns only after its first transfer period at least.
        let mut s = K::sink(1e6, 1, 0.05);
        let stop = AtomicBool::new(false);
        let t = Instant::now();
        for _ in 0..8 {
            K::write(&mut s, 50_000, &stop).unwrap();
        }
        let took = t.elapsed();
        assert!(took > Duration::from_millis(100), "{took:?}");
        K::finish(&mut s, false).unwrap();
        K::check(&s.radio().sent.lock().unwrap(), 8, 50_000);
    }

    #[test]
    fn queued_sink_back_pressure_paces_the_writer() {
        back_pressure_paces_the_writer::<Iq>();
        back_pressure_paces_the_writer::<Codes>();
    }

    /// A device that takes components ahead of real time: the stream
    /// starts once that much is converted, not at the prefill frames, and
    /// the queue holds it.
    #[test]
    fn the_prefill_covers_what_the_device_takes_ahead() {
        let mut r = MockRadio::<Cs12>::new(1e6);
        r.ahead = 5 * 2 * 1000;
        let mut s = QueuedSink::with_encoder(r, &opts(10.0, 2, 0.001), CodeEncoder::new()).unwrap();
        let stop = AtomicBool::new(false);
        for _ in 0..4 {
            s.write(&codes(1000), &stop).unwrap();
            assert_eq!(s.radio().starts, 0);
        }
        s.write(&codes(1000), &stop).unwrap();
        assert_eq!((s.radio().starts, s.depth), (1, 6));
        s.finish(false).unwrap();
        Codes::check(&s.radio().sent.lock().unwrap(), 5, 1000);
    }

    #[test]
    fn queued_sink_sends_the_quantized_samples() {
        // What reaches the radio is exactly the quantisation with the
        // configured backoff (the bytes a cs8 file of the same signal holds).
        let x: Vec<C32> = (0..300_000)
            .map(|i| C32::new((i as f32 * 0.37).sin(), (i as f32 * 0.11).cos()))
            .collect();
        let mut s = sink(64e6, 10.0, 2, 0.01);
        let stop = AtomicBool::new(false);
        for c in x.chunks(100_000) {
            s.write(c, &stop).unwrap();
        }
        s.finish(false).unwrap();
        let sent = s.radio().sent.lock().unwrap().clone();
        assert_eq!(sent, Quantizer::<Cs8>::new(10.0).convert(&x));
    }

    /// The packer round trip through the sink: what reaches the radio
    /// unpacks to the codes written, in any chunking, the partial last DMA
    /// sample padded with zero symbols by `finish`.
    #[test]
    fn queued_sink_sends_the_packed_codes() {
        let x: Vec<u8> = codes(30_000).into_iter().chain([9, 10, 11]).collect();
        let mut s = QueuedSink::with_encoder(
            MockRadio::<Cs12>::new(64e6),
            &opts(10.0, 2, 0.001),
            CodeEncoder::new(),
        )
        .unwrap();
        let stop = AtomicBool::new(false);
        for c in x.chunks(10_007) {
            s.write(c, &stop).unwrap();
        }
        let summary = s.finish(false).unwrap();
        assert!(
            summary.contains(&format!("{} symbol codes in 30001 DMA samples", x.len())),
            "{summary}"
        );
        let back = unpack(&s.radio().sent.lock().unwrap());
        assert_eq!(back.len(), 8 * 30_001);
        assert_eq!(&back[..x.len()], x.as_slice());
        assert!(back[x.len()..].iter().all(|&c| c == txformat::ZERO));
    }

    /// A radio recording the hooks [`QueuedSink`] calls, around a mock.
    struct Hooked {
        inner: MockRadio<Cs12>,
        log: Vec<String>,
    }

    impl RadioTx for Hooked {
        type Format = Cs12;
        fn start_tx(&mut self, depth: usize) -> Result<TxStream<i16>> {
            self.log.push(format!("start {depth}"));
            self.inner.start_tx(depth)
        }
        fn stop(&mut self) -> Result<()> {
            self.log.push("stop".into());
            self.inner.stop()
        }
        fn is_streaming(&mut self) -> bool {
            self.inner.is_streaming()
        }
        fn started(&mut self, counters: &dyn StreamCounters) -> Result<()> {
            // the stream runs: its counters are the stream's
            assert!(!counters.finished());
            self.log.push("started".into());
            Ok(())
        }
        fn status(&mut self) -> String {
            self.log.push("status".into());
            " | device ok".into()
        }
        fn summary(&mut self) -> String {
            self.log.push("summary".into());
            "; device summary".into()
        }
    }

    /// `started` follows the start and the prefill, `status` and `summary`
    /// add their fragments, and the summary is taken before the stop.
    #[test]
    fn the_device_hooks_are_called_in_order() {
        let radio = Hooked {
            inner: MockRadio::new(1e6),
            log: Vec::new(),
        };
        let mut s =
            QueuedSink::with_encoder(radio, &opts(10.0, 2, 0.01), CodeEncoder::new()).unwrap();
        let stop = AtomicBool::new(false);
        for _ in 0..3 {
            s.write(&codes(1000), &stop).unwrap();
        }
        let st = s.status();
        assert!(st.ends_with("DMA samples | device ok"), "{st}");
        let summary = s.finish(false).unwrap();
        assert!(
            summary.ends_with("; device summary; device stopped streaming: yes"),
            "{summary}"
        );
        assert_eq!(
            s.radio().log,
            ["start 3", "started", "status", "summary", "stop"]
        );
    }

    /// One captured event: level, message, fields.
    type Captured = (
        tracing::Level,
        String,
        std::collections::BTreeMap<String, String>,
    );

    /// A `tracing` `Layer` that records the level, message and fields of
    /// the events emitted on one thread.
    ///
    /// It is installed as the global subscriber, not a scoped one: with a
    /// single scoped dispatcher, tracing-core caches a callsite's interest
    /// from whichever thread reaches it first, so a concurrent test without
    /// a subscriber (`queued_sink_counts_underruns`) disables it for this
    /// one.
    struct Capture {
        events: std::sync::Arc<std::sync::Mutex<Vec<Captured>>>,
        thread: std::thread::ThreadId,
    }

    impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for Capture {
        fn on_event(
            &self,
            event: &tracing::Event<'_>,
            _ctx: tracing_subscriber::layer::Context<'_, S>,
        ) {
            if std::thread::current().id() != self.thread {
                return;
            }
            struct V(String, std::collections::BTreeMap<String, String>);
            impl tracing::field::Visit for V {
                fn record_debug(&mut self, f: &tracing::field::Field, v: &dyn std::fmt::Debug) {
                    if f.name() == "message" {
                        self.0 = format!("{v:?}");
                    } else {
                        self.1.insert(f.name().to_string(), format!("{v:?}"));
                    }
                }
            }
            let mut v = V(String::new(), Default::default());
            event.record(&mut v);
            self.events
                .lock()
                .unwrap()
                .push((*event.metadata().level(), v.0, v.1));
        }
    }

    /// The stream start is an `INFO` event and the underruns since the
    /// last status a `WARN` event with their count as a field.
    #[test]
    fn stream_start_and_underruns_are_events() {
        use tracing_subscriber::layer::SubscriberExt;
        let events = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let subscriber = tracing_subscriber::registry().with(Capture {
            events: events.clone(),
            thread: std::thread::current().id(),
        });
        tracing::subscriber::set_global_default(subscriber)
            .expect("the only subscriber of this test binary");
        let mut s = sink(4e6, 0.0, 1, 0.01);
        let stop = AtomicBool::new(false);
        s.write(&frame(10_000, 0.1), &stop).unwrap();
        std::thread::sleep(Duration::from_millis(100));
        let st = s.status();
        s.finish(false).unwrap();
        let events = events.lock().unwrap();
        let started = events
            .iter()
            .find(|e| e.1.starts_with("radio TX stream started"))
            .unwrap_or_else(|| panic!("no start event in {events:?}"));
        assert_eq!(started.0, tracing::Level::INFO);
        assert_eq!(started.2["prefill_frames"], "1");
        let underran = events
            .iter()
            .find(|e| e.1.starts_with("radio underran"))
            .unwrap_or_else(|| panic!("no underrun event in {events:?}; status {st}"));
        assert_eq!(underran.0, tracing::Level::WARN);
        let n: u64 = underran.2["new_underruns"].parse().unwrap();
        assert!(n > 0 && st.contains(&format!("(+{n})")), "{n} vs {st}");
    }
}
