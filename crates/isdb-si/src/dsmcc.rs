//! DSM-CC data carousel sections as ARIB uses them for data broadcasting
//! (ARIB STD-B24 Vol. 3 Ch. 6 "Data carousel transmission"): the
//! DownloadInfoIndication (DII, table_id 0x3B) and DownloadDataBlock (DDB,
//! table_id 0x3C) messages and the descriptors of the DII module
//! information and private areas (§6.2.3, Tables 6-2 and 6-4 … 6-20; the
//! BML private data descriptor of Vol. 2 §9.3.4).
//!
//! The DSM-CC section (Vol. 3 §6.5 Table 6-24) is a long-form section:
//! `section_syntax_indicator` "must be set to 1 to transmit DII messages
//! and DDB messages", `private_indicator` "stores the complement value of
//! the section_syntax_indicator" (0), `dsmcc_section_length` "does not
//! exceed 4093". The `table_id_extension` is "the least significant two
//! bytes of the transaction_id" for a DII and "the module_id" for a DDB;
//! the `version_number` is "set to 0" for a DII ("Version management of
//! DII message is made by transaction_id") and "the least significant 5
//! bits of the moduleVersion" for a DDB; the `section_number` of a DDB is
//! "the least significant eight bits of the block number".
//!
//! Codecs are lossless: reserved fields are kept, `messageLength`,
//! `moduleInfoLength`, `privateDataLength` and `numberOfModules` are
//! recomputed on encode, and `encode(decode(x)) == x` for every valid
//! message. A `messageLength` that disagrees with the section is an error.

use isdb_ts::bits::{BitReader, BitWriter};
use isdb_ts::section::{Long, LongBody, LongHeader};
use isdb_ts::{Error, Result};

/// table_id of a DII section (Vol. 3 Table 6-24).
pub const TABLE_ID_DII: u8 = 0x3B;
/// table_id of a DDB section (Vol. 3 Table 6-24).
pub const TABLE_ID_DDB: u8 = 0x3C;
/// protocolDiscriminator "set to 0x11" (Vol. 3 §6.2.2 / §6.3.2).
pub const PROTOCOL_DISCRIMINATOR: u8 = 0x11;
/// dsmccType "0x03 (U-N download message)" (Vol. 3 §6.2.2 / §6.3.2).
pub const DSMCC_TYPE_UN_DOWNLOAD: u8 = 0x03;
/// messageId of a DII (Vol. 3 §6.2.2).
pub const MESSAGE_ID_DII: u16 = 0x1002;
/// messageId of a DDB (Vol. 3 §6.3.2).
pub const MESSAGE_ID_DDB: u16 = 0x1003;
/// transaction_id bits 31–30 "set to '10' (transaction_id allocated by the
/// Network)" (Vol. 3 §6.2.2 Fig. 6-2); bits 29–0 are the Transaction
/// Number = the DII version.
pub const TRANSACTION_ID_ORIGINATOR_NETWORK: u32 = 0b10 << 30;

/// Bytes of the DDB message that are not block data: dsmcc_section header
/// (8), dsmccDownloadDataHeader without adaptation (12), moduleId (2),
/// moduleVersion (1), reserved (1), blockNumber (2), CRC_32 (4). With the
/// 4096-byte section limit the largest blockSize is 4096 − 30 = 4066.
pub const DDB_OVERHEAD: usize = 30;
/// Largest blockSize that keeps a DDB section within 4096 bytes.
pub const MAX_BLOCK_SIZE: u16 = (4096 - DDB_OVERHEAD) as u16;

/// Transaction Number (bits 29–0) → transaction_id with the network
/// originator bits.
pub fn transaction_id(transaction_number: u32) -> u32 {
    TRANSACTION_ID_ORIGINATOR_NETWORK | (transaction_number & 0x3FFF_FFFF)
}

/// downloadId with `data_event_id` in bits 31–28 (Vol. 3 §6.2.1 Fig. 6-1)
/// and `low` in bits 27–0.
pub fn download_id(data_event_id: u8, low: u32) -> u32 {
    ((data_event_id as u32 & 0xF) << 28) | (low & 0x0FFF_FFFF)
}

