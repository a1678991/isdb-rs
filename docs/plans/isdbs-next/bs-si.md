# Satellite-shaped (BS) SI in the multiplexer: investigation and design

Investigation only, 2026-10-30, for Work item 2 of
[isdbs-next.md](../isdbs-next.md). Repository at `371e72b`; PIX-DT295 driver at
`/path/to/PIX-DT295` `72872e5`. Nothing was transmitted and no device was opened.
Marks: **[spec]** a clause of a local spec; **[live]** seen in a BS recording
(see §3.3); **[knowledge]** neither (from tables of other projects or memory);
**[confirm]** still to be settled on the bench.

**Status (2026-10-31):** pieces A (`82da6c5`), B (`a82008d`), C (`b9cc79e`)
and D (bench and CLI glue, §6.3) are merged and verified offline
(VERIFICATION.md "ISDB-S BS-shaped SI, offline"). The bench acceptance
(§6.4 item 7) is pending until TX1 is reconnected to the PIX-DT295
(isdbs-next.md, bench notes of 2026-10-31). As implemented,
`satellite.channel` takes the odd channels BS1…BS23 only (§6.1).

## 0. Summary

- Today's multiplexer (`isdbt-mux`) makes terrestrial SI only. The terrestrial
  parts are fixed in five places: the `[network]` schema (required `area_code`,
  `remote_control_key_id`, UHF `channels`), the NIT builder (0xFA, 0xCD, 0xFB
  always; broadcasting_identifier '000011'), the BIT builder (0xCE type 0x1),
  the config checks, and the network_id range warning. SDT, EIT, TOT, PAT and
  PMT have nothing terrestrial apart from where they are placed in the frame.
  The ISDB-T slot model (layers, partial reception) stays as the packet clock;
  generalising it is item 3.
- `isdb-si` already encodes and decodes everything BS needs: 0x43, 0x41, 0x40,
  0xFE, 0xD8, 0xD7, 0xCD. 0x43 keeps its BCD fields raw, has no typed view, no
  spec vector and no conformance check. 0xCE has no satellite form, and B10
  Table 6-84 defines none.
- B10 has almost no rules that differ for BS. The ones that do: 0x43 is
  "Mandatory in digital satellite broadcasting" (Table 6-1); BS network_id is
  0x0004 (Annex N); the broadcasting_identifier is '000010' (Table 6-36);
  TS_ID is "unique within original network"; service_id is "unique within
  Japan for the same broadcasting medium" (Part 1 Table 7-1). TR-B15, the BS
  operational guideline, is not local. A 2023 BS recording, read here for its
  structure only, settles the shape:
  - NIT network loop: 0x40 + 0xFE 0x0201;
  - NIT TS loop: 0x41 then 0x43, with no 0xCD and no 0xFB;
  - BIT: 0xD7 in the first loop, and 0xD8 + 0x41 (+ 0xD7) per broadcaster, with
    no 0xCE.
- `isdb ts validate` already accepts BS SI with 0 errors. Its only complaint is
  the warning `cross.nit` "no terrestrial_delivery_system_descriptor"
  (`validate.rs:1307-1310`). It does not check network_id against the delivery
  system. A prototype made with today's knobs (`[nit] replace_sections` from
  `tools/bs-nit-section.py`, a raw 0x41 in the BIT) validates with 0 errors and
  that one warning (§4.2). The bench can use it now.
- `dt295` has no SI readout. It selects the TS by TS_ID alone (TC90532 regs
  0x8F/0x90) and records it. The SI readout that exists is mirakc with
  `recisdb-dt295` (it found the ISDB-T bench service before) plus
  `isdb ts inspect` on a `dt295 stream` recording.
- Design: `[network] kind = "terrestrial" | "bs"` (default terrestrial,
  byte-identical output) and a `[network.satellite]` table (`channel = "BS1"`,
  `orbital_position = "110.0E"`). Everything else in 0x43 is derived. Typed
  `services` go into the BIT broadcaster loop. Validate gets satellite-aware
  checks, and the bench gets placeholder identities: TS_ID 0x4017,
  service_id 0x03E7, broadcaster_id 0xEE. Work split: A `isdb-si` → (B
  `isdbt-mux` ∥ C `isdb-analyze`) → D bench / CLI (§6.6).

## 1. SI generation in `isdbt-mux` today (question 1)

### 1.1 Structure

| Part | Where | What |
|---|---|---|
| Schema | `config.rs:53-98` `Config`; `:100-141` `Network`; `:153-163` `Transport`; `:509-537` BIT broadcasters | serde, `deny_unknown_fields`; module doc "one terrestrial transport stream" (`config.rs:1-11`) |
| Config checks | `validate.rs:258-305` (typed-vs-raw descriptor collisions per loop), `signalling.rs:930-975` (`RemoteControlKeyId` 1..=12, "terrestrial receivers") | |
| Build | `si.rs:629-` `SiBuilder::build` dispatches per `TableKind`; PAT `:832`, CAT `:851`, PMT `:895`, NIT `:1060-1157`, SDT `:1165-1214`, EIT p/f `:1396`, schedule `:1433`, TOT `:1544`, BIT `:1595-1657` | typed `isdb-si` bodies, then `post()` applies `TableOpts` (add_descriptors, header overrides, byte_patches, replace/extra sections) |
| Placement | `si.rs:251-262` `control_layer` (B31 Part 2 Table 3-1), `:718-741` `default_layer`, `:695-716` PIDs (B10 Table 5-1), `:743-755` intervals (Table 5-2) | ISDB-T layers/frame (`mux.rs`, `MultiplexPattern`) |
| Mux start | `mux.rs:239-255` | warns when network_id is outside 0x7880–0x7FE8; errors when `channels` and `frequencies` are both empty |

