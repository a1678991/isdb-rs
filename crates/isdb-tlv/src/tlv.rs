//! TLV packets (ARIB STD-B32 v3.11 Part 3 §3.5) and their reassembly from
//! the slots of an ISDB-S3 TLV stream (ARIB STD-B44 v2.1 §3.2 (1),
//! Fig. 3.2-1 and its Appendix 1).
//!
//! A TLV packet is `01` `111111` (one byte 7Fh), packet type (8), data
//! length (16, bytes following), data. Types: 01h IPv4, 02h IPv6, 03h
//! header-compressed IP, FEh transmission control signal, FFh null (all
//! FFh data); others undefined.
//!
//! A TLV stream is the concatenation, in slot order and frame after frame,
//! of the data of the slots the TMCC assigns to it; packets run across
//! slot boundaries. Each slot's header begins with the 16-bit head-TLV
//! indication: the byte offset of the first packet that starts in the slot
//! (from the start of the slot's data), FFFFh when none starts. The
//! assembler uses it to (re)gain packet sync and to check it.

/// The fixed first byte of every TLV packet: `01` + `111111`.
pub const SYNC: u8 = 0x7F;

/// TLV packet type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum PacketType {
    /// IPv4 packet (01h).
    Ipv4,
    /// IPv6 packet (02h).
    Ipv6,
    /// Header-compressed IP packet (03h).
    Compressed,
    /// Transmission control signal (FEh).
    Control,
    /// Null (FFh).
    Null,
    /// Undefined value.
    Other(u8),
}

impl PacketType {
    /// From the type byte.
    pub fn from_byte(b: u8) -> Self {
        match b {
            0x01 => Self::Ipv4,
            0x02 => Self::Ipv6,
            0x03 => Self::Compressed,
            0xFE => Self::Control,
            0xFF => Self::Null,
            v => Self::Other(v),
        }
    }
    /// Short name.
    pub fn name(self) -> String {
        match self {
            Self::Ipv4 => "IPv4".into(),
            Self::Ipv6 => "IPv6".into(),
            Self::Compressed => "compressed IP".into(),
            Self::Control => "control".into(),
            Self::Null => "null".into(),
            Self::Other(v) => format!("type {v:#04x}"),
        }
    }
}

/// One TLV packet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Packet {
    /// Its type.
    pub kind: PacketType,
    /// Its data (without the 4-byte header).
    pub data: Vec<u8>,
}

impl Packet {
    /// The packet's bytes, header included, or [`isdb_ts::Error::Overflow`]
    /// when `data` does not fit the 16-bit `data_length` field.
    pub fn encode(&self) -> isdb_ts::Result<Vec<u8>> {
        let ty = match self.kind {
            PacketType::Ipv4 => 1,
            PacketType::Ipv6 => 2,
            PacketType::Compressed => 3,
            PacketType::Control => 0xFE,
            PacketType::Null => 0xFF,
            PacketType::Other(v) => v,
        };
        let len = u16::try_from(self.data.len()).map_err(|_| isdb_ts::Error::Overflow {
            what: "TLV data_length",
            value: self.data.len() as u64,
            bits: 16,
        })?;
        let mut b = vec![SYNC, ty];
        b.extend_from_slice(&len.to_be_bytes());
        b.extend_from_slice(&self.data);
        Ok(b)
    }
}

/// Counters of an [`Assembler`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Stats {
    /// Packets delivered.
    pub packets: u64,
    /// Bytes thrown away while (re)synchronising.
    pub skipped_bytes: u64,
    /// Slots whose head-TLV indication pointed where the assembler did not
    /// find a packet start (packet sync was taken from the indication).
    pub pointer_mismatches: u64,
    /// Slots whose data was missing or failed error correction.
    pub lost_slots: u64,
}

/// Reassembles one TLV stream from its slots.
#[derive(Debug, Clone, Default)]
pub struct Assembler {
    buf: Vec<u8>,
    /// Whether `buf` starts at a packet boundary.
    synced: bool,
    /// Counters.
    pub stats: Stats,
}

