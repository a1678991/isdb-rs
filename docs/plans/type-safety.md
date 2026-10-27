# Plan: spec constraints carried by the type system

Workflow for moving ISDB-T / ISDB-S / ISDB-S3 / TS / SI spec constraints
out of run-time checks and into Rust types, so the compiler proves them at
every construction site. Written 2026-10-27 after the pilot
(`518a1b8`, `TimeInterleave`). Repository rules apply unchanged:
[CONTRIBUTING.md](../../CONTRIBUTING.md) (spec-first, every public item
cites its clause, lossless codecs, `spec_*` / `prop_*` test names,
conventional commits) and the verification ladder of
[VERIFICATION.md](../../VERIFICATION.md).

## 1. What "proven by the compiler" means here

A constraint is *carried by a type* when a value that violates it cannot
be constructed outside the module that owns the type. Then:

- the check runs once, at the one place the raw value enters (the decoder,
  the CLI parser, the TOML loader), and is *absent* everywhere else;
- the error variant for the violation disappears, which removes a code
  path from every caller that matched on it;
- `rustc` finds every construction site. A `grep` finds the ones that
  spell the type's name.

The project already does this in places: `isdbt_core::Params` (private
fields, `Params::new` validates §3.11.2), `isdbs3_core::CodeRate(u8)`
(private, `new` checks the 11 rates), `isdb_si::values::coded_enum!`
(lossless typed view of a B10 code table), `isdbt_mux::signalling::U4`.
This plan makes that the norm.

### 1.1 The two shapes

**Closed enum** — for a *configuration* value where only the assigned
codes may exist (the modulator / demodulator cannot do anything with a
reserved code). Example: `TimeInterleave { K0, K1, K2, K3 }`. The
decoder maps the raw code through `from_code() -> Option<Self>` and
rejects `None` with the existing error type; `code()` is the inverse.
Every other `impl` is infallible. The reserved codes have no spelling.

**Lossless coded enum** — for a *wire* field the decoder must keep
bit-exact (`encode(decode(x)) == x`, VERIFICATION.md rung 2), whose
table reserves codes: named variants plus one `Reserved(u8)` (or
`Unused`, `Extension`, whatever the table calls it) that keeps the raw
value masked to the field width. `coded_enum!` in `isdb-si` is this
shape; `isdb-ts` and the satellite crates get their own copy or a shared
one (see §3, item A).

Choose by asking: *is a reserved code an input the program has to
preserve and report, or an input it has to refuse?* Preserve → lossless
coded enum. Refuse → closed enum.

**Not in scope (yet):** full newtypes for 13-bit PID / 16-bit TS_ID style
fields where the only constraint is the width. `BitWriter::write` already
rejects overflow at encode time and `pid: u16` appears 118 times; a
`Pid(u16)` newtype is a large diff for a check that already exists. It
goes in the inventory (§3) as a later item, after the enums.

### 1.2 Recipe (what the pilot did, in order)

1. **Read the table.** Open the clause in `specs/` and list every code it
   assigns and every code it reserves. The enum doc comment cites the
   table; each variant's doc gives its code and the table's words.
2. **Pick the shape** (§1.1) and write the type with `code()`,
   `from_code()`, `ALL` (closed) and `Display` where callers print the
   code. Make the pure accessors `const fn`.
3. **Delete the run-time check** that the type now makes impossible, and
   its error variant. If the variant is matched anywhere, the compiler
   tells you.
4. **Follow the compiler.** `cargo clippy --workspace --all-targets
   --locked -- -D warnings` until clean. Fix *call sites*, not the type:
   if a caller needs a `u8`, it calls `code()`; if it has a `u8`, it calls
   `from_code()` and handles `None` with the error type it already uses.
   Test helpers that took `ti: u8` take `TimeInterleave`.
5. **Prove the closure.** A `compile_fail` doctest that names a reserved
   code (`TimeInterleave::K4`) and a `spec_<table>_…` test that
   `from_code(code()) == Some(self)` for `ALL` and `None` for every
   reserved code. For the lossless shape, the existing `prop_*` round trip
   must still pass unchanged.
