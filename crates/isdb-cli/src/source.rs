//! Packet sources: a reader thread turns a byte stream (file, FIFO, stdin)
//! into 188-byte TS packets (H.222.0 §2.4.3.2, sync byte 0x47) on a bounded
//! queue; [`PacketStream`] takes them according to the feed policy.

use anyhow::{Context, Result, bail};
use std::io::Read;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, TryRecvError, sync_channel};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use isdb_ts::packet::{Header, TransportScramblingControl};

/// 188-byte packet.
pub type Packet = [u8; 188];

/// H.222.0 §2.4.3.3 null packet (PID 0x1FFF, payload only, 0xFF stuffing).
pub fn null_packet() -> Packet {
    isdbt_phy::null_packet()
}

/// PID of a packet (H.222.0 Table 2-2).
pub fn pid(p: &Packet) -> u16 {
    (u16::from(p[1] & 0x1f) << 8) | u16::from(p[2])
}

/// True for a null packet (PID 0x1FFF).
pub fn is_null(p: &Packet) -> bool {
    pid(p) == 0x1fff
}

/// transport_scrambling_control of a packet (H.222.0 §2.4.3.3, Table 2-4);
/// anything but `NotScrambled` means the payload is scrambled.
pub fn scrambling_control(p: &Packet) -> TransportScramblingControl {
    Header::of_packet(p).transport_scrambling_control
}

/// The first scrambled packet met by any reader thread (the input is then
/// refused: `isdb` never descrambles and does not transmit scrambled
/// content).
static SCRAMBLED: OnceLock<String> = OnceLock::new();

/// Error if an input carried scrambled packets (see [`scrambling_control`]).
pub fn check_not_scrambled() -> Result<()> {
    if let Some(m) = SCRAMBLED.get() {
        bail!("{m}");
    }
    Ok(())
}

/// How a source behaves when its input runs dry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum Feed {
    /// Read ahead (blocking); end the transmission at EOF.
    File,
    /// Rewind at EOF and play again (continuity counters will jump).
    Loop,
    /// Live pipe/FIFO: never block the modulator; pad with null packets
    /// whenever the input lags, end when the writer closes.
    Live,
}

/// Opens (and for [`Feed::Loop`] re-opens) the byte stream.
pub type Opener = Box<dyn FnMut() -> std::io::Result<Box<dyn Read + Send>> + Send>;

/// Opener for a path; "-" is stdin (not re-openable).
pub fn path_opener(path: &str) -> Opener {
    let path = path.to_string();
    let mut stdin_used = false;
    Box::new(move || {
        if path == "-" {
            if stdin_used {
                return Err(std::io::Error::other("stdin cannot be rewound"));
            }
            stdin_used = true;
            Ok(Box::new(std::io::stdin()))
        } else {
            Ok(Box::new(std::fs::File::open(&path)?))
        }
    })
}

/// Reader-thread counters.
#[derive(Debug, Default)]
pub struct ReaderStats {
    /// Bytes skipped while (re)synchronising on 0x47.
    pub skipped_bytes: AtomicU64,
    /// Times the input was rewound (loop feed).
    pub rewinds: AtomicU64,
}

/// Packet queue fed by a reader thread.
pub struct PacketStream {
    rx: Receiver<Packet>,
    feed: Feed,
    ended: bool,
    /// Reader counters.
    pub stats: Arc<ReaderStats>,
    /// Name for messages.
    pub name: String,
}

/// Split a byte buffer into packets, resynchronising on the sync byte: a
/// packet is accepted at `i` when `buf[i] == 0x47` and, if the next packet
/// start is already buffered, `buf[i+188] == 0x47` too.  Returns the number
/// of bytes consumed (accepted packets plus skipped garbage).
pub fn split_packets(
    buf: &[u8],
    skipped: &mut u64,
    mut emit: impl FnMut(Packet) -> bool,
) -> (usize, bool) {
    let mut i = 0;
    while i + 188 <= buf.len() {
        if buf[i] != 0x47 || (i + 376 <= buf.len() && buf[i + 188] != 0x47) {
            i += 1;
            *skipped += 1;
            continue;
        }
        if !emit(buf[i..i + 188].try_into().expect("188 bytes")) {
            return (i, false);
        }
        i += 188;
    }
    (i, true)
}

