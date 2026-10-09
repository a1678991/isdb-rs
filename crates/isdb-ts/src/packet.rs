//! Transport Stream packet (H.222.0 §2.4.3.2 Table 2-2) and adaptation
//! field (§2.4.3.4 Table 2-6), decoded losslessly.
//!
//! [`Packet::decode`] keeps every bit of a 188-byte packet — reserved bits,
//! marker bits, stuffing bytes and even bytes the syntax does not describe
//! (payload bytes of an `adaptation_field_control = '00'`/`'10'` packet) —
//! so [`Packet::encode`] reproduces the input exactly.  Only structurally
//! impossible inputs (lengths running past the packet) are rejected.
//! Semantic rules (value ranges, null-packet constraints, …) are reported
//! by [`check`], not enforced by the codec.
//!
//! [`View`] is a zero-copy accessor for hot paths (demultiplexers) that only
//! need the header, payload slice, PCR and discontinuity_indicator.

use crate::bits::{BitReader, BitWriter};
use crate::error::{Error, Result};
use crate::pcr::{Pcr, Timestamp};
use crate::validate::{Violation, require, reserved_ones};

/// Transport Stream packets "shall be 188 bytes long" (§2.4.3).
pub const PACKET_SIZE: usize = 188;
/// Bytes after the 4-byte header: "N … is specified by 184 minus the number
/// of bytes in the adaptation_field()" (§2.4.3.3 data_byte).
pub const PAYLOAD_CAPACITY: usize = 184;
/// sync_byte value '0100 0111' (§2.4.3.3).
pub const SYNC_BYTE: u8 = 0x47;
/// PID of the Program Association Table (Table 2-3).
pub const PID_PAT: u16 = 0x0000;
/// PID of the Conditional Access Table (Table 2-3).
pub const PID_CAT: u16 = 0x0001;
/// PID of the Transport Stream Description Table (Table 2-3).
pub const PID_TSDT: u16 = 0x0002;
/// PID of null packets (Table 2-3).
pub const PID_NULL: u16 = 0x1FFF;
/// Largest PID value (13 bits, Table 2-2).
pub const PID_MAX: u16 = 0x1FFF;

/// adaptation_field_control values (Table 2-5).
pub mod afc {
    /// '00' — reserved for future use; decoders shall discard the packet.
    pub const RESERVED: u8 = 0b00;
    /// '01' — no adaptation_field, payload only.
    pub const PAYLOAD_ONLY: u8 = 0b01;
    /// '10' — adaptation_field only, no payload.
    pub const ADAPTATION_ONLY: u8 = 0b10;
    /// '11' — adaptation_field followed by payload.
    pub const ADAPTATION_AND_PAYLOAD: u8 = 0b11;
}

/// The 4-byte packet header of Table 2-2, every field verbatim.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Header {
    /// sync_byte (spec value 0x47, §2.4.3.3).
    pub sync_byte: u8,
    /// transport_error_indicator.
    pub transport_error_indicator: bool,
    /// payload_unit_start_indicator.
    pub payload_unit_start_indicator: bool,
    /// transport_priority.
    pub transport_priority: bool,
    /// PID (13 bits).
    pub pid: u16,
    /// transport_scrambling_control (2 bits, Table 2-4).
    pub transport_scrambling_control: u8,
    /// adaptation_field_control (2 bits, Table 2-5, see [`afc`]).
    pub adaptation_field_control: u8,
    /// continuity_counter (4 bits).
    pub continuity_counter: u8,
}

impl Header {
    /// Header with sync 0x47, flags clear, not scrambled.
    pub fn new(pid: u16, adaptation_field_control: u8, continuity_counter: u8) -> Self {
        Self {
            sync_byte: SYNC_BYTE,
            transport_error_indicator: false,
            payload_unit_start_indicator: false,
            transport_priority: false,
            pid,
            transport_scrambling_control: 0,
            adaptation_field_control,
            continuity_counter,
        }
    }

    /// Decode the first 4 bytes of `b` (Table 2-2).
    pub fn decode(b: &[u8]) -> Result<Self> {
        if b.len() < 4 {
            return Err(Error::Truncated {
                what: "transport_packet header",
                need: 4,
                have: b.len(),
            });
        }
        Ok(Self {
            sync_byte: b[0],
            transport_error_indicator: b[1] & 0x80 != 0,
            payload_unit_start_indicator: b[1] & 0x40 != 0,
            transport_priority: b[1] & 0x20 != 0,
            pid: (((b[1] & 0x1F) as u16) << 8) | b[2] as u16,
            transport_scrambling_control: b[3] >> 6,
            adaptation_field_control: (b[3] >> 4) & 3,
            continuity_counter: b[3] & 0x0F,
        })
    }