6. **Update the prose:** crate README (doctested), `lib.rs` module docs,
   CHANGELOG "Changed" with a `BREAKING CHANGE` footer on the commit if a
   public field or signature changed.
7. **Verify** with `mise run check` (lint, test, doc, deny, msrv **and
   `oracle`**, which compiles the out-of-workspace helper the workspace
   build skips; see §1.4), plus the two `--no-default-features` clippy
   runs CI adds (`isdb-cli`, `isdb-radio`). Then `tools/type-census.sh
   <crate>` to record what the change retired.
8. **One commit per type**, `refactor(<crate>)!: <Type> is a <shape> of
   <table>`. Body: what was checked at run time before, where the raw value
   now enters, how many sites the compiler found.

### 1.3 Pilot: `TimeInterleave` (`518a1b8`)

| | before | after |
|---|---|---|
| type | `pub struct TimeInterleave(pub u8)` | `pub enum TimeInterleave { K0, K1, K2, K3 }` |
| range check | `Params::new` → `ParamError::TimeInterleaveIndex` | none (type is closed) |
| `length(mode)` | `Option<usize>` | `usize`, `const fn` |
| reserved codes '100'–'111' | rejected in `Params::new` *and* `TmccLayer::config` *and* `phyargs` | rejected in `from_code`, used by both |
| proof | one unit test with `TimeInterleave(4)` | `compile_fail` doctest + `spec_table_3_12_time_interleave_codes` |
| sites changed | — | 43 in 37 files, all found by `rustc` |

Non-obvious call sites the compiler surfaced: a `#[cfg(test)]` module in
`isdbt-mux/src/capacity.rs` that imported the type (keep the import,
scoped to tests), the README doctest (`Some(2)` → `2`), a test
`arb_params` strategy indexing `ALL` instead of casting a random `u8`.

### 1.4 Caveats learned

- `cargo test --workspace` has two wall-clock tests in `isdb-cli`
  (`symstream::tests::a_loss_ends_the_stretch`, `tests/cli.rs
  live_feed_with_stalling_input_stays_cbr_and_lossless`) that fail under
  CPU contention (parallel cargo runs) and pass alone. Not a regression
  signal; run the final suite with nothing else building.
- A `sed` over literals (`TimeInterleave\(([0-3])\)` → `::K\1`) does the
  bulk; everything else is a hand edit guided by the compiler. Do not
  widen the regex to catch variables; let `rustc` list them.
- Keep the `#[doc(hidden)] pub const _X_CLOSED: () = ();` carrier for the
  `compile_fail` doctest: rustdoc only runs doctests on items.
- `oracle/rust_tx3` is a stand-alone crate outside the workspace that
  depends on `isdbt-core` by path: `cargo clippy --workspace` does not
  compile it, so the compiler's list of sites misses it. The pilot broke
  it and only `mise run oracle` (a CI job) noticed, after item F. Step 7
  of the recipe is `mise run check`, which includes `oracle`; the
  workspace-only commands are not enough.

## 2. The cast lints

`tools/type-census.sh` (added with this plan) runs clippy with the five
`cast_*` lints the workspace does not enable (`Cargo.toml` says why: they
"fire thousands of times on bit-field code"). Baseline at `518a1b8`,
workspace total **2 462**:

| kind | count | meaning |
|---|---|---|
| `cast_possible_truncation` | 718 (+135 on 32-bit, +50 on 64-bit) | `as u8` / `as u16` from a wider type |
| `cast_lossless` | 638 | `as` where `From` is infallible |
| `cast_precision_loss` | 593 | integer → float |
| `cast_sign_loss` / `cast_possible_wrap` | 182 + 37 | signed ↔ unsigned |

