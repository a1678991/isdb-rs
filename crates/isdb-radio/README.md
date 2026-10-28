# isdb-radio

Device-neutral SDR layer between the ISDB-T baseband of `isdbt-phy`
(complex `f32` samples at 512/63 MHz) and a radio. Callers stream through
two traits and pick a device by name; adding a device adds a backend here,
not options or code paths to its callers. A backend can also take a
capture of raw samples at any rate the device supports
(`Backend::capture`, behind `isdb capture`), in one contiguous piece or
streamed with its losses reported: the way to record ISDB-S and ISDB-S3
satellite IF, whose rates (≥ 39 / ≥ 35 Msps of 8-bit samples, 78 / 70
MB/s) are above what a Pluto streams over Gigabit Ethernet (about
63–66 MB/s).

> **Transmitting needs a licence.** This crate has no RF gate: an opened
> TX sink radiates whatever it is given. Radiating on broadcast (UHF TV)
> frequencies requires a licence (in Japan under the Radio Act, elsewhere
> under local law); use a conducted or shielded setup. The `--rf-enable` /
> `--dry-run` gate lives in `isdb-cli`.

## Contents

| Item | Role |
|------|------|
| `IqSink` / `IqSource` | where a transmitter writes frames of samples / where a receiver reads chunks |
| `DeviceSpec` | device selection `NAME[:key=value[,key=value...]]` (`FromStr`, `Display`) |
| `RadioConfig` | programmed frequency, rational sample rate, analog bandwidth request, device arguments, named gains |
| `TxOptions`, `RxOptions` | TX: digital backoff (0..40 dB), prefill frames and frame duration (`validate()`); RX: receive queue length in seconds of signal, which the backend converts to its buffer count |
| `Backend` | one kind of radio: `describe_tx` / `describe_rx` / `describe_capture` validate without hardware (dry run), `open_tx` / `open_rx` open the device; TX gain defaults are the device's minimum output power. Optional: `capture_limit` (the largest capture and why, `u64::MAX` for a stream; `None` by default) and `capture` (writes the sample bytes to an `io::Write` as they arrive); `offloads` (the receive stages the device runs itself, probed without changing it; none by default) and `capture_symbols` (streams the `Symbols` words of the device's front end) |
| `Offload`, `SymbolsCapture` | a stage a radio runs itself (`Offload::Symbols { version, taps }`: the `isdb-frontend` stage in the FPGA); what `capture_symbols` took (words, DMA overflow reports, settings read back) |
| `Capture`, `CaptureFormat`, `CaptureGap` | what one capture wrote: its format (`Cs8`, or `Cs16` with a full scale of 2048 or 32768), the samples written and the losses inside it, and the rate, LO, bandwidth and gain read back from the device, its identity, the capture method and the host time |
| `backends()`, `backend(name)` | the compiled-in backends; an unknown name reports the `isdb-radio` feature of each known backend (forwarded by `isdb-cli` under the same name) |
| `Quantizer<F>`, `IntFormat`, `Cs8`, `Cs12` | unit-RMS signal → integer samples (cs8; cs12 left-aligned in `i16`) with backoff below full scale, saturation and clip count |
| `QueuedSink<R>`, `RadioTx`, `TxStream`, `StreamCounters` | the shared TX path of every radio fed with integer sample buffers: quantisation into `R::Format`, start after the prefill, back-pressure, stall detection, drain on finish, stop flag, underrun counters, "device stopped streaming" check |
| `mock` (feature `mock`) | `MemorySink` and `MockRadio` (a `RadioTx` consuming in real time; in a `QueuedSink` it behaves like a radio backend) for tests |

Errors are `RadioError`, one variant per kind of failure with its facts
as fields: a device selection that does not parse (`Spec`, `KeyValue`),
an unknown backend (`UnknownBackend`), a device argument or gain the
backend refuses (`UnknownKey`, `RepeatedKey`, `Argument` with the `Rule`
it breaks, `Conflict`), an option or tuning out of range (`Backoff`,
`Duration`, `Tuning`, `CaptureLength`), a frequency or rate the device
did not take (`Readback`), an operation the backend or device cannot do
(`Unsupported`, `Firmware`, `FrontEnd`), a stream that stopped
(`Stream`, `StillStreaming`), a device reply it cannot use (`Reply`), and
a device library or I/O failure (`Device`, `Io`). `Device` names the
backend and the operation and keeps the library's error
(`isdb_hackrf::Error`, `isdb_iio::Error`) as its `source`. An `IqSink` /
`IqSource` over a file reports `Io`.