    /// Encode to 4 bytes; errors if a field exceeds its width.
    pub fn encode(&self) -> Result<[u8; 4]> {
        let mut w = BitWriter::new();
        w.write("sync_byte", 8, self.sync_byte as u64)?;
        w.bool(self.transport_error_indicator);
        w.bool(self.payload_unit_start_indicator);
        w.bool(self.transport_priority);
        w.write("PID", 13, self.pid as u64)?;
        w.write(
            "transport_scrambling_control",
            2,
            self.transport_scrambling_control as u64,
        )?;
        w.write(
            "adaptation_field_control",
            2,
            self.adaptation_field_control as u64,
        )?;
        w.write("continuity_counter", 4, self.continuity_counter as u64)?;
        let v = w.into_bytes();
        Ok([v[0], v[1], v[2], v[3]])
    }

    /// adaptation_field_control is '10' or '11' (Table 2-2).
    pub fn has_adaptation_field(&self) -> bool {
        self.adaptation_field_control & 0b10 != 0
    }

    /// adaptation_field_control is '01' or '11' (Table 2-2).
    pub fn has_payload(&self) -> bool {
        self.adaptation_field_control == afc::PAYLOAD_ONLY
            || self.adaptation_field_control == afc::ADAPTATION_AND_PAYLOAD
    }
}

/// adaptation_field() of Table 2-6.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AdaptationField {
    /// adaptation_field_length = 0: "for inserting a single stuffing byte"
    /// (§2.4.3.5); no flags byte follows.
    Empty,
    /// adaptation_field_length > 0.
    Fields(AdaptationFields),
}

/// The part of Table 2-6 under `if (adaptation_field_length > 0)`.
/// Flags are implied by the `Option`s.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AdaptationFields {
    /// discontinuity_indicator (§2.4.3.5).
    pub discontinuity_indicator: bool,
    /// random_access_indicator.
    pub random_access_indicator: bool,
    /// elementary_stream_priority_indicator.
    pub elementary_stream_priority_indicator: bool,
    /// PCR (PCR_flag).
    pub pcr: Option<Pcr>,
    /// OPCR (OPCR_flag).
    pub opcr: Option<Pcr>,
    /// splice_countdown, two's complement (`tcimsbf`) (splicing_point_flag).
    pub splice_countdown: Option<i8>,
    /// private_data_bytes (transport_private_data_flag); length implied.
    pub transport_private_data: Option<Vec<u8>>,
    /// adaptation field extension (adaptation_field_extension_flag).
    pub extension: Option<AdaptationFieldExtension>,
    /// Trailing stuffing_bytes, kept verbatim (spec value 0xFF).
    pub stuffing: Vec<u8>,
}

/// Adaptation field extension of Table 2-6.  `adaptation_field_extension_length`
/// is implied by the encoded size of the fields plus `reserved_bytes`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AdaptationFieldExtension {
    /// The 5 reserved bits after seamless_splice_flag (spec '11111').
    pub reserved: u8,
    /// ltw_valid_flag and ltw_offset (ltw_flag).
    pub ltw: Option<Ltw>,
    /// reserved(2) and piecewise_rate (piecewise_rate_flag).
    pub piecewise_rate: Option<PiecewiseRate>,
    /// splice_type and DTS_next_AU (seamless_splice_flag); the splice_type
    /// occupies [`Timestamp::prefix`].
    pub seamless_splice: Option<Timestamp>,
    /// The trailing `reserved` bytes ("for (i = 0; i < N; i++) reserved 8").
    pub reserved_bytes: Vec<u8>,
}

/// Legal time window fields (Table 2-6, §2.4.3.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ltw {
    /// ltw_valid_flag.
    pub valid: bool,
    /// ltw_offset (15 bits, units of 300/fs s).
    pub offset: u16,
}

/// piecewise_rate with its 2 leading reserved bits (Table 2-6).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PiecewiseRate {
    /// The 2 reserved bits (spec '11').
    pub reserved: u8,
    /// piecewise_rate (22 bits).
    pub rate: u32,
}

impl AdaptationFieldExtension {
    fn decode(b: &[u8]) -> Result<Self> {
        let mut r = BitReader::new(b, "adaptation_field_extension");
        let ltw_flag = r.bool()?;
        let pw_flag = r.bool()?;
        let ss_flag = r.bool()?;
        let reserved = r.read(5)? as u8;
        let ltw = if ltw_flag {
            Some(Ltw {
                valid: r.bool()?,
                offset: r.read(15)? as u16,
            })
        } else {
            None
        };
        let piecewise_rate = if pw_flag {
            Some(PiecewiseRate {
                reserved: r.read(2)? as u8,
                rate: r.read(22)? as u32,
            })
        } else {
            None
        };
        let seamless_splice = if ss_flag {
            Some(Timestamp::read(&mut r)?)
        } else {
            None
        };
        let reserved_bytes = r.rest()?.to_vec();
        Ok(Self {
            reserved,
            ltw,
            piecewise_rate,
            seamless_splice,
            reserved_bytes,
        })
    }

