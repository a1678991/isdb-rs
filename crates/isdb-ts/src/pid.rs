//! Packet identifier (H.222.0 §2.4.3.3, Table 2-3): the 13-bit PID field
//! of the transport packet header, as a value type that cannot hold more
//! than 13 bits.
//!
//! Before this type existed the PID was a `u16` at every site and only
//! [`BitWriter::write`](crate::bits::BitWriter::write) refused values above
//! 0x1FFF, at encode time.  Now the range is checked once, where a raw
//! number enters ([`Pid::new`], [`TryFrom<u16>`], the serde
//! `Deserialize`), and decoders build it from the 13 wire bits directly
//! ([`Pid::from_wire`]).

use std::fmt;

use crate::error::Error;

/// A 13-bit PID (H.222.0 §2.4.3.3: "The PID is a 13-bit field, indicating
/// the type of the data stored in the packet payload").
///
/// Table 2-3 assigns 0x0000 (PAT), 0x0001 (CAT), 0x0002 (TSDT), 0x0003
/// (IPMP control information), reserves 0x0004–0x000F, leaves
/// 0x0010–0x1FFE free to be "assigned as network_PID, Program_map_PID,
/// elementary_PID, or for other purposes" and gives 0x1FFF to null packets.
/// Those assignments are the associated constants below and
/// [`Pid::is_reserved`].
///
/// Formatting matches how the analysers print PIDs: `{}` and `{:?}` give
/// `0x1fff` (`{:#06x}` of the value); `{:x}` / `{:X}` / `{:#06x}` format the
/// value as for a `u16`, so existing format strings keep working.
///
/// ```
/// use isdb_ts::Pid;
///
/// let pmt = Pid::new(0x01F0).unwrap();
/// assert_eq!(pmt.get(), 0x01F0);
/// assert!(Pid::new(0x2000).is_none());
/// assert_eq!(Pid::try_from(0x1FFFu16), Ok(Pid::NULL));
/// assert_eq!(format!("{pmt} {pmt:#06X}"), "0x01f0 0x01F0");
/// ```
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Pid(u16);

impl Pid {
    /// Program Association Table (Table 2-3, 0x0000).
    pub const PAT: Pid = Pid(0x0000);
    /// Conditional Access Table (Table 2-3, 0x0001).
    pub const CAT: Pid = Pid(0x0001);
    /// Transport Stream Description Table (Table 2-3, 0x0002).
    pub const TSDT: Pid = Pid(0x0002);
    /// IPMP Control Information Table (Table 2-3, 0x0003; ISO/IEC 13818-11).
    pub const IPMP: Pid = Pid(0x0003);
    /// Null packet (Table 2-3, 0x1FFF).  Also the PCR_PID value meaning "no
    /// PCR" (§2.4.4.9).
    pub const NULL: Pid = Pid(0x1FFF);
    /// Largest PID (13 bits, Table 2-2); the same value as [`Pid::NULL`].
    pub const MAX: Pid = Pid(0x1FFF);

    /// The PID with value `v`, or `None` if `v` does not fit in 13 bits.
    pub const fn new(v: u16) -> Option<Self> {
        if v <= Self::MAX.0 { Some(Pid(v)) } else { None }
    }

    /// The low 13 bits of `bits`, for decoders that have just read a 13-bit
    /// field (`PID`, `program_map_PID`, `PCR_PID`, `elementary_PID`, …).
    /// Such a value is already within range, so the mask never changes it
    /// and the codec stays lossless; the mask exists so that a decoder is
    /// `const`, infallible and panic-free instead of carrying an
    /// `expect` for a case that cannot occur.
    pub const fn from_wire(bits: u16) -> Self {
        Pid(bits & Self::MAX.0)
    }

    /// The PID of a transport packet from its first three bytes (Table 2-2:
    /// the low 5 bits of byte 1 and byte 2), or `None` if `packet` is
    /// shorter than that.  The sync byte is not checked.
    pub const fn of_packet(packet: &[u8]) -> Option<Self> {
        if packet.len() < 3 {
            None
        } else {
            Some(Self::from_wire(
                ((packet[1] & 0x1F) as u16) << 8 | packet[2] as u16,
            ))
        }
    }

    /// The value, 0..=0x1FFF.
    pub const fn get(self) -> u16 {
        self.0
    }

    /// True for the PIDs Table 2-3 does not leave free for assignment:
    /// 0x0000–0x0003 (PAT, CAT, TSDT, IPMP), 0x0004–0x000F ("Reserved")
    /// and 0x1FFF (null packet).  The SI PIDs of ARIB STD-B10 Part 1 Table
    /// 5-1 (0x0010–0x0027) are assignments *within* the free range, not
    /// H.222.0 reservations, and are not reported here.
    pub const fn is_reserved(self) -> bool {
        self.0 <= 0x000F || self.0 == Self::NULL.0
    }

    /// True if Table 2-3 allows the PID to be "assigned as network_PID,
    /// Program_map_PID, elementary_PID, or for other purposes": 0x0010–0x1FFE.
    pub const fn is_assignable(self) -> bool {
        !self.is_reserved()
    }

    /// True if a packet on this PID may carry a PCR (Table 2-3 NOTE:
    /// "PID values 0x0000, 0x0001, and 0x0010-0x1FFE").
    pub const fn may_carry_pcr(self) -> bool {
        matches!(self.0, 0x0000 | 0x0001 | 0x0010..=0x1FFE)
    }
}

