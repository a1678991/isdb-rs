# isdb-iio

Minimal safe wrapper over the system `libiio` (FFI, 0.x API) for attribute
access, continuous TX and RX streaming and one-shot RX captures of 16-bit I/Q with an AD936x
radio such as the ADALM-Pluto or a PlutoSky R2: connect to an IIO context
(USB, Ethernet or local), read and write device and channel attributes,
start a stream and exchange interleaved `i16` buffers over bounded
channels. It knows nothing about ISDB-T or the AD936x; the `pluto` backend
of `isdb-radio` (`isdb tx/rx --radio pluto`) uses it to put the baseband
from `isdbt-phy` on air and to receive it.

> **Transmitting needs a licence.** This crate has no RF gate:
> `Context::start_tx` sends whatever is queued to the DAC. Radiating on
> broadcast (UHF TV) frequencies requires a licence (in Japan under the
> Radio Act, elsewhere under local law); use a conducted or shielded
> setup. The `--rf-enable` / `--dry-run` gate lives in `isdb-cli`.

## Spec scope

No broadcast specification. The surface is the libiio 0.x C API
(`create_context_from_uri`, `context_destroy`, `context_get_description`,
`context_get_attr_value`, `context_set_timeout`, `context_find_device`,
`device_find_channel`, `device_attr_read`/`write`,
`device_debug_attr_read`/`write`, `device_set_kernel_buffers_count`,
`device_get_sample_size`, `channel_attr_read`/`write` (text, `longlong`,
`double`), `channel_enable`/`disable`, `device_create_buffer`,
`buffer_push`/`refill`/`cancel`/`start`/`end`/`destroy`, `strerror`,
`library_get_version`), declared by hand in `src/ffi.rs`.

- **Sample format:** interleaved `i16`, one component per enabled channel
  per sample in channel order (I then Q for an AD936x port); only devices
  whose enabled channels are all stored in 16 bits are accepted. Buffers
  are little-endian on the wire and converted to native order. The
  AD936x-specific scaling (12-bit samples, left-aligned on TX) is the
  backend's.
- **libiio version:** the 0.x API, 0.21 or later (tested: libiio 0.26 on
  the host, iiod 0.25 in Pluto firmware v0.38, network backend). libiio
  1.x replaced these calls and is not supported.
- **Linking:** `build.rs` emits `cargo:rustc-link-lib=dylib=iio`, so
  building this crate (and anything depending on it, e.g. `isdb-radio` and
  `isdb-cli` with their default `pluto` feature) needs the system library:
  Arch `libiio`, Debian/Ubuntu `libiio-dev`, Fedora `libiio-devel`,
  Homebrew `libiio` (for a library outside the linker's default search
  path, add `RUSTFLAGS="-L <dir>"`). On docs.rs (`DOCS_RS` set) nothing is
  linked. The manifest declares `links = "iio"`. `isdb-radio` and
  `isdb-cli` can be built without this crate (`--no-default-features`).
- **Attribution:** the declarations in `src/ffi.rs` follow libiio's
  `iio.h` (LGPL-2.1-or-later, © Analog Devices, Inc.). Linking the system
  library dynamically puts no extra obligation on this crate; a binary
  distribution that bundles libiio must meet the LGPL's terms for it.

## Position in the stack

- Depends on: `thiserror` and the system `libiio` only.
- Used by: `isdb-radio`, as an optional dependency behind its `pluto`
  feature (`isdb tx --radio pluto`, `isdb rx --radio pluto`). No other
  workspace crate depends on it directly.

## Key types and entry points

| Item | Role |
|---|---|
| `Context::open(uri)` | connect (`ip:192.168.2.1`, `ip:pluto.local`, `usb:1.4.5`, `local:`); `uri`, `description`, `attr` (context attributes such as `hw_model`, `fw_version`), `set_timeout` |
| `Context::device(name) -> Device` | `read`/`write` and `read_debug`/`write_debug` of device attributes, `channel(name, output) -> Channel` |
| `Channel` | `read`/`write` (text), `read_i64`/`write_i64`, `read_f64`/`write_f64` |
| `Context::start_tx(device, channels, samples, kernel_buffers, depth) -> TxStream` | `sender: SyncSender<Vec<i16>>` (drop it to end the stream) + `counters` |
| `Context::start_rx(device, channels, samples, kernel_buffers, depth) -> RxStream` | `receiver: Receiver<Vec<i16>>` + `counters` |
| `Context::capture(device, channels, samples, kernel_buffers) -> Vec<i16>` | one buffer, refilled once: `samples` contiguous samples (one DMA transfer), without a stream thread |
| `Context::stop`, `is_streaming` | stop every stream (returns the first error a stream thread ended with) |
| `Counters` | atomics: `buffers`, `underruns` (TX), `overruns` (RX), `finished` |
| `library_version()` | information |
| `Error` | failing call, what it was called on, errno and `iio_strerror` text |

Receive one buffer from an AD936x (needs a device, so not run in tests):

