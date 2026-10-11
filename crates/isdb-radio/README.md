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
| `Quantizer<F>`, `IntFormat`, `Cs8` | unit-RMS signal → integer samples with backoff below full scale, saturation and clip count |
| `QueuedSink<R>`, `RadioTx`, `TxStream`, `StreamCounters` | the shared TX path of every radio fed with integer sample buffers: quantisation into `R::Format`, start after the prefill, back-pressure, stall detection, drain on finish, stop flag, underrun counters, "device stopped streaming" check |
| `mock` (feature `mock`) | `MemorySink` and `MockRadio` (a `RadioTx` consuming in real time; in a `QueuedSink` it behaves like a radio backend) for tests |

Errors are `anyhow::Error`: they are reported to a person, not matched on.

## Backends

| Name | Feature | Device args | Gains (TX) | Gains (RX) |
|------|---------|-------------|------------|------------|
| `hackrf` | `hackrf` (default) | `serial` | `txvga` 0..47 dB, `amp` on/off | `lna` 0..40 dB (8 dB steps), `vga` 0..62 dB (2 dB steps), `amp` on/off |

The `hackrf` backend links the system libhackrf through `isdb-hackrf`;
with `default-features = false` nothing links a system library and
`backend("hackrf")` says which feature to enable. It transmits through a
`QueuedSink` of cs8 buffers; the baseband filter is the largest MAX2837
bandwidth ≤ `RadioConfig::bandwidth_hz` (1.75 MHz below that; above
28 MHz is refused), as libhackrf's `hackrf_compute_baseband_filter_bw`
computes it, and `open_tx` / `open_rx` report the value programmed.

A backend for a device with integer samples implements `RadioTx` for it
(`type Format = Cs8`, or another `IntFormat` for 12/16-bit devices) and
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