/// data_event_id of a downloadId (bits 31–28).
pub fn data_event_id(download_id: u32) -> u8 {
    (download_id >> 28) as u8
}

// ---------------------------------------------------------------------------
// shared header
// ---------------------------------------------------------------------------

/// The common part of dsmccMessageHeader (Table 6-3) and
/// dsmccDownloadDataHeader (Table 6-22) after the 32-bit identifier.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MessageHeaderTail {
    /// reserved (8), spec value 0xFF.
    pub reserved: u8,
    /// dsmccAdaptationHeader() bytes (`adaptationLength` of them; Vol. 3
    /// §6.4: adaptationType then adaptationDataByte). Normally empty.
    pub adaptation: Vec<u8>,
}

impl MessageHeaderTail {
    /// Spec values, no adaptation header.
    pub fn new() -> Self {
        Self {
            reserved: 0xFF,
            adaptation: Vec::new(),
        }
    }
}

/// Read protocolDiscriminator, dsmccType, messageId (checked against
/// `message_id`) and the 32-bit identifier; then reserved,
/// adaptationLength, messageLength and the adaptation header. Returns the
/// identifier, the tail and the payload bytes (messageLength −
/// adaptationLength), which must end the buffer.
fn read_header<'a>(
    r: &mut BitReader<'a>,
    what: &'static str,
    message_id: u16,
) -> Result<(u32, MessageHeaderTail, &'a [u8])> {
    let pd = r.u8()?;
    if pd != PROTOCOL_DISCRIMINATOR {
        return Err(Error::invalid(
            what,
            format!("protocolDiscriminator {pd:#04x}, expected 0x11"),
        ));
    }
    let ty = r.u8()?;
    if ty != DSMCC_TYPE_UN_DOWNLOAD {
        return Err(Error::invalid(
            what,
            format!("dsmccType {ty:#04x}, expected 0x03"),
        ));
    }
    let mid = r.u16()?;
    if mid != message_id {
        return Err(Error::invalid(
            what,
            format!("messageId {mid:#06x}, expected {message_id:#06x}"),
        ));
    }
    let id = r.u32()?;
    let reserved = r.u8()?;
    let adaptation_length = r.u8()? as usize;
    let message_length = r.u16()? as usize;
    if message_length != r.remaining_bytes() {
        return Err(Error::invalid(
            what,
            format!(
                "messageLength {message_length} but {} bytes follow",
                r.remaining_bytes()
            ),
        ));
    }
    if adaptation_length > message_length {
        return Err(Error::invalid(
            what,
            format!("adaptationLength {adaptation_length} > messageLength {message_length}"),
        ));
    }
    let adaptation = r.bytes(adaptation_length)?.to_vec();
    let payload = r.rest()?;
    Ok((
        id,
        MessageHeaderTail {
            reserved,
            adaptation,
        },
        payload,
    ))
}

fn write_header(
    w: &mut BitWriter,
    message_id: u16,
    id: u32,
    tail: &MessageHeaderTail,
    payload: &[u8],
) -> Result<()> {
    w.write("protocolDiscriminator", 8, PROTOCOL_DISCRIMINATOR as u64)?;
    w.write("dsmccType", 8, DSMCC_TYPE_UN_DOWNLOAD as u64)?;
    w.write("messageId", 16, message_id as u64)?;
    w.write("transaction_id/downloadId", 32, id as u64)?;
    w.write("reserved", 8, tail.reserved as u64)?;
    w.write("adaptationLength", 8, tail.adaptation.len() as u64)?;
    w.write(
        "messageLength",
        16,
        (tail.adaptation.len() + payload.len()) as u64,
    )?;
    w.bytes(&tail.adaptation);
    w.bytes(payload);
    Ok(())
}

// ---------------------------------------------------------------------------
// module information / private area descriptors
// ---------------------------------------------------------------------------

