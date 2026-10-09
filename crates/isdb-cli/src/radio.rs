//! HackRF streaming for `isdb tx` / `isdb rx`.
//!
//! The transmit path is written against the [`RadioTx`] trait so the whole
//! sink logic (start after prefill, back-pressure, stall detection, drain,
//! stop confirmation) is tested with an in-memory radio; [`isdb_hackrf::Device`]
//! implements it for the real thing.

use crate::phyargs::Tuning;
use crate::sink::{Cs8Scaler, IqSink};
use anyhow::{Context, Result, bail};
use isdb_hackrf::Counters;
use isdbt_ofdm::C32;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{SyncSender, TrySendError};
use std::time::{Duration, Instant};

/// A transmitting radio taking interleaved cs8 buffers.
pub trait RadioTx {
    /// Start streaming with `depth` buffers queued ahead.
    fn start_tx(&mut self, depth: usize) -> Result<(SyncSender<Vec<i8>>, Arc<Counters>)>;
    /// Stop streaming.
    fn stop(&mut self) -> Result<()>;
    /// True while the device reports streaming.
    fn is_streaming(&mut self) -> bool;
}

impl RadioTx for isdb_hackrf::Device {
    fn start_tx(&mut self, depth: usize) -> Result<(SyncSender<Vec<i8>>, Arc<Counters>)> {
        let s = isdb_hackrf::Device::start_tx(self, depth)?;
        Ok((s.sender, s.counters))
    }
    fn stop(&mut self) -> Result<()> {
        Ok(isdb_hackrf::Device::stop(self)?)
    }
    fn is_streaming(&mut self) -> bool {
        isdb_hackrf::Device::is_streaming(self)
    }
}

/// HackRF transmit settings.
#[derive(Debug, Clone)]
pub struct TxRfSettings {
    /// Frequencies and sample clock.
    pub tuning: Tuning,
    /// TX IF gain 0–47 dB.
    pub txvga_db: u32,
    /// RF amplifier (~+11 dB).
    pub amp: bool,
    /// Baseband filter request.
    pub bb_filter_hz: u32,
    /// Digital backoff of the OFDM RMS below cs8 full scale.
    pub backoff_db: f32,
    /// Serial number filter.
    pub serial: Option<String>,
}

impl TxRfSettings {
    /// Validate ranges.
    pub fn check(&self) -> Result<()> {
        if self.txvga_db > 47 {
            bail!("--txvga {} outside 0..47 dB", self.txvga_db);
        }
        if !(0.0..=40.0).contains(&self.backoff_db) {
            bail!("--backoff-db {} outside 0..40", self.backoff_db);
        }
        Ok(())
    }

    /// Multi-line description printed before transmitting.
    pub fn describe(&self) -> String {
        format!(
            "  device       HackRF {}\n  tuning       {}\n  bb filter    {:.2} MHz requested\n  TX VGA       {} dB\n  RF amp       {}\n  backoff      {:.1} dB below cs8 full scale (RMS {:.1} of 127)",
            self.serial.as_deref().unwrap_or("(first found)"),
            self.tuning.describe(),
            self.bb_filter_hz as f64 / 1e6,
            self.txvga_db,
            if self.amp { "ON (+~11 dB)" } else { "off" },
            self.backoff_db,
            127.0 * 10f32.powf(-self.backoff_db / 20.0)
        )
    }
}

/// Open and configure a HackRF for transmission (not yet streaming).
pub fn open_tx(s: &TxRfSettings) -> Result<(isdb_hackrf::Device, String)> {
    let mut d = isdb_hackrf::Device::open(s.serial.as_deref()).context("open HackRF")?;
    let info = device_info(&mut d);
    d.set_antenna_enable(false)?;
    d.set_amp_enable(false)?;
    d.set_sample_rate_manual(s.tuning.rate_num, s.tuning.rate_den)?;
    let bw = d.set_baseband_filter(s.bb_filter_hz)?;
    d.set_freq(s.tuning.set_hz)?;
    d.set_txvga_gain(s.txvga_db)?;
    d.set_amp_enable(s.amp)?;
    Ok((
        d,
        format!("{info}; baseband filter {:.2} MHz", bw as f64 / 1e6),
    ))
}

