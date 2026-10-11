//! IQ file sink for `isdb tx` (cf32 / cs8). The sink trait and the radio
//! sinks are in `isdb_radio`.

use anyhow::{Context, Result};
use isdb_radio::{Cs8, IqSink, Quantizer};
use isdbt_ofdm::C32;
use std::io::Write;
use std::sync::atomic::AtomicBool;

/// IQ sample file format.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum IqFormat {
    /// Interleaved little-endian f32 I/Q (GNU Radio complex).
    Cf32,
    /// Interleaved signed 8-bit I/Q (tx writes it with --backoff-db scaling).
    Cs8,
}

/// IQ file / stdout sink.
pub struct FileSink {
    w: std::io::BufWriter<Box<dyn Write + Send>>,
    format: IqFormat,
    quantizer: Quantizer<Cs8>,
    /// cs8 components of the current write.
    buf: Vec<i8>,
    /// Bytes of the current write.
    bytes: Vec<u8>,
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
            quantizer: Quantizer::new(backoff_db),
            buf: Vec::new(),
            bytes: Vec::new(),
            samples: 0,
            path: path.into(),
        })
    }
}

impl IqSink for FileSink {
    fn write(&mut self, s: &[C32], _stop: &AtomicBool) -> Result<()> {
        // Both buffers are reused across writes.
        self.bytes.clear();
        match self.format {
            IqFormat::Cf32 => {
                for c in s {
                    self.bytes.extend_from_slice(&c.re.to_le_bytes());
                    self.bytes.extend_from_slice(&c.im.to_le_bytes());
                }
            }
            IqFormat::Cs8 => {
                self.quantizer.convert_into(s, &mut self.buf);
                self.bytes.extend(self.buf.iter().map(|&x| x as u8));
            }
        }
        self.w.write_all(&self.bytes)?;
        self.samples += s.len() as u64;
        Ok(())
    }

    fn status(&mut self) -> String {
        match self.format {
            IqFormat::Cs8 => format!(" {}", self.quantizer.describe()),
            IqFormat::Cf32 => String::new(),
        }
    }

    fn finish(&mut self, _aborted: bool) -> Result<String> {
        self.w.flush()?;
        let extra = if self.format == IqFormat::Cs8 {
            format!("; {}", self.quantizer.describe())
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cs8_file_bytes() {
        // The cs8 file holds the Quantizer<Cs8> output byte for byte: the
        // same scaling the radio backends with 8-bit samples apply.
        let dir = std::env::temp_dir().join(format!("isdb-sink-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("x.cs8");
        let x = [
            C32::new(0.5, -0.5),
            C32::new(1.5, -2.0),
            C32::new(0.01, 0.0),
        ];
        let mut s = FileSink::create(p.to_str().unwrap(), IqFormat::Cs8, 0.0).unwrap();
        s.write(&x, &AtomicBool::new(false)).unwrap();
        let fin = s.finish(false).unwrap();
        assert!(fin.contains("wrote 3 samples (Cs8)"), "{fin}");
        assert!(fin.contains("clipped 2 of 6 components"), "{fin}");
        drop(s);
        let b = std::fs::read(&p).unwrap();
        assert_eq!(b, [64, 192, 127, 129, 1, 0]);
        // cf32 holds the isdbt-phy file encoding.
        let p = dir.join("x.cf32");
        let mut s = FileSink::create(p.to_str().unwrap(), IqFormat::Cf32, 0.0).unwrap();
        s.write(&x, &AtomicBool::new(false)).unwrap();
        s.write(&x[..1], &AtomicBool::new(false)).unwrap();
        s.finish(false).unwrap();
        drop(s);
        let mut want = isdbt_phy::iq::cf32_to_bytes(&x);
        want.extend(isdbt_phy::iq::cf32_to_bytes(&x[..1]));
        assert_eq!(std::fs::read(&p).unwrap(), want);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