Diagnostics are `tracing` events, never printed; the crate installs no
subscriber (the `isdb` binary does). `info`: a device opened, with its
tuning, bandwidth and gains as fields (`rate_hz`, `freq_hz`,
`bandwidth_hz`, `gain`), and a TX stream started. `warn`: underruns since
the last status (`new_underruns`, `underruns`, `buffers`: zeros went on
air), RX buffers dropped, ADC overflows during a capture, a TX queue
that did not drain. `debug`: the full settings line, maia-sdr HTTP
requests and each stream's end.

## Backends

| Name | Feature | Device args | Gains (TX) | Gains (RX) |
|------|---------|-------------|------------|------------|
| `hackrf` | `hackrf` (default) | `serial` | `txvga` 0..47 dB, `amp` on/off | `lna` 0..40 dB (8 dB steps), `vga` 0..62 dB (2 dB steps), `amp` on/off |
| `pluto` | `pluto` (default) | `uri` (default `ip:pluto.local`), `channel` 1/2, `buffer` (samples), TX: `loopback` on/off; capture: `recorder` iio/maia/stream, `bits` 8/12/16 (`buffer` with `recorder=stream` only) | `att` 0..89.75 dB attenuation (0.25 dB steps, default 89.75) | `agc` manual/slow_attack/fast_attack/hybrid (default slow_attack), `gain` dB (manual) |

The `hackrf` backend links the system libhackrf through `isdb-hackrf`,
the `pluto` backend the system libiio (0.x) through `isdb-iio`; with
`default-features = false` nothing links a system library and
`backend("hackrf")` / `backend("pluto")` say which feature to enable.
The HackRF backend It transmits through a
`QueuedSink` of cs8 buffers; the baseband filter is the largest MAX2837
bandwidth ≤ `RadioConfig::bandwidth_hz` (1.75 MHz below that; above
28 MHz is refused), as libhackrf's `hackrf_compute_baseband_filter_bw`
computes it, and `open_tx` / `open_rx` report the value programmed.

