//! PSI tables of H.222.0 §2.4.4: Program Association Table (Table 2-30),
//! Conditional Access Table (Table 2-32) and TS Program Map Table
//! (Table 2-33), as [`LongBody`] implementations over [`crate::section`].
//!
//! Every reserved bit is kept, so `encode(decode(s)) == s` for every section
//! the decoders accept.  The table-specific envelope fields live in the
//! [`LongHeader`]: `table_id_extension` is transport_stream_id (PAT),
//! program_number (PMT) or the first 16 of the CAT's 18 reserved bits (the
//! other two are `reserved2`).  Value rules are reported by
//! [`check_pat`]/[`check_cat`]/[`check_pmt`].

use crate::bits::{BitReader, BitWriter};
use crate::descriptor::{self, Descriptor};
use crate::error::{Error, Result};
use crate::section::{Long, LongBody, LongHeader};
use crate::validate::{Violation, require, reserved_ones};

/// table_id of program_association_section (Table 2-31).
pub const TABLE_ID_PAT: u8 = 0x00;
/// table_id of CA_section (Table 2-31).
pub const TABLE_ID_CAT: u8 = 0x01;
/// table_id of TS_program_map_section (Table 2-31).
pub const TABLE_ID_PMT: u8 = 0x02;
/// table_id of TS_description_section (Table 2-31).
pub const TABLE_ID_TSDT: u8 = 0x03;
/// Maximum section_length of a PSI section ("shall not exceed 1021
/// (0x3FD)", §2.4.4.5/§2.4.4.7/§2.4.4.9).
pub const PSI_MAX_SECTION_LENGTH: usize = 1021;

/// One entry of the PAT program loop (Table 2-30).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PatEntry {
    /// program_number; 0x0000 means `pid` is the network_PID.
    pub program_number: u16,
    /// The 3 reserved bits (spec '111').
    pub reserved: u8,
    /// network_PID (program_number 0) or program_map_PID.
    pub pid: u16,
}

impl PatEntry {
    /// Entry with reserved bits '111'.
    pub fn new(program_number: u16, pid: u16) -> Self {
        Self {
            program_number,
            reserved: 0b111,
            pid,
        }
    }
}

/// Body of a program_association_section (H.222.0 §2.4.4.3, Table 2-30).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Pat {
    /// The program loop in transmission order.
    pub entries: Vec<PatEntry>,
}

impl Pat {
    /// network_PID (entry with program_number 0x0000), if present.
    pub fn network_pid(&self) -> Option<u16> {
        self.entries
            .iter()
            .find(|e| e.program_number == 0)
            .map(|e| e.pid)
    }

    /// `(program_number, program_map_PID)` of every program (number ≠ 0).
    pub fn programs(&self) -> impl Iterator<Item = (u16, u16)> + '_ {
        self.entries
            .iter()
            .filter(|e| e.program_number != 0)
            .map(|e| (e.program_number, e.pid))
    }

    /// A single-section PAT (`transport_stream_id` in table_id_extension).
    pub fn section(transport_stream_id: u16, version: u8, entries: Vec<PatEntry>) -> Long<Pat> {
        Long {
            header: LongHeader::new(TABLE_ID_PAT, transport_stream_id, version),
            body: Pat { entries },
        }
    }
}

impl LongBody for Pat {
    fn accepts_table_id(table_id: u8) -> bool {
        table_id == TABLE_ID_PAT
    }

    fn decode(_: &LongHeader, body: &[u8]) -> Result<Self> {
        if body.len() & 3 != 0 {
            return Err(Error::invalid(
                "program_association_section",
                format!("program loop is {} bytes, not a multiple of 4", body.len()),
            ));
        }
        let mut r = BitReader::new(body, "program_association_section");
        let mut entries = Vec::with_capacity(body.len() / 4);
        while r.remaining_bits() > 0 {
            entries.push(PatEntry {
                program_number: r.u16()?,
                reserved: r.read(3)? as u8,
                pid: r.read(13)? as u16,
            });
        }
        Ok(Self { entries })
    }