impl Assembler {
    /// A new assembler (not yet synchronised).
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed one slot's data with its head-TLV indication; complete packets
    /// go to `out`. `data = None` marks a slot that failed to decode: the
    /// partial packet is dropped and sync is regained at the next
    /// indication.
    pub fn push_slot(&mut self, data: Option<&[u8]>, head: u16, out: &mut Vec<Packet>) {
        let Some(data) = data else {
            self.stats.lost_slots += 1;
            self.stats.skipped_bytes += self.buf.len() as u64;
            self.buf.clear();
            self.synced = false;
            return;
        };
        let start = self.buf.len();
        self.buf.extend_from_slice(data);
        if head != 0xFFFF && (head as usize) < data.len() {
            let at = start + head as usize;
            if self.synced {
                // the parse below must land on `at`; drain what precedes
                // and check
                self.drain(out, Some(at));
            } else {
                self.stats.skipped_bytes += at as u64;
                self.buf.drain(..at);
                self.synced = true;
            }
        }
        if self.synced {
            self.drain(out, None);
        }
    }

    /// Parse complete packets from the front of `buf`. With `until` (an
    /// offset where a packet is known to start), parse only the packets
    /// that end at or before it: if they do not end exactly there, count a
    /// mismatch; either way continue from `until`.
    fn drain(&mut self, out: &mut Vec<Packet>, until: Option<usize>) {
        let limit = until.unwrap_or(usize::MAX);
        let mut pos = 0usize;
        let mut lost = false;
        while pos < limit {
            let rest = &self.buf[pos..];
            if rest.len() < 4 {
                break;
            }
            if rest[0] != SYNC {
                lost = true;
                break;
            }
            let len = u16::from_be_bytes([rest[2], rest[3]]) as usize;
            if rest.len() < 4 + len || pos + 4 + len > limit {
                break;
            }
            out.push(Packet {
                kind: PacketType::from_byte(rest[1]),
                data: rest[4..4 + len].to_vec(),
            });
            self.stats.packets += 1;
            pos += 4 + len;
        }
        match until {
            Some(u) => {
                if pos != u {
                    self.stats.pointer_mismatches += 1;
                    self.stats.skipped_bytes += (u - pos.min(u)) as u64;
                }
                self.buf.drain(..u);
            }
            None if lost => {
                // lost packet sync with no indication to resynchronise on:
                // drop the rest and wait for the next one
                self.stats.skipped_bytes += (self.buf.len() - pos) as u64;
                self.buf.clear();
                self.synced = false;
            }
            None => {
                self.buf.drain(..pos);
            }
        }
    }
}

/// Reads the TLV packets of a TLV stream file (packets back to back, as
/// `isdb rx --system isdbs3` writes them) from any reader, a chunk at a
/// time: memory stays bounded by one chunk and one packet. Bytes that are
/// not a TLV header are skipped up to the next sync byte and counted.
pub struct FileReader<R> {
    r: R,
    buf: Vec<u8>,
    pos: usize,
    eof: bool,
    /// Bytes skipped while looking for a TLV header.
    pub skipped: u64,
}

impl<R: std::io::Read> FileReader<R> {
    /// Bytes read from the underlying reader at a time.
    pub const CHUNK: usize = 1 << 20;

    /// A reader over `r`.
    pub fn new(r: R) -> Self {
        Self {
            r,
            buf: Vec::new(),
            pos: 0,
            eof: false,
            skipped: 0,
        }
    }

