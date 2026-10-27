//! Continuous reception from a stream of `Symbols` words: a radio's FPGA
//! front end (`isdb rx --system isdbs3|isdbs --radio … --offload auto`) or
//! a words file (`isdb rx --symbols FILE`).
//!
//! The words are parsed by `isdb_frontend::format::Reader` (CRC-checked
//! blocks with sequence numbers), their symbols appended to a buffer, and
//! the buffer handed to the system's receiver whenever it holds
//! [`CHUNK_FRAMES`] frames. The receiver decodes the whole buffer, the
//! caller keeps what starts at the last frame (ISDB-S3) or superframe
//! (ISDB-S) it found, and the next call begins there: the frames are cut on
//! their own boundaries, so a continuous stream decodes as one, and the
//! per-stream TLV assemblers carry partial packets across calls. A loss
//! (blocks missing or words skipped) ends the contiguous stretch: the buffer
//! is decoded to its end, the assemblers are closed, and the stream starts
//! afresh after it — a discontinuity in the output, reported.

use anyhow::{Context, Result};
use isdb_frontend::format::{Event, Reader};
use isdbs3_phy::Cplx;
use std::io::Read;
use std::sync::mpsc::{Receiver, SyncSender, sync_channel};
use tracing::{info, warn};

/// ISDB-S3 frames buffered before the receiver runs (24 × 33.05 ms ≈
/// 0.8 s of latency). Each call synchronises, trains the equaliser and
/// decodes its last whole frame once more in the next call, so larger
/// chunks cost less per frame: on a 10 s FPGA stream of BS7, 8 / 16 / 24
/// frames decoded at 0.81× / 1.07× / 1.14× real time on 32 cores, with
/// the same result (301 frames, TMCC on all, 0 LDPC failures).
pub const CHUNK_FRAMES: usize = 24;
/// ISDB-S superframes buffered before the receiver runs (each 11.07 ms).
pub const CHUNK_SUPERFRAMES: usize = 24;

/// Chunks the buffer may hold before it is decoded to its end and the
/// stream restarted (each call keeps what starts at its last whole frame,
/// normally under one chunk).
pub const MAX_KEPT_CHUNKS: usize = 3;

/// Chunks of parsed symbols that may wait for the decoder (≈ 3.2 s of
/// ISDB-S3, ≈ 1.7 GB as complex values). Beyond, a [`Source::Live`]
/// parser drops whole blocks and reports them as a loss: the capture is
/// never held up (which would overflow the radio's buffers mid-stream),
/// and memory stays bounded when the decoder falls behind for longer than
/// that. A [`Source::File`] parser waits instead.
pub const MAX_QUEUED_CHUNKS: usize = 4;

/// Where the words come from, which decides what happens when the decoder
/// falls [`MAX_QUEUED_CHUNKS`] behind the parser.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// A radio: the parser drops blocks rather than hold the capture up.
    Live,
    /// A file: the parser waits for the decoder, so a replay is complete
    /// and the same on every run whatever the decoder's speed.
    File,
}

/// Symbols parsed and not yet taken by the decoder, shared by the parser
/// thread (adds) and the decoder (subtracts, and wakes a waiting parser).
struct Queue {
    symbols: std::sync::Mutex<u64>,
    drained: std::sync::Condvar,
    /// Set by the decoder when it stops taking items, so a parser waiting
    /// on `drained` returns instead of waiting for a drain that never
    /// comes.
    closed: std::sync::atomic::AtomicBool,
}

impl Queue {
    fn new() -> Self {
        Self {
            symbols: std::sync::Mutex::new(0),
            drained: std::sync::Condvar::new(),
            closed: std::sync::atomic::AtomicBool::new(false),
        }
    }
    /// Reserve `n` symbols for a block. `false` means the block is to be
    /// dropped (`Live`, queue full) or the decoder has gone.
    fn reserve(&self, n: u64, max: u64, source: Source) -> bool {
        let mut q = self.symbols.lock().unwrap_or_else(|e| e.into_inner());
        while *q >= max {
            if self.closed.load(std::sync::atomic::Ordering::Relaxed) {
                return false;
            }
            match source {
                Source::Live => return false,
                Source::File => q = self.drained.wait(q).unwrap_or_else(|e| e.into_inner()),
            }
        }
        *q += n;
        true
    }
    fn release(&self, n: u64) -> u64 {
        let mut q = self.symbols.lock().unwrap_or_else(|e| e.into_inner());
        *q -= n;
        let left = *q;
        drop(q);
        self.drained.notify_one();
        left
    }
    fn len(&self) -> u64 {
        *self.symbols.lock().unwrap_or_else(|e| e.into_inner())
    }
    fn close(&self) {
        self.closed
            .store(true, std::sync::atomic::Ordering::Relaxed);
        self.drained.notify_all();
    }
}

