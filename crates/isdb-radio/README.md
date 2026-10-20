# isdb-radio

Device-neutral SDR layer between the ISDB-T baseband of `isdbt-phy`
(complex `f32` samples at 512/63 MHz) and a radio. Callers stream through
two traits and pick a device by name; adding a device adds a backend here,
not options or code paths to its callers.

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
| `Backend` | one kind of radio: `describe_tx` / `describe_rx` validate without hardware (dry run), `open_tx` / `open_rx` open the device; TX gain defaults are the device's minimum output power |
| `backends()`, `backend(name)` | the compiled-in backends; an unknown name reports the `isdb-radio` feature of each known backend (forwarded by `isdb-cli` under the same name) |
| `Quantizer<F>`, `IntFormat`, `Cs8`, `Cs12` | unit-RMS signal → integer samples (cs8; cs12 left-aligned in `i16`) with backoff below full scale, saturation and clip count |
| `QueuedSink<R>`, `RadioTx`, `TxStream`, `StreamCounters` | the shared TX path of every radio fed with integer sample buffers: quantisation into `R::Format`, start after the prefill, back-pressure, stall detection, drain on finish, stop flag, underrun counters, "device stopped streaming" check |
| `mock` (feature `mock`) | `MemorySink` and `MockRadio` (a `RadioTx` consuming in real time; in a `QueuedSink` it behaves like a radio backend) for tests |

Errors are `anyhow::Error`: they are reported to a person, not matched on.

## Backends

| Name | Feature | Device args | Gains (TX) | Gains (RX) |
|------|---------|-------------|------------|------------|
| `hackrf` | `hackrf` (default) | `serial` | `txvga` 0..47 dB, `amp` on/off | `lna` 0..40 dB (8 dB steps), `vga` 0..62 dB (2 dB steps), `amp` on/off |
| `pluto` | `pluto` (default) | `uri` (default `ip:pluto.local`), `channel` 1/2, `buffer` (samples), TX: `loopback` on/off | `att` 0..89.75 dB attenuation (0.25 dB steps, default 89.75) | `agc` manual/slow_attack/fast_attack/hybrid (default slow_attack), `gain` dB (manual) |

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
# Ok::<(), anyhow::Error>(())
```
