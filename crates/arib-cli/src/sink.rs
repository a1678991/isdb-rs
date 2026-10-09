//! Baseband sinks for `arib tx`: IQ files (cf32 / cs8), memory (tests) and
//! the radio (see [`crate::radio`]).

use anyhow::{Context, Result};
use isdbt_ofdm::C32;
use std::io::Write;
use std::sync::atomic::AtomicBool;
#[cfg(test)]
use std::sync::{Arc, Mutex};

/// IQ sample file format.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum IqFormat {
    /// Interleaved little-endian f32 I/Q (GNU Radio complex).
    Cf32,
    /// Interleaved signed 8-bit I/Q (HackRF / hackrf_transfer).
    Cs8,
}

/// Scaling of the unit-mean-power OFDM signal into signed 8-bit samples:
/// RMS amplitude = 127 · 10^(−backoff/20), with saturation counting.
#[derive(Debug, Clone)]
pub struct Cs8Scaler {
    /// Multiplier applied to the samples.
    pub scale: f32,
    /// Saturated I or Q components so far.
    pub clipped: u64,
    /// Components converted so far.
    pub components: u64,
}

impl Cs8Scaler {
    /// Scaler for `backoff_db` below full scale.
    pub fn new(backoff_db: f32) -> Self {
        Self {
            scale: 127.0 * 10f32.powf(-backoff_db / 20.0),
            clipped: 0,
            components: 0,
        }
    }

    /// Convert one block.
    pub fn convert(&mut self, s: &[C32]) -> Vec<i8> {
        let mut out = Vec::new();
        self.clipped += isdbt_phy::iq::cs8_from_samples(s, self.scale, &mut out) as u64;
        self.components += 2 * s.len() as u64;
        out
    }

    /// Clip summary.
    pub fn describe(&self) -> String {
        let r = if self.components > 0 {
            self.clipped as f64 / self.components as f64
        } else {
            0.0
        };
        format!(
            "clipped {} of {} components ({:.2e})",
            self.clipped, self.components, r
        )
    }
}

/// Where `arib tx` sends the baseband.
pub trait IqSink {
    /// Write one frame of samples (blocking as the device needs).
    fn write(&mut self, samples: &[C32], stop: &AtomicBool) -> Result<()>;
    /// Status fragment for periodic lines.
    fn status(&mut self) -> String {
        String::new()
    }
    /// End of transmission; `aborted` = stopped by signal or error (do not
    /// wait for queued samples).  Returns a summary line.
    fn finish(&mut self, aborted: bool) -> Result<String>;
    /// True if the sink consumes in real time (paces the producer itself).
    fn realtime(&self) -> bool;
}

/// IQ file / stdout sink.
pub struct FileSink {
    w: std::io::BufWriter<Box<dyn Write + Send>>,
    format: IqFormat,
    scaler: Cs8Scaler,
    samples: u64,
    path: String,
}

impl FileSink {
    /// Create `path` ("-" = stdout).
    pub fn create(path: &str, format: IqFormat, backoff_db: f32) -> Result<Self> {
        let w: Box<dyn Write + Send> = if path == "-" {
            Box::new(std::io::stdout())
        } else {
            Box::new(std::fs::File::create(path).with_context(|| format!("create {path}"))?)
        };
        Ok(Self {
            w: std::io::BufWriter::with_capacity(1 << 22, w),
            format,
            scaler: Cs8Scaler::new(backoff_db),
            samples: 0,
            path: path.into(),
        })
    }
}

impl IqSink for FileSink {
    fn write(&mut self, s: &[C32], _stop: &AtomicBool) -> Result<()> {
        match self.format {
            IqFormat::Cf32 => self.w.write_all(&isdbt_phy::iq::cf32_to_bytes(s))?,
            IqFormat::Cs8 => {
                let v = self.scaler.convert(s);
                let b: Vec<u8> = v.iter().map(|&x| x as u8).collect();
                self.w.write_all(&b)?
            }
        }
        self.samples += s.len() as u64;
        Ok(())
    }

    fn status(&mut self) -> String {
        match self.format {
            IqFormat::Cs8 => format!(" {}", self.scaler.describe()),
            IqFormat::Cf32 => String::new(),
        }
    }

    fn finish(&mut self, _aborted: bool) -> Result<String> {
        self.w.flush()?;
        let extra = if self.format == IqFormat::Cs8 {
            format!("; {}", self.scaler.describe())
        } else {
            String::new()
        };
        Ok(format!(
            "wrote {} samples ({:?}) to {}{extra}",
            self.samples, self.format, self.path
        ))
    }

    fn realtime(&self) -> bool {
        false
    }
}

/// In-memory sink (tests).
#[cfg(test)]
#[derive(Clone, Default)]
pub struct MemorySink {
    /// Everything written.
    pub samples: Arc<Mutex<Vec<C32>>>,
}

#[cfg(test)]
impl IqSink for MemorySink {
    fn write(&mut self, s: &[C32], _stop: &AtomicBool) -> Result<()> {
        self.samples.lock().unwrap().extend_from_slice(s);
        Ok(())
    }

    fn finish(&mut self, _aborted: bool) -> Result<String> {
        Ok(format!(
            "{} samples in memory",
            self.samples.lock().unwrap().len()
        ))
    }

    fn realtime(&self) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cs8_scaling_and_clip_count() {
        let mut s = Cs8Scaler::new(0.0);
        assert!((s.scale - 127.0).abs() < 1e-4);
        let v = s.convert(&[C32::new(0.5, -0.5), C32::new(1.5, -2.0)]);
        assert_eq!(v, vec![64, -64, 127, -127]);
        assert_eq!((s.clipped, s.components), (2, 4));
        let s = Cs8Scaler::new(20.0);
        assert!((s.scale - 12.7).abs() < 1e-4);
    }
}
