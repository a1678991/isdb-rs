# isdb-mmt

The content of ISDB-S3 / advanced BS TLV streams (ARIB STD-B60 v1.14):
the tables a receiver uses to find services, events and assets, and the
media of each asset reassembled into an elementary stream. `isdb-tlv`
delivers the layers below (TLV packets, IP, MMTP headers); this crate
reads what they carry.

| Module | Contents |
|---|---|
| `desc` | MMT-SI descriptors: 16-bit tag, length width per descriptor (§7.4.3); MPU timestamp and extended timestamp, access control, scrambler, MH-stream identifier, MH-service, MH-service list, MH-short event, MH-content, MH-HEVC, MH-broadcaster name; others kept raw |
| `table` | MPT and PLT (PA message), MH-EIT, MH-SDT, MH-TOT, MH-BIT (M2 section / short section messages), TLV-NIT and AMT (transmission-control packets); CRC-32 checked |
| `message` | control-message reassembly: divided messages per packet id, aggregated messages split, each message cut into its tables |
| `mpu` | MPU payload header and MFUs (Table 6-1), divided MFUs reassembled per asset, packet-sequence and MPU-sequence continuity, scrambled payloads counted from their clear header |
| `es` | HEVC as an Annex B byte stream, MPEG-4 audio as LOAS, caption MFUs (Table 9-1) |
| `decoder` | the streaming decoder over a stream's TLV packets: NTP time, TLV-SI, MMT-SI tables kept at their latest version (repeats not decoded again), MFUs with their asset |

Why a crate of its own: `isdb-tlv` is the transport (packets, flows,
headers, counters); the tables, descriptors and media framing here are
larger and depend on `isdb-si` (MJD/BCD times), which the transport does
not need.

The coded fields of the MPU payload header (Table 6-1) are lossless
coded enums of their tables: `mpu::FragmentType` (Table 6-2: MPU
metadata, movie fragment metadata, MFU, `Reserved(u8)` for 3–15) and
`mpu::FragmentationIndicator` (Table 6-3: `Complete`, `Head`, `Middle`,
`Last`, all four codes assigned). The MFU reassembler matches on the
variants; `from_code()` / `code()` go both ways over the whole field
width, so a parsed header re-encodes bit-exactly:

```rust
use isdb_mmt::mpu::{self, FragmentType, FragmentationIndicator};

// A payload header: length 6, MFU, timed, middle fragment of a divided
// MFU, not aggregated, MPU sequence number 0021B0ECh
let p = [0x00, 0x06, 0x2C, 0x00, 0x00, 0x21, 0xB0, 0xEC];
let h = mpu::parse_header(&p).unwrap();
assert_eq!(h.fragment_type, FragmentType::Mfu);
assert_eq!(h.fragmentation, FragmentationIndicator::Middle);
assert!(h.timed && !h.aggregated && !h.fragmentation.completes());
assert_eq!(h.mpu_sequence_number, 0x0021_B0EC);

// Reserved fragment types are kept, not dropped.
assert_eq!(FragmentType::from_code(7), FragmentType::Reserved(7));
assert_eq!(FragmentType::Reserved(7).code(), 7);
```

Text in MMT-SI is UTF-8 (B60 §7.1), not the ARIB 8-unit code of TS SI.

Scrambled payloads (ACAS) are recognised from the MMTP header extension
and counted; only their payload header is in the clear (SPEC-NOTES
"scrambled-mpu"). They are never descrambled (project policy).

ISO/IEC 23008-1 (MMT), ARIB STD-B61 / B62, ITU-T H.265 and ISO/IEC
14496-3 are not available: what B60 defines is implemented from B60; the
framing details from the codec standards are marked "from knowledge" in
[SPEC-NOTES.md](SPEC-NOTES.md).
