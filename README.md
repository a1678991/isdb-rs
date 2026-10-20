[日本語版 README](README.ja.md)

# isdb-rs

A from-the-specification Rust implementation of Japanese digital terrestrial
television (ISDB-T): MPEG-2 transport stream and ARIB service information
codecs, a service multiplexer, a transport-stream analyzer, and the ARIB
STD-B31 modulator and demodulator, with SDR I/O (HackRF One and the ADALM-Pluto family). Both directions run on
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
> - **Conducted or shielded use only.** `isdb tx --radio` is intended for
>   lab set-ups in which nothing is radiated: coax from the radio through
>   attenuators straight into the receiver, or a shielded enclosure. No
>   antenna. It radiates only with `--rf-enable`: `--radio` without
>   `--rf-enable` or `--dry-run` is rejected, and `--dry-run` prints the RF
>   settings and exits before opening the device. The centre frequency is limited to the UHF TV band, 470–710 MHz
>   (channels 13–52), unless `--freq-unrestricted` is given. See
>   [docs/transmitting.md](docs/transmitting.md).
> - **No descrambling.** There is no B-CAS / conditional-access support and
>   the tool never descrambles. Input must be unscrambled
>   (transport_scrambling_control '00'); scrambled input is refused. A
>   relayed program keeps its source copy-control signalling and its
>   CA_descriptor/ECM by default.
> - **Content rights.** You are responsible for having the rights to any
>   content you transmit or relay, including recordings. Rebroadcasting a
>   broadcaster's programme, even privately over coax, may be restricted
>   where you live.
> - The software is provided under the Apache License 2.0 **without
>   warranty** (see [LICENSE](LICENSE)).
> - **No affiliation.** isdb-rs is an independent project. It is not
>   affiliated with, endorsed by or sponsored by ARIB, ITU, ISO/IEC, Great
>   Scott Gadgets, Analog Devices, OpenSourceSDRLab, Sony, MyGica or any
>   broadcaster. These names, and product names such as HackRF,
>   ADALM-Pluto, PlutoSky and BDZ, are used only to identify the standards
>   implemented and the equipment tested; they may be trademarks of their
>   respective owners (HackRF is a trademark of Great Scott Gadgets,
>   ADALM-Pluto of Analog Devices).

## Features

- **TS and PSI** (`isdb-ts`, ITU-T H.222.0): packets, adaptation field, PCR,
  PES, section assembly and packetisation, PAT/CAT/PMT, continuity
  tracking, a resynchronising reader for 188/192/204-byte packets.
- **ARIB SI** (`isdb-si`, ARIB STD-B10): NIT, BAT, SDT, EIT, TDT, TOT, RST,
  ST, PCAT, BIT, NBIT, LDT and the B10 descriptors as typed, *lossless*
  codecs (every reserved bit is kept, so `encode(decode(x)) == x`), MJD/BCD
  time, and a separate conformance checker.
- **ARIB text** (`isdb-text`, ARIB STD-B24): the 8-unit character coding of
  SI strings — decode to Unicode with every unrepresentable code reported,
  and encode back.
- **Multiplexer** (`isdbt-mux`): a TOML service description (network,
  services, events, SI tables, experiment overrides) plus MPEG-2 video and
  AAC ADTS files → a constant-rate multiplex paced to the OFDM frame, with
  an H.222.0 T-STD buffer model. **TS relay**: rebroadcast one program of
  an unscrambled recorded TS (for example one you produced yourself) as a
  terrestrial service with its own PSI/SI; the stream roles and PIDs are
  detected from the recording, and the source's copy control and
  conditional-access signalling are kept by default. **Data broadcasting
  and Hybridcast**: the files of a directory as a BML data carousel
  (DSM-CC DII/DDB, ARIB STD-B24 Vol. 2/3) with the PMT and EIT signalling,
  and an AIT (B24 Vol. 4, IPTVFJ STD-0010/0013) that names the HTML5
  application URL a Hybridcast receiver starts, carried in the carousel
  with a generated launcher or on its own PID.