/// Where a contiguous stretch of symbols ended and why.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cut {
    /// The stream continues: decode what is complete, keep the rest.
    More,
    /// The stream broke (a loss) or ended: decode everything.
    End,
}

/// Counters of the word stream.
#[derive(Debug, Clone, Copy, Default)]
pub struct WordStats {
    /// Whole blocks delivered.
    pub blocks: u64,
    /// Symbols delivered.
    pub symbols: u64,
    /// Loss events (each a discontinuity in the output).
    pub losses: u64,
    /// Blocks missing (from the sequence numbers).
    pub blocks_lost: u64,
    /// Words skipped while resynchronising (the first loss event's are
    /// usually the stream joined mid-block).
    pub words_skipped: u64,
    /// Symbols parsed but not yet decoded, after the last decode call (how
    /// far behind the decoder is).
    pub queued_symbols: u64,
    /// The most [`WordStats::queued_symbols`] seen.
    pub max_queued_symbols: u64,
    /// Times the buffer grew to [`MAX_KEPT_CHUNKS`] chunks without a cut
    /// and was decoded to its end (lost lock; a discontinuity, like a loss).
    pub resyncs: u64,
    /// Blocks dropped on the host because the decoder was more than
    /// [`MAX_QUEUED_CHUNKS`] chunks behind (counted in `blocks_lost` too;
    /// each run of them is one loss).
    pub blocks_dropped: u64,
    /// The run was stopped (Ctrl-C) with symbols still queued.
    pub stopped: bool,
}

/// Bytes of a word stream, as they arrive (from a radio thread or a file).
pub type Bytes = Receiver<Vec<u8>>;

/// A writer that sends what it is given over a channel, for
/// `Backend::capture_symbols` running on its own thread; a closed channel
/// (the receiver stopped) ends the capture with an error.
pub struct ChannelWriter(pub SyncSender<Vec<u8>>);

impl std::io::Write for ChannelWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0
            .send(buf.to_vec())
            .map_err(|_| std::io::Error::new(std::io::ErrorKind::BrokenPipe, "receiver stopped"))?;
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// The bytes of a words file, in chunks, from a thread.
pub fn file_bytes(path: &std::path::Path) -> Result<Bytes> {
    let mut f = std::fs::File::open(path).with_context(|| format!("open {}", path.display()))?;
    let (tx, rx) = sync_channel::<Vec<u8>>(8);
    std::thread::spawn(move || {
        let mut buf = vec![0u8; 1 << 20];
        loop {
            match f.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if tx.send(buf[..n].to_vec()).is_err() {
                        break;
                    }
                }
            }
        }
    });
    Ok(rx)
}

/// What the parser thread hands the decoder.
enum Item {
    /// A whole block's symbols.
    Symbols(Vec<Cplx>),
    /// A loss (`blocks` missing, `words` skipped) before the next block;
    /// `dropped` of the blocks were dropped on the host (decoder behind).
    Loss {
        blocks: u64,
        words: u64,
        dropped: u64,
    },
}

