//! Which ISDB-S3 services and events are unscrambled, by the broadcaster's
//! own signalling: the `free_CA_mode` bit of MH-SDT services and MH-EIT
//! events (ARIB STD-B60 v1.14 §7.3.3.9 Table 7-18, and MH-SDT: "all
//! component streams … are not scrambled when it is '0'. When it is '1',
//! … access to more than or equal to one stream is controlled by CA
//! system"). Reads `isdb rx --system isdbs3` TLV stream files, streaming
//! them (only the current packet is held), follows TLV → IP → MMTP →
//! signalling messages → M2 section messages, keeps the CRC-valid MH-SDT
//! and MH-EIT sections and prints, per service id, its MH-SDT
//! `free_CA_mode` and how many of its MH-EIT events have each value.
//!
//! It also measures each MMTP packet id's media payload: byte entropy, and
//! the first byte of the MMTP scramble extension (B60 Table 6-9 type
//! 0001h, laid out in ARIB STD-B61, which is not local). Entropy is the
//! check that needs no B61: on 2026-10-26 the BS7/11/17 media read
//! 7.98–8.000 bits/byte, like a known-scrambled ISDB-S PID (8.000), while
//! clear 8K AV1 video reads 6.65 — the media is encrypted although every
//! MH-SDT service and MH-EIT event there signals `free_CA_mode` 0 (with
//! `running_status` 0, "undefined"): the broadcasters leave the field at 0,
//! so the signalling alone does not say whether a service is clear.
//!
//! cargo run --release -p isdb-tlv --example freeca -- FILE.tlv [FILE.tlv …]

use isdb_tlv::ip::{compressed, ipv4, ipv6};
use isdb_tlv::mmtp;
use isdb_tlv::tlv::PacketType;
use isdb_ts::crc::crc32;
use std::collections::BTreeMap;
use std::io::{BufReader, Read};

/// MMTP payload type of signalling messages (B60 Table 6-5).
const SIGNALLING: u8 = 0x02;
/// MMTP payload type of MPUs (media; B60 Table 6-5).
const MPU: u8 = 0x00;

/// What the media payloads of one MMTP packet id look like, measured
/// without decrypting anything: byte entropy (encrypted data is ≈ 8 bits
/// per byte), and the scramble extension's first byte (its meaning is in
/// ARIB STD-B61, not local, so only whether it varies is used).
#[derive(Default)]
struct Media {
    packets: u64,
    bytes: u64,
    hist: Vec<u64>,
    /// first byte of the scramble extension (type 0001h) → packets
    ext0: BTreeMap<u8, u64>,
    no_ext: u64,
}

impl Media {
    fn add(&mut self, payload: &[u8], ext: Option<u8>) {
        self.packets += 1;
        if self.hist.is_empty() {
            self.hist = vec![0; 256];
        }
        // skip the MPU payload header (fragment type etc., 8 bytes) so
        // only coded media bytes are counted
        for &b in payload.get(8..).unwrap_or(&[]) {
            self.hist[usize::from(b)] += 1;
            self.bytes += 1;
        }
        match ext {
            Some(e) => *self.ext0.entry(e).or_default() += 1,
            None => self.no_ext += 1,
        }
    }

    /// Shannon entropy in bits per byte.
    fn entropy(&self) -> f64 {
        let n = self.bytes as f64;
        self.hist
            .iter()
            .filter(|&&c| c > 0)
            .map(|&c| {
                let p = c as f64 / n;
                -p * p.log2()
            })
            .sum()
    }
}