- **Analyzer** (`isdb-analyze`): inspect, validate (every finding cites the
  clause it comes from), table/field-level diff of two captures, and edits
  that keep the TS valid (null a PID, remove/replace/insert sections, typed
  field mutation).
- **ISDB-T PHY** (`isdbt-core`, `isdbt-fec`, `isdbt-ofdm`, `isdbt-phy`,
  ARIB STD-B31): Modes 1–3, all guard intervals, up to three hierarchical
  layers with partial reception, DQPSK/QPSK/16QAM/64QAM, all code rates and
  time-interleave lengths, TMCC and AC. The receiver does synchronisation,
  CFO and timing-drift correction, channel estimation, TMCC decoding,
  soft-decision Viterbi and RS decoding, and reproduces the 13-segment TS.
- **I/O** (`isdb-radio`, `isdb-hackrf`, `isdb-iio`, `isdb-cli`): IQ files
  (cf32 for GNU Radio, cs8 for `hackrf_transfer`) and radio streaming
  through device-neutral IQ sink/source traits with one backend per device
  (HackRF One, `--radio hackrf`; ADALM-Pluto and compatibles such as the
  PlutoSky R2 over USB or Ethernet, `--radio pluto`); one `isdb` binary with `tx`, `rx`,
  `loopback`, `mux`, `ts` and `completions` commands.

## Crates

| Crate | Spec | Scope |
|---|---|---|
| [`isdb-ts`](crates/isdb-ts/README.md) | H.222.0 | bit I/O, CRC_32, TS packet / adaptation field / PCR, PES, section assembly and packetisation, PAT/PMT/CAT, TS reader |
| [`isdb-text`](crates/isdb-text/README.md) | B24 Vol. 1 Part 2 Ch. 7 | ARIB 8-unit string decode/encode |
| [`isdb-si`](crates/isdb-si/README.md) | B10, B24 Vol. 2–4 | SI tables, descriptors, MJD/BCD time; lossless typed codecs plus conformance checks; DSM-CC data carousel (DII/DDB), AIT and BML signalling structures |
| [`isdb-analyze`](crates/isdb-analyze/README.md) | H.222.0, B10, B32 Part 2 | TS inspection, validation with clause citations, diff of two captures, editing |
| [`isdbt-mux`](crates/isdbt-mux/README.md) | H.222.0, B10, B24, B31, B32, IPTVFJ STD-0010/0013 | service multiplexer: PSI/SI generation, MPEG-2 video / AAC packetisation, recorded-program relay, BML data carousel and Hybridcast AIT, output paced to the OFDM frame |
| [`isdbt-core`](crates/isdbt-core/README.md) | B31 tables | modes, guard intervals, layer parameters, derived rates and delays |
| [`isdbt-fec`](crates/isdbt-fec/README.md) | B31 §3.2–3.8 | TS re-multiplexing, RS(204,188), energy dispersal, byte interleaving, convolutional code / Viterbi |
| [`isdbt-ofdm`](crates/isdbt-ofdm/README.md) | B31 §3.9–3.16 | mapping, interleaving, OFDM frame with SP/CP/TMCC/AC, IFFT/GI; receiver synchronisation and equalisation |
| [`isdbt-phy`](crates/isdbt-phy/README.md) | B31 | TX/RX pipelines (TS packets ⇄ complex baseband), IQ file formats, the broadcasting TS with ISDB-T_information and IIP (Part 2 §5.5) |
| [`isdb-radio`](crates/isdb-radio/README.md) | — | device-neutral SDR layer: IQ sink/source traits, `--radio` device selection, backend registry (HackRF behind the `hackrf` feature, ADALM-Pluto behind `pluto`), integer sample quantisers |
| [`isdb-hackrf`](crates/isdb-hackrf/README.md) | — | minimal safe wrapper over the system libhackrf (FFI), used by the `isdb-radio` HackRF backend |
| [`isdb-iio`](crates/isdb-iio/README.md) | — | minimal safe wrapper over the system libiio 0.x (FFI), used by the `isdb-radio` Pluto backend |
| [`isdb-cli`](crates/isdb-cli/README.md) | — | the `isdb` binary |