impl TryFrom<u16> for Pid {
    type Error = Error;

    /// [`Error::Overflow`] (the error `BitWriter` gave for the same value
    /// at encode time) if `v` does not fit in 13 bits.
    fn try_from(v: u16) -> Result<Self, Error> {
        Self::new(v).ok_or(Error::Overflow {
            what: "PID",
            value: u64::from(v),
            bits: 13,
        })
    }
}

impl From<Pid> for u16 {
    fn from(p: Pid) -> u16 {
        p.0
    }
}

impl From<Pid> for u32 {
    fn from(p: Pid) -> u32 {
        u32::from(p.0)
    }
}

impl From<Pid> for u64 {
    fn from(p: Pid) -> u64 {
        u64::from(p.0)
    }
}

impl From<Pid> for usize {
    fn from(p: Pid) -> usize {
        usize::from(p.0)
    }
}

impl fmt::Display for Pid {
    /// `0x1fff`: four hex digits, lower case, as the analysers print PIDs.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:#06x}", self.0)
    }
}

impl fmt::Debug for Pid {
    /// Same as [`Display`](fmt::Display): `0x1fff`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

impl fmt::LowerHex for Pid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::LowerHex::fmt(&self.0, f)
    }
}

impl fmt::UpperHex for Pid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::UpperHex::fmt(&self.0, f)
    }
}

#[cfg(feature = "serde")]
impl serde::Serialize for Pid {
    /// The integer value (so a PID in a TOML config or JSON report is a
    /// plain number).
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_u16(self.0)
    }
}

#[cfg(feature = "serde")]
impl<'de> serde::Deserialize<'de> for Pid {
    /// An integer 0..=8191; anything else is an error naming the value and
    /// the range.  Deserialisers that track the key being read (TOML,
    /// JSON) add its location.
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let v = i64::deserialize(d)?;
        u16::try_from(v)
            .ok()
            .and_then(Pid::new)
            .ok_or_else(|| {
                serde::de::Error::custom(format!(
                    "PID {v} is out of range: a PID is a 13-bit field, 0..=8191 / 0x1FFF (H.222.0 Table 2-3)"
                ))
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Table 2-3: the assigned values, the 13-bit limit and the reserved
    /// ranges.
    #[test]
    fn spec_table_2_3_pids() {
        assert_eq!(Pid::PAT.get(), 0x0000);
        assert_eq!(Pid::CAT.get(), 0x0001);
        assert_eq!(Pid::TSDT.get(), 0x0002);
        assert_eq!(Pid::IPMP.get(), 0x0003);
        assert_eq!(Pid::NULL.get(), 0x1FFF);
        assert_eq!(Pid::MAX, Pid::NULL);

        assert_eq!(Pid::new(0x1FFF), Some(Pid::NULL));
        assert_eq!(Pid::new(0x2000), None);
        assert_eq!(Pid::new(u16::MAX), None);
        assert!(matches!(
            Pid::try_from(0x2000u16),
            Err(Error::Overflow {
                what: "PID",
                value: 0x2000,
                bits: 13
            })
        ));
        assert_eq!(u16::from(Pid::NULL), 0x1FFF);

        // 0x0000–0x000F and 0x1FFF are not free for assignment; everything
        // between is ("May be assigned as network_PID, Program_map_PID,
        // elementary_PID, or for other purposes").
        for v in 0x0000..=0x000F {
            assert!(Pid(v).is_reserved(), "{v:#06x}");
            assert!(!Pid(v).is_assignable(), "{v:#06x}");
        }
        for v in 0x0010..=0x1FFE {
            assert!(!Pid(v).is_reserved(), "{v:#06x}");
            assert!(Pid(v).is_assignable(), "{v:#06x}");
        }
        assert!(Pid::NULL.is_reserved());
        assert!(!Pid::NULL.is_assignable());

        // NOTE: PCR allowed on 0x0000, 0x0001 and 0x0010–0x1FFE.
        assert!(Pid::PAT.may_carry_pcr());
        assert!(Pid::CAT.may_carry_pcr());
        assert!(!Pid::TSDT.may_carry_pcr());
        assert!(!Pid::IPMP.may_carry_pcr());
        assert!(!Pid(0x000F).may_carry_pcr());
        assert!(Pid(0x0010).may_carry_pcr());
        assert!(Pid(0x1FFE).may_carry_pcr());
        assert!(!Pid::NULL.may_carry_pcr());

        // Display / Debug are the analysers' `{:#06x}`; the hex traits
        // format the value like a u16.
        assert_eq!(Pid::NULL.to_string(), "0x1fff");
        assert_eq!(format!("{:?}", Pid::PAT), "0x0000");
        assert_eq!(format!("{:#06x}", Pid(0x01F0)), "0x01f0");
        assert_eq!(format!("{:#06X}", Pid(0x01F0)), "0x01F0");
        assert_eq!(format!("{:x}", Pid(0x01F0)), "1f0");
    }

    /// Table 2-2: the PID of a packet is bits 3..=15 of bytes 1–2.
    #[test]
    fn spec_table_2_2_pid_bytes() {
        assert_eq!(Pid::of_packet(&[0x47, 0x5F, 0xFF]), Some(Pid::NULL));
        assert_eq!(Pid::of_packet(&[0x47, 0xE1, 0xF0]), Some(Pid(0x01F0)));
        assert_eq!(Pid::of_packet(&[0x47, 0x00]), None);
        assert_eq!(Pid::from_wire(0xFFFF), Pid::NULL);
    }
}