/// The first byte of an MMTP packet's multi-type header extension of
/// type 0001h (scramble information), read at the positions
/// `mmtp::parse` uses.
fn scramble_ext(b: &[u8]) -> Option<u8> {
    if b.len() < 12 || b[0] & 0x02 == 0 {
        return None;
    }
    let pos = 12 + if b[0] & 0x20 != 0 { 4 } else { 0 };
    let h = b.get(pos..pos + 4)?;
    if u16::from_be_bytes([h[0], h[1]]) != 0x0000 {
        return None;
    }
    let len = usize::from(u16::from_be_bytes([h[2], h[3]]));
    let body = b.get(pos + 4..pos + 4 + len)?;
    let mut q = 0;
    while let Some(e) = body.get(q..q + 4) {
        let t = u16::from_be_bytes([e[0], e[1]]) & 0x7FFF;
        let l = usize::from(u16::from_be_bytes([e[2], e[3]]));
        if t == 0x0001 {
            return body.get(q + 4).copied();
        }
        q += 4 + l;
        if e[0] & 0x80 != 0 {
            break;
        }
    }
    None
}
/// M2 section message id (B60 Table 4-6).
const M2_SECTION: u16 = 0x8000;

#[derive(Default)]
struct Service {
    /// MH-SDT free_CA_mode values seen (normally one).
    sdt: BTreeMap<bool, u64>,
    /// MH-EIT events by free_CA_mode: event_id → value.
    events: BTreeMap<u16, bool>,
}

fn main() {
    let files: Vec<String> = std::env::args().skip(1).collect();
    assert!(!files.is_empty(), "usage: freeca FILE.tlv [FILE.tlv …]");
    let mut services: BTreeMap<(u16, u16), Service> = BTreeMap::new();
    let mut media: BTreeMap<u16, Media> = BTreeMap::new();
    let (mut sections, mut bad_crc) = (0u64, 0u64);
    for f in &files {
        let mut r = BufReader::with_capacity(1 << 20, std::fs::File::open(f).expect("open"));
        let mut head = [0u8; 4];
        while r.read_exact(&mut head).is_ok() {
            if head[0] != 0x7F {
                eprintln!("{f}: lost TLV sync; stopping this file");
                break;
            }
            let len = usize::from(u16::from_be_bytes([head[2], head[3]]));
            let mut data = vec![0u8; len];
            if r.read_exact(&mut data).is_err() {
                break;
            }
            let dg = match PacketType::from_byte(head[1]) {
                PacketType::Ipv4 => ipv4(&data),
                PacketType::Ipv6 => ipv6(&data),
                PacketType::Compressed => compressed(&data),
                _ => continue,
            };
            let Ok(dg) = dg else { continue };
            let Some((h, payload)) = mmtp::parse(&dg.payload) else {
                continue;
            };
            if h.payload_type == MPU {
                let m = media.entry(h.packet_id).or_default();
                m.add(payload, scramble_ext(&dg.payload));
                continue;
            }
            if h.payload_type != SIGNALLING {
                continue;
            }
            for msg in undivided_messages(payload) {
                if msg.len() < 5 || u16::from_be_bytes([msg[0], msg[1]]) != M2_SECTION {
                    continue;
                }
                // id (16), version (8), length (16), then the section
                let sec = &msg[5..];
                let Some(sec) = whole_section(sec) else {
                    continue;
                };
                sections += 1;
                if crc32(sec) != 0 {
                    bad_crc += 1;
                    continue;
                }
                match sec[0] {
                    0x9F | 0xA0 => sdt(sec, &mut services),
                    0x8B..=0x9B => eit(sec, &mut services),
                    _ => {}
                }
            }
        }
    }
    println!("media payloads per MMTP packet id (entropy ≈ 8.0 = random, as encrypted data is):");
    println!("  packet_id   packets   MB     entropy  scramble-ext first byte → packets");
    for (pid, m) in &media {
        let ext: Vec<String> = m
            .ext0
            .iter()
            .map(|(b, n)| format!("{b:#04x}×{n}"))
            .collect();
        println!(
            "  {pid:#06x}   {:8}  {:6.2}  {:6.3}   {}{}",
            m.packets,
            m.bytes as f64 / 1e6,
            m.entropy(),
            ext.join(" "),
            if m.no_ext > 0 {
                format!(" none×{}", m.no_ext)
            } else {
                String::new()
            }
        );
    }
    println!(
        "{sections} MH-SDT/MH-EIT and other M2 sections read, {bad_crc} with a bad CRC (skipped)"
    );
    println!("  tlv_stream  service   MH-SDT free_CA_mode   MH-EIT events: free (0) / CA (1)");
    for ((ts, sid), s) in &services {
        let sdt: Vec<String> = s
            .sdt
            .iter()
            .map(|(v, n)| format!("{}×{n}", u8::from(*v)))
            .collect();
        let free = s.events.values().filter(|v| !**v).count();
        let ca = s.events.values().filter(|v| **v).count();
        println!(
            "  {ts:#06x}      {sid:#06x}    {:<20}  {free} / {ca}",
            if sdt.is_empty() {
                "-".to_string()
            } else {
                sdt.join(" ")
            }
        );
    }
}