/// Run the stream: `bytes` → blocks → `decode(buffer, cut)`, which returns
/// how many symbols of the buffer it consumed (the rest is kept for the
/// next call; with [`Cut::End`] everything is consumed), and `ended(stats)`
/// after every contiguous stretch (a loss or the end). `status` runs after
/// each decode call.
///
/// The words are parsed on their own thread, which keeps draining `bytes`
/// whatever the decoder is doing: a radio's capture never waits for the
/// receiver (its buffers would overflow on the device), and blocks queue up
/// in memory while a decode call runs. [`WordStats::queued_symbols`] says
/// how far behind the decoder is. `stop` (Ctrl-C) ends the run at once,
/// leaving the queued symbols undecoded.
pub fn run(
    bytes: Bytes,
    source: Source,
    chunk_symbols: usize,
    stop: &std::sync::atomic::AtomicBool,
    mut decode: impl FnMut(&[Cplx], Cut) -> Result<usize>,
    mut ended: impl FnMut(&WordStats) -> Result<()>,
    mut status: impl FnMut(&WordStats),
) -> Result<WordStats> {
    let (itx, irx) = std::sync::mpsc::channel::<Item>();
    let queued = std::sync::Arc::new(Queue::new());
    let q = std::sync::Arc::clone(&queued);
    let max_queued = (MAX_QUEUED_CHUNKS * chunk_symbols) as u64;
    let parser = std::thread::spawn(move || {
        let mut reader = Reader::new();
        let mut events = Vec::new();
        // blocks dropped since the last block sent, and a loss the reader
        // reported meanwhile (merged into one loss item)
        let dropped = std::cell::Cell::new(0u64);
        let pending = std::cell::Cell::new(None::<(u64, u64)>);
        // the loss accumulated since the last block sent, as one item
        let flush = || -> Option<Item> {
            let d = dropped.replace(0);
            let p = pending.take();
            (d > 0 || p.is_some()).then(|| {
                let (b, w) = p.unwrap_or((0, 0));
                Item::Loss {
                    blocks: b + d,
                    words: w,
                    dropped: d,
                }
            })
        };
        let send = |e: Event| -> bool {
            let item = match e {
                Event::Block { symbols, .. } => {
                    if !q.reserve(symbols.len() as u64, max_queued, source) {
                        if q.closed.load(std::sync::atomic::Ordering::Relaxed) {
                            return false;
                        }
                        dropped.set(dropped.get() + 1);
                        return true;
                    }
                    if let Some(loss) = flush()
                        && itx.send(loss).is_err()
                    {
                        return false;
                    }
                    Item::Symbols(
                        symbols
                            .iter()
                            .map(|s| Cplx::new(f64::from(s[0]), f64::from(s[1])))
                            .collect(),
                    )
                }
                Event::Loss { blocks, words } => {
                    let (b, w) = pending.get().unwrap_or((0, 0));
                    pending.set(Some((b + blocks, w + words)));
                    return true;
                }
                Event::Truncated { .. } => return true,
            };
            itx.send(item).is_ok()
        };
        for chunk in bytes {
            reader.feed_bytes(&chunk, &mut events);
            for e in events.drain(..) {
                if !send(e) {
                    return;
                }
            }
        }
        reader.finish(&mut events);
        for e in events.drain(..) {
            if !send(e) {
                return;
            }
        }
        // blocks dropped at the very end still count
        if let Some(loss) = flush() {
            let _ = itx.send(loss);
        }
    });
    let mut buf: Vec<Cplx> = Vec::new();
    let mut st = WordStats::default();
    let mut decode_buffer = |buf: &mut Vec<Cplx>, cut: Cut, st: &mut WordStats| -> Result<()> {
        let used = decode(buf, cut)?.min(buf.len());
        buf.drain(..used);
        if cut == Cut::End {
            buf.clear();
        }
        st.queued_symbols = queued.len();
        st.max_queued_symbols = st.max_queued_symbols.max(st.queued_symbols);
        status(st);
        Ok(())
    };
    let mut started = false;
    // decode when the buffer reaches `next_at`: one chunk after the part a
    // call kept, so a call that keeps much (frames found late, or none)
    // waits for a whole chunk of new symbols before the next attempt
    let mut next_at = chunk_symbols;
    let r = (|| -> Result<()> {
        for item in irx {
            // Ctrl-C: stop at once, without decoding the queued backlog
            if stop.load(std::sync::atomic::Ordering::Relaxed) {
                st.stopped = true;
                break;
            }
            match item {
                Item::Symbols(sy) => {
                    queued.release(sy.len() as u64);
                    st.blocks += 1;
                    st.symbols += sy.len() as u64;
                    buf.extend_from_slice(&sy);
                    started = true;
                    if buf.len() >= next_at {
                        if buf.len() >= MAX_KEPT_CHUNKS * chunk_symbols {
                            // nothing found to cut on for several chunks
                            // (lost lock): decode it all and start afresh
                            st.resyncs += 1;
                            warn!(
                                target: "isdb::rx",
                                buffered_symbols = buf.len(),
                                symbol = st.symbols,
                                resyncs = st.resyncs,
                                "no frame to cut on in {} symbols (lost lock) at symbol {}: decoding them and restarting the stream",
                                buf.len(),
                                st.symbols
                            );
                            decode_buffer(&mut buf, Cut::End, &mut st)?;
                            ended(&st)?;
                        } else {
                            decode_buffer(&mut buf, Cut::More, &mut st)?;
                        }
                        next_at = buf.len() + chunk_symbols;
                    }
                }
                Item::Loss {
                    blocks,
                    words,
                    dropped,
                } => {
                    st.words_skipped += words;
                    st.blocks_lost += blocks;
                    st.blocks_dropped += dropped;
                    // the first event before any block is the stream joined
                    // mid-block: nothing was lost from the output yet
                    if started {
                        st.losses += 1;
                        warn!(
                            target: "isdb::rx",
                            symbol = st.symbols,
                            blocks_lost = blocks,
                            blocks_dropped = dropped,
                            words_skipped = words,
                            losses = st.losses,
                            "loss at symbol {}: {blocks} block(s) missing{}, {words} words skipped",
                            st.symbols,
                            if dropped > 0 {
                                format!(" ({dropped} dropped here: the receiver fell behind)")
                            } else {
                                String::new()
                            }
                        );
                        decode_buffer(&mut buf, Cut::End, &mut st)?;
                        ended(&st)?;
                        next_at = chunk_symbols;
                    }
                }
            }
        }
        decode_buffer(&mut buf, Cut::End, &mut st)?;
        ended(&st)
    })();
    // on an error or a stop the parser stops when its channel closes, or,
    // if it is waiting for the decoder to drain, when the queue closes
    queued.close();
    let _ = parser.join();
    r?;
    Ok(st)
}

