# Repository instructions

## Start here

- Investigate the relevant code, tests and specifications before changing
  behaviour. Explain conclusions concisely, with evidence and explicit unknowns.
- Read [CONTRIBUTING.md](CONTRIBUTING.md) for project policy and the affected
  crate's README and SPEC-NOTES for its contracts. Use
  [docs/architecture.md](docs/architecture.md) for data flow,
  [SPECS.md](SPECS.md) for specification editions and
  [VERIFICATION.md](VERIFICATION.md) for recorded verification evidence.
  Historical measurements and plans are not proof of current behaviour.
- Inspect `git status` first; preserve unrelated edits and worktrees. Keep
  reusable investigation scripts in `scripts/` or `tools/` and commit them;
  keep generated data, build products and large logs out of git.

## Public repository and local overrides

- This repository is public. In committed code, documentation, examples,
  configuration and verification reports, use placeholders for host-specific
  IP addresses, hostnames and filesystem paths. For example, use
  `ip:<DEVICE_IP>` and `/path/to/capture.ts`, not actual device addresses,
  usernames, home directories or recording locations. Repository-relative
  paths such as `fpga/build/` should remain concrete.
- Make host-specific settings overridable at execution time through CLI
  arguments, environment variables or gitignored local configuration. Prefer
  existing interfaces such as `ISDB_PLUTO_URI` and `ISDB_RS_CAPTURE_TS`;
  document the override mechanism when adding a new setting.
- The user or agent may substitute actual values locally for an authorized
  run without editing committed defaults. Do not write resolved values back
  into tracked files. Sanitize command transcripts and generated reports
  before committing or posting them publicly, and review staged diffs for
  accidental disclosure of local addresses or paths.

## Repository map

This is a Rust workspace implementing Japanese digital broadcasting from
specifications, with an Amaranth FPGA implementation alongside it.

| Area | Location |
| --- | --- |
| TS/PES/PSI, SI, text and captions | `crates/isdb-ts`, `isdb-si`, `isdb-text`, `isdb-caption` |
| Multiplexing, analysis and editing | `crates/isdbt-mux`, `isdb-analyze` |
| Terrestrial parameters, FEC, OFDM and TX/RX | `crates/isdbt-core`, `isdbt-fec`, `isdbt-ofdm`, `isdbt-phy` |
| Shared FEC and satellite chains | `crates/isdb-fec`, `isdbs-core`, `isdbs-phy`, `isdbs3-core`, `isdbs3-phy` |
| Advanced-BS TLV/MMT and media output | `crates/isdb-tlv`, `isdb-mmt`, `isdb-mkv`, `isdb-remux` |
| IQ files, symbol front end and radio abstraction | `crates/isdb-iq`, `isdb-frontend`, `isdb-radio` |
| Native radio FFI | `crates/isdb-hackrf`, `isdb-iio` |
| `isdb` command-line application | `crates/isdb-cli` |
| HDL, simulation, synthesis and board integration | `fpga/` (read `fpga/README.md`) |
| Independent differential checks | `oracle/`; `oracle/rust_tx3` is a separate Cargo project |

`package.json` is repository tooling for commitlint, not an application.

## Implementation contracts

- Published specifications are the ground truth. Cite the implemented clause
  or table in public API documentation. Do not copy code, constants or tables
  from other implementations; use them only as independent differential
  oracles. Resolve disagreements by reading the specification.
- Record ambiguous interpretations in the implementing crate's SPEC-NOTES;
  add new files to the root [index](SPEC-NOTES.md). Cross-crate decisions go
  in the root file. Mark an interpretation confirmed only with named evidence.
- Preserve all decoded bits, including reserved bits, stuffing and unknown
  raw bytes. Keep syntax decoding separate from conformance validation;
  enforce `encode(decode(x)) == x` for decodable input. Malformed input must
  return errors, not panic.
- Keep the generator and checker independent: the analyzer must not reuse
  the multiplexer logic that it is meant to validate. Preserve frame counts,
  latency and timing contracts when changing a PHY stage.
- Inherit workspace lints. `unsafe` belongs only in the existing
  `isdb-hackrf` and `isdb-iio` FFI boundaries. Explain lint allowances with
  the invariant or reason; do not suppress warnings just to pass checks.
- Libraries report through return values and `tracing`, not stdout/stderr
  prints or their own subscribers. Applications own output and logging setup.
  Keep hot-loop logging out of line and measure throughput when changing it;
  see [the logging rules](docs/plans/errors-and-logging.md).
