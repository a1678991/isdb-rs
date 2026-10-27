//! Reading transport packets from bytes: sync acquisition, packet-size
//! autodetection and resynchronisation.
//!
//! H.222.0 defines only the 188-byte packet ("Transport Stream packets
//! shall be 188 bytes long", §2.4.3) whose first byte is sync_byte 0x47
//! (§2.4.3.3), and the stream as a sequence of packets "while (nextbits()
//! == sync_byte)" (Table 2-1).  Two framings found in practice are also
//! accepted; they are *not* H.222.0 syntax and their extra bytes are passed
//! through opaquely:
//! * 192 bytes: a 4-byte prefix (e.g. arrival timestamp) before each packet;
//! * 204 bytes: a 16-byte trailer after each packet (e.g. RS(204,188)
//!   parity, ARIB STD-B31 §3.3).
//!
//! Sync is acquired where 0x47 appears at the packet period for
//! [`Synchronizer::count`] consecutive packets; on a missing sync byte the
//! reader searches forward and re-acquires.

use std::io::{self, Read};

use crate::packet::{PACKET_SIZE, SYNC_BYTE};

/// Framing of transport packets in a byte stream: H.222.0 §2.4.3 188-byte
/// packets, optionally with a non-H.222.0 prefix/trailer (module docs).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum PacketFormat {
    /// Plain 188-byte packets (H.222.0 §2.4.3).
    Ts188,
    /// 4-byte prefix + 188-byte packet.
    Ts192,
    /// 188-byte packet + 16-byte trailer.
    Ts204,
}

impl PacketFormat {
    /// All formats in autodetection order.
    pub const ALL: [PacketFormat; 3] = [
        PacketFormat::Ts188,
        PacketFormat::Ts192,
        PacketFormat::Ts204,
    ];

    /// Bytes per unit.
    pub const fn size(self) -> usize {
        match self {
            PacketFormat::Ts188 => 188,
            PacketFormat::Ts192 => 192,
            PacketFormat::Ts204 => 204,
        }
    }

    /// Offset of the sync byte within a unit.
    pub const fn sync_offset(self) -> usize {
        match self {
            PacketFormat::Ts192 => 4,
            _ => 0,
        }
    }
}

/// Largest unit size.
const MAX_UNIT: usize = 204;
/// Bytes requested from the source per read.
const READ_CHUNK: usize = 64 * 1024;

/// Counters kept by the readers (sync per H.222.0 §2.4.3.3 / Table 2-1).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ReaderStats {
    /// Times sync was acquired (first acquisition included).
    pub acquisitions: u64,
    /// Times an expected sync byte was missing.
    pub sync_losses: u64,
    /// Bytes skipped while searching for sync.
    pub skipped_bytes: u64,
    /// Bytes at the end too short for a whole unit.
    pub trailing_bytes: u64,
}

/// Sync state machine shared by [`SliceReader`] and [`StreamReader`]:
/// sync_byte 0x47 (H.222.0 §2.4.3.3) at the packet period (Table 2-1).
#[derive(Debug, Clone)]
pub struct Synchronizer {
    /// Consecutive sync bytes required to (re)acquire.
    pub count: usize,
    /// Fixed format, or `None` to autodetect.
    pub forced: Option<PacketFormat>,
    format: Option<PacketFormat>,
    synced: bool,
    /// Counters.
    pub stats: ReaderStats,
}

impl Synchronizer {
    /// Autodetecting synchronizer requiring `count` (≥ 1) consecutive syncs.
    pub fn new(count: usize) -> Self {
        Self {
            count: count.max(1),
            forced: None,
            format: None,
            synced: false,
            stats: ReaderStats::default(),
        }
    }

    /// Current format (after the first acquisition).
    pub fn format(&self) -> Option<PacketFormat> {
        self.format
    }

    fn lookahead(&self) -> usize {
        (self.count + 1) * MAX_UNIT
    }

