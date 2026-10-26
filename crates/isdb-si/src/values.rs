//! Typed values of descriptor fields whose codes ARIB STD-B10 defines in a
//! table: every variant maps to exactly the code printed in the cited
//! table, and every code decodes (codes the table reserves land in a
//! `Reserved` / `Extension` variant that keeps the value), so the typed
//! view is as lossless as the raw integer fields it accompanies.
//!
//! The descriptor structs keep their raw integer fields (lossless codecs);
//! the typed views are additional accessors on them (e.g.
//! [`crate::descriptor::VideoDecodeControl::encode_format`]).
//!
//! Each enum implements [`CodedValue`]: `code()` / `from_code()`, a stable
//! short `name()` (used by configuration files and reports) and the
//! table's description text.

/// A field value coded per a B10 table.
pub trait CodedValue: Copy + Eq + Sized + 'static {
    /// Field name as printed in the syntax table.
    const FIELD: &'static str;
    /// Clause and table defining the codes.
    const TABLE: &'static str;
    /// Width of the field in bits.
    const BITS: u32;
    /// Every value the table gives a meaning to, in code order (reserved
    /// codes are not listed).
    const NAMED: &'static [Self];
    /// The code (right-aligned in a `u8`).
    fn code(self) -> u8;
    /// The value of `code` (masked to [`Self::BITS`]).
    fn from_code(code: u8) -> Self;
    /// Stable short name, e.g. `"copy-one-generation"`; reserved codes
    /// give `"reserved"`.
    fn name(self) -> &'static str;
    /// The table's description text for this code.
    fn description(self) -> &'static str;
    /// The named value called `name` (exact match).
    fn from_name(name: &str) -> Option<Self> {
        Self::NAMED.iter().copied().find(|v| v.name() == name)
    }
}