    fn write(&self, w: &mut BitWriter) -> Result<()> {
        w.bool(self.ltw.is_some());
        w.bool(self.piecewise_rate.is_some());
        w.bool(self.seamless_splice.is_some());
        w.write(
            "adaptation_field_extension reserved",
            5,
            self.reserved as u64,
        )?;
        if let Some(l) = self.ltw {
            w.bool(l.valid);
            w.write("ltw_offset", 15, l.offset as u64)?;
        }
        if let Some(p) = self.piecewise_rate {
            w.write("piecewise_rate reserved", 2, p.reserved as u64)?;
            w.write("piecewise_rate", 22, p.rate as u64)?;
        }
        if let Some(t) = self.seamless_splice {
            t.write(w)?;
        }
        w.bytes(&self.reserved_bytes);
        Ok(())
    }

    /// Value of adaptation_field_extension_length for this extension.
    pub fn length(&self) -> usize {
        1 + self.ltw.map_or(0, |_| 2)
            + self.piecewise_rate.map_or(0, |_| 3)
            + self.seamless_splice.map_or(0, |_| Timestamp::LEN)
            + self.reserved_bytes.len()
    }
}

impl AdaptationFields {
    /// Value of adaptation_field_length for these fields (flags byte,
    /// optional fields and stuffing).
    pub fn length(&self) -> usize {
        1 + self.pcr.map_or(0, |_| Pcr::LEN)
            + self.opcr.map_or(0, |_| Pcr::LEN)
            + self.splice_countdown.map_or(0, |_| 1)
            + self
                .transport_private_data
                .as_ref()
                .map_or(0, |d| 1 + d.len())
            + self.extension.as_ref().map_or(0, |e| 1 + e.length())
            + self.stuffing.len()
    }
}

impl AdaptationField {
    /// Decode an adaptation_field starting at `b[0]`
    /// (adaptation_field_length); `b` is the rest of the packet.  Returns the
    /// field and its total size including the length byte.
    pub fn decode(b: &[u8]) -> Result<(Self, usize)> {
        let Some(&afl) = b.first() else {
            return Err(Error::Truncated {
                what: "adaptation_field_length",
                need: 1,
                have: 0,
            });
        };
        let afl = afl as usize;
        if 1 + afl > b.len() {
            return Err(Error::invalid(
                "adaptation_field_length",
                format!(
                    "{afl} runs past the packet ({} bytes available)",
                    b.len() - 1
                ),
            ));
        }
        if afl == 0 {
            return Ok((AdaptationField::Empty, 1));
        }
        let mut r = BitReader::new(&b[1..1 + afl], "adaptation_field");
        let discontinuity_indicator = r.bool()?;
        let random_access_indicator = r.bool()?;
        let elementary_stream_priority_indicator = r.bool()?;
        let pcr_flag = r.bool()?;
        let opcr_flag = r.bool()?;
        let splicing_point_flag = r.bool()?;
        let private_flag = r.bool()?;
        let ext_flag = r.bool()?;
        let pcr = if pcr_flag {
            Some(Pcr::decode(r.bytes(Pcr::LEN)?)?)
        } else {
            None
        };
        let opcr = if opcr_flag {
            Some(Pcr::decode(r.bytes(Pcr::LEN)?)?)
        } else {
            None
        };
        let splice_countdown = if splicing_point_flag {
            Some(r.u8()? as i8)
        } else {
            None
        };
        let transport_private_data = if private_flag {
            let n = r.u8()? as usize;
            Some(
                r.bytes(n)
                    .map_err(|_| {
                        Error::invalid(
                            "transport_private_data_length",
                            "private data extends beyond the adaptation field",
                        )
                    })?
                    .to_vec(),
            )
        } else {
            None
        };
        let extension = if ext_flag {
            let n = r.u8()? as usize;
            if n == 0 {
                return Err(Error::invalid(
                    "adaptation_field_extension_length",
                    "0 leaves no room for the ltw/piecewise_rate/seamless_splice flags byte",
                ));
            }
            let e = r.bytes(n).map_err(|_| {
                Error::invalid(
                    "adaptation_field_extension_length",
                    "extension runs past the adaptation field",
                )
            })?;
            Some(AdaptationFieldExtension::decode(e)?)
        } else {
            None
        };
        let stuffing = r.rest()?.to_vec();
        Ok((
            AdaptationField::Fields(AdaptationFields {
                discontinuity_indicator,
                random_access_indicator,
                elementary_stream_priority_indicator,
                pcr,
                opcr,
                splice_countdown,
                transport_private_data,
                extension,
                stuffing,
            }),
            1 + afl,
        ))
    }