    fn locate(&self, v: &[u8], eof: bool) -> Result<(PacketFormat, usize), usize> {
        let mut order: Vec<PacketFormat> = Vec::with_capacity(3);
        if let Some(f) = self.forced {
            order.push(f);
        } else {
            if let Some(f) = self.format {
                order.push(f);
            }
            order.extend(
                PacketFormat::ALL
                    .iter()
                    .copied()
                    .filter(|f| Some(*f) != self.format),
            );
        }
        let limit = if eof {
            v.len()
        } else {
            v.len().saturating_sub(self.lookahead()) + 1
        };
        for p in 0..limit {
            for &f in &order {
                let (size, off) = (f.size(), f.sync_offset());
                if p + off >= v.len() || v[p + off] != SYNC_BYTE {
                    continue;
                }
                let avail = (v.len() - p) / size;
                let need = if eof {
                    self.count.min(avail)
                } else {
                    self.count
                };
                if need > 0 && (0..need).all(|k| v[p + k * size + off] == SYNC_BYTE) {
                    return Ok((f, p));
                }
            }
        }
        Err(limit.min(v.len()))
    }

    /// Advance `*pos` in `buf` to the start of the next unit and return its
    /// format, or `None` if more data is needed (`!eof`) or the data ended.
    pub fn next_unit(&mut self, buf: &[u8], pos: &mut usize, eof: bool) -> Option<PacketFormat> {
        loop {
            let v = &buf[*pos..];
            if self.synced {
                let f = self.format.expect("synced implies format");
                if v.len() < f.size() {
                    if eof {
                        self.stats.trailing_bytes += v.len() as u64;
                        *pos = buf.len();
                    }
                    return None;
                }
                if v[f.sync_offset()] == SYNC_BYTE {
                    return Some(f);
                }
                self.synced = false;
                self.stats.sync_losses += 1;
                *pos += 1;
                self.stats.skipped_bytes += 1;
                continue;
            }
            if v.is_empty() || (!eof && v.len() < self.lookahead()) {
                return None;
            }
            match self.locate(v, eof) {
                Ok((f, p)) => {
                    *pos += p;
                    self.stats.skipped_bytes += p as u64;
                    self.stats.acquisitions += 1;
                    self.format = Some(f);
                    self.synced = true;
                }
                Err(discard) => {
                    *pos += discard;
                    self.stats.skipped_bytes += discard as u64;
                    if eof {
                        return None;
                    }
                }
            }
        }
    }
}

/// A transport packet (H.222.0 Table 2-2) borrowed from a slice.
#[derive(Debug, Clone, Copy)]
pub struct PacketRef<'a> {
    /// Sequential index of the packet (0-based) among packets yielded.
    pub index: u64,
    /// Byte offset of the packet's sync byte in the input.
    pub offset: u64,
    /// Framing of the unit.
    pub format: PacketFormat,
    /// The 188-byte transport packet.
    pub packet: &'a [u8; PACKET_SIZE],
    /// The 4-byte prefix of a 192-byte unit (empty otherwise).
    pub prefix: &'a [u8],
    /// The 16-byte trailer of a 204-byte unit (empty otherwise).
    pub trailer: &'a [u8],
}

/// Iterator over the transport packets (H.222.0 Table 2-1) of a byte slice.
#[derive(Debug, Clone)]
pub struct SliceReader<'a> {
    buf: &'a [u8],
    pos: usize,
    index: u64,
    /// Sync state and counters.
    pub sync: Synchronizer,
}

impl<'a> SliceReader<'a> {
    /// Autodetecting reader requiring 5 consecutive syncs to acquire.
    pub fn new(buf: &'a [u8]) -> Self {
        Self::with_sync(buf, Synchronizer::new(5))
    }

    /// Reader with an explicit synchronizer configuration.
    pub fn with_sync(buf: &'a [u8], sync: Synchronizer) -> Self {
        Self {
            buf,
            pos: 0,
            index: 0,
            sync,
        }
    }
}

impl<'a> Iterator for SliceReader<'a> {
    type Item = PacketRef<'a>;