    fn encode(&self, out: &mut Vec<u8>) -> Result<()> {
        let mut w = BitWriter::new();
        for e in &self.entries {
            w.write("program_number", 16, u64::from(e.program_number))?;
            w.write("reserved", 3, u64::from(e.reserved))?;
            w.write("program_map_PID", 13, u64::from(e.pid))?;
        }
        out.extend_from_slice(&w.into_bytes());
        Ok(())
    }
}

/// Body of a CA_section (H.222.0 §2.4.4.6, Table 2-32): a descriptor loop.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Cat {
    /// descriptor() loop (CA_descriptors, §2.6.16).
    pub descriptors: Vec<Descriptor>,
}

impl Cat {
    /// A single-section CAT with the 18 reserved bits set to '1'.
    pub fn section(version: u8, descriptors: Vec<Descriptor>) -> Long<Cat> {
        Long {
            header: LongHeader::new(TABLE_ID_CAT, 0xFFFF, version),
            body: Cat { descriptors },
        }
    }
}

impl LongBody for Cat {
    fn accepts_table_id(table_id: u8) -> bool {
        table_id == TABLE_ID_CAT
    }

    fn decode(_: &LongHeader, body: &[u8]) -> Result<Self> {
        Ok(Self {
            descriptors: descriptor::parse_loop(body)?,
        })
    }

    fn encode(&self, out: &mut Vec<u8>) -> Result<()> {
        out.extend_from_slice(&descriptor::encode_loop(&self.descriptors)?);
        Ok(())
    }
}

/// One elementary stream entry of the PMT (Table 2-33).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PmtStream {
    /// stream_type (Table 2-34).
    pub stream_type: u8,
    /// The 3 reserved bits before elementary_PID (spec '111').
    pub reserved_pid: u8,
    /// elementary_PID.
    pub elementary_pid: u16,
    /// The 4 reserved bits before ES_info_length (spec '1111').
    pub reserved_info: u8,
    /// ES_info descriptors (ES_info_length implied).
    pub descriptors: Vec<Descriptor>,
}

impl PmtStream {
    /// Entry with reserved bits all '1'.
    pub fn new(stream_type: u8, elementary_pid: u16, descriptors: Vec<Descriptor>) -> Self {
        Self {
            stream_type,
            reserved_pid: 0b111,
            elementary_pid,
            reserved_info: 0b1111,
            descriptors,
        }
    }
}

/// Body of a TS_program_map_section (H.222.0 §2.4.4.8, Table 2-33).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pmt {
    /// The 3 reserved bits before PCR_PID (spec '111').
    pub reserved_pcr: u8,
    /// PCR_PID (0x1FFF: no PCR, §2.4.4.9).
    pub pcr_pid: u16,
    /// The 4 reserved bits before program_info_length (spec '1111').
    pub reserved_info: u8,
    /// Program descriptors (program_info_length implied).
    pub program_info: Vec<Descriptor>,
    /// Elementary stream loop.
    pub streams: Vec<PmtStream>,
}

impl Pmt {
    /// PMT body with reserved bits all '1'.
    pub fn new(pcr_pid: u16, program_info: Vec<Descriptor>, streams: Vec<PmtStream>) -> Self {
        Self {
            reserved_pcr: 0b111,
            pcr_pid,
            reserved_info: 0b1111,
            program_info,
            streams,
        }
    }

    /// A PMT section for `program_number` (in table_id_extension).
    pub fn section(program_number: u16, version: u8, body: Pmt) -> Long<Pmt> {
        Long {
            header: LongHeader::new(TABLE_ID_PMT, program_number, version),
            body,
        }
    }
}

impl LongBody for Pmt {
    fn accepts_table_id(table_id: u8) -> bool {
        table_id == TABLE_ID_PMT
    }