`cast_lossless` is mechanical (`u16::from(b)`), and enabling it at
`warn` is safe once the 638 are fixed. The truncations are where the
bugs would be: each is either a field whose width the type should carry
(then the enum/newtype work above removes the cast) or an arithmetic
result that needs `try_from` with the existing `Error::Overflow`. The
per-crate table is the output of the script; the heaviest are `isdb-si`
(1 011), `isdbt-mux` (930), `isdb-frontend` (659), `isdb-cli` (521),
`isdbt-ofdm` (520), `isdb-ts` (380).

Goal: `cast_lossless = "warn"` in `[workspace.lints.clippy]` after item F
below; `cast_possible_truncation = "warn"` crate by crate as each crate's
count reaches zero, with `#[allow]` + comment on the few that are
intentional (DSP code taking the low bits on purpose). Covered (item H3,
a crate-level `#![warn(clippy::cast_possible_truncation)]` in `lib.rs`):
`isdbt-core`, `isdb-hackrf`, `isdb-iio`, `isdb-text`, `isdb-radio`,
`isdb-tlv`; three `#[allow]`s remain there (libiio's `-errno` in
`isdb-iio`, the clamped 12-bit sample in `isdb-radio::quant`, the
saturating buffer count in `RxOptions::buffers`).

## 3. Inventory (work items, disjoint file sets)

Each item is one agent's brief: the files listed are the only ones it
edits, so items run in parallel in separate worktrees without merge
conflicts. Order within an item follows the recipe (§1.2).

### A. `isdb-ts` packet header codes — H.222.0 Tables 2-4, 2-5

`Header.transport_scrambling_control: u8` and
`Header.adaptation_field_control: u8` (`packet.rs`), with loose consts in
`mod afc`. Shape: **lossless coded enum** (TSC '01'–'11' are "user
defined", AFC '00' is "reserved … decoders shall discard"; both are wire
fields the TS layer must preserve). `AdaptationFieldControl { Reserved,
PayloadOnly, AdaptationOnly, AdaptationAndPayload }` with
`has_payload()` / `has_adaptation_field()` moved onto it;
`TransportScramblingControl { NotScrambled, UserDefined(u8) }` (keep the
value). `Header::new` takes the enum. Also `continuity_counter: u8` →
leave (4-bit counter arithmetic, `cc.rs` wraps it; a newtype is item G).
Files: `crates/isdb-ts/src/{packet,cc,packetize,stats}.rs`,
`crates/isdb-ts/tests/{capture,prop}.rs`, and the callers:
`isdb-analyze/src/{scan,validate}.rs`, `isdb-analyze/tests/{checks,real_data}.rs`,
`isdb-cli/src/{source,tx}.rs`, `isdb-cli/tests/cli.rs`,
`isdb-text/tests/synthetic.rs`, `isdbt-mux/src/{config,relay}.rs`,
`isdbt-mux/tests/adts.rs`, `isdbt-phy/src/broadcast_ts.rs`. The relay
policy (CONTRIBUTING "Relay policy") reads TSC: keep its tests green and
its behaviour byte-identical.

### B. `isdbt-ofdm` TMCC layer codes — ARIB STD-B31 Tables 3-28 … 3-31

`TmccLayer { modulation: u8, code_rate: u8, interleave: u8, segments: u8 }`
(`tmcc.rs`) are raw 3/3/3/4-bit codes with '111' / '1111' = unused.
Shape: **lossless coded enum** per field (the TMCC decoder must keep the
received word; `TmccInfo` is compared bit-exactly against broadcast
vectors in `tests/broadcast.rs`). `Table3_28 { Dqpsk, Qpsk, Qam16, Qam64,
Reserved(u8), Unused }` etc., each with `code()` / `from_code()` (total),
and `TmccLayer::config()` maps assigned variants to `isdbt_core` types
with no `match` on integers left. `interleave` becomes
`Option<TimeInterleave>`-like via the pilot's `from_code`. Files:
`crates/isdbt-ofdm/src/tmcc.rs`, `crates/isdbt-ofdm/tests/{broadcast,spec,loopback}.rs`
(only where they construct `TmccLayer`), `crates/isdbt-ofdm/README.md`,
`SPEC-NOTES.md` if the table reading needed interpretation. Depends on
the pilot (already on the branch). Does **not** touch `isdbt-core`.

