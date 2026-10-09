# arib-analyze

Transport stream analysis and experiment tooling built only on the spec
decoders of `arib-ts` (H.222.0), `arib-si` (ARIB STD-B10) and `arib-text`
(ARIB STD-B24 strings): one scan of a capture feeds an inspection report,
a conformance validation with clause citations and severities, a
table/field-level diff of two captures, and edits that produce a valid TS
(null PIDs, remove/replace/insert sections, typed field mutation). It is the
library behind `arib ts inspect|validate|diff|edit|extract`.

## Spec scope

- ITU-T H.222.0 (05/2006): packet rules (`arib_ts::packet::check`),
  continuity (§2.4.3.3), PCR interval ≤ 0.1 s (§2.7.2) and ±500 ns
  accuracy estimate (§2.4.2.2), PCR-interpolated time base (eq. 2-4),
  PSI (§2.4.4: PAT/PMT/CAT, table_id exclusivity on 0x0000/0x0001, version
  coherence), PID allocation (Table 2-3), PES timing (§2.7.4 0.7 s PTS
  interval, §2.4.2.6 T-STD delay estimate, Table 2-22 stream_id classes),
  stream_type names (Table 2-34), section packetisation for `edit`
  (§2.4.4 pointer_field / stuffing).
- ARIB STD-B10 v5.13: PID/table allocation (Part 1 and Part 2 Table 5-1),
  transmission level and repetition criteria (Part 2 §5.1.3 Table 5-2),
  4 KB per 32 ms per SI PID (§5.1.4), sub_table/version rules (§5.1.1),
  EIT p/f and schedule rules (§5.2.7, Appendix §1.4), cross-table identities
  (§5.2.4, §5.2.6, Appendix §1.1/§1.3, Table 6-1), plus all value checks of
  `arib_si::conformance`.
- ARIB STD-B32 v3.11 Part 2: §5.2.1–5.2.2 MPEG-2 AAC ADTS header
  restrictions on every stream_type 0x0F PID (frames reassembled from the
  PES payloads in `aac`; crc_check presence counted, its value not
  verified; no raw_data_block rules), Attachment Ch. 3 Table 1
  quality_indicator vs the measured rate.

Severities: **error** = a "shall" of H.222.0 / B10 Part 1–2 / B32 Part 2 §5.2 violated;
**warning** = reserved-bit values, substitutable rules, Table 5-2
frequencies, Appendix guidelines, timing estimates, receiver-compatibility
checks; **info** = observations. Live data never suppresses a finding.

## Position in the stack

- Depends on: `arib-ts`, `arib-si`, `arib-text`; `arib-mux` only for the
  B32 Part 2 ADTS rule identities and texts, the quality modes
  (`arib_mux::adts::b32`) and, in tests, `SilentFrame` — the ADTS header
  parse is re-implemented independently in `aac`; `serde`/`serde_json`
  for JSON reports, `anyhow`.
