[日本語版 README](README.ja.md)

# isdb-rs

**Japanese digital broadcasting — ISDB-T, ISDB-S and ISDB-S3 — implemented
in Rust from the specifications.** Build a transport stream from a service
description, modulate it to IQ and transmit it on a conducted bench;
receive terrestrial and satellite signals back to transport streams or TLV
streams; inspect, validate, diff and edit what you get.

Every direction runs on the same library, so each layer is checked against
its inverse, against the numbers printed in the standards, and against
independent receivers (a USB tuner, a TV recorder, GNU Radio's gr-isdbt).
It is meant for receiver testing, protocol study and broadcast-engineering
experiments in the lab.

```text
 services (TOML) ─▶ isdb mux ─▶ TS ─▶ isdb tx ─▶ IQ file / radio
                                                    │
 analysis ◀─ isdb ts ◀─ TS / TLV ◀─ isdb rx ◀───────┘
```

At a glance (`test.toml` as in the [quick start](#quick-start)):

```sh
isdb mux -c test.toml --seconds 10 --out-ts out.ts            # services → transport stream
isdb tx --mux-config test.toml --iq out.cf32 --seconds 3      # → ISDB-T IQ file
isdb rx --iq out.cf32 --out rx.ts                             # IQ → transport stream
isdb ts inspect rx.ts                                         # PIDs, programs, decoded SI
isdb ts validate rx.ts                                        # findings, each citing its clause
isdb ts diff out.ts rx.ts                                     # section-level comparison

isdb tx --mux-config test.toml --radio hackrf --channel 27 --gain txvga=20 --dry-run      # RF settings only
isdb rx --radio pluto --channel BS7 --system isdbs3 --seconds 30 --out 'bs7-{tlv_id}.tlv' # advanced BS → TLV
isdb tlv inspect bs7-b070.tlv                                 # services, events, assets
isdb-remux --mkv -i bs7-b070.tlv -o show.mkv                  # clear TLV → Matroska with captions
```

Transmitting to a radio needs `--rf-enable` and a conducted bench; read
the notice below first.

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
>   settings and exits before opening the device. The terrestrial centre
>   frequency is limited to the UHF TV band, 470–710 MHz (channels 13–52),
>   unless `--freq-unrestricted` is given. See
>   [docs/transmitting.md](docs/transmitting.md).
> - **No descrambling.** There is no B-CAS / ACAS / conditional-access
>   support and nothing here ever descrambles. Scrambled input is
>   recognised and refused (or, in the receivers, counted and passed on
>   untouched). A relayed program keeps its source copy-control signalling
>   and its CA_descriptor/ECM by default.
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
>   ADALM-Pluto, PlutoSky, PIX-DT295 and BDZ, are used only to identify the
>   standards implemented and the equipment tested; they may be trademarks
>   of their respective owners (HackRF is a trademark of Great Scott
>   Gadgets, ADALM-Pluto of Analog Devices).

## What it does

| | Transmit (generate) | Receive (decode) |
|---|---|---|
| **ISDB-T** — terrestrial, ARIB STD-B31 | ✅ services → TS → OFDM IQ → HackRF / Pluto; all modes, guard intervals, three hierarchical layers, partial reception | ✅ IQ file or radio → TS per layer, with TMCC/AC and status |
| **ISDB-S** — BS / 110° CS, ITU-R BO.1408 | ✅ up to 8 TSs → TC8PSK/QPSK/BPSK IQ; to a radio through the PlutoSky R2's FPGA | ✅ IF recording or radio → one TS per TS_ID |
| **ISDB-S3** — advanced BS 4K/8K, ARIB STD-B44 | — | ✅ IF recording or radio → one TLV stream per stream id; MMT-SI decoded; clear media remuxed to TS / MKV |

Around the physical layers:

- **Transport streams and SI** — PSI and ARIB SI as lossless typed codecs
  (`encode(decode(x)) == x` for every table and descriptor), ARIB 8-unit
  text, a validator whose every finding cites its clause, a field-level
  diff and TS editing.
- **A service multiplexer** — a TOML description of the network, services
  and events plus MPEG-2 video / AAC files becomes a constant-rate multiplex
  paced to the transmission frame; it can also relay one program of an
  unscrambled recording, carry a BML data carousel and a Hybridcast AIT, or
  build an ISDB-S multiplex in process.
- **TLV / MMT** for advanced BS — TLV packets, compressed IP, MMTP and the
  MMT-SI tables (services, events, packages, assets), with each clear
  asset's elementary stream extracted.
- **Playable files from advanced BS** — `isdb-remux` turns a clear TLV
  stream into a timed MPEG-TS or a Matroska file with every audio track,
  the captions (SubRip / WebVTT, keeping the broadcast's layout and colours
  where WebVTT can) and the data broadcasting packages; the Matroska file
  has a chapter per programme of the MH-EIT, at the stream's real switch.
- **SDR I/O** — HackRF One and the ADALM-Pluto family (USB or Ethernet),
  raw IQ capture with SigMF metadata, a satellite IF survey that finds the
  symbol rate, carrier and system, and an FPGA symbol front end for the
  PlutoSky R2 (Amaranth HDL, bit-exact against its CPU model).

## Quick start

Linux, Rust ≥ 1.88. The default build links the system `libhackrf` and
`libiio` for the radio backends, even if you never attach a radio:

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

No radio libraries? `cargo build --release -p isdb-cli
--no-default-features` builds everything that works on files (add
`--features hackrf` or `--features pluto` for one backend).

**Try it without any hardware or data** — modulate test packets, add noise
and a frequency offset, demodulate, and compare every packet:

```sh
isdb loopback --synthetic                  # ends with "[loopback] PASS"
isdb loopback --synthetic --frames 8 --snr-db 22 --cfo-carriers 2.3
```

**Make a broadcast and receive it back.** A small service description with
no media files:

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

isdb mux -c test.toml --seconds 5 --out-ts test.ts   # the transport stream
isdb ts inspect test.ts                    # PIDs, PCR, programs, decoded SI
isdb ts validate test.ts                   # 0 errors (one warning: the declared stream has no packets)

isdb tx --mux-config test.toml --iq test.cf32 --seconds 3   # modulate (≈ 195 MB of cf32)
isdb rx --iq test.cf32 --out rx.ts --json  # demodulate
isdb ts diff test.ts rx.ts                 # every section identical
```

`isdb mux --print-config-example` prints a fully annotated configuration
with video and audio. From here, [docs/getting-started.md](docs/getting-started.md)
is the longer walk-through.

## Commands

One binary, `isdb`, plus `isdb-remux`:

| Command | What it does |
|---|---|
| `isdb mux` | service description (TOML) + media → transport stream |
| `isdb tx` | TS → ISDB-T or ISDB-S → IQ file or radio |
| `isdb rx` | IQ file or radio → ISDB-T / ISDB-S TS, or ISDB-S3 TLV streams |
| `isdb loopback` | TX → noise, offset, echo → RX, checking every packet |
| `isdb ts` | `inspect`, `validate`, `diff`, `edit`, `extract` a transport stream |
| `isdb tlv` | `inspect`, `validate`, `extract` a TLV stream (advanced BS) |
| `isdb capture` | raw IQ capture from a radio, receive only, with SigMF metadata |
| `isdb iq` | satellite recordings: `survey`, `symbols`, `tmcc`, `s3sync`, `s3tmcc` |
| `isdb symbols` | satellite symbols from the radio's FPGA front end or the CPU model |
| `isdb completions` | shell completion scripts |
| `isdb-remux` | clear advanced-BS TLV stream → timed MPEG-TS or Matroska, with captions |

Every command has `--help`. Binary output never lands on an interactive
terminal by accident: give `-o FILE`, pipe it, or ask for it with `-o -`.

## Documentation

| | |
|---|---|
| [Getting started](docs/getting-started.md) | a longer first walk-through |
| [Transmitting](docs/transmitting.md) | bench set-up, radios, relaying a recording, ISDB-S |
| [Receiving](docs/receiving.md) | ISDB-T, ISDB-S and ISDB-S3 from files and radios |
| [Analysis](docs/analysis.md) | inspect, validate, diff, edit |
| [Multiplex configuration](docs/mux-config.md) | every field of the TOML description |
| [Architecture](docs/architecture.md) | how the crates fit together, the data flow |
| [Glossary](docs/glossary.md) | the acronyms of broadcasting |
| [Test data](docs/test-data.md) | opt-in tests on your own captures |
| [FPGA](fpga/README.md) | the PlutoSky R2 symbol blocks |
| [VERIFICATION.md](VERIFICATION.md) | what has been checked, how, and the open questions |
| [SPECS.md](SPECS.md) | the specifications, editions and where to get them |

## Crates

The workspace is a set of library crates you can use on their own; each
links its README.

| Layer | Crates |
|---|---|
| Transport stream and SI | [`isdb-ts`](crates/isdb-ts/README.md) (H.222.0 TS, PES, PSI) · [`isdb-si`](crates/isdb-si/README.md) (ARIB SI, data carousel, AIT) · [`isdb-text`](crates/isdb-text/README.md) (ARIB 8-unit text) · [`isdb-analyze`](crates/isdb-analyze/README.md) (inspect, validate, diff, edit) |
| Multiplexing | [`isdbt-mux`](crates/isdbt-mux/README.md) (services → TS, relay, data broadcasting, ISDB-S schedules) |
| Channel coding | [`isdb-fec`](crates/isdb-fec/README.md) (RS, convolutional / Viterbi, PRBS shared by the systems) |
| ISDB-T | [`isdbt-core`](crates/isdbt-core/README.md) · [`isdbt-fec`](crates/isdbt-fec/README.md) · [`isdbt-ofdm`](crates/isdbt-ofdm/README.md) · [`isdbt-phy`](crates/isdbt-phy/README.md) |
| ISDB-S | [`isdbs-core`](crates/isdbs-core/README.md) · [`isdbs-phy`](crates/isdbs-phy/README.md) |
| ISDB-S3 and MMT | [`isdbs3-core`](crates/isdbs3-core/README.md) · [`isdbs3-phy`](crates/isdbs3-phy/README.md) · [`isdb-tlv`](crates/isdb-tlv/README.md) (TLV, IP, MMTP) · [`isdb-mmt`](crates/isdb-mmt/README.md) (MMT-SI, MPU/MFU, elementary streams) |
| Media files | [`isdb-remux`](crates/isdb-remux/README.md) (timed TS / MKV) · [`isdb-mkv`](crates/isdb-mkv/README.md) (Matroska writer) · [`isdb-caption`](crates/isdb-caption/README.md) (ARIB TTML → SRT / WebVTT) |
| Satellite front end and IQ | [`isdb-frontend`](crates/isdb-frontend/README.md) (IQ ⇄ symbols, the FPGA's model) · [`isdb-iq`](crates/isdb-iq/README.md) (recordings, SigMF, survey) |
| Radios | [`isdb-radio`](crates/isdb-radio/README.md) (device-neutral) · [`isdb-hackrf`](crates/isdb-hackrf/README.md) · [`isdb-iio`](crates/isdb-iio/README.md) (Pluto) |
| Command line | [`isdb-cli`](crates/isdb-cli/README.md) (the `isdb` binary) |

## How it is verified

The specifications are the only ground truth: no code or tables are taken
from other ISDB/ARIB implementations, which serve only as independent
oracles. Every layer is checked without trusting the implementation:

1. **Spec vectors** — numbers printed in the standards (check values,
   tables, PRBS states, frame lengths) asserted in tests named after them.
2. **Lossless round trips** — decoders keep every bit; `encode(decode(x))
   == x` is property-tested and runs over your own captures if you have
   them.
3. **Inverse pairs** — each transmit stage against its receive inverse, and
   the whole chain through noise, carrier offset, timing offset and echo,
   byte-exact.
4. **A differential oracle** — the ISDB-T TX and RX cross-decode with
   GNU Radio's gr-isdbt in both directions; every difference found is a
   gr-isdbt deviation from the standard.
5. **Real receivers and broadcasts** — over coax, a MyGica S270 tuner
   decodes our ISDB-T with zero packet errors and a Sony BDZ recorder
   plays the service; a PIX-DT295 locks to our ISDB-S transmission and
   records it clean; the receivers decode the terrestrial, BS, 110° CS and
   advanced-BS broadcasts of a cable feed.

Where a standard is ambiguous, the reading taken is recorded with the
quoted text in the crates' `SPEC-NOTES.md` ([index](SPEC-NOTES.md)).
Results and how to reproduce them: [VERIFICATION.md](VERIFICATION.md).

## Status and limits

- **Tested configurations.** Real ISDB-T receivers have seen one layout
  (Mode 3, GI 1/8, partial reception, 1-segment QPSK 2/3 + 12-segment
  64QAM 3/4); others are verified by the oracle and loopback.
- **Loopback-only so far.** DQPSK, AC content, the emergency-alarm flag
  and parameter switching (no station sends them).
- **Operational values.** ARIB TR-B14 is not available, so values such as
  PID plans and SI placement are documented choices
  ([list](crates/isdbt-mux/SPEC-NOTES.md#operational-choices-not-dictated-by-the-local-specs--test-on-receivers)).
- **Media.** The multiplexer takes MPEG-2 video and AAC ADTS; AAC is
  checked at the header level. H.264 one-seg can be declared, not
  packetised.
- **Data broadcasting** displays on a real receiver only under a
  registered broadcaster's network and service IDs; no Hybridcast receiver
  has been tested.
- **Speed.** Live reception needs a fast machine: ISDB-S3's LDPC uses many
  cores, and the ISDB-T demodulator runs at about 1.4× real time on a
  current 16-core desktop. Always use `--release` builds for real-time
  work.
- **Radios.** HackRF One and the ADALM-Pluto family. A Pluto streams one
  direction at a time; ISDB-S and ISDB-S3 rates need the PlutoSky R2's FPGA
  blocks for continuous streaming over Ethernet.
- **Not implemented.** ISDB-S3 transmission, SFN synchronisation, AC data
  in the broadcasting TS, DRCS glyphs, ISDB-T<sub>SB</sub>.

## Development

[mise](https://mise.jdx.dev/) runs every check, the same tasks as CI
([mise.toml](mise.toml)):

```sh
mise run check      # everything CI runs: lint, test, doc, deny, msrv
mise run test       # unit, integration and doc tests; no radio or data needed
mise run lint       # rustfmt + clippy -D warnings
```

Heavy tests are `#[ignore]`d (`mise run test-ignored`); tests on real
captures are opt-in through `ISDB_RS_*` variables
([docs/test-data.md](docs/test-data.md)). [CONTRIBUTING.md](CONTRIBUTING.md)
has the rules — specifications as ground truth, SPEC-NOTES entries, RF
safety for hardware tests — and the commit format. Security issues:
[SECURITY.md](SECURITY.md). Changes: [CHANGELOG.md](CHANGELOG.md).

## Specifications

The implementation follows ITU-T H.222.0 | ISO/IEC 13818-1, ARIB STD-B10,
B24, B31, B32, B44 and B60, ITU-R BO.1408-1 and MIC Ordinance No. 87 of
2011. The PDFs are not redistributed here (they are copyrighted); ITU-T
recommendations and the English ARIB translations are free downloads.
[SPECS.md](SPECS.md) lists the editions, their SHA-256 hashes and URLs,
which clauses each crate implements, and how each is confirmed.

## License

Licensed under the Apache License, Version 2.0 ([LICENSE](LICENSE); see
also [NOTICE](NOTICE)). Unless you explicitly state otherwise, any
contribution intentionally submitted for inclusion in this work shall be
licensed as above, without any additional terms or conditions.
