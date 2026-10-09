//! Section dispatcher: PID + table_id → typed SI table (ARIB STD-B10 Part 2
//! §5.1.3, Tables 5-1 and 5-2).

use crate::tables::*;
use isdb_ts::section::{LongHeader, split_long, total_len};
use isdb_ts::{Error, Result};

/// Whether ARIB STD-B10 Part 2 Table 5-1 allows `table_id` (Table 5-2) on
/// `pid` for the tables this crate types:
///
/// | table | table_id | PID |
/// |---|---|---|
/// | NIT | 0x40, 0x41 | 0x0010 |
/// | SDT, BAT | 0x42, 0x46, 0x4A | 0x0011 |
/// | EIT | 0x4E–0x6F | 0x0012 (terrestrial also 0x0026, 0x0027) |
/// | RST | 0x71 | 0x0013 |
/// | TDT, TOT | 0x70, 0x73 | 0x0014 |
/// | ST | 0x72 | any except 0x0000, 0x0001, 0x0014 |
/// | PCAT | 0xC2 | 0x0022 |
/// | BIT | 0xC4 | 0x0024 |
/// | NBIT, LDT | 0xC5, 0xC6, 0xC7 | 0x0025 |
pub fn table_allowed_on_pid(table_id: u8, pid: u16) -> bool {
    match table_id {
        0x40 | 0x41 => pid == 0x0010,
        0x42 | 0x46 | 0x4A => pid == 0x0011,
        0x4E..=0x6F => matches!(pid, 0x0012 | 0x0026 | 0x0027),
        0x71 => pid == 0x0013,
        0x70 | 0x73 => pid == 0x0014,
        0x72 => !matches!(pid, 0x0000 | 0x0001 | 0x0014),
        0xC2 => pid == 0x0022,
        0xC4 => pid == 0x0024,
        0xC5..=0xC7 => pid == 0x0025,
        _ => false,
    }
}

/// A section whose (PID, table_id) has no typed decoder in this crate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawSection {
    /// PID it was carried on.
    pub pid: u16,
    /// table_id.
    pub table_id: u8,
    /// Parsed header when section_syntax_indicator = 1 (CRC_32 verified).
    pub long_header: Option<LongHeader>,
    /// The complete section bytes.
    pub bytes: Vec<u8>,
}

/// Any SI section (ARIB STD-B10 Part 2 §5.2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AnyTable {
    /// NIT, table_id 0x40/0x41 (§5.2.4).
    Nit(Nit),
    /// BAT, table_id 0x4A (§5.2.5).
    Bat(Bat),
    /// SDT, table_id 0x42/0x46 (§5.2.6).
    Sdt(Sdt),
    /// EIT, table_id 0x4E–0x6F (§5.2.7).
    Eit(Eit),
    /// TDT, table_id 0x70 (§5.2.8).
    Tdt(Tdt),
    /// TOT, table_id 0x73 (§5.2.9).
    Tot(Tot),
    /// RST, table_id 0x71 (§5.2.10).
    Rst(Rst),
    /// ST, table_id 0x72 (§5.2.11).
    St(StuffingSection),
    /// PCAT, table_id 0xC2 (§5.2.12).
    Pcat(Pcat),
    /// BIT, table_id 0xC4 (§5.2.13).
    Bit(Bit),
    /// NBIT, table_id 0xC5/0xC6 (§5.2.14).
    Nbit(Nbit),
    /// LDT, table_id 0xC7 (§5.2.15).
    Ldt(Ldt),
    /// Anything else (other PSI, SDTT, CDT, company tables, or an SI
    /// table_id on a PID Table 5-1 does not assign to it).
    Unknown(RawSection),
}

impl AnyTable {
    /// Decode one complete section carried on `pid`.  Typed decoding is
    /// used when [`table_allowed_on_pid`]; otherwise the section is
    /// returned as [`AnyTable::Unknown`] (with CRC_32 verified when
    /// section_syntax_indicator = 1).  Length, CRC and syntax errors are
    /// errors, never silently downgraded.
    pub fn decode(pid: u16, sec: &[u8]) -> Result<Self> {
        let table_id = *sec.first().ok_or(Error::Truncated {
            what: "section",
            need: 1,
            have: 0,
        })?;
        if table_id == 0xFF {
            return Err(Error::invalid(
                "table_id",
                "0xFF is stuffing, not a table (B10 Part 2 §5.1.2)",
            ));
        }
        let total = total_len(sec).ok_or(Error::Truncated {
            what: "section header",
            need: 3,
            have: sec.len(),
        })?;
        if total != sec.len() {
            return Err(Error::invalid(
                "section",
                format!("buffer is {} bytes, section_length says {total}", sec.len()),
            ));
        }
        if !table_allowed_on_pid(table_id, pid) {
            let long_header = if sec[1] & 0x80 != 0 {
                Some(split_long(sec)?.0)
            } else {
                None
            };
            return Ok(AnyTable::Unknown(RawSection {
                pid,
                table_id,
                long_header,
                bytes: sec.to_vec(),
            }));
        }
        Ok(match table_id {
            0x40 | 0x41 => AnyTable::Nit(Nit::decode(sec)?),
            0x4A => AnyTable::Bat(Bat::decode(sec)?),
            0x42 | 0x46 => AnyTable::Sdt(Sdt::decode(sec)?),
            0x4E..=0x6F => AnyTable::Eit(Eit::decode(sec)?),
            0x70 => AnyTable::Tdt(Tdt::decode(sec)?),
            0x73 => AnyTable::Tot(Tot::decode(sec)?),
            0x71 => AnyTable::Rst(Rst::decode(sec)?),
            0x72 => AnyTable::St(StuffingSection::decode(sec)?),
            0xC2 => AnyTable::Pcat(Pcat::decode(sec)?),
            0xC4 => AnyTable::Bit(Bit::decode(sec)?),
            0xC5 | 0xC6 => AnyTable::Nbit(Nbit::decode(sec)?),
            0xC7 => AnyTable::Ldt(Ldt::decode(sec)?),
            _ => unreachable!("table_allowed_on_pid covers only typed table_ids"),
        })
    }