- Used by: `arib-cli` (`arib ts inspect|validate|diff|edit|extract`, used
  on captures, `arib mux` output and `arib rx` output alike). It shares no
  packing code with `arib-mux`, so validating mux output is an independent
  check. The AAC checks are header-level only (`arib-mux` SPEC-NOTES "AAC
  ADTS: raw_data_block walk removed").

## Key types and entry points

| Module | Entry points |
|---|---|
| `scan` | `scan_path`, `scan_bytes`, `scan_packets` → `Capture` (per-PID stats, CC events, PCRs, packet violations, every PSI/SI section with packet positions, PES timestamps); `read_packets` (188/192/204-byte framing via `arib_ts::reader`) |
| `inspect` | `inspect(&Capture, &InspectOptions)` → `InspectReport` (`to_text()`, `Serialize`) |
| `validate` | `validate(&Capture, &ValidateOptions)` → `ValidationReport` of `Finding { severity, check, clause, location, message, count }` and B10 Table 5-2 `FrequencyRow`s; optional bench `Expectations` (TSID, ONID, network_id, service ids, frequency, GI, mode, area code, remote-control key, names) |
| `diff` | `diff(&a, &b, &DiffOptions)` → `DiffReport` (presence, repetition and field changes per (PID, table_id, table_id_extension, section_number), PID inventory) |
| `edit` | `apply(&mut packets, &[EditOp], &EditOptions)`; `EditOp::{NullPids, Remove, Replace, Insert, Mutate}`; `Selector` (`PID:TID[:EXT[:SN]]`), `NullSlots` policy, `extract` / `sections_from_hex` / `load_sections` |
| `mutate` | `Expr::parse("nit.ts[0].desc[0xFA].guard_interval=2")` — decode → modify typed struct → encode, never a byte patch |
| `aac` | `AdtsTrack` (ADTS reassembly across PES packets with resync, per-rule frame counts, rate, fullness range), independent `header` parse, `check_name` (`aac.*` identifiers) |
| `decode`, `tree`, `si`, `names`, `timebase` | section → `Table` (PSI or `AnyTable`); generic field tree from the decoders' `Debug` output (JSON, dumps, diffs); human SI summary; spec name tables; packet index → time |

```rust
use arib_analyze::edit::{self, EditOp, EditOptions};
use arib_analyze::mutate::Expr;
use arib_analyze::validate::{Severity, validate};
use arib_ts::cc::Counter;
use arib_ts::packetize::{Packing, packetize_sections};
use arib_ts::psi::{Pat, PatEntry};

// A stream that carries only a PAT (H.222.0 Table 2-30).
let pat = Pat::section(0x7FD5, 1, vec![PatEntry::new(0x0408, 0x01F0)]).encode()?;
let pkts = packetize_sections(0x0000, &[pat], Packing::OnePerPacket, &mut Counter::new(0))?;
let cap = arib_analyze::scan_packets(&pkts, &Default::default());
assert_eq!(cap.pat().unwrap().header.table_id_extension, 0x7FD5);

// B10 Table 5-2 makes CAT/PMT/NIT/SDT/EIT p/f/TDT-or-TOT mandatory.
let report = validate(&cap, &Default::default());
assert!(report.findings.iter().any(|f| f.severity == Severity::Error
    && f.check == "tables.mandatory" && f.clause.contains("Table 5-2")));

// Typed mutation re-encodes the section; the diff shows exactly that field.
let mut edited = pkts.clone();
edit::apply(&mut edited, &[EditOp::Mutate(Expr::parse("pat.version=2")?)], &EditOptions::default())?;
let d = arib_analyze::diff::diff(&cap, &arib_analyze::scan_packets(&edited, &Default::default()), &Default::default());
let changes: Vec<_> = d.sections.iter().flat_map(|s| &s.changes).collect();
assert_eq!(changes.len(), 1);
assert_eq!(changes[0].path, "header.version_number");
# Ok::<(), anyhow::Error>(())
```

## Design notes

- **Decoders only.** Sections come from `arib_ts::demux::SectionAssembler`,
  PES headers from `arib_ts::pes`, tables from `arib-ts`/`arib-si`; no
  multiplexer logic is reused, so mux and validator cannot share a bug.
- **Time from PCRs.** Repetition intervals, PCR intervals and PES delays use
  PCR values (interpolated per eq. 2-4), not packet positions, so captures
  with null packets stripped are still timed; without PCRs a nominal bit
  rate (`nominal_bps`) can be given. Capture-edge gaps are a lower bound
  ("exceeded (edge gap)", "insufficient capture").
- **One field tree.** `tree` parses the decoders' `#[derive(Debug)]` output
  into a `Node` tree (strings decoded, MJD times in JST, codes as text), so
  JSON, dumps and diffs need no second hand-written description of each
  table.
- **Edits keep the TS valid.** Sections are repacketised per §2.4.4 into
  the PID's original packet slots (never earlier than the original section
  start), continuity counters are renumbered, CRC_32 is recomputed by the
  codecs, and packet positions are kept (unused slots become null packets),
  so a CBR stream stays CBR. Extra room comes from null packets per
  `NullSlots`.

## Verification