/// Probes of the acquisition before giving up.
pub const ACQUIRE_ATTEMPTS: usize = 3;

/// The radio side of a streamed reception.
pub struct RadioStream {
    /// The words as they arrive.
    pub bytes: Bytes,
    /// The capture thread (its result: the capture's summary).
    pub thread: std::thread::JoinHandle<Result<isdb_radio::SymbolsCapture>>,
    /// Symbols per second the stream carries.
    pub symbol_rate_hz: f64,
    /// The front end's parameters (for the summary).
    pub params: isdb_frontend::Params,
}

/// Whether to use `backend`'s symbol front end with `cfg`: `Some(taps)`
/// when it offers a compatible one and `offload` allows it; an error for
/// `fpga` without one.
pub fn fpga_taps(
    backend: &dyn isdb_radio::Backend,
    cfg: &isdb_radio::RadioConfig,
    offload: crate::symbols::OffloadArg,
) -> Result<Option<usize>> {
    use crate::symbols::OffloadArg;
    if offload == OffloadArg::Cpu {
        return Ok(None);
    }
    let found = backend.offloads(cfg)?.into_iter().find_map(|o| match o {
        isdb_radio::Offload::Symbols { version, taps }
            if (isdb_frontend::params::MIN_ARITH_VERSION
                ..=isdb_frontend::params::FORMAT_VERSION)
                .contains(&version) =>
        {
            Some(taps)
        }
        _ => None,
    });
    if found.is_none() && offload == OffloadArg::Fpga {
        anyhow::bail!("--offload fpga: the radio has no compatible symbol front end");
    }
    Ok(found)
}