/// The messages of an undivided signalling payload (B60 Table 6-1:
/// fragmentation indicator 0), aggregated or not.
fn undivided_messages(p: &[u8]) -> Vec<&[u8]> {
    let mut out = Vec::new();
    if p.len() < 2 || p[0] >> 6 != 0 {
        return out;
    }
    let long_len = p[0] & 0x02 != 0;
    let aggregated = p[0] & 0x01 != 0;
    let mut rest = &p[2..];
    if !aggregated {
        out.push(rest);
        return out;
    }
    while !rest.is_empty() {
        let (n, l) = if long_len {
            let Some(h) = rest.get(..4) else { break };
            (u32::from_be_bytes([h[0], h[1], h[2], h[3]]) as usize, 4)
        } else {
            let Some(h) = rest.get(..2) else { break };
            (usize::from(u16::from_be_bytes([h[0], h[1]])), 2)
        };
        let Some(m) = rest.get(l..l + n) else { break };
        out.push(m);
        rest = &rest[l + n..];
    }
    out
}

/// The long-form section at the start of `b`, CRC included.
fn whole_section(b: &[u8]) -> Option<&[u8]> {
    if b.len() < 3 || b[1] & 0x80 == 0 {
        return None;
    }
    let len = (usize::from(b[1] & 0x0F) << 8) | usize::from(b[2]);
    b.get(..3 + len)
}

/// MH-SDT: header (table_id … last_section_number: 8 bytes), then
/// original_network_id (16), reserved_future_use (8), then per service
/// service_id (16), reserved (3) EIT_user_defined_flags (3)
/// EIT_schedule_flag (1) EIT_present_following_flag (1), running_status
/// (3) free_CA_mode (1) descriptors_loop_length (12), descriptors; the
/// TLV stream id is the table id extension (B60 MH-SDT, like ARIB B10 SDT).
fn sdt(sec: &[u8], services: &mut BTreeMap<(u16, u16), Service>) {
    let ts = u16::from_be_bytes([sec[3], sec[4]]);
    let end = sec.len() - 4;
    let mut i = 11;
    while i + 5 <= end {
        let sid = u16::from_be_bytes([sec[i], sec[i + 1]]);
        let free_ca = sec[i + 3] & 0x10 != 0;
        let dl = (usize::from(sec[i + 3] & 0x0F) << 8) | usize::from(sec[i + 4]);
        *services
            .entry((ts, sid))
            .or_default()
            .sdt
            .entry(free_ca)
            .or_default() += 1;
        i += 5 + dl;
    }
}

/// MH-EIT (Table 7-18): header to last_table_id (14 bytes after the
/// 3-byte table_id/section_length), service_id is the table id extension,
/// tlv_stream_id at bytes 8–9; then per event event_id (16) start_time
/// (40) duration (24) running_status (3) free_CA_mode (1)
/// descriptors_loop_length (12), descriptors.
fn eit(sec: &[u8], services: &mut BTreeMap<(u16, u16), Service>) {
    let sid = u16::from_be_bytes([sec[3], sec[4]]);
    let ts = u16::from_be_bytes([sec[8], sec[9]]);
    let end = sec.len() - 4;
    let mut i = 14;
    while i + 12 <= end {
        let event = u16::from_be_bytes([sec[i], sec[i + 1]]);
        let free_ca = sec[i + 10] & 0x10 != 0;
        let dl = (usize::from(sec[i + 10] & 0x0F) << 8) | usize::from(sec[i + 11]);
        services
            .entry((ts, sid))
            .or_default()
            .events
            .insert(event, free_ca);
        i += 12 + dl;
    }
}