/// Tag of the arib_bxml_privatedata_descriptor (Vol. 2 §9.3.4 Table 9-14).
pub const TAG_ARIB_BXML_PRIVATEDATA: u8 = 0xF0;

/// A descriptor of the DII module information area or private area (Vol. 3
/// §6.2.3 Table 6-2 tag ranges: 0x01–0x7F DVB-compatible, 0x80–0xBF
/// broadcaster-defined, 0xC0–0xEF ARIB-defined, 0xF0–0xFF defined by the
/// data coding format).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModuleDescriptor {
    /// 0x01 Type descriptor (§6.2.3.1 Table 6-5): the media type of the
    /// file in the module, "complying with RFC1521 and RFC1590" (for BML:
    /// Vol. 2 Table 9-6, e.g. `text/X-arib-bml; charset="euc-jp"`,
    /// `multipart/mixed`).
    Type {
        /// text_char bytes.
        media_type: Vec<u8>,
    },
    /// 0x02 Name descriptor (§6.2.3.2 Table 6-6): the name of the file
    /// transmitted as a single module.
    Name {
        /// text_char bytes.
        name: Vec<u8>,
    },
    /// 0x03 Info descriptor (§6.2.3.3 Table 6-7).
    Info {
        /// ISO_639_language_code.
        language: [u8; 3],
        /// text_char bytes.
        text: Vec<u8>,
    },
    /// 0x04 Module_link descriptor (§6.2.3.4 Table 6-8).
    ModuleLink {
        /// position: 0x00 head, 0x01 middle, 0x02 end.
        position: u8,
        /// moduleId of the next module (ignored when position = 0x02).
        module_id: u16,
    },
    /// 0x05 CRC32 descriptor (§6.2.3.5 Table 6-9): CRC_32 of the module.
    Crc32 {
        /// CRC_32.
        crc: u32,
    },
    /// 0xC2 CompressionType descriptor (§6.2.3.9 Table 6-13).
    CompressionType {
        /// compression_type ("specified in an operational guideline").
        compression_type: u8,
        /// original_size (bytes before compression).
        original_size: u32,
    },
    /// 0xC3 Control descriptor (§6.2.3.10 Table 6-14): control_data_byte,
    /// whose structure the data coding format defines (BML: Vol. 2
    /// Table 9-15, [`crate::bxml::BmlControl`]).
    Control {
        /// control_data_byte.
        data: Vec<u8>,
    },
    /// 0xC8 DataEncoding descriptor (§6.2.3.15 Table 6-19).
    DataEncoding {
        /// data_component_id (as in the PMT data component descriptor).
        data_component_id: u16,
        /// additional_data_encoding_info.
        info: Vec<u8>,
    },
    /// 0xF0 arib_bxml_privatedata_descriptor (Vol. 2 §9.3.4 Table 9-14),
    /// DII private area of a BML carousel.
    AribBxmlPrivateData {
        /// return_to_entry_flag: "If this flag is 1, a BML document which
        /// is destined to be invoked before any other document in this
        /// component is forced to be invoked first on an update of data
        /// event".
        return_to_entry: bool,
        /// Reserved (7), kept verbatim.
        reserved: u8,
    },
    /// Any other tag, kept raw.
    Unknown {
        /// descriptor_tag.
        tag: u8,
        /// descriptor body.
        data: Vec<u8>,
    },
}

impl ModuleDescriptor {
    /// descriptor_tag.
    pub fn tag(&self) -> u8 {
        match self {
            ModuleDescriptor::Type { .. } => 0x01,
            ModuleDescriptor::Name { .. } => 0x02,
            ModuleDescriptor::Info { .. } => 0x03,
            ModuleDescriptor::ModuleLink { .. } => 0x04,
            ModuleDescriptor::Crc32 { .. } => 0x05,
            ModuleDescriptor::CompressionType { .. } => 0xC2,
            ModuleDescriptor::Control { .. } => 0xC3,
            ModuleDescriptor::DataEncoding { .. } => 0xC8,
            ModuleDescriptor::AribBxmlPrivateData { .. } => TAG_ARIB_BXML_PRIVATEDATA,
            ModuleDescriptor::Unknown { tag, .. } => *tag,
        }
    }

