//! Writing frames to transport stream files.

use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::Path;

use crate::error::Result;
use crate::mux::{Multiplexer, Packet};

/// Destination of one output: a file path or `-` for stdout.
fn open(path: &str) -> Result<Box<dyn Write>> {
    Ok(if path == "-" {
        Box::new(BufWriter::new(std::io::stdout().lock()))
    } else {
        Box::new(BufWriter::new(File::create(Path::new(path))?))
    })
}

/// Writes per-layer TS files (the packets fed to each layer, nulls
/// included) and/or the multiplex-frame TS a 13-segment receiver
/// reproduces (null slots included).
pub struct TsWriter {
    layers: [Option<Box<dyn Write>>; 3],
    full: Option<Box<dyn Write>>,
    /// Packets written per output (A, B, C, full).
    pub written: [u64; 4],
}

impl TsWriter {
    /// Open the given outputs (`None` = not written).
    pub fn new(layers: [Option<&str>; 3], full: Option<&str>) -> Result<Self> {
        let mut l: [Option<Box<dyn Write>>; 3] = [None, None, None];
        for i in 0..3 {
            if let Some(p) = layers[i] {
                l[i] = Some(open(p)?);
            }
        }
        Ok(Self {
            layers: l,
            full: full.map(open).transpose()?,
            written: [0; 4],
        })
    }

    /// Write one frame.
    pub fn write_frame(&mut self, full: &[Packet], layers: &[Vec<Packet>; 3]) -> Result<()> {
        for (i, (out, pkts)) in self.layers.iter_mut().zip(layers).enumerate() {
            if let Some(w) = out.as_mut() {
                for p in pkts {
                    w.write_all(p)?;
                }
                self.written[i] += pkts.len() as u64;
            }
        }
        if let Some(w) = self.full.as_mut() {
            for p in full {
                w.write_all(p)?;
            }
            self.written[3] += full.len() as u64;
        }
        Ok(())
    }

    /// Run `mux` for `frames` frames into the outputs.
    pub fn run(&mut self, mux: &mut Multiplexer, frames: u64) -> Result<()> {
        for _ in 0..frames {
            let (full, layers) = mux.next_frame_full();
            self.write_frame(&full, &layers)?;
        }
        self.flush()
    }

    /// Flush all outputs.
    pub fn flush(&mut self) -> Result<()> {
        for w in self.layers.iter_mut().flatten() {
            w.flush()?;
        }
        if let Some(w) = self.full.as_mut() {
            w.flush()?;
        }
        Ok(())
    }
}
