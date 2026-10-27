# isdb-tlv

TLV streams of ISDB-S3 / advanced BS (ARIB STD-B32 v3.11 Part 3 §3.5,
§3.7, §3.9, §3.10; ARIB STD-B60 v1.14; ARIB STD-B44 v2.1 §3.2 (1)):

- `tlv`: TLV packets and the `Assembler` that rebuilds them from a
  stream's slots, using each slot header's head-TLV indication to gain
  and check packet sync and to recover after a lost slot;
- `ip`: IPv4, IPv6 and header-compressed IP (HCfB, header types 20h, 21h,
  60h, 61h) to the UDP flow and payload;
- `mmtp`: MMTP headers, control-message payloads, and the MMT-SI messages
  and tables they carry (PA message with MPT / PLT, M2 section and short
  section messages with MH-EIT, MH-SDT, MH-TOT, MH-AIT, ECM, EMM, …);
- `si`: TLV-SI sections (TLV-NIT, AMT) found by header and CRC-32 in
  transmission-control packets;
- `stats`: per-stream counters over all of the above.

The coded fields of the MMTP header are lossless coded enums of their
B60 tables: `mmtp::FecType` (Table 6-5) and `mmtp::PayloadType` (Table
6-6). Each names the assigned codes, keeps every other code in a
`Reserved` variant, and goes both ways with `from_code()` / `code()`, so
a parsed header re-encodes bit-exactly and the reserved codes are never
lost:

```rust
use isdb_tlv::mmtp::{self, FecType, PayloadType};

// An MMTP header as on air: version 00, not FEC-protected, payload type
// 02h (control messages), packet id 0000h (PA message), sequence 1
let pkt = [0x00, 0x02, 0x00, 0x00, 0, 0, 0, 0, 0, 0, 0, 1, 0x3C, 0x00];
let (h, payload) = mmtp::parse(&pkt).unwrap();
assert_eq!(h.fec_type, FecType::NotProtected);
assert_eq!(h.payload_type, PayloadType::Messages);
assert_eq!(payload, &[0x3C, 0x00]);

// Every 6-bit code round-trips, the reserved range included. A `Reserved`
// payload can only be decoded (private constructor), so `==` is wire
// equality.
assert!(matches!(PayloadType::from_code(0x2A), PayloadType::Reserved(r) if r.get() == 0x2A));
assert_eq!(PayloadType::from_code(0x2A).code(), 0x2A);
assert_eq!(PayloadType::from_code(0xEA), PayloadType::from_code(0x2A)); // masked to 6 bits
assert_eq!(FecType::from_code(3), FecType::Reserved);
```

ISO/IEC 23008-1 (MMT) is not available: only what B32 Part 3 and B60
define is parsed. Scrambled payloads are recognised, never descrambled.
Interpretations are in [SPEC-NOTES.md](SPEC-NOTES.md).
