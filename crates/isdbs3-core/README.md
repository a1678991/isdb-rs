# isdbs3-core

ISDB-S3 transmission parameters from ARIB STD-B44 v2.1 (ITU-R BO.2098-0
"System F"; MIC Ordinance No. 87 of 2011, Chapter 5 §3): frame and slot
geometry, the eleven inner code rates and their LDPC codes (address tables
transcribed from the standard by `tools/b44-ldpc-tables.py`), the
π/2-BPSK / QPSK / 8PSK / 16APSK / 32APSK constellations with their bit
labels, the bit interleaver, the slot / TMCC / pilot energy dispersals, the
TMCC information codec and signal coding, and the slot allocation.

The BCH and LDPC machinery is in `isdb-fec`; the receiver is
`isdbs3-phy`. Interpretations of the standard, and how each was confirmed
on live signals, are in [SPEC-NOTES.md](SPEC-NOTES.md).

| Module | B44 clause | Contents |
|---|---|---|
| `params` | §3.2–§3.4, Tables 3.2-1, 3.4-2, 3.8-2/3 | geometry, modulations, code rates, γ |
| `ldpc`, `ldpc_tables` | §3.4 (2) Appendix 1 | the LDPC codes |
| `constellation` | §3.8, Fig. 3.8-1 | signal points and labels |
| `interleave` | §3.7 | bit interleaver |
| `dispersal` | §3.6 | energy dispersal |
| `tmcc` | §3.2 (3), §3.5, §3.11 | TMCC information and signal |
| `slots` | §3.2 (2) | slot allocation, codeword placement |

## TMCC code fields are lossless coded enums

The TMCC codec keeps every received word bit-exactly
(`TmccInfo::parse(&t.bits()) == t`, property-tested), so the coded fields
of §3.11 are enums with one variant per assigned code *and* a variant that
holds a reserved code as received: `ModulationCode` (Table 3.11-1, 4 bits),
`RateCode` (Table 3.11-2, 4 bits) and `StreamType` (Table 3.11-4, 8 bits).
`from_code(u8)` is total (it masks to the field width) and `code()` is its
inverse over the whole code range; `Mode::modulation()` / `rate()` /
`is_used()` are pattern matches on them. Table 3.11-2 prints 101/120 at
both `1001` and `1010`; `1010` is read as 105/120, the one rate the table
otherwise lacks (SPEC-NOTES "TMCC code-rate table").

```rust
use isdbs3_core::{CodeRate, Mode, Modulation, ModulationCode, RateCode, StreamType};

let m = Mode {
    modulation_code: ModulationCode::from_code(0b0100),
    rate_code: RateCode::from_code(0b1010),
    slots: 120,
    obo: 0,
};
assert_eq!(m.modulation(), Some(Modulation::Apsk16));
assert_eq!(m.rate(), CodeRate::new(105));
assert!(m.is_used());

// 1111 is "no scheme allocated" (an unused mode); other codes are kept
// in `Reserved`, whose payload can only be decoded (private constructor),
// so `==` is wire equality.
assert_eq!(ModulationCode::from_code(0b1111), ModulationCode::Unused);
assert!(matches!(RateCode::from_code(0b1100), RateCode::Reserved(r) if r.get() == 0b1100));
assert_eq!(RateCode::from_code(0b1100).code(), 0b1100);
assert_eq!(RateCode::from_code(0xFC), RateCode::from_code(0b1100)); // masked to 4 bits
assert!(!Mode { slots: 5, ..Mode::UNUSED }.is_used());

assert_eq!(StreamType::from_code(0x02), StreamType::Tlv);
assert_eq!(StreamType::from_code(0xFF), StreamType::None);
assert_eq!(StreamType::from_code(0x7E).code(), 0x7E);
```
