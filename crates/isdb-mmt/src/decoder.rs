//! A streaming decoder over the TLV packets of one TLV stream: TLV-SI
//! (TLV-NIT, AMT) from transmission-control packets, NTP time (UDP port
//! 123, ARIB STD-B60 v1.14 §3.2), MMT-SI tables from control messages,
//! and the MFUs of every asset, with the asset each belongs to.
//!
//! Tables are kept per (table id, extension, section number) at their
//! latest version, so a repeated table is decoded once ("repeat
//! suppression"); a changed version replaces it and is counted.

use crate::message::{Messages, TableBytes};
use crate::mpu::{Mfu, Reassembler};
use crate::table::{Amt, Bit, Eit, Mpt, Nit, Plt, Sdt, SectionError, Tot};
use isdb_tlv::ip::{Datagram, compressed, ipv4, ipv6};
use isdb_tlv::mmtp;
use isdb_tlv::tlv::{Packet, PacketType};
use std::collections::BTreeMap;

/// NTP's UDP port (§3.2).
pub const NTP_PORT: u16 = 123;

/// Key of a section-format table instance.
type SectionKey = (u8, u16, u8);

/// Counters of the decoder.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Counters {
    /// TLV packets.
    pub tlv_packets: u64,
    /// IP packets that do not decode.
    pub ip_errors: u64,
    /// UDP payloads that are neither NTP nor MMTP.
    pub non_mmtp: u64,
    /// NTP packets.
    pub ntp: u64,
    /// Tables received (all copies).
    pub tables: u64,
    /// Tables whose CRC failed.
    pub crc_errors: u64,
    /// Tables that do not follow their structure.
    pub malformed: u64,
    /// Version changes of a table already held.
    pub version_changes: u64,
    /// Control messages lost in reassembly (see `Messages`).
    pub message_errors: crate::message::MessageErrors,
}

/// The decoder's view of the stream.
#[derive(Debug, Default)]
pub struct Decoder {
    messages: Messages,
    mpu: Reassembler,
    last_ctrl_seq: BTreeMap<u16, u32>,
    /// MPTs per packet id of the PA message they came on (latest).
    pub mpts: BTreeMap<u16, Mpt>,
    /// The latest PLT.
    pub plt: Option<Plt>,
    /// MH-EIT sections by (table id, service id, section number).
    pub eits: BTreeMap<SectionKey, Eit>,
    /// MH-SDT sections by (table id, TLV stream id, section number).
    pub sdts: BTreeMap<SectionKey, Sdt>,
    /// MH-BIT sections.
    pub bits: BTreeMap<SectionKey, Bit>,
    /// TLV-NIT sections.
    pub nits: BTreeMap<SectionKey, Nit>,
    /// AMT sections.
    pub amts: BTreeMap<SectionKey, Amt>,
    /// The latest MH-TOT.
    pub tot: Option<Tot>,
    /// The latest NTP transmit timestamp (64-bit NTP format).
    pub ntp: Option<u64>,
    /// Other tables seen: table id → count (ECM, EMM, CAT, MH-AIT, …:
    /// recognised, contents not decoded).
    pub other_tables: BTreeMap<u8, u64>,
    /// Counters.
    pub counters: Counters,
}

impl Decoder {
    /// A decoder with nothing seen.
    pub fn new() -> Self {
        Self::default()
    }

    /// The MPU counters of every packet id.
    pub fn mpu_stats(&self) -> impl Iterator<Item = (u16, &crate::mpu::AssetStats)> {
        self.mpu.stats()
    }

    /// The asset (and the service id of its MPT) that `packet_id` carries,
    /// from the MPTs held.
    pub fn asset_of(&self, packet_id: u16) -> Option<(Option<u16>, &crate::table::Asset)> {
        self.mpts.values().find_map(|m| {
            m.assets
                .iter()
                .find(|a| a.packet_id() == Some(packet_id))
                .map(|a| (m.service_id(), a))
        })
    }

    /// Feed one TLV packet; whole MFUs go to `mfus`.
    pub fn push(&mut self, p: &Packet, mfus: &mut Vec<Mfu>) {
        self.counters.tlv_packets += 1;
        let dg = match p.kind {
            PacketType::Ipv4 => Some(ipv4(&p.data)),
            PacketType::Ipv6 => Some(ipv6(&p.data)),
            PacketType::Compressed => Some(compressed(&p.data)),
            PacketType::Control => {
                self.control(&p.data);
                None
            }
            _ => None,
        };
        match dg {
            Some(Ok(d)) => self.datagram(&d, mfus),
            Some(Err(_)) => self.counters.ip_errors += 1,
            None => {}
        }
    }