```rust,no_run
let mut ctx = isdb_iio::Context::open("ip:192.168.2.1")?;
let phy = ctx.device("ad9361-phy")?;
phy.channel("voltage0", false)?.write_i64("sampling_frequency", 8_126_984)?;
phy.channel("altvoltage0", true)?.write_i64("frequency", 473_142_857)?; // RX LO
let rx = ctx.start_rx("cf-ad9361-lpc", &["voltage0", "voltage1"], 262_144, 4, 16)?;
let iq: Vec<i16> = rx.receiver.recv().expect("stream running"); // I, Q, I, Q, …
println!("{} samples, overruns {:?}", iq.len() / 2, rx.counters.overruns);
ctx.stop()?;
# Ok::<(), isdb_iio::Error>(())
```

`examples/info.rs` prints the library version, the context description
and attributes, the ENSM mode and the TX clock chain of an AD936x without
any RF activity: `cargo run -p isdb-iio --example info -- ip:192.168.2.1`.

## Design notes

- **A thread per stream.** `start_tx` / `start_rx` enable the channels,
  set the kernel buffer count, create a non-cyclic IIO buffer and hand it
  to a thread. TX: the thread waits for the first queued buffer (the
  device does not start on zeros), then fills each IIO buffer from the
  queue and pushes it; when the queue is empty it pads with zeros and
  counts an underrun. `iio_buffer_push` blocks while the kernel queue is
  full, which paces the thread (and, through the bounded queue, the
  producer) at the sample rate. When the sender is dropped the thread pads
  the last buffer and pushes `kernel_buffers` zero buffers, so the queued
  samples leave the kernel queue before `finished` is set. RX: the thread
  refills and `try_send`s a copy of each buffer; a full queue drops it and
  counts an overrun.
- **Stopping.** `stop` sets the stream's stop flag and waits up to 3 s for
  the thread, which notices it after the push or refill in progress (one
  buffer of signal); only then does it cancel the buffer
  (`iio_buffer_cancel`, documented thread-safe), because destroying a
  cancelled network buffer makes libiio print read errors to stderr.
  Then the thread is joined, the buffer destroyed and the channels
  disabled. Dropping a `Context` stops its streams before destroying it.
- **Threads.** `Context` is `Send` but not `Sync` (libiio does not document
  its contexts as thread-safe); `Device` and `Channel` borrow it. A stream
  thread uses only its own buffer while the owner may keep changing
  attributes: libiio's backends serve buffers apart from attribute
  requests, the same concurrency SoapyPlutoSDR and pyadi-iio rely on.
- **One-shot capture.** `capture` creates a buffer of the requested size,
  refills it once and destroys it, so the samples are one DMA transfer and
  contiguous whatever the link speed. Its size is bounded by the memory
  the device can allocate for one buffer: on a PlutoSky R2 with Tezuka
  v0.3.24, 16 515 072 samples (63 MiB of 2 × 16-bit) worked and
  16 531 456 failed with `ENOMEM` in iiod, with 1 and with 4 kernel
  blocks alike (measured 2026-10-20). `isdb-radio`'s `pluto` backend uses
  it for `isdb capture`.
- **unsafe.** Like `isdb-hackrf`, this FFI crate lifts the workspace's
  `unsafe_code` denial with `#![allow(unsafe_code)]` in `src/lib.rs`; all
  of it is FFI calls and the buffer slice between `iio_buffer_start` and
  `iio_buffer_end`.

## Verification

Unit tests cover the error mapping, the version string and the refusal of
an unknown URI scheme (no device needed). The streaming path was checked
on a PlutoSky R2 (AD9361; ADI-based firmware v0.38 and Tezuka v0.3.24,
iiod 0.25 and 0.26) over Gigabit Ethernet at 512/63 MHz through the
`isdb-radio` `pluto` backend: TX samples arrive
bit-exact at the AD9361 (its digital TX→RX loopback, compared with the
modulator's IQ file), TX keeps real time without underruns, and RX streams
without gaps and is demodulated by `isdb rx`, including a broadcast
received through a cable feed (VERIFICATION.md "Broadcast reception").
`capture` took single buffers of up to 16 515 072 samples at 57.72 and
61.44 Msps whose symbol-rate line stays continuous across the whole
buffer (VERIFICATION.md "Satellite IF survey"). `cargo test -p isdb-iio
--doc` compiles (but does not run) the example above, which needs libiio
at link time.

## Known limitations

- Only the calls listed above are wrapped (no scanning for contexts, no
  triggers, no cyclic buffers, no per-channel demultiplexing or format
  conversion, no events).
- RX copies each buffer into a new `Vec<i16>` (no buffer pool).
- `depth` counts queued `Vec<i16>` buffers, not samples: on TX they are
  the caller's buffers (any length), on RX one per IIO buffer.
- Device-side underflows and overflows (DMA starved or full because the
  host or the link is too slow) are not visible to these counters; a
  stream that cannot keep up shows as pacing below real time (TX) or
  missing samples (RX).
- No SPEC-NOTES (no specification involved); see the workspace index
  [SPEC-NOTES.md](https://github.com/a1678991/isdb-rs/blob/main/SPEC-NOTES.md) and [README](https://github.com/a1678991/isdb-rs/blob/main/README.md).
