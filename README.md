# isdb-rs

A from-the-specification Rust implementation of Japanese digital terrestrial
television (ISDB-T): MPEG-2 transport stream and ARIB service information
codecs, a service multiplexer, a transport-stream analyzer, and the ARIB
STD-B31 modulator and demodulator, with HackRF I/O. Both directions run on
the same library — services → TS → OFDM baseband → radio, and radio → OFDM
→ TS → analysis — so every layer can be checked against its inverse, against
the specifications' own numbers, and against independent receivers. It is
meant for receiver testing, protocol study and broadcast-engineering
experiments on a conducted bench. Only ISDB-T is implemented today; the name
leaves room for ISDB-S and ISDB-S3.

> [!WARNING]
> **Legal and RF safety notice**
>
> - **A licence is required to transmit.** Radiating on broadcast (UHF TV)
>   frequencies requires a radio station licence — in Japan under the Radio
>   Act (電波法), elsewhere under local law. Transmitting without one is
>   illegal and can disrupt television reception for everyone nearby.
> - **Conducted or shielded use only.** `arib tx --hackrf` is intended for
>   lab set-ups in which nothing is radiated: coax from the HackRF through
>   attenuators straight into the receiver, or a shielded enclosure. No
>   antenna. It radiates only with `--rf-enable`; without it (or with
>   `--dry-run`) it prints the RF settings and exits before opening the
>   device. See [docs/transmitting.md](docs/transmitting.md).
> - **No descrambling.** There is no B-CAS / conditional-access support, and
>   scrambled input (transport_scrambling_control ≠ '00') is refused.
> - **Content rights.** You are responsible for having the rights to any
>   content you transmit or relay, including recordings.
> - The software is provided under the Apache License 2.0 **without
>   warranty** (see [LICENSE](LICENSE)).

## Features

- **TS and PSI** (`arib-ts`, ITU-T H.222.0): packets, adaptation field, PCR,
  PES, section assembly and packetisation, PAT/CAT/PMT, continuity
  tracking, a resynchronising reader for 188/192/204-byte packets.
- **ARIB SI** (`arib-si`, ARIB STD-B10): NIT, BAT, SDT, EIT, TDT, TOT, RST,
  ST, PCAT, BIT, NBIT, LDT and the B10 descriptors as typed, *lossless*
  codecs (every reserved bit is kept, so `encode(decode(x)) == x`), MJD/BCD
  time, and a separate conformance checker.
- **ARIB text** (`arib-text`, ARIB STD-B24): the 8-unit character coding of
  SI strings — decode to Unicode with every unrepresentable code reported,
  and encode back.
- **Multiplexer** (`arib-mux`): a TOML service description (network,
  services, events, SI tables, experiment overrides) plus MPEG-2 video and
  AAC ADTS files → a constant-rate multiplex paced to the OFDM frame, with
  an H.222.0 T-STD buffer model. **TS relay**: rebroadcast one program of a
  recorded TS (e.g. a satellite recording) as a terrestrial service with
  its own PSI/SI; the stream roles and PIDs are detected from the recording.
- **Analyzer** (`arib-analyze`): inspect, validate (every finding cites the
  clause it comes from), table/field-level diff of two captures, and edits
  that keep the TS valid (null a PID, remove/replace/insert sections, typed
  field mutation).
- **ISDB-T PHY** (`isdbt-core`, `isdbt-fec`, `isdbt-ofdm`, `isdbt-phy`,
  ARIB STD-B31): Modes 1–3, all guard intervals, up to three hierarchical
  layers with partial reception, DQPSK/QPSK/16QAM/64QAM, all code rates and
  time-interleave lengths, TMCC and AC. The receiver does synchronisation,
  CFO and timing-drift correction, channel estimation, TMCC decoding,
  soft-decision Viterbi and RS decoding, and reproduces the 13-segment TS.
