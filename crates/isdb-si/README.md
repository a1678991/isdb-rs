# isdb-si

Typed, lossless codecs for ARIB STD-B10 service information: the SI tables
(NIT, BAT, SDT, EIT, TDT, TOT, RST, ST, PCAT, BIT, NBIT, LDT), the B10
descriptors, MJD/BCD time coding and the table-coded field values, plus a
separate conformance checker that reports every value the B10 text forbids
or reserves. The section envelope (header, `section_length`, CRC_32) comes
from `isdb-ts`; strings are kept as raw ARIB 8-unit bytes
([`isdb_text::AribString`]).

## Spec scope

ARIB STD-B10 v5.13 (`specs/ARIB-STD-B10-v5.13.txt`), Parts 1 and 2, and
ITU-T H.222.0 (05/2006) for the section envelope and the conditional access
descriptor.

| Area | Clauses / tables | Module |
|---|---|---|
| Table bodies | Part 2 §5.2.4–§5.2.15, Tables 5-3 … 5-15 | `tables` |
| PID / table_id dispatch | Part 2 §5.1.3, Tables 5-1, 5-2 | `any` |
| Descriptors (44 B10 tags + 0x09 CA) | Part 2 §6.2, Table 6-1; tags Part 1 Table 5-3; CA from H.222.0 §2.6.16 Table 2-58 | `descriptor` |
| Time | Part 2 §5.2.7–§5.2.9, §6.2.25, Annex C; JST = UTC+9 (§3.1) | `time` |
| Typed field values | Part 2 Tables 6-5, 6-39, 6-43, 6-45, 6-60, 6-60-1; Annex F Tables F-2/F-4; §6.2.23 maximum_bitrate unit | `values` |
| Value conformance | Part 2 §3.1 reserved bits, §5.1.1 / per-table length limits, Table 5-1 placement, Table 6-1 unconditional mandatory descriptors | `conformance` |

Typed descriptor tags are listed in `descriptor::TYPED_TAGS`.

## Position in the stack

- Depends on: `isdb-ts` (bit I/O, section envelope `Long`/`Short`,
  `Error`), `isdb-text` (`AribString`).
- Used by: `isdb-analyze` (inspection, validation, diff, mutation),
  `isdbt-mux` (PSI/SI generation); `isdb-cli` through those.

## Key types and entry points

- `AnyTable::decode(pid, section)` / `AnyTable::encode()` — dispatch a
  complete section to its typed table. Sections whose (PID, table_id) Table
  5-1 does not assign, and table_ids without a typed decoder (SDTT, CDT,
  company tables …), come back as `AnyTable::Unknown(RawSection)` with the
  CRC still verified for long-form sections. `AnyTable::descriptors()` walks
  every descriptor loop.
- `tables::{Nit, Bat, Sdt, Eit, Pcat, Bit, Nbit, Ldt}` = `Long<…Body>`,
  `tables::{Tdt, Tot, Rst}` = `Short<…Body>`, `tables::StuffingSection`
  (coded whole, no CRC; see SPEC-NOTES).
- `Descriptor` — one variant per typed tag, `Descriptor::Unknown { tag,
  data }` otherwise; `Descriptor::decode_body(tag, data)`.
- `time::MjdTime` (40-bit field, `to_jst`/`from_jst`/`to_utc`/`from_utc`),
  `time::BcdDuration`, `time::BcdOffset`, `time::{mjd_to_date,
  date_to_mjd, weekday, mjd_to_week}`.
- `conformance::check(pid, &table)` / `conformance::check_descriptors` →
  `Vec<Deviation>` (clause, `Kind`, location, message).
- `values::*` — enums with the exact printed codes; reserved codes decode to
  a `Reserved`/`Extension` variant carrying the value, so the typed view
  never fails.

```rust
use isdb_si::{AnyTable, conformance};

// time_date_section (B10 Part 2 Table 5-8) carrying the §5.2.7 example
// time 0xC079124500 = 1993-10-13 12:45:00 JST.
let tdt = [0x70, 0x70, 0x05, 0xC0, 0x79, 0x12, 0x45, 0x00];
let AnyTable::Tdt(t) = AnyTable::decode(0x0014, &tdt)? else { unreachable!() };
assert_eq!(t.body.jst_time.to_jst().unwrap().to_string(), "1993-10-13 12:45:00");
assert_eq!(AnyTable::Tdt(t.clone()).encode()?, tdt); // lossless
assert!(conformance::check(0x0014, &AnyTable::Tdt(t)).is_empty());

// The 2 reserved bits set to '00' still decode (and re-encode
// identically); conformance reports them separately.
let odd = [0x40, 0x05, 0xC0, 0x79, 0x12, 0x45, 0x00];
let t = AnyTable::decode(0x0014, &[&[0x70][..], &odd].concat())?;
let dev = conformance::check(0x0014, &t);
assert!(dev.iter().any(|d| d.kind == conformance::Kind::ReservedBits));
# Ok::<(), isdb_ts::Error>(())
```

## Design notes

- **Lossless.** Every reserved / reserved_future_use field is stored
  verbatim; loop lengths, `descriptor_length`, `section_length` and CRC_32
  are recomputed on encode, so `encode(decode(x)) == x` for every section
  that decodes. A typed descriptor must account for every byte of its
  `descriptor_length` or decoding fails (no silent truncation).
