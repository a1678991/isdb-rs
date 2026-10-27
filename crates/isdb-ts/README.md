# isdb-ts

MPEG-2 transport-stream layer of the workspace: bit I/O, CRC_32, the
188-byte TS packet and adaptation field, PCR and PES timestamps, PES
packets, the section envelope, PAT/CAT/PMT, section and PES
packetisation/reassembly, continuity checking and a sync-acquiring packet
reader. It is the base every ARIB SI, analysis and multiplexing crate builds
on, and it implements what ISDB-T needs from H.222.0.

## Spec scope

ITU-T H.222.0 (05/2006) | ISO/IEC 13818-1 (`specs/ITU-T-H.222.0-2006.txt`);
clause and table numbers are those of the 2006 edition (PES = Table 2-21,
descriptor tags = Table 2-45, see SPEC-NOTES).

| Module | Clauses / tables |
|---|---|
| `bits` | §2.2.6 `uimsbf`/`bslbf` MSB-first reader/writer |
| `crc` | Annex A CRC_32 (poly 0x04C11DB7, table-driven and bitwise) |
| `pid` | §2.4.3.3 Table 2-3 `Pid`: a 13-bit value type (`Pid::new` checks the range once; `PAT`, `CAT`, `TSDT`, `NULL` consts; `is_reserved`, `may_carry_pcr`), with `serde` as a range-checked integer behind the `serde` feature |
| `packet` | §2.4.3.2 Table 2-2 header (Tables 2-4/2-5 transport_scrambling_control and adaptation_field_control as lossless coded enums), §2.4.3.4–2.4.3.5 Table 2-6 adaptation field and extension, `check` |
| `pcr` | §2.4.2.2 eq. 2-1..2-3 PCR/OPCR, Table 2-21 33-bit PTS/DTS coding, wrap arithmetic |
| `cc` | §2.4.3.3 continuity_counter, duplicates, §2.4.3.5 discontinuity_indicator |
| `pes` | §2.4.3.6–2.4.3.7 Table 2-21 PES, Table 2-22 stream_id, Table 2-24 trick mode; packetiser and reassembler |
| `section` | long/short section envelope (Tables 2-30/2-32/2-33/2-40; ARIB STD-B10 §5.1 Fig. 5-1) |
| `psi` | PAT Table 2-30, CAT Table 2-32, PMT Table 2-33 and their `check_*` |
| `descriptor` | §2.6 raw descriptor loop, Table 2-45 tag classes |
| `demux` | §2.4.4.1–2.4.4.2 pointer_field, PUSI, 0xFF stuffing: section reassembly |
| `packetize` | sections → packets (`Packing::OnePerPacket` / `Packed`) |
| `reader` | Table 2-1 sync; also 192-byte (4-byte prefix) and 204-byte (16-byte trailer) framings |
| `stats` | per-PID counters (TEI, scrambling, PUSI, PCRs, CC events) |

## Position in the stack

- Depends on: nothing in the workspace (only `thiserror`; `serde` behind
  the optional `serde` feature).
- Used by: `isdb-si` (section envelope and `Error` for every SI table),
  `isdb-analyze`, `isdbt-mux`; `isdb-cli` reaches it only through
  `isdb-analyze` / `isdbt-mux`. The PHY crates (`isdbt-core`,
  `isdbt-fec`, `isdbt-ofdm`, `isdbt-phy`) do not use it; they carry TS
  packets as plain `[u8; 188]`.

## Key types and entry points

- `Pid`: the 13-bit PID (`Pid::new(u16) -> Option<Pid>`, `get()`,
  `Pid::PAT` … `Pid::NULL`); every `pid` field and parameter in the crate
  takes it, so an out-of-range PID cannot reach an encoder.
- `packet::Packet` (lossless codec) and `packet::View` (zero-copy header /
  payload / PCR accessor for hot paths); `packet::check`.
- `section::{Long, Short}` with the `LongBody` / `ShortBody` traits that
  `isdb-si` implements for every B10 table; `split_long`, `build_long`.
- `psi::{Pat, Cat, Pmt}`; `pes::PesPacket`, `pes::PesAssembler`,
  `pes::packetize`.
- `demux::SectionDemux` / `SectionAssembler` → `Assembled::{Section, Event}`;
  `packetize::packetize_sections`; `cc::{Counter, CcChecker, Tracker}`.
- `reader::{SliceReader, StreamReader}` → packets with index, byte offset
  and framing; `stats::Stats`.
- `Error` (`Truncated`, `Invalid`, `Crc`, `Overflow`, each naming the syntax
  element) and `Violation` (clause + message) are re-exported at the root.

Build a PAT, packetise it, read it back and check that decoding is lossless:

```rust
use isdb_ts::Pid;
use isdb_ts::cc::Counter;
use isdb_ts::demux::{Assembled, SectionDemux};
use isdb_ts::packetize::{Packing, packetize_sections};
use isdb_ts::psi::{Pat, PatEntry, check_pat};
use isdb_ts::reader::SliceReader;
use isdb_ts::section::Long;

// PAT (H.222.0 Table 2-30): NIT on PID 0x0010, one programme on 0x01F0.
// A PID is 13 bits (Table 2-3): `Pid::new` is `None` above 0x1FFF.
let nit = Pid::new(0x0010).unwrap();
let pmt = Pid::new(0x01F0).unwrap();
assert!(Pid::new(0x2000).is_none());
let pat = Pat::section(0x7FE8, 0, vec![PatEntry::new(0, nit), PatEntry::new(0x0408, pmt)]);
assert!(check_pat(&pat).is_empty());
let bytes = pat.encode().unwrap(); // section_length and CRC_32 computed

let mut cc = Counter::new(0);
let packets = packetize_sections(Pid::PAT, &[bytes.clone()], Packing::OnePerPacket, &mut cc).unwrap();
let stream: Vec<u8> = packets.concat();

let mut demux = SectionDemux::new();
demux.add_pid(Pid::PAT);
let mut sections = Vec::new();
for p in SliceReader::new(&stream) {
    for a in demux.push(p.index, p.packet) {
        if let Assembled::Section(s) = a {
            sections.push(s.data);
        }
    }
}
let back = Long::<Pat>::decode(&sections[0]).unwrap();
assert_eq!(back.body.network_pid(), Some(nit));
assert_eq!(back.encode().unwrap(), bytes); // encode(decode(x)) == x
```