    /// Value of adaptation_field_length.
    pub fn length(&self) -> usize {
        match self {
            AdaptationField::Empty => 0,
            AdaptationField::Fields(f) => f.length(),
        }
    }

    /// Total encoded size including the length byte.
    pub fn encoded_len(&self) -> usize {
        1 + self.length()
    }

    /// Append the encoded adaptation_field (length byte included).
    pub fn encode(&self, out: &mut Vec<u8>) -> Result<()> {
        let f = match self {
            AdaptationField::Empty => {
                out.push(0);
                return Ok(());
            }
            AdaptationField::Fields(f) => f,
        };
        let len = f.length();
        if len > 255 {
            return Err(Error::Overflow {
                what: "adaptation_field_length",
                value: len as u64,
                bits: 8,
            });
        }
        let mut w = BitWriter::new();
        w.write("adaptation_field_length", 8, len as u64)?;
        w.bool(f.discontinuity_indicator);
        w.bool(f.random_access_indicator);
        w.bool(f.elementary_stream_priority_indicator);
        w.bool(f.pcr.is_some());
        w.bool(f.opcr.is_some());
        w.bool(f.splice_countdown.is_some());
        w.bool(f.transport_private_data.is_some());
        w.bool(f.extension.is_some());
        if let Some(p) = f.pcr {
            p.write(&mut w)?;
        }
        if let Some(p) = f.opcr {
            p.write(&mut w)?;
        }
        if let Some(s) = f.splice_countdown {
            w.write("splice_countdown", 8, s as u8 as u64)?;
        }
        if let Some(d) = &f.transport_private_data {
            w.write("transport_private_data_length", 8, d.len() as u64)?;
            w.bytes(d);
        }
        if let Some(e) = &f.extension {
            w.write("adaptation_field_extension_length", 8, e.length() as u64)?;
            e.write(&mut w)?;
        }
        w.bytes(&f.stuffing);
        out.extend_from_slice(&w.into_bytes());
        Ok(())
    }

    /// The fields, `None` for [`AdaptationField::Empty`].
    pub fn fields(&self) -> Option<&AdaptationFields> {
        match self {
            AdaptationField::Empty => None,
            AdaptationField::Fields(f) => Some(f),
        }
    }

    /// The PCR if PCR_flag is set.
    pub fn pcr(&self) -> Option<Pcr> {
        self.fields().and_then(|f| f.pcr)
    }

    /// discontinuity_indicator ('0' when not present, §2.4.3.5).
    pub fn discontinuity_indicator(&self) -> bool {
        self.fields().is_some_and(|f| f.discontinuity_indicator)
    }

    /// random_access_indicator ('0' when not present).
    pub fn random_access_indicator(&self) -> bool {
        self.fields().is_some_and(|f| f.random_access_indicator)
    }

    /// Adaptation field of exactly `total` bytes (length byte included)
    /// holding `fields` padded with 0xFF stuffing_bytes (§2.4.3.5 "Stuffing
    /// is accomplished by defining an adaptation field longer than the sum of
    /// the lengths of the data elements in it").  `fields.stuffing` is
    /// replaced.  `total = 1` yields [`AdaptationField::Empty`] when
    /// `fields` is `None`.
    pub fn stuffed(fields: Option<AdaptationFields>, total: usize) -> Result<Self> {
        match fields {
            None if total == 1 => Ok(AdaptationField::Empty),
            None if total == 0 => Err(Error::invalid(
                "adaptation_field",
                "size 0 cannot hold adaptation_field_length",
            )),
            None => Self::stuffed(Some(AdaptationFields::default()), total),
            Some(mut f) => {
                f.stuffing.clear();
                let need = 1 + f.length();
                if need > total {
                    return Err(Error::invalid(
                        "adaptation_field",
                        format!("fields need {need} bytes, only {total} available"),
                    ));
                }
                f.stuffing = vec![0xFF; total - need];
                Ok(AdaptationField::Fields(f))
            }
        }
    }
}

/// A complete 188-byte transport packet (Table 2-2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Packet {
    /// Packet header.
    pub header: Header,
    /// adaptation_field, present iff adaptation_field_control is '10'/'11'.
    pub adaptation_field: Option<AdaptationField>,
    /// The bytes after the header and adaptation field: data_bytes for
    /// '01'/'11'; for '10' and the reserved '00' the bytes the syntax does
    /// not describe (kept for losslessness; see [`check`]).
    pub payload: Vec<u8>,
}