- **I/O** (`hackrf`, `arib-cli`): IQ files (cf32 for GNU Radio, cs8 for
  `hackrf_transfer`) and HackRF One streaming; one `arib` binary with
  `tx`, `rx`, `loopback`, `mux` and `ts` commands.

## Crates

| Crate | Spec | Scope |
|---|---|---|
| [`arib-ts`](crates/arib-ts/README.md) | H.222.0 | bit I/O, CRC_32, TS packet / adaptation field / PCR, PES, section assembly and packetisation, PAT/PMT/CAT, TS reader |
| [`arib-text`](crates/arib-text/README.md) | B24 Vol. 1 Part 2 Ch. 7 | ARIB 8-unit string decode/encode |
| [`arib-si`](crates/arib-si/README.md) | B10 | SI tables, descriptors, MJD/BCD time; lossless typed codecs plus conformance checks |
| [`arib-analyze`](crates/arib-analyze/README.md) | H.222.0, B10, B32 Part 2 | TS inspection, validation with clause citations, diff of two captures, editing |
| [`arib-mux`](crates/arib-mux/README.md) | H.222.0, B10, B31, B32 | service multiplexer: PSI/SI generation, MPEG-2 video / AAC packetisation, recorded-program relay, output paced to the OFDM frame |
| [`isdbt-core`](crates/isdbt-core/README.md) | B31 tables | modes, guard intervals, layer parameters, derived rates and delays |
| [`isdbt-fec`](crates/isdbt-fec/README.md) | B31 §3.2–3.8 | TS re-multiplexing, RS(204,188), energy dispersal, byte interleaving, convolutional code / Viterbi |
| [`isdbt-ofdm`](crates/isdbt-ofdm/README.md) | B31 §3.9–3.16 | mapping, interleaving, OFDM frame with SP/CP/TMCC/AC, IFFT/GI; receiver synchronisation and equalisation |
| [`isdbt-phy`](crates/isdbt-phy/README.md) | B31 | TX/RX pipelines (TS packets ⇄ complex baseband), IQ file formats |
| [`hackrf`](crates/hackrf/README.md) | — | minimal safe wrapper over the system libhackrf (FFI) |
| [`arib-cli`](crates/arib-cli/README.md) | — | the `arib` binary |

How they fit together, the frame/latency contract and the data flow:
[docs/architecture.md](docs/architecture.md).

## Quick start

Tested on Linux with Rust ≥ 1.88 (edition 2024). The `arib` binary links
the system `libhackrf` dynamically, so it must be installed even if you
never attach a radio:

| System | Package |
|---|---|
| Arch Linux | `hackrf` |
| Debian / Ubuntu | `libhackrf-dev` |
| Fedora | `hackrf-devel` |

```sh
git clone https://github.com/a1678991/isdb-rs && cd isdb-rs
cargo build --release -p arib-cli          # → target/release/arib
export PATH="$PWD/target/release:$PATH"
```

Everything below works without a radio and without any captured data.

Software loopback — modulate deterministic test packets, demodulate them
again and compare every packet:

```sh
arib loopback --synthetic                  # ends with "[loopback] PASS"
arib loopback --synthetic --frames 8 --snr-db 22 --cfo-carriers 2.3
```

Print the annotated example multiplex configuration (it references media
files you supply; see [docs/mux-config.md](docs/mux-config.md)):

```sh
arib mux --print-config-example > bench.toml
```

Generate a transport stream from a media-free configuration and analyse it:

