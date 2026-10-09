# Transport-stream analysis

`isdb ts` inspects, validates, compares and edits MPEG-2 transport
streams: captures from a tuner, the output of `isdb mux`, or what
`isdb rx` received. It is built only on the decoders of `isdb-ts`,
`isdb-si` and `isdb-text` and shares no code with the multiplexer, so
validating the multiplexer's output is an independent check. Library use:
[`crates/isdb-analyze`](../crates/isdb-analyze/README.md).

Input packets may be 188, 192 (4-byte prefix, e.g. M2TS) or 204 bytes
(16-byte trailer); the framing is detected automatically and the reader
resynchronises after corruption. Common options:

| Option | Meaning |
|---|---|
| `--json` | machine-readable output |
| `--max-packets N` | stop after N packets |
| `--bitrate BPS` | nominal bit rate for timing when the stream has no PCR |

The decoded-field parts of the JSON (`inspect --json` tables and
descriptors, `diff` field paths) are generated from the decoders' Rust
`Debug` representation, so field names and nesting may change between
releases: treat that JSON as **unstable**, and do not build tooling that
depends on its exact shape yet.

Timing (repetition intervals, PCR spacing, PES delays) comes from the PCR,
interpolated between PCR packets (H.222.0 eq. 2-4), not from packet
positions, so captures with null packets stripped are still timed.

The examples below use `test.ts` and `rx.ts` from
[getting-started.md](getting-started.md).

## inspect

```sh
isdb ts inspect test.ts
isdb ts inspect test.ts --decode        # plus the decoded field tree of every section
isdb ts inspect test.ts --json > report.json
```

The report has these parts:

- **Summary** — packet count and framing, sync losses, duration, the
  PCR-derived packet rate, null packets.
- **PIDs** — per PID: packets, share, rate, transport_error_indicator,
  scrambled and continuity-error counts, PCRs, and what the PID carries
  (PAT, NIT, PMT of a program, PCR, elementary stream, null).
- **PCR** — per PCR PID: interval min/mean/max against the 0.1 s limit
  (H.222.0 §2.7.2), discontinuities, and the deviation from a
  constant-rate fit.
- **Programs** — for each PMT: PCR_PID, program descriptors, and each
  elementary stream with its stream_type, packets, rate, PES count,
  stream_id and PTS span, with its ES descriptors.
- **SI** — a decoded summary: PAT, NIT (network name, area code, guard
  interval and mode, frequencies, remote-control key, TS name, service
  list), SDT (provider and service names, running status), BIT, EIT
  events with JST start, duration and title, TOT/TDT time.
- **Sections** — every (PID, table_id, table_id_extension,
  section_number) with count, length, min/mean/max repetition interval and
  version history.

```text
-- SI --
PAT: transport_stream_id 0x79ee network_PID 0x0010 programs 0x0400→0x01f0
NIT tid 0x40 network_id 0x79ee v0 name "テスト" system_management_id 0x0301
  TS 0x79ee onid 0x79ee
    delivery: area 0x5a5 GI 1/8 (2) Mode 3 (2) freq [3900] = ["557.142857"] MHz
    TS info: remote key 9 name "テスト" types
    service list: 0x0400/0x01
SDT tid 0x42 ts 0x79ee onid 0x79ee service 0x0400 type 0x01 "テスト" / "テストHD" sched false p/f true undefined free_CA false
EIT 0x4e service 0x0400 sec 0 event 0x0001 2026-01-01 11:50:00 JST +01:00:00 running "試験放送"
TOT 2026-01-01 12:00:00 JST TDT -
```

`--decode` prints every field of every table and descriptor as the
decoders see it — strings decoded from ARIB 8-unit code, MJD times in JST,
coded values with their table names. The field paths it shows are the ones
`diff` reports and `edit --set` accepts.

## validate

```sh
isdb ts validate capture.ts
isdb ts validate capture.ts --show info            # include informational findings
isdb ts validate capture.ts --fail-on warning      # non-zero exit on warnings too
isdb ts validate capture.ts --json
```

Each finding has a severity, a check identifier, the clause it comes from
(with the quoted text where useful), a location and a count:

```text
WARNING [pid.missing] H.222.0 §2.4.4 ("Every program shall be fully defined within the Transport Stream itself") — program 0x0400: elementary PID 0x0111 (stream_type 0x02) listed in PMT has no packets
```

| Severity | Meaning |
|---|---|
| **error** | a "shall" of H.222.0, ARIB STD-B10 Parts 1–2 or ARIB STD-B32 Part 2 §5.2 is violated |
| **warning** | reserved-bit values, substitutable rules, B10 Table 5-2 repetition criteria (which B10 calls operational, "not the standard value"), appendix guidelines, timing estimates, receiver-compatibility checks |
| **info** | observations (PCR accuracy estimate, measured PCR interval, AAC summary, …) |