impl Packet {
    /// Decode a 188-byte packet losslessly.
    pub fn decode(b: &[u8]) -> Result<Self> {
        if b.len() != PACKET_SIZE {
            return Err(Error::invalid(
                "transport_packet",
                format!("{} bytes, shall be 188", b.len()),
            ));
        }
        let header = Header::decode(b)?;
        let rest = &b[4..];
        let (adaptation_field, n) = if header.has_adaptation_field() {
            let (af, n) = AdaptationField::decode(rest)?;
            (Some(af), n)
        } else {
            (None, 0)
        };
        Ok(Self {
            header,
            adaptation_field,
            payload: rest[n..].to_vec(),
        })
    }

    /// Encode to 188 bytes.  Errors if the adaptation field presence
    /// disagrees with adaptation_field_control or the sizes do not add up to
    /// 188.
    pub fn encode(&self) -> Result<[u8; PACKET_SIZE]> {
        if self.header.has_adaptation_field() != self.adaptation_field.is_some() {
            return Err(Error::invalid(
                "adaptation_field_control",
                format!(
                    "'{:02b}' disagrees with adaptation field presence ({})",
                    self.header.adaptation_field_control,
                    self.adaptation_field.is_some()
                ),
            ));
        }
        let mut v = Vec::with_capacity(PACKET_SIZE);
        v.extend_from_slice(&self.header.encode()?);
        if let Some(af) = &self.adaptation_field {
            af.encode(&mut v)?;
        }
        v.extend_from_slice(&self.payload);
        if v.len() != PACKET_SIZE {
            return Err(Error::invalid(
                "transport_packet",
                format!("encodes to {} bytes, shall be 188", v.len()),
            ));
        }
        let mut out = [0u8; PACKET_SIZE];
        out.copy_from_slice(&v);
        Ok(out)
    }

    /// Packet carrying `payload` (≤ 184 bytes) on `pid`.  A full 184-byte
    /// payload gives adaptation_field_control '01'; a shorter one is padded
    /// with adaptation-field stuffing ('11'), the only stuffing method
    /// allowed for PES (§2.4.3.5).
    pub fn with_payload(pid: u16, pusi: bool, cc: u8, payload: &[u8]) -> Result<Self> {
        if payload.len() == PAYLOAD_CAPACITY {
            let mut header = Header::new(pid, afc::PAYLOAD_ONLY, cc);
            header.payload_unit_start_indicator = pusi;
            Ok(Self {
                header,
                adaptation_field: None,
                payload: payload.to_vec(),
            })
        } else {
            Self::with_adaptation(pid, pusi, cc, None, payload)
        }
    }

    /// Packet with an adaptation field holding `fields` (or only stuffing
    /// if `None`) followed by `payload`; stuffing bytes fill the adaptation
    /// field so the packet is exactly 188 bytes.  An empty `payload` gives
    /// adaptation_field_control '10' (adaptation_field_length 183).
    pub fn with_adaptation(
        pid: u16,
        pusi: bool,
        cc: u8,
        fields: Option<AdaptationFields>,
        payload: &[u8],
    ) -> Result<Self> {
        if payload.len() > PAYLOAD_CAPACITY - 1 {
            return Err(Error::invalid(
                "data_byte",
                format!(
                    "{} payload bytes leave no room for an adaptation field",
                    payload.len()
                ),
            ));
        }
        let afc = if payload.is_empty() {
            afc::ADAPTATION_ONLY
        } else {
            afc::ADAPTATION_AND_PAYLOAD
        };
        let af = AdaptationField::stuffed(fields, PAYLOAD_CAPACITY - payload.len())?;
        let mut header = Header::new(pid, afc, cc);
        header.payload_unit_start_indicator = pusi;
        Ok(Self {
            header,
            adaptation_field: Some(af),
            payload: payload.to_vec(),
        })
    }

    /// Adaptation-field-only packet ('10', adaptation_field_length 183)
    /// carrying `pcr` (§2.4.3.4/§2.4.3.5).
    pub fn pcr_only(pid: u16, cc: u8, pcr: Pcr) -> Self {
        let fields = AdaptationFields {
            pcr: Some(pcr),
            ..Default::default()
        };
        Self::with_adaptation(pid, false, cc, Some(fields), &[]).expect("PCR fits in 183 bytes")
    }

    /// Null packet: PID 0x1FFF, payload_unit_start_indicator '0',
    /// transport_scrambling_control '00', adaptation_field_control '01'
    /// (§2.4.3.3); data_bytes "may be assigned any value" — 0xFF here.
    pub fn null() -> Self {
        Self {
            header: Header::new(PID_NULL, afc::PAYLOAD_ONLY, 0),
            adaptation_field: None,
            payload: vec![0xFF; PAYLOAD_CAPACITY],
        }
    }

    /// The data_bytes if adaptation_field_control is '01' or '11'.
    pub fn data_bytes(&self) -> Option<&[u8]> {
        self.header.has_payload().then_some(self.payload.as_slice())
    }
}