    /// Decode one body given its tag.
    pub fn decode_body(tag: u8, data: &[u8]) -> Result<Self> {
        const WHAT: &str = "DII module descriptor";
        let mut r = BitReader::new(data, WHAT);
        let d = match tag {
            0x01 => ModuleDescriptor::Type {
                media_type: r.rest()?.to_vec(),
            },
            0x02 => ModuleDescriptor::Name {
                name: r.rest()?.to_vec(),
            },
            0x03 => {
                let l = r.bytes(3)?;
                ModuleDescriptor::Info {
                    language: [l[0], l[1], l[2]],
                    text: r.rest()?.to_vec(),
                }
            }
            0x04 => ModuleDescriptor::ModuleLink {
                position: r.u8()?,
                module_id: r.u16()?,
            },
            0x05 => ModuleDescriptor::Crc32 { crc: r.u32()? },
            0xC2 => ModuleDescriptor::CompressionType {
                compression_type: r.u8()?,
                original_size: r.u32()?,
            },
            0xC3 => ModuleDescriptor::Control {
                data: r.rest()?.to_vec(),
            },
            0xC8 => ModuleDescriptor::DataEncoding {
                data_component_id: r.u16()?,
                info: r.rest()?.to_vec(),
            },
            TAG_ARIB_BXML_PRIVATEDATA => ModuleDescriptor::AribBxmlPrivateData {
                return_to_entry: r.bool()?,
                reserved: r.read(7)? as u8,
            },
            _ => {
                let data = r.rest()?.to_vec();
                ModuleDescriptor::Unknown { tag, data }
            }
        };
        r.finish()?;
        Ok(d)
    }

    fn encode_body(&self, out: &mut Vec<u8>) -> Result<()> {
        match self {
            ModuleDescriptor::Type { media_type } => out.extend_from_slice(media_type),
            ModuleDescriptor::Name { name } => out.extend_from_slice(name),
            ModuleDescriptor::Info { language, text } => {
                out.extend_from_slice(language);
                out.extend_from_slice(text);
            }
            ModuleDescriptor::ModuleLink {
                position,
                module_id,
            } => {
                out.push(*position);
                out.extend_from_slice(&module_id.to_be_bytes());
            }
            ModuleDescriptor::Crc32 { crc } => out.extend_from_slice(&crc.to_be_bytes()),
            ModuleDescriptor::CompressionType {
                compression_type,
                original_size,
            } => {
                out.push(*compression_type);
                out.extend_from_slice(&original_size.to_be_bytes());
            }
            ModuleDescriptor::Control { data } => out.extend_from_slice(data),
            ModuleDescriptor::DataEncoding {
                data_component_id,
                info,
            } => {
                out.extend_from_slice(&data_component_id.to_be_bytes());
                out.extend_from_slice(info);
            }
            ModuleDescriptor::AribBxmlPrivateData {
                return_to_entry,
                reserved,
            } => {
                let mut w = BitWriter::new();
                w.bool(*return_to_entry);
                w.write("reserved", 7, *reserved as u64)?;
                out.extend_from_slice(&w.into_bytes());
            }
            ModuleDescriptor::Unknown { data, .. } => out.extend_from_slice(data),
        }
        Ok(())
    }

    /// Append tag, length and body.
    pub fn encode(&self, out: &mut Vec<u8>) -> Result<()> {
        let mut body = Vec::new();
        self.encode_body(&mut body)?;
        frame(self.tag(), &body, out)
    }
}

/// Append `tag`, `descriptor_length` and `body` (fails above 255 bytes).
pub(crate) fn frame(tag: u8, body: &[u8], out: &mut Vec<u8>) -> Result<()> {
    if body.len() > 255 {
        return Err(Error::Overflow {
            what: "descriptor_length",
            value: body.len() as u64,
            bits: 8,
        });
    }
    out.push(tag);
    out.push(body.len() as u8);
    out.extend_from_slice(body);
    Ok(())
}