`--show` sets the lowest severity printed (default `warning`), `--fail-on`
the lowest that makes the exit status non-zero (default `error`; `never`
always exits 0). A live capture never suppresses a finding: if a
broadcaster violates a rule, it is reported.

The checks, by identifier prefix:

| Prefix | What is checked |
|---|---|
| `transport.*` | sync, transport_error_indicator, scrambling, packet syntax, continuity counters (H.222.0 §2.4.3) |
| `pid.*` | PID allocation (H.222.0 Table 2-3, B10 Table 5-1), PIDs listed in a PMT without packets, unreferenced PIDs |
| `psi.*` | PAT/CAT/PMT syntax and values |
| `section.*` | assembly, CRC_32, table_id allowed on its PID, sub_table completeness, version coherence |
| `si.*` | SI decoding, B10 value conformance (reserved bits, code points, mandatory descriptors), the 4 KB per 32 ms rate per SI PID, copy control type '00' (`si.copy_control`, a receiver-compatibility warning) |
| `tables.*` | tables B10 makes mandatory, and their repetition against B10 Table 5-2 |
| `cross.*` | identities between tables: transport_stream_id, original_network_id, services in PAT vs SDT, NIT, EIT flags |
| `eit.*` | EIT present/following and schedule rules (B10 §5.2.7 and Appendix §1.4) |
| `pcr.*` | PCR missing, interval ≤ 0.1 s, discontinuities, accuracy estimate, unreferenced PCR PIDs |
| `pes.*` | PES headers, stream_id classes, PTS spacing ≤ 0.7 s, T-STD delay estimate, order |
| `aac.*` | ADTS headers of every stream_type 0x0F PID against ARIB STD-B32 Part 2 §5.2 (protection_absent, profile, sampling frequency, channel configuration, adts_buffer_fullness, raw data blocks, ID), quality_indicator vs the measured rate, a per-PID summary. Header level only: crc_check presence is counted, its value is not verified |

After the findings, a table lists every section against its B10 Table 5-2
repetition criterion (`ok`, `exceeded`, or `insufficient capture` when the
capture is shorter than the interval). Gaps at the capture edges count
only as a lower bound ("exceeded (edge gap)").

**Bench expectations.** When checking a received stream against what you
transmitted, `--expect-*` options turn mismatches into errors (check
`expect`):

| Option | Expected value |
|---|---|
| `--expect-tsid N` | transport_stream_id (PAT, SDT/EIT actual) |
| `--expect-onid N` | original_network_id (SDT, NIT TS loop, EIT, BIT) |
| `--expect-nid N` | NIT network_id |
| `--expect-services LIST` | service ids in the PAT and SDT, comma separated |
| `--expect-freq-mhz F` | a frequency the NIT terrestrial delivery descriptor must list |
| `--expect-guard C` / `--expect-mode C` | guard_interval code (0 = 1/32 … 3 = 1/4) / transmission_mode code (0–2 = Mode 1–3), B10 Tables 6-62/6-63 |
| `--expect-area C` | area_code |
| `--expect-remote-key K` | remote_control_key_id |
| `--expect-ts-name S` / `--expect-network-name S` | TS name / network name |

```sh
isdb ts validate rx.ts --expect-tsid 0x79ee --expect-freq-mhz 557.142857 --expect-remote-key 9
```

How each check reads the spec text (and why some are warnings, not
errors) is recorded in
[`crates/isdb-analyze/SPEC-NOTES.md`](../crates/isdb-analyze/SPEC-NOTES.md).

## diff

```sh
isdb ts diff ours.ts reference.ts
isdb ts diff ours.ts reference.ts --all     # every EIT schedule section, not a per-sub_table summary
```

Compares two streams table by table: the PID inventory with rates side by
side, then every section key with its count, maximum interval and length
in each stream, marked `identical`, `ONLY IN A` / `ONLY IN B`, or with
the list of field changes found by decoding both:

```text
pid=0x0010 tid=0x40 ext=0x79ee sn=0 NIT actual — x1|x1 max interval -|- s len 53|53 2 field changes
    header.version_number: 0 → 5
    body.transport_streams[transport_stream_id=0x79ee].descriptors[TsInformation].remote_control_key_id: 0x09 → 0x03
```

Typical uses: the multiplexer's output against a live capture (what does a
real station send differently?), a received stream against the transmitted
one, a stream before and after `edit`.

## edit

`isdb ts edit IN OUT` applies experiment transforms to synthetic streams
and to streams you produced or have the rights to (it is a test tool, not
a way to alter broadcast signalling such as copy control in recordings of
others), and writes a TS that stays valid: sections are re-packetised into their PID's original packet
slots (H.222.0 §2.4.4 pointer_field and stuffing), continuity counters are
renumbered, CRC_32 is recomputed by the codecs, and packet positions are
kept (freed slots become null packets), so a constant-rate stream stays
constant-rate. Operations are applied in this order, each repeatable:

