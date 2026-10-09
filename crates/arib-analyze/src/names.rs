//! Names and allocation tables quoted from the specs: stream_type
//! (H.222.0 Table 2-34), table_id (ARIB STD-B10 Part 1 Table 5-2), PID
//! allocation (H.222.0 Table 2-3, B10 Part 1 Table 5-1 / Part 2 Table 5-1)
//! and transmission level / frequency (B10 Part 2 Table 5-2).

/// stream_type description (H.222.0 (05/2006) Table 2-34).
pub fn stream_type_name(t: u8) -> &'static str {
    match t {
        0x00 => "reserved",
        0x01 => "ISO/IEC 11172-2 video",
        0x02 => "H.262 | 13818-2 video",
        0x03 => "ISO/IEC 11172-3 audio",
        0x04 => "ISO/IEC 13818-3 audio",
        0x05 => "H.222.0 private_sections",
        0x06 => "PES private data",
        0x07 => "ISO/IEC 13522 MHEG",
        0x08 => "H.222.0 Annex A DSM-CC",
        0x09 => "H.222.1",
        0x0A => "13818-6 type A",
        0x0B => "13818-6 type B",
        0x0C => "13818-6 type C",
        0x0D => "13818-6 type D",
        0x0E => "H.222.0 auxiliary",
        0x0F => "13818-7 AAC (ADTS)",
        0x10 => "14496-2 visual",
        0x11 => "14496-3 audio (LATM)",
        0x12 => "14496-1 SL/FlexMux in PES",
        0x13 => "14496-1 SL/FlexMux in sections",
        0x14 => "13818-6 synchronized download",
        0x15 => "metadata in PES",
        0x16 => "metadata in sections",
        0x17 => "metadata in data carousel",
        0x18 => "metadata in object carousel",
        0x19 => "metadata in SDP",
        0x1A => "IPMP (13818-11)",
        0x1B => "H.264 | 14496-10 AVC video",
        0x1C..=0x7E => "reserved",
        0x7F => "IPMP stream",
        _ => "user private",
    }
}

/// Whether elementary streams of `stream_type` carry sections rather than
/// PES packets (H.222.0 Table 2-34: private_sections, 13818-6 types A–D
/// (DSM-CC sections), 14496 sections, metadata sections).
pub fn stream_type_is_sections(t: u8) -> bool {
    matches!(t, 0x05 | 0x0A | 0x0B | 0x0C | 0x0D | 0x13 | 0x16)
}

/// Video stream types (H.222.0 Table 2-34).
pub fn stream_type_is_video(t: u8) -> bool {
    matches!(t, 0x01 | 0x02 | 0x10 | 0x1B)
}

/// Audio stream types (H.222.0 Table 2-34).
pub fn stream_type_is_audio(t: u8) -> bool {
    matches!(t, 0x03 | 0x04 | 0x0F | 0x11)
}

/// Table name for a table_id (B10 Part 1 Table 5-2; H.222.0 Table 2-31).
pub fn table_name(tid: u8) -> &'static str {
    match tid {
        0x00 => "PAT",
        0x01 => "CAT",
        0x02 => "PMT",
        0x03 => "TSDT",
        0x3A..=0x3F => "DSM-CC",
        0x40 => "NIT actual",
        0x41 => "NIT other",
        0x42 => "SDT actual",
        0x46 => "SDT other",
        0x4A => "BAT",
        0x4C => "INT",
        0x4E => "EIT p/f actual",
        0x4F => "EIT p/f other",
        0x50..=0x5F => "EIT schedule actual",
        0x60..=0x6F => "EIT schedule other",
        0x70 => "TDT",
        0x71 => "RST",
        0x72 => "ST",
        0x73 => "TOT",
        0x74 => "AIT",
        0x75 => "AIT (ARIB-J)",
        0x7E => "DIT",
        0x7F => "SIT",
        0x82 | 0x83 => "ECM",
        0x84 | 0x85 => "EMM",
        0x87 | 0x88 => "DCM",
        0x89 | 0x8A => "DMM",
        0x90..=0xBF => "company table",
        0xC0 => "DCT",
        0xC1 => "DLT",
        0xC2 => "PCAT",
        0xC3 => "SDTT",
        0xC4 => "BIT",
        0xC5 => "NBIT body",
        0xC6 => "NBIT reference",
        0xC7 => "LDT",
        0xC8 => "CDT",
        0xD0 => "LIT",
        0xD1 => "ERT",
        0xD2 => "ITT",
        0xFE => "AMT",
        0xFF => "forbidden",
        _ => "unassigned",
    }
}

/// Fixed PIDs of the tables (B10 Part 1 §5.1 Table 5-1, H.222.0 Table 2-3).
/// `None` when the table's PID is designated indirectly (PMT, ECM, EMM,
/// DSM-CC, DLT, ITT, AIT, INT, DCM, DMM, company tables) or ST
/// ("Exclude 0x0000, 0x0001, 0x0014"), which is handled separately.
pub fn fixed_pids(tid: u8) -> Option<&'static [u16]> {
    Some(match tid {
        0x00 => &[0x0000],
        0x01 => &[0x0001],
        0x40 | 0x41 => &[0x0010],
        0x42 | 0x46 | 0x4A => &[0x0011],
        0x4E..=0x6F => &[0x0012, 0x0026, 0x0027],
        0x71 => &[0x0013],
        0x70 | 0x73 => &[0x0014],
        0x7E => &[0x001E],
        0x7F => &[0x001F],
        0xC0 => &[0x0017],
        0xC2 => &[0x0022],
        0xC3 => &[0x0023, 0x0028],
        0xC4 => &[0x0024],
        0xC5..=0xC7 => &[0x0025],
        0xC8 => &[0x0029],
        0xFE => &[0x002E],
        _ => return None,
    })
}