/// Acquire on a short IQ capture from `backend` (`acquire(x, fs)` gives the
/// front end's parameters and a one-line description of the acquisition,
/// or `None` when the probe gave no usable lock: up to [`ACQUIRE_ATTEMPTS`]
/// probes are taken), then start the FPGA front end streaming its words,
/// until `stop`.
pub fn start_radio(
    backend: &'static dyn isdb_radio::Backend,
    cfg: &isdb_radio::RadioConfig,
    symbol_rate_hz: f64,
    stop: &std::sync::Arc<std::sync::atomic::AtomicBool>,
    acquire: impl FnMut(&[isdb_iq::C32], f64) -> Result<Option<(isdb_frontend::Params, String)>>,
) -> Result<RadioStream> {
    let (limit, _) = backend
        .capture_limit(cfg)?
        .context("this radio cannot capture IQ, which the acquisition needs")?;
    // f64 -> u64 saturates: 0.2 s of samples, bounded by the limit
    #[allow(clippy::cast_possible_truncation)]
    let n = limit.min((0.2 * cfg.sample_rate_hz()) as u64);
    // `acquire` returns None when it found no usable lock on this probe
    // (e.g. the first capture after the radio idled, before its AGC
    // settled): take another probe, a few times
    let mut acquire = acquire;
    let mut got = None;
    for attempt in 1..=ACQUIRE_ATTEMPTS {
        let mut probe = Vec::new();
        let cap = backend.capture(cfg, n, &mut probe, stop)?;
        let x = crate::symbols::decode(cap.format, &probe);
        let secs = x.len() as f64 / cap.sample_rate_hz;
        match acquire(&x, cap.sample_rate_hz)? {
            Some((params, what)) => {
                info!(
                    target: "isdb::rx",
                    seconds = secs,
                    attempt,
                    "acquisition on {secs:.3} s of IQ: {what}"
                );
                got = Some(params);
                break;
            }
            None => warn!(
                target: "isdb::rx",
                seconds = secs,
                attempt,
                attempts = ACQUIRE_ATTEMPTS,
                "acquisition attempt {attempt} of {ACQUIRE_ATTEMPTS} on {secs:.3} s of IQ found no lock; probing again"
            ),
        }
    }
    let params =
        got.context("no lock on the signal after several probes (wrong channel, or no signal)")?;
    let (tx, rx) = sync_channel::<Vec<u8>>(64);
    let cfg = cfg.clone();
    let p = params.clone();
    let stop = std::sync::Arc::clone(stop);
    let thread = std::thread::spawn(move || {
        let mut w = ChannelWriter(tx);
        backend.capture_symbols(&cfg, &p, u64::MAX, &mut w, &stop)
    });
    Ok(RadioStream {
        bytes: rx,
        thread,
        symbol_rate_hz,
        params,
    })
}

/// Run a started radio stream to its end: `decode(bytes)` receives the
/// words until `seconds` of signal (0: until `stop`, Ctrl-C), then the
/// capture is stopped and its summary reported. Returns `decode`'s result.
pub fn finish_radio(
    stream: RadioStream,
    seconds: f64,
    stop: &std::sync::Arc<std::sync::atomic::AtomicBool>,
    cap_stop: &std::sync::Arc<std::sync::atomic::AtomicBool>,
    decode: impl FnOnce(Bytes) -> Result<f64>,
) -> Result<f64> {
    use std::sync::atomic::Ordering;
    let limit = (seconds > 0.0).then_some(seconds * stream.symbol_rate_hz);
    let (user, cs) = (std::sync::Arc::clone(stop), std::sync::Arc::clone(cap_stop));
    let (tx, rx) = sync_channel::<Vec<u8>>(64);
    let wps = stream.params.words_per_symbol();
    let bytes = stream.bytes;
    // forwards the words and stops the capture at the end of `seconds`, or
    // on Ctrl-C
    let feeder = std::thread::spawn(move || {
        let mut words = 0u64;
        for b in bytes {
            words += (b.len() / 2) as u64;
            if tx.send(b).is_err() {
                break;
            }
            if user.load(Ordering::Relaxed) || limit.is_some_and(|l| words as f64 / wps >= l) {
                cs.store(true, Ordering::SeqCst);
            }
        }
    });
    let secs = decode(rx);
    cap_stop.store(true, Ordering::SeqCst);
    let _ = feeder.join();
    match stream.thread.join().expect("capture thread") {
        Ok(c) => {
            if c.overflows > 0 {
                warn!(
                    target: "isdb::rx",
                    overflows = c.overflows,
                    "the radio reported {} DMA overflow(s); the words' sequence numbers count what was lost",
                    c.overflows
                );
            }
            info!(target: "isdb::rx", "{}; {}", c.method, c.gain);
        }
        Err(e) => warn!(target: "isdb::rx", "the capture ended: {e:#}"),
    }
    secs
}

