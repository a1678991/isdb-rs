# Transmitting

`isdb tx` turns transport streams into an ISDB-T signal and sends it to an
IQ file or a radio (`--radio`; the backends are HackRF One, `hackrf`,
and the ADALM-Pluto family, `pluto`, see [below](#adalm-pluto-and-plutosky-r2)). This page covers the conducted bench set-up, the
levels and channel plan, the `isdb tx` inputs and options, relaying a
recorded program, ISDB-S (`--system isdbs`, to IQ files, and to a Pluto
whose FPGA has the transmit block; [below](#isdb-s-bs-and-110-cs)),
and what real receivers turned out to need.

## Safety and legal requirements

> [!WARNING]
> Radiating on UHF television frequencies requires a radio station licence
> (in Japan under the Radio Act, 電波法; elsewhere under local law).
> Unlicensed transmission is illegal and interferes with television
> reception around you. Use `isdb tx --radio` only in a **conducted**
> set-up (coax and attenuators into the receiver, nothing radiating) or in
> a **shielded enclosure**. Never connect an antenna to the radio while
> transmitting. You are responsible for the rights to any content you
> transmit or relay; `isdb` does not descramble and refuses scrambled
> input.

`isdb tx --radio` radiates only with `--rf-enable`, whatever the backend;
without `--rf-enable` or `--dry-run` the command line is rejected. With
`--dry-run` it prints the backend's settings (after validating the device
arguments, gains and tuning range) and exits with "not transmitting"
before the device is opened, so you can check every setting first. With HackRF every exit path stops streaming and switches
the RF amplifier off; with a Pluto every exit path stops streaming, sets
the maximum TX attenuation and powers the TX LO down. A normal end prints
`device stopped streaming: yes`.

## Bench set-up

```text
HackRF One ──SMA coax── fixed attenuators ── DC block ── F/IEC adapter ── receiver antenna input
 (TX port)               (e.g. 30–60 dB)                                   (TV, recorder, USB tuner)
```

- **Attenuation.** A TV tuner is specified for roughly −75 dBm to −20 dBm
  at its input (ARIB STD-B21 §5.2.5: minimum input level −75 dBm or lower,
  maximum −20 dBm or higher). The HackRF can deliver far more than the
  maximum, so start with the RF amplifier off (`--gain amp=off`, the
  default), a low `--gain txvga=N` and plenty
  of attenuation, then raise the TX gain until the receiver's
  signal-quality indicator is comfortably good. Aim for the middle of the
  range rather than the top: overload produces errors that look like a
  weak signal.
- **DC block.** Many TV receivers and recorders can supply DC power on the
  antenna input (for mast amplifiers). Switch that off in the receiver's
  settings and put a DC block in the line to protect the HackRF. `isdb`
  itself keeps the HackRF's own antenna-port power off.
- **Shielding.** Use double-shielded coax and terminated, tight
  connectors; disconnect the receiver's normal antenna. Leakage works both
  ways: strong local broadcasts can leak into the bench and the bench can
  leak out.
- **Receiving with a second HackRF** (for `isdb rx`, [receiving.md](receiving.md)):
  a splitter after the attenuators feeds both receivers. Keep the level at
  the HackRF's RX port low — HackRF documents −5 dBm as the maximum RX
  input before damage, and its receiver works best far below that.
- **Clock accuracy.** The HackRF's reference oscillator may be several ppm
  off. ISDB-T receivers track ±20 ppm of clock error (B21 §5.2.4), but a
  large offset slows acquisition. `--freq-corr-ppm P` divides both the
  tuning frequency and the sample clock by (1 + P·10⁻⁶) so the signal is at
  the nominal values; measure P with `isdb rx` (the CFO figure) or a
  reference receiver.

### ADALM-Pluto and PlutoSky R2

The `pluto` backend drives an AD936x radio running ADI's Pluto firmware
(ADALM-Pluto, PlutoSky R2 and other compatibles) through libiio, over USB
or Ethernet: `--radio pluto` finds the board by mDNS (the one host on the
link answering for `_iio._tcp.local` or `pluto.local`);
`--radio pluto:uri=ip:ADDRESS` (or `usb:…`), or the `ISDB_PLUTO_URI`
environment variable, gives it instead and sends no mDNS query.
Device arguments, gains and ranges are listed in the
[`isdb-radio` README](../crates/isdb-radio/README.md#backends).

- **Ethernet.** Gigabit Ethernet carries 512/63 MHz (32.5 MB/s) in one
  direction. Give the Pluto a static address on your LAN from its shell
  (over SSH: `<PLUTO_IP>` on USB, or its current
  Ethernet address): `fw_setenv ipaddr_eth <PLUTO_IP>;
  fw_setenv netmask_eth 255.255.255.0`, then `reboot`. The firmware writes
  `/etc/network/interfaces` from these variables at every boot (default
  `<DEFAULT_ETHERNET_IP>/<PREFIX_LENGTH>`, which may collide with another device on a home
  network); it has no gateway setting, so the host must be on the same
  subnet. Check with `iio_info -u ip:<PLUTO_IP>`.
- **Level.** The TX attenuation defaults to its maximum (`att=89.75`).
  At `att=0` and full scale the AD9361 delivers several dBm (more on
  PlutoSky variants with the 10 dB PA), so start high (`--gain att=60`)
  with the attenuators of the bench above and lower it step by step. The
  DAC has 12 bits: `--backoff-db 10` puts the RMS at 647 of 2047. On the
  bench chain above, a TV received a PlutoSky R2's TX2 at `att=60` and
  `att=80` but not at `att=85` (2026-10-21; [VERIFICATION.md](../VERIFICATION.md#real-receivers)),
  so `att=60` already leaves about 20 dB of margin there.
- **Clock.** The AD936x derives the LO and the sample clock from one
  40 MHz reference, so one `--freq-corr-ppm` corrects both (the
  firmware's own `xo_correction` only spans ±0.5 ppm). The integer-Hz
  clock chain runs 512/63 MHz at 8 126 983.94 Hz (−0.02 ppm), which the
  open line reports.
- **PlutoSky R2 reference.** Its 40 MHz VCTCXO is tuned only by the
  charge pump of an ADF4001 PLL locked to the 10 MHz input. The vendor
  firmware (ADI-based, in flash) and Tezuka before v0.3.22 keep that pump
  active with no 10 MHz connected, so it drags the oscillator around:
  the received signal then has about 1 rad of common phase jitter between
  OFDM symbols at UHF (and a wandering offset, +11 ppm on average on the
  unit tested), and `isdb rx` never frame-locks. Boot
  [Tezuka](https://github.com/F5OEO/tezuka_fw) v0.3.22 or later from the
  SD card (it three-states the pump without a reference and locks when one
  is connected), or connect a 10 MHz reference. With Tezuka v0.3.24 and no
  reference the unit tested was 0.6 ppm off and decoded broadcast
  channels. Two boots in three failed the AD9361 data-interface
  calibration ("Tuning TX FAILED!" in `dmesg`, no `cf-ad9361-lpc`
  device, so `--radio pluto` fails with "RX1 streaming"). Rebinding the
  drivers repeats the calibration without a reboot (on the Pluto, as
  root):

  ```sh
  D=/sys/bus/platform/drivers
  echo 79024000.cf-ad9361-dds-core-lpc > $D/cf_axi_dds/unbind
  echo spi0.0 > /sys/bus/spi/drivers/ad9361/unbind
  echo spi0.0 > /sys/bus/spi/drivers/ad9361/bind   # re-probes the TX core too
  echo 79020000.cf-ad9361-lpc > $D/cf_axi_adc/bind
  /etc/init.d/S23udc restart                       # iiod picks up the RX device
  ```

  Repeat if `dmesg` shows the failure again.
- **Self-test without radiating.** `--radio pluto:uri=…,loopback=on`
  powers the TX LO down and routes the TX samples digitally into the
  device's own RX path (the AD936x data-port loopback); `isdb rx --radio
  pluto:uri=…` then receives them. `--rf-enable` is still required. Over
  Ethernet the RX side drops about a third of its samples while TX also
  streams (with ADI's firmware iiod runs on one enabled CPU and cannot
  serve 65 MB/s; not yet retested on Tezuka, which enables both), so
  the receiver does not lock; TX and RX each keep real time on their own.
  ISDB-S through the FPGA blocks loads the link far less (14.4 MB/s of
  codes out, 43 MB/s of symbols back): the self-test of
  [the ISDB-S section](#isdb-s-bs-and-110-cs).
- **Ports.** Never transmit into an open TX port or an antenna; connect
  the attenuators (or a dummy load) first. `channel=2` selects TX2/RX2
  (firmware mode `2r2t`).

## Channel plan

Japanese UHF television channels are 6 MHz wide. The ISDB-T signal is
centred 1/7 MHz above the channel centre:

  f_c(N) = 473 + 6 · (N − 13) + 1/7 MHz, for channel N = 13 … 62

| Channel | Band (MHz) | f_c (MHz) | NIT frequency code (1/7 MHz units) |
|---|---|---|---|
| 13 | 470–476 | 473.142857 | 3312 |
| 27 | 554–560 | 557.142857 | 3900 |
| 44 | 656–662 | 659.142857 | 4614 |
| 52 | 704–710 | 707.142857 | 4950 |

The NIT carries the frequency in 1/7 MHz units (ARIB STD-B10 §6.2.31),
i.e. `7·(473 + 6·(N − 13)) + 1`. Today Japanese terrestrial television
uses channels 13–52 (470–710 MHz); `--channel` maps 13–62 and
`--freq HZ` sets a centre frequency instead (the formula is not in the
ARIB specs used here; it is the Japanese channel plan). A centre outside
470–710 MHz (channels 53–62, or any other `--freq`) is refused unless
`--freq-unrestricted` is given, which leaves only the radio backend's own
range (HackRF: 1 MHz–6 GHz; Pluto: TX 47 MHz–6 GHz, RX 70 MHz–6 GHz) for
shielded lab setups.

**Choosing a channel.** Use a channel that no station receivable at your
location uses, so that leakage cannot interfere in either direction, and
set the multiplexer's `[network] channels` to the same channel: receivers
retune to the NIT frequency after a scan, so a mismatch registers the
service on a channel with no signal.

## The signal

Each OFDM frame is 204 symbols. In Mode 3 with GI 1/8 a frame lasts
231.336 ms and has 1 880 064 samples at 512/63 MHz ≈ 8.127 MS/s, the rate
used for files and for radios. The defaults are the Japanese
broadcast parameters:

| Option | Default | Meaning |
|---|---|---|
| `--mode` | 3 | 1, 2 or 3 (2k, 4k, 8k FFT) |
| `--gi` | 8 | guard interval denominator: 4, 8, 16, 32 |
| `--partial` | true | partial reception (layer A = the centre segment, one-seg) |
| `--layer-a` | `1,qpsk,2/3,2` | 64 TSP/frame, 0.42 Mbit/s |
| `--layer-b` | `12,64qam,3/4,2` | 2592 TSP/frame, 16.85 Mbit/s |
| `--layer-c` | `none` | |

A layer is `SEGMENTS,MODULATION,CODE_RATE,TI` (modulation
`dqpsk|qpsk|16qam|64qam`, code rate `1/2…7/8`, TI = TMCC time-interleave
index 0–3). The start-up line prints the TSPs and the TS rate of every
layer; the rates follow from ARIB STD-B31 Table 3-3.

The level in an integer-sample radio such as the HackRF (8-bit DAC) or a
Pluto (12-bit DAC) and in cs8 files is set by `--backoff-db` (default
10 dB): the OFDM RMS sits that far below full scale (RMS 40 of 127; 647
of 2047 for a Pluto), which
leaves headroom for OFDM's peaks. Clipped components are counted and
reported; a few in 10⁵ are normal at 10 dB.

## `isdb tx` inputs

Exactly one input:

| Input | Use |
|---|---|
| `--mux-config FILE.toml` | the multiplexer runs in-process ([mux-config.md](mux-config.md)); never ends by itself (unless every relay has `at_eof = "end"`); a `[network] kind = "bs"` file is refused (satellite SI: send it with `--system isdbs --mux-config`, [ISDB-S](#isdb-s-bs-and-110-cs)); one TS only (repeatable with `--system isdbs`) |
| `--relay FILE` | rebroadcast a recorded program as a terrestrial service, no TOML needed ([below](#relaying-a-recording)) |
| `--a-ts/--b-ts/--c-ts PATH` | per-layer transport streams (`-` = stdin, FIFOs fine), e.g. from `isdb mux --out-a/--out-b`; a layer without input carries null packets |
| `--mux-ts PATH --a-pids … [--c-pids …]` | one TS split into layers by PID: the listed PIDs go to A / C, every other PID to B, input null packets are dropped |
| `--synthetic` | deterministic test packets (PID 0x0100/0x0200/0x0300); never ends |
| `--iq-in PATH [--iq-in-format cf32\|cs8]` | replay a baseband file instead of modulating TS (e.g. one written by `isdb tx --iq`); its parameters come from its TMCC ([below](#playback-keys)) |

`--feed` sets how file and pipe inputs are consumed: `file` (default; read
ahead, end at EOF), `loop` (rewind at EOF; continuity counters and time
stamps jump back, except in a TS paced by its PCR in `tx --system isdbs`,
which is joined seamlessly), or
`live` (never block the modulator: what has arrived is sent, the rest of
the frame is null packets, the null-fill ratio is reported; ends when the
writer closes; in `tx --system isdbs` a TS with PCRs is paced by them and
waits for its packets up to a deadline, [ISDB-S](#isdb-s-bs-and-110-cs)).

With `--mux-ts` and `--feed file|loop`, the input is **paced by its own
PCR** (H.222.0 §2.4.2.2): each OFDM frame carries the packets due before
its end, padded with nulls, so a recording goes on air at its recorded
rate. A regular file is probed first (up to 30 s of PCR) and refused, with
the layer layouts that would fit, if its mean rate exceeds a layer's
capacity; `--allow-overrate` sends it anyway (it then falls behind its own
schedule). A station multiplex split by PID keeps that station's SI — for
a program of *another* network (e.g. a satellite recording), use
`--relay`, which gives it terrestrial SI.

## `isdb tx` options

| Option | Default | Meaning |
|---|---|---|
| `--iq PATH` | — | write baseband to a file (`-` = stdout) instead of a radio |
| `--iq-format cf32\|cs8` | cf32 | float32 I/Q (GNU Radio) or signed 8-bit I/Q (`hackrf_transfer`, scaled with `--backoff-db`) |
| `--radio NAME[:key=value,...]` | — | transmit with a radio backend and its device arguments (`hackrf`, `hackrf:serial=0000abcd`, `pluto:uri=ip:<PLUTO_IP>`); needs `--channel` or `--freq`, and `--rf-enable` or `--dry-run`. An unknown name lists the compiled-in backends and their cargo features |
| `--channel T` / `--freq HZ` | — | terrestrial channel T13–T52 (a bare number N means TN), or a centre frequency in Hz, within 470–710 MHz. Satellite channels (BSn, NDn) are refused |
| `--freq-unrestricted` | off | allow a centre outside the 470–710 MHz TV band (shielded lab setups only); the backend's own range still applies (HackRF 1 MHz–6 GHz, Pluto 47 MHz–6 GHz) |
| `--rf-enable` | off | **actually radiate** (licensed or conducted/shielded use only) |
| `--dry-run` | off | print the settings and exit without opening the device |
| `--gain NAME=VALUE` | backend's | radio gain, repeatable; names, ranges and defaults are the backend's (an unknown name lists them). HackRF: `txvga=0..47` dB TX IF gain (0), `amp=on\|off` RF amplifier ≈ +11 dB (off). Pluto: `att=0..89.75` dB attenuation in 0.25 dB steps (89.75) |
| `--backoff-db DB` | 10 | OFDM RMS below integer full scale (radios with integer samples, e.g. HackRF cs8, Pluto cs12, and cs8 files) |
| `--freq-corr-ppm P` | 0 | reference-clock correction (positive = the device clock runs fast) |
| `--bb-filter-hz HZ` | 6 000 000 | analog (baseband filter) bandwidth request; the backend picks a supported bandwidth (HackRF: the largest MAX2837 setting ≤ the request; Pluto: the AD936x RF bandwidth, 0.2–40 MHz). 6 MHz passes the 5.57 MHz ISDB-T band |
| `--seconds S` | 0 | bound on air time including lead-in and flush; `0` = unbounded (until the input ends, or Ctrl-C / SIGTERM) |
| `--lead-frames N` | 2 | null frames before the data |
| `--no-flush` | off | omit the null frames that flush the interleavers at the end |
| `--realtime` | off | pace production to real time even into a file (on by default for `--feed live` with `--iq`) |
| `--queue-frames N` | 4 | frames produced ahead of the sink |
| `--prefill-frames N` | 2 | frames converted before the radio starts streaming |
| `--status-secs S` | 5 | status line interval (0 = final summary only) |
| `--log-level L`, `--log-format F` | `info`, `text` | global options, before the subcommand: the diagnostics on stderr (every `[tx] …` line here is one) are `tracing` events; `--log-level trace|debug|info|warn|error|off` or `RUST_LOG=<directive>` filters them, `--log-format json` prints one JSON object per line with the event's fields (`frames`, `null_fill_pct`, …) for log collectors and scripts |
| `--allow-overrate` | off | send a `--mux-ts` file whose PCR rate exceeds a layer's capacity (ISDB-S: a PCR-paced `--ts` file above its slots' capacity) |
| `--seek-step S` | 10 | seconds per ←/→ key ([playback keys](#playback-keys)) |
| `--no-keys` | off | do not read playback keys from the terminal |

`--radio` names a backend of `isdb-radio`; the compiled-in backends with
their device arguments and gains are listed in the
[isdb-radio README](../crates/isdb-radio/README.md#backends), and adding
one (e.g. a PlutoSDR) needs no change to the CLI
([isdb-cli README](../crates/isdb-cli/README.md#radio-backends)).

An input that never ends (`--synthetic`, `--mux-config`, `--relay`,
`--feed loop`) written to a regular `--iq` file is refused unless
`--seconds` is given (the file would fill the disk); stdout, FIFOs and
devices such as `/dev/null` need no bound. Ctrl-C or SIGTERM stops
immediately; a normal end drains the queue first.

**On-air sequence.** The transmitter first modulates and discards a few
frames (pre-roll) so the first emitted frame is ordinary OFDM, sends
`--lead-frames` null frames (a receiver that locks on the first or second
frame still outputs every input packet), the data, then null frames that
push the last packets through the interleavers and the receiver's
look-ahead. Details: [architecture.md](architecture.md#frame-and-latency-contract).

## Checking before transmitting

Run the whole chain without a radio first:

```sh
isdb mux -c bench.toml --seconds 20 --out-ts check.ts --report   # optional: check the multiplex
isdb ts validate check.ts
isdb loopback --mux-config bench.toml --frames 8 --snr-db 25     # exactly what will be modulated
isdb tx --mux-config bench.toml --iq /dev/null --seconds 10      # producer speed, no radio
```

The `tx` status line reports the **producer speed** — on-air time of the
frames produced divided by the time spent making them:

```text
[tx]     3.5 s on air, 15 frames (12 data) | null fill A  94.4% B  99.7% (total A  94.4% B  99.7%) | mux late PES 0 errors 0 | producer 4.2× real time (input 0.2 + modulate 54.4 ms/frame)
```

Below 1× the radio underruns (USB buffers sent as zeros, reported in the
status lines) and `isdb tx` prints a warning with the per-stage times. An
unoptimised build runs at ≈ 0.2×; use `cargo build --release`.

Then check the RF settings, still without radiating:

```sh
isdb tx --mux-config bench.toml --radio hackrf --channel 27 --gain txvga=20 --dry-run
```

```text
hackrf TX settings (T27):
  device       HackRF (first found)
  tuning       centre 557.142857 MHz, programmed 557142857 Hz; sample rate 512000000/63 Hz = 8.126984 MHz
  bb filter    6.00 MHz requested
  TX VGA       20 dB
  RF amp       off
  backoff      10.0 dB below cs8 full scale (RMS 40.2 of 127)
Error: dry run, not transmitting: use --rf-enable instead of --dry-run to radiate with the settings above (coax/attenuator or shielded setup only)
```

## Transmitting (conducted bench only)

```sh
# the multiplexer in-process, until Ctrl-C
isdb tx --mux-config bench.toml --radio hackrf --channel 27 --gain txvga=20 --rf-enable

# per-layer files, looped, at most 10 minutes
isdb tx --radio hackrf --channel 27 --gain txvga=20 --rf-enable --seconds 600 \
    --b-ts service.ts --a-ts oneseg.ts --feed loop

# a live multiplex from a FIFO, split by PID, padded with nulls when it lags
isdb tx --radio hackrf --channel 27 --gain txvga=20 --rf-enable \
    --mux-ts /tmp/live.fifo --feed live --a-pids 0x0000,0x0010,0x0011,0x0012,0x0014,0x1fc8,0x1fc9
```

The status lines show air time, frames, null fill per layer, input
counters (for `--mux-config`: late PES and errors; for `--mux-ts`: the
PCR-measured input rate per layer and the lag behind the PCR schedule),
the producer speed and the radio's own counters (HackRF: USB buffers and
underruns; Pluto: IIO buffers and underruns).

Typical end-to-end check with a second HackRF:

```sh
isdb rx --radio hackrf:serial=<RX-SERIAL> --channel 27 --seconds 30 --out ours.ts --json
isdb ts validate ours.ts
isdb ts diff ours.ts check.ts
```

## Relaying a recording

`--relay FILE` (on `tx`, `loopback` and `mux`) rebroadcasts one program of
a recorded transport stream — for example a recording of another network
— as a terrestrial service. The recording must be unscrambled
(transport_scrambling_control '00'; scrambled input is refused), and you
must have the rights to relay it. A foreign TS relayed as is keeps that
network's NIT/SDT, which a terrestrial receiver will not register. The
relay instead:

- takes only the program's elementary streams; their PES bytes go out
  unchanged, at the recording's own PCR pace, with PTS/DTS shifted onto
  this multiplex's clock;
- generates terrestrial PSI/SI around them: service 0x0400, PMT PID
  0x01F0, PCR PID 0x01FF, network / TS id 0x79EE (a placeholder: use an ID
  not used where your receiver is), a NIT naming the transmit channel,
  SDT, EIT (event names from the recording), TOT, BIT;
- **carries the recording's conditional-access signalling** by default
  (`--ca keep`; TOML `ca = "keep"`): the PMT's conditional_access_descriptors
  (program and ES loops), the ECM PIDs they name, and the CAT's
  CA_descriptors with the EMM PIDs present in the recording, every CA_PID
  following the PID mapping. The payload stays unscrambled, so receivers
  see CA signalling with a clear payload. `--ca drop` (`ca = "drop"`)
  relays none of it;
- selects the program automatically when the recording has one
  (`--program N` or `--service-name S` otherwise; a missing or ambiguous
  choice lists the programs with their names and streams);
- classifies every stream from the PMT and renumbers it by role: video →
  0x0111, main audio → 0x0112 (more audio 0x0113, …), captions → 0x0130,
  superimpose → 0x0138, data → 0x0140;
- **keeps the recording's copy restriction** by default: the generated
  digital copy control descriptor takes the source program's recording
  control value — from its PMT program loop, else its relayed ES loops,
  else its SDT, else its EIT p/f (copy-free only if the source signals
  none) — and the relayed streams keep their per-component (ES loop)
  descriptors. `--copy` (TOML `recording_control`) sets any value
  explicitly; it is used as given. The source's value is printed as an
  info line either way, e.g. `source copy control: copy-one-generation
  (PMT); relayed as: copy-free (explicit)`, and raw descriptors or byte
  patches that set the copy control of a relayed service are reported
  with what is actually sent. Relaying content you have no rights to is
  your responsibility.

Check it without a radio, then transmit:

```sh
isdb mux --relay FILE --auto-layout --seconds 60 --out-ts check.ts   # probe report + TS
isdb ts validate check.ts
isdb loopback --relay FILE --auto-layout --frames 8 --snr-db 25
isdb tx --relay FILE --auto-layout --radio hackrf --channel 27 --gain txvga=20 --dry-run
isdb tx --relay FILE --auto-layout --radio hackrf --channel 27 --gain txvga=20 --rf-enable
```

At start the probe report lists the source program, each source PID → role
(and the rule that decided it) → output PID with its rate, the relayed
ECM / EMM PIDs, the dropped PIDs with the reason, the source's copy
control (every 0xC1 / 0xDE found in its PMT, SDT and EIT p/f), and the
rate the whole recording needs against the layer capacity. The status line shows, per relay, the lag behind the PCR
schedule, late PES, peak rates and the loop period; PES that leave after
their decoding time (receivers drop them, audio first) also print a
`mux warning`, at most once a minute per relay. The recording loops
until Ctrl-C (`--at-eof end` stops after one pass; `--seconds S` bounds
the air time).

| Option (`--relay` only) | Default | Meaning |
|---|---|---|
| `--program N` / `--service-name S` | the only program | source program_number, or SDT service name (exact, else a unique substring; full-width ASCII folded) |
| `--channel N` | `tx`: the transmit channel; `mux`/`loopback`: 44 | terrestrial channel (TN) named in the NIT |
| `--remote-key K` | 9 | remote_control_key_id 1–12 |
| `--area-code C` | 0x5A5 | area_code (12 bits, B10 Annex D Table D-2); set the receiver's area |
| `--name S` | the recording's SDT service name | network / TS / service / provider name |
| `--copy copy-free\|copy-one-generation\|copy-never` | the source program's value | recording control of the digital copy control descriptor; an explicit value is used as given |
| `--ca keep\|drop` | keep | the recording's CA descriptors, ECM and CAT/EMM: relayed (payload unscrambled) or dropped |
| `--at-eof loop\|end` | loop | at the end of the recording |
| `--eit-from-source [BOOL]` | true | EIT present/following names, texts, genres from the recording |
| `--clock-start JST` | system clock | fixed stream clock, for reproducible output |
| `--print-config` | — | print the generated TOML and exit (save it, edit it, run it with `--mux-config` / `-c`) |

**Capacity.** HD recordings often exceed the default layer B
(12 segments 64QAM 3/4 = 16.85 Mbit/s). The rate checked is what the whole
recording needs, not the mean of its first seconds: the file is read once
at start, and the need is the smallest constant rate that delivers every
relayed packet by the decoding time of its PES (`whole file: …` in the
probe report). The relay keeps the recording's pace, so on a layer below
that rate it falls further behind for as long as the recording stays
above it, and its PES — audio first, whose lead before decoding is the
shortest — arrive too late to be played. Such a relay is refused with a list
of layouts that fit; `--auto-layout` takes the first one that changes only
the relay's layer (the most robust code rate that carries that rate
plus PCR and SI, the configured GI first; partial reception and the one-seg
layer kept) and prints the choice. For example, a recording of about
18.5 Mbit/s is refused on 3/4 and carried on 12 segments 64QAM 7/8
(19.66 Mbit/s). A single 13-segment 64QAM 7/8 layer (`--partial false
--layer-a 13,64qam,7/8,2 --layer-b none`, 21.30 Mbit/s) leaves more room
but has no one-seg layer.

To keep and tweak the generated configuration:

```sh
isdb mux --relay FILE --auto-layout --print-config > relay.toml
isdb tx --mux-config relay.toml --layer-b 12,64qam,7/8,2 --radio hackrf --channel 27 --gain txvga=20 --rf-enable
```

`crates/isdbt-mux/examples/relay-ts.toml` is the same configuration with
comments; the `ts-program` keys are described in [mux-config.md](mux-config.md).

## Playback keys

With `--relay` (or a `--mux-config` with `ts-program` relays) and with
`--iq-in`, `isdb tx` reads keys from the terminal (when stdin is one;
`--no-keys` turns this off). Ctrl-C stops as before.

| Key | `--relay` | `--iq-in` |
|---|---|---|
| space | pause / resume | pause / resume |
| ← / → | seek back / forward `--seek-step` seconds (default 10) | the same, by whole frames |
| ↑ / ↓ | next faster / slower play rate: 0.25, 0.5, 1, 1.5, 2, 4, 8, 16× | — (the rate of an IQ file is fixed) |

Each key prints the new state (`[tx] playing 01:22.8 / 30:02.0 at 2×`),
and the status lines show it. The producer runs a few frames ahead of the
radio, and the receiver plays what is already in its buffers, so a key
takes effect after about a second.

**Relay.** The relay keeps sending PSI/SI and PCR in every state, so a
receiver stays tuned:

- *Pause*: the PES being sent complete, then the relayed streams send
  nothing; the receiver shows the last picture and is silent. On resume
  everything that follows is shown later by the pause. While paused the
  stream's PTS stop, so a pause longer than 0.7 s leaves a PTS gap that
  H.222.0 §2.7.4 does not allow (`isdb ts validate` reports it); the
  MyGica S270 capture of a paused relay decoded without errors.
- *Seek*: reading continues from the file position (a 0.5 s index made by
  the start-up scan), at the next access point (sequence header), as for a
  loop. The receiver shows the last picture for about a second.
- *Play rate*: above 1×, the relay skips video pictures (whole PES, only
  ones no kept picture refers to, at most 85 % of the source's bytes
  divided by the rate, at least a frame period apart) and audio frames
  (one per output frame period), so it never needs more than the source's
  own rate on air. Below 1×, pictures are shown longer and audio frames
  repeated. After a rate change the picture holds for up to the source's
  longest arrival-to-decoding lead (about 0.5 s) while the receiver's
  buffers refill.
- Under playback control AAC audio is re-packetised one ADTS frame per PES
  (the source's PES bytes otherwise go out unchanged), and the relay sends
  the packet with the earliest decoding time first.

On the bench (MyGica S270, the recording of the relay section, 110 s of
pause, 1.5×, 2×, 0.5×, 1× and three seeks over RF) every PES arrived before
its decoding time (video ≥ 0.23 s, audio ≥ 0.046 s ahead), PER 0.

**IQ file.** `--iq-in` replays a cf32 or cs8 file at 512/63 MHz (as `isdb
tx --iq` or `hackrf_transfer -r` write it). At start the receiver finds
the transmission parameters (TMCC) and the start of the OFDM frames in
its first seconds; the PHY options are not used, and the file is scaled to
unit RMS. It plays to its end (`--feed loop` repeats it); no lead-in or
flush is added. Pause sends null frames modulated with the file's
parameters, so the receiver stays locked. Pause and seek splice frames,
which breaks time interleaving: the receiver loses about one frame's
packets at each splice (≈ 2800 on a 12-segment layer).

## ISDB-S (BS and 110° CS)

`isdb tx --system isdbs` transmits ISDB-S (ITU-R BO.1408-1, the ARIB
STD-B20 system; MIC Ordinance No. 87 of 2011 annexed tables 45–51): TS
packets per TS_ID → TMCC and slot assignment, RS(204,188), energy
dispersal, the depth-8 inter-frame interleaver, the inner code and
mapping (BPSK, punctured QPSK, TC8PSK), header and bursts → superframes
of 8 frames × 39 936 symbols at 28.86 Mbaud (11.07 ms) → root-raised-cosine
pulse shaping (roll-off 0.35, BO.1408-1 Annex 1 §9) at `--rate`, 40 Msps
by default. The libraries are `isdbs_core::TmccInfo::assign`,
`isdbs_phy::tx::Transmitter` and `isdb_frontend::shape::Shaper`.

```sh
# the BS layout (TC8PSK 2/3 × 48 slots), two TSs of 24 slots, to a file
isdb tx --system isdbs --ts 0x4010:24=a.ts --ts 0x4011:24=b.ts --iq bs.cf32
# … read back: rx finds ISDB-S from the sidecar's rate and the survey
isdb rx --iq bs.cf32 --out 'bs-{ts_id}.ts' --strip-nulls
# the CS layout (QPSK 3/4 × 48, 36 sent slots), one synthetic TS, 1 s
isdb tx --system isdbs --combination qpsk3/4:48 --seconds 1 --iq cs.cs8 --iq-format cs8
# a mixed layout: TC8PSK 2/3 × 44 and BPSK 1/2 × 4 (45 sent slots)
isdb tx --system isdbs --combination tc8psk2/3:44 --combination bpsk1/2:4 \
    --ts 0x4010:30=a.ts --ts 0x4011:15=b.ts --iq mix.cf32
```

**Layout.** `--combination WORD:N` (repeatable, at most four) gives each
modulation-code combination of BO.1408-1 Table 1 and its slots per frame,
dummy slots included; the slots sum to 48 in whole units of each
combination (`bpsk1/2` 4 slots with 1 sent, `qpsk1/2` 2/1, `qpsk2/3` 3/2,
`qpsk3/4` 4/3, `qpsk5/6` 6/5, `qpsk7/8` 8/7, `tc8psk2/3` 1/1). `--ts
ID[:SLOTS][=PATH]` (repeatable, at most eight) gives each TS its TS_ID,
its sent slots per frame (they must sum to the layout's sent slots; a
single `--ts` gets them all) and its packets: a TS file (`-` = stdin,
FIFOs, `--feed file|loop|live`) or synthetic packets. The sent slots go to
the TSs in slot order, relative TS 0 first (BO.1408-1 Fig. 9); unused TS_ID
entries are FFFFh and the other TMCC fields take the values observed on
the BS transponders (`isdbs-core` README). A file TS that has no packet
when its slot comes is padded with a null packet (counted in the status
lines). A receiver selects the TS by the TMCC's TS_ID and then reads the
TS's PAT: a file TS whose first PAT names another transport_stream_id
than its `--ts` TS_ID is warned about once (on air the two are equal).

**Placement by PCR (`--ts-pacing auto|pcr|arrival`).** A TS with N sent
slots per frame goes out at a constant rate, N × 1.086875 Mbit/s (52.17
Mbit/s for 48 slots), one packet per slot. A TS file usually runs slower
(the bench TS: 29.96 Mbit/s), so filling every slot with its packets as
they come plays it 1.74× too fast, and its PCRs no longer match the time
a receiver outputs them. By default (`auto`) a TS file that carries a PCR
is placed by it instead: each packet goes into the first free slot at or
after the time its PCRs give it (H.222.0 eq. 2-4), its input null packets
are dropped, the slots left over carry nulls, and every PCR is corrected
by the time its packet moved (at most one slot, 28.8 µs at 48 slots;
H.222.0 Annex D.0.4), so the receiver's output TS has the file's own
timing with PCRs accurate to the 27 MHz tick. Nothing else in the packets
changes.

| input | `auto` |
|---|---|
| regular file, `--feed file` or `loop` | probed (its first 30 s / 128 MB): paced when it has a PCR. A mean rate above its slots' capacity is refused with the slots it needs (`--allow-overrate` sends it anyway; it then lags its PCRs, shown as `PCR lag` in the status lines); a 1-s peak above it warns |
| FIFO or stdin, `--feed file` | paced from its first PCR (looked for in its first 65 536 packets); waits for its writer without bound |
| any input, `--feed live` | paced from its first PCR if one comes within 1 s and its first 65 536 packets, with a deadline (below); else arrival order |
| no PCR | arrival order, as before, said once at start |

`--ts-pacing pcr` refuses a file without PCR, and paces a `--feed live`
input from its first PCR whenever it comes (nulls until then);
`--ts-pacing arrival` keeps the old rule for every file: each packet,
nulls included and unmodified, in the next slot as it comes (a byte-exact
relay). With `--feed loop` the seams are joined: each pass n ≥ 1 of the
file has its PCR, PTS and DTS moved on by a constant O_n (the loop period
the pacer measured, a multiple of 300 ticks of 27 MHz) and its continuity
counters renumbered per PID (H.222.0 §2.4.3.3), so the received TS runs on
across the seam with no PCR discontinuity and no continuity error; pass 0,
and `--feed file`, change no field but the PCR. SI times (TOT/TDT, EIT)
still restart with each pass. The media of a looped file are not seamless:
the file ends with what the multiplexer had sent by then, so each pass
leaves a gap in the time stamps and skips the media still unsent (the
bench service with the 10 s media of docs/test-data.md, a TS of one media
loop: video 137.6 ms, audio 603 ms per pass); a live `isdb mux --out-ts -`
into a FIFO has no such seam (`tools/isdbs-bench-tx.sh --media` sends the
loop by default, `--media --feed fifo` the live multiplexer;
`tools/media-decode-check.py` checks the decoded media of a recording
against the source). The plan is printed before anything is opened, so
`--dry-run` shows it; the status lines add, per paced TS, `PCR lag` (and
its maximum), the input rate against the capacity, `nulls-dropped`,
`restamped`, `slot-busy` and `pcr-discontinuities`, and with a loop
`passes`, `cc-fixed` (counters renumbered at a seam) and `pes-shifted`. `isdb loopback
--system isdbs` places its files the same way.

**A FIFO fed in real time: `--feed live` or `--feed file`.** Both pace a
FIFO by its PCR; they differ in how long the transmitter waits for the
writer. With `--feed file` it blocks until the packets a superframe needs
have come: the producer runs ahead of the air by what the sink holds (about
1.2 s of codes on the `--offload fpga` path), so a writer that stalls for
longer stalls the transmission and the radio underruns. With `--feed live`
it waits only up to a deadline on the wall clock (`pace.rs`, "Live
inputs"): the first PCR goes on air 0.3 s (L) after it arrived, the null
superframes before it go out at once, and each superframe waits for its
packets until 0.2 s (G) after L past their place on the writer's schedule
(the time of the first PCR plus their PCR time). Then it goes out as it is:
packets that come later take the first free slots of a later superframe,
their PCRs restamped by the lag, so the output PCRs stay on the slot grid,
and the superframe counts as a deadline miss. The lag is kept, not
re-anchored (that would break the PCRs' continuity), and returns to zero
when the writer catches up with its backlog. A writer that stalls leaves a
hole in the TS but the transmission keeps its timing; for a real-time
writer into a radio, use `--feed live` (`tools/isdbs-bench-tx.sh` does in
its fifo mode). Paced live, the status lines add `live-input lag` (or
`lead`): when each PCR arrived against its place on the writer's
schedule, and the worst so far, and `deadline-misses`. A writer slower
than real time against the transmitter's clock shows a growing lag;
superframes start to miss once it exceeds L + G (0.5 s), and the PCR lag
grows from there. `tools/isdbs-pacing-e2e.sh` with `FEED=live` checks a
`pv`-paced FIFO end to end, `STALL=1` with a 1 s stall of the writer.

**`--placed-ts TEMPLATE`** writes each TS's packets as placed in its slots,
null packets included, for every data superframe (`{ts_id}` is replaced
by the TS_ID in 4 hex digits, as in `isdb rx --out`; needed with more than
one TS). They are the packets handed to the modulator, the same for
`--iq` and `--offload fpga`, so the file is the TS a receiver outputs for
that TS_ID: a recording can be compared with it byte for byte after
aligning the start (the lead superframes' nulls are not in it), and
`isdb ts validate --expect-rate 52170000:1` checks its PCRs offline.

```sh
# a 29.96 Mbit/s TS on the BS layout, paced by its PCR, and its placed TS
isdb tx --system isdbs --ts 0x4010=bench.ts --iq bs.cf32 --placed-ts 'placed-{ts_id}.ts'
isdb ts validate placed-4010.ts --expect-rate 52170000:1
```

**In-process multiplexer (`--mux-config FILE.toml`, `--mux-pcr-grid`).**
Instead of a TS file, a TS can be multiplexed in-process from an isdbt-mux
TOML ([mux-config.md](mux-config.md), "ISDB-S"), superframe by superframe:
the multiplexer fills exactly the TS's slots, null packets where nothing
is due, and makes its PCRs, PTS/DTS and SI for the time each packet goes
out, so nothing is paced or restamped. `--mux-config` is repeatable, one
TS each. The file must describe a satellite network (`[network] kind =
"bs"`, a `[network.satellite] channel`; a terrestrial one is refused) and
puts its services, streams and tables on layer `"A"`, the TS's one lane
(`examples/bs-bench.toml` has its service on layer B for the ISDB-T slot
model of `isdb mux`: change it to `"A"`). Its `[transport]
transport_stream_id` is the TS_ID: with one TS it takes every sent slot;
with more, its slots come from the `--ts ID:SLOTS` of that TS_ID (given
without `=PATH`, which also fixes its place among the TSs), and a
configuration without one is a TS after the `--ts` ones. A `--channel`
other than the NIT's `[network.satellite] channel` is warned about.
`[timing] pcr_per_frame` counts PCRs per superframe (default 1, every
11.07 ms). Like `--synthetic`, it never ends by itself (unless every
`ts-program` relay has `at_eof = "end"`), so an `--iq` file needs
`--seconds`. `--dry-run`, the input line and the status lines name it
(`TS_ID 0x4017 <- in-process multiplexer FILE (PCR grid uniform)`;
`0x4017 mux late PES 0 errors 0`).

`--mux-pcr-grid` chooses the time line: `uniform` (default) puts packet q
of a TS with N sent slots per frame at q / (8 N) of a superframe, the
constant-rate TS a receiver outputs for that TS_ID (and the grid a paced
`--ts` file is placed on), so its PCRs fit a constant rate to the 27 MHz
tick (≤ 18.5 ns) as the BS transponders' do; `slot` uses the true symbol
time of each slot (before the interleaver), whose frame headers and, for
a TS sharing the frame, the other TSs' slots put a frame-periodic
deviation on the PCRs against that rate (up to 6.6 µs at 48 slots, ±0.45
ms for 16 contiguous slots per frame) unless every PCR sits at the same
place in the frame (`isdb-cli/SPEC-NOTES.md`).

```sh
# the BS bench service, multiplexed in-process (its service on layer "A")
sed 's/^layer = "B"/layer = "A"/' crates/isdbt-mux/examples/bs-bench.toml > bs.toml
isdb tx --system isdbs --channel BS1 --mux-config bs.toml --iq bs.cs8 --iq-format cs8 \
    --seconds 6 --placed-ts 'placed-{ts_id}.ts'
isdb rx --iq bs.cs8 --out 'rx-{ts_id}.ts'
isdb ts validate rx-4017.ts --expect-rate 52170000:1 --expect-tsid 0x4017
# next to a TS file: 32 + 16 sent slots
isdb tx --system isdbs --ts 0x4010:32=a.ts --ts 0x4017:16 --mux-config bs.toml --iq two.cf32 --seconds 2
# the same TS to a file, timed for its superframes (no modulation)
isdb mux --system isdbs -c bs.toml --seconds 10 --out-ts bs-4017.ts
```

`isdb loopback --system isdbs --mux-config` checks the same through the
software channel, and `tools/isdbs-pacing-e2e.sh` with `MUX=1` (`GRID`,
`MEDIA`) end to end through `tx --iq` and `rx`.

**On air** come a pre-roll frame (the last frame of a null superframe:
a receiver's loops settle during its first symbols), `--lead-superframes`
null superframes (1), the data superframes, and a null frame with the
pulse shaper's tail (`--no-flush` omits them, and a receiver then loses
the last superframe).

**Level.** The samples are written at `--backoff-db` below full scale in
every format, cf32 included: at the default 10 dB the RMS is 0.316 in cf32
and 40 of 127 in cs8. Unlike the ISDB-T receiver, which normalises its
input, the ISDB-S receiver (and the FPGA front end it models) reads the
samples as a fraction of a 12-bit full scale, so a cf32 file at unit RMS
would clip. A single-carrier PSK signal has a low crest factor after the
pulse shaping; nothing clips at 10 dB.

**The SigMF sidecar.** `--iq NAME.cf32` also writes `NAME.cf32.sigmf-meta`
(`NAME.sigmf-meta` for `NAME.sigmf-data`) with the sample format and rate,
`core:description` (the layout), `core:recorder` and `isdb:system
"ISDB-S"`, and a capture segment whose `core:frequency` is the IF of
`--channel BSn` / `NDn` (with the channel fields `isdb capture` writes) or
`--freq`, when given. `isdb rx --iq NAME.cf32` needs it: an ISDB-S
recording's rate and format come from its metadata. stdout, FIFOs and
devices get no sidecar.

**To a radio — the limit.** ISDB-S occupies (1 + 0.35) × 28.86 = 38.96 MHz
and needs at least that many samples per second. To a Pluto that is
40 Msps × 4 bytes (cs12 in 16 bits) = 160 MB/s, beyond Gigabit Ethernet
(about 65 MB/s measured, VERIFICATION.md "8-bit captures and
streaming"); a HackRF stops at 20 Msps. `--radio` is wired through the
same `--rf-enable` / `--dry-run` gate and backends: `--channel BS1` is
tuned at its IF (1049.48 MHz; outside the UHF band, so with
`--freq-unrestricted`), the sample rate is `--rate` (ppm-corrected) and
`--bb-filter-hz` defaults to 40 MHz, the Pluto's widest TX filter. With
IQ samples a Pluto then underruns (counted in the status lines; `isdb tx`
warns at start), and the HackRF backend refuses 40 Msps. Test the chain
with files and `isdb loopback --system isdbs`
([receiving.md](receiving.md#software-loopback)).

**To a radio — the FPGA transmit block.** A PlutoSky R2 booted with the
isdb bitstream that has the transmit block (`SymbolsTx`,
[fpga/README.md](../fpga/README.md) "The transmit block") takes the
symbols instead of the samples: one 4-bit code per symbol, eight per DMA
sample, 14.43 MB/s, which the block shapes on the board with the
root-raised-cosine interpolator of `isdb_frontend::shape::fixed`
(512 phases × 24 taps, the same arithmetic bit for bit) at the DAC rate.
`--offload` chooses:

- `auto` (default): the codes when the radio reports a compatible block
  (with `--rf-enable` it is probed read-only, one register, before it is
  opened), else IQ samples as above;
- `fpga`: the codes, an error without the block;
- `cpu`: IQ samples always. `--iq` files always get the CPU's samples
  (`--offload fpga` with `--iq` is refused).

A dry run opens nothing and so cannot probe: `--offload fpga --dry-run`
prints the block's settings (the modulator, the link at 14.43 MB/s, the
buffers and `kbuffers`, the level as the block's backoff), `auto` both
paths. On air come the same pre-roll frame, lead superframes, data and
null flush frame as on the IQ path; the producer runs at about 11× real
time without the shaper (release build). Opening configures the block
(13 315 commands, about 4 s through iiod, skipped when the block's
BANK_TAG says the default bank is already in place), selects it before
any code is streamed and starts it once the stream has filled half the
board's kernel queue (8 buffers, 580 ms of codes) and the block reports
itself primed, so a clean run misses no symbol (MISSED 0 on
`isdb_txrx2_top.bin`; 8–16 zero symbols in the lead-in on the older image
without the prefetch); the status lines add the block's MISSED,
the summary its CONSUMED, MISSED, FIRST_MISS and the DAC underflow flag.
Closing, also on an error, stops the stream, puts the DAC back on the IQ
pass-through, then the attenuation at its maximum and the LO down. The
device arguments and their defaults (`buffer=262144`, `kbuffers=16`, TX1
only) are in the [`isdb-radio` README](../crates/isdb-radio/README.md).

```sh
# check the settings, nothing opened
isdb tx --system isdbs --radio pluto --channel BS1 --freq-unrestricted --gain att=60 --dry-run
isdb tx --system isdbs --radio pluto --channel BS1 --freq-unrestricted --offload fpga --dry-run
```

**The self-test, nothing radiated.** The AD9361's digital data-port
loopback (`loopback=on`: the TX LO powered down, the attenuation at its
maximum) returns the DAC words to the RX path sample-exact (VERIFICATION.md
"ISDB-S transmitter in the FPGA: board checks"), so the transmit block and
the receive front end of the same board can check each other end to end,
two processes with their own libiio contexts, both at 40 Msps:

```sh
# one shell: lead-in long enough for the receiver to acquire and configure
isdb tx --system isdbs --radio pluto:uri=ip:<PLUTO_IP>,loopback=on \
    --channel BS1 --freq-unrestricted --offload fpga --rf-enable \
    --ts 0x4010:24=a.ts --ts 0x4011:24=b.ts --lead-superframes 1500
# another, once the first prints "FPGA transmit block running"
isdb rx --system isdbs --radio pluto:uri=ip:<PLUTO_IP> --channel BS1 \
    --offload fpga --ts-id 0x4010 --ts-id 0x4011 --seconds 20 \
    --out 'o-{ts_id}.ts' --strip-nulls --json
```

Acceptance: every output TS byte-exact against its input, 0 uncorrectable
RS words, 0 host underruns and MISSED 0 in the transmitter's summary
(≤ 16 at FIRST_MISS 0 on an image without the prefetch), 0 losses in the
receiver's `stream`. `isdb-cli`'s opt-in test
`tx_symbols_loopback_on_a_pluto` runs exactly this
(`ISDB_RS_PLUTO_TX_LOOPBACK_URI`, [test-data.md](test-data.md)); the
receiver must end before the transmitter closes the loopback, so the
test's files carry null packets after the data.

## Receiver compatibility notes

These were found by feeding `isdb tx` output over a conducted link to two
consumer receivers — a **MyGica S270** USB tuner and a **Sony BDZ-series**
Blu-ray recorder — in one configuration (Mode 3, GI 1/8, partial reception,
layer A 1 segment QPSK 2/3, layer B 12 segments 64QAM 3/4, time-interleave
index 2). They are requirements of those receivers, not of the
specifications, and the defaults follow them.

- **Copy control type '01'.** The recorder refuses to play a service whose
  digital copy control descriptor (0xC1) carries copy_control_type '00' in
  its user_defined bits ("Unsupported signal: This program is not
  available") and plays it with '01' (user_defined 0b0100, what live
  broadcasters send). In a one-change-at-a-time bisection, the recording
  control value (copy-free or copy-one-generation), the placement (PMT +
  SDT or PMT + EIT), the video decode control byte and the EIT component
  type made no difference. B10 only shows this layout in an informative
  annex (Annex F); the ARIB operational guidelines that may make it
  normative are not available to the project. `isdbt-mux` defaults to
  copy_control_type '01' + APS '00', and `isdb ts validate` warns
  (`si.copy_control`) on '00'. Details:
  [isdbt-mux SPEC-NOTES](../crates/isdbt-mux/SPEC-NOTES.md#digital-copy-control-descriptor-0xc1-and-its-user_defined-bits).
- **Remote-control key collisions.** Receivers file a TS under its
  remote_control_key_id (one-touch key 1–12) and list its services as
  three-digit channels key·10 + index (key 9 → 091, 092, …). A key that a
  local station already occupies made the recorder's scan merge or ignore
  the bench service. Use a key no station in your area uses; the examples
  use 9.
- **NIT channel = transmit channel.** The recorder registers services from
  the terrestrial NIT of the received TS and tunes by its frequency, so
  `[network] channels` must name the channel you transmit on (`--relay`
  does this automatically on `tx`). A relayed foreign TS with its original
  NIT is not registered.
- **Channel choice.** Pick a channel unused at your location (see
  [Channel plan](#channel-plan)).
- **USB tuner.** The MyGica S270 decoded the Rust transmitter with zero
  packet errors, and its capture validated with 0 errors. On that tuner
  `dvbv5-scan` failed to read sections for driver reasons unrelated to the
  TS (the all-PID DVR path works), so channel discovery on it is untested.
- **Open observation.** While the SDT carried running_status 4
  ("running") the recorder listed the service as off air (休止中). The SDT
  now sends 0, as the live stations do, but the recorder still showed the
  service as off air afterwards, so the cause is not isolated.
- **Data broadcasting needs a registered broadcaster identity.** On a
  Sony BDZ-ZW1500 a `bml-carousel` under the placeholder network
  (network_id 0x79EE, key 9, T44) lit the "d" mark but the d button never
  started the BML browser (no "data fetching" message), and the same held
  for a live station's own carousel relayed packet for packet into that
  network. Re-emitting a local station's recording with its own
  identifiers (network_id, TS and service IDs, remote key, PMT and PCR
  PIDs, BIT broadcaster entry) on **its own channel** opened its live
  data broadcasting, and replacing only the carousel with this crate's
  (`examples/bml/startup.bml`) displayed our page over the picture; a
  service made entirely here (MPEG-2 video and AAC files, the two example
  pages with colour-key navigation) under the same identity worked the
  same way. The same identity on another channel (T44) showed "this
  channel is suspended" with neither picture, sound nor d mark, so the
  recorder binds a known network to the frequency in its channel table
  and matches the service against it before it allows anything. To test BML or Hybridcast on such a receiver, re-emit a
  recording of a station the receiver has registered, on that station's
  channel, in a shielded bench with the antenna disconnected; see
  [mux-config.md](mux-config.md#bml-carousel-keys) for the configuration
  (`pid_map = "keep"`, the station's IDs, a `bml-carousel` on the entry
  PID). The Hybridcast launcher loads on the same recorder and reports
  "This receiver has no Hybridcast engine"; the application start needs
  a Hybridcast TV.

Other modes, guard intervals, DQPSK, three-layer and non-partial-reception
configurations have not yet been shown to a real receiver
([VERIFICATION.md](../VERIFICATION.md#open-questions)).