impl PacketStream {
    /// Start a reader thread on `opener`; up to `depth` packets are queued.
    pub fn spawn(name: &str, mut opener: Opener, feed: Feed, depth: usize) -> Result<Self> {
        let mut r = opener().with_context(|| format!("open {name}"))?;
        let (tx, rx) = sync_channel::<Packet>(depth.max(1));
        let stats = Arc::new(ReaderStats::default());
        let st = stats.clone();
        let tname = name.to_string();
        std::thread::Builder::new()
            .name(format!("read {name}"))
            .spawn(move || {
                let mut buf = Vec::<u8>::with_capacity(1 << 20);
                let mut chunk = vec![0u8; 1 << 16];
                let mut got_any = false;
                loop {
                    let n = match r.read(&mut chunk) {
                        Ok(0) => 0,
                        Ok(n) => n,
                        Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                        Err(e) => {
                            eprintln!("isdb: read {tname}: {e}");
                            0
                        }
                    };
                    if n == 0 {
                        if feed == Feed::Loop && got_any {
                            match opener() {
                                Ok(nr) => {
                                    r = nr;
                                    buf.clear();
                                    st.rewinds.fetch_add(1, Ordering::Relaxed);
                                    continue;
                                }
                                Err(e) => eprintln!("isdb: reopen {tname}: {e}"),
                            }
                        }
                        return; // dropping `tx` signals the end
                    }
                    buf.extend_from_slice(&chunk[..n]);
                    let mut skipped = 0;
                    let (used, alive) = split_packets(&buf, &mut skipped, |p| {
                        let tsc = scrambling_control(&p);
                        if tsc.is_scrambled() && !is_null(&p) {
                            let m = format!(
                                "{tname}: scrambled packet on PID {:#06x} (transport_scrambling_control '{tsc}'); \
                                 scrambled input is refused: isdb does not descramble (no B-CAS / CAS support) \
                                 and transmits only content you have the rights to",
                                pid(&p),
                            );
                            eprintln!("isdb: {m}");
                            let _ = SCRAMBLED.set(m);
                            crate::stop::flag().store(true, Ordering::SeqCst);
                            return false;
                        }
                        got_any = true;
                        tx.send(p).is_ok()
                    });
                    st.skipped_bytes.fetch_add(skipped, Ordering::Relaxed);
                    if !alive {
                        return;
                    }
                    buf.drain(..used);
                }
            })?;
        Ok(Self {
            rx,
            feed,
            ended: false,
            stats,
            name: name.to_string(),
        })
    }

    /// Stream over in-memory packets (tests).
    #[cfg(test)]
    pub fn from_receiver(name: &str, rx: Receiver<Packet>, feed: Feed) -> Self {
        Self {
            rx,
            feed,
            ended: false,
            stats: Arc::default(),
            name: name.into(),
        }
    }

    /// Feed policy.
    pub fn feed(&self) -> Feed {
        self.feed
    }

    /// True once the input has ended (EOF / writer closed) and is drained.
    pub fn ended(&self) -> bool {
        self.ended
    }

    /// Next packet according to the feed policy: `File`/`Loop` block until
    /// a packet arrives (polling `stop`), `Live` never blocks.  `None` means
    /// no packet now (live lag) or end of input ([`Self::ended`]) or stop.
    pub fn next(&mut self, stop: &AtomicBool) -> Option<Packet> {
        if self.ended {
            return None;
        }
        match self.feed {
            Feed::Live => match self.rx.try_recv() {
                Ok(p) => Some(p),
                Err(TryRecvError::Empty) => None,
                Err(TryRecvError::Disconnected) => {
                    self.ended = true;
                    None
                }
            },
            Feed::File | Feed::Loop => loop {
                match self.rx.recv_timeout(Duration::from_millis(50)) {
                    Ok(p) => return Some(p),
                    Err(RecvTimeoutError::Timeout) if !stop.load(Ordering::Relaxed) => continue,
                    Err(RecvTimeoutError::Timeout) => return None,
                    Err(RecvTimeoutError::Disconnected) => {
                        self.ended = true;
                        return None;
                    }
                }
            },
        }
    }
}

/// Queue depth for a layer carrying `per_frame` packets per OFDM frame of
/// `frame_s` seconds: about two seconds of capacity, at least 1024.
pub fn queue_depth(per_frame: usize, frame_s: f64) -> usize {
    // f64 -> usize saturates: a packet count, at least 1024
    #[allow(clippy::cast_possible_truncation)]
    let n = ((per_frame as f64 / frame_s) * 2.0).ceil().max(1024.0) as usize;
    n
}