/// Name of a PID allocated by H.222.0 Table 2-3 or B10 Part 1 Table 5-1.
pub fn pid_allocation(pid: u16) -> Option<&'static str> {
    Some(match pid {
        0x0000 => "PAT",
        0x0001 => "CAT",
        0x0002 => "TSDT",
        0x0003 => "IPMP control information",
        0x0004..=0x000F => "reserved (H.222.0 Table 2-3)",
        0x0010 => "NIT",
        0x0011 => "SDT/BAT",
        0x0012 => "EIT",
        0x0013 => "RST",
        0x0014 => "TDT/TOT",
        0x0017 => "DCT",
        0x001E => "DIT",
        0x001F => "SIT",
        0x0020 => "LIT (program group index)",
        0x0021 => "ERT (program group index)",
        0x0022 => "PCAT",
        0x0023 => "SDTT",
        0x0024 => "BIT",
        0x0025 => "NBIT/LDT",
        0x0026 | 0x0027 => "EIT (terrestrial)",
        0x0028 => "SDTT (terrestrial)",
        0x0029 => "CDT",
        0x002D => "fragmented TLV packet",
        0x002E => "AMT",
        0x002F => "multiple frame header information",
        0x1FFF => "null",
        _ => return None,
    })
}

/// Transmission level of B10 Part 2 Table 5-2.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum Level {
    /// "Mandatory".
    Mandatory,
    /// "Mandatory *2" — "Transmitting either TDT or TOT is mandatory".
    MandatoryTdtOrTot,
    /// "Optional".
    Optional,
}

/// Row of B10 Part 2 Table 5-2: transmission level and frequency criterion
/// ("Once or more/N sec."; `None` = "Optional").  "The value specified as
/// sending frequency in table 5-2 is only a criterion of operation and is
/// not the standard value."
#[derive(Debug, Clone, Copy, serde::Serialize)]
pub struct Criterion {
    /// Transmission level.
    pub level: Level,
    /// Maximum interval in seconds, if a frequency is given.
    pub max_interval_s: Option<f64>,
    /// Text of the frequency column.
    pub text: &'static str,
}

/// B10 Part 2 Table 5-2 row for `tid`.  EIT schedule: each table_id spans
/// 32 segments × 3 h = 4 days (B10 Appendix §1.4.2.1 b/c), so 0x50/0x51
/// (0x60/0x61) are "within 8 days" and the others "after 8 days" (see
/// SPEC-NOTES.md).
pub fn criterion(tid: u8) -> Option<Criterion> {
    use Level::*;
    let c = |level, s: Option<f64>, text| Some(Criterion { level, max_interval_s: s, text });
    match tid {
        0x00 => c(Mandatory, Some(0.1), "Once or more/100m sec."),
        0x01 => c(Mandatory, Some(1.0), "Once or more/1 sec."),
        0x02 => c(Mandatory, Some(0.1), "Once or more/100m sec."),
        0x40 => c(Mandatory, Some(10.0), "Once or more/10 sec."),
        0x41 => c(Optional, Some(10.0), "Once or more/10 sec."),
        0x42 => c(Mandatory, Some(2.0), "Once or more/2 sec."),
        0x46 => c(Optional, Some(10.0), "Once or more/10 sec."),
        0x4A => c(Optional, Some(10.0), "Once or more/10 sec."),
        0x4C => c(Optional, Some(30.0), "Once or more/30 sec."),
        0x4E => c(Mandatory, Some(2.0), "Once or more/2 sec."),
        0x4F => c(Optional, Some(10.0), "Once or more/10 sec."),
        0x50 | 0x51 | 0x60 | 0x61 => c(Optional, Some(10.0), "Once or more/10 sec. (within 8 days)"),
        0x52..=0x5F | 0x62..=0x6F => c(Optional, Some(30.0), "Once or more/30 sec. (after 8 days)"),
        0x70 => c(MandatoryTdtOrTot, Some(30.0), "Once or more/30 sec."),
        0x71 | 0x72 | 0xC2 => c(Optional, None, "Optional"),
        0x73 => c(MandatoryTdtOrTot, Some(30.0), "Once or more/30 sec."),
        0xC4 => c(Optional, Some(20.0), "Once or more/20 sec."),
        0xC5 => c(Optional, Some(20.0), "Once or more/20 sec."),
        0xC6 => c(Optional, Some(10.0), "1 sec. or more 10 sec."),
        0xC7 => c(Optional, Some(20.0), "1 sec. or more 20 sec."),
        0xFE => c(Optional, Some(10.0), "Once or more/10 sec."),
        _ => None,
    }
}

/// Guard interval (B10 Part 2 Table 6-62).
pub fn guard_interval_name(g: u8) -> &'static str {
    match g {
        0 => "1/32",
        1 => "1/16",
        2 => "1/8",
        _ => "1/4",
    }
}

/// Mode information (B10 Part 2 Table 6-63).
pub fn transmission_mode_name(m: u8) -> &'static str {
    match m {
        0 => "Mode 1",
        1 => "Mode 2",
        2 => "Mode 3",
        _ => "undefined",
    }
}

/// SDT/EIT running_status (B10 Part 2 Table 5-6).
pub fn running_status_name(r: u8) -> &'static str {
    match r {
        0 => "undefined",
        1 => "not running",
        2 => "starts in a few seconds",
        3 => "pausing",
        4 => "running",
        _ => "reserved",
    }
}
