# Plan: error handling and structured logging

Workflow for two properties of the workspace: every failure a library
reports is a typed value a caller can match on and a user can read, and
every diagnostic a library has to say is a `tracing` event with fields,
printed by nobody but the `isdb` binary. Written 2026-10-28 after the two
pilots (`b88e62a` logging, `795b0e9` errors). Repository rules apply
unchanged: [CONTRIBUTING.md](../../CONTRIBUTING.md) (every public item
cites its clause; decoders return errors and never panic; `spec_*` /
`prop_*` tests; conventional commits) and the type-safety recipe of
[type-safety.md](type-safety.md), which this plan continues.

## 1. Starting state (survey of 2026-10-28, tree `cd29a67`)

**Logging.** No library prints or logs anything: no `log` / `tracing`
dependency anywhere, zero `eprintln!` outside `isdb-cli` (two `println!`
in `isdbt-fec` tests). Libraries hand diagnostics up as data
(`relay::Report { remarks: Vec<String>, skipped, dropped_descriptors }`,
`Stats` structs) and `isdb-cli` prints them with 130 `eprintln!` lines
tagged `[tx]`, `[rx]`, `[symbols]`, `[loopback]`, `isdb:` on stderr, plus
53 `println!` for reports on stdout. The CLI tests match stderr by
substring (`err.contains("null fill")`), never by exact line.

**Errors.** Every library crate has a `thiserror` enum, and the shape is
good where it is specific (`isdbt_core::ParamError`, `isdb_ts::Error::
{Truncated, Crc, Overflow}` with the syntax element named). The weak
spots:

| where | what | count |
|---|---|---|
| `isdbt_mux::MuxError::Config(String)` | free text; the TOML key is in the sentence, not a field | 109 producers |
| `isdb_ts::Error::Invalid { what, detail: String }` | `what` is structured, the *kind* of violation (range, reserved, inconsistent lengths) is prose | 61 producers |
| `isdbt_ofdm::Error::Tmcc(String)` | which field was reserved is prose | 6 |
| `isdb-radio` | `anyhow::Result` in a library (its docs argue the layers are many); `isdb-cli` cannot tell "no device" from "bad argument" | whole crate |
| `isdb_analyze::edit` | `anyhow` in a library module | 1 module |
| library panics | `unwrap` 31, `expect` 126, `panic!` 4, `unreachable!` 7, `assert!` 78 in non-test library code (`tools/panic-census.py`) | see §3 E4 |

Most `expect`s state an invariant the types could carry. The pilot
(`795b0e9`) retired seven of them by one signature change: `Pid::of_packet`
took `&[u8]` and returned `Option` while every caller held a `[u8; 188]`.

## 2. Rules

### 2.1 Logging