| Test | What it proves | Data |
|---|---|---|
| `tests/checks.rs` | one test per check: a synthetic stream (built with the `arib-ts`/`arib-si` encoders in `tests/common/mod.rs`) violating exactly one rule yields that finding, severity and clause; the baseline stream has no errors; bench `Expectations`; `si.copy_control` warning | none |
| `tests/checks.rs` `aac_adts_rules`, `aac_quality_indicator_consistency` | a stream_type 0x0F PID carrying conforming silent ADTS has no `aac.*` finding above info; one header field changed per case (protection_absent, adts_buffer_fullness 0x7FF, profile, sampling_frequency_index, ID, raw data blocks, channel_configuration 7) gives its error and clause; EIT quality_indicator '01' at 75 kbit/s → warning, '11' → info | none |
| `tests/edit.rs` | identity rewrite is byte-exact; null PID keeps positions; a NIT mutation diffs as exactly that field; larger replacements use null slots; remove/insert; `pat.add`; `extract` round-trips through the hex loader; every output CRC/CC clean per the validator | none |
| `tests/real_data.rs` | opt-in, on a descrambled live terrestrial capture: inspect decodes every section (no section error, every SI section and descriptor typed except B10-external tags 0xC9/0xCB/0xCC; NIT network name, TS information, terrestrial delivery system, SDT service names, EIT p/f titles and TOT present); validation reports no section/CC error and no `aac.*` finding above info (other operational findings such as PCR intervals are printed); identity rewrite keeps the PSI/SI section sequence of every SI PID and every PMT PID of the PAT; null-PID (first ES of the first PMT) and NIT-mutation (guard_interval) edits stay clean and diff as exactly that field; with a second stream written by this project's multiplexer: NIT transplant is CRC/CC clean and an ours-vs-capture report is printed | the files named by `ISDB_RS_CAPTURE_TS` (live capture) and `ISDB_RS_OUR_TS` (own multiplex) |
| unit tests (`src/mutate.rs`, `src/tree.rs`, `src/diff.rs`, `src/timebase.rs`) | mutation grammar examples parse and re-encode; `Debug` tree parsing/normalisation and JST rendering; descriptor-list matching in diffs; PCR interpolation and its inverse | none |

No capture is committed. Each `real_data.rs` test prints a `SKIP` line and
returns (passes) when its environment variable is unset or names a missing
file. On a live terrestrial capture (2026-10-07) validation found one
operational PCR-interval error (the one-seg service's PCR PID), no
section/CC error, and both AAC PIDs (full-seg and one-seg) without an
`aac.*` finding above info. Run with `--nocapture` to see the reports:

```text
cargo test -p arib-analyze
ISDB_RS_CAPTURE_TS=/path/to/capture.ts ISDB_RS_OUR_TS=/path/to/ours.ts \
    cargo test -p arib-analyze --test real_data -- --nocapture
```

## Known limitations

- Timing checks on captures with null packets stripped are estimates:
  section times are interpolated between PCRs (≈ ±10 ms observed on a live capture),
  so marginal Table 5-2 frequency warnings can be measurement noise; PCR
  accuracy (±500 ns) is only estimated when null packets are present (CBR).
- The T-STD delay check uses the first byte of each PES only and is a
  warning.
- EIT p/f "one event" rule does not distinguish NVOD reference services.
- Conditional Table 6-1 descriptors (terrestrial delivery, system
  management) are warnings because the TS may not be terrestrial.
- `si.copy_control` (copy_control_type '00' in the Annex F user_defined
  layout) is a receiver-compatibility warning from bench evidence (Sony
  BDZ-series recorder, 2026-10-08), not a B10 "shall"; the Annex F layout is
  informative in B10.
- Mutation covers `pat`, `cat`, `pmt`, `nit`, `sdt`, `bat`, `eit`, `tot`,
  `tdt`, `bit` (see the `mutate` module doc for the grammar).

See [SPEC-NOTES.md](SPEC-NOTES.md) for how each check reads the spec text,
and the workspace [README](../../README.md) for the ground truth and
verification policy.