#[cfg(test)]
mod tests {
    use super::*;
    use isdb_frontend::format::Packer;
    use isdb_frontend::params::{Design, Params};

    /// `n_blocks` blocks of `Symbols` words (block_log2 12: 4 096 symbols).
    fn words(n_blocks: usize) -> Vec<u8> {
        let mut d = Design::new(40e6, 33.7561e6, 0.03);
        d.block_log2 = 12;
        let mut pk = Packer::new(&Params::design(&d).unwrap());
        let mut w = Vec::new();
        // a (0..7, 0..5) symbol pattern, cycling
        let (mut a, mut b) = (0i8, 0i8);
        for _ in 0..n_blocks * 4096 {
            let sym = [a - 3, b - 2];
            (a, b) = ((a + 1) % 7, (b + 1) % 5);
            pk.push(sym, 1024, 1 << 24, &mut w);
        }
        w.iter().flat_map(|x| x.to_le_bytes()).collect()
    }

    fn bytes_of(b: Vec<u8>) -> Bytes {
        let (tx, rx) = sync_channel(4);
        std::thread::spawn(move || {
            for c in b.chunks(65_536) {
                if tx.send(c.to_vec()).is_err() {
                    break;
                }
            }
        });
        rx
    }

    /// A decoder that never finds a frame to cut on (keeps everything)
    /// must not be called again for every new block: calls are paced on a
    /// chunk of new symbols, and the buffer is decoded to its end once it
    /// holds MAX_KEPT_CHUNKS chunks. (A live run that lost frames called
    /// the receiver on every 4 096-symbol block over a growing buffer and
    /// fell minutes behind, plan §0.16.)
    #[test]
    fn a_decoder_that_finds_nothing_is_paced_and_bounded() {
        let chunk = 40_000;
        let blocks = 200; // 819 200 symbols ≈ 20 chunks
        let stop = std::sync::atomic::AtomicBool::new(false);
        let (mut calls, mut max_len) = (0usize, 0usize);
        let st = run(
            bytes_of(words(blocks)),
            Source::File,
            chunk,
            &stop,
            |z, cut| {
                calls += 1;
                max_len = max_len.max(z.len());
                Ok(if cut == Cut::End { z.len() } else { 0 })
            },
            |_| Ok(()),
            |_| {},
        )
        .unwrap();
        assert_eq!(st.symbols, (blocks * 4096) as u64);
        assert_eq!(st.losses, 0);
        // ≈ one call per chunk of new symbols, not one per block
        assert!(calls <= blocks * 4096 / chunk + 2, "{calls} calls");
        assert!(calls < blocks / 4, "{calls} calls for {blocks} blocks");
        assert!(
            max_len < MAX_KEPT_CHUNKS * chunk + 4096,
            "buffer grew to {max_len}"
        );
        assert!(st.resyncs > 0);
    }

    /// A decoder that cuts every call keeps the stream contiguous: every
    /// symbol is passed to it exactly once (consumed or kept), in order.
    #[test]
    fn every_symbol_is_decoded_once_in_order() {
        let stop = std::sync::atomic::AtomicBool::new(false);
        let mut seen: Vec<Cplx> = Vec::new();
        let st = run(
            bytes_of(words(30)),
            Source::File,
            10_000,
            &stop,
            |z, cut| {
                // consume all but the last 1 000 symbols (as a frame cut would)
                let used = if cut == Cut::End {
                    z.len()
                } else {
                    z.len().saturating_sub(1_000)
                };
                seen.extend_from_slice(&z[..used]);
                Ok(used)
            },
            |_| Ok(()),
            |_| {},
        )
        .unwrap();
        assert_eq!(seen.len() as u64, st.symbols);
        let want: Vec<Cplx> = (0..30 * 4096)
            .map(|i| Cplx::new(f64::from((i % 7) as i8 - 3), f64::from((i % 5) as i8 - 2)))
            .collect();
        assert!(seen == want, "symbols out of order or altered");
    }