    fn control(&mut self, data: &[u8]) {
        for s in isdb_tlv::si::sections(data) {
            self.counters.tables += 1;
            let key = (s.table_id, s.extension, s.bytes[6]);
            match s.table_id {
                0x40 | 0x41 => keep(
                    &mut self.nits,
                    &mut self.counters,
                    key,
                    s.version,
                    || Nit::parse(&s.bytes),
                    |n| n.header.version,
                ),
                0xFE => keep(
                    &mut self.amts,
                    &mut self.counters,
                    key,
                    s.version,
                    || Amt::parse(&s.bytes),
                    |a| a.header.version,
                ),
                _ => {}
            }
        }
    }

    fn datagram(&mut self, d: &Datagram, mfus: &mut Vec<Mfu>) {
        if d.flow.as_ref().is_some_and(|f| f.dport == NTP_PORT) {
            self.counters.ntp += 1;
            // transmit_timestamp: the last 8 bytes of the 48-byte format
            // (Table 3-1)
            if let Some(t) = d.payload.get(40..48) {
                self.ntp = Some(u64::from_be_bytes(t.try_into().unwrap_or_default()));
            }
            return;
        }
        let Some((h, payload)) = mmtp::parse(&d.payload) else {
            self.counters.non_mmtp += 1;
            return;
        };
        match h.payload_type {
            mmtp::MESSAGES => {
                let gap = self
                    .last_ctrl_seq
                    .insert(h.packet_id, h.sequence)
                    .is_some_and(|l| h.sequence != l.wrapping_add(1));
                let mut tables = Vec::new();
                self.messages.push(h.packet_id, payload, gap, &mut tables);
                self.counters.message_errors = self.messages.errors;
                for t in tables {
                    self.table(t);
                }
            }
            mmtp::MPU => self.mpu.push(&h, payload, mfus),
            _ => {}
        }
    }

    fn table(&mut self, t: TableBytes) {
        self.counters.tables += 1;
        let b = &t.bytes;
        let Some(&id) = b.first() else { return };
        match id {
            0x11..=0x20 => match Mpt::parse(b) {
                Some(m) => {
                    if let Some(old) = self.mpts.get(&t.packet_id)
                        && old.version != m.version
                    {
                        self.counters.version_changes += 1;
                    }
                    self.mpts.insert(t.packet_id, m);
                }
                None => self.counters.malformed += 1,
            },
            0x80 => match Plt::parse(b) {
                Some(p) => {
                    if self.plt.as_ref().is_some_and(|o| o.version != p.version) {
                        self.counters.version_changes += 1;
                    }
                    self.plt = Some(p);
                }
                None => self.counters.malformed += 1,
            },
            0xA1 => match Tot::parse(b) {
                Ok(tot) => self.tot = Some(tot),
                Err(e) => self.error(e),
            },
            0x8B..=0x9B | 0x9D | 0x9F | 0xA0 => {
                // long sections: key from the header before decoding
                let (Some(ext), Some(v), Some(sn)) = (
                    b.get(3..5).map(|x| u16::from_be_bytes([x[0], x[1]])),
                    b.get(5).map(|x| (x >> 1) & 0x1F),
                    b.get(6),
                ) else {
                    self.counters.malformed += 1;
                    return;
                };
                let key = (id, ext, *sn);
                match id {
                    0x9D => keep(
                        &mut self.bits,
                        &mut self.counters,
                        key,
                        v,
                        || Bit::parse(b),
                        |x| x.header.version,
                    ),
                    0x9F | 0xA0 => keep(
                        &mut self.sdts,
                        &mut self.counters,
                        key,
                        v,
                        || Sdt::parse(b),
                        |x| x.header.version,
                    ),
                    _ => keep(
                        &mut self.eits,
                        &mut self.counters,
                        key,
                        v,
                        || Eit::parse(b),
                        |x| x.header.version,
                    ),
                }
            }
            _ => *self.other_tables.entry(id).or_default() += 1,
        }
    }

    fn error(&mut self, e: SectionError) {
        match e {
            SectionError::Crc => self.counters.crc_errors += 1,
            _ => self.counters.malformed += 1,
        }
    }
}

/// Keep a section-format table at its latest version, decoding it only
/// when it is new or its version changed.
fn keep<T>(
    map: &mut BTreeMap<SectionKey, T>,
    c: &mut Counters,
    key: SectionKey,
    version: u8,
    parse: impl FnOnce() -> Result<T, SectionError>,
    version_of: impl Fn(&T) -> u8,
) {
    if let Some(old) = map.get(&key) {
        if version_of(old) == version {
            return;
        }
        c.version_changes += 1;
    }
    match parse() {
        Ok(t) => {
            map.insert(key, t);
        }
        Err(SectionError::Crc) => c.crc_errors += 1,
        Err(_) => c.malformed += 1,
    }
}