    fn next(&mut self) -> Option<PacketRef<'a>> {
        let f = self.sync.next_unit(self.buf, &mut self.pos, true)?;
        let unit = &self.buf[self.pos..self.pos + f.size()];
        let off = f.sync_offset();
        let packet: &[u8; PACKET_SIZE] =
            unit[off..off + PACKET_SIZE].try_into().expect("188 bytes");
        let r = PacketRef {
            index: self.index,
            offset: (self.pos + off) as u64,
            format: f,
            packet,
            prefix: &unit[..off],
            trailer: &unit[off + PACKET_SIZE..],
        };
        self.pos += f.size();
        self.index += 1;
        Some(r)
    }
}

/// An owned transport packet (H.222.0 Table 2-2) read from a [`Read`]
/// source.
#[derive(Debug, Clone)]
pub struct OwnedPacket {
    /// Sequential index of the packet among packets yielded.
    pub index: u64,
    /// Byte offset of the packet's sync byte in the input.
    pub offset: u64,
    /// Framing of the unit.
    pub format: PacketFormat,
    unit: [u8; MAX_UNIT],
}

impl OwnedPacket {
    /// The 188-byte transport packet.
    pub fn packet(&self) -> &[u8; PACKET_SIZE] {
        let o = self.format.sync_offset();
        self.unit[o..o + PACKET_SIZE].try_into().expect("188 bytes")
    }

    /// The 4-byte prefix of a 192-byte unit (empty otherwise).
    pub fn prefix(&self) -> &[u8] {
        &self.unit[..self.format.sync_offset()]
    }

    /// The 16-byte trailer of a 204-byte unit (empty otherwise).
    pub fn trailer(&self) -> &[u8] {
        let o = self.format.sync_offset() + PACKET_SIZE;
        &self.unit[o..self.format.size()]
    }
}

/// Iterator over the transport packets (H.222.0 Table 2-1) of a [`Read`]
/// source.
#[derive(Debug)]
pub struct StreamReader<R> {
    src: R,
    buf: Vec<u8>,
    pos: usize,
    base: u64,
    eof: bool,
    index: u64,
    /// Sync state and counters.
    pub sync: Synchronizer,
}

impl<R: Read> StreamReader<R> {
    /// Autodetecting reader requiring 5 consecutive syncs to acquire.
    pub fn new(src: R) -> Self {
        Self::with_sync(src, Synchronizer::new(5))
    }

    /// Reader with an explicit synchronizer configuration.
    pub fn with_sync(src: R, sync: Synchronizer) -> Self {
        Self {
            src,
            buf: Vec::new(),
            pos: 0,
            base: 0,
            eof: false,
            index: 0,
            sync,
        }
    }