/// HackRF receive settings.
#[derive(Debug, Clone)]
pub struct RxRfSettings {
    /// Frequencies and sample clock.
    pub tuning: Tuning,
    /// LNA gain 0–40 dB (8 dB steps).
    pub lna_db: u32,
    /// Baseband VGA gain 0–62 dB (2 dB steps).
    pub vga_db: u32,
    /// RF amplifier.
    pub amp: bool,
    /// Baseband filter request.
    pub bb_filter_hz: u32,
    /// Serial number filter.
    pub serial: Option<String>,
}

impl RxRfSettings {
    /// Validate ranges.
    pub fn check(&self) -> Result<()> {
        if self.lna_db > 40 || !self.lna_db.is_multiple_of(8) {
            bail!("--lna {} must be 0..40 in 8 dB steps", self.lna_db);
        }
        if self.vga_db > 62 || !self.vga_db.is_multiple_of(2) {
            bail!("--vga {} must be 0..62 in 2 dB steps", self.vga_db);
        }
        Ok(())
    }

    /// Multi-line description.
    pub fn describe(&self) -> String {
        format!(
            "  device       HackRF {}\n  tuning       {}\n  bb filter    {:.2} MHz requested\n  LNA / VGA    {} / {} dB\n  RF amp       {}",
            self.serial.as_deref().unwrap_or("(first found)"),
            self.tuning.describe(),
            self.bb_filter_hz as f64 / 1e6,
            self.lna_db,
            self.vga_db,
            if self.amp { "ON" } else { "off" }
        )
    }
}

/// Open and configure a HackRF for reception (not yet streaming).
pub fn open_rx(s: &RxRfSettings) -> Result<(isdb_hackrf::Device, String)> {
    let mut d = isdb_hackrf::Device::open(s.serial.as_deref()).context("open HackRF")?;
    let info = device_info(&mut d);
    d.set_antenna_enable(false)?;
    d.set_sample_rate_manual(s.tuning.rate_num, s.tuning.rate_den)?;
    let bw = d.set_baseband_filter(s.bb_filter_hz)?;
    d.set_freq(s.tuning.set_hz)?;
    d.set_lna_gain(s.lna_db)?;
    d.set_vga_gain(s.vga_db)?;
    d.set_amp_enable(s.amp)?;
    Ok((
        d,
        format!("{info}; baseband filter {:.2} MHz", bw as f64 / 1e6),
    ))
}

fn device_info(d: &mut isdb_hackrf::Device) -> String {
    let fw = d.firmware_version().unwrap_or_else(|e| format!("? ({e})"));
    let board = d
        .board_id()
        .map(|b| b.to_string())
        .unwrap_or_else(|e| format!("? ({e})"));
    format!(
        "libhackrf {}, board id {board}, firmware {fw}",
        isdb_hackrf::library_version()
    )
}

/// Radio sink: converts frames to cs8 and streams them.  Streaming starts
/// once `prefill` frames are converted, so the device never starts on an
/// empty queue.
pub struct RadioSink<R: RadioTx> {
    radio: R,
    scaler: Cs8Scaler,
    prefill: usize,
    pending: Vec<Vec<i8>>,
    stream: Option<(SyncSender<Vec<i8>>, Arc<Counters>)>,
    counters: Option<Arc<Counters>>,
    frame_s: f64,
    stall: Duration,
    last_underruns: u64,
}

impl<R: RadioTx> RadioSink<R> {
    /// New sink over a configured radio.
    pub fn new(radio: R, backoff_db: f32, prefill: usize, frame_s: f64) -> Self {
        Self {
            radio,
            scaler: Cs8Scaler::new(backoff_db),
            prefill: prefill.max(1),
            pending: Vec::new(),
            stream: None,
            counters: None,
            frame_s,
            stall: Duration::from_secs_f64((frame_s * 4.0).max(2.0)),
            last_underruns: 0,
        }
    }