### C. `isdbs-core` TMCC combination word — BO.1408-1 Fig. 8 / Table 1

`CombinationEntry { word: u8, slots: u8 }`: `word` is 4 bits, 0001–0111
assigned (`Combination`), 1111 dummy, the rest reserved
(`TmccError::ReservedWord`). Shape: **lossless coded enum**
`CombinationWord { Assigned(Combination), Dummy, Reserved(u8) }` so the
`validate()` match becomes exhaustive and `combination()` is a pattern,
not a lookup. `slots: u8` is 6 bits with the sum-to-48 invariant
checked in `validate()`; leave it. `order_of_change: u8` (5 bits) and
`uplink: u8` (4 bits): leave (plain counters, width enforced at encode).
Files: `crates/isdbs-core/src/{tmcc,combination,slots,mainsig,lib}.rs`,
`crates/isdbs-core/tests/props.rs`, `crates/isdbs-core/README.md`, and
the readers `crates/isdbs-phy/src/{demod,rx,testsig,ts,lib}.rs`,
`crates/isdb-cli/src/{rxs,tmcc}.rs`. **Not** `isdbs3-*` (item D).

### D. `isdbs3-core` TMCC mode and stream type — ARIB STD-B44 Tables 3.11-1, 3.11-2, 3.11-4

`tmcc::Mode { modulation_code: u8, rate_code: u8, … }` carries raw codes
beside typed accessors returning `Option`; `TmccInfo.stream_type: [u8;
16]` is 1 TS / 2 TLV / FF none. Shape: **lossless coded enum** for all
three (`ModulationCode { Assigned(Modulation), Unused, Reserved(u8) }`,
`RateCode` likewise over the existing `CodeRate`, `StreamType { Ts, Tlv,
None, Reserved(u8) }`). `Mode::is_used()` becomes a pattern match. The
SPEC-NOTES entry "TMCC code-rate table" (101/120 printed twice) must stay
referenced from `RateCode::from_code`. Files:
`crates/isdbs3-core/src/{tmcc,slots,lib}.rs`,
`crates/isdbs3-core/tests/props.rs`, `crates/isdbs3-core/README.md`,
readers `crates/isdbs3-phy/src/{rx,tmcc}.rs`,
`crates/isdbs3-phy/examples/tmccprobe.rs`, `crates/isdb-cli/src/{rxs3,s3tmcc}.rs`.
(`isdbt-mux` has an unrelated `sampling_rate_code`; not this item.)

### E. `isdb-mmt` / `isdb-tlv` MMTP and MPU header codes — ARIB STD-B60 Table 6-1, §6.x

`isdb_tlv::mmtp::Header { fec_type: u8, payload_type: u8 }` (consts `MPU`,
`MESSAGES`), `isdb_mmt::mpu::PayloadHeader { fragment_type: u8,
fragmentation: u8 }`. Shape: **lossless coded enum** (`PayloadType { Mpu,
Messages, Reserved(u8) }`; `FragmentType { MpuMetadata,
MovieFragmentMetadata, Mfu, Reserved(u8) }`; `FragmentationIndicator`
with the four B60 §6 values). The reassembly state machine in
`decoder.rs` then matches on variants. Files:
`crates/isdb-tlv/src/{mmtp,stats}.rs`, `crates/isdb-tlv/examples/{freeca,mmtpdump}.rs`,
`crates/isdb-mmt/src/{mpu,decoder}.rs`, `crates/isdb-mmt/tests/synthetic.rs`,
`crates/isdb-mmt/examples/scrambleprobe.rs`, `crates/isdbs3-phy/examples/padump.rs`,
both READMEs. Cite B60 §6.1 (MMTP header), Table 6-1 (MPU payload); the
text is in `specs/ARIB-STD-B60-v1.14.txt` (lines ≈ 2196–2320 for
`fragment_type` / `fragmentation_indicator`).

