//! `isdb tx --iq-in FILE`: replay a baseband file (cf32 or cs8 at
//! 512/63 MHz, as `isdb tx --iq` or `hackrf_transfer -r` write it) to a
//! radio or another file, with playback keys.
//!
//! At start the receiver ([`isdbt_phy::Receiver`]) runs over the beginning
//! of the file to find its transmission parameters (TMCC) and where its
//! OFDM frames start; the file is then sent frame by frame from there,
//! scaled to unit RMS (the level the radio sinks expect). Pause sends null
//! frames modulated with the same parameters, so receivers stay locked;
//! seek moves by whole frames. Either breaks the time interleaving at the
//! splice, so a receiver loses a few frames' packets there. The play rate
//! cannot change (the sample rate is fixed).

use crate::keys::Command;
use crate::producer::{EndReason, FrameKind, ProducedFrame};
use crate::source::Packet;
use anyhow::{Context, Result, bail};
use isdbt_core::Params;
use isdbt_ofdm::C32;
use isdbt_phy::Transmitter;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, sync_channel};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::sink::IqFormat;

/// Signal time the receiver may take to find the parameters (s).
const DETECT_SECONDS: f64 = 4.0;

/// A baseband file and what the receiver found in it.
#[derive(Debug, Clone)]
pub struct IqFile {
    path: PathBuf,
    format: IqFormat,
    /// Transmission parameters (TMCC).
    pub params: Params,
    /// Sample index where the first whole OFDM frame starts.
    pub first_sample: u64,
    /// Whole frames from there to the end of the file.
    pub frames: u64,
    /// Factor to unit RMS.
    pub scale: f32,
}

fn bytes_per_sample(f: IqFormat) -> usize {
    match f {
        IqFormat::Cf32 => 8,
        IqFormat::Cs8 => 2,
    }
}

fn decode(f: IqFormat, b: &[u8]) -> Vec<C32> {
    match f {
        IqFormat::Cf32 => isdbt_phy::iq::cf32_from_bytes(b),
        IqFormat::Cs8 => {
            let v: Vec<i8> = b.iter().map(|&x| x as i8).collect();
            isdbt_phy::iq::samples_from_cs8(&v)
        }
    }
}

impl IqFile {
    /// Run the receiver over the start of `path` until it decodes a TMCC.
    pub fn detect(path: &Path, format: IqFormat) -> Result<Self> {
        let mut f = File::open(path).with_context(|| format!("--iq-in {}", path.display()))?;
        let len = f.metadata()?.len();
        let bps = bytes_per_sample(format);
        let rate = isdbt_core::sample_rate_hz();
        let mut rx = isdbt_phy::Receiver::new(isdbt_ofdm::DemodConfig::default());
        // f64 -> usize saturates: 0.1 s of the 512/63 MHz clock
        #[allow(clippy::cast_possible_truncation)]
        let chunk = (rate / 10.0) as usize;
        let mut buf = vec![0u8; chunk * bps];
        let (mut pushed, mut power, mut nz) = (0u64, 0f64, 0u64);
        let found = loop {
            if pushed as f64 > DETECT_SECONDS * rate {
                break None;
            }
            let n = read_full(&mut f, &mut buf)?;
            if n < bps {
                break None;
            }
            let s = decode(format, &buf[..n - n % bps]);
            for c in &s {
                let p = f64::from(c.norm_sqr());
                if p > 0.0 {
                    power += p;
                    nz += 1;
                }
            }
            let out = rx.push(&s).context("receiver")?;
            pushed += s.len() as u64;
            if let Some(fr) = out.first() {
                break Some((fr.params.clone(), fr.start_sample));
            }
        };
        let Some((params, start)) = found else {
            bail!(
                "--iq-in {}: no ISDB-T signal found in its first {DETECT_SECONDS} s (a cf32 or cs8 file at 512/63 MHz centred on the signal; --iq-in-format names the format)",
                path.display()
            );
        };
        let spf = params.samples_per_frame() as u64;
        let first_sample = start % spf;
        let total = len / bps as u64;
        let frames = total.saturating_sub(first_sample) / spf;
        if frames == 0 {
            bail!("--iq-in {}: shorter than one frame", path.display());
        }
        let rms = (power / nz.max(1) as f64).sqrt();
        Ok(Self {
            path: path.to_path_buf(),
            format,
            params,
            first_sample,
            frames,
            // f64 -> f32 rounds: a gain near 1/RMS of the samples
            #[allow(clippy::cast_possible_truncation)]
            scale: if rms > 0.0 { (1.0 / rms) as f32 } else { 1.0 },
        })
    }

    /// Length (s).
    pub fn seconds(&self) -> f64 {
        self.frames as f64 * self.params.frame_seconds()
    }
}