    fn decode(_: &LongHeader, body: &[u8]) -> Result<Self> {
        let mut r = BitReader::new(body, "TS_program_map_section");
        let reserved_pcr = r.read(3)? as u8;
        let pcr_pid = r.read(13)? as u16;
        let reserved_info = r.read(4)? as u8;
        let pil = r.read(12)? as usize;
        let program_info = descriptor::parse_loop(r.bytes(pil).map_err(|_| {
            Error::invalid(
                "program_info_length",
                format!("{pil} runs past the section"),
            )
        })?)?;
        let mut streams = Vec::new();
        while r.remaining_bits() > 0 {
            let stream_type = r.u8()?;
            let reserved_pid = r.read(3)? as u8;
            let elementary_pid = r.read(13)? as u16;
            let reserved_info = r.read(4)? as u8;
            let n = r.read(12)? as usize;
            let descriptors = descriptor::parse_loop(r.bytes(n).map_err(|_| {
                Error::invalid("ES_info_length", format!("{n} runs past the section"))
            })?)?;
            streams.push(PmtStream {
                stream_type,
                reserved_pid,
                elementary_pid,
                reserved_info,
                descriptors,
            });
        }
        Ok(Self {
            reserved_pcr,
            pcr_pid,
            reserved_info,
            program_info,
            streams,
        })
    }

    fn encode(&self, out: &mut Vec<u8>) -> Result<()> {
        let mut w = BitWriter::new();
        w.write("reserved", 3, u64::from(self.reserved_pcr))?;
        w.write("PCR_PID", 13, u64::from(self.pcr_pid))?;
        w.write("reserved", 4, u64::from(self.reserved_info))?;
        let pi = descriptor::encode_loop(&self.program_info)?;
        w.write("program_info_length", 12, pi.len() as u64)?;
        w.bytes(&pi);
        for s in &self.streams {
            w.write("stream_type", 8, u64::from(s.stream_type))?;
            w.write("reserved", 3, u64::from(s.reserved_pid))?;
            w.write("elementary_PID", 13, u64::from(s.elementary_pid))?;
            w.write("reserved", 4, u64::from(s.reserved_info))?;
            let es = descriptor::encode_loop(&s.descriptors)?;
            w.write("ES_info_length", 12, es.len() as u64)?;
            w.bytes(&es);
        }
        out.extend_from_slice(&w.into_bytes());
        Ok(())
    }
}

fn check_envelope<B: LongBody>(
    v: &mut Vec<Violation>,
    s: &Long<B>,
    table_id: u8,
    clause: &'static str,
) {
    let h = &s.header;
    require(v, h.table_id == table_id, clause, || {
        format!("table_id {:#04x}, shall be {table_id:#04x}", h.table_id)
    });
    require(v, !h.private_indicator, clause, || {
        "bit after section_syntax_indicator shall be '0'".into()
    });
    reserved_ones(
        v,
        u64::from(h.reserved1),
        2,
        "reserved before section_length",
    );
    match s.encode() {
        Ok(b) => {
            let sl = b.len() - 3;
            require(v, sl <= PSI_MAX_SECTION_LENGTH, clause, || {
                format!("section_length {sl} exceeds 1021")
            });
        }
        Err(e) => v.push(Violation::new(clause, format!("not encodable: {e}"))),
    }
}

/// Table 2-3: PIDs a program_map_PID/network_PID may take
/// (0x0010..=0x1FFE; 0x0000–0x000F are assigned/reserved, 0x1FFF is null).
fn assignable_pid(pid: u16) -> bool {
    (0x0010..=0x1FFE).contains(&pid)
}