## Design notes

- **Lossless codecs.** Decoders keep every bit: reserved and marker bits,
  adaptation-field and PES-header stuffing, the raw trick-mode byte, the
  forbidden PTS_DTS_flags '01', PCR extensions ≥ 300 and even payload bytes
  of AFC '00'/'10' packets that the syntax does not describe. Only
  structurally impossible input (lengths past the end, failed CRC_32) is
  rejected, with an `Error` naming the field. Encoders recompute lengths
  and CRC_32.
- **Validation is separate.** `packet::check`, `pes::check` and
  `psi::check_{pat,cat,pmt}` report forbidden values as `Violation`s with
  the H.222.0 clause; constructors (`PatEntry::new`, `Cat::section`,
  `LongHeader::new`) set reserved bits to '1'. A capture that violates the
  spec can therefore still be decoded, edited and re-encoded.
- **Streaming reassembly** reports, rather than hides, what it drops:
  `DemuxEvent` (pointer_field mismatch, bad stuffing, unexpected bytes,
  partial section discarded on CC break or TEI) and `PesEvent`; continuity
  events are classified per §2.4.3.3 (`Continuity`, `CcEventKind`).
- No panics on malformed input in decoders.

## Verification

| Test | What it proves | Data |
|---|---|---|
| `tests/prop.rs` | `prop_*`: packet, PES, PAT/CAT/PMT `decode(encode(x)) == x` for generated structures; arbitrary packet / PES / PSI body bytes re-encode byte-identically; sections packetised (packed or one-per-packet, any start CC) reassemble to the same sections; PES packetised (with/without PCR) reassembles | synthetic (proptest; regressions in `prop.proptest-regressions`) |
| `tests/capture.rs` | opt-in, over an ISDB-T capture: every packet decodes and re-encodes byte-identically; no sync loss or skipped bytes; every long section on PIDs 0x0000, 0x0001, 0x0010–0x0014, 0x0024 and the PMT PIDs has CRC_32 residue 0; PAT/CAT/PMT round-trip byte-identically (these checks treat payloads as opaque); every PES on the PMT's elementary PIDs that are unscrambled (transport_scrambling_control '00', e.g. a one-seg service, which is sent in the clear) round-trips byte-identically, scrambled PIDs are skipped; spec violations are printed, not asserted | the file named by the environment variable `ISDB_RS_CAPTURE_TS` (no capture is committed); prints `SKIP` and passes when the variable is unset or the file is missing |
| in-crate `#[cfg(test)]` (`crc`, `cc`, `demux`, `packet`, `pes`, `pcr`, `psi`, `reader`, `section`, `packetize`, `bits`, `descriptor`, `stats`) | spec vectors named after the table/clause (`spec_table2_2_header_layout`, `spec_table2_30_pat_layout`, `spec_2_4_3_3_*` continuity rules incl. duplicates and the PCR exception, `spec_table2_21_*` PES fields); CRC check value, zero residue and table vs bitwise; section header split across packets, TEI / pointer_field discards; reader format autodetection behind garbage, resync after corruption; packetiser layouts at boundary lengths; PCR/90 kHz wrap helpers | synthetic |

Run: `cargo test -p isdb-ts`; the real-data test with
`ISDB_RS_CAPTURE_TS=/path/to/capture.ts cargo test -p isdb-ts --test capture -- --nocapture`
(prints its statistics and violations; obtaining a capture:
[docs/test-data.md](https://github.com/a1678991/isdb-rs/blob/main/docs/test-data.md)). During development, on a
live terrestrial capture of 120 091 packets, it found no sync loss, 825
sections with valid CRC_32 and byte-identical PAT/CAT/PMT round trips
(development observation, not reproducible from the repository).

## Known limitations

- Scope is restricted to ISDB-T use: no Program Stream, no TSDT body
  (`TABLE_ID_TSDT` is only a constant), no typed descriptors beyond the raw
  `tag/length/data` loop (typed ARIB descriptors live in `isdb-si`).
- Pending real-world checks (SPEC-NOTES): PES_extension_field_length
  reading (no PES_extension_flag_2 in the capture), duplicate-packet
  handling and discontinuity_indicator in the reassemblers (neither occurs
  in the capture).
- The 192/204-byte framing detection is a heuristic (5 consecutive sync
  bytes at the unit period), not an H.222.0 rule; prefix/trailer bytes are
  passed through opaquely.
- The PMT CRC_32 citation "Annex B" in §2.4.4.9 is treated as an editorial
  error; Annex A is used for every section.

## See also

- [SPEC-NOTES.md](https://github.com/a1678991/isdb-rs/blob/main/crates/isdb-ts/SPEC-NOTES.md) — interpretations with quoted spec text and
  confirmation status.
- Workspace [README](https://github.com/a1678991/isdb-rs/blob/main/README.md) — ground truth, verification levels,
  conventions; [docs/architecture.md](https://github.com/a1678991/isdb-rs/blob/main/docs/architecture.md).