How they fit together, the frame/latency contract and the data flow:
[docs/architecture.md](docs/architecture.md).

## Quick start

Tested on Linux with Rust ≥ 1.88 (edition 2024). Radio support is two
cargo features of `isdb-cli` (forwarded to the `isdb-radio` backends),
both on by default: `hackrf` links the system `libhackrf` (2022.09.1 or
later) and `pluto` the system `libiio` (0.x, 0.21 or later) dynamically,
so the default build needs both libraries installed even if you never
attach a radio:

| System | Packages |
|---|---|
| Arch Linux | `hackrf libiio` |
| Debian / Ubuntu | `libhackrf-dev libiio-dev` |
| Fedora | `hackrf-devel libiio-devel` |

```sh
git clone https://github.com/a1678991/isdb-rs && cd isdb-rs
cargo build --release -p isdb-cli          # → target/release/isdb
export PATH="$PWD/target/release:$PATH"
```

Without either library, build with
`cargo build --release -p isdb-cli --no-default-features` (add
`--features hackrf` or `--features pluto` for one of them): every
file-based command (`tx --iq`, `rx --iq`, `loopback`, `mux`, `ts`) works,
and `--radio` with a left-out backend fails with a message naming the
feature.

Everything below works without a radio and without any captured data.

Software loopback — modulate deterministic test packets, demodulate them
again and compare every packet:

```sh
isdb loopback --synthetic                  # ends with "[loopback] PASS"
isdb loopback --synthetic --frames 8 --snr-db 22 --cfo-carriers 2.3
```

Print the annotated example multiplex configuration (it references media
files you supply; see [docs/mux-config.md](docs/mux-config.md)):

```sh
isdb mux --print-config-example > bench.toml
```

Generate a transport stream from a media-free configuration and analyse it:

```sh
cat > test.toml <<'EOF'
[network]
network_id = 0x79EE            # placeholder: pick an ID unused where you are
name = "テスト"
area_code = 0x5A5
remote_control_key_id = 9
channels = [27]

[transport]
transport_stream_id = 0x79EE

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
isdb mux -c test.toml --seconds 5 --out-ts test.ts
isdb ts inspect test.ts                    # PIDs, PCR, programs, decoded SI
isdb ts validate test.ts                   # 0 errors (one warning: the declared stream has no packets)
```

Then put it through the PHY in files and back:

```sh
isdb tx --mux-config test.toml --iq test.cf32 --seconds 3   # ≈ 195 MB of cf32
isdb rx --iq test.cf32 --out rx.ts --json
isdb ts diff test.ts rx.ts                 # every section identical
```

Next steps: [docs/getting-started.md](docs/getting-started.md) (a longer
walk-through), [docs/transmitting.md](docs/transmitting.md) (bench set-up,
radios, relaying a recording), [docs/receiving.md](docs/receiving.md),
[docs/analysis.md](docs/analysis.md), [docs/mux-config.md](docs/mux-config.md),
[docs/test-data.md](docs/test-data.md), [docs/glossary.md](docs/glossary.md).

## How it is verified