### F. `cast_lossless` sweep (mechanical, every crate)

Replace the 638 `x as T` where `T: From<X>` by `T::from(x)` or `.into()`,
then add `cast_lossless = "warn"` to `[workspace.lints.clippy]` with a
comment. No behaviour change by construction; `cargo test --workspace`
must be identical. Conflicts with A–E are textual only (same lines in
the files they edit): run F **after** A–E have merged, or exclude their
file lists. The script: `cargo clippy --workspace --all-targets --fix
--allow-dirty -- -W clippy::cast_lossless` does most of it; review the
diff, as `--fix` can produce `u64::from(a) + u64::from(b)` where one
`From` suffices.

### H. Second wave (after G1 `Segments` and G2 `Pid`)

- **H1 canonical reserved payloads.** In each crate with a lossless coded
  enum (`isdb-ts`, `isdbt-ofdm`, `isdbs-core`, `isdbs3-core`, `isdb-tlv`,
  `isdb-mmt`), `Reserved(u8)` / `UserDefined(u8)` become
  `Reserved(ReservedCode)` where `ReservedCode` has a private field,
  `pub const fn get(self) -> u8`, `Display`, and a constructor that is
  `pub(crate)` and only `from_code` calls, after it has excluded the
  assigned and unused codes. Then every value of the enum is canonical by
  construction, `PartialEq` is the wire equality, and a `compile_fail`
  doctest proves `Reserved(…)` cannot be built outside the crate.
- **H2 `ContinuityCounter(u8)`** (4 bits, H.222.0 §2.4.3.3) in `isdb-ts`
  with `wrapping_next()` / `is_next_of()`, replacing the `& 0x0F`
  arithmetic in `cc.rs`, `packetize.rs`, the muxer and `isdb-analyze`;
  and, in the same item because the files overlap, the raw-byte TSC/AFC
  readers outside `isdb-ts` (`p[3] >> 6`, `(p[3] >> 4) & 3` in
  `isdb-analyze/src/{scan,validate}.rs`, `isdb-cli/src/{source,tx}.rs`,
  `isdbt-mux/src/{config,relay}.rs`, `isdbt-phy/src/broadcast_ts.rs`) go
  through `Header::decode` and the item-A enums.
- **H3 `cast_possible_truncation`** as a crate-level `#![warn]` in every
  crate whose census count is zero or can be made zero in a few edits
  (`isdbt-core` 0, `isdb-hackrf` 0, `isdb-iio` 1, `isdb-text` 2,
  `isdb-radio` 5, `isdb-tlv` 6), each remaining cast replaced by
  `try_from` + the crate's error or `#[allow]` with a one-line reason.

### G. Later (not dispatched now)

- `Pid(u16)` newtype in `isdb-ts` (13 bits, `pid: u16` × 118 sites; H.222.0
  Table 2-3 reserved PIDs as consts on it), `TableId(u8)` with the B10
  Table 5-2 assignment and `table_allowed_on_pid` as a method.
- `ContinuityCounter(u8)` with `wrapping_next()` (4 bits), replacing the
  `& 0x0F` arithmetic in `cc.rs` and the muxer.
- `segments: usize` in `LayerConfig` → `Segments(u8)` 1..=13 (Table 3-31),
  removing `ParamError::EmptyLayer` and the `1..=13` check in TMCC decode.
- `cast_possible_truncation = "warn"` per crate as counts reach zero.
- The raw-byte TSC/AFC readers outside `isdb-ts` (`p[3] >> 6`,
  `(p[3] >> 4) & 3` in `isdb-analyze/src/{scan,validate}.rs`,
  `isdb-cli/src/{source,tx}.rs`, `isdbt-mux/src/{config,relay}.rs`,
  `isdbt-phy/src/broadcast_ts.rs`) go through `Header::decode` and the
  item-A enums.

## 3.1 Status (2026-10-27, after the first dispatch)

