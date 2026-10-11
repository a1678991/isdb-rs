//! In-memory sinks and radio for tests of code that drives an [`IqSink`]
//! (feature `mock`).
//!
//! * [`MemorySink`]: records every sample, not real time.
//! * [`MockRadio`]: a [`RadioTx`] that consumes its queue on a thread at a
//!   fixed sample rate in transfers of [`MockRadio::TRANSFER`] samples,
//!   counting transfers that found the queue empty as underruns. Wrapped
//!   in a [`QueuedSink`](crate::QueuedSink) it is a real-time sink with
//!   the same prefill, back-pressure, drain and stop behaviour as a radio
//!   backend.

use crate::{C32, Cs8, IntFormat, IqSink, RadioTx, StreamCounters, TxStream};
use anyhow::Result;
use std::marker::PhantomData;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{TryRecvError, sync_channel};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// Records every sample written; not real time.
#[derive(Debug, Clone, Default)]
pub struct MemorySink {
    /// Everything written.
    pub samples: Arc<Mutex<Vec<C32>>>,
}

impl IqSink for MemorySink {
    fn write(&mut self, s: &[C32], _stop: &AtomicBool) -> Result<()> {
        self.samples
            .lock()
            .expect("not poisoned")
            .extend_from_slice(s);
        Ok(())
    }

    fn finish(&mut self, _aborted: bool) -> Result<String> {
        Ok(format!(
            "{} samples in memory",
            self.samples.lock().expect("not poisoned").len()
        ))
    }

    fn realtime(&self) -> bool {
        false
    }
}

/// Plain atomic stream counters of a [`MockRadio`].
#[derive(Debug, Default)]
pub struct MockCounters {
    /// Transfers made.
    pub buffers: AtomicU64,
    /// Transfers that found the queue empty.
    pub underruns: AtomicU64,
    /// Set when the stream ended (queue closed and empty).
    pub finished: AtomicBool,
}

impl StreamCounters for MockCounters {
    fn buffers(&self) -> u64 {
        self.buffers.load(Ordering::Relaxed)
    }
    fn underruns(&self) -> u64 {
        self.underruns.load(Ordering::Relaxed)
    }
    fn finished(&self) -> bool {
        self.finished.load(Ordering::SeqCst)
    }
}

/// In-memory radio taking samples of format `F`: a thread consumes the
/// queue at [`MockRadio::rate`] samples/s in transfers of
/// [`MockRadio::TRANSFER`] samples, recording what it consumes in
/// [`MockRadio::sent`].
pub struct MockRadio<F: IntFormat = Cs8> {
    /// Components "transmitted" (excluding underrun gaps).
    pub sent: Arc<Mutex<Vec<F::Component>>>,
    /// Streaming flag.
    pub streaming: Arc<AtomicBool>,
    /// Consumption rate in samples/s.
    pub rate: f64,
    /// Calls to [`RadioTx::start_tx`].
    pub starts: usize,
    worker: Option<JoinHandle<()>>,
    format: PhantomData<F>,
}

impl<F: IntFormat> std::fmt::Debug for MockRadio<F> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MockRadio")
            .field("format", &F::NAME)
            .field("rate", &self.rate)
            .field("starts", &self.starts)
            .field("streaming", &self.streaming.load(Ordering::SeqCst))
            .finish_non_exhaustive()
    }
}

impl<F: IntFormat> MockRadio<F> {
    /// Samples per transfer.
    pub const TRANSFER: usize = 131_072;

    /// Radio consuming `rate` samples/s.
    pub fn new(rate: f64) -> Self {
        Self {
            sent: Arc::default(),
            streaming: Arc::default(),
            rate,
            starts: 0,
            worker: None,
            format: PhantomData,
        }
    }
}

impl<F: IntFormat> RadioTx for MockRadio<F> {
    type Format = F;

    fn start_tx(&mut self, depth: usize) -> Result<TxStream<F::Component>> {
        self.starts += 1;
        let (tx, rx) = sync_channel::<Vec<F::Component>>(depth);
        let counters = Arc::new(MockCounters::default());
        let (c, sent, streaming, rate) = (
            counters.clone(),
            self.sent.clone(),
            self.streaming.clone(),
            self.rate,
        );
        streaming.store(true, Ordering::SeqCst);
        self.worker = Some(std::thread::spawn(move || {
            // Components per transfer (I and Q).
            let xfer = 2 * Self::TRANSFER;
            let mut pending: Vec<F::Component> = Vec::new();
            let mut pos = 0;
            let period = Duration::from_secs_f64(Self::TRANSFER as f64 / rate);
            let mut next = Instant::now();
            while streaming.load(Ordering::SeqCst) {
                let mut filled = 0;
                let mut starved = false;
                while filled < xfer {
                    if pos == pending.len() {
                        match rx.try_recv() {
                            Ok(v) => {
                                pending = v;
                                pos = 0;
                                continue;
                            }
                            Err(TryRecvError::Empty) => {
                                starved = true;
                                break;
                            }
                            Err(TryRecvError::Disconnected) => {
                                c.finished.store(true, Ordering::SeqCst);
                                streaming.store(false, Ordering::SeqCst);
                                return;
                            }
                        }
                    }
                    let n = (pending.len() - pos).min(xfer - filled);
                    sent.lock()
                        .expect("not poisoned")
                        .extend_from_slice(&pending[pos..pos + n]);
                    pos += n;
                    filled += n;
                }
                if starved {
                    c.underruns.fetch_add(1, Ordering::Relaxed);
                }
                c.buffers.fetch_add(1, Ordering::Relaxed);
                next += period;
                let now = Instant::now();
                if next > now {
                    std::thread::sleep(next - now);
                }
            }
        }));
        Ok(TxStream {
            sender: tx,
            counters: counters as Arc<dyn StreamCounters>,
        })
    }

    fn stop(&mut self) -> Result<()> {
        self.streaming.store(false, Ordering::SeqCst);
        if let Some(w) = self.worker.take() {
            let _ = w.join();
        }
        Ok(())
    }

    fn is_streaming(&mut self) -> bool {
        self.streaming.load(Ordering::SeqCst)
    }
}

impl<F: IntFormat> Drop for MockRadio<F> {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Quantizer, QueuedSink, TxOptions};

    fn ramp(n: usize, k: usize) -> Vec<C32> {
        (0..n)
            .map(|i| {
                let x = ((k * n + i) % 200) as f32 / 100.0 - 1.0;
                C32::new(x, -x)
            })
            .collect()
    }

    #[test]
    fn memory_sink_and_mock_radio_record_the_same() {
        let stop = AtomicBool::new(false);
        let mut m = MemorySink::default();
        let mut r = QueuedSink::new(
            MockRadio::<Cs8>::new(20e6),
            &TxOptions {
                prefill_frames: 2,
                frame_seconds: 0.005,
                backoff_db: 6.0,
            },
        )
        .unwrap();
        for k in 0..5 {
            let f = ramp(100_000, k);
            m.write(&f, &stop).unwrap();
            r.write(&f, &stop).unwrap();
        }
        assert!(m.finish(false).unwrap().contains("500000 samples"));
        let s = r.finish(false).unwrap();
        assert!(s.contains("queue drained"), "{s}");
        assert!(!r.radio_mut().is_streaming());
        assert_eq!(
            *r.radio().sent.lock().unwrap(),
            Quantizer::<Cs8>::new(6.0).convert(&m.samples.lock().unwrap())
        );
        assert!(r.realtime() && !m.realtime());
    }
}