/// Check a path-based input is usable with the feed policy.
pub fn check_feed(path: &str, feed: Feed) -> Result<()> {
    if path == "-" && feed == Feed::Loop {
        bail!("--feed loop cannot rewind stdin");
    }
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::io::Cursor;

    pub(crate) fn pkt(pid: u16, n: u32) -> Packet {
        let mut p = [0u8; 188];
        p[0] = 0x47;
        p[1..3].copy_from_slice(&pid.to_be_bytes());
        p[3] = 0x10 | (n & 15) as u8;
        p[4..8].copy_from_slice(&n.to_be_bytes());
        p
    }

    fn bytes(pkts: &[Packet]) -> Vec<u8> {
        pkts.iter().flatten().copied().collect()
    }

    fn opener_of(data: Vec<u8>) -> Opener {
        Box::new(move || Ok(Box::new(Cursor::new(data.clone())) as Box<dyn Read + Send>))
    }

    #[test]
    fn resync_skips_garbage() {
        let mut b = vec![0x00, 0x47, 0x12];
        b.extend(bytes(&[pkt(0x100, 0), pkt(0x100, 1)]));
        let mut skipped = 0;
        let mut got = vec![];
        let (used, _) = split_packets(&b, &mut skipped, |p| {
            got.push(p);
            true
        });
        assert_eq!(skipped, 3);
        assert_eq!(used, b.len());
        assert_eq!(got, vec![pkt(0x100, 0), pkt(0x100, 1)]);
    }

    #[test]
    fn file_feed_ends_at_eof() {
        let pk: Vec<_> = (0..10).map(|n| pkt(0x100, n)).collect();
        let mut s = PacketStream::spawn("t", opener_of(bytes(&pk)), Feed::File, 4).unwrap();
        let stop = AtomicBool::new(false);
        let mut got = vec![];
        while let Some(p) = s.next(&stop) {
            got.push(p);
        }
        assert!(s.ended());
        assert_eq!(got, pk);
        assert_eq!(s.next(&stop), None);
    }

    #[test]
    fn loop_feed_rewinds() {
        let pk: Vec<_> = (0..3).map(|n| pkt(0x100, n)).collect();
        let mut s = PacketStream::spawn("t", opener_of(bytes(&pk)), Feed::Loop, 4).unwrap();
        let stop = AtomicBool::new(false);
        let got: Vec<_> = (0..10).map(|_| s.next(&stop).unwrap()).collect();
        for (i, p) in got.iter().enumerate() {
            assert_eq!(*p, pk[i % 3]);
        }
        assert!(s.stats.rewinds.load(Ordering::Relaxed) >= 3);
        assert!(!s.ended());
    }

    #[test]
    fn empty_loop_input_does_not_spin() {
        let mut s = PacketStream::spawn("t", opener_of(vec![]), Feed::Loop, 4).unwrap();
        assert_eq!(s.next(&AtomicBool::new(false)), None);
        assert!(s.ended());
    }

    #[test]
    fn live_feed_never_blocks() {
        let (tx, rx) = sync_channel(8);
        let mut s = PacketStream::from_receiver("t", rx, Feed::Live);
        let stop = AtomicBool::new(false);
        let t = std::time::Instant::now();
        assert_eq!(s.next(&stop), None);
        assert!(!s.ended());
        tx.send(pkt(0x100, 1)).unwrap();
        assert_eq!(s.next(&stop), Some(pkt(0x100, 1)));
        drop(tx);
        assert_eq!(s.next(&stop), None);
        assert!(s.ended());
        assert!(t.elapsed() < Duration::from_millis(50));
    }

    #[test]
    fn file_feed_honours_stop() {
        let (_tx, rx) = sync_channel::<Packet>(8);
        let mut s = PacketStream::from_receiver("t", rx, Feed::File);
        let stop = AtomicBool::new(true);
        assert_eq!(s.next(&stop), None);
        assert!(!s.ended());
    }

    #[test]
    fn stdin_loop_rejected() {
        assert!(check_feed("-", Feed::Loop).is_err());
        assert!(check_feed("-", Feed::Live).is_ok());
    }
}
