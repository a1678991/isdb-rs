//! Per-stream counters over TLV packets: packets and bytes by type, UDP
//! flows, MMTP packet ids and payload types, MMT-SI messages and tables,
//! TLV-SI sections, and malformed packets.

use crate::ip::{Datagram, compressed, ipv4, ipv6};
use crate::mmtp;
use crate::si;
use crate::tlv::{Packet, PacketType};
use std::collections::BTreeMap;

/// Counters of one TLV stream.
#[derive(Debug, Clone, Default)]
pub struct StreamStats {
    /// Packets and bytes per TLV packet type.
    pub by_type: BTreeMap<String, (u64, u64)>,
    /// IP packets whose content could not be read.
    pub ip_errors: u64,
    /// MMTP packets per packet id: (count, payload type of the last one).
    pub mmtp: BTreeMap<u16, (u64, u8)>,
    /// UDP payloads that do not parse as MMTP.
    pub non_mmtp: u64,
    /// MMT-SI messages seen (message id → count).
    pub messages: BTreeMap<u16, u64>,
    /// MMT-SI tables seen (table id → count).
    pub mmt_tables: BTreeMap<u8, u64>,
    /// TLV-SI sections seen (table id → count) and the offset each was
    /// found at within its control packet (offset → count).
    pub tlv_si: BTreeMap<u8, u64>,
    /// Offsets of TLV-SI sections inside control packets.
    pub si_offsets: BTreeMap<usize, u64>,
    /// TLV-NIT stream entries (TLV stream id, original network id).
    pub nit_streams: BTreeMap<(u16, u16), u64>,
    /// Network ids of TLV-NIT sections.
    pub nit_networks: BTreeMap<u16, u64>,
    /// HCfB contexts seen.
    pub contexts: BTreeMap<u16, u64>,
    /// MPU payloads (payload type 0) whose MMTP header carries the
    /// scramble-information extension (B60 Table 6-9 type 0001h, content
    /// in STD-B61).
    pub mpu_with_extension: u64,
    /// MPU payloads.
    pub mpu: u64,
}

impl StreamStats {
    /// Count one TLV packet.
    pub fn add(&mut self, p: &Packet) {
        let e = self.by_type.entry(p.kind.name()).or_default();
        e.0 += 1;
        e.1 += p.data.len() as u64 + 4;
        let dg: Option<Result<Datagram, _>> = match p.kind {
            PacketType::Ipv4 => Some(ipv4(&p.data)),
            PacketType::Ipv6 => Some(ipv6(&p.data)),
            PacketType::Compressed => Some(compressed(&p.data)),
            PacketType::Control => {
                for s in si::sections(&p.data) {
                    *self.tlv_si.entry(s.table_id).or_default() += 1;
                    *self.si_offsets.entry(s.offset).or_default() += 1;
                    if matches!(s.table_id, 0x40 | 0x41) {
                        *self.nit_networks.entry(s.extension).or_default() += 1;
                        for e in si::nit_streams(&s) {
                            *self.nit_streams.entry(e).or_default() += 1;
                        }
                    }
                }
                None
            }
            _ => None,
        };
        match dg {
            Some(Ok(d)) => self.add_datagram(&d),
            Some(Err(_)) => self.ip_errors += 1,
            None => {}
        }
    }

    fn add_datagram(&mut self, d: &Datagram) {
        if let Some(c) = d.context {
            *self.contexts.entry(c).or_default() += 1;
        }
        let Some((h, payload)) = mmtp::parse(&d.payload) else {
            self.non_mmtp += 1;
            return;
        };
        let e = self.mmtp.entry(h.packet_id).or_default();
        e.0 += 1;
        e.1 = h.payload_type;
        match h.payload_type {
            mmtp::MESSAGES => {
                if let Some(ms) = mmtp::messages(payload) {
                    for m in ms {
                        *self.messages.entry(m.id).or_default() += 1;
                        for t in m.tables {
                            *self.mmt_tables.entry(t).or_default() += 1;
                        }
                    }
                }
            }
            mmtp::MPU => {
                self.mpu += 1;
                if h.scramble_info {
                    self.mpu_with_extension += 1;
                }
            }
            _ => {}
        }
    }
}