```sh
cat > test.toml <<'EOF'
[network]
network_id = 0x7FE0
name = "テスト"
area_code = 0x8D5
remote_control_key_id = 9
channels = [27]

[transport]
transport_stream_id = 0x7FE0

[clock]
source = "fixed"
start = "2026-01-01T12:00:00"

[[service]]
service_id = 0x0400
name = "テストHD"
provider = "テスト"
layer = "B"
pmt_pid = 0x01F0
pcr_pid = 0x01FF
copy_control = { recording_control = "copy-free" }

  [[service.stream]]
  kind = "declared"          # listed in the PMT, no media needed
  pid = 0x0111
  stream_type = 0x02
  component_tag = 0x00

  [[service.event]]
  event_id = 0x0001
  start_offset_s = -600
  duration = "01:00:00"
  name = "試験放送"
EOF
arib mux -c test.toml --seconds 5 --out-ts test.ts
arib ts inspect test.ts                    # PIDs, PCR, programs, decoded SI
arib ts validate test.ts                   # 0 errors (one warning: the declared stream has no packets)
```

Then put it through the PHY in files and back:

```sh
arib tx --mux-config test.toml --iq test.cf32 --seconds 3   # ≈ 195 MB of cf32
arib rx --iq test.cf32 --out rx.ts --json
arib ts diff test.ts rx.ts                 # every section identical
```

Next steps: [docs/getting-started.md](docs/getting-started.md) (a longer
walk-through), [docs/transmitting.md](docs/transmitting.md) (bench set-up,
HackRF, relaying a recording), [docs/receiving.md](docs/receiving.md),
[docs/analysis.md](docs/analysis.md), [docs/mux-config.md](docs/mux-config.md),
[docs/test-data.md](docs/test-data.md), [docs/glossary.md](docs/glossary.md).

## How it is verified

The specifications are the only ground truth. Other implementations are
never copied; they serve only as independent oracles, and a disagreement
is settled by reading the spec. Every layer is checked without trusting the
implementation itself:

1. **Spec vectors** — numbers printed in the specs (check values, tables,
   PRBS states, constellation points, frame lengths, TSP counts) are
   asserted directly in tests named after the table.
2. **Lossless round trips** — decoders keep every bit, and
   `encode(decode(x)) == x` is property-tested and run over synthetic
   multiplexes and, optionally, over every section of your own capture.
3. **Inverse pairs** — each TX stage is tested against its RX inverse, and
   the whole PHY as TX → impairments (noise, CFO, timing offset, echo) → RX,
   byte-exact.
4. **Differential oracle** — the Rust TX and RX cross-decode byte-exactly
   with GNU Radio's gr-isdbt in both directions in 8 parameter sets; every
   place where the two differ is a gr-isdbt deviation from B31.
5. **Real receivers** — over a conducted link, a MyGica S270 USB tuner
   decoded the Rust TX with zero packet errors, and a Sony BDZ-series
   recorder registered and played the service.

Results, reproduction steps and open questions:
[VERIFICATION.md](VERIFICATION.md). Field *values* (reserved bits, allowed
table_id per PID, repetition rates) are checked by validators kept
separate from the lossless codecs, so non-conforming captures can still be
decoded, edited and re-encoded.

## Specifications

| Spec | Used for |
|---|---|
| ITU-T H.222.0 (05/2006) \| ISO/IEC 13818-1 | transport stream, PES, PSI, T-STD |
| ARIB STD-B10 v5.13 | service information |
| ARIB STD-B24 v6.4 Vol. 1 | 8-unit character coding |
| ARIB STD-B31 v2.2 | ISDB-T transmission |
| ARIB STD-B32 v3.11 Parts 1–3 | video/audio coding constraints, stream types |

The specification PDFs are **not** redistributed here (they are
copyrighted). ITU-T recommendations and the English translations of the
ARIB standards are free downloads; [SPECS.md](SPECS.md) lists the URLs,
the SHA-256 hashes of the editions used, how to extract greppable text into
the gitignored `specs/` directory, which clauses each crate implements and
how each is confirmed. Clause and table numbers in code comments refer to
these editions. Where the text is ambiguous or garbled, the reading adopted
is recorded with the quoted text in the crates' `SPEC-NOTES.md`
([index](SPEC-NOTES.md)).

## Status and limitations

The library and CLI generate, transmit, receive and analyse ISDB-T end to
end, but some parts have had less independent checking than others:

