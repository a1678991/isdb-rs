# Receiving

`isdb rx` demodulates an ISDB-T signal from an IQ file or a radio
(`--radio`; HackRF One or an ADALM-Pluto / PlutoSky R2) and
writes the transport stream it carries. It needs no configuration: the
mode, guard interval and layer parameters are detected from the signal
(guard-interval correlation and the TMCC).

## Sources

```sh
isdb rx --iq capture.cf32 --out out.ts                  # float32 I/Q (GNU Radio "complex")
isdb rx --iq capture.cs8 --iq-format cs8 --out out.ts   # signed 8-bit I/Q (hackrf_transfer)
some-producer | isdb rx --iq - --out out.ts              # stdin
isdb rx --radio hackrf --channel 27 --seconds 30 --out out.ts # live, from a HackRF
isdb rx --radio pluto:uri=ip:192.0.2.30 --channel 27 --out out.ts # a Pluto over Ethernet
```

IQ files must be complex baseband centred on the ISDB-T signal at
512/63 MHz ≈ 8.126984 MS/s. `isdb tx --iq` writes exactly that; so does
`hackrf_transfer -r FILE -f <f_c in Hz> -s 8126984` (the integer rate is
0.02 ppm off, well within what the receiver tracks). The centre frequency
of channel N is 473 + 6·(N − 13) + 1/7 MHz ([transmitting.md](transmitting.md#channel-plan)).

Receiving is not transmitting, but the bench rules still apply when the
signal comes from your own transmitter: keep a HackRF's RX input well
below its −5 dBm damage limit (attenuators, splitter; an AD936x's limit is
in the same range), and see
[transmitting.md](transmitting.md#bench-set-up). For off-air captures, see
[test-data.md](test-data.md#capturing-your-own-data).

| Option | Default | Meaning |
|---|---|---|
| `--iq PATH` | — | read baseband from a file (`-` = stdin) |
| `--iq-format cf32\|cs8` | cf32 | file format |
| `--radio NAME[:key=value,...]` | — | receive with a radio backend and its device arguments (`hackrf`, `hackrf:serial=0000abcd`, `pluto:uri=ip:192.168.2.1`); needs `--channel` or `--freq`. An unknown name lists the compiled-in backends |
| `--channel N` / `--freq HZ` | — | UHF channel 13–52 or centre frequency in Hz, within 470–710 MHz |
| `--freq-unrestricted` | off | allow a centre outside 470–710 MHz (channels 53–62, other bands); the backend's own range still applies |
| `--gain NAME=VALUE` | backend's | radio gain, repeatable; names, ranges and defaults are the backend's (an unknown name lists them). HackRF: `lna=0..40` dB in 8 dB steps (16), `vga=0..62` dB baseband VGA in 2 dB steps (20), `amp=on\|off` RF amplifier (off). Pluto: `agc=manual\|slow_attack\|fast_attack\|hybrid` (slow_attack), `gain=dB` manual gain (implies `agc=manual`; the range depends on the LO and is checked at open) |
| `--freq-corr-ppm P` | 0 | reference-clock correction (tuning and sample clock) |
| `--bb-filter-hz HZ` | 6 MHz | analog (baseband filter) bandwidth request; the backend picks a supported bandwidth |
| `--seconds S` | 0 | bound on signal time; `0` = the whole file, or until Ctrl-C / SIGTERM with `--radio` |
| `--status-secs S` | 1 | status line interval in seconds of signal (0 = none) |

## Outputs

Only frames the receiver marks *valid* are written (see below), so the
start of a capture — before lock and while the deinterleavers fill — is
not output.

| Option | Writes |
|---|---|
| `--out PATH` | the single TS a 13-segment receiver reproduces (ARIB STD-B31 §3.2.2): all layers in multiplex-frame slot order, with null packets in the slots of no layer |
| `--a-out/--b-out/--c-out PATH` | the packets of one layer |
| `--strip-nulls` | drop null packets (PID 0x1FFF) from every output |
| `--json` | a summary on stdout at the end |

Packets whose RS word could not be corrected are written with
transport_error_indicator = 1 (H.222.0), so downstream tools can see them.
`isdb rx` does not descramble: services that a broadcaster scrambles stay
scrambled in the output, while PSI/SI and unscrambled services are
readable. The output can be fed straight to `isdb ts` ([analysis.md](analysis.md)).

## Status lines

```text
isdb rx: test.cf32 (Cf32); until end of input
[rx]     1.0 s LOCKED    (no frame yet)
[rx]     2.1 s LOCKED    M3 GI1/8 | frames 1/4 valid | MER A 118.6 B 125.1 dB | CFO +0.000 car | RS clean/corr/uncorr A 64/0/0 B 2592/0/0
[rx]     3.0 s LOCKED    M3 GI1/8 | frames 5/5 valid | MER A 118.6 B 125.1 dB | CFO +0.000 car | RS clean/corr/uncorr A 320/0/0 B 12960/0/0
[rx] done: 3.01 s of signal, 12 frames (9 valid); Mode 3, GI 1/8, partial reception; A 1seg QPSK 2/3 TI2 (64 TSP/frame); B 12seg 64QAM 3/4 TI2 (2592 TSP/frame)
[rx]   layer A: 576 packets; RS clean 576 corrected 0 (0 bytes) uncorrectable 0; MER mean 118.6 dB
[rx]   layer B: 23328 packets; RS clean 23328 corrected 0 (0 bytes) uncorrectable 0; MER mean 125.1 dB
```

| Field | Meaning |
|---|---|
| time | seconds of *signal* processed (not wall-clock time) |
| `searching` / `acquiring` / `LOCKED` | receiver state: looking for an ISDB-T signal; mode known, synchronising (frame sync from the TMCC); frame-locked |
| `M3 GI1/8` | mode and guard interval detected from the signal |
| `frames v/n valid` | OFDM frames in this interval, and how many were valid: the TMCC was decoded and every deinterleaver of the active layers was primed. After lock, the first frames are not valid because time interleaving delays data by up to several frames |
| `MER A … B … dB` | modulation error ratio per layer, from the equalised carriers. A noise-free cf32 file gives ≈ 120 dB (float precision), a cs8 file at the default back-off ≈ 41 dB (8-bit quantisation); in the software channel's white noise the default 64QAM 3/4 layer still decodes without packet loss at 18 dB SNR, with RS corrections (`isdb loopback --snr-db` to try) |
| `CFO … car` | carrier frequency offset in carrier spacings (Mode 1: 3.968 kHz, Mode 2: 1.984 kHz, Mode 3: 0.992 kHz). On a bench, a steady CFO is mostly the radio's clock error: CFO × spacing / f_c gives the ppm for `--freq-corr-ppm` |
| `RS clean/corr/uncorr` | per layer, packets whose RS(204,188) word was error-free, corrected (up to 8 bytes), or uncorrectable |
| `TMCC parity failed n` | frames whose TMCC failed the difference-set parity check (shown only when non-zero) |
| `overruns` | with `--radio`: buffers (HackRF USB transfers, Pluto IIO buffers) dropped because the demodulator fell behind |

The final summary repeats the totals; `--json` prints them as one object:
`frames`, `valid_frames`, `tmcc_parity_failed`, `params` (mode, guard
interval, partial reception, per-layer segments, modulation, code rate,
time-interleave index, TSPs per frame), `cfo_carriers`,
`timing_drift_samples_per_symbol`, per-layer `packets`, `rs_clean`,
`rs_corrected_packets`, `rs_corrected_bytes`, `rs_uncorrectable`,
`mer_db_mean`, `mer_db_min`, and the number of packets written per output.

## Performance

The receiver runs as a pipeline of two threads (demodulation, then FEC
decoding). On a current 16-core desktop CPU it processes a Mode 3 signal at about 1.4×
real time, the demodulator being the bottleneck. Live reception with
`--radio` therefore needs a fast CPU and a release build; if it falls
behind, the status line counts `overruns`. Recording to a file
(`hackrf_transfer` or `isdb tx`-generated files) and decoding offline has
no such limit.

## Software loopback

`isdb loopback` runs `tx` and `rx` in one process through a software
channel and checks every packet; it is the quickest way to see what the
receiver does under impairments:

```sh
isdb loopback --synthetic --frames 8 --snr-db 22 --cfo-carriers 2.3 --sample-offset -700000
```

`--snr-db` adds white Gaussian noise (`--seed` for another noise
realisation), `--cfo-carriers` a frequency offset in carrier spacings, and
`--sample-offset N` starts the receiver N samples late (N < 0) or after N
samples of silence (N > 0). The first valid frame is anchored to the
transmitted frame with the same content and every later frame is compared
packet by packet per layer and as a reproduced TS. It prints PASS/FAIL
counts and lost packets per layer and exits non-zero on failure; `--json`
gives a machine-readable verdict. Inputs are the same as for `isdb tx`
(`--synthetic` is the default when none is given).