    /// A file source is replayed completely however slow the decoder is:
    /// the parser waits at the queue bound instead of dropping, so every
    /// block arrives and the counters are the same on every run.
    #[test]
    fn a_file_source_waits_for_a_slow_decoder_and_loses_nothing() {
        let chunk = 8_192; // bound: 4 chunks = 8 blocks of 4 096
        let blocks = 60;
        let stop = std::sync::atomic::AtomicBool::new(false);
        let mut first = true;
        let st = run(
            bytes_of(words(blocks)),
            Source::File,
            chunk,
            &stop,
            |z, _| {
                if first {
                    // the parser fills the queue and then waits
                    first = false;
                    std::thread::sleep(std::time::Duration::from_millis(300));
                }
                Ok(z.len())
            },
            |_| Ok(()),
            |_| {},
        )
        .unwrap();
        assert_eq!(st.blocks, blocks as u64);
        assert_eq!(st.symbols, (blocks * 4096) as u64);
        assert_eq!((st.blocks_dropped, st.blocks_lost, st.losses), (0, 0, 0));
        assert!(
            st.max_queued_symbols <= (MAX_QUEUED_CHUNKS * chunk + 4096) as u64,
            "queue grew to {}",
            st.max_queued_symbols
        );
    }

    /// A stop while the file parser waits at the queue bound ends the run
    /// (the queue is closed and the parser wakes) instead of hanging.
    #[test]
    fn a_stop_wakes_a_waiting_file_parser() {
        let chunk = 8_192;
        let stop = std::sync::atomic::AtomicBool::new(false);
        let st = run(
            bytes_of(words(60)),
            Source::File,
            chunk,
            &stop,
            |z, _| {
                std::thread::sleep(std::time::Duration::from_millis(200));
                stop.store(true, std::sync::atomic::Ordering::Relaxed);
                Ok(z.len())
            },
            |_| Ok(()),
            |_| {},
        )
        .unwrap();
        assert!(st.stopped);
        assert!(st.blocks < 60);
    }

    /// A decoder that stalls lets symbols queue up only to
    /// MAX_QUEUED_CHUNKS chunks: the parser then drops whole blocks and
    /// reports them as one loss, instead of growing without bound (the
    /// capture keeps draining whatever the decoder does).
    #[test]
    fn a_stalled_decoder_drops_whole_blocks_beyond_the_queue_bound() {
        let chunk = 8_192; // bound: 4 chunks = 8 blocks of 4 096
        let blocks = 60;
        let stop = std::sync::atomic::AtomicBool::new(false);
        let mut first = true;
        let st = run(
            bytes_of(words(blocks)),
            Source::Live,
            chunk,
            &stop,
            |z, _| {
                if first {
                    // the parser keeps going meanwhile
                    first = false;
                    std::thread::sleep(std::time::Duration::from_millis(1500));
                }
                Ok(z.len())
            },
            |_| Ok(()),
            |_| {},
        )
        .unwrap();
        assert!(st.blocks_dropped > 0, "nothing dropped");
        assert_eq!(st.blocks_dropped, st.blocks_lost);
        assert_eq!(st.blocks + st.blocks_dropped, blocks as u64);
        assert!(st.losses >= 1);
        assert!(
            st.max_queued_symbols <= (MAX_QUEUED_CHUNKS * chunk + 4096) as u64,
            "queue grew to {}",
            st.max_queued_symbols
        );
    }

    /// A loss in the words is reported and ends the contiguous stretch.
    #[test]
    fn a_loss_ends_the_stretch() {
        let mut b = words(20);
        // drop the middle of block 10 (block = 9 + 4096·2·6/16 + 2 words)
        let bw = (9 + 4096 * 2 * 6 / 16 + 2) * 2;
        b.drain(10 * bw + 100..10 * bw + 900);
        let stop = std::sync::atomic::AtomicBool::new(false);
        let mut ends = 0;
        let st = run(
            bytes_of(b),
            Source::File,
            8_192,
            &stop,
            |z, _| Ok(z.len()),
            |_| {
                ends += 1;
                Ok(())
            },
            |_| {},
        )
        .unwrap();
        assert_eq!(st.losses, 1);
        assert_eq!(st.blocks_lost, 1);
        assert_eq!(st.blocks, 19);
        assert_eq!(ends, 2, "the loss and the end each close a stretch");
    }
}