- **One receiver configuration.** Real receivers have only been shown Mode
  3, GI 1/8, partial reception, layer A 1 segment QPSK 2/3, layer B 12
  segments 64QAM 3/4 (time-interleave index 2), on two receivers. Other
  modes and layouts are verified by the oracle and loopback only.
- **No off-air capture yet.** No IQ capture of a real broadcaster has been
  decoded. The TMCC/AC parity bit order agrees with gr-isdbt but is not
  settled by the spec text alone.
- **DQPSK, AC content, the emergency-alarm flag and parameter switching**
  are verified by self-consistent loopback only (gr-isdbt implements none
  of them).
- **ARIB TR-B14** (terrestrial operational guidelines) is not available to
  the project, so operational values (PCR positions, SI layer placement,
  remote-control key range, PID plan, …) are documented choices, listed in
  the [arib-mux notes](crates/arib-mux/SPEC-NOTES.md#operational-choices-not-dictated-by-the-local-specs--test-on-receivers).
- **AAC is checked at the header level only.** ADTS headers are checked
  against ARIB STD-B32 Part 2; crc_check values are neither verified nor
  inserted, because parsing the raw_data_block needs ISO/IEC 13818-7, which
  the project does not have. Non-conforming AAC is refused by default.
- **Media**: MPEG-2 video and AAC ADTS files only; H.264 one-seg streams
  can be declared in the PMT but are not packetised.
- **Not implemented**: duplicate packets for a PID on two layers, the
  broadcast-TS information packet (IIP), SFN synchronisation, data
  broadcasting (BML/DSM-CC), DRCS glyphs, ISDB-T<sub>SB</sub>.
- **Radio**: HackRF One only. The demodulator runs at about 1.4× real time
  on a Ryzen 9 7950X, so live reception needs a fast machine.

## Roadmap

- Decode an off-air ISDB-T IQ capture (TMCC/AC parity order, slot order of
  the re-multiplexed TS) and test more configurations and receivers.
- Replace operational choices with TR-B14 values where they can be
  confirmed.
- AAC raw_data_block parsing (crc_check verification) once it can be
  written from ISO/IEC 13818-7 itself; H.264 one-seg packetisation.
- ISDB-S (ARIB STD-B20) and ISDB-S3 (ARIB STD-B44) transmission, reusing
  the TS/SI crates — not started.

## Development

Contribution rules (specifications as ground truth, SPEC-NOTES entries,
RF safety for hardware tests, toolchain, hooks and commit format) are in
[CONTRIBUTING.md](CONTRIBUTING.md); security issues go through
[SECURITY.md](SECURITY.md); changes are listed in
[CHANGELOG.md](CHANGELOG.md).

```sh
cargo test --workspace --release           # all tests; no radio or data needed
cargo test --workspace --doc               # doctests, including every crate README
cargo clippy --workspace --all-targets
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps
```

The dev profile builds the signal-processing crates and all dependencies
with `opt-level = 3`, because unoptimised the modulator runs at ≈ 0.2× real
time and a HackRF would underrun; use `--release` for transmissions anyway.
Heavy tests are `#[ignore]`d; tests that use real captures, recordings or
media are opt-in through `ISDB_RS_*` environment variables and print
`SKIP` otherwise ([docs/test-data.md](docs/test-data.md)). Conventions:
every public item's doc comment names its clause or table; spec-vector
tests are named `spec_<table>_…` and property tests `prop_…`; no `unsafe`
outside the `hackrf` FFI crate; decoders never panic on malformed input;
soft bits are `f32` LLRs `ln P(b=0)/P(b=1)`.

## License

Licensed under the Apache License, Version 2.0 ([LICENSE](LICENSE); see
also [NOTICE](NOTICE)). Unless you explicitly state otherwise, any
contribution intentionally submitted for inclusion in this work shall be
licensed as above, without any additional terms or conditions.