/// Rules of §2.4.4.5 (Table 2-30) on a PAT section: table_id 0x00, '0' bit,
/// reserved bits '1' (§2.1.64), section_length ≤ 1021, program_number
/// unique, PIDs "only take values as specified in Table 2-3".
pub fn check_pat(s: &Long<Pat>) -> Vec<Violation> {
    let mut v = Vec::new();
    check_envelope(&mut v, s, TABLE_ID_PAT, "§2.4.4.5");
    reserved_ones(
        &mut v,
        u64::from(s.header.reserved2),
        2,
        "reserved before version_number",
    );
    let mut seen = std::collections::HashSet::new();
    for e in &s.body.entries {
        reserved_ones(&mut v, u64::from(e.reserved), 3, "program loop reserved");
        require(&mut v, seen.insert(e.program_number), "§2.4.4.5", || {
            format!(
                "program_number {:#06x} appears more than once",
                e.program_number
            )
        });
        require(&mut v, assignable_pid(e.pid), "§2.4.4.5 Table 2-3", || {
            format!(
                "program_number {:#06x} → PID {:#06x} not in 0x0010..0x1FFE",
                e.program_number, e.pid
            )
        });
    }
    v
}

/// Rules of §2.4.4.7 (Table 2-32) on a CAT section: table_id 0x01, '0' bit,
/// the 18 reserved bits and others '1', section_length ≤ 1021.
pub fn check_cat(s: &Long<Cat>) -> Vec<Violation> {
    let mut v = Vec::new();
    check_envelope(&mut v, s, TABLE_ID_CAT, "§2.4.4.7");
    let r18 = (u64::from(s.header.table_id_extension) << 2) | u64::from(s.header.reserved2);
    reserved_ones(&mut v, r18, 18, "CA_section 18 reserved bits");
    v
}

