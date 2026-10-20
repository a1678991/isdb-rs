# isdb-hackrf

Minimal safe wrapper over the system `libhackrf` (FFI) for continuous TX
and RX streaming with a HackRF One: open a device, tune it, start a stream
and exchange interleaved signed 8-bit I/Q buffers over bounded channels.
It knows nothing about ISDB-T; the `hackrf` backend of `isdb-radio`
(`isdb tx/rx --radio hackrf`) uses it to put the baseband from
`isdbt-phy` on air and to receive it.

> **Transmitting needs a licence.** This crate has no RF gate:
> `Device::start_tx` radiates whatever is queued. Radiating on broadcast
> (UHF TV) frequencies requires a licence (in Japan under the Radio Act,
> elsewhere under local law); use a conducted or shielded setup. The
> `--rf-enable` / `--dry-run` gate lives in `isdb-cli`.

## Spec scope

No broadcast specification. The surface is the libhackrf C API
(`hackrf_init`/`exit`, `open`/`open_by_serial`/`close`,
`board_id_read`, `version_string_read`, `set_freq`,
`set_sample_rate_manual`, `compute_baseband_filter_bw` +
`set_baseband_filter_bandwidth`, `set_amp_enable`, `set_txvga_gain`,
`set_lna_gain`, `set_vga_gain`, `set_antenna_enable`, `is_streaming`,
`start_tx`/`stop_tx`, `start_rx`/`stop_rx`, `error_name`,
`library_version`), declared by hand in `src/ffi.rs`.

- **Sample format:** interleaved signed 8-bit I/Q (`Vec<i8>`, I first),
  the HackRF wire format. Conversion from/to complex samples is in
  `isdbt_phy::iq` (`cs8_from_samples`, `samples_from_cs8`).
- **Sample rate:** `set_sample_rate_manual(freq_hz, divider)` sets an exact
  rational rate, e.g. 512 MHz / 63 for ISDB-T.
- **Linking:** `build.rs` emits `cargo:rustc-link-lib=dylib=hackrf`, so
  building this crate (and anything depending on it, e.g. `isdb-radio`
  and `isdb-cli` with their default `hackrf` feature)
  needs the system library: Arch `hackrf`, Debian/Ubuntu `libhackrf-dev`,
  Homebrew `hackrf` (for a library outside the linker's default search
  path, add `RUSTFLAGS="-L <dir>"`). On docs.rs (`DOCS_RS` set) nothing is
  linked. The manifest declares `links = "hackrf"`, so Cargo allows only
  one crate linking libhackrf in a dependency graph. `isdb-radio` and
  `isdb-cli` can be built without this crate (`--no-default-features`).
- **Minimum libhackrf: 2022.09.1.** From that release on `hackrf_stop_tx`
  / `hackrf_stop_rx` wait until every in-flight USB transfer has completed
  or been cancelled (`cancel_transfers` waits on `active_transfers` in the
  libhackrf source; hackrf.h does not say so), which is what makes freeing
  the callback context after a successful stop sound. `Device::open`
  refuses an older release reported by `hackrf_library_release()`
  (unparsable strings such as git builds are accepted).
- **Attribution:** the declarations in `src/ffi.rs` follow libhackrf's
  `hackrf.h` (BSD-3-Clause, © Great Scott Gadgets, Jared Boone, Benjamin
  Vernoux). Linking the system library dynamically puts no extra
  obligation on this crate; a binary distribution that bundles libhackrf
  must carry its BSD notice.

## Position in the stack

- Depends on: `thiserror` and the system `libhackrf` only.
- Used by: `isdb-radio`, as an optional dependency behind its `hackrf`
  feature (`isdb tx --radio hackrf`, `isdb rx --radio hackrf`). No other
  workspace crate depends on it directly.

## Key types and entry points

| Item | Role |
|---|---|
| `Device::open(serial: Option<&str>)` | open the first HackRF or the one with this serial (reference-counted `hackrf_init`/`hackrf_exit`) |
| `Device::set_freq`, `set_sample_rate_manual`, `set_baseband_filter` (returns the bandwidth actually set: largest supported ≤ request), `set_amp_enable`, `set_txvga_gain` (0–47 dB), `set_lna_gain` (0–40 dB, 8 dB steps), `set_vga_gain` (0–62 dB, 2 dB steps), `set_antenna_enable` | radio configuration |
| `Device::start_tx(depth) -> TxStream` | `sender: SyncSender<Vec<i8>>` (drop it to end the stream) + `counters` |
| `Device::start_rx(depth) -> RxStream` | `receiver: Receiver<Vec<i8>>` + `counters` |
| `Device::stop` | stop any stream and free its callback context |
| `Counters` | atomics: `buffers`, `underruns` (TX), `overruns` (RX), `finished` |
| `library_version()`, `library_release()`, `MIN_LIBRARY_RELEASE`, `Device::board_id`, `firmware_version`, `is_streaming` | information |
| `Error` | failing call, libhackrf code and name |

