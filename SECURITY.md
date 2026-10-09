# Security policy

## Supported versions

isdb-rs has not had a release yet. Security fixes go to the `main` branch;
once releases exist, only the latest release receives fixes.

## Reporting a vulnerability

Please report vulnerabilities **privately**. Do not open a public issue,
pull request or discussion for them.

- Preferred: GitHub private vulnerability reporting — on
  <https://github.com/a1678991/isdb-rs>, open the **Security** tab and choose
  **Report a vulnerability**.
- Alternatively, e-mail the maintainer at the address in the `authors` field
  of the workspace `Cargo.toml`.

Include the affected crate(s) and commit, a description of the impact, and
a reproducer if you have one (for parser issues, a minimal input; do not
send captures or recordings you are not allowed to share).

You can expect an acknowledgement within 7 days. Once a fix is ready, the
issue is disclosed through a GitHub security advisory (and a RustSec
advisory for published crates), crediting the reporter unless they prefer
otherwise.

## Scope

In scope, for example:

- Memory-safety issues (the only `unsafe` code is the libhackrf FFI wrapper,
  `crates/isdb-hackrf`).
- Panics, unbounded memory use or hangs triggered by malformed transport
  streams, sections, descriptors, strings or IQ input in the decoders and
  analysers (`isdb-ts`, `isdb-text`, `isdb-si`, `isdb-analyze`, the PHY
  crates `isdbt-core`/`isdbt-fec`/`isdbt-ofdm`/`isdbt-phy`, `isdb rx`,
  `isdb ts`).
- Bugs that make `isdb tx` radiate without `--rf-enable`, or keep the
  device transmitting after the program reports that it stopped.

Out of scope: radio-regulation questions about how someone operates their
own equipment, and requests for descrambling / conditional-access support
(not a goal of this project).