/// Split a descriptor loop that exactly fills `bytes` into (tag, body).
pub(crate) fn split_loop<'a>(bytes: &'a [u8], what: &'static str) -> Result<Vec<(u8, &'a [u8])>> {
    let mut v = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes.len() - i < 2 {
            return Err(Error::Truncated {
                what,
                need: i + 2,
                have: bytes.len(),
            });
        }
        let tag = bytes[i];
        let len = bytes[i + 1] as usize;
        let start = i + 2;
        if start + len > bytes.len() {
            return Err(Error::Truncated {
                what,
                need: start + len,
                have: bytes.len(),
            });
        }
        v.push((tag, &bytes[start..start + len]));
        i = start + len;
    }
    Ok(v)
}

/// Decode a module descriptor loop.
pub fn decode_module_descriptors(bytes: &[u8]) -> Result<Vec<ModuleDescriptor>> {
    split_loop(bytes, "DII descriptor loop")?
        .into_iter()
        .map(|(t, b)| ModuleDescriptor::decode_body(t, b))
        .collect()
}

/// Encode a module descriptor loop.
pub fn encode_module_descriptors(ds: &[ModuleDescriptor]) -> Result<Vec<u8>> {
    let mut v = Vec::new();
    for d in ds {
        d.encode(&mut v)?;
    }
    Ok(v)
}

// ---------------------------------------------------------------------------
// DII
// ---------------------------------------------------------------------------

/// One module entry of the DII module loop (Vol. 3 Table 6-1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiiModule {
    /// moduleId.
    pub module_id: u16,
    /// moduleSize (bytes; 0 when unknown).
    pub module_size: u32,
    /// moduleVersion.
    pub module_version: u8,
    /// moduleInfoByte descriptors (moduleInfoLength implied).
    pub info: Vec<ModuleDescriptor>,
}

/// Body of a DownloadInfoIndication (Vol. 3 §6.2 Table 6-1 with the
/// dsmccMessageHeader of Table 6-3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiiBody {
    /// transaction_id (Fig. 6-2: bits 31–30 '10', bits 29–0 the version).
    pub transaction_id: u32,
    /// reserved / adaptation header of dsmccMessageHeader.
    pub header: MessageHeaderTail,
    /// downloadId (Fig. 6-1: data_event_id in bits 31–28).
    pub download_id: u32,
    /// blockSize: bytes of every block but the last of a module.
    pub block_size: u16,
    /// windowSize ("not used … set to 0").
    pub window_size: u8,
    /// ackPeriod ("not used … set to 0").
    pub ack_period: u8,
    /// tCDownloadWindow ("not used … set to 0").
    pub t_c_download_window: u32,
    /// tCDownloadScenario: download timeout in microseconds.
    pub t_c_download_scenario: u32,
    /// compatibilityDescriptor() after its compatibilityDescriptorLength:
    /// ISO/IEC 13818-6 structure; B24: "When the content … is not needed,
    /// descriptorCount is set to 0x0000 and then the field length is
    /// 4-byte" ([`DiiBody::EMPTY_COMPATIBILITY`]).
    pub compatibility: Vec<u8>,
    /// Module loop (numberOfModules implied).
    pub modules: Vec<DiiModule>,
    /// privateDataByte descriptors (privateDataLength implied).
    pub private: Vec<ModuleDescriptor>,
}

impl DiiBody {
    /// compatibilityDescriptor() content without descriptors: the
    /// descriptorCount 0x0000 (the whole structure is then 4 bytes with
    /// its length field).
    pub const EMPTY_COMPATIBILITY: [u8; 2] = [0x00, 0x00];

    /// A DII with the spec constants (window/ack/download window 0, no
    /// compatibility descriptors, no adaptation header).
    pub fn new(transaction_id: u32, download_id: u32, block_size: u16) -> Self {
        Self {
            transaction_id,
            header: MessageHeaderTail::new(),
            download_id,
            block_size,
            window_size: 0,
            ack_period: 0,
            t_c_download_window: 0,
            t_c_download_scenario: 0,
            compatibility: Self::EMPTY_COMPATIBILITY.to_vec(),
            modules: Vec::new(),
            private: Vec::new(),
        }
    }