Transmit a buffer of I/Q at 512/63 MHz (needs a device, so not run in
tests):

```rust,no_run
let mut dev = isdb_hackrf::Device::open(None)?;
dev.set_sample_rate_manual(512_000_000, 63)?;
dev.set_baseband_filter(6_000_000)?;
dev.set_freq(659_142_857)?; // UHF ch 44 centre + 1/7 MHz
dev.set_txvga_gain(20)?;
let tx = dev.start_tx(16)?;
let iq: Vec<i8> = vec![0; 262_144]; // interleaved I/Q, signed 8-bit
tx.sender.send(iq).expect("stream running");
drop(tx.sender); // end of stream: the callback stops transmitting
while !tx.counters.finished.load(std::sync::atomic::Ordering::SeqCst) {
    std::thread::sleep(std::time::Duration::from_millis(10));
}
println!("underruns: {:?}", tx.counters.underruns);
// Dropping `dev` stops the stream, switches the RF amp off and closes it.
# Ok::<(), isdb_hackrf::Error>(())
```

`examples/info.rs` prints the library version, board id, firmware version
and streaming state of the attached HackRF without any RF activity:
`cargo run -p isdb-hackrf --example info`.

## Design notes

- **Bounded channels.** TX: the USB callback pulls buffers from a
  `sync_channel(depth)`; when none is queued it fills the rest of the USB
  transfer with zeros and counts an underrun. When the sender is dropped
  the callback zero-fills, sets `finished` and returns −1 so libhackrf
  stops transmitting. RX: the callback copies each transfer and
  `try_send`s it; a full queue drops the buffer and counts an overrun; a
  dropped receiver sets `finished` and stops the stream.
- **Ownership and Drop safety.** The callback context is a boxed value
  passed to libhackrf and freed only after a successful
  `hackrf_stop_tx/rx`. When the stop (or a `hackrf_start_*`) fails,
  transfers may still be in flight, so the context — and with it the
  stream's channel end — is kept by the `Device` and freed only after
  `hackrf_close`, which joins libhackrf's transfer thread. Dropping a
  `Device` stops any stream, disables the RF amplifier, closes the device
  and then frees such kept contexts, so an early return or an unwinding
  panic never leaves the radio transmitting. `Drop` does not run when the
  process aborts (`panic = "abort"`, or a panic inside the `extern "C"`
  callbacks, which aborts since Rust 1.81); the device is then released by
  the OS. `Device` is `Send` but not `Sync`.
- **unsafe.** This and `isdb-iio` are the only crates that allow
  `unsafe_code`; here all of it is FFI calls and the callback-context
  pointers. It uses the workspace lint
  table (`[lints] workspace = true`) and lifts `unsafe_code` with
  `#![allow(unsafe_code)]` in `src/lib.rs`.

## Verification

There are no automated tests: everything requires hardware. The crate is
exercised on a conducted bench through `isdb tx` / `isdb rx` (see
[isdb-cli](https://github.com/a1678991/isdb-rs/blob/main/crates/isdb-cli/README.md) and
[docs/transmitting.md](https://github.com/a1678991/isdb-rs/blob/main/docs/transmitting.md)); the HackRF sink of
`isdb-radio` is tested against an in-memory radio. `cargo test -p isdb-hackrf --doc`
compiles (but does not run) the example above, which needs `libhackrf` at
link time.

## Known limitations

- Only the calls listed above are wrapped (no sweep mode, clock-source or
  CLKOUT control, firmware/CPLD operations, transceiver-mode queries).
- RX copies each USB transfer into a new `Vec<i8>` on the USB thread (no
  buffer pool).
- `depth` counts queued `Vec<i8>` buffers, not samples: on TX they are the
  caller's buffers (any length), on RX one per USB transfer, so queue
  latency depends on buffer sizes.
- No SPEC-NOTES (no specification involved); see the workspace index
  [SPEC-NOTES.md](https://github.com/a1678991/isdb-rs/blob/main/SPEC-NOTES.md) and [README](https://github.com/a1678991/isdb-rs/blob/main/README.md).