| Option | Effect |
|---|---|
| `--null-pid LIST` | replace every packet of these PIDs with null packets |
| `--remove PID:TID[:EXT[:SN]]` | remove the matching sections |
| `--replace SEL=FILE` | replace the matching sections with those of FILE (a `.ts` capture or a hex fixture from `extract`), matched by section_number |
| `--set EXPR` | typed field mutation (below) |
| `--insert SEL=FILE@MS` | insert the matching sections of FILE into null-packet slots every MS milliseconds (timed by PCR), starting at `--insert-offset-ms` |
| `--null-slots never\|needed\|always` | where larger rewritten sections may take extra room: only the PID's own slots, null packets once behind the original schedule (default), or any null packet |

Selectors are `PID:TID[:EXT[:SN]]`, numbers in hex or decimal, e.g.
`0x0010:0x40` (NIT actual) or `0x0012:0x4e:0x0400:1`.

```sh
# change the NIT version and the remote-control key, then see exactly that in the diff
isdb ts edit test.ts edited.ts --set nit.version=5 --set 'nit.ts[0].desc[0xCD].remote_control_key_id=3'
isdb ts diff test.ts edited.ts

# drop the BIT and blank the PCR PID
isdb ts edit test.ts e3.ts --remove 0x0024:0xC4 --null-pid 0x01ff

# transplant a NIT from another stream, or insert it every 500 ms from t = 1 s
isdb ts extract edited.ts --pid 0x0010 -o nit.hex
isdb ts edit test.ts e2.ts --replace 0x0010:0x40=nit.hex
isdb ts edit test.ts e4.ts --insert 0x0010:0x40=nit.hex@500 --insert-offset-ms 1000
```

Each operation reports, per PID, sections in / changed / removed /
inserted / out, the slots used and the maximum delay; sections that could
not be placed are reported as `DROPPED`. `--json` prints the report as
JSON.

### Mutation expressions

`--set` decodes the section, changes the typed structure and encodes it
again — never a byte patch — so lengths and CRC_32 stay correct:

```text
expr  := table ('@' tid)? ('[' ext ']')? ('.' seg)* '=' value
seg   := field ('[' sel ']')?      sel: decimal index | 0x… id or tag | 0xTT#k (k-th, 0-based)
```

- Tables: `pat`, `cat`, `pmt[program]`, `nit` (`@0x41` for NIT other),
  `sdt` (`@0x46`), `bat`, `eit[service_id]` (`@0x50` …), `tot`, `tdt`,
  `bit`.
- Header fields of any long-form table: `version`, `tidext`, `cni`, `sn`,
  `lsn`.
- Descriptor loops are `desc` (aliases `descriptors`, `program_info`,
  `first`): `desc.add=<hex of tag+length+body>`, `desc.del=0xTT`,
  `desc[0xTT]=<hex>` (replace), `desc[0xTT].<field>=value`. A final
  segment `add` / `del` adds or removes a list element.
- Values: integers (decimal or `0x…`), booleans, `"text"` (ARIB-encoded)
  or `hex:…` for strings, 3-letter codes, hex byte strings, `[a,b,…]`
  lists, `YYYY-MM-DD HH:MM:SS` (JST) times, `HH:MM:SS` durations, `HH:MM`
  offsets, `none` for absent optional fields.

Examples: `nit.ts[0].desc[0xFA].guard_interval=2`, `nit.version=24`,
`pat.add=0x0e28:0x1fff`, `sdt.services[0x0400].desc[0x48].service_name="ABC"`,
`pmt[0x0408].streams.add=0x0f:0x0111`, `eit[0x0408].events[0].duration=01:00:00`.
The field names are those printed by `isdb ts inspect --decode`.

## extract

```sh
isdb ts extract capture.ts                       # every section, latest version of each
isdb ts extract capture.ts --pid 0x0010 --tid 0x40 -o nit.hex
isdb ts extract capture.ts --all-versions        # every distinct body of every version
```

Writes sections as a text fixture, one per line: `pid table_id hex`, after
a `#` comment header. Fixtures can be fed back to `edit --replace` /
`--insert`, and to the multiplexer's `replace_sections` / `extra_sections`
/ `[[raw_table]]` ([mux-config.md](mux-config.md)), which take the hex of
one section per string.

```text
# sections of test.ts (pid table_id hex)
0x0010 0x40 40f03279eec10000f00b40051b6f463948fe020301f01a79ee79eef014fa045a5a0f3ccd0709141b6f4639484103040001e315344c
```

Captures may contain material you have no rights to redistribute; keep
extracted fixtures from broadcasts out of public repositories
([test-data.md](test-data.md)).