macro_rules! coded_enum {
    // Table with reserved codes: they decode into `$res(code)`.
    (
        $(#[$m:meta])*
        pub enum $name:ident: $field:literal, $table:literal, $bits:literal bits {
            $( $(#[$vm:meta])* $var:ident = $code:literal => $key:literal, $desc:literal; )*
        }
        reserved $(#[$rm:meta])* $res:ident => $resdesc:literal;
    ) => {
        $(#[$m])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        pub enum $name {
            $( $(#[$vm])* $var, )*
            $(#[$rm])*
            $res(u8),
        }
        impl CodedValue for $name {
            const FIELD: &'static str = $field;
            const TABLE: &'static str = $table;
            const BITS: u32 = $bits;
            const NAMED: &'static [Self] = &[$($name::$var),*];
            fn code(self) -> u8 {
                match self {
                    $( $name::$var => $code, )*
                    $name::$res(c) => c & ((1u16 << $bits) - 1) as u8,
                }
            }
            fn from_code(code: u8) -> Self {
                match code & ((1u16 << $bits) - 1) as u8 {
                    $( $code => $name::$var, )*
                    c => $name::$res(c),
                }
            }
            fn name(self) -> &'static str {
                match self {
                    $( $name::$var => $key, )*
                    $name::$res(_) => "reserved",
                }
            }
            fn description(self) -> &'static str {
                match self {
                    $( $name::$var => $desc, )*
                    $name::$res(_) => $resdesc,
                }
            }
        }
        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(f, "{} ({:#04x}: {})", self.name(), self.code(), self.description())
            }
        }
    };
    // Table defining every code of the field.
    (
        $(#[$m:meta])*
        pub enum $name:ident: $field:literal, $table:literal, $bits:literal bits {
            $( $(#[$vm:meta])* $var:ident = $code:literal => $key:literal, $desc:literal; )*
        }
    ) => {
        $(#[$m])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        pub enum $name {
            $( $(#[$vm])* $var, )*
        }
        impl CodedValue for $name {
            const FIELD: &'static str = $field;
            const TABLE: &'static str = $table;
            const BITS: u32 = $bits;
            const NAMED: &'static [Self] = &[$($name::$var),*];
            fn code(self) -> u8 {
                match self {
                    $( $name::$var => $code, )*
                }
            }
            fn from_code(code: u8) -> Self {
                // Every code of the masked width is listed (checked by the
                // `complete_tables` test), so the fallback is never taken;
                // it returns the first entry instead of panicking.
                match code & ((1u16 << $bits) - 1) as u8 {
                    $( $code => $name::$var, )*
                    _ => Self::NAMED[0],
                }
            }
            fn name(self) -> &'static str {
                match self {
                    $( $name::$var => $key, )*
                }
            }
            fn description(self) -> &'static str {
                match self {
                    $( $name::$var => $desc, )*
                }
            }
        }
        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(f, "{} ({:#04x}: {})", self.name(), self.code(), self.description())
            }
        }
    };
}

coded_enum! {
    /// video_encode_format of the video decode control descriptor — ARIB
    /// STD-B10 Part 2 §6.2.30, Table 6-60 "Video encode format".
    pub enum VideoEncodeFormat: "video_encode_format", "B10 §6.2.30 Table 6-60", 4 bits {
        /// '0000' 1080/P.
        P1080 = 0b0000 => "1080p", "1080/P";
        /// '0001' 1080/I.
        I1080 = 0b0001 => "1080i", "1080/I";
        /// '0010' 720/P.
        P720 = 0b0010 => "720p", "720/P";
        /// '0011' 480/P.
        P480 = 0b0011 => "480p", "480/P";
        /// '0100' 480/I.
        I480 = 0b0100 => "480i", "480/I";
        /// '0101' 240/P.
        P240 = 0b0101 => "240p", "240/P";
        /// '0110' 120/P.
        P120 = 0b0110 => "120p", "120/P";
        /// '0111' 2160/60/P.
        P2160x60 = 0b0111 => "2160p60", "2160/60/P";
        /// '1000' 180/p.
        P180 = 0b1000 => "180p", "180/p";
        /// '1001' 2160/120/P.
        P2160x120 = 0b1001 => "2160p120", "2160/120/P";
        /// '1010' 4320/60/P.
        P4320x60 = 0b1010 => "4320p60", "4320/60/P";
        /// '1011' 4320/120/P.
        P4320x120 = 0b1011 => "4320p120", "4320/120/P";
    }
    reserved
    /// '1100'–'1111' "For extension of video encode format".
    Extension => "For extension of video encode format";
}

coded_enum! {
    /// transfer_characteristics (2 bits) of the video decode control
    /// descriptor — ARIB STD-B10 Part 2 §6.2.30, Table 6-59 / Table 6-60-1
    /// "Transfer characteristics".  The values name the VUI
    /// transfer_characteristics they signal.  Editions before v5.x (and
    /// still Part 1 Fig. 6.2-32) show these two bits as
    /// reserved_future_use, whose value is '11' (B10 Part 2 §3.1) — the
    /// same bits as [`TransferCharacteristics::Undefined`].
    pub enum TransferCharacteristics: "transfer_characteristics", "B10 §6.2.30 Table 6-60-1", 2 bits {
        /// '00' VUI transfer_characteristics = 1, 11 or 14 (Rec. ITU-R
        /// BT.709-5, BT.2020 or IEC 61966-2-4).
        Bt709 = 0b00 => "bt709", "transfer_characteristics = 1, 11 or 14 (Rec. ITU-R BT.709-5, BT.2020 or IEC 61966-2-4) in VUI";
        /// '01' VUI transfer_characteristics = 16 (Rec. ITU-R BT.2100 PQ).
        Pq = 0b01 => "pq", "transfer_characteristics = 16 (Rec. ITU-R BT.2100 PQ) in VUI";
        /// '10' VUI transfer_characteristics = 18 (Rec. ITU-R BT.2100 HLG).
        Hlg = 0b10 => "hlg", "transfer_characteristics = 18 (Rec. ITU-R BT.2100 HLG) in VUI";
        /// '11' "Undefined" (= the '11' of the former reserved_future_use bits).
        Undefined = 0b11 => "undefined", "Undefined";
    }
}

coded_enum! {
    /// document_resolution (4 bits) of additional_arib_bxml_info() /
    /// arib_bxml_info() — ARIB STD-B24 Vol. 2 §9.3.2 Table 9-12 and §9.3.3
    /// Table 9-13 "Document Resolution" (resolution and
    /// display-aspect-ratio of the BML content).
    pub enum DocumentResolution: "document_resolution", "B24 Vol. 2 §9.3.2 Table 9-12", 4 bits {
        /// '0000' documents with different resolutions / aspect ratios, or
        /// none specified.
        Mixed = 0b0000 => "mixed", "BML/B-XML documents with different resolutions and aspect ratios (or none specified)";
        /// '0001' 1920x1080 (16:9).
        R1920x1080 = 0b0001 => "1920x1080", "1920x1080 (16:9)";
        /// '0010' 1280x720 (16:9).
        R1280x720 = 0b0010 => "1280x720", "1280x720 (16:9)";
        /// '0011' 960x540 (16:9).
        R960x540 = 0b0011 => "960x540", "960x540 (16:9)";
        /// '0100' 720x480 (16:9).
        R720x480Wide = 0b0100 => "720x480-16:9", "720x480 (16:9)";
        /// '0101' 720x480 (4:3).
        R720x480 = 0b0101 => "720x480-4:3", "720x480 (4:3)";
        /// '0110' 320x240 (4:3).
        R320x240 = 0b0110 => "320x240", "320x240 (4:3)";
        /// '1111' only documents with no resolution nor aspect ratio
        /// specified.
        Unspecified = 0b1111 => "unspecified", "only BML/B-XML documents with no resolutions nor aspect ratios specified";
    }
    reserved
    /// '0111'–'1110' "Reserved for future use".
    Reserved => "Reserved for future use";
}

coded_enum! {
    /// application_control_code of the AIT application loop — ARIB STD-B24
    /// Vol. 4 §5.1 Table 5-1 semantics (the default semantics "when it is
    /// not specified to every application form"; Hybridcast uses 0x01,
    /// 0x02 and 0x04 only, IPTVFJ STD-0013 §5.2.3).
    pub enum ApplicationControlCode: "application_control_code", "B24 Vol. 4 §5.1", 8 bits {
        /// 0x01 AUTOSTART.
        Autostart = 0x01 => "autostart", "AUTOSTART: start the application";
        /// 0x02 PRESENT.
        Present = 0x02 => "present", "PRESENT: the application is in a feasible state";
        /// 0x04 KILL.
        Kill = 0x04 => "kill", "KILL: end the application";
        /// 0x05 PREFETCH.
        Prefetch = 0x05 => "prefetch", "PREFETCH: acquire and hold the application";
    }
    reserved
    /// Any other value (not defined; a receiver "shall disregard" the
    /// application, STD-0013 §5.2.3).
    Reserved => "not defined";
}

coded_enum! {
    /// visibility (2 bits) of the AIT application descriptor — ARIB
    /// STD-B24 Vol. 4 §5.3.1 Table 5-3 semantics.
    pub enum ApplicationVisibility: "visibility", "B24 Vol. 4 §5.3.1", 2 bits {
        /// '00' invisible (except error reporting such as the log output).
        Invisible = 0b00 => "invisible", "treated as invisible excluding exceptional error reporting such as the log output";
        /// '01' invisible to the user, visible to other applications
        /// through the API.
        ApplicationsOnly = 0b01 => "applications-only", "unable to be seen by a user, but visible from other application through API";
        /// '11' visible to the user and other applications.
        Visible = 0b11 => "visible", "visible for both a user and other application";
    }
    reserved
    /// '10' "Reserved for future use".
    Reserved => "Reserved for future use";
}

coded_enum! {
    /// digital_recording_control_data of the digital copy control
    /// descriptor — ARIB STD-B10 Part 2 §6.2.23, Table 6-39 "Digital copy
    /// control information".
    pub enum DigitalRecordingControl: "digital_recording_control_data", "B10 §6.2.23 Table 6-39", 2 bits {
        /// '00' "Copy can be made without control condition".
        CopyFree = 0b00 => "copy-free", "Copy can be made without control condition";
        /// '01' "Defined by service provider" (*1 "Broadcasting service
        /// provider can define independently"; Part 1 Table 7-1
        /// "Operational standard of identifiers": "0x01 Specified and
        /// operated by the company").
        ProviderDefined = 0b01 => "provider-defined", "Defined by service provider";
        /// '10' "Copy can be made for only one generation" (*2 "Received
        /// broadcasting signals can be recorded (first-generation copy) but
        /// the recorded signals cannot be reproduced furthermore").
        CopyOneGeneration = 0b10 => "copy-one-generation", "Copy can be made for only one generation";
        /// '11' "Copy is forbidden".
        CopyNever = 0b11 => "copy-never", "Copy is forbidden";
    }
}

coded_enum! {
    /// copy_control_type: the upper 2 bits of the digital copy control
    /// descriptor's 4-bit user_defined field in the *example* layout of
    /// ARIB STD-B10 Annex F (informative), Table F-2 "Copy control type
    /// information".  B10 itself leaves user_defined to the broadcaster
    /// (§6.2.23 "broadcasting service provider can define it originally");
    /// the ARIB operational guidelines (TR-B14 terrestrial / TR-B15 BS, not
    /// available locally) are understood to make this layout normative.
    ///
    /// Receiver evidence (observed on one Sony BDZ-series recorder): with
    /// digital_recording_control_data '10', copy_control_type '01' (user_defined
    /// 0b0100, as live terrestrial broadcasters send) plays; '00'
    /// (user_defined 0b0000) is refused with "Unsupported signal".
    pub enum CopyControlType: "copy_control_type", "B10 Annex F (informative) Table F-2", 2 bits {
        /// '00' "Undefined".  Refused by one Sony BDZ-series recorder (observation).
        Undefined00 = 0b00 => "undefined-00", "Undefined";
        /// '01' "Output by encoding to serial interface" (*1 "Encoding
        /// method specified by service provider is used"); the next 2 bits
        /// are APS_control_data.  Live broadcasters send it; one Sony
        /// BDZ-series recorder was observed to require it.
        SerialOutputEncoded = 0b01 => "serial-output-encoded", "Output by encoding to serial interface";
        /// '10' "Undefined".
        Undefined10 = 0b10 => "undefined-10", "Undefined";
        /// '11' "Output by not encoding to serial interface".
        SerialOutputUnencoded = 0b11 => "serial-output-unencoded", "Output by not encoding to serial interface";
    }
}

coded_enum! {
    /// APS_control_data (analog output copy control) — ARIB STD-B10 Annex F
    /// (informative), Table F-4; present when copy_control_type = '01'.
    pub enum ApsControlData: "APS_control_data", "B10 Annex F (informative) Table F-4", 2 bits {
        /// '00' "Can be copied without control condition".
        CopyFree = 0b00 => "copy-free", "Can be copied without control condition";
        /// '01' "With pseudo-sync pulse".
        PseudoSync = 0b01 => "pseudo-sync", "With pseudo-sync pulse";
        /// '10' "Pseudo-sync pulse + 2-line reversed division burst insertion".
        PseudoSyncBurst2 = 0b10 => "pseudo-sync-2-line-burst", "Pseudo-sync pulse + 2-line reversed division burst insertion";
        /// '11' "Pseudo-sync pulse + 4-line reversed division burst insertion".
        PseudoSyncBurst4 = 0b11 => "pseudo-sync-4-line-burst", "Pseudo-sync pulse + 4-line reversed division burst insertion";
    }
}

/// The 4-bit user_defined field of the digital copy control descriptor read
/// with the example layout of ARIB STD-B10 Annex F (informative), Table F-1:
/// `copy_control_type (2)` then `APS_control_data (2)` if
/// copy_control_type = '01', else `reserved_future_use (2)`.
///
/// Informative only: B10 §6.2.23 says "broadcasting service provider can
/// define it originally"; [`UserDefinedAnnexF::from_code`] /
/// [`UserDefinedAnnexF::code`] are an exact bijection on 0..=15, so nothing
/// is lost by viewing a value this way.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct UserDefinedAnnexF {
    /// copy_control_type (Table F-2).
    pub copy_control_type: CopyControlType,
    /// The low 2 bits: APS_control_data (Table F-4) when copy_control_type
    /// is '01'; otherwise reserved_future_use (kept raw).
    pub low_bits: AnnexFLowBits,
}

/// Low 2 bits of [`UserDefinedAnnexF`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AnnexFLowBits {
    /// APS_control_data (copy_control_type '01').
    Aps(ApsControlData),
    /// reserved_future_use (other copy_control_type values), raw 2 bits.
    Reserved(u8),
}

impl UserDefinedAnnexF {
    /// Split the 4-bit user_defined value (masked to 4 bits).
    pub fn from_code(user_defined: u8) -> Self {
        let cct = CopyControlType::from_code(user_defined >> 2);
        let low = user_defined & 0b11;
        Self {
            copy_control_type: cct,
            low_bits: if cct == CopyControlType::SerialOutputEncoded {
                AnnexFLowBits::Aps(ApsControlData::from_code(low))
            } else {
                AnnexFLowBits::Reserved(low)
            },
        }
    }
    /// The 4-bit user_defined value.
    pub fn code(self) -> u8 {
        let low = match self.low_bits {
            AnnexFLowBits::Aps(a) => a.code(),
            AnnexFLowBits::Reserved(r) => r & 0b11,
        };
        (self.copy_control_type.code() << 2) | low
    }
}

coded_enum! {
    /// component_type for stream_content 0x1 (MPEG-2 video) of the
    /// component descriptor — ARIB STD-B10 Part 2 §6.2.3, Table 6-5
    /// "stream_content and component_type".
    ///
    /// The names follow the table's pattern: low nibble 1 = 4:3, 2 = 16:9
    /// with pan vectors, 3 = 16:9 without pan vectors, 4 = wider than 16:9.
    /// The 240p rows 0xD2–0xD4 are printed "4:3 aspect ratio, with pan
    /// vectors / without pan vector / > 16:9" in v5.13 (text and PDF); the
    /// variants are named by the pattern (16:9), as the H.264 rows
    /// 0x5/0xD2–0xD4 of the same table print them (SPEC-NOTES).
    pub enum VideoComponentType: "component_type", "B10 §6.2.3 Table 6-5 (stream_content 0x1)", 8 bits {
        /// 0x01 "Video 480i(525i), 4:3 aspect ratio".
        V480i4x3 = 0x01 => "480i-4:3", "Video 480i(525i), 4:3 aspect ratio";
        /// 0x02 "Video 480i(525i), 16:9 aspect ratio, with pan vectors".
        V480i16x9Pan = 0x02 => "480i-16:9-with-pan-vectors", "Video 480i(525i), 16:9 aspect ratio, with pan vectors";
        /// 0x03 "Video 480i(525i), 16:9 aspect ratio, without pan vectors".
        V480i16x9 = 0x03 => "480i-16:9-without-pan-vectors", "Video 480i(525i), 16:9 aspect ratio, without pan vectors";
        /// 0x04 "Video 480i(525i), >16:9 aspect ratio".
        V480iWide = 0x04 => "480i-over-16:9", "Video 480i(525i), >16:9 aspect ratio";
        /// 0x83 "Video 4320p, 16:9 aspect ratio".
        V4320p16x9 = 0x83 => "4320p-16:9", "Video 4320p, 16:9 aspect ratio";
        /// 0x91 "Video 2160p, 4:3 aspect ratio".
        V2160p4x3 = 0x91 => "2160p-4:3", "Video 2160p, 4:3 aspect ratio";
        /// 0x92 "Video 2160p, 16:9 aspect ratio, with pan vectors".
        V2160p16x9Pan = 0x92 => "2160p-16:9-with-pan-vectors", "Video 2160p, 16:9 aspect ratio, with pan vectors";
        /// 0x93 "Video 2160p, 16:9 aspect ratio, without pan vectors".
        V2160p16x9 = 0x93 => "2160p-16:9-without-pan-vectors", "Video 2160p, 16:9 aspect ratio, without pan vectors";
        /// 0x94 "Video 2160p, >16:9 aspect ratio".
        V2160pWide = 0x94 => "2160p-over-16:9", "Video 2160p, >16:9 aspect ratio";
        /// 0xA1 "Video 480p(525p), 4:3 aspect ratio".
        V480p4x3 = 0xA1 => "480p-4:3", "Video 480p(525p), 4:3 aspect ratio";
        /// 0xA2 "Video 480p(525p), 16:9 aspect ratio, with pan vectors".
        V480p16x9Pan = 0xA2 => "480p-16:9-with-pan-vectors", "Video 480p(525p), 16:9 aspect ratio, with pan vectors";
        /// 0xA3 "Video 480p(525p), 16:9 aspect ratio, without pan vectors".
        V480p16x9 = 0xA3 => "480p-16:9-without-pan-vectors", "Video 480p(525p), 16:9 aspect ratio, without pan vectors";
        /// 0xA4 "Video 480p(525p), >16:9 aspect ratio".
        V480pWide = 0xA4 => "480p-over-16:9", "Video 480p(525p), >16:9 aspect ratio";
        /// 0xB1 "Video 1080i(1125i), 4:3 aspect ratio".
        V1080i4x3 = 0xB1 => "1080i-4:3", "Video 1080i(1125i), 4:3 aspect ratio";
        /// 0xB2 "Video 1080i(1125i), 16:9 aspect ratio, with pan vectors".
        V1080i16x9Pan = 0xB2 => "1080i-16:9-with-pan-vectors", "Video 1080i(1125i), 16:9 aspect ratio, with pan vectors";
        /// 0xB3 "Video 1080i(1125i), 16:9 aspect ratio, without pan vectors".
        V1080i16x9 = 0xB3 => "1080i-16:9-without-pan-vectors", "Video 1080i(1125i), 16:9 aspect ratio, without pan vectors";
        /// 0xB4 "Video 1080i(1125i), >16:9 aspect ratio".
        V1080iWide = 0xB4 => "1080i-over-16:9", "Video 1080i(1125i), >16:9 aspect ratio";
        /// 0xC1 "Video 720p(750p), 4:3 aspect ratio".
        V720p4x3 = 0xC1 => "720p-4:3", "Video 720p(750p), 4:3 aspect ratio";
        /// 0xC2 "Video 720p(750p), 16:9 aspect ratio, with pan vectors".
        V720p16x9Pan = 0xC2 => "720p-16:9-with-pan-vectors", "Video 720p(750p), 16:9 aspect ratio, with pan vectors";
        /// 0xC3 "Video 720p(750p), 16:9 aspect ratio, without pan vectors".
        V720p16x9 = 0xC3 => "720p-16:9-without-pan-vectors", "Video 720p(750p), 16:9 aspect ratio, without pan vectors";
        /// 0xC4 "Video 720p(750p), >16:9 aspect ratio".
        V720pWide = 0xC4 => "720p-over-16:9", "Video 720p(750p), >16:9 aspect ratio";
        /// 0xD1 "Video 240p, 4:3 aspect ratio".
        V240p4x3 = 0xD1 => "240p-4:3", "Video 240p, 4:3 aspect ratio";
        /// 0xD2 printed "Video 240p, 4:3 aspect ratio, with pan vectors"
        /// (16:9 by the table pattern).
        V240p16x9Pan = 0xD2 => "240p-16:9-with-pan-vectors", "Video 240p, 4:3 aspect ratio, with pan vectors [sic]";
        /// 0xD3 printed "Video 240p, 4:3 aspect ratio, without pan vector"
        /// (16:9 by the table pattern).
        V240p16x9 = 0xD3 => "240p-16:9-without-pan-vectors", "Video 240p, 4:3 aspect ratio, without pan vector [sic]";
        /// 0xD4 printed "Video 240p, 4:3 aspect ratio > 16:9".
        V240pWide = 0xD4 => "240p-over-16:9", "Video 240p, 4:3 aspect ratio > 16:9 [sic]";
        /// 0xE1 "Video 1080p(1125p), 4:3 aspect ratio".
        V1080p4x3 = 0xE1 => "1080p-4:3", "Video 1080p(1125p), 4:3 aspect ratio";
        /// 0xE2 "Video 1080p(1125p), 16:9 aspect ratio, with pan vectors".
        V1080p16x9Pan = 0xE2 => "1080p-16:9-with-pan-vectors", "Video 1080p(1125p), 16:9 aspect ratio, with pan vectors";
        /// 0xE3 "Video 1080p(1125p), 16:9 aspect ratio, without pan vectors".
        V1080p16x9 = 0xE3 => "1080p-16:9-without-pan-vectors", "Video 1080p(1125p), 16:9 aspect ratio, without pan vectors";
        /// 0xE4 "Video 1080p(1125p), >16:9 aspect ratio".
        V1080pWide = 0xE4 => "1080p-over-16:9", "Video 1080p(1125p), >16:9 aspect ratio";
        /// 0xF1 "Video 180p, 4:3 aspect ratio".
        V180p4x3 = 0xF1 => "180p-4:3", "Video 180p, 4:3 aspect ratio";
        /// 0xF2 "Video 180p, 16:9 aspect ratio, with pan vectors".
        V180p16x9Pan = 0xF2 => "180p-16:9-with-pan-vectors", "Video 180p, 16:9 aspect ratio, with pan vectors";
        /// 0xF3 "Video 180p, 16:9 aspect ratio, without pan vector".
        V180p16x9 = 0xF3 => "180p-16:9-without-pan-vectors", "Video 180p, 16:9 aspect ratio, without pan vector";
        /// 0xF4 "Video 180p, aspect ratio > 16:9".
        V180pWide = 0xF4 => "180p-over-16:9", "Video 180p, aspect ratio > 16:9";
    }
    reserved
    /// Any code Table 6-5 marks "Reserved for future use" for stream_content 0x1.
    Reserved => "Reserved for future use";
}

coded_enum! {
    /// Audio mode, component_type b4–b0 of the audio component descriptor
    /// — ARIB STD-B10 Part 2 §6.2.26, Table 6-43.  Names are the table's
    /// mode notation.
    pub enum AudioMode: "component_type (b4-b0, audio mode)", "B10 §6.2.26 Table 6-43", 5 bits {
        /// '00001' 1/0 mode (single monaural channel).
        Mono = 0b00001 => "1/0", "1/0 mode (single monaural channel)";
        /// '00010' 1/0 + 1/0 mode (dual monaural channel).
        DualMono = 0b00010 => "1/0+1/0", "1/0 + 1/0 mode (dual monaural channel)";
        /// '00011' 2/0 mode (stereo).
        Stereo = 0b00011 => "2/0", "2/0 mode (stereo)";
        /// '00100' 2/1 mode.
        M2_1 = 0b00100 => "2/1", "2/1 mode";
        /// '00101' 3/0 mode.
        M3_0 = 0b00101 => "3/0", "3/0 mode";
        /// '00110' 2/2 mode.
        M2_2 = 0b00110 => "2/2", "2/2 mode";
        /// '00111' 3/1 mode.
        M3_1 = 0b00111 => "3/1", "3/1 mode";
        /// '01000' 3/2 mode.
        M3_2 = 0b01000 => "3/2", "3/2 mode";
        /// '01001' 3/2 + LFE mode (3/2.1 mode).
        M3_2Lfe = 0b01001 => "3/2+LFE", "3/2 + LFE mode (3/2.1 mode)";
        /// '01010' 3/3.1 mode.
        M3_3_1 = 0b01010 => "3/3.1", "3/3.1 mode";
        /// '01011' 2/0/0-2/0/2-0.1 mode.
        M2_0_0_2_0_2_0_1 = 0b01011 => "2/0/0-2/0/2-0.1", "2/0/0-2/0/2-0.1 mode";
        /// '01100' 5/2.1 mode.
        M5_2_1 = 0b01100 => "5/2.1", "5/2.1 mode";
        /// '01101' 3/2/2.1 mode.
        M3_2_2_1 = 0b01101 => "3/2/2.1", "3/2/2.1 mode";
        /// '01110' 2/0/0-3/0/2-0.1 mode.
        M2_0_0_3_0_2_0_1 = 0b01110 => "2/0/0-3/0/2-0.1", "2/0/0-3/0/2-0.1 mode";
        /// '01111' 0/2/0-3/0/2-0.1 mode.
        M0_2_0_3_0_2_0_1 = 0b01111 => "0/2/0-3/0/2-0.1", "0/2/0-3/0/2-0.1 mode";
        /// '10000' 2/0/0-3/2/3-0.2 mode.
        M2_0_0_3_2_3_0_2 = 0b10000 => "2/0/0-3/2/3-0.2", "2/0/0-3/2/3-0.2 mode";
        /// '10001' 3/3/3-5/2/3-3/0/0.2 mode.
        M3_3_3_5_2_3_3_0_0_2 = 0b10001 => "3/3/3-5/2/3-3/0/0.2", "3/3/3-5/2/3-3/0/0.2 mode";
    }
    reserved
    /// '00000' and '10010'–'11111' "Reserved for future use".
    Reserved => "Reserved for future use";
}

coded_enum! {
    /// Audio for a person with disabilities, component_type b6–b5 of the
    /// audio component descriptor — ARIB STD-B10 Part 2 §6.2.26, Table 6-43.
    pub enum AudioForDisabilities: "component_type (b6-b5, audio for a person with disabilities)", "B10 §6.2.26 Table 6-43", 2 bits {
        /// '00' "Not specified audio for a person with disabilities".
        None = 0b00 => "none", "Not specified audio for a person with disabilities";
        /// '01' "Audio commentary for a person with visually impaired".
        VisuallyImpairedCommentary = 0b01 => "visually-impaired-commentary", "Audio commentary for a person with visually impaired";
        /// '10' "Audio for a person with hearing impaired".
        HearingImpaired = 0b10 => "hearing-impaired", "Audio for a person with hearing impaired";
    }
    reserved
    /// '11' "Reserved for future use".
    Reserved => "Reserved for future use";
}

/// component_type of the audio component descriptor — ARIB STD-B10 Part 2
/// §6.2.26, Table 6-43: b7 dialog control, b6–b5 audio for a person with
/// disabilities, b4–b0 audio mode.  Exact bijection with the 8-bit code.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct AudioComponentType {
    /// b7: '1' "Audio stream includes dialog control information".
    pub dialog_control: bool,
    /// b6–b5.
    pub for_disabilities: AudioForDisabilities,
    /// b4–b0.
    pub mode: AudioMode,
}

impl AudioComponentType {
    /// Decode the 8-bit component_type.
    pub fn from_code(c: u8) -> Self {
        Self {
            dialog_control: c & 0x80 != 0,
            for_disabilities: AudioForDisabilities::from_code(c >> 5),
            mode: AudioMode::from_code(c),
        }
    }
    /// The 8-bit component_type.
    pub fn code(self) -> u8 {
        (u8::from(self.dialog_control) << 7)
            | (self.for_disabilities.code() << 5)
            | self.mode.code()
    }
}

coded_enum! {
    /// sampling_rate of the audio component descriptor — ARIB STD-B10 Part 2
    /// §6.2.26, Table 6-45 "Sampling frequency".
    pub enum SamplingRate: "sampling_rate", "B10 §6.2.26 Table 6-45", 3 bits {
        /// '001' 16kHz.
        Khz16 = 0b001 => "16kHz", "16kHz";
        /// '010' 22.05kHz.
        Khz22_05 = 0b010 => "22.05kHz", "22.05kHz";
        /// '011' 24kHz.
        Khz24 = 0b011 => "24kHz", "24kHz";
        /// '101' 32kHz.
        Khz32 = 0b101 => "32kHz", "32kHz";
        /// '110' 44.1kHz.
        Khz44_1 = 0b110 => "44.1kHz", "44.1kHz";
        /// '111' 48kHz.
        Khz48 = 0b111 => "48kHz", "48kHz";
    }
    reserved
    /// '000' "Reserved for future use", '100' "Reserved".
    Reserved => "Reserved";
}

/// maximum_bitrate of the digital copy control descriptor — ARIB STD-B10
/// Part 2 §6.2.23: "transmission rate of TS packet of each event or
/// elementary stream by rolling up in each 1/4Mbps".  The raw value counts
/// 1/4 Mbit/s units.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct MaximumBitrate(pub u8);

impl MaximumBitrate {
    /// Unit of the field: 1/4 Mbit/s = 250 000 bit/s.
    pub const UNIT_BPS: u64 = 250_000;
    /// The rate in bit/s.
    pub fn bps(self) -> u64 {
        u64::from(self.0) * Self::UNIT_BPS
    }
    /// The rate in Mbit/s.
    pub fn mbps(self) -> f64 {
        f64::from(self.0) / 4.0
    }
    /// Code a rate in bit/s, "rolling up" to the next 1/4 Mbit/s; `None`
    /// if the result does not fit in 8 bits (> 63.75 Mbit/s).
    pub fn from_bps_rounded_up(bps: u64) -> Option<Self> {
        u8::try_from(bps.div_ceil(Self::UNIT_BPS)).ok().map(Self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn complete<T: CodedValue + std::fmt::Debug>() {
        assert_eq!(T::NAMED.len(), 1 << T::BITS, "{}", T::TABLE);
        for c in 0..(1u16 << T::BITS) as u8 {
            assert_eq!(T::from_code(c).code(), c);
        }
    }

    #[test]
    fn complete_tables() {
        complete::<TransferCharacteristics>();
        complete::<DigitalRecordingControl>();
        complete::<CopyControlType>();
        complete::<ApsControlData>();
    }

    #[test]
    fn every_code_round_trips_and_names_are_unique() {
        fn check<T: CodedValue + std::fmt::Debug>() {
            for c in 0..(1u16 << T::BITS) as u8 {
                assert_eq!(T::from_code(c).code(), c, "{} {c:#x}", T::TABLE);
            }
            let mut names: Vec<_> = T::NAMED.iter().map(|v| v.name()).collect();
            names.sort();
            names.dedup();
            assert_eq!(names.len(), T::NAMED.len(), "{}", T::TABLE);
            for v in T::NAMED {
                assert_eq!(T::from_name(v.name()), Some(*v));
            }
        }
        check::<VideoEncodeFormat>();
        check::<TransferCharacteristics>();
        check::<DigitalRecordingControl>();
        check::<CopyControlType>();
        check::<ApsControlData>();
        check::<VideoComponentType>();
        check::<AudioMode>();
        check::<AudioForDisabilities>();
        check::<SamplingRate>();
        check::<DocumentResolution>();
        check::<ApplicationVisibility>();
        for c in 0..=255u8 {
            assert_eq!(ApplicationControlCode::from_code(c).code(), c);
        }
        for c in 0..=255u8 {
            assert_eq!(AudioComponentType::from_code(c).code(), c);
        }
        for c in 0..16u8 {
            assert_eq!(UserDefinedAnnexF::from_code(c).code(), c);
        }
    }
}