    /// Encode back to complete section bytes (lengths and CRC_32
    /// recomputed; [`AnyTable::Unknown`] returns its bytes unchanged).
    pub fn encode(&self) -> Result<Vec<u8>> {
        match self {
            AnyTable::Nit(t) => t.encode(),
            AnyTable::Bat(t) => t.encode(),
            AnyTable::Sdt(t) => t.encode(),
            AnyTable::Eit(t) => t.encode(),
            AnyTable::Tdt(t) => t.encode(),
            AnyTable::Tot(t) => t.encode(),
            AnyTable::Rst(t) => t.encode(),
            AnyTable::St(t) => t.encode(),
            AnyTable::Pcat(t) => t.encode(),
            AnyTable::Bit(t) => t.encode(),
            AnyTable::Nbit(t) => t.encode(),
            AnyTable::Ldt(t) => t.encode(),
            AnyTable::Unknown(r) => Ok(r.bytes.clone()),
        }
    }

    /// table_id of the section.
    pub fn table_id(&self) -> u8 {
        match self {
            AnyTable::Nit(t) => t.header.table_id,
            AnyTable::Bat(t) => t.header.table_id,
            AnyTable::Sdt(t) => t.header.table_id,
            AnyTable::Eit(t) => t.header.table_id,
            AnyTable::Tdt(t) => t.header.table_id,
            AnyTable::Tot(t) => t.header.table_id,
            AnyTable::Rst(t) => t.header.table_id,
            AnyTable::St(t) => t.table_id,
            AnyTable::Pcat(t) => t.header.table_id,
            AnyTable::Bit(t) => t.header.table_id,
            AnyTable::Nbit(t) => t.header.table_id,
            AnyTable::Ldt(t) => t.header.table_id,
            AnyTable::Unknown(r) => r.table_id,
        }
    }

    /// Short table name ("NIT", "EIT", …, "unknown").
    pub fn name(&self) -> &'static str {
        match self {
            AnyTable::Nit(_) => "NIT",
            AnyTable::Bat(_) => "BAT",
            AnyTable::Sdt(_) => "SDT",
            AnyTable::Eit(_) => "EIT",
            AnyTable::Tdt(_) => "TDT",
            AnyTable::Tot(_) => "TOT",
            AnyTable::Rst(_) => "RST",
            AnyTable::St(_) => "ST",
            AnyTable::Pcat(_) => "PCAT",
            AnyTable::Bit(_) => "BIT",
            AnyTable::Nbit(_) => "NBIT",
            AnyTable::Ldt(_) => "LDT",
            AnyTable::Unknown(_) => "unknown",
        }
    }

    /// Every descriptor of the section, in order, with the name of the
    /// loop it sits in.
    pub fn descriptors(&self) -> Vec<(&'static str, &crate::descriptor::Descriptor)> {
        let mut v = Vec::new();
        match self {
            AnyTable::Nit(t) => {
                v.extend(t.body.0.descriptors.iter().map(|d| ("network", d)));
                for ts in &t.body.0.transport_streams {
                    v.extend(ts.descriptors.iter().map(|d| ("transport_stream", d)));
                }
            }
            AnyTable::Bat(t) => {
                v.extend(t.body.0.descriptors.iter().map(|d| ("bouquet", d)));
                for ts in &t.body.0.transport_streams {
                    v.extend(ts.descriptors.iter().map(|d| ("transport_stream", d)));
                }
            }
            AnyTable::Sdt(t) => {
                for s in &t.body.services {
                    v.extend(s.descriptors.iter().map(|d| ("service", d)));
                }
            }
            AnyTable::Eit(t) => {
                for e in &t.body.events {
                    v.extend(e.descriptors.iter().map(|d| ("event", d)));
                }
            }
            AnyTable::Tot(t) => v.extend(t.body.descriptors.iter().map(|d| ("TOT", d))),
            AnyTable::Pcat(t) => {
                for c in &t.body.content_versions {
                    v.extend(c.descriptors.iter().map(|d| ("content_version", d)));
                }
            }
            AnyTable::Bit(t) => {
                v.extend(t.body.first_descriptors.iter().map(|d| ("first", d)));
                for b in &t.body.broadcasters {
                    v.extend(b.descriptors.iter().map(|d| ("broadcaster", d)));
                }
            }
            AnyTable::Nbit(t) => {
                for i in &t.body.informations {
                    v.extend(i.descriptors.iter().map(|d| ("information", d)));
                }
            }
            AnyTable::Ldt(t) => {
                for d in &t.body.descriptions {
                    v.extend(d.descriptors.iter().map(|x| ("description", x)));
                }
            }
            AnyTable::Tdt(_) | AnyTable::Rst(_) | AnyTable::St(_) | AnyTable::Unknown(_) => {}
        }
        v
    }
}