Items A–E ran in parallel worktrees (one agent each) off `c156dc5` and
are merged on `feat/pluto-backend` (`04900bb` A, `a8190d7` C, `b8a0d1e`
B, `5e4ea5f` E, `66f5f82` D); the full ladder (fmt, clippy `-D
warnings`, workspace tests, rustdoc `-D warnings`, MSRV) is green on the
merged tree. What each agent reported:

| item | sites the compiler found | bare-int `pub` fields | spec notes |
|---|---|---|---|
| F cast sweep | 632 casts, 81 files, all by `clippy --fix`; `cast_lossless = "warn"` now on (`58515da`) | unchanged | none; `From`-only diff, round-trip checked by a normaliser |
| A `isdb-ts` | 2 sites, 2 files (the 17 planned callers read raw bytes, not `Header`) | 80 → 78 | none; H.222.0 Tables 2-4/2-5 verbatim |
| B `isdbt-ofdm` | 0 outside `tmcc.rs` | 26 → 22 | Table 3-30 '100' "not used" (sound broadcasting) kept as `Reserved(0b100)`; unused layer decided by the modulation field alone |
| C `isdbs-core` | 22 sites, 8 files | 6 → 5 | none; BO.1408-1 Table 1 unambiguous. Lab captures re-decoded identically |
| D `isdbs3-core` | 13 sites, 6 files | 15 → 13 (+ `isdbs3-phy` 14 → 13) | none new; the 1010 = 105/120 reading stays in `RateCode::from_code` |
| E `isdb-mmt` / `isdb-tlv` | 15 files | 69 → 67, 20 → 18 | B60 Table 6-6 (payload type) vs 6-5 (FEC type): an old `freeca.rs` comment cited the wrong table; `FecType` typed too |

Follow-ups the agents surfaced, added to §3 G: the raw-byte readers of
TSC/AFC in `isdb-analyze`, `isdb-cli`, `isdbt-mux`, `isdbt-phy` (item A
left them; they could take the enum through `Header::decode`), and
`SegmentsCode::Segments(l.segments as u8)` in `isdbt-ofdm`, the one cast
item G's `Segments(u8)` retires.

Cast census after A–E (`tools/type-census.sh`): 903 truncate, 632
lossless, 327 sign/wrap, 594 precision, total **2 456** (was 2 462): the
enum items were never about casts; item F is.

**Independent review of A–F** (read-only agent, tree at `86972d6`, no
shell so no old-vs-new diff; the two questions it raised were checked
against `13ace6a` by hand): no lossless-ness regression (every
`from_code` masks to the width, every encoder writes `code()`); no
behaviour change (`is_unused` compared all four fields before and after,
`config()` gated on modulation alone before and after); no new panic on
wire data; every new item cites its table. One structural finding,
valid: the `Reserved(u8)` / `UserDefined(u8)` payloads are public, so a
hand-built `Reserved(0xF6)` is `!=` `Reserved(0x06)` while both encode as
6, and `Reserved(0b111)` can be spelled where `Unused` is the canonical
form. The decoders never produce such values and `code()` masks, but the
bijection is only guaranteed on the `from_code` path. Fix queued as item
H1 below: a crate-private-constructor newtype for the payload so the only
way to hold a reserved code is to have decoded it.

Practical notes for the next dispatch: agent worktrees are cut from
`main`, not the current branch, so each brief must say to reset onto the
branch tip first; the scratchpad directory is shared between parallel
agents (one overwrote another's commit-message file), so temporary files
need unique names; every agent inserts its CHANGELOG entry at the top of
`### Changed`, so that file conflicts on every merge and is resolved by
keeping all entries.

## 4. Merging the parallel items

Each item is a branch off `feat/pluto-backend` at or after `518a1b8` in
its own worktree. Merge order A, B, C, D, E (independent file sets; any
order works), then F. After every merge: `mise run lint && cargo test
--workspace --locked && mise run doc`. Then `tools/type-census.sh` and
update §2's baseline here.