    fn fill(&mut self) -> io::Result<()> {
        if self.pos > 1 << 20 {
            self.buf.drain(..self.pos);
            self.base += self.pos as u64;
            self.pos = 0;
        }
        let want = (self.sync.count + 2) * MAX_UNIT;
        // Called once per packet: read straight into the buffer, and only
        // when it runs short (no per-call scratch buffer to initialise).
        while !self.eof && self.buf.len() - self.pos < want {
            let old = self.buf.len();
            self.buf.resize(old + READ_CHUNK, 0);
            let r = self.src.read(&mut self.buf[old..]);
            let n = *r.as_ref().unwrap_or(&0);
            self.buf.truncate(old + n);
            match r {
                Ok(0) => self.eof = true,
                Ok(_) => {}
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }

    /// Next packet, `Ok(None)` at end of input.
    pub fn next_packet(&mut self) -> io::Result<Option<OwnedPacket>> {
        loop {
            self.fill()?;
            if let Some(f) = self.sync.next_unit(&self.buf, &mut self.pos, self.eof) {
                let mut unit = [0u8; MAX_UNIT];
                unit[..f.size()].copy_from_slice(&self.buf[self.pos..self.pos + f.size()]);
                let p = OwnedPacket {
                    index: self.index,
                    offset: self.base + (self.pos + f.sync_offset()) as u64,
                    format: f,
                    unit,
                };
                self.pos += f.size();
                self.index += 1;
                return Ok(Some(p));
            }
            if self.eof {
                return Ok(None);
            }
        }
    }
}

impl<R: Read> Iterator for StreamReader<R> {
    type Item = io::Result<OwnedPacket>;

    fn next(&mut self) -> Option<Self::Item> {
        self.next_packet().transpose()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::packet::Packet;
    use crate::pid::Pid;

    fn stream(n: usize, fmt: PacketFormat) -> Vec<u8> {
        let mut v = Vec::new();
        for i in 0..n {
            // payload with occasional sync-byte emulation
            let mut payload: Vec<u8> = (0..184).map(|k| (k * 3 + i) as u8).collect();
            payload[0] = i as u8;
            let p = Packet::with_payload(
                Pid::new(0x100 + i as u16).unwrap(),
                false,
                (i & 15) as u8,
                &payload,
            )
            .unwrap()
            .encode()
            .unwrap();
            match fmt {
                PacketFormat::Ts188 => v.extend_from_slice(&p),
                PacketFormat::Ts192 => {
                    v.extend_from_slice(&(i as u32).to_be_bytes());
                    v.extend_from_slice(&p);
                }
                PacketFormat::Ts204 => {
                    v.extend_from_slice(&p);
                    v.extend_from_slice(&[0xA5; 16]);
                }
            }
        }
        v
    }

    #[test]
    fn autodetects_each_format_with_garbage_prefix() {
        for fmt in PacketFormat::ALL {
            let mut data = vec![0x12, 0x47, 0x00, 0x47, 0x99];
            data.extend(stream(20, fmt));
            let pkts: Vec<_> = SliceReader::new(&data).collect();
            assert_eq!(pkts.len(), 20, "{fmt:?}");
            assert!(pkts.iter().all(|p| p.format == fmt));
            assert_eq!(pkts[0].offset, 5 + fmt.sync_offset() as u64);
            assert_eq!(pkts[3].packet[4], 3);
            assert_eq!(pkts[3].index, 3);
            let mut sr = StreamReader::new(&data[..]);
            let mut n = 0;
            while let Some(p) = sr.next_packet().unwrap() {
                assert_eq!(p.packet(), pkts[n].packet);
                assert_eq!(p.offset, pkts[n].offset);
                assert_eq!(p.prefix(), pkts[n].prefix);
                assert_eq!(p.trailer(), pkts[n].trailer);
                n += 1;
            }
            assert_eq!(n, 20);
        }
    }

    #[test]
    fn resyncs_after_corruption() {
        let mut data = stream(30, PacketFormat::Ts188);
        // drop 50 bytes in the middle of packet 10
        data.drain(10 * 188 + 20..10 * 188 + 70);
        let mut r = SliceReader::new(&data);
        let pkts: Vec<_> = r.by_ref().collect();
        // packet 10 still starts with a sync byte and is yielded (corrupt,
        // swallowing 50 bytes of packet 11); packet 11 is lost; resync at 12.
        assert_eq!(pkts.len(), 29);
        assert_eq!(r.sync.stats.sync_losses, 1);
        assert_eq!(r.sync.stats.acquisitions, 2);
        assert_eq!(pkts[10].packet[4], 10);
        assert_eq!(pkts[11].packet[4], 12);
        assert_eq!(pkts[11].offset, 12 * 188 - 50);
        let mut sr = StreamReader::new(&data[..]);
        let mut n = 0;
        while sr.next_packet().unwrap().is_some() {
            n += 1;
        }
        assert_eq!(n, 29);
    }

    #[test]
    fn short_and_empty_inputs() {
        assert_eq!(SliceReader::new(&[]).count(), 0);
        assert_eq!(SliceReader::new(&[0x47; 100]).count(), 0);
        let one = stream(1, PacketFormat::Ts188);
        assert_eq!(SliceReader::new(&one).count(), 1);
        let mut sr = StreamReader::new(&[0u8; 1000][..]);
        assert!(sr.next_packet().unwrap().is_none());
    }
}