/// Zero-copy view of a 188-byte packet (H.222.0 Table 2-2): header plus the
/// adaptation-field (Table 2-6) and payload slices, validated only
/// structurally.
#[derive(Debug, Clone, Copy)]
pub struct View<'a> {
    /// Decoded header.
    pub header: Header,
    /// Whole adaptation_field (length byte included) if present.
    pub adaptation_field: Option<&'a [u8]>,
    /// data_bytes if adaptation_field_control is '01'/'11'.
    pub payload: Option<&'a [u8]>,
}

impl<'a> View<'a> {
    /// Parse `b` (188 bytes).  Errors only if the adaptation field runs past
    /// the packet.
    pub fn parse(b: &'a [u8]) -> Result<Self> {
        if b.len() != PACKET_SIZE {
            return Err(Error::invalid(
                "transport_packet",
                format!("{} bytes, shall be 188", b.len()),
            ));
        }
        let header = Header::decode(b)?;
        let mut pos = 4;
        let mut adaptation_field = None;
        if header.has_adaptation_field() {
            let afl = b[4] as usize;
            if 5 + afl > PACKET_SIZE {
                return Err(Error::invalid(
                    "adaptation_field_length",
                    format!("{afl} runs past the packet"),
                ));
            }
            adaptation_field = Some(&b[4..5 + afl]);
            pos = 5 + afl;
        }
        let payload = header.has_payload().then(|| &b[pos..]);
        Ok(Self {
            header,
            adaptation_field,
            payload,
        })
    }

    /// discontinuity_indicator ('0' when absent).
    pub fn discontinuity_indicator(&self) -> bool {
        self.adaptation_field
            .is_some_and(|a| a.len() > 1 && a[1] & 0x80 != 0)
    }

    /// random_access_indicator ('0' when absent).
    pub fn random_access_indicator(&self) -> bool {
        self.adaptation_field
            .is_some_and(|a| a.len() > 1 && a[1] & 0x40 != 0)
    }

    /// The PCR if PCR_flag is set and the field fits.
    pub fn pcr(&self) -> Option<Pcr> {
        let a = self.adaptation_field?;
        if a.len() >= 2 + Pcr::LEN && a[1] & 0x10 != 0 {
            Pcr::decode(&a[2..2 + Pcr::LEN]).ok()
        } else {
            None
        }
    }
}