### 1.2 Hard-wired terrestrial assumptions

| Assumption | Evidence |
|---|---|
| `area_code` and `remote_control_key_id` are required keys | `config.rs:109`, `:112` (non-`Option`) |
| Frequencies are UHF channels 13..62 → 1/7 MHz codes | `config.rs:116-122`; `si.rs:286-299`, `:1078-1085` |
| 0xFA is always sent, with GI and mode taken from ISDB-T `Params` | `si.rs:1086-1093`, `:265-283` |
| 0xCD is always sent, `transmission_types` per ISDB-T layer | `si.rs:1104-1119`; `config.rs:136` |
| 0xFB is sent when the ISDB-T layout has partial reception and a service is on layer A | `si.rs:1130-1136`; `config.rs:140` |
| system_management_id identifier '000011' (terrestrial TV) | `si.rs:66-69`, `:1067-1075` |
| BIT: 0xCE only as broadcaster_type 0x1 (`terrestrial = {…}`) | `si.rs:1609-1626`; `config.rs:518-537` |
| network_id range 0x7880–0x7FE8 (warning) | `mux.rs:239-249` |
| `channels` or `frequencies` must be non-empty | `mux.rs:250-255` |
| Typed-loop sets: 0xFA/0xCD/0x41 always, 0xFB conditional | `validate.rs:278-300` |
| `--relay` template and `isdb mux --channel` are terrestrial | `relay_template.rs:37-104`; `isdb-cli/src/mux.rs:35-37`, `relay.rs:51-55` |

Not hard-wired:

- ts_id = network_id holds only in the examples (`bench-minimal.toml`
  network_id and TS_ID both 0x79EE). The code takes them separately
  (`config.rs:158`).
- original_network_id defaults to network_id (`config.rs:1488-1493`), which
  is B10 §5.2.4's rule and also right for BS.
- SDT (`si.rs:1165-1214`) and EIT (`:1420`, `:1531`) use only the onid, the
  TS_ID and the per-service fields.
- service_ids are free 16-bit values, with no range check.

### 1.3 What the bench sends today

`tools/isdbs-bench-tx.sh:30-45` rewrites `examples/bench-minimal.toml`. It
declares the media streams and changes the TS_ID to 0x4010. Everything else
stays as it is:

- network_id and onid 0x79EE, area 0x5A5, key 9, UHF 27;
- services 0x0400 (layer B) and the one-seg 0x0588 (layer A, 0xC0);
- BIT broadcaster 0x01.

The result is the "terrestrial-shaped" NIT of VERIFICATION.md:1753. `--ts
0x4010=` (`:58`) puts 0x4010 in the TMCC. The WORKDIR with the generated
`bs-bench.toml` was not found on disk; it is regenerated on each run.

## 2. What `isdb-si` provides for satellite (question 2)

| Descriptor | Codec | Typed view / checks | Tests |
|---|---|---|---|
| 0x43 satellite_delivery_system (B10 §6.2.6 Table 6-8) | `descriptor/dvb.rs:142-188`, registry `descriptor/mod.rs:119-120`. Fields are raw: `frequency` u32 (8 BCD), `orbital_position` u16 (4 BCD), `west_east_flag`, `polarization` (2), `modulation` (5), `symbol_rate` (28 bits, 7 BCD; "system_rate" in the table, SPEC-NOTES "semantics stating wrong widths"), `fec_inner` (4) | None. No Table 6-9/6-10/6-11 enums in `values.rs`, no BCD accessors, no arm in `conformance.rs:679-926` | Proptest round-trip only (`tests/prop_roundtrip.rs:78-99`); no `spec_` vector |
| 0x41 service_list (§6.2.14) | `dvb.rs:83-122` | Mandatory in the NIT is checked (`conformance.rs:485-490`) | prop + vectors |
| 0x40 network_name, 0xFE system_management (§6.2.21) | `dvb.rs:72-75`; `arib.rs:1637-1670` with accessors `broadcasting_flag()` / `broadcasting_identifier()` | no Table 6-36 enum | prop |
| 0xCD TS_information (§6.2.42) | `arib.rs:431-500` | `conformance.rs:792` | prop + vectors |
| 0xCE extended_broadcaster (§6.2.43) | `arib.rs:574-660`: `TerrestrialTv`/`TerrestrialSound`/`Other{broadcaster_type,data}` | Table 6-84 defines only 0x1/0x2 | prop |
| 0xD7 SI_parameter, 0xD8 broadcaster_name | `arib.rs:987`, `:1023` | content is "operational guidelines" | prop |
| BIT, NIT, SDT tables | `tables.rs` | | synthetic/spec vectors |

Related precedent: `isdb-mmt` decodes the TLV-NIT's 0x43 frequency as BCD
(`isdb-mmt/src/table.rs:653-668`). `isdb-cli` has the BS channel plan:
`bands.rs:79-86` `bs_rf_hz(n)` = 11 727.48 + 19.18·(n − 1) MHz, odd channels
right-hand and even left-hand (sources in SPECS.md §1.2 l.132).

## 3. What the specs say about BS SI (question 3)

### 3.1 Local specs (B10 v5.13, `specs/6-STD-B10v5_13-E1.txt`)

