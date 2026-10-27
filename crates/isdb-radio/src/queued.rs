//! The device-neutral transmit path of a radio fed with buffers of integer
//! samples: [`QueuedSink`] over a [`RadioTx`].
//!
//! A backend implements [`RadioTx`] for its device (start a stream that
//! consumes a bounded queue of interleaved I/Q buffers on its own thread,
//! stop it, report whether the device still streams) and returns a
//! [`QueuedSink`] from [`crate::Backend::open_tx`]. The sink logic — start
//! after the prefill, back-pressure, stall detection, drain on finish, the
//! stop flag and the underrun counters — is shared by every such backend
//! and tested against [`crate::mock::MockRadio`].

use crate::{C32, IntFormat, IqSink, Quantizer, TxOptions};
use anyhow::{Result, bail};
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
    /// Queue of interleaved I/Q buffers (I first, any length). Dropping it
    /// ends the stream once the device has sent what is queued.
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

/// A transmitting radio fed with buffers of interleaved integer I/Q in the
/// sample format [`RadioTx::Format`].
pub trait RadioTx {
    /// The device's sample format.
    type Format: IntFormat;
    /// Start streaming with up to `depth` buffers queued ahead.
    fn start_tx(
        &mut self,
        depth: usize,
    ) -> Result<TxStream<<Self::Format as IntFormat>::Component>>;
    /// Stop streaming (without waiting for queued buffers).
    fn stop(&mut self) -> Result<()>;
    /// True while the device reports streaming.
    fn is_streaming(&mut self) -> bool;
}

/// Radio sink: quantises frames into the radio's sample format and queues
/// them for its stream.
///
/// * Streaming starts once [`TxOptions::prefill_frames`] frames are
///   converted, so the device never starts on an empty queue; a shorter
///   transmission is started by [`IqSink::finish`].
/// * The queue holds the prefill plus the frame being converted; a full
///   queue blocks [`IqSink::write`] (back-pressure paces the producer)
///   until the device consumes, the stop flag is set (the frame is
///   dropped) or no buffer is transferred for max(4 frames, 2 s) (an
///   error: the device stalled).
/// * [`IqSink::finish`] waits up to (prefill + 2) frames + 2 s for the
///   queue to drain (unless aborted), stops the device and fails if it
///   still reports streaming.
pub struct QueuedSink<R: RadioTx> {
    radio: R,
    quantizer: Quantizer<R::Format>,
    prefill: usize,
    pending: Vec<Vec<<R::Format as IntFormat>::Component>>,
    stream: Option<TxStream<<R::Format as IntFormat>::Component>>,
    counters: Option<Arc<dyn StreamCounters>>,
    frame_s: f64,
    stall: Duration,
    last_underruns: u64,
}

impl<R: RadioTx> std::fmt::Debug for QueuedSink<R> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("QueuedSink")
            .field("format", &<R::Format as IntFormat>::NAME)
            .field("prefill", &self.prefill)
            .field("pending", &self.pending.len())
            .field("stream", &self.stream)
            .field("frame_s", &self.frame_s)
            .finish_non_exhaustive()
    }
}