/// Rules of §2.4.4.9 (Table 2-33) on a PMT section: table_id 0x02, '0'
/// bit, reserved bits '1', section_length ≤ 1021, section_number and
/// last_section_number 0x00, program_info_length/ES_info_length first two
/// bits '00', PCR_PID per Table 2-3 NOTE (or 0x1FFF for none).
pub fn check_pmt(s: &Long<Pmt>) -> Vec<Violation> {
    let mut v = Vec::new();
    let c = "§2.4.4.9";
    check_envelope(&mut v, s, TABLE_ID_PMT, c);
    let h = &s.header;
    reserved_ones(
        &mut v,
        u64::from(h.reserved2),
        2,
        "reserved before version_number",
    );
    require(&mut v, h.section_number == 0, c, || {
        format!("section_number {}, shall be 0x00", h.section_number)
    });
    require(&mut v, h.last_section_number == 0, c, || {
        format!(
            "last_section_number {}, shall be 0x00",
            h.last_section_number
        )
    });
    let b = &s.body;
    reserved_ones(
        &mut v,
        u64::from(b.reserved_pcr),
        3,
        "reserved before PCR_PID",
    );
    reserved_ones(
        &mut v,
        u64::from(b.reserved_info),
        4,
        "reserved before program_info_length",
    );
    require(
        &mut v,
        matches!(b.pcr_pid, 0x0000 | 0x0001 | 0x0010..=0x1FFF),
        "§2.4.4.9 Table 2-3",
        || format!("PCR_PID {:#06x} may not carry a PCR", b.pcr_pid),
    );
    let pil = descriptor::loop_len(&b.program_info);
    require(&mut v, pil < 1024, c, || {
        format!("program_info_length {pil}: first two bits shall be '00'")
    });
    for st in &b.streams {
        reserved_ones(
            &mut v,
            u64::from(st.reserved_pid),
            3,
            "reserved before elementary_PID",
        );
        reserved_ones(
            &mut v,
            u64::from(st.reserved_info),
            4,
            "reserved before ES_info_length",
        );
        let n = descriptor::loop_len(&st.descriptors);
        require(&mut v, n < 1024, c, || {
            format!("ES_info_length {n}: first two bits shall be '00'")
        });
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Table 2-30: program_number(16) reserved(3) PID(13); program 0 →
    /// network_PID.
    #[test]
    fn spec_table2_30_pat_layout() {
        let s = Pat::section(
            0x79EE,
            3,
            vec![PatEntry::new(0, 0x0010), PatEntry::new(0x0428, 0x01F0)],
        );
        let b = s.encode().unwrap();
        // header: 00 B0 11 79 EE C7 00 00 ; body: 0000 E010 0428 E1F0 ; CRC
        assert_eq!(&b[..8], &[0x00, 0xB0, 0x11, 0x79, 0xEE, 0xC7, 0x00, 0x00]);
        assert_eq!(&b[8..16], &[0x00, 0x00, 0xE0, 0x10, 0x04, 0x28, 0xE1, 0xF0]);
        let d = Long::<Pat>::decode(&b).unwrap();
        assert_eq!(d, s);
        assert_eq!(d.body.network_pid(), Some(0x10));
        assert_eq!(
            d.body.programs().collect::<Vec<_>>(),
            vec![(0x0428, 0x01F0)]
        );
        assert!(check_pat(&d).is_empty());
        // reserved bits kept verbatim and flagged by the checker
        let mut s2 = s.clone();
        s2.body.entries[1].reserved = 0b010;
        let d2 = Long::<Pat>::decode(&s2.encode().unwrap()).unwrap();
        assert_eq!(d2.body.entries[1].reserved, 0b010);
        assert!(check_pat(&d2).iter().any(|v| v.clause == "§2.1.64"));
        // body not a multiple of 4 bytes
        let bad = crate::section::build_long(&s.header, &[0, 1, 0xE0]).unwrap();
        assert!(Long::<Pat>::decode(&bad).is_err());
    }

    /// Table 2-32: 18 reserved bits between section_length and version_number.
    #[test]
    fn spec_table2_32_cat_reserved18() {
        let s = Cat::section(1, vec![Descriptor::new(0x09, [0x00, 0x05, 0xE1, 0x00])]);
        let b = s.encode().unwrap();
        assert_eq!(&b[3..6], &[0xFF, 0xFF, 0xC3]); // 18 ones, version 1, cni 1
        let d = Long::<Cat>::decode(&b).unwrap();
        assert_eq!(d.encode().unwrap(), b);
        assert!(check_cat(&d).is_empty());
    }

    /// Table 2-33: reserved '111' PCR_PID, '1111' program_info_length, ES loop.
    #[test]
    fn spec_table2_33_pmt_layout() {
        let body = Pmt::new(
            0x01FF,
            vec![Descriptor::new(0xC1, [0x84])],
            vec![
                PmtStream::new(0x02, 0x0111, vec![]),
                PmtStream::new(0x0F, 0x0112, vec![Descriptor::new(0x52, [0x10])]),
            ],
        );
        let s = Pmt::section(0x0428, 0, body);
        let b = s.encode().unwrap();
        assert_eq!(&b[8..12], &[0xE1, 0xFF, 0xF0, 0x03]);
        assert_eq!(&b[12..15], &[0xC1, 0x01, 0x84]);
        assert_eq!(&b[15..20], &[0x02, 0xE1, 0x11, 0xF0, 0x00]);
        assert_eq!(
            &b[20..28],
            &[0x0F, 0xE1, 0x12, 0xF0, 0x03, 0x52, 0x01, 0x10]
        );
        let d = Long::<Pmt>::decode(&b).unwrap();
        assert_eq!(d, s);
        assert!(check_pmt(&d).is_empty(), "{:?}", check_pmt(&d));
        let mut s2 = s.clone();
        s2.header.section_number = 1;
        s2.header.last_section_number = 1;
        assert_eq!(
            check_pmt(&s2)
                .iter()
                .filter(|v| v.clause == "§2.4.4.9")
                .count(),
            2
        );
        // truncated ES loop
        let bad = crate::section::build_long(&s.header, &b[8..b.len() - 6]).unwrap();
        assert!(Long::<Pmt>::decode(&bad).is_err());
    }
}