| Topic | Clause (line in the text) | Content |
|---|---|---|
| Required tables | Part 2 §5.1.3 Table 5-2 (l.4767-4803) | Same for every medium: PAT, CAT, PMT, NIT actual, SDT actual, EIT p/f actual, TDT or TOT mandatory; BIT, SDT/EIT other, EIT schedule optional. **Nothing BS-specific.** |
| NIT | §5.2.4 (l.4878-4910) | network_id "assigned by the standardization organization"; network_id = original_network_id on the originating network |
| Delivery descriptor | Table 6-1 (l.6414-6415, 6488-6490) | 0x43 "Mandatory in digital satellite broadcasting"; 0xFA "Mandatory for digital terrestrial broadcasting and multimedia"; 0xFB only for partial reception; 0xCD "Optional" in the NIT; 0xCE "Optional" in the BIT; 0x41 "Mandatory in NIT (actual network) … Optional in BIT" |
| 0x43 coding | §6.2.6 Tables 6-8…6-11 (l.6960-7085) | frequency 8 BCD digits in GHz, point after the 3rd digit ("012.73300GHz"); orbital_position 4 BCD digits, point after the 3rd ("144.0"); west_east_flag 1 = east; polarization '11' circular right (Table 6-9); modulation '01000' "Wide broad-band satellite digital broadcasting system (refer to TMCC signal)" (Table 6-10); symbol_rate 7 BCD digits in Msym/s, point after the 3rd ("027.4500"); FEC_inner '1000' "wide band satellite digital broadcasting system (refer to TMCC signal)" (Table 6-11); reserved: modulation 0x02–0x07, 0x0C–0x1F; FEC 0110–0111, 1100–1110 |
| system_management | §6.2.21 Table 6-36 (l.8033-8080) | '000010' = "wide band digital satellite broadcasting using 34.5 MHz bandwidth in 11.7 to 12.2 GHz" (BS); '000100' = the same at 12.2–12.75 GHz (110° CS); '000011' = terrestrial TV. additional_broadcasting_identification: "operation standard of service providers" |
| Extended broadcaster | §6.2.43 Table 6-84 (l.9848-9853) | only 0x1 terrestrial TV and 0x2 terrestrial sound; **no satellite type** |
| network_id | Annex N Table N-1 (l.13019-13045, informative) | "BS digital broadcasting 0x0004, ARIB TR-B15 Part 1"; 110° CS 0x0006/0x0007; advanced BS 0x000B |
| Identifier rules | Part 1 Table 7-1 (l.3333-3418) | transport_stream_id: "Specified and operated by the company; Unique within original network identifier"; service_id: "… Unique within Japan for the same broadcasting medium"; broadcaster_id and remote_control_key_id: "Specified and operated by the company" |
| NIT validity | Appendix §1.1 b), c) (l.15561-15585, guideline) | the NIT of the actual delivery system is valid iff it has that system's delivery descriptor; it "shall l[i]st all TSs of the actual delivery system" |
| TS_ID in TMCC | BO.1408-1 §6.1.3, Figs. 9-10 (`ITU-R-BO.1408-1.txt:497-546`) | 16-bit MPEG TS_IDs per relative TS number; no numbering rule |
| Symbol rate | ordinance Art. 52(3), BO.1408-1 Table 3 (SPECS.md l.280, 538) | 28.860 Mbaud |

### 3.2 Missing from the repository's specs

- **ARIB TR-B15** (BS / wide-band CS operational guidelines): only the
  sample tables of contents are free (SPECS.md l.118). It would define the
  BS NIT/SDT/BIT/EIT operation, SI_parameter contents, the TS_ID numbering
  (transponder/slot), service_id and broadcaster_id assignments, and whether
  0xCD is used.
- **MIC Notification No. 304 of 2011** (TMCC layout; `isdbs-core` SPEC-NOTES
  "tmcc-layout").
- **ARIB STD-B21** (receiver): not in `specs/`, although SPECS.md lists it
  as referenced.
- **ARIB STD-B20** (TMCC / transmission for BS): named in the `isdbs-core`
  notes as pending.
- So the following are all **[live]** or **[knowledge]**, never [spec]:
  - the TS_ID layout `0x4 | 3 bits | transponder(5) | 0 | TS number(3)`
    (stated by `/path/to/PIX-DT295/crates/dt295-recisdb/src/tsid.rs:1-20` from
    the recisdb-rs and px4_drv tables);
  - that receivers show the BS service_id as a 3-digit decimal channel
    number;
  - the 110.0°E orbital position;
  - the absence of 0xCD on BS.

### 3.3 Live BS structure (a 2023 recording, structure only)

A user-owned BS recording (outside the repository, read with `isdb ts
inspect --json` and `isdb ts extract`; no names or content recorded here):

- **NIT actual**, network_id 0x0004, one section of 829 B listing 28 TSs.
  - Network loop: `0x40`, `0xFE 02 01`. That is flag '00', identifier
    '000010', additional 0x01.
  - Every TS entry: onid 0x0004, loop `0x41` (service list) then `0x43`. There
    is no 0xCD, 0xFA, 0xFB or other descriptor.
  - The 0x43 bodies are `0117 2748 1100 E8 0288 6008` for BS-1 and the same
    pattern per transponder. Every frequency equals 11 727.48 + 19.18·(n − 1)
    MHz: BS3 01176584, BS9 01188092, … BS23 01214944. Orbital 110.0, east,
    circular right, modulation 01000, symbol rate 028.8600, FEC 1000.
  - Every TS_ID fits `0x4000 | x<<9 | n<<4 | k`, with the transponder n
    agreeing with the 0x43 frequency (e.g. 0x46B2 ↔ 11.91928 GHz = BS11).
- **SDT** actual and other (0x46) for every TS. Each service carries 0x48,
  sometimes 0xC1, 0x4A or 0xCB. running_status is 0.
  - service_types seen: 0x01, 0x02, 0xA1, 0xA4, 0xC0.
  - service_ids: 69 values between 101 and 929, all three-digit decimal
    numbers. 999 (0x03E7) is not among them.