- Keep workspace dependencies versioned alongside their path, following the
  existing publish-cycle dev-dependency exception. New dependencies must pass
  cargo-deny and the MSRV. Treat an MSRV increase as an explicit change.

## Development and validation

Run commands from the repository root unless stated otherwise. Rustup reads
`rust-toolchain.toml`; the MSRV is `workspace.package.rust-version` in
`Cargo.toml`. `mise install` installs the pinned harness tools, pnpm tooling
and lefthook hooks. `mise tasks` lists the commands defined in `mise.toml`.

The default radio features link system libhackrf and libiio (0.x API).
For a file-only CLI build without either library:

```sh
cargo build --locked -p isdb-cli --no-default-features
```

Choose checks for the affected behaviour, then run the applicable broader
checks before handing off. Do not claim skipped or unrun checks passed.

| Change/check | Command |
| --- | --- |
| Focused Rust tests | `cargo test --locked -p <crate>` |
| Formatting and all-target clippy | `mise run lint` |
| Workspace unit, integration and doc tests | `mise run test` |
| Feature combinations | `mise run features` |
| Rustdoc | `mise run doc` |
| Dependency policy and MSRV | `mise run deny` and `mise run msrv` |
| Full Rust check group, including oracle helper lint | `mise run check` |
| Heavy ignored stress/throughput tests, when relevant | `mise run test-ignored` |
| FPGA/model changes | `mise run fpga:test` (separate from `check`) |
| Workflow/action changes | `mise run actions:lint` (separate from `check`) |

- Tests asserting published values use `spec_<table>_…`; property tests use
  `prop_…`. Test codec round trips, malformed inputs and TX/RX stage inverses.
- Committed fixtures must be synthetic and generated by this project's own
  encoders. Never commit recordings, IQ/media files or specification PDFs and
  full text extractions. Local specification extractions belong in ignored
  `specs/`; real-data tests use `ISDB_RS_*` variables and skip with an explicit
  message when data is absent. See [docs/test-data.md](docs/test-data.md).
- Keep wall-clock-sensitive tests in their own test binary, following
  `crates/isdb-cli/tests/live_feed.rs`; do not make correctness depend on host
  speed. Use `tools/ci-flake-check.sh` when investigating timing flakes.
- Changes to terrestrial FEC/OFDM/PHY behaviour also need the differential
  oracle procedure in `oracle/README.md` and CONTRIBUTING. `mise run oracle`
  only lints/build-checks the helper; it does not run that differential test.
- HDL must match the Rust/Python reference vectors bit for bit. `fpga:test`
  generates vectors in `fpga/build/` and runs the locked uv test environment.
  Verilator is the preferred compiled backend where installed; CXXRTL is
  also available via `FPGA_SIM`. For real decoder/chain changes, also run
  `FPGA_CHAIN_REAL=1 mise run fpga:test` and verify that
  `RealDecoderChainTest` actually ran. Preserve end-to-end assertions;
  stub tests and throughput measurements alone do not prove correctness.
- After changing an Actions `uses:`, run `mise run actions:lock` and commit
  `.github/workflows/actions.lock` with it. Use the existing
  `uses: $/.github/actions/<name>` form for local actions. Review changed
  resolved commits and repository identities in the lockfile.

## Radio and content boundaries

- Automated tests and CI must never open a radio. `describe_tx`,
  `describe_rx` and `--dry-run` validate settings without opening devices.
- Preserve the explicit `--rf-enable` transmission gate, stream shutdown and
  amplifier-off handling on every exit path. Hardware TX checks require a
  conducted or shielded bench, attenuation, low gain and bounded runs; record
  the setup and results. Follow CONTRIBUTING's RF procedure.
- Do not add descrambling or conditional-access support. Preserve the
  existing scrambled-input rejection or receiver pass-through behaviour.
  Relays keep source CA/ECM and copy-control signalling by default; relaxing
  these requires explicit user options and visible reporting.

## Finishing work

- Update affected crate documentation and SPEC-NOTES alongside behaviour.
  Keep English/Japanese README usage aligned when changing public commands;
  add user-visible changes under `Unreleased` in `CHANGELOG.md`.
- Use Conventional Commits. Optional scopes are crate directory names or
  `fpga`, `oracle`, `workspace`, `ci`, `deps` (see `commitlint.config.mjs`).
- Commit a verified logical checkpoint locally, staging only relevant files.
  Keep hooks enabled; investigate failures rather than bypassing them by
  default. Report the commit hash, change summary, checks and limitations.
- Work on a branch. Publishing, pushing and merging require the user's
  instruction; the repository's PR workflow expects a green `CI OK` and a
  rebase merge. Never push directly to `main` or bypass required checks.
