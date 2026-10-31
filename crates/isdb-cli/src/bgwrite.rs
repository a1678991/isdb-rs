//! A file written on its own thread, so a receiver's decode loop never
//! waits on the filesystem.
//!
//! `isdb rx` writes its TS / TLV streams from the thread that decodes the
//! symbols; a live stream gives that thread only a few seconds of queue
//! ([`crate::symstream::MAX_QUEUED_CHUNKS`]) before blocks are dropped.
//! A `write` that blocks for longer (a nearly full disk, writeback
//! throttling, an NFS server's latency) would cost symbols. Here the bytes
//! are gathered into [`BLOCK`]-byte buffers and handed over a bounded
//! channel to a thread that writes them, so up to [`QUEUED_BLOCKS`] blocks
//! (per file) can wait for the disk. Beyond that the writer blocks the
//! caller (back-pressure: memory stays bounded).
//!
//! A write error is returned by the next `write` or `flush` after the
//! thread met it; `flush` waits until everything before it is written and
//! flushed. Dropping the writer writes what is pending and waits for the
//! thread (an error then is lost: call `flush` first).

use std::io::Write;
use std::sync::mpsc::{Receiver, SyncSender, sync_channel};
use std::thread::JoinHandle;

/// Bytes gathered before they are handed to the thread.
pub const BLOCK: usize = 1 << 20;
/// Blocks that may wait for the disk, per file (128 MiB: ≈ 28 s of one
/// ISDB-S3 TLV stream at the ≈ 4.5 MB/s of a BS 4K service).
pub const QUEUED_BLOCKS: usize = 128;

enum Msg {
    Data(Vec<u8>),
    Flush(SyncSender<std::io::Result<()>>),
}

/// See the module documentation.
pub struct BackgroundFile {
    buf: Vec<u8>,
    tx: Option<SyncSender<Msg>>,
    thread: Option<JoinHandle<std::io::Result<()>>>,
}

impl BackgroundFile {
    /// Write to `w` on a new thread.
    pub fn new<W: Write + Send + 'static>(w: W) -> Self {
        Self::with_queue(w, QUEUED_BLOCKS)
    }

    fn with_queue<W: Write + Send + 'static>(mut w: W, blocks: usize) -> Self {
        let (tx, rx): (SyncSender<Msg>, Receiver<Msg>) = sync_channel(blocks);
        let thread = std::thread::spawn(move || -> std::io::Result<()> {
            for m in rx {
                match m {
                    Msg::Data(b) => w.write_all(&b)?,
                    Msg::Flush(ack) => {
                        let r = w.flush();
                        let failed = r.is_err();
                        let _ = ack.send(r);
                        if failed {
                            return Ok(());
                        }
                    }
                }
            }
            w.flush()
        });
        Self {
            buf: Vec::with_capacity(BLOCK),
            tx: Some(tx),
            thread: Some(thread),
        }
    }

    /// The thread's error once it has stopped (the channel is closed).
    fn failed(&mut self) -> std::io::Error {
        self.tx = None;
        match self.thread.take().map(JoinHandle::join) {
            Some(Ok(Err(e))) => e,
            Some(Err(p)) => std::panic::resume_unwind(p),
            _ => std::io::Error::new(std::io::ErrorKind::BrokenPipe, "the file's writer stopped"),
        }
    }

    fn send(&mut self, m: Msg) -> std::io::Result<()> {
        let Some(tx) = &self.tx else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "the file's writer stopped",
            ));
        };
        if tx.send(m).is_err() {
            return Err(self.failed());
        }
        Ok(())
    }

    fn send_buf(&mut self) -> std::io::Result<()> {
        if self.buf.is_empty() {
            return Ok(());
        }
        let b = std::mem::replace(&mut self.buf, Vec::with_capacity(BLOCK));
        self.send(Msg::Data(b))
    }
}

impl Write for BackgroundFile {
    fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
        self.buf.extend_from_slice(data);
        if self.buf.len() >= BLOCK {
            self.send_buf()?;
        }
        Ok(data.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.send_buf()?;
        let (ack, done) = sync_channel(1);
        self.send(Msg::Flush(ack))?;
        match done.recv() {
            Ok(r) => r,
            Err(_) => Err(self.failed()),
        }
    }
}

impl Drop for BackgroundFile {
    fn drop(&mut self) {
        let _ = self.send_buf();
        self.tx = None;
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    /// A writer that records what it is given, optionally slowly, and
    /// fails after `fail_after` bytes.
    #[derive(Clone)]
    struct Probe {
        got: Arc<Mutex<Vec<u8>>>,
        delay_ms: u64,
        fail_after: usize,
    }

    impl Write for Probe {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            std::thread::sleep(std::time::Duration::from_millis(self.delay_ms));
            let mut g = self.got.lock().unwrap();
            if g.len() + b.len() > self.fail_after {
                return Err(std::io::Error::other("disk full"));
            }
            g.extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    fn probe(delay_ms: u64, fail_after: usize) -> Probe {
        Probe {
            got: Arc::new(Mutex::new(Vec::new())),
            delay_ms,
            fail_after,
        }
    }

    /// Every byte arrives, in order, by the end of a flush.
    #[test]
    fn writes_everything_in_order() {
        let p = probe(0, usize::MAX);
        let mut w = BackgroundFile::new(p.clone());
        let want: Vec<u8> = (0..3 * BLOCK + 12_345)
            .map(|i| u8::try_from(i * 7 % 251).unwrap())
            .collect();
        for c in want.chunks(4_000) {
            w.write_all(c).unwrap();
        }
        w.flush().unwrap();
        assert!(*p.got.lock().unwrap() == want);
        drop(w);
    }

    /// A slow disk does not hold the caller up while the queue has room.
    #[test]
    fn a_slow_disk_does_not_block_the_caller() {
        let p = probe(200, usize::MAX);
        let mut w = BackgroundFile::with_queue(p.clone(), 8);
        let t = std::time::Instant::now();
        // 4 blocks: each write of a block to the probe takes 200 ms
        w.write_all(&vec![1u8; 4 * BLOCK]).unwrap();
        assert!(
            t.elapsed() < std::time::Duration::from_millis(150),
            "{:?}",
            t.elapsed()
        );
        w.flush().unwrap();
        assert_eq!(p.got.lock().unwrap().len(), 4 * BLOCK);
    }

    /// A write error on the thread is returned to the caller.
    #[test]
    fn an_error_reaches_the_caller() {
        let p = probe(0, BLOCK / 2);
        let mut w = BackgroundFile::new(p);
        let mut r = Ok(());
        for _ in 0..4 {
            r = w.write_all(&vec![0u8; BLOCK]);
            if r.is_err() {
                break;
            }
        }
        let e = r.and_then(|()| w.flush()).unwrap_err();
        assert!(e.to_string().contains("disk full"), "{e}");
    }
}