- **BIT**: onid 0x0004, broadcast_view_propriety 1.
  - First loop: 0xD7 SI_parameter, with table descriptions for 0x40, 0xC4,
    0x42, 0x46, 0x4E, 0x4F, 0x50, 0x60, 0xC3.
  - 22 broadcasters with ids in 1..26. Each loop has 0xD8, 0x41 (its services)
    and usually 0xD7. There is no 0xCE. 0xEE is unused.
- **EIT**: p/f and schedule on 0x12, with 80 schedule sections in the first
  30 s.
- **Previous evidence** (VERIFICATION.md:1039-1048): on all lab BS
  transponders the PAT transport_stream_id equals the TMCC TS_ID, and
  validate reports 0 errors.

## 4. `isdb ts validate` and BS SI (question 4)

### 4.1 Checks that touch SI identity (`crates/isdb-analyze/src/validate.rs`)

- `tables.mandatory` (`:1048-1103`): B10 Table 5-2. It does not depend on
  the medium.
- `cross.tsid`: SDT and EIT actual TS_ID equal to the PAT's (`:1220-1255`).
- `cross.onid`: the onid of SDT, EIT, the NIT TS loop and the BIT agree;
  error otherwise (`:1357-1361`).
- `cross.nit` (all warnings), for the NIT actual (`:1290-1354`):
  - the actual TS is listed;
  - onid = network_id;
  - **"no terrestrial_delivery_system_descriptor for the actual TS"**
    (`:1307-1310`, citing Table 6-1 "Mandatory for digital terrestrial
    broadcasting");
  - 0xFE in neither the NIT nor a PMT.
- `cross.services`: the PAT against the NIT service list, 0xFB and the SDT.
- `si.conformance`: `isdb_si::conformance::check`. It has no 0x43 rules.
- Expectations: `--expect-freq-mhz/--expect-guard/--expect-mode/--expect-area`
  read only 0xFA. When a NIT has no 0xFA they report the error "no
  terrestrial_delivery_system_descriptor" (`:2144-2165`).
- No check compares network_id, broadcasting_identifier and delivery
  descriptor with one another. Nothing checks the TS_ID layout.
- `isdb ts inspect` (`isdb-analyze/src/si.rs:37-80`, `:300-345`) decodes
  only 0xFA. A 0x43 is listed as "other: satellite_delivery_system_descriptor
  (0x43)".
- SPEC-NOTES "cross-table identities" (`isdb-analyze/SPEC-NOTES.md:163-181`)
  already says that a missing 0xFA is a warning because "the analysed TS may
  not be terrestrial".

### 4.2 Measured (scratch runs, `target/release/isdb` of `371e72b`)

| Input | `isdb ts validate` |
|---|---|
| Today's bench TS (bench-minimal, declared streams, TS_ID 0x4010, 12 s) | 0 errors; 4 × `pid.missing` (declared streams) |
| Prototype BS config (§6.7) | 0 errors; 2 × `pid.missing` + **1 × `cross.nit` "no terrestrial_delivery_system_descriptor"**. `isdbt-mux` also warns "network_id 0x0004 is outside the terrestrial TV range". The prototype is network_id/onid 0x0004, TS_ID 0x4017, one service 0x03E7, `[nit] replace_sections` from `tools/bs-nit-section.py`, BIT 0xEE with a raw 0x41, 21 s. |

`isdb ts inspect` of the prototype: `NIT tid 0x40 network_id 0x0004 …
system_management_id 0x0201 / TS 0x4017 onid 0x0004 / service list
0x03e7/0x01 / other: satellite_delivery_system_descriptor (0x43)`; BIT onid
0x0004, broadcaster 0xee with 0x41.

**Answer:** validate does not treat satellite SI as an error. It warns once
for the missing 0xFA, and it does not tie network_id to the delivery system.

## 5. PIX-DT295 and SI (question 5)

- **`dt295` has no SI readout.**
  - Its subcommands (`crates/dt295-cli/src/main.rs:62-140`) are list, init,
    tune, status, stream, probes, register dumps and plans.
  - The only TS parsing is a PAT CRC count in the stream check
    (`crates/dt295-frontend/src/stream.rs:586-626`) and a PAT check for
    link-key candidates (`crates/dt295-device/src/secmsg.rs:740-760`).
- **The TS is selected by TS_ID alone.**
  - The tune writes the TS_ID to TC90532 `8f <hi>`, `90 <lo>`
    (`docs/frontend.md:244-265`).
  - `crates/dt295-recisdb/src/tsid.rs:1-20`: "The PIX-DT295 driver selects
    the TS by TS-ID only … and sends no TMCC reads".
  - `dt295 tune --isdbs bs1 --tsid 4010` therefore needs that TS_ID in the
    TMCC. The firmware does not look at the SI. The lock is a carrier lock;
    which TS was picked shows only in the output (`docs/mirakc.md`, the
    BS / CS110 bullet).
  - Default TS_ID: `isdbs::bs_tsid(n, slot) = 0x4000 | n<<4 | slot`
    (`crates/dt295-frontend/src/isdbs.rs:113-115`). `tsid.rs:54-81` holds the
    2026-10 on-air table; it contains 0x4010 (BS-1 TS 0) but not 0x4017.
  - `recisdb-dt295 BS01_7` falls back to the conventional 0x4017 with a
    warning (`crates/dt295-recisdb/src/channel.rs:331-357`). `--tsid
    0x4017` works directly.
- **SI readouts that exist:**
  1. `dt295 stream --isdbs bs1 --tsid 4017 --raw-config --out F --seconds N`,
     then `isdb ts inspect/validate F`. This is how VERIFICATION.md:1735-1766
     read the current bench.
  2. **mirakc** 3.4.90 with `recisdb-dt295` (`tools/mirakc/`,
     `docs/mirakc.md`). Its `scan-services` job (mirakc-arib) found the
     ISDB-T bench service from the DT295's TS: name, service_id 1024,
     network_id 31214, remote key 9 (`docs/stream-links.md:1876-1900`).
     mirakc's channel type (GR/BS/CS) comes from its config; its service id
     is nid·100000 + sid. "Listed as a BS channel" therefore means: a
     `type: BS` channel `BS01` with extra args `--tsid 0x4017` gives
     `/api/services` an entry with `networkId 4` (id 400999).
  - The mirakc-arib source is not local. What it requires of a BS TS is
    **[confirm]**: NIT 0xCD (absent on BS, §3.3), SDT, service_type filter.
  - The unit's firmware used to stop after about 61 s of streaming
    (`stream-links.md` §19.2). The later commit `926d7ab` ("run-time
    authentication keeps the stream alive") addresses that.
  3. StationTV X (vendor app, Windows VM `vm/`): BS scan in the VM is
     blocked by its HDCP loop and B-CAS needs (`docs/capture-plan.md:24`).
     Not a practical acceptance path.

## 6. Design

### 6.1 Configuration schema (backward compatible)

`[network]` gains `kind` and an optional `[network.satellite]` table:

| Key | terrestrial (default) | bs |
|---|---|---|
| `kind` | `"terrestrial"` | `"bs"` |
| `network_id` | as today; warn outside 0x7880–0x7FE8 | warn if ≠ 0x0004 (Annex N) |
| `name`, `ts_name`, `system_management`, `additional_broadcasting_identification` | as today | same; 0xFE identifier '000010' |
| `area_code`, `channels`, `frequencies`, `partial_reception_descriptor` | as today (`area_code` required; `channels`/`frequencies` non-empty) | **refused** (`MuxError::Config`, "terrestrial only") |
| `remote_control_key_id` | required (0xCD always) | optional; 0xCD only when given (Table 6-1 "Optional"; absent on BS [live]) |
| `transmission_types` | as today | only together with `remote_control_key_id` |
| `[network.satellite]` | refused | **required** |
| `satellite.channel` | — | the odd channels `"BS1"`, `"BS3"`…`"BS23"` → frequency and polarization (as implemented: the even channels carry the advanced BS (ISDB-S3) network; `crates/isdbt-mux/SPEC-NOTES.md`, "NIT of kind bs") |
| `satellite.orbital_position` | — | default `"110.0E"` ([live]); `ddd.dE`/`ddd.dW` |

`[[bit.broadcasters]]` gains `services = [u16…]`: a 0x41 in the broadcaster
loop for both kinds (Table 6-1 "Optional in BIT"; every BS broadcaster has
one [live]). The service_types come from the `[[service]]` entries, and a
service_id that is not configured is an error. With `kind = "bs"`,
`terrestrial = {…}` (0xCE type 0x1) is refused, because Table 6-84 has no
satellite type.

The derived 0x43 values are not configurable, in the same way 0xFA's
GI/mode are not. Experiments use `[nit] byte_patches`:

- modulation '01000' and FEC '1000' ("refer to TMCC");
- symbol rate 028.8600 (Art. 52(3));
- polarization from the channel's parity.

Example `crates/isdbt-mux/examples/bs-bench.toml` (placeholder identities;
media-free like the bench):

```toml
# BS-shaped SI (B10 Annex N, Table 6-1, §6.2.6). network_id 0x0004 is the
# BS network's (Annex N); TS_ID, service_id and broadcaster_id are
# placeholders that no BS broadcaster uses (checked against a 2023 NIT).
[network]
kind = "bs"
network_id = 0x0004
name = "ベンチ"
# remote_control_key_id: not sent on BS (no 0xCD) unless given

  [network.satellite]
  channel = "BS1"                # MUST equal `isdb tx --system isdbs --channel`
  orbital_position = "110.0E"

[transport]
transport_stream_id = 0x4017     # BS-1, TS number 7: not on air; = the TMCC's TS_ID (--ts 0x4017=…)
# original_network_id defaults to network_id (B10 §5.2.4)

[[service]]
service_id = 0x03E7              # 999: unused on BS
service_type = 0x01
provider = "ベンチ"
name = "ベンチHD"
layer = "B"                      # slot model only (ISDB-T layout) until item 3
pmt_pid = 0x01F0
pcr_pid = 0x01FF
copy_control = { recording_control = "copy-free" }
  [[service.stream]]
  kind = "declared"
  pid = 0x0111
  stream_type = 0x02
  component_tag = 0x00
  [[service.stream]]
  kind = "declared"
  pid = 0x0112
  stream_type = 0x0F
  component_tag = 0x10
  [[service.event]]
  event_id = 0x0001
  start_offset_s = -600
  duration = "01:00:00"
  name = "ベンチ試験放送"

[bit]
broadcast_view_propriety = true
broadcasters = [ { broadcaster_id = 0xEE, name = "ベンチ", services = [0x03E7] } ]
```

Serde mechanics: `area_code: Option<u16>` and `remote_control_key_id:
Option<RemoteControlKeyId>`. `partial_reception_descriptor: Option<bool>`
has to be an `Option` so that the `bs` kind can refuse a key that was given.
`satellite: Option<SatelliteCfg>`. Add `skip_serializing_if` so that
`to_toml` (`--print-config`) round-trips. Required-for-terrestrial becomes a
`MuxError::Missing { key: "network.area_code", needed_by:
"network.kind = \"terrestrial\" (0xFA area_code)" }`. Today serde reports
"missing field". The behaviour is the same, but the variant is now typed.

### 6.2 Descriptor contents and byte layouts (kind = "bs")

NIT actual (table_id 0x40, ext = network_id; `si_header`,
reserved_future_use '1'):

- **Network loop**, in the order seen on air [live]:
  - `40 len <name>`;
  - `FE 02 02 01`: system_management_id =
    `00`(flag, Table 6-35) `000010`(Table 6-36) `0x01`(abi) = 0x0201.
- **TS loop** for the one TS (TS_ID, onid), in the order `0x41`, `0x43`,
  then optional `0xCD`, then `add_descriptors`:
  - `41 3k (sid16 type8)…`: every service, sorted by service_id (as today).
  - `43 0B F F F F O O W·PP·MMMMM S S S S·EEEE`, with the fields of B10
    Table 6-8 in order:

    | Bytes | Field | Width | Value for BS1 |
    |---|---|---|---|
    | 0-3 | frequency | 32 bits = 8 BCD | `01 17 27 48` = 011.72748 GHz (10 kHz units: 1 172 748 + 1 918·(n − 1)) |
    | 4-5 | orbital_position | 16 bits = 4 BCD | `11 00` = 110.0° |
    | 6 | west_east_flag (1) · polarization (2) · modulation (5) | 8 bits | `E8` = 1 (east) · 11 (circular right, Table 6-9; even BS channels: 10) · 01000 (Table 6-10) |
    | 7-10 | symbol_rate · FEC_inner | 28 + 4 bits | `02 88 60 08` = 0288600 (028.8600 Msym/s) · 1000 (Table 6-11) |

  This is byte-identical to the BS-1 entry on air [live].

Golden section for the example (network name "ベンチ" = `1b6f597341` as
`isdb-text` codes it; v0): `tools/bs-nit-section.py 1b6f597341` →

```text
40f0300004c10000f00b40051b6f597341fe020201f01840170004f012410303e701
430b011727481100e8028860083f9c8672
```

| Table | kind = "bs" |
|---|---|
| SDT | unchanged: one section, 0x48 + configured descriptors, onid from config. SDT other is not generated (Table 5-2 optional; a one-TS network has no other TS) |
| EIT p/f and schedule | unchanged (Table 5-2: schedule optional; BS sends it [live]; `[eit_schedule] enabled = true` exists for mirakc's EPG) |
| BIT | header onid; first loop raw (0xD7 contents are TR-B15's; not generated); broadcaster loop: 0xD8 name, **0x41** from `services`, raw descriptors |
| PAT, PMT, CAT, TOT/TDT | unchanged |

### 6.3 Code changes per crate

**isdb-si** (piece A):

- `values.rs`:
  - `Polarization` (Table 6-9, 2 bits, four named codes);
  - `SatelliteModulation` (Table 6-10, 5 bits; 00000 / 01001 "not-defined",
    00001 `qpsk`, 01000 `isdb-s`, 01010 `advanced-narrow-band-cs`, 01011
    `isdb-s3`; Reserved otherwise);
  - `FecInner` (Table 6-11, 4 bits; 1000 `isdb-s`, 1010, 1011, 1111
    `none`; Reserved otherwise);
  - `BroadcastingIdentifier` (Table 6-36, 6 bits) with an accessor on
    `SystemManagement`.
- `descriptor/dvb.rs`, on `SatelliteDeliverySystem`:
  - typed accessors `polarization()`, `modulation_type()`, `fec()`;
  - BCD accessors `frequency_10khz() -> Option<u32>`,
    `orbital_position_tenths() -> Option<u16>`,
    `symbol_rate_100sps() -> Option<u32>`;
  - a checked constructor `from_values(frequency_10khz, orbital_tenths, east,
    Polarization, SatelliteModulation, symbol_rate_100sps, FecInner) ->
    Result<Self>` that encodes the BCD and refuses values that do not fit.
- `conformance.rs`: a 0x43 arm. Non-BCD digits give `Range`, and reserved
  modulation or FEC codes give `ReservedValue` (B10 §6.2.6).
- Tests:
  - `spec_table_6_8_satellite_delivery` with the BS-1 bytes above (decode,
    typed values, encode);
  - conformance cases.
- SPEC-NOTES "satellite delivery units": 10 kHz and 100 sym/s from "point
  after the third character".

**isdbt-mux** (piece B):

- `config.rs`: `NetworkKind`, `SatelliteCfg`, the Option fields, and
  `BroadcasterCfg.services`.
- `validate.rs:258-305`: per-kind rules (§6.1) and typed-loop sets:
  - bs: 0x41 and 0x43 always, 0xCD if a key is given;
  - BIT: 0xD8 if a name is given, 0x41 if `services` is given.
- `si.rs`:
  - `nit()` branches on kind;
  - `bs_frequency_10khz(channel)` and `bs_polarization(channel)` (same
    formula as `isdb-cli` `bands::bs_rf_hz`; SPECS.md §1.2; cite it as for
    the UHF formula, `si.rs:284-299`);
  - BCD encode (or A's `from_values`);
  - `BROADCASTING_IDENTIFIER` per kind;
  - `bit()` adds 0x41.
- `mux.rs:239-255`: per-kind network_id warning; the channels requirement
  only for terrestrial.
- New `examples/bs-bench.toml`.
- Docs:
  - `docs/mux-config.md` (`[network]`, new `[network.satellite]`, BIT
    `services`, load-time checks);
  - `SPEC-NOTES.md` (NIT: the BS branch; operational choices: TS-loop order
    0x41/0x43 [live], no 0xCD on BS [live], 110.0E [live], placeholder ids);
  - SPECS.md coverage: 0x43 is now generated.

**isdb-analyze** (piece C):

- `validate.rs:1307-1310`: "no delivery system descriptor (0xFA or 0x43) for
  the actual TS" (Appendix §1.1 b), Table 6-1).
- New warning `cross.delivery`. It compares three things:
  - the delivery descriptor kind of the actual TS;
  - the 0xFE broadcasting_identifier (Table 6-36: '000011'/'000101' terrestrial
    ↔ 0xFA; '000001'/'000010'/'000100'/'000111'…'001001' satellite ↔ 0x43);
  - the network_id against Annex N: 0x7880–0x7FE8 terrestrial ↔ 0xFA,
    0x0001–0x000C satellite ↔ 0x43. Annex N is informative, so this is a
    warning.
- Expectations:
  - `--expect-freq-mhz` also matches a 0x43 frequency (10 kHz units ⇒ MHz);
  - new `--expect-orbital`/`--expect-polarization` are optional;
  - the error text "no delivery system descriptor".
- `si.rs`: `NitTs.satellite: Option<SatelliteDelivery>` with frequency in
  MHz, orbital position, east, and polarization / modulation / FEC names;
  `inspect` text prints it. JSON gains a field (additive).
- `SPEC-NOTES.md` "cross-table identities".

**isdb-cli / tools** (piece D, lead after merges):

- `tools/isdbs-bench-tx.sh`: use `examples/bs-bench.toml` (drop the python
  rewrite, `:30-45`), with `--ts "0x4017=…"` and `--tsid 4017` in the
  comments.
- `txs.rs:85-86`: the comment "a placeholder, not a station's" is wrong for
  0x4010, which is BS-1 TS 0 on air (`dt295-recisdb/src/tsid.rs:55`). Fix
  the comment. Changing the default touches many loopback tests, so leave
  it.
- Optional: the ISDB-S producer warns once when a TS's first PAT
  transport_stream_id differs from its `--ts` TS_ID (VERIFICATION.md:1039
  shows they are equal on air).
- Optional: `isdb mux --print-config-example bs`.

### 6.4 Test plan

1. **Golden SI** (piece B, `tests/mux.rs`):
   - `bs-bench.toml` with a fixed clock gives exactly the NIT section of §6.2
     (hex constant built in the test from the B10 field layout, not copied
     from the multiplexer);
   - the BIT's broadcaster loop is `D8 …`, `41 03 03 E7 01`.
2. **Round trip** through `AnyTable::decode`:
   - NIT ext 0x0004;
   - TS loop tags `[0x41, 0x43]`;
   - typed 0x43 values (piece A accessors): 1 172 748 × 10 kHz, 1100, east,
     `Polarization::CircularRight`, `SatelliteModulation::IsdbS`,
     `FecInner::IsdbS`, 288 600 × 100 sym/s;
   - 0xFE identifier 0b000010;
   - no 0xFA, 0xFB or 0xCD.
3. **Terrestrial unchanged** (piece B):
   - before the change, record the NIT/SDT/BIT/EIT sections of
     `bench-minimal.toml` and `bench.toml` (declared, fixed clock) as hex
     fixtures, and assert byte equality after it;
   - existing `derived_nit_values` and `config_errors` pass;
   - every `examples/*.toml` loads.
4. **Config errors** (`tests/config_errors.rs`, typed):
   - bs + `area_code`, bs + `channels`, bs without `[network.satellite]`;
   - `channel = "BS25"` and `"ND2"` (and, as implemented, the even `"BS2"`);
   - a bad `orbital_position`;
   - bs + `terrestrial` broadcaster;
   - `services` naming an unknown sid;
   - terrestrial without `area_code` gives `Missing`.
5. **Validate clean**:
   - `isdbt-mux` test: the BS mux output through
     `isdb_analyze::validate::validate` has no errors and no `cross.nit` or
     `cross.delivery` findings;
   - `isdb-cli` `tests/cli.rs`: `isdb mux -c examples/bs-bench.toml
     --seconds 12` then `isdb ts validate --expect-nid 0x0004 --expect-onid
     0x0004 --expect-tsid 0x4017 --expect-freq-mhz 11727.48` exits 0.
6. **isdb-analyze**: a synthetic TS with the golden NIT gets no delivery
   warning, a terrestrial TS is unchanged, and a mismatched one (0x43 with
   identifier '000011') gives `cross.delivery`.
7. **Bench** (manual, conducted, `--rf-enable`, att ≥ 45 dB):
   - `tools/isdbs-bench-tx.sh WORK 45 ip:<PLUTO_IP>`;
   - `dt295 init`, `dt295 tune --isdbs bs1 --tsid 4017`, then `dt295 stream
     --isdbs bs1 --tsid 4017 --raw-config --out bs.ts --seconds 20`;
   - `isdb ts inspect bs.ts` shows NIT 0x0004 with 0x43 BS1 and SDT 0x03E7.
     `isdb ts validate bs.ts` has no SI findings; the timing findings until
     item 1 lands are expected;
   - then mirakc: a `BS01` channel with `--tsid 0x4017`, and `/api/services`
     lists 400999.
   - Record the result in VERIFICATION.md.

### 6.5 Backward compatibility

- `kind` defaults to `"terrestrial"`.
- Every existing key keeps its meaning, and every existing configuration
  loads unchanged.
- The terrestrial NIT bytes are unchanged: same descriptor order and
  values. Test 3 asserts it.
- `Network`'s public field types change (`Option`); only `isdbt-mux`,
  `isdb-cli` (`relay.rs` via the template text, `mux.rs`) and tests read
  them (`grep network\.` in §1.2).
- `isdb tx --mux-config` and `isdb loopback --mux-config` (ISDB-T,
  `isdb-cli/src/input.rs:600-633`, `relay.rs:377-383`) should refuse `kind =
  "bs"`, because a satellite NIT on an ISDB-T signal is a contradiction.
  Alternatively they can warn; the lead decides. Implemented (piece D):
  refused, with the `isdb mux` / `isdb tx --system isdbs` commands to use
  instead.

### 6.6 Implementation split (worktree agents)

| Piece | Crate(s) | Depends on | Contract |
|---|---|---|---|
| **A** satellite typed view | `isdb-si` (+ SPEC-NOTES, SPECS.md matrix row) | — | Public API: `values::{Polarization, SatelliteModulation, FecInner, BroadcastingIdentifier}` (CodedValue); `SatelliteDeliverySystem::{polarization, modulation_type, fec, frequency_10khz, orbital_position_tenths, symbol_rate_100sps, from_values}`; `SystemManagement::broadcasting_identifier_typed()`; conformance arm for 0x43. Codec bytes unchanged (prop tests still pass). Spec vector with the BS-1 bytes. |
| **B** BS kind in the multiplexer | `isdbt-mux`, `docs/mux-config.md`, example | A (or a local BCD helper first, swapped for `from_values` at merge) | Schema of §6.1; NIT/BIT of §6.2; tests 1–5 of §6.4; terrestrial bytes identical (fixture test committed **before** the refactor in the same branch). No change to SDT/EIT/PAT/PMT code paths. |
| **C** satellite-aware validate/inspect | `isdb-analyze` (+ `isdb-cli` `ts.rs` help text) | A | `cross.nit` delivery wording; new `cross.delivery` (warning); `--expect-freq-mhz` on 0x43; `SiView` satellite field (additive JSON); synthetic tests §6.4-6; SPEC-NOTES. No change in severities of existing checks. |
| **D** bench and CLI glue | `tools/isdbs-bench-tx.sh`, `isdb-cli` (`txs.rs` comment, optional PAT/TMCC TS_ID warning, refusal of `kind = "bs"` in ISDB-T `--mux-config`, cli test) | B, C merged | Bench acceptance §6.4-7; VERIFICATION.md section. |

Order: A first (small), then B and C in parallel, then D on the bench. The
lead can ship the interim (§6.7) at once, because it needs no code.

### 6.7 Interim without code (works today)

`tools/bs-nit-section.py` (added by this investigation, uncommitted) prints
the BS NIT section. A config like §6.1's example, written with today's keys,
transmits BS-shaped SI now. It keeps the terrestrial keys as dummies
(`area_code`, `remote_control_key_id`, `channels = [27]`), which the
replaced NIT ignores. It adds:

- `[nit] replace_sections = ["<hex of §6.2>"]`;
- `[bit] broadcasters = [{ broadcaster_id = 0xEE, name = "ベンチ",
  descriptors = [{ tag = 0x41, hex = "03E701" }] }]`.

Measured: §4.2. The cost is the mux's range warning and one `cross.nit`
warning.

## 7. Risks and open questions

1. **No operational guideline.** BS details beyond B10 rest on one 2023
   recording and on other projects' TS_ID tables: no 0xCD, the TS-loop order,
   0xD7 contents, service_id/TS_ID numbering, broadcaster ids. TR-B15
   Part 1 would settle them. Mark each such item [live] in SPEC-NOTES.
2. **Identity policy.**
   - network_id 0x0004 is the BS network's (Annex N), shared by every BS
     broadcaster. It is what makes software classify the service as BS, and
     the plan asks for it. The lead or user should confirm that it is
     acceptable under "no real station identities".
   - TS_ID, service_id and broadcaster_id are placeholders not on air in the
     2023 NIT, and that list can change.
   - The CLI default 0x4010 is a real BS-1 TS_ID, contrary to its comment.
3. **Receivers that store the BS NIT.** A consumer receiver that has
   scanned real BS and then sees this NIT (network_id 4, one TS, version 0)
   may replace its stored BS network table. Use only the PIX-DT295 and
   mirakc on this bench, and keep the conducted-only rule. A receiver that
   retunes by the NIT will find BS1, which is the frequency transmitted.
4. **mirakc-arib requirements are unknown**: NIT 0xCD (the ISDB-T run
   reported "remote key 9"), service_type filters, or whether it needs NIT
   at all. Needs the bench test **[confirm]**. If it needs 0xCD,
   `remote_control_key_id` on BS is already designed as optional.
5. **TS_ID selection**: a TS_ID absent from the TMCC gives a lock with no
   output **[confirm]**. The TMCC, the PAT and `--tsid` must agree, and the
   optional check in D catches PAT ≠ TMCC.
6. **The slot model is still ISDB-T**:
   - `layer`, partial reception and the 29.96 Mbit/s CBR remain;
   - the ISDB-S side pads to 52.17 Mbit/s;
   - PSI/SI placement is unaffected by kind.
   Item 3 generalises it; until then a BS config must name an active ISDB-T
   layer for each service.
7. **`RemoteControlKeyId` 1..=12** encodes terrestrial practice. If BS ever
   uses 0xCD the range may differ **[knowledge]**.
8. **No SDT/EIT "other"**, and the NIT lists only our TS. That is valid per
   B10 Appendix §1.1 c) for a one-TS network. Some receivers may expect a
   full BS network; not checkable without one.
9. **110° CS** (network_id 0x0006/0x0007, identifier '000100', ND plan) fits
   the same design as a later `kind = "cs110"`; not in scope.