    /// The next packet, `None` at the end of the input (a truncated last
    /// packet is counted as skipped).
    pub fn next_packet(&mut self) -> std::io::Result<Option<Packet>> {
        loop {
            let avail = &self.buf[self.pos..];
            if avail.len() >= 4 {
                if avail[0] != SYNC {
                    let n = avail[1..]
                        .iter()
                        .position(|&b| b == SYNC)
                        .map_or(avail.len(), |i| i + 1);
                    self.skipped += n as u64;
                    self.pos += n;
                    continue;
                }
                let len = usize::from(u16::from_be_bytes([avail[2], avail[3]]));
                if avail.len() >= 4 + len {
                    let p = Packet {
                        kind: PacketType::from_byte(avail[1]),
                        data: avail[4..4 + len].to_vec(),
                    };
                    self.pos += 4 + len;
                    return Ok(Some(p));
                }
            }
            if self.eof {
                self.skipped += avail.len() as u64;
                self.pos = self.buf.len();
                return Ok(None);
            }
            self.buf.drain(..self.pos);
            self.pos = 0;
            let start = self.buf.len();
            self.buf.resize(start + Self::CHUNK, 0);
            let n = self.r.read(&mut self.buf[start..])?;
            self.buf.truncate(start + n);
            self.eof = n == 0;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pkt(kind: PacketType, n: usize) -> Packet {
        Packet {
            kind,
            data: (0..=u8::MAX).cycle().take(n).collect(),
        }
    }

    /// Split a byte stream into slots of `size` bytes with their head-TLV
    /// indications.
    fn slots(stream: &[u8], starts: &[usize], size: usize) -> Vec<(Vec<u8>, u16)> {
        stream
            .chunks(size)
            .enumerate()
            .map(|(i, c)| {
                let lo = i * size;
                let head = starts
                    .iter()
                    .find(|&&s| s >= lo && s < lo + c.len())
                    .map_or(0xFFFF, |&s| {
                        u16::try_from(s - lo).expect("offset within a slot")
                    });
                (c.to_vec(), head)
            })
            .collect()
    }

    #[test]
    fn reassembles_packets_across_slots() {
        let ps = vec![
            pkt(PacketType::Ipv6, 96),
            pkt(PacketType::Compressed, 1400),
            pkt(PacketType::Null, 10),
            pkt(PacketType::Control, 300),
        ];
        let mut stream = Vec::new();
        let mut starts = Vec::new();
        for p in &ps {
            starts.push(stream.len());
            stream.extend(p.encode().unwrap());
        }
        let mut a = Assembler::new();
        let mut out = Vec::new();
        for (d, h) in slots(&stream, &starts, 500) {
            a.push_slot(Some(&d), h, &mut out);
        }
        assert_eq!(out, ps);
        assert_eq!(a.stats.pointer_mismatches, 0);
    }

    #[test]
    fn a_lost_slot_drops_only_the_packets_it_touches() {
        let ps: Vec<Packet> = (0..20).map(|_| pkt(PacketType::Ipv4, 180)).collect();
        let mut stream = Vec::new();
        let mut starts = Vec::new();
        for p in &ps {
            starts.push(stream.len());
            stream.extend(p.encode().unwrap());
        }
        let sl = slots(&stream, &starts, 700);
        let mut a = Assembler::new();
        let mut out = Vec::new();
        for (i, (d, h)) in sl.iter().enumerate() {
            a.push_slot((i != 2).then_some(d.as_slice()), *h, &mut out);
        }
        // slot 2 covers bytes 1400..2100: packets starting before 2100 that
        // reach into it are lost; everything from slot 3's indication on
        // is delivered
        assert!(out.len() >= 20 - 6 && out.len() < 20);
        assert!(out.iter().all(|p| *p == ps[0]));
        assert_eq!(a.stats.lost_slots, 1);
    }

    #[test]
    fn file_reader_streams_and_resyncs() {
        let a = Packet {
            kind: PacketType::Ipv6,
            data: (0..=u8::MAX).cycle().take(3000).collect(),
        };
        let b = Packet {
            kind: PacketType::Null,
            data: vec![0xFF; 7],
        };
        // garbage before, between and a truncated packet at the end
        let mut bytes = vec![1, 2, 3];
        for _ in 0..800 {
            bytes.extend(a.encode().unwrap());
            bytes.extend(b.encode().unwrap());
        }
        bytes.extend([9, 9]);
        bytes.extend(b.encode().unwrap());
        bytes.extend(&a.encode().unwrap()[..10]);
        let mut r = FileReader::new(&bytes[..]);
        let mut n = 0;
        while let Some(p) = r.next_packet().unwrap() {
            assert!(p == a || p == b);
            n += 1;
        }
        assert_eq!(n, 1601);
        assert_eq!(r.skipped, 3 + 2 + 10);
    }
}