/// Report violations of H.222.0 rules on a single decoded packet.
///
/// Rules checked (clause): sync_byte 0x47 (§2.4.3.3); adaptation_field_control
/// '00' reserved (Table 2-5); adaptation_field_length 0..182 for '11' and 183
/// for '10' (§2.4.3.5); bytes not described by the syntax (Table 2-2);
/// null packets: PUSI '0', scrambling '00', AFC '01' (§2.4.3.3); PCR only
/// on PIDs 0x0000, 0x0001, 0x0010–0x1FFE (Table 2-3 NOTE); PCR/OPCR
/// extension < 300 (eq. 2-3/2-10); OPCR only with PCR (§2.4.3.5);
/// stuffing_byte 0xFF (§2.4.3.5); seamless_splice_flag only with
/// splicing_point_flag (§2.4.3.5); marker bits '1' (§2.4.3.7); reserved
/// bits/bytes all '1' (§2.1.64).
pub fn check(p: &Packet) -> Vec<Violation> {
    let mut v = Vec::new();
    let h = &p.header;
    require(&mut v, h.sync_byte == SYNC_BYTE, "§2.4.3.3", || {
        format!("sync_byte {:#04x}, shall be 0x47", h.sync_byte)
    });
    require(
        &mut v,
        h.adaptation_field_control != afc::RESERVED,
        "§2.4.3.3 Table 2-5",
        || "adaptation_field_control '00' is reserved; decoders shall discard the packet".into(),
    );
    if let Some(af) = &p.adaptation_field {
        let len = af.length();
        if h.adaptation_field_control == afc::ADAPTATION_AND_PAYLOAD {
            require(&mut v, len <= 182, "§2.4.3.5", || {
                format!("adaptation_field_length {len} with AFC '11', shall be 0..182")
            });
        } else if h.adaptation_field_control == afc::ADAPTATION_ONLY {
            require(&mut v, len == 183, "§2.4.3.5", || {
                format!("adaptation_field_length {len} with AFC '10', shall be 183")
            });
        }
    }
    if !h.has_payload() && !p.payload.is_empty() {
        v.push(Violation::new(
            "§2.4.3.2 Table 2-2",
            format!(
                "{} bytes after the adaptation field are not described by the syntax (no payload)",
                p.payload.len()
            ),
        ));
    }
    if h.pid == PID_NULL {
        require(&mut v, !h.payload_unit_start_indicator, "§2.4.3.3", || {
            "null packet with payload_unit_start_indicator '1'".into()
        });
        require(
            &mut v,
            h.transport_scrambling_control == 0,
            "§2.4.3.3",
            || {
                format!(
                    "null packet transport_scrambling_control '{:02b}', shall be '00'",
                    h.transport_scrambling_control
                )
            },
        );
        require(
            &mut v,
            h.adaptation_field_control == afc::PAYLOAD_ONLY,
            "§2.4.3.3",
            || {
                format!(
                    "null packet adaptation_field_control '{:02b}', shall be '01'",
                    h.adaptation_field_control
                )
            },
        );
    }
    let Some(f) = p.adaptation_field.as_ref().and_then(|a| a.fields()) else {
        return v;
    };
    if let Some(pcr) = f.pcr {
        let ok_pid = matches!(h.pid, 0x0000 | 0x0001 | 0x0010..=0x1FFE);
        require(&mut v, ok_pid, "§2.4.3.3 Table 2-3 NOTE", || {
            format!("PCR on PID {:#06x}", h.pid)
        });
        check_pcr(&mut v, &pcr, "PCR");
    }
    if let Some(opcr) = f.opcr {
        require(&mut v, f.pcr.is_some(), "§2.4.3.5", || {
            "OPCR present without PCR".into()
        });
        check_pcr(&mut v, &opcr, "OPCR");
    }
    require(
        &mut v,
        f.stuffing.iter().all(|&b| b == 0xFF),
        "§2.4.3.5",
        || "adaptation field stuffing_byte not 0xFF".into(),
    );
    if let Some(e) = &f.extension {
        reserved_ones(
            &mut v,
            e.reserved as u64,
            5,
            "adaptation_field_extension reserved",
        );
        if let Some(pw) = e.piecewise_rate {
            reserved_ones(&mut v, pw.reserved as u64, 2, "piecewise_rate reserved");
        }
        if let Some(ss) = e.seamless_splice {
            require(&mut v, f.splice_countdown.is_some(), "§2.4.3.5", || {
                "seamless_splice_flag '1' without splicing_point_flag".into()
            });
            require(&mut v, ss.markers == 0b111, "§2.4.3.7", || {
                format!("DTS_next_AU marker bits {:03b}", ss.markers)
            });
        }
        require(
            &mut v,
            e.reserved_bytes.iter().all(|&b| b == 0xFF),
            "§2.1.64",
            || "adaptation_field_extension reserved bytes not 0xFF".into(),
        );
    }
    v
}