- **Errors are not downgraded.** Length, CRC and syntax errors are `Err`;
  only a missing typed decoder yields `Unknown`.
- **Conformance is separate.** Codecs accept any field value the syntax can
  carry (e.g. reserved bits '0', section_length up to 4093 for every
  table); `conformance` reports what B10 states (reserved bits '1', 1021
  vs 4093 length limits, reserved code points, invalid BCD/MJD, Table 6-1
  unconditional mandatory descriptors, Table 5-1 placement). Only rules
  stated in the B10 text are checked, not operational guidelines.
- **Exact time arithmetic.** Annex C formulas are evaluated as exact
  rational floors, never floating point; conversion returns `None` outside
  MJD 15079 (1900-03-01) … 88127 (2100-02-28).

## Verification

| Test | What it proves | Data |
|---|---|---|
| `tests/spec_vectors.rs` | `spec_*`: Annex C worked example, §5.2.7 JST example, BCD durations, exhaustive day-by-day calendar continuity over the Annex C range; every table (NIT … LDT) and several descriptors built field by field from the syntax tables, decoded, checked and re-encoded; descriptors rejecting unaccounted bytes; dispatch errors; reserved-bit conformance | none |
| `tests/spec_values.rs` | every `values` variant maps to exactly the code printed in its B10 table (6-5, 6-39, 6-43, 6-45, 6-60, 6-60-1, Annex F); maximum_bitrate unit; the copy control / component / audio component descriptors of the synthetic multiplex (values as live terrestrial broadcasters send them) decode to the expected variants | `tests/synthetic` (built in code) |
| `tests/prop_roundtrip.rs` | proptest: round trip of every typed descriptor (a test asserts the strategies cover `TYPED_TAGS`) and every table body; decode-side fuzzing (any bytes a decoder accepts re-encode identically) | none |
| `tests/synthetic_si.rs` | every section of a synthetic terrestrial multiplex (NIT, SDT, BIT, EIT p/f, L-EIT, EIT schedule basic 0x50/0x51 and extended 0x58/0x59, TOT, TDT; 21 descriptor tags incl. terrestrial delivery with several frequencies, TS information with two transmission types, partial reception, logo transmission schemes 1/2/simple, event group, extended event items spanning several descriptors, SI parameter, extended broadcaster, emergency information): CRC_32 valid, typed decode with no `Unknown` table or descriptor, byte-identical re-encode, coverage of every table_id / tag listed, and no conformance deviation except the short event descriptor missing from event-group-only and extended-only events (as on live data, see SPEC-NOTES) | `tests/synthetic` (built in code) |
| `src/values.rs` (unit) | Annex F user_defined bijection and code helpers | none |
| `tests/capture.rs` | opt-in: every distinct SI section on PIDs 0x0010–0x0012, 0x0014, 0x0024, 0x0026, 0x0027 of a real capture: CRC_32 valid, typed decode (no `Unknown` table; no `Unknown` descriptor except B10-external 0xC9/0xCB/0xCC), byte-identical re-encode; prints counts and conformance deviations | file named by `ISDB_RS_CAPTURE_TS` (prints `SKIP` and passes when unset or missing) |

`tests/synthetic/mod.rs` builds the sections with this crate's typed
encoders and `isdb-text` (fictional names and texts only), so every test in
this crate except the opt-in `tests/capture.rs` runs without external data.
`tests/capture.rs` (and the capture tests of `isdb-ts`, `isdb-text` and
`isdb-analyze`) read the terrestrial capture named by
`ISDB_RS_CAPTURE_TS` ([docs/test-data.md](../../docs/test-data.md)).
During development, on a live terrestrial capture, all 144 distinct SI
sections decoded typed (no `Unknown` table or descriptor) and re-encoded
byte-identically; the only conformance deviations were the 217 EIT events
without a short event descriptor described in SPEC-NOTES.

```text
cargo test -p isdb-si
cargo test -p isdb-si --test synthetic_si -- --nocapture   # summary report
ISDB_RS_CAPTURE_TS=/path/to/capture.ts cargo test -p isdb-si --test capture -- --nocapture
```

## Known limitations

- No typed decoders for tables and descriptors that B10 delegates to other
  standards: SDTT (0xC3), CDT (0xC8), download content (0xC9), CA contract
  information (0xCB), CA service (0xCC) stay `Unknown` until ARIB STD-B21 /
  B25 are admitted as ground truth.
- PCAT `content_descriptor_length` scope is a literal reading, unconfirmed
  (no PCAT in any capture).
- Conditional Table 6-1 rules ("for digital terrestrial broadcasting",
  "either in PMT or NIT", …) are not checked here (they need context beyond
  one section; `isdb-analyze` checks some cross-table ones). The *2
  "substitutable" note cannot be verified, so absence is always reported.
- `audio_component_descriptor` ES_multi_lingual_flag "reserved" is not
  checked (self-contradictory text).
- `descriptor` does its own tag/length framing and does not yet share
  `isdb_ts::descriptor::Descriptor` (the raw H.222.0 form used in PMT/CAT
  loops).

See [SPEC-NOTES.md](SPEC-NOTES.md) for every interpretation (with quoted
text and status) and the workspace [README](../../README.md) for the ground
truth and verification policy.