    /// The DII section: table_id 0x3B, table_id_extension = transaction_id
    /// bits 15–0, version_number 0, private_indicator 0 (Table 6-24).
    pub fn section(self) -> Dii {
        let mut h = LongHeader::new(TABLE_ID_DII, self.transaction_id as u16, 0);
        h.private_indicator = false;
        Long {
            header: h,
            body: self,
        }
    }
}

impl LongBody for DiiBody {
    fn accepts_table_id(t: u8) -> bool {
        t == TABLE_ID_DII
    }
    fn decode(_: &LongHeader, body: &[u8]) -> Result<Self> {
        const WHAT: &str = "DownloadInfoIndication";
        let mut r = BitReader::new(body, WHAT);
        let (transaction_id, header, payload) = read_header(&mut r, WHAT, MESSAGE_ID_DII)?;
        let mut r = BitReader::new(payload, WHAT);
        let download_id = r.u32()?;
        let block_size = r.u16()?;
        let window_size = r.u8()?;
        let ack_period = r.u8()?;
        let t_c_download_window = r.u32()?;
        let t_c_download_scenario = r.u32()?;
        let cl = r.u16()? as usize;
        let compatibility = r.bytes(cl)?.to_vec();
        let n = r.u16()? as usize;
        let mut modules = Vec::with_capacity(n);
        for _ in 0..n {
            let module_id = r.u16()?;
            let module_size = r.u32()?;
            let module_version = r.u8()?;
            let il = r.u8()? as usize;
            let info = decode_module_descriptors(r.bytes(il)?)?;
            modules.push(DiiModule {
                module_id,
                module_size,
                module_version,
                info,
            });
        }
        let pl = r.u16()? as usize;
        let private = decode_module_descriptors(r.bytes(pl)?)?;
        r.finish()?;
        Ok(Self {
            transaction_id,
            header,
            download_id,
            block_size,
            window_size,
            ack_period,
            t_c_download_window,
            t_c_download_scenario,
            compatibility,
            modules,
            private,
        })
    }
    fn encode(&self, out: &mut Vec<u8>) -> Result<()> {
        let mut p = BitWriter::new();
        p.write("downloadId", 32, self.download_id as u64)?;
        p.write("blockSize", 16, self.block_size as u64)?;
        p.write("windowSize", 8, self.window_size as u64)?;
        p.write("ackPeriod", 8, self.ack_period as u64)?;
        p.write("tCDownloadWindow", 32, self.t_c_download_window as u64)?;
        p.write("tCDownloadScenario", 32, self.t_c_download_scenario as u64)?;
        p.write(
            "compatibilityDescriptorLength",
            16,
            self.compatibility.len() as u64,
        )?;
        p.bytes(&self.compatibility);
        p.write("numberOfModules", 16, self.modules.len() as u64)?;
        for m in &self.modules {
            p.write("moduleId", 16, m.module_id as u64)?;
            p.write("moduleSize", 32, m.module_size as u64)?;
            p.write("moduleVersion", 8, m.module_version as u64)?;
            let info = encode_module_descriptors(&m.info)?;
            p.write("moduleInfoLength", 8, info.len() as u64)?;
            p.bytes(&info);
        }
        let private = encode_module_descriptors(&self.private)?;
        p.write("privateDataLength", 16, private.len() as u64)?;
        p.bytes(&private);
        let mut w = BitWriter::new();
        write_header(
            &mut w,
            MESSAGE_ID_DII,
            self.transaction_id,
            &self.header,
            &p.into_bytes(),
        )?;
        out.extend_from_slice(&w.into_bytes());
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// DDB
// ---------------------------------------------------------------------------

/// Body of a DownloadDataBlock (Vol. 3 §6.3 Tables 6-21 and 6-22).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DdbBody {
    /// downloadId ("the same value as the download identifier in the
    /// corresponding DII message").
    pub download_id: u32,
    /// reserved / adaptation header of dsmccDownloadDataHeader.
    pub header: MessageHeaderTail,
    /// moduleId.
    pub module_id: u16,
    /// moduleVersion.
    pub module_version: u8,
    /// reserved (8), spec value 0xFF.
    pub reserved: u8,
    /// blockNumber ("The first block of a module must be represented by
    /// the block number 0").
    pub block_number: u16,
    /// blockDataByte (blockSize bytes, the last block of a module may be
    /// shorter).
    pub data: Vec<u8>,
}

impl DdbBody {
    /// A block with the spec reserved values and no adaptation header.
    pub fn new(
        download_id: u32,
        module_id: u16,
        module_version: u8,
        block_number: u16,
        data: Vec<u8>,
    ) -> Self {
        Self {
            download_id,
            header: MessageHeaderTail::new(),
            module_id,
            module_version,
            reserved: 0xFF,
            block_number,
            data,
        }
    }