The specifications are the only ground truth. Other implementations are
never copied; they serve only as independent oracles, and a disagreement
is settled by reading the spec. No code or tables from other ISDB/ARIB
implementations are included: the JIS X 0208 → Unicode mapping comes at
run time from the `encoding_rs` dependency (WHATWG index), and the
gr-isdbt source was read only after the implementation existed, to explain
oracle disagreements ([VERIFICATION.md](VERIFICATION.md#discrepancies)).
Every layer is checked without trusting the implementation itself:

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
   decoded the Rust TX with zero packet errors, and one Sony BDZ-series
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
  the [isdbt-mux notes](crates/isdbt-mux/SPEC-NOTES.md#operational-choices-not-dictated-by-the-local-specs--test-on-receivers).
- **AAC is checked at the header level only.** ADTS headers are checked
  against ARIB STD-B32 Part 2; crc_check values are neither verified nor
  inserted, because parsing the raw_data_block needs ISO/IEC 13818-7, which
  the project does not have. Non-conforming AAC is refused by default.
- **Media**: MPEG-2 video and AAC ADTS files only; H.264 one-seg streams
  can be declared in the PMT but are not packetised.
- **Data broadcasting works on a receiver only under a registered
  broadcaster's identity.** The carousel and its signalling follow ARIB
  STD-B24 Vol. 2–4 (layouts checked against four live terrestrial
  stations) and a Sony recorder displayed this project's BML page, but
  only when the service carried a local station's own network / service
  IDs on that station's channel; under the placeholder network it shows
  the "d" mark and never starts the BML browser. Hybridcast (IPTVFJ
  STD-0010/0013) is implemented per the texts; the values without a local
  source (start_priority bit, monitored AIT module, AIT
  data_component_id) are documented defaults, no Hybridcast-capable
  receiver has been tested, and which organization_id values a receiver
  accepts is IPTV Forum policy.
- **Not implemented**: duplicate packets for a PID on two layers, SFN
  synchronisation (only the IIP's `network_synchronization_information`
  syntax is handled), AC data in the broadcasting TS, data events and
  event messages of data broadcasting, DRCS glyphs, ISDB-T<sub>SB</sub>.
- **Radio**: HackRF One and the ADALM-Pluto family are the backends
  (others plug into `isdb-radio` without CLI changes). A Pluto streams one
  direction at a time at 512/63 MHz over Ethernet: TX and RX together
  exceed what its iiod serves with ADI's firmware (one CPU enabled), and
  the receive side then drops samples. A PlutoSky R2 needs Tezuka
  firmware v0.3.22 or later to receive (its factory firmware leaves the
  reference oscillator wandering). The demodulator runs at about 1.4× real time
  on a current 16-core desktop CPU, so live reception needs a fast machine.

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

[mise](https://mise.jdx.dev/) is the single task runner for checks (tasks
in [mise.toml](mise.toml)), and CI runs the same tasks:

```sh
mise run check                             # everything CI runs: lint, test, doc, deny, msrv, oracle helper
mise run test                              # unit, integration and doc tests; no radio or data needed
mise run lint                              # rustfmt check + clippy -D warnings
mise tasks                                 # all tasks
```

[CONTRIBUTING.md](CONTRIBUTING.md) has the rules (specifications as ground
truth, SPEC-NOTES entries, RF safety for hardware tests, the relay
copy-control and conditional-access policy), the toolchain, git hooks and
commit format; the code layout is in
[docs/architecture.md](docs/architecture.md). Security issues go through
[SECURITY.md](SECURITY.md); changes are listed in
[CHANGELOG.md](CHANGELOG.md).

Use `--release` builds for anything real time: unoptimised, the modulator
runs at ≈ 0.2× real time and a HackRF would underrun. Heavy tests are
`#[ignore]`d (`mise run test-ignored`); tests that use real captures,
recordings or media are opt-in through `ISDB_RS_*` environment variables
and print `SKIP` otherwise ([docs/test-data.md](docs/test-data.md)).

## License

Licensed under the Apache License, Version 2.0 ([LICENSE](LICENSE); see
also [NOTICE](NOTICE)). Unless you explicitly state otherwise, any
contribution intentionally submitted for inclusion in this work shall be
licensed as above, without any additional terms or conditions.