fn check_pcr(v: &mut Vec<Violation>, p: &Pcr, name: &'static str) {
    require(v, p.extension < 300, "§2.4.2.2 eq. 2-3", || {
        format!("{name} extension {} ≥ 300", p.extension)
    });
    reserved_ones(v, p.reserved as u64, 6, name);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Table 2-2 field layout: 8+1+1+1+13+2+2+4 bits.
    #[test]
    fn spec_table2_2_header_layout() {
        let h = Header::decode(&[0x47, 0x5F, 0xFF, 0x9A]).unwrap();
        assert_eq!(h.sync_byte, 0x47);
        assert!(!h.transport_error_indicator);
        assert!(h.payload_unit_start_indicator);
        assert!(!h.transport_priority);
        assert_eq!(h.pid, 0x1FFF);
        assert_eq!(h.transport_scrambling_control, 0b10);
        assert_eq!(h.adaptation_field_control, 0b01);
        assert_eq!(h.continuity_counter, 0xA);
        assert_eq!(h.encode().unwrap(), [0x47, 0x5F, 0xFF, 0x9A]);
        let h = Header::decode(&[0x47, 0xA0, 0x00, 0x30]).unwrap();
        assert!(
            h.transport_error_indicator && h.transport_priority && !h.payload_unit_start_indicator
        );
        assert_eq!(h.adaptation_field_control, afc::ADAPTATION_AND_PAYLOAD);
    }

    /// §2.4.3.5: AFC '10' ⇒ adaptation_field_length 183; '11' ⇒ 0..182;
    /// length 0 is a single stuffing byte.
    #[test]
    fn spec_2_4_3_5_adaptation_field_length_rules() {
        let p = Packet::pcr_only(0x100, 3, Pcr::from_ticks(1234));
        let raw = p.encode().unwrap();
        assert_eq!(raw[3] >> 4 & 3, afc::ADAPTATION_ONLY);
        assert_eq!(raw[4], 183);
        assert!(check(&p).is_empty());
        let p = Packet::with_payload(0x100, false, 0, &[0xAB; 183]).unwrap();
        let raw = p.encode().unwrap();
        assert_eq!(raw[4], 0); // single stuffing byte
        assert_eq!(p.adaptation_field, Some(AdaptationField::Empty));
        assert!(check(&p).is_empty());
        // AFC '11' with length 183 violates 0..182
        let mut bad = raw;
        bad[3] = 0x30;
        bad[4] = 183;
        bad[5] = 0; // flags: none
        let p = Packet::decode(&bad).unwrap();
        assert!(check(&p).iter().any(|v| v.clause == "§2.4.3.5"));
        // AFC '10' with length 100 violates == 183 and leaves undescribed bytes
        let mut bad = [0xFFu8; 188];
        bad[..5].copy_from_slice(&[0x47, 0x01, 0x00, 0x20, 100]);
        bad[5] = 0;
        let p = Packet::decode(&bad).unwrap();
        assert_eq!(p.encode().unwrap(), bad);
        let c = check(&p);
        assert!(c.iter().any(|v| v.clause == "§2.4.3.5"));
        assert!(c.iter().any(|v| v.clause == "§2.4.3.2 Table 2-2"));
        // runs past the packet: structural error
        bad[4] = 184;
        assert!(Packet::decode(&bad).is_err());
    }

    /// Table 2-5: '00' is reserved; §2.4.3.3 null packet constraints.
    #[test]
    fn spec_table2_5_reserved_afc_and_null_packet() {
        let mut raw = Packet::null().encode().unwrap();
        assert_eq!(&raw[..4], &[0x47, 0x1F, 0xFF, 0x10]);
        assert!(check(&Packet::null()).is_empty());
        raw[3] = 0x00; // AFC '00'
        let p = Packet::decode(&raw).unwrap();
        assert_eq!(p.encode().unwrap(), raw);
        let c = check(&p);
        assert!(c.iter().any(|v| v.clause == "§2.4.3.3 Table 2-5"));
        assert!(
            c.iter()
                .any(|v| v.message.contains("null packet adaptation_field_control"))
        );
        raw[3] = 0x50;
        raw[1] |= 0x40;
        let c = check(&Packet::decode(&raw).unwrap());
        assert!(c.iter().any(|v| v.message.contains("payload_unit_start")));
        assert!(c.iter().any(|v| v.message.contains("scrambling")));
    }

    /// Table 2-6 order: flags, PCR, OPCR, splice_countdown, private data,
    /// extension (ltw, piecewise_rate, seamless_splice, reserved), stuffing.
    #[test]
    fn spec_table2_6_full_adaptation_field() {
        let f = AdaptationFields {
            discontinuity_indicator: true,
            random_access_indicator: true,
            elementary_stream_priority_indicator: false,
            pcr: Some(Pcr {
                base: 0x1_0000_0001,
                reserved: 0x3F,
                extension: 0x123,
            }),
            opcr: Some(Pcr::from_ticks(42)),
            splice_countdown: Some(-3),
            transport_private_data: Some(vec![1, 2, 3]),
            extension: Some(AdaptationFieldExtension {
                reserved: 0x1F,
                ltw: Some(Ltw {
                    valid: true,
                    offset: 0x7FFF,
                }),
                piecewise_rate: Some(PiecewiseRate {
                    reserved: 3,
                    rate: 0x3F_FFFF,
                }),
                seamless_splice: Some(Timestamp::new(0b0000, 0x1_2345_6789)),
                reserved_bytes: vec![0xFF, 0xFF],
            }),
            stuffing: vec![],
        };
        let p = Packet::with_adaptation(0x1FF, true, 7, Some(f.clone()), &[0x55; 100]).unwrap();
        let raw = p.encode().unwrap();
        assert_eq!(raw[4] as usize, 183 - 100);
        assert_eq!(raw[5], 0b1101_1111);
        assert_eq!(raw[12..18], Pcr::from_ticks(42).encode().unwrap());
        assert_eq!(raw[18], 0xFD); // -3 two's complement
        assert_eq!(&raw[19..23], &[3, 1, 2, 3]);
        assert_eq!(raw[23], 1 + 2 + 3 + 5 + 2); // adaptation_field_extension_length
        assert_eq!(raw[24], 0b1111_1111);
        let back = Packet::decode(&raw).unwrap();
        assert_eq!(back, p);
        assert!(check(&back).is_empty(), "{:?}", check(&back));
    }

    #[test]
    fn view_matches_decode() {
        let p = Packet::with_adaptation(
            0x30,
            true,
            1,
            Some(AdaptationFields {
                discontinuity_indicator: true,
                pcr: Some(Pcr::from_ticks(99)),
                ..Default::default()
            }),
            &[1, 2, 3],
        )
        .unwrap();
        let raw = p.encode().unwrap();
        let v = View::parse(&raw).unwrap();
        assert!(v.discontinuity_indicator());
        assert_eq!(v.pcr(), Some(Pcr::from_ticks(99)));
        assert_eq!(v.payload, Some(&[1u8, 2, 3][..]));
    }
}