impl<R: RadioTx> QueuedSink<R> {
    /// New sink over a configured radio; fails if `opts` does not pass
    /// [`TxOptions::validate`].
    pub fn new(radio: R, opts: &TxOptions) -> Result<Self> {
        opts.validate()?;
        let frame_s = opts.frame_seconds;
        Ok(Self {
            radio,
            quantizer: Quantizer::new(opts.backoff_db),
            prefill: opts.prefill_frames.max(1),
            pending: Vec::new(),
            stream: None,
            counters: None,
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

    fn start(&mut self) -> Result<()> {
        // Queue: the prefill plus one frame being converted.
        let stream = self.radio.start_tx(self.prefill + 1)?;
        self.counters = Some(stream.counters.clone());
        self.stream = Some(stream);
        info!(
            prefill_frames = self.pending.len(),
            queue = self.prefill + 1,
            format = <R::Format as IntFormat>::NAME,
            "radio TX stream started with {} frame(s) converted ({} samples), queue of {} buffers",
            self.pending.len(),
            <R::Format as IntFormat>::NAME,
            self.prefill + 1
        );
        for b in std::mem::take(&mut self.pending) {
            self.send(b, &AtomicBool::new(false))?;
        }
        Ok(())
    }

    fn send(
        &mut self,
        mut b: Vec<<R::Format as IntFormat>::Component>,
        stop: &AtomicBool,
    ) -> Result<()> {
        let TxStream { sender, counters } = self.stream.as_ref().expect("streaming");
        let mut last_progress = Instant::now();
        let mut last_buffers = counters.buffers();
        loop {
            match sender.try_send(b) {
                Ok(()) => return Ok(()),
                Err(TrySendError::Disconnected(_)) => bail!("radio stream ended unexpectedly"),
                Err(TrySendError::Full(back)) => b = back,
            }
            if stop.load(Ordering::Relaxed) {
                debug!(
                    samples = b.len() / 2,
                    "frame of {} samples dropped: stopping",
                    b.len() / 2
                );
                return Ok(()); // dropped: we are stopping anyway
            }
            if counters.finished() {
                bail!("radio stream finished unexpectedly");
            }
            let n = counters.buffers();
            if n != last_buffers {
                last_buffers = n;
                last_progress = Instant::now();
            } else if last_progress.elapsed() > self.stall {
                bail!(
                    "radio stalled: no buffer transferred for {:.1} s",
                    self.stall.as_secs_f64()
                );
            }
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    fn underruns(&self) -> u64 {
        self.counters.as_ref().map_or(0, |c| c.underruns())
    }
}

impl<R: RadioTx> IqSink for QueuedSink<R> {
    fn write(&mut self, samples: &[C32], stop: &AtomicBool) -> Result<()> {
        let b = self.quantizer.convert(samples);
        if self.stream.is_none() {
            self.pending.push(b);
            if self.pending.len() >= self.prefill {
                self.start()?;
            }
            return Ok(());
        }
        self.send(b, stop)
    }

    fn status(&mut self) -> String {
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
            " | buffers {buffers} underruns {u} (+{d}) | {}",
            self.quantizer.describe()
        )
    }

    fn finish(&mut self, aborted: bool) -> Result<String> {
        let mut drained = "stopped immediately".to_string();
        if !aborted && self.stream.is_none() && !self.pending.is_empty() {
            // Short transmission: never reached the prefill.
            self.start()?;
        }
        if let Some(TxStream { sender, counters }) = self.stream.take()
            && !aborted
        {
            drop(sender); // the device ends the stream once the queue is empty
            let limit = Duration::from_secs_f64(self.frame_s * (self.prefill + 2) as f64 + 2.0);
            let t = Instant::now();
            while !counters.finished() && t.elapsed() < limit {
                std::thread::sleep(Duration::from_millis(10));
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
            "radio: {drained}; underruns {}; {}; device stopped streaming: {}",
            self.underruns(),
            self.quantizer.describe(),
            if streaming { "NO" } else { "yes" }
        );
        if streaming {
            bail!("{summary} — power-cycle the device");
        }
        Ok(summary)
    }

    fn realtime(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Cs8;
    use crate::mock::MockRadio;

    fn frame(n: usize, v: f32) -> Vec<C32> {
        vec![C32::new(v, -v); n]
    }

    fn sink(rate: f64, backoff_db: f32, prefill: usize, frame_s: f64) -> QueuedSink<MockRadio> {
        QueuedSink::new(
            MockRadio::new(rate),
            &TxOptions {
                prefill_frames: prefill,
                frame_seconds: frame_s,
                backoff_db,
            },
        )
        .unwrap()
    }

    #[test]
    fn options_are_validated() {
        let ok = TxOptions {
            prefill_frames: 0,
            frame_seconds: 0.05,
            backoff_db: 10.0,
        };
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
        }
        let e = TxOptions {
            backoff_db: 41.0,
            ..ok
        }
        .validate()
        .unwrap_err()
        .to_string();
        assert_eq!(e, "backoff 41 dB outside 0..40");
    }

    #[test]
    fn queued_sink_prefills_streams_and_drains() {
        // 4 "frames" of 100k samples at 2 Msps: 0.2 s of air time.
        let mut s = sink(2e6, 0.0, 2, 0.05);
        let stop = AtomicBool::new(false);
        s.write(&frame(100_000, 0.1), &stop).unwrap();
        assert_eq!(s.radio().starts, 0, "waits for the prefill");
        for _ in 0..3 {
            s.write(&frame(100_000, 0.1), &stop).unwrap();
        }
        assert_eq!(s.radio().starts, 1);
        let summary = s.finish(false).unwrap();
        assert!(summary.contains("queue drained"), "{summary}");
        assert!(
            summary.contains("device stopped streaming: yes"),
            "{summary}"
        );
        let sent = s.radio().sent.lock().unwrap().clone();
        assert_eq!(sent.len(), 4 * 100_000 * 2);
        assert!(sent.chunks(2).all(|c| c == [13, -13]));
        assert!(!s.radio_mut().is_streaming());
    }

    #[test]
    fn queued_sink_short_transmission_and_abort() {
        let mut s = sink(2e6, 0.0, 4, 0.05);
        let stop = AtomicBool::new(false);
        s.write(&frame(1000, 0.1), &stop).unwrap();
        // Below the prefill: finish still transmits it.
        assert!(s.finish(false).unwrap().contains("stopped streaming: yes"));
        assert_eq!(s.radio().sent.lock().unwrap().len(), 2000);

        let mut s = sink(5e5, 0.0, 1, 0.05);
        for _ in 0..3 {
            s.write(&frame(100_000, 0.1), &stop).unwrap();
        }
        // Abort: stops without waiting for the 3 s queued.
        let t = Instant::now();
        let summary = s.finish(true).unwrap();
        assert!(t.elapsed() < Duration::from_millis(500));
        assert!(summary.contains("stopped immediately"));
        assert!(!s.radio_mut().is_streaming());
    }

    #[test]
    fn queued_sink_counts_underruns() {
        let mut s = sink(4e6, 0.0, 1, 0.01);
        let stop = AtomicBool::new(false);
        s.write(&frame(10_000, 0.1), &stop).unwrap();
        std::thread::sleep(Duration::from_millis(100));
        let st = s.status();
        assert!(!st.contains("underruns 0 "), "{st}");
        s.finish(false).unwrap();
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

    #[test]
    fn queued_sink_stop_flag_releases_back_pressure() {
        // A full queue blocks `write` until the radio consumes (10 frames
        // of 0.2 s here would take ≈ 2 s); with the stop flag set `write`
        // returns at once and drops the frame.
        let mut s = sink(5e5, 0.0, 1, 0.05);
        let stop = AtomicBool::new(false);
        for _ in 0..3 {
            s.write(&frame(100_000, 0.1), &stop).unwrap();
        }
        stop.store(true, Ordering::SeqCst);
        let t = Instant::now();
        for _ in 0..10 {
            s.write(&frame(100_000, 0.1), &stop).unwrap();
        }
        assert!(t.elapsed() < Duration::from_millis(500));
        assert!(s.finish(true).unwrap().contains("stopped immediately"));
    }
}