    /// Access the radio (tests).
    #[cfg(test)]
    pub fn radio(&mut self) -> &mut R {
        &mut self.radio
    }

    fn start(&mut self) -> Result<()> {
        // Queue: the prefill plus one frame being converted.
        let (sender, counters) = self.radio.start_tx(self.prefill + 1)?;
        self.counters = Some(counters.clone());
        self.stream = Some((sender, counters));
        for b in std::mem::take(&mut self.pending) {
            self.send(b, &AtomicBool::new(false))?;
        }
        Ok(())
    }

    fn send(&mut self, mut b: Vec<i8>, stop: &AtomicBool) -> Result<()> {
        let (sender, counters) = self.stream.as_ref().expect("streaming");
        let mut last_progress = Instant::now();
        let mut last_buffers = counters.buffers.load(Ordering::Relaxed);
        loop {
            match sender.try_send(b) {
                Ok(()) => return Ok(()),
                Err(TrySendError::Disconnected(_)) => bail!("radio stream ended unexpectedly"),
                Err(TrySendError::Full(back)) => b = back,
            }
            if stop.load(Ordering::Relaxed) {
                return Ok(()); // dropped: we are stopping anyway
            }
            if counters.finished.load(Ordering::Relaxed) {
                bail!("radio stream finished unexpectedly");
            }
            let n = counters.buffers.load(Ordering::Relaxed);
            if n != last_buffers {
                last_buffers = n;
                last_progress = Instant::now();
            } else if last_progress.elapsed() > self.stall {
                bail!(
                    "radio stalled: no USB transfer for {:.1} s",
                    self.stall.as_secs_f64()
                );
            }
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    fn underruns(&self) -> u64 {
        self.counters
            .as_ref()
            .map_or(0, |c| c.underruns.load(Ordering::Relaxed))
    }
}

impl<R: RadioTx> IqSink for RadioSink<R> {
    fn write(&mut self, samples: &[C32], stop: &AtomicBool) -> Result<()> {
        let b = self.scaler.convert(samples);
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
        let buffers = self
            .counters
            .as_ref()
            .map_or(0, |c| c.buffers.load(Ordering::Relaxed));
        format!(
            " | usb buffers {buffers} underruns {u} (+{d}) | {}",
            self.scaler.describe()
        )
    }

    fn finish(&mut self, aborted: bool) -> Result<String> {
        let mut drained = "stopped immediately".to_string();
        if !aborted && self.stream.is_none() && !self.pending.is_empty() {
            // Short transmission: never reached the prefill.
            self.start()?;
        }
        if let Some((sender, counters)) = self.stream.take()
            && !aborted
        {
            {
                drop(sender); // the callback ends the stream once the queue is empty
                let limit = Duration::from_secs_f64(self.frame_s * (self.prefill + 2) as f64 + 2.0);
                let t = Instant::now();
                while !counters.finished.load(Ordering::Relaxed) && t.elapsed() < limit {
                    std::thread::sleep(Duration::from_millis(10));
                }
                drained = if counters.finished.load(Ordering::Relaxed) {
                    format!("queue drained in {:.2} s", t.elapsed().as_secs_f64())
                } else {
                    "queue did not drain in time; stopped".into()
                };
            }
        }
        self.radio.stop()?;
        let streaming = self.radio.is_streaming();
        let summary = format!(
            "radio: {drained}; underruns {}; {}; device stopped streaming: {}",
            self.underruns(),
            self.scaler.describe(),
            if streaming { "NO" } else { "yes" }
        );
        if streaming {
            bail!("{summary} — power-cycle the HackRF");
        }
        Ok(summary)
    }

    fn realtime(&self) -> bool {
        true
    }
}

/// In-memory radio (tests): a thread consumes buffers at `rate` samples/s
/// in USB-sized transfers, counting underruns like the libhackrf callback.
#[cfg(test)]
pub mod mock {
    use super::*;
    use std::sync::Mutex;
    use std::sync::mpsc::{TryRecvError, sync_channel};

    /// Mock radio.
    #[derive(Default)]
    pub struct MockRadio {
        /// Bytes "transmitted" (excluding underrun zeros).
        pub sent: Arc<Mutex<Vec<i8>>>,
        /// Streaming flag.
        pub streaming: Arc<AtomicBool>,
        /// Consumption rate in samples/s.
        pub rate: f64,
        /// Calls to `start_tx`.
        pub starts: usize,
        worker: Option<std::thread::JoinHandle<()>>,
    }

    impl MockRadio {
        /// Consume at `rate` samples/s.
        pub fn new(rate: f64) -> Self {
            Self {
                rate,
                ..Default::default()
            }
        }
    }

    impl RadioTx for MockRadio {
        fn start_tx(&mut self, depth: usize) -> Result<(SyncSender<Vec<i8>>, Arc<Counters>)> {
            self.starts += 1;
            let (tx, rx) = sync_channel::<Vec<i8>>(depth);
            let counters = Arc::new(Counters::default());
            let (c, sent, streaming, rate) = (
                counters.clone(),
                self.sent.clone(),
                self.streaming.clone(),
                self.rate,
            );
            streaming.store(true, Ordering::SeqCst);
            self.worker = Some(std::thread::spawn(move || {
                const XFER: usize = 262_144; // bytes per USB transfer
                let mut pending: Vec<i8> = Vec::new();
                let mut pos = 0;
                let period = Duration::from_secs_f64(XFER as f64 / 2.0 / rate);
                let mut next = Instant::now();
                while streaming.load(Ordering::SeqCst) {
                    let mut filled = 0;
                    let mut starved = false;
                    while filled < XFER {
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
                        let n = (pending.len() - pos).min(XFER - filled);
                        sent.lock()
                            .unwrap()
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
            Ok((tx, counters))
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
}

#[cfg(test)]
mod tests {
    use super::mock::MockRadio;
    use super::*;

    fn frame(n: usize, v: f32) -> Vec<C32> {
        vec![C32::new(v, -v); n]
    }

    #[test]
    fn radio_sink_prefills_streams_and_drains() {
        // 4 "frames" of 100k samples at 2 Msps: 0.2 s of air time.
        let mut s = RadioSink::new(MockRadio::new(2e6), 0.0, 2, 0.05);
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
        assert!(!s.radio().is_streaming());
    }

    #[test]
    fn radio_sink_short_transmission_and_abort() {
        let mut s = RadioSink::new(MockRadio::new(2e6), 0.0, 4, 0.05);
        let stop = AtomicBool::new(false);
        s.write(&frame(1000, 0.1), &stop).unwrap();
        // Below the prefill: finish still transmits it.
        assert!(s.finish(false).unwrap().contains("stopped streaming: yes"));
        assert_eq!(s.radio().sent.lock().unwrap().len(), 2000);

        let mut s = RadioSink::new(MockRadio::new(5e5), 0.0, 1, 0.05);
        for _ in 0..3 {
            s.write(&frame(100_000, 0.1), &stop).unwrap();
        }
        // Abort: stops without waiting for the 3 s queued.
        let t = Instant::now();
        let summary = s.finish(true).unwrap();
        assert!(t.elapsed() < Duration::from_millis(500));
        assert!(summary.contains("stopped immediately"));
        assert!(!s.radio().is_streaming());
    }

    #[test]
    fn radio_sink_counts_underruns() {
        let mut s = RadioSink::new(MockRadio::new(4e6), 0.0, 1, 0.01);
        let stop = AtomicBool::new(false);
        s.write(&frame(10_000, 0.1), &stop).unwrap();
        std::thread::sleep(Duration::from_millis(100));
        let st = s.status();
        assert!(!st.contains("underruns 0 "), "{st}");
        s.finish(false).unwrap();
    }
}