The `pluto` backend drives an AD936x running ADI's Pluto firmware or a
derivative such as Tezuka (ADALM-Pluto, PlutoSky R2, …) over any libiio
URI; Gigabit Ethernet (`uri=ip:ADDRESS`) carries 512/63 MHz in one
direction (TX and RX at once exceed what iiod serves with ADI's
firmware, one CPU enabled: the receive side drops samples). It transmits through a `QueuedSink` of `Cs12` buffers (12-bit
samples left-aligned in `i16`, the DAC's bits 15..4) in IIO buffers of
`buffer` samples (default 262 144), four of them in the device kernel.
The AD936x clock chain takes integer-Hz rates, so 512/63 MHz runs at
1 040 253 944 Hz / 128 = 8 126 983.94 Hz (−0.02 ppm); the rate read back
from `tx_path_rates` / `rx_path_rates`, the LO and the RF bandwidth read
back are reported when the device is opened, and a rate more than 10 ppm
off is refused. Opening the TX sets the maximum attenuation first and
switches the DDS tone generator off; closing it (also on error or panic)
sets the maximum attenuation again and powers the TX LO down.
`loopback=on` routes the TX samples into the RX path inside the AD936x
(its digital data-port loopback) with the TX LO powered down, so nothing
is radiated and an `isdb rx --radio pluto` on the same device receives
the transmission; it is turned off when the TX closes.

Captures (`Backend::capture`, `pluto` only; `hackrf` cannot capture)
program the RX as `open_rx` does and wait 100 ms for the LO and the AGC.
`bits` chooses the sample size: 16 (default), 12 (maia-sdr only) or 8
(RX1 only):

| `recorder` | how | samples at most (16 / 12 / 8 bits) | 8-bit samples | loss |
|---|---|---|---|---|
| `iio` (default) | one libiio buffer refilled once (`isdb_iio::Context::capture`), the largest iiod allocated on a PlutoSky R2 with Tezuka v0.3.24 (63 MiB; one more 16 Ki-sample step failed with `ENOMEM`, with 1 and 4 kernel blocks alike) | 16 515 072 / – / 33 030 144 | Tezuka's packing (below) | none (one DMA transfer) |
| `maia` | Tezuka's maia-sdr recorder over its HTTP API on port 80 of the `ip:` host (`PATCH /api/recorder` with the mode `IQ16bit`/`IQ12bit`/`IQ8bit`, the duration, `Start`; `GET /recording`, a tar of SigMF files), refused when maia-sdr's DDC is enabled; it records the AD936x settings libiio programmed into 128 MiB of DDR (the device tree reserves 256 MiB, but Tezuka's `maia_iio_lite` gateware ends its DMA at 0x0E000000) | 33 554 432 / 44 739 242 / 67 108 864 (0.58 / 0.78 / 1.16 s at 57.72 Msps; 8 bits at 40 Msps: 1.68 s) | the 12-bit sample / 16 rounded down (mean −0.5 LSB) | none (one DMA transfer) |
| `stream` | libiio buffers (`isdb_iio::Context::start_rx_raw`, `buffer` samples each, default 1 Mi) for as long as wanted, the ADC core's overflow bit read after every buffer | unbounded | Tezuka's packing | reported (below) |

- **Formats.** 16 bits: through libiio 12 bits sign-extended (full scale
  2048), from maia-sdr left-aligned (32768); maia-sdr's 12-bit mode is
  downloaded sign-extended (2048). 8 bits: `ci8`, full scale 128.
  Through libiio, 8-bit samples need Tezuka's bitstream: with `voltage0`
  the only channel enabled it packs `{Q8, I8}` into one 16-bit word,
  each the 12-bit sample / 16 rounded with first-order error feedback
  and saturated to −128..127 (maia-hdl `cs12_cs8mux.v`); the backend
  refuses other firmware (`fw_version` not `tezuka…`). The two 8-bit
  paths were checked against 16-bit captures of the same signal: same
  power (−11.0 dBFS), same carrier offset (so I is the first byte), DC
  mean +0.004 LSB (libiio) and −0.465 LSB (maia-sdr).
- **Losses of a stream.** The overflow bit of the ADC core
  (`cf-ad9361-lpc` register 0x80000088, bit 2, write 1 to clear) is
  sticky and set whenever the DMA had no free block: samples were lost
  since the last check, but not where. The buffers before the check that
  found it are contiguous; the loss lies before the end of the buffers
  still queued in the device (4 kernel blocks), in iiod, the network and
  the host queue (8). The stream therefore drops the buffer that found it
  and the next 14 (`CaptureGap::discarded`), and what it writes after
  that is contiguous again. Checked against the sync counters Tezuka's
  12-bit stream mode embeds every 262 144 samples (VERIFICATION.md
  "8-bit captures and streaming over Gigabit Ethernet"): 410 losses in 90 s, each flagged
  at most 12 buffers before its data arrived. A streamed capture spans
  the samples asked for, counting the discarded ones, so a lossy link
  writes fewer.

A backend for a device with integer samples implements `RadioTx` for it
(`type Format = Cs8`, `Cs12`, or another `IntFormat`) and
returns `QueuedSink::new(device, opts)` from `open_tx`.

## Example

```rust
use isdb_radio::{DeviceSpec, RadioConfig, TxOptions};

let spec: DeviceSpec = "hackrf".parse()?;
let cfg = RadioConfig {
    center_hz: 473_142_857,
    nominal_hz: 473_142_857,
    rate_num: 512_000_000,
    rate_den: 63,
    bandwidth_hz: 6_000_000,
    args: spec.args.clone(),
    gains: vec![("txvga".into(), "20".into())],
};
let opts = TxOptions { prefill_frames: 2, frame_seconds: 0.231, backoff_db: 10.0 };
if let Ok(radio) = isdb_radio::backend(&spec.backend) {
    // No hardware needed: validates and describes the settings.
    eprintln!("{}", radio.describe_tx(&cfg, &opts)?);
    // radio.open_tx(&cfg, &opts) would open the device.
}
# Ok::<(), isdb_radio::RadioError>(())
```