    /// The DDB section (Table 6-24): table_id 0x3C, table_id_extension =
    /// moduleId, version_number = moduleVersion bits 4–0, section_number =
    /// blockNumber bits 7–0, last_section_number = `last_block` bits 7–0,
    /// private_indicator 0.
    pub fn section(self, last_block: u16) -> Ddb {
        let mut h = LongHeader::new(TABLE_ID_DDB, self.module_id, self.module_version & 0x1F);
        h.private_indicator = false;
        h.section_number = self.block_number as u8;
        h.last_section_number = last_block as u8;
        Long {
            header: h,
            body: self,
        }
    }
}

impl LongBody for DdbBody {
    fn accepts_table_id(t: u8) -> bool {
        t == TABLE_ID_DDB
    }
    fn decode(_: &LongHeader, body: &[u8]) -> Result<Self> {
        const WHAT: &str = "DownloadDataBlock";
        let mut r = BitReader::new(body, WHAT);
        let (download_id, header, payload) = read_header(&mut r, WHAT, MESSAGE_ID_DDB)?;
        let mut r = BitReader::new(payload, WHAT);
        let module_id = r.u16()?;
        let module_version = r.u8()?;
        let reserved = r.u8()?;
        let block_number = r.u16()?;
        let data = r.rest()?.to_vec();
        Ok(Self {
            download_id,
            header,
            module_id,
            module_version,
            reserved,
            block_number,
            data,
        })
    }
    fn encode(&self, out: &mut Vec<u8>) -> Result<()> {
        let mut p = Vec::with_capacity(6 + self.data.len());
        p.extend_from_slice(&self.module_id.to_be_bytes());
        p.push(self.module_version);
        p.push(self.reserved);
        p.extend_from_slice(&self.block_number.to_be_bytes());
        p.extend_from_slice(&self.data);
        let mut w = BitWriter::new();
        write_header(&mut w, MESSAGE_ID_DDB, self.download_id, &self.header, &p)?;
        out.extend_from_slice(&w.into_bytes());
        Ok(())
    }
}

/// DownloadInfoIndication section (table_id 0x3B).
pub type Dii = Long<DiiBody>;
/// DownloadDataBlock section (table_id 0x3C).
pub type Ddb = Long<DdbBody>;

/// Split module bytes into DDB sections of `block_size` (Vol. 3 §6.3.1:
/// "A module is divided into data blocks with a fixed length"; the last
/// block may be shorter; an empty module gives one empty block). Fails
/// when the module needs more than 256 blocks: the DDB `section_number`
/// carries only the low 8 bits of the block number, so a larger module
/// would repeat section numbers within one sub-table.
pub fn module_blocks(
    download_id: u32,
    module_id: u16,
    module_version: u8,
    block_size: u16,
    module: &[u8],
) -> Result<Vec<Ddb>> {
    if block_size == 0 {
        return Err(Error::invalid("blockSize", "0"));
    }
    let n = module.len().div_ceil(block_size as usize).max(1);
    if n > 256 {
        return Err(Error::invalid(
            "module",
            format!(
                "{} bytes need {n} blocks of {block_size}; the DDB section_number is 8 bits, so a module is limited to 256 blocks ({} bytes)",
                module.len(),
                256 * block_size as usize
            ),
        ));
    }
    let last = (n - 1) as u16;
    let mut v = Vec::with_capacity(n);
    if module.is_empty() {
        v.push(DdbBody::new(download_id, module_id, module_version, 0, Vec::new()).section(0));
        return Ok(v);
    }
    for (i, chunk) in module.chunks(block_size as usize).enumerate() {
        v.push(
            DdbBody::new(
                download_id,
                module_id,
                module_version,
                i as u16,
                chunk.to_vec(),
            )
            .section(last),
        );
    }
    Ok(v)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dii_roundtrip_and_header_values() {
        let mut dii = DiiBody::new(transaction_id(5), download_id(3, 0x1234), 4066);
        dii.t_c_download_scenario = 2_000_000;
        dii.modules.push(DiiModule {
            module_id: 0,
            module_size: 100,
            module_version: 1,
            info: vec![ModuleDescriptor::Type {
                media_type: b"multipart/mixed".to_vec(),
            }],
        });
        dii.private.push(ModuleDescriptor::AribBxmlPrivateData {
            return_to_entry: true,
            reserved: 0x7F,
        });
        let sec = dii.clone().section();
        assert_eq!(sec.header.table_id, 0x3B);
        assert_eq!(sec.header.table_id_extension, 0x0005);
        assert!(!sec.header.private_indicator);
        assert_eq!(sec.header.version_number, 0);
        let bytes = sec.encode().unwrap();
        // dsmccMessageHeader after the 8-byte section header
        assert_eq!(&bytes[8..12], &[0x11, 0x03, 0x10, 0x02]);
        assert_eq!(&bytes[12..16], &0x8000_0005u32.to_be_bytes());
        assert_eq!(bytes[16], 0xFF);
        assert_eq!(bytes[17], 0);
        let message_length = u16::from_be_bytes([bytes[18], bytes[19]]) as usize;
        assert_eq!(message_length, bytes.len() - 20 - 4);
        let back = Dii::decode(&bytes).unwrap();
        assert_eq!(back.body, dii);
        assert_eq!(back.encode().unwrap(), bytes);
        assert_eq!(data_event_id(back.body.download_id), 3);
    }

    #[test]
    fn ddb_blocks() {
        let module: Vec<u8> = (0..10_000u32).map(|i| i as u8).collect();
        let blocks = module_blocks(7, 0x0001, 2, 4066, &module).unwrap();
        assert_eq!(blocks.len(), 3);
        assert_eq!(blocks[2].body.data.len(), 10_000 - 2 * 4066);
        for (i, b) in blocks.iter().enumerate() {
            assert_eq!(b.header.table_id, 0x3C);
            assert_eq!(b.header.table_id_extension, 0x0001);
            assert_eq!(b.header.version_number, 2);
            assert_eq!(b.header.section_number, i as u8);
            assert_eq!(b.header.last_section_number, 2);
            let bytes = b.encode().unwrap();
            assert!(bytes.len() <= 4096);
            assert_eq!(&bytes[8..12], &[0x11, 0x03, 0x10, 0x03]);
            assert_eq!(Ddb::decode(&bytes).unwrap(), *b);
        }
        assert_eq!(blocks[0].encode().unwrap().len(), 4096);
        assert!(module_blocks(7, 1, 0, 10, &vec![0u8; 2561]).is_err());
        assert_eq!(module_blocks(7, 1, 0, 10, &[]).unwrap().len(), 1);
    }

    #[test]
    fn bad_message_length_is_rejected() {
        let bytes = DiiBody::new(transaction_id(0), 0, 100)
            .section()
            .encode()
            .unwrap();
        let mut bad = bytes.clone();
        bad[19] ^= 1;
        let n = bad.len();
        let c = isdb_ts::crc::crc32(&bad[..n - 4]);
        bad[n - 4..].copy_from_slice(&c.to_be_bytes());
        assert!(Dii::decode(&bad).is_err());
    }
}
