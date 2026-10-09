//! C0 / C1 control codes of the 8-unit code and the number of parameter
//! bytes each one carries.
//!
//! ARIB STD-B24 v6.4 Vol.1 Part 2 §7.1.2, Table 7-14 (code table),
//! Table 7-15 (C0), Table 7-16 (C1), Table 7-17 (CSI).

/// NUL (00/0). Table 7-15.
pub const NUL: u8 = 0x00;
/// BEL (00/7). Table 7-15.
pub const BEL: u8 = 0x07;
/// APB, active position backward (00/8). Table 7-15.
pub const APB: u8 = 0x08;
/// APF, active position forward (00/9). Table 7-15.
pub const APF: u8 = 0x09;
/// APD, active position down (00/10). Table 7-15.
pub const APD: u8 = 0x0A;
/// APU, active position up (00/11). Table 7-15.
pub const APU: u8 = 0x0B;
/// CS, clear screen (00/12). Table 7-15.
pub const CS: u8 = 0x0C;
/// APR, active position return (00/13): "active position down is made,
/// moving to the first position of the same line". Table 7-15.
pub const APR: u8 = 0x0D;
/// LS1 (00/14). Table 7-1.
pub const LS1: u8 = 0x0E;
/// LS0 (00/15). Table 7-1.
pub const LS0: u8 = 0x0F;
/// PAPF, parameterized active position forward (01/6) + P1. Table 7-15.
pub const PAPF: u8 = 0x16;
/// CAN, cancel (01/8). Table 7-15.
pub const CAN: u8 = 0x18;
/// SS2 (01/9). Table 7-1.
pub const SS2: u8 = 0x19;
/// ESC (01/11). Table 7-1.
pub const ESC: u8 = 0x1B;
/// APS, active position set (01/12) + P1 P2. Table 7-15.
pub const APS: u8 = 0x1C;
/// SS3 (01/13). Table 7-1.
pub const SS3: u8 = 0x1D;
/// RS, record separator (01/14). Table 7-15.
pub const RS: u8 = 0x1E;
/// US, unit separator (01/15). Table 7-15.
pub const US: u8 = 0x1F;
/// SP, space (02/0). §7.1.2.3, Table 7-14.
pub const SP: u8 = 0x20;
/// DEL (07/15). §7.1.2.3, Table 7-14.
pub const DEL: u8 = 0x7F;

/// BKF, foreground black (08/0); RDF 08/1, GRF 08/2, YLF 08/3, BLF 08/4,
/// MGF 08/5, CNF 08/6, WHF 08/7 follow. Table 7-16.
pub const BKF: u8 = 0x80;
/// WHF, foreground white (08/7). Table 7-16.
pub const WHF: u8 = 0x87;
/// SSZ, small size (08/8). Table 7-16.
pub const SSZ: u8 = 0x88;
/// MSZ, middle size (08/9). Table 7-16.
pub const MSZ: u8 = 0x89;
/// NSZ, normal size (08/10). Table 7-16.
pub const NSZ: u8 = 0x8A;
/// SZX, character size controls (08/11) + P1. Table 7-16.
pub const SZX: u8 = 0x8B;
/// COL, colour controls (09/0) + P1, or + 02/0 P2 (palette). Table 7-16.
pub const COL: u8 = 0x90;
/// FLC, flashing control (09/1) + P1. Table 7-16.
pub const FLC: u8 = 0x91;
/// CDC, conceal display controls (09/2) + P1, or + 02/0 P2. Table 7-16.
pub const CDC: u8 = 0x92;
/// POL, pattern polarity controls (09/3) + P1. Table 7-16.
pub const POL: u8 = 0x93;
/// WMM, writing mode modification (09/4) + P1. Table 7-16.
pub const WMM: u8 = 0x94;
/// MACRO, macro command (09/5) + P1 (04/0 / 04/1 start a definition
/// `MACRO P1 MC body MACRO 04/15`). Table 7-16.
pub const MACRO: u8 = 0x95;
/// HLC, highlighting character block (09/7) + P1. Table 7-16.
pub const HLC: u8 = 0x97;
/// RPC, repeat character (09/8) + P1. Table 7-16.
pub const RPC: u8 = 0x98;
/// SPL, stop lining (09/9). Table 7-16.
pub const SPL: u8 = 0x99;
/// STL, start lining (09/10). Table 7-16.
pub const STL: u8 = 0x9A;
/// CSI, control sequence introducer (09/11). Table 7-16, Table 7-17.
pub const CSI: u8 = 0x9B;
/// TIME, time controls (09/13) + 02/0 P2 | 02/8 P2 | 02/9 … F. Table 7-16.
pub const TIME: u8 = 0x9D;

/// Name of a defined C0/C1 control code, or `None` for the blanks of
/// Table 7-14 ("Blanks of C0 area and C1 area are undefined", Note 2).
pub fn name(code: u8) -> Option<&'static str> {
    Some(match code {
        NUL => "NUL",
        BEL => "BEL",
        APB => "APB",
        APF => "APF",
        APD => "APD",
        APU => "APU",
        CS => "CS",
        APR => "APR",
        LS1 => "LS1",
        LS0 => "LS0",
        PAPF => "PAPF",
        CAN => "CAN",
        SS2 => "SS2",
        ESC => "ESC",
        APS => "APS",
        SS3 => "SS3",
        RS => "RS",
        US => "US",
        SP => "SP",
        DEL => "DEL",
        0x80 => "BKF",
        0x81 => "RDF",
        0x82 => "GRF",
        0x83 => "YLF",
        0x84 => "BLF",
        0x85 => "MGF",
        0x86 => "CNF",
        0x87 => "WHF",
        SSZ => "SSZ",
        MSZ => "MSZ",
        NSZ => "NSZ",
        SZX => "SZX",
        COL => "COL",
        FLC => "FLC",
        CDC => "CDC",
        POL => "POL",
        WMM => "WMM",
        MACRO => "MACRO",
        HLC => "HLC",
        RPC => "RPC",
        SPL => "SPL",
        STL => "STL",
        CSI => "CSI",
        TIME => "TIME",
        _ => return None,
    })
}

/// Number of parameter bytes that follow a control code whose count
/// does not depend on the parameters themselves (Tables 7-15, 7-16).
/// `None` for codes with a variable count (COL, CDC, TIME, CSI, MACRO,
/// ESC), which the decoder parses explicitly, and for undefined codes.
pub fn fixed_params(code: u8) -> Option<usize> {
    Some(match code {
        NUL | BEL | APB | APF | APD | APU | CS | APR | LS1 | LS0 | CAN | SS2 | SS3 | RS | US
        | SP | DEL => 0,
        PAPF => 1,
        APS => 2,
        0x80..=0x87 | SSZ | MSZ | NSZ | SPL | STL => 0,
        SZX | FLC | POL | WMM | HLC | RPC => 1,
        _ => return None,
    })
}
