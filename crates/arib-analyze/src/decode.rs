//! Section → typed table, using only the `arib-ts` (H.222.0 §2.4.4 PSI)
//! and `arib-si` (B10 Part 2 §5.2) decoders.

use crate::tree::{self, Node};
use arib_si::AnyTable;
use arib_ts::psi::{Cat, Pat, Pmt};
use arib_ts::section::Long;

/// A decoded section.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Table {
    /// program_association_section (H.222.0 Table 2-30).
    Pat(Long<Pat>),
    /// CA_section (H.222.0 Table 2-32).
    Cat(Long<Cat>),
    /// TS_program_map_section (H.222.0 Table 2-33).
    Pmt(Long<Pmt>),
    /// SI table (B10 Part 2 §5.2) or an untyped section.
    Si(AnyTable),
}

/// Decode a complete section carried on `pid`.  PSI table_ids are decoded
/// as PSI whatever the PID (placement is a validator concern); an SI
/// table_id on a PID that B10 Table 5-1 does not assign to it is decoded
/// as if it were on its assigned PID, so it can still be shown and
/// compared.
pub fn decode(pid: u16, sec: &[u8]) -> arib_ts::Result<Table> {
    match sec.first() {
        Some(0x00) => Ok(Table::Pat(Long::decode(sec)?)),
        Some(0x01) => Ok(Table::Cat(Long::decode(sec)?)),
        Some(0x02) => Ok(Table::Pmt(Long::decode(sec)?)),
        Some(&tid) => {
            let t = AnyTable::decode(pid, sec)?;
            if let AnyTable::Unknown(_) = &t
                && let Some(p) = crate::names::fixed_pids(tid)
                && !arib_si::any::table_allowed_on_pid(tid, pid)
                && arib_si::any::table_allowed_on_pid(tid, p[0])
            {
                return Ok(Table::Si(AnyTable::decode(p[0], sec)?));
            }
            Ok(Table::Si(t))
        }
        None => Err(arib_ts::Error::Truncated {
            what: "section",
            need: 1,
            have: 0,
        }),
    }
}

impl Table {
    /// Short name ("PAT", "NIT", …).
    pub fn name(&self) -> &'static str {
        match self {
            Table::Pat(_) => "PAT",
            Table::Cat(_) => "CAT",
            Table::Pmt(_) => "PMT",
            Table::Si(AnyTable::Unknown(r)) => crate::names::table_name(r.table_id),
            Table::Si(t) => t.name(),
        }
    }

    /// Normalised field tree (see [`crate::tree`]).
    pub fn tree(&self) -> Node {
        match self {
            Table::Pat(t) => tree::of(t),
            Table::Cat(t) => tree::of(t),
            Table::Pmt(t) => tree::of(t),
            Table::Si(t) => match t {
                AnyTable::Nit(x) => tree::of(x),
                AnyTable::Bat(x) => tree::of(x),
                AnyTable::Sdt(x) => tree::of(x),
                AnyTable::Eit(x) => tree::of(x),
                AnyTable::Tdt(x) => tree::of(x),
                AnyTable::Tot(x) => tree::of(x),
                AnyTable::Rst(x) => tree::of(x),
                AnyTable::St(x) => tree::of(x),
                AnyTable::Pcat(x) => tree::of(x),
                AnyTable::Bit(x) => tree::of(x),
                AnyTable::Nbit(x) => tree::of(x),
                AnyTable::Ldt(x) => tree::of(x),
                AnyTable::Unknown(r) => Node::Struct(
                    "RawSection".into(),
                    vec![
                        ("table_id".into(), Node::Int(r.table_id as i128)),
                        (
                            "long_header".into(),
                            r.long_header.as_ref().map(tree::of).unwrap_or(Node::Null),
                        ),
                        ("bytes".into(), Node::Bytes(r.bytes.clone())),
                    ],
                ),
            },
        }
    }

    /// Re-encode (lengths and CRC_32 recomputed).
    pub fn encode(&self) -> arib_ts::Result<Vec<u8>> {
        match self {
            Table::Pat(t) => t.encode(),
            Table::Cat(t) => t.encode(),
            Table::Pmt(t) => t.encode(),
            Table::Si(t) => t.encode(),
        }
    }
}