- **Libraries emit `tracing` events and never print.** `tracing` with
  default features off is the only dependency they add; no subscriber,
  no `log` bridge. `isdb-cli` owns the subscriber (`logging.rs`): text
  format `[component] message` on stderr (component = last segment of the
  event's target), or JSON one object per line (`--log-format json`),
  level from `--log-level` or `RUST_LOG`.
- **Reports are not events.** What a subcommand *produces* (a report, a
  table, `--json` output) goes to stdout as today. What it *says while
  working*, and every warning, is an event.
- **The message stands on its own; the fields repeat its values.** The
  text format prints the message only, so a message like `"descriptor
  dropped"` is useless there: write `"PID {pid}: {tag} descriptor dropped
  ({why})"` and add `pid = %pid, tag, why` as fields. Fields are for
  machines (JSON) and tests.
- **Levels.** `error`: the command will fail or the output is wrong.
  `warn`: a deviation from the spec or from the configuration that the
  program worked around (a dropped descriptor, a CC error, an underrun,
  a reserved code on air). `info`: state changes and progress a user
  wants to see by default (device opened, lock acquired/lost, TMCC
  changed, periodic status, end-of-run summary). `debug`: per-frame,
  per-section and per-table detail (RS corrections per frame, a section
  assembled, a timing-loop step). `trace`: per-packet and per-symbol.
  Nothing above `trace` in a per-packet or per-sample loop.
- **An event inside a hot loop goes out of line**, even at `trace`: a
  `#[cold] #[inline(never)]` helper holding the macro, called behind
  `if tracing::enabled!(Level::TRACE)`. The macro's expansion costs speed
  by code size alone with tracing off: the per-symbol timing step inline
  in `isdbt_ofdm::rx::Track::push_symbol` slowed the receiver 3–4 %
  (`perf_realtime_factor` RX 1.49× → 1.43×); out of line it measures the
  same as before the event existed. Measure the crate's throughput test
  before and after any event added to a per-symbol or per-packet path.
- **Target** is the default (module path): `isdbt_mux::relay`, so the
  text format shows `[relay]` and `RUST_LOG=isdbt_mux=debug` works.
  `isdb-cli` modules keep their current tag as the target where it
  differs from the module name (`target: "isdb::tx"`).
- **Fields** use the project's `Display` impls: `pid = %pid`, `layer =
  %id`, `table_id = %tid`; counts and seconds as numbers, not strings.
  Prefer one event with fields over a `format!`ted list.
- **Tests.** Stderr substring matches keep working because the message
  text is unchanged. A test that needs a field parses `--log-format json`
  (`tests/cli.rs::diagnostics_are_structured_events` is the model). For a
  library event, a test captures events with a custom `Layer`; keep it to
  the few events whose presence is the behaviour under test. **Install
  the capturing subscriber globally and filter by thread**, as
  `isdbt-mux/tests/common/mod.rs` and `isdb-radio`'s `queued` tests do:
  `tracing` caches per event site whether any subscriber is interested,
  so a scoped `with_default` subscriber misses events when another test
  thread without a subscriber reaches the same site first (found in
  L2b: the `isdb-radio` capture test failed every time in the full
  suite and passed alone). A scoped subscriber is safe only in a test
  binary whose every test installs one (the L2c `tests/tracing.rs`
  files: 150 runs, no failure).
- **Enforced by the compiler** once every crate has moved:
  `clippy::print_stdout` and `clippy::print_stderr` as workspace lints,
  with `#![allow]` in the `isdb-cli` modules that write reports and in
  the one that implements the subscriber.

### 2.2 Errors

- **A library error is an enum variant with the facts as fields.** The
  message (`#[error("…")]`) is derived from the fields, not the other way
  round. A `String` payload is acceptable only for text that came from
  outside (an OS error message, a TOML parser's message) or for genuinely
  free-form detail *next to* structured fields, never instead of them.
- **Name the syntax element and the rule.** `Error::Range { what:
  "section_number", value, max }` beats `Invalid { detail: "section_number
  300 > 255" }`: the caller can match, the test can assert, the message is
  still the same sentence.
- **A library returns `Result`, never panics on input.** `expect` is for
  invariants the code establishes itself, and then only when no type can
  carry the invariant instead (§2.3). `unwrap` in library code is a bug
  unless on a constant.
- **`anyhow` is for the binary.** A library that mixes error sources
  defines an enum with `#[from]` variants per source and one `Display`
  that reads as a sentence; `isdb-cli` adds context with `anyhow`.
- **Error chains.** `main` returns `anyhow::Result`, which prints the
  chain (`Caused by:`); a library error that wraps another keeps it as
  `#[source]` so the chain is real.

### 2.3 Panics: the triage order

For every `unwrap` / `expect` / `panic!` / `unreachable!` in library code
(`tools/panic-census.py`), in order of preference:

1. **A type carries the invariant**, and the call becomes infallible:
   `&[u8; PACKET_SIZE]` instead of `&[u8]` + length check (the pilot);
   `slice::split_first_chunk::<N>()` / `as_chunks::<N>()` (stable since
   1.77 / 1.88) instead of `try_into().expect("N bytes")`; an enum
   instead of a `match` with `_ => unreachable!()`; `NonZero`, the
   `Pid` / `Segments` style newtypes, an `Option` the constructor already
   excluded.
2. **The function returns `Result`** when the invariant depends on
   input or configuration after all.
3. **The invariant stays, documented**: `expect("<the invariant>")` with a
   `# Panics` section on the public function, only for programming-error
   contracts (a builder used out of order, an index the same function
   computed). `assert!` on a public function's argument contract is fine
   with `# Panics`.

Test code (`#[cfg(test)]`, `tests/`, `examples/`) may unwrap.

## 3. Inventory (disjoint file sets per item)

### Wave 1: logging

- **L1a `isdb-cli` receive side.** The `eprintln!`s of `rx.rs`, `rxs.rs`,
  `rxs3.rs`, `symstream.rs`, `symbols.rs`, `capture.rs`, `iq.rs`,
  `s3sync.rs`, `s3tmcc.rs`, `tmcc.rs`, `satpipe.rs`, `stop.rs` become
  events (`[rx] …` lines → `info!(target: "isdb::rx", …)`, losses and
  resyncs → `warn!`, the Ctrl-C lines → `info!`). Reports (`--json`,
  tables) stay `println!`. Message text unchanged. `tx.rs` (done) is the
  model.
- **L1b `isdb-cli` transmit and tools side.** `loopback.rs`, `mux.rs`,
  `relay.rs`, `ts.rs`, `tlv.rs`, `producer.rs`, `input.rs`, `source.rs`,
  `sink.rs`, `channel.rs`, `iqplay.rs`, `keys.rs`, `phyargs.rs`,
  `bands.rs`. Same rules; `ts.rs` has 50 `println!` reports that stay.
- **L2a ISDB-T libraries.** `isdbt-mux` (relay: every `remarks` /
  `skipped` / `dropped_descriptors` push also a `warn!` with `pid`,
  `tag`, `role` fields at the point of detection, the `Vec`s stay for the
  API; muxer: over-capacity and late PES → `warn!`, carousel rebuild →
  `info!`, section emitted → `trace!`), `isdbt-phy` and `isdbt-ofdm`
  (receiver: lock acquired / lost → `info!` with `offset_hz`, `snr_db`;
  TMCC parameter change → `info!` with the layers; RS uncorrectable count
  per frame → `debug!`; symbol timing step → `trace!`), `isdbt-fec` (remux
  pattern built → `debug!`; nothing per packet).
- **L2b satellite and radio libraries.** `isdbs-phy`, `isdbs3-phy`
  (frame / superframe sync found or lost → `info!`, TMCC decoded or
  changed → `info!` with modulation and rate, BCH / LDPC failure counts
  per frame → `debug!`), `isdb-frontend` (timing lock, AGC settle →
  `info!`; per-chunk statistics → `debug!`), `isdb-radio`, `isdb-iio`,
  `isdb-hackrf` (device opened with its settings → `info!`; underrun,
  overflow, dropped buffer → `warn!` with counts; libiio / libhackrf
  messages → `debug!`), `isdb-iq` (recording opened, SigMF fields read →
  `debug!`).
- **L2c codec libraries.** `isdb-ts` (demux: CC discontinuity, duplicate,
  TEI → `debug!` with `pid`, `expected`, `got`; section CRC failure →
  `debug!`; nothing per packet above `trace`), `isdb-si` (a section with
  a conformance deviation decoded → `debug!`; `conformance::check` stays
  a pure function), `isdb-tlv` and `isdb-mmt` (reassembly dropped an MFU,
  header-compressed IP context missing → `debug!` with the packet id),
  `isdb-text` (unknown control code → `trace!`). `isdb-analyze` emits
  nothing: its output is the report.

After wave 1 merges: `#![warn(clippy::print_stdout, clippy::print_stderr)]`
in every library's `lib.rs` and `allow-print-in-tests = true` in
`clippy.toml`. Not in `[workspace.lints]`: binaries (`isdb-cli`),
examples and integration tests print by design, and a workspace lint
would need an allow in each.

### Status (2026-10-28): wave 1 done

All five items merged (`e2ea95a` L1b, `533f1cf` L1a, `cc3a19d` L2c,
`2f2a57a` L2a, `4e6143d` L2b); the no-print lint is on in all 21
libraries (a planted `println!` in `isdb-ts` fails clippy). `isdb-cli`
has no `eprintln!` left. The interrupted L2a/L2b agents were finished by
fresh agents working in the saved worktrees. Things learned:

- Throughput unchanged where hot paths were touched: muxer 4.02–4.13×
  real time against a 4.05–4.11× baseline; codec demux within noise.
- The FPGA vectors of `isdb-frontend` are byte-identical before and
  after (both configurations).
- Two merges conflicted on the tests both CLI agents appended to
  `tests/cli.rs`; resolving dropped a closing brace (caught by the
  compiler, fixed in the merge commit). `Cargo.lock` conflicts are
  resolved by taking one side and letting cargo add the missing
  `tracing` edges (the diff is the dependency lines only, no version
  change).


### Wave 2: errors

- **E1 `isdb-radio` typed error.** `RadioError { Argument { backend, key,
  value, why }, Unsupported { backend, what }, Device { backend, op,
  source }, Io, Tuning { … } }` (the agent inventories the `bail!` sites
  and groups them); `describe_tx` / `open_tx` / capture return it;
  `isdb-cli` wraps with `anyhow` context as before. The `--dry-run` /
  `--rf-enable` gate's messages must not change (tests match them).
- **E2 `isdbt-mux` config errors.** `MuxError::Config(String)` → `Config
  { key: String, problem: String }` where `key` is the TOML path
  (`services[0].streams[2].pid`) and typed variants for the classes with
  three or more producers after the inventory (`Hex`, `Time`, `Duration`,
  `Capacity` exists). The CLI's error text must still name the key.
- **E3 codec errors.** `isdb_ts::Error` gains `Range { what, value, min,
  max }` and `Reserved { what, value }`; the 61 `Invalid` producers that
  are range or reserved-value checks move (in `isdb-ts`, `isdb-si`,
  `isdb-tlv`, `isdb-mmt`); `Invalid` stays for inconsistent lengths and
  the like. `isdbt_ofdm::Error::Tmcc(String)` → `Tmcc { field, code }`.
  `isdb_analyze::edit` → `EditError` enum, `anyhow` out of the library.
- **E4 library panics** (§2.3), by crate group, each with the census
  before and after: E4a `isdbt-mux`, `isdbt-ofdm`, `isdbt-fec`,
  `isdbt-phy`; E4b `isdb-analyze`, `isdb-ts`, `isdb-si`, `isdb-tlv`,
  `isdb-mmt`, `isdb-text`; E4c `isdb-frontend`, `isdb-iq`, `isdb-radio`,
  `isdb-iio`, `isdb-hackrf`, `isdbs-phy`, `isdbs3-phy`, `isdbs-core`,
  `isdbs3-core`, `isdb-fec`. The slice→array `expect("188 bytes")`
  pattern (6 sites) is triage step 1.

## 4. Brief for a wave-1 item (the agents read this section)

You are in your own git worktree, cut from `main`, not the working
branch: `git status` (clean), then `git reset --hard feat/pluto-backend`.
Commit on your worktree branch and report its name and hash. Temporary
files go in your scratchpad with your item's prefix; the scratchpad may
be shared.

Read: this plan (§2.1 rules, your item in §3), `CONTRIBUTING.md` "Ground
rules", `crates/isdb-cli/src/logging.rs` (the subscriber and the text
format), and `git show b88e62a -- crates/isdb-cli/src/tx.rs` (the worked
example: what became an event, how the fields were chosen, how the
message text was kept).

For a library item: add `tracing.workspace = true` to the crate's
`Cargo.toml` (no features), `use tracing::{debug, info, trace, warn};`
where used. Choose the sites from your item's list and from reading the
crate: every place the crate already records a diagnostic as data (a
`remarks` push, a counter of a deviation, a `problems` string) gets an
event at the same place; state changes a user would want to see get
`info!`. Do not add events inside per-packet or per-sample loops above
`trace!`, and measure with the crate's throughput test (`cargo test
--release -p <crate> … -- --ignored`, VERIFICATION.md "Running the
tests") if you add anything to a hot path. Do not change any public
signature or any `Vec<String>` the CLI prints: the events are in
addition. Do not edit crates outside your item or `Cargo.toml` at the
root.

For an `isdb-cli` item: each `eprintln!` becomes `info!` / `warn!` with
the same message text (minus the `[tag] ` prefix, which the target
supplies: `info!(target: "isdb::rx", …)`) and with the numbers it prints
as fields; `println!` reports stay. Multi-line `eprintln!`s stay one
event with embedded newlines. The CLI tests (`tests/cli.rs`) must pass
unchanged except where a test checked for an exact `[tag]` prefix (then
update the test and say so).

Proof: for an `isdb-cli` item, extend `diagnostics_are_structured_events`
or add one test per subcommand that runs with `--log-format json` and
asserts a field of one event. For a library item, one test per crate that
captures the events of a behaviour the crate already tests (a relay
dropping a descriptor, a CC error) through a capturing `Layer` and
asserts the event's level and one field.

Docs: the crate README gets a sentence under its usage that it emits
`tracing` events and at which levels; CHANGELOG.md one entry at the top
of `## [Unreleased]` → `### Added` naming the crates.

Verify, in order: `cargo fmt --all`; `cargo clippy --workspace
--all-targets --locked -- -D warnings`; `cargo clippy -p isdb-cli
--no-default-features --all-targets --locked -- -D warnings`; `cargo test
--workspace --locked` (two wall-clock tests in `isdb-cli`,
`a_loss_ends_the_stretch` and `live_feed_with_stalling_input…`, flake
under CPU load; rerun alone before concluding); `RUSTDOCFLAGS="-D warnings" cargo doc
--workspace --no-deps --locked`; `cargo deny --locked check bans licenses
sources`. Do not run `mise run msrv` / `oracle` or release builds (shared
disk; the coordinator runs them on the merged tree).

Commit once: `feat(<crates>): tracing events for …` under 100 chars,
body listing the events added by level and anything you left out and
why, last line `Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>`.
`LEFTHOOK=0 git commit` after running the checks yourself if the hook
misbehaves in the worktree.

Report back: branch, hash, events added per crate by level, tests added,
any site you decided not to instrument and why, any failing check.

## 5. Wave 2: errors, by crate group

The items of §3 wave 2 touch the same crates in pairs (E2 and E4a in
`isdbt-mux`, E3 and E4b in the codecs, E1 and E4c in `isdb-radio`), so
the dispatch groups them by crate instead: each agent types the errors
**and** triages the panics of its own crates, and no two agents edit the
same crate. `isdb-cli` is shared: an agent may change `isdb-cli` only
where it calls the agent's crates and must keep those edits to the call
sites (the merge resolves the rest).

| group | crates | §3 items | panic census at `080b297` (unwrap / expect / panic! / unreachable!) |
|---|---|---|---|
| W1 | `isdbt-mux` | E2, E4a | 13 / 17 / 1 / 1 |
| W2 | `isdb-radio`, `isdb-iio`, `isdb-hackrf` | E1, E4c | 2 / 6 / 0 / 0 |
| W3 | `isdb-ts`, `isdb-si`, `isdb-tlv`, `isdb-mmt`, `isdb-text`, `isdb-analyze` | E3 (codec kinds, `EditError`), E4b | 12 / 12 / 0 / 4 |
| W4 | `isdbt-ofdm`, `isdbt-fec`, `isdbt-phy`, `isdbt-core` | E3 (`Error::Tmcc`), E4a | 1 / 26 / 3 / 1 |
| W5 | `isdb-frontend`, `isdb-iq`, `isdbs-phy`, `isdbs3-phy`, `isdbs-core`, `isdbs3-core`, `isdb-fec` | E4c | 2 / 43 / 0 / 1 |

`isdb-cli`'s own 13 `expect`s are not in wave 2: it is a binary, where
`expect` on an invariant ends the program with its message, the same
outcome as `?` into `anyhow` minus the chain. They go with a later pass.

### 5.1 Brief for a wave-2 group (the agents read this section)

You are in your own git worktree, cut from `main`, not the working
branch: `git status` (clean), then `git reset --hard feat/pluto-backend`.
Commit on your worktree branch and report its name and hash. Temporary
files go in your own scratch directory with your group's prefix.

Read: this plan §2.2 (error rules), §2.3 (panic triage order), §3 wave 2
(the items your group covers), and `CONTRIBUTING.md` "Ground rules"
(decoders return errors and never panic; every public item cites its
clause). Worked example of the panic triage: `git show 795b0e9` (a type
carries the invariant: `Pid::of_packet` takes `&[u8; 188]`, seven
`expect`s gone). Census: `python3 -I tools/panic-census.py <crates>`.

**Error typing.** For each error type in your crates, list its variants
and their producers (`grep` the constructors). A variant whose payload
is a `String` built with `format!` from values the code had in hand
becomes a variant with those values as fields and an `#[error("…")]`
that renders the same sentence (or a better one). A free-text payload
stays only for text from outside (an OS or parser message). Group
producers into variants by the rule they enforce (range, reserved code,
inconsistent length, missing item), not one variant per call site. Keep
`#[non_exhaustive]` on public error enums; add it where missing.
`anyhow` leaves library code (W2's `isdb-radio`, W3's
`isdb_analyze::edit`): the library defines the enum, `isdb-cli` adds
`.context()`.

**Messages users and tests see must not regress.** `isdb-cli`'s tests
match error text by substring (`grep -n 'contains(' crates/isdb-cli/tests/cli.rs`);
the radio gate's "dry run, not transmitting" and every relay-policy
refusal (CONTRIBUTING "Relay policy") are documented. Render the same
sentence from the fields; if a sentence improves, update the test and
the docs and say so.

**Panic triage.** Every `unwrap` / `expect` / `panic!` / `unreachable!`
in your crates' library code, in the order of §2.3: a type carries the
invariant; else the function returns `Result`; else the invariant stays
as `expect("<invariant>")` with a `# Panics` doc on the public function.
`assert!` on a public function's argument contract stays with a
`# Panics` section. Report every site with its resolution (a table is
fine). No behaviour change on valid input; on invalid input a panic may
become an error, which you list.

**Proof.** For each new error variant, a test that produces it from bad
input and matches the variant (`assert!(matches!(e, Error::Range { what:
"…", .. }))`), not the message. For a public signature that changed to
`Result`, a test of the error path. Existing `prop_*` round trips and
`spec_*` tests unchanged.

**Do not** edit crates outside your group except `isdb-cli` call sites
of your crates. CHANGELOG.md: one entry at the top of `## [Unreleased]`
→ `### Changed` naming your crates, with a `BREAKING CHANGE:` footer on
the commit if a public type or signature changed.

**Verify**, in order: `cargo fmt --all`; `cargo clippy --workspace
--all-targets --locked -- -D warnings`; `cargo clippy -p isdb-cli
--no-default-features --all-targets --locked -- -D warnings`; `cargo
clippy -p isdb-radio --no-default-features --all-targets --locked -- -D
warnings`; `cargo test --workspace --locked` (the two wall-clock tests in
`isdb-cli`, `a_loss_ends_the_stretch` and `live_feed_with_stalling_input…`,
flake under CPU load: rerun alone before concluding); `RUSTDOCFLAGS="-D
warnings" cargo doc --workspace --no-deps --locked`; `cargo deny --locked
check bans licenses sources`; the census before and after for your
crates. Do not run `mise run msrv` / `oracle` or release builds (shared
disk; the coordinator runs them on the merged tree), except a hot-path
throughput test if you changed a per-packet or per-sample path, and
then report the numbers.

**Commit** once: `refactor(<crates>)!: typed errors and panic triage` (or
`refactor(<crates>): …` with no breaking change) under 100 chars, body
with the census before/after and the variants added, last line exactly
`Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`. `LEFTHOOK=0
git commit` after running the checks yourself if the hook misbehaves in
the worktree.

**Report back:** branch, hash, census before/after per crate, error
variants added or changed per crate, public signature changes,
messages that changed (and the tests/docs updated for them), any check
that did not pass.

## 6. Status (2026-10-28): both waves done

All five wave-2 groups merged (`23bbc15` W4, `a31c65c` W2, `28ae21a` W5,
`262d33c` W1, `f201969` W3); every merge ran the full workspace test
suite, and the finished tree passes the whole CI ladder (fmt, clippy
`-D warnings` incl. the two `--no-default-features` builds, tests,
doctests, rustdoc, cargo-deny, MSRV 1.88, the `oracle/rust_tx3` helper).

**Library panic sites** (`tools/panic-census.py`, `isdb-cli` excluded):

| | at `cd29a67` (start) | now |
|---|---|---|
| `unwrap` | 31 | 12, every one inside a `const` item (a bad value fails the build) |
| `expect` | 126 | 4: three documented invariants the function establishes itself (`isdb-fec` `decode_all`, `isdbt-mux` `analysis` packet index, `playback` timeline) and the `SilentFrame` test-fixture generator |
| `panic!` | 4 | 3: two argument contracts with `# Panics` (`isdbt-core` `segment_layer`, `isdbt-fec` `MultiplexPattern::new`'s model self-check) and `SilentFrame` |
| `unreachable!` | 7 | 0 |

Two of the removed sites were **reachable from input**: a mux TOML
`[[raw_table]]` with `sections = []` (now refused at load), and the float
LDPC decoder with `max_iter = 0` on a non-codeword (now `NotConverged`).

**Error types.** No library uses `anyhow` any more (`isdb-radio` →
`RadioError` with the libiio / libhackrf error as `#[source]`;
`isdb_analyze::edit` → `EditError` / `MutateError`). The free-text
payloads are gone where the values were in hand: `MuxError::Config(String)`
(109 producers) → `Config { key, problem }` plus 13 typed variants keyed
by the TOML path; `isdb_ts::Error` gained `Range`, `Reserved`, `Mismatch`,
`NotAccepted` (18 of 60 `Invalid` producers moved, the rest are length
inconsistencies); `isdbt_ofdm::Error::Tmcc { field, code }`;
`isdbt_phy::BroadcastTsError`; `isdb_frontend::ParamError` and
`isdb_iq::Error` enums; `isdb_iio::Error` / `isdb_hackrf::Error` enums.

**Speed**, measured interleaved against the tree before each change on a
quiet machine: muxer 4.52–4.63× real time (base 4.53–4.68×); ISDB-T
receiver unchanged, transmitter ≈ 17 % faster (W4's allocation-free
mapper and ring FIFOs); satellite front end unchanged, its 16-tap
fallback kernel ≈ 2.4× faster (array arguments let it vectorise); LDPC
unchanged; TS readers slightly faster than before after `580282c`
inlined W3's unit split (which had cost `SliceReader` 13 %). Output is
bit-identical: ISDB-T TX IQ / RX LLRs / RX TS hashed for three
configurations (`isdbt-phy/tests/bitexact_hashes.rs`, ignored), the FPGA
vectors, and the TMCC acceptance over the 21 lab recordings.

**Lessons for the next dispatch**

- Run the **full** workspace suite after every merge, not the merged
  crates' tests: two `isdb-cli` tests broke when the libraries started
  emitting events under the CLI's subscriber, and only a later agent
  noticed (`4f60aa8`).
- `oracle/rust_tx3` has its own `Cargo.lock`; a new dependency of a path
  crate needs `cargo metadata --manifest-path oracle/rust_tx3/Cargo.toml
  --offline` there too (`946cd0e`). `mise run check` catches it.
- `tools/panic-census.py` now cuts at the test *module*, not the first
  `#[cfg(test)]` (W5 found one on a struct field), and skips files a
  parent declares `#[cfg(test)] mod name;`.
- A typed refactor of a per-packet path can cost speed through code
  shape alone (W3's reader, L2a's trace): measure it, and look at
  inlining before anything else.

**`isdb-cli`, done afterwards** ("panic triage of the isdb binary"): its
14 `expect`s and 1 `assert_eq!` are 0 now. Types carry six
(`split_first_chunk::<188>` in the TS splitter, the capture's last segment
held by value, the anchor offset bound by a `match`, the PCR pacer passed
in as `&mut Pacer`, the verifier numbering frames itself instead of
asserting the producer's index); the clap groups are matched once as
`rx::Input` / `tx::Sink` enums, the impossible combination an explicit
error (four sites; the RF-safety gate is unchanged and still before the
device is opened); thread spawns and the loopback's recorded packets
return `Result`; the two joins re-raise a worker's panic
(`resume_unwind`) and the front-end block queue recovers a poisoned lock.

**Left** (not dispatched): `cast_sign_loss` /
`cast_possible_wrap` (213) and `cast_precision_loss` (629, mostly
`usize as f64` in reports and the new event fields) stay off, as in
`type-safety.md` §2.