/// `read` until `buf` is full or the file ends; the bytes read.
fn read_full(f: &mut File, buf: &mut [u8]) -> Result<usize> {
    let mut n = 0;
    while n < buf.len() {
        match f.read(&mut buf[n..]) {
            Ok(0) => break,
            Ok(k) => n += k,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e.into()),
        }
    }
    Ok(n)
}

/// Playback settings.
#[derive(Debug, Clone)]
pub struct PlayConfig {
    /// Bound on the frames sent (None = until the end / Ctrl-C).
    pub max_frames: Option<u64>,
    /// Start again at the end of the file.
    pub looped: bool,
    /// Pace to real time (file sinks).
    pub pace: bool,
    /// Frames queued ahead of the sink.
    pub depth: usize,
}

fn mmss(s: f64) -> String {
    let s = s.max(0.0);
    format!("{:02.0}:{:04.1}", (s / 60.0).floor(), s % 60.0)
}

/// Start the playback thread (the same frame stream as
/// [`crate::producer::spawn`]).
pub fn spawn(
    file: IqFile,
    cfg: PlayConfig,
    stop: Arc<AtomicBool>,
    controls: Option<Receiver<Command>>,
) -> (Receiver<ProducedFrame>, JoinHandle<Result<EndReason>>) {
    let (out, rx) = sync_channel::<ProducedFrame>(cfg.depth.max(1));
    let h = std::thread::Builder::new()
        .name("iq-playback".into())
        .spawn(move || -> Result<EndReason> {
            let mut f = File::open(&file.path)?;
            let spf = file.params.samples_per_frame();
            let bps = bytes_per_sample(file.format);
            let frame_s = file.params.frame_seconds();
            let mut buf = vec![0u8; spf * bps];
            let mut nulls: Option<Transmitter> = None;
            let (mut frame, mut paused, mut index) = (0u64, false, 0u64);
            let t0 = Instant::now();
            let state = |frame: u64, paused: bool| {
                format!(
                    "{} {} / {}",
                    if paused { "paused" } else { "playing" },
                    mmss(frame as f64 * frame_s),
                    mmss(file.seconds())
                )
            };
            loop {
                if stop.load(Ordering::Relaxed) {
                    return Ok(EndReason::Stopped);
                }
                if cfg.max_frames.is_some_and(|m| index >= m) {
                    return Ok(EndReason::Limit);
                }
                for cmd in controls.iter().flat_map(Receiver::try_iter) {
                    let note = match cmd {
                        Command::TogglePause => {
                            paused = !paused;
                            String::new()
                        }
                        Command::Seek(d) => {
                            // f64 -> i64 saturates: the seek in frames, clamped next
                            #[allow(clippy::cast_possible_truncation)]
                            let k = (d / frame_s).round() as i64;
                            frame = (frame as i64 + k).clamp(0, file.frames as i64 - 1) as u64;
                            format!(" (seek {d:+} s)")
                        }
                        Command::Faster | Command::Slower => {
                            " (the play rate of an IQ file is fixed)".into()
                        }
                    };
                    eprintln!("[tx] {}{note}", state(frame, paused));
                }
                if cfg.pace {
                    let due = t0 + Duration::from_secs_f64(index as f64 * frame_s);
                    if let Some(w) = due.checked_duration_since(Instant::now()) {
                        std::thread::sleep(w);
                    }
                }
                let ti = Instant::now();
                let samples = if paused {
                    let tx = match nulls.as_mut() {
                        Some(t) => t,
                        None => {
                            let mut t = Transmitter::new(file.params.clone());
                            t.preroll()?;
                            nulls.insert(t)
                        }
                    };
                    tx.transmit_null_frame()?
                } else {
                    if frame >= file.frames {
                        if !cfg.looped {
                            return Ok(EndReason::InputEnded);
                        }
                        frame = 0;
                    }
                    f.seek(SeekFrom::Start(
                        (file.first_sample + frame * spf as u64) * bps as u64,
                    ))?;
                    let n = read_full(&mut f, &mut buf)?;
                    if n < buf.len() {
                        return Ok(EndReason::InputEnded);
                    }
                    frame += 1;
                    let mut s = decode(file.format, &buf);
                    for c in &mut s {
                        *c *= file.scale;
                    }
                    s
                };
                let it = ti.elapsed();
                let pf = ProducedFrame {
                    index,
                    kind: FrameKind::Data,
                    samples,
                    real: [0; 3],
                    nulls: [0; 3],
                    packets: None::<Arc<[Vec<Packet>; 3]>>,
                    input_status: format!(" | {}", state(frame, paused)),
                    input_time: it,
                    modulate_time: Duration::ZERO,
                };
                if out.send(pf).is_err() {
                    return Ok(if stop.load(Ordering::Relaxed) {
                        EndReason::Stopped
                    } else {
                        EndReason::SinkClosed
                    });
                }
                index += 1;
            }
        })
        .expect("spawn iq playback");
    (rx, h)
}
