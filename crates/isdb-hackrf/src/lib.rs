//! Safe, minimal wrapper over the system `libhackrf` for continuous TX/RX.
//!
//! Samples are interleaved signed 8-bit I/Q (the HackRF wire format).
//! Streaming uses bounded channels: TX pulls buffers and transmits zeros on
//! underrun (counted); RX pushes buffers and drops on overrun (counted).
//! Dropping a [`Device`] stops any stream, switches the RF amplifier off
//! and closes the device, so an early return or an unwinding panic never
//! leaves the radio transmitting. `Drop` does not run when the process
//! aborts (`panic = "abort"`, or a panic inside the `extern "C"` USB
//! callbacks, which aborts since Rust 1.81); libhackrf and the firmware
//! then stop when the USB handle is released by the OS, not by this crate.
//!
//! # Transmitting
//!
//! This crate has no RF gate of its own: [`Device::start_tx`] radiates
//! whatever the caller queues. Transmitting on broadcast frequencies needs
//! a licence (in Japan under the Radio Act); use a conducted or shielded
//! setup. `isdb-cli` adds the `--rf-enable` / `--dry-run` gate.
//!
//! # libhackrf version
//!
//! Requires libhackrf **2022.09.1 or later**: from that release on,
//! `hackrf_stop_tx` / `hackrf_stop_rx` wait until every in-flight USB
//! transfer has completed or been cancelled (`cancel_transfers` waits on
//! `active_transfers`), so the callback context can be freed after a
//! successful stop. hackrf.h does not document this; it is read from the
//! libhackrf source. [`Device::open`] refuses an older release reported by
//! `hackrf_library_release` (a release string it cannot parse, e.g. a git
//! build, is accepted). If a stop fails, the context is kept until
//! `hackrf_close` has joined the transfer thread.
//!
//! Entry points: [`Device::open`], the radio settings on [`Device`],
//! [`Device::start_tx`] → [`TxStream`], [`Device::start_rx`] →
//! [`RxStream`], live [`Counters`].  Building needs the system libhackrf
//! (`build.rs` links `-lhackrf`).  There is no broadcast specification in
//! this crate; it is used by `isdb-cli` to radiate and receive the
//! `isdbt-phy` baseband.  See the crate README for the streaming design.
//!
//! # Example
//!
//! ```no_run
//! let mut dev = isdb_hackrf::Device::open(None)?;
//! dev.set_sample_rate_manual(512_000_000, 63)?; // ISDB-T 512/63 MHz
//! dev.set_freq(659_142_857)?;
//! let tx = dev.start_tx(16)?;
//! tx.sender.send(vec![0i8; 262_144]).expect("stream running"); // I/Q, signed 8-bit
//! drop(tx.sender); // end of stream
//! # Ok::<(), isdb_hackrf::Error>(())
//! ```

#![allow(unsafe_code)]

mod ffi;

use std::ffi::{CStr, CString, c_void};
use std::os::raw::c_int;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, TryRecvError, TrySendError, sync_channel};
use std::sync::{Arc, Mutex};

/// libhackrf error.
#[derive(Debug, Clone, thiserror::Error)]
#[error("{call}: {name} ({code})")]
pub struct Error {
    /// Function that failed.
    pub call: &'static str,
    /// libhackrf error code.
    pub code: i32,
    /// libhackrf error name.
    pub name: String,
}

/// Result alias.
pub type Result<T> = std::result::Result<T, Error>;

fn check(call: &'static str, rc: c_int) -> Result<()> {
    if rc == ffi::HACKRF_SUCCESS {
        return Ok(());
    }
    // SAFETY: hackrf_error_name returns a static NUL-terminated string.
    let name = unsafe { CStr::from_ptr(ffi::hackrf_error_name(rc)) }
        .to_string_lossy()
        .into_owned();
    Err(Error {
        call,
        code: rc,
        name,
    })
}

static LIB_USERS: Mutex<usize> = Mutex::new(0);

fn lib_acquire() -> Result<()> {
    let mut n = LIB_USERS.lock().unwrap();
    if *n == 0 {
        // SAFETY: plain library initialisation.
        check("hackrf_init", unsafe { ffi::hackrf_init() })?;
    }
    *n += 1;
    Ok(())
}

fn lib_release() {
    let mut n = LIB_USERS.lock().unwrap();
    *n -= 1;
    if *n == 0 {
        // SAFETY: all devices using the library have been closed.
        unsafe { ffi::hackrf_exit() };
    }
}

/// libhackrf version string.
pub fn library_version() -> String {
    // SAFETY: returns a static string.
    unsafe { CStr::from_ptr(ffi::hackrf_library_version()) }
        .to_string_lossy()
        .into_owned()
}

/// libhackrf release string (e.g. `2026.01.3`; `unknown` or a git
/// description for builds outside a release).
pub fn library_release() -> String {
    // SAFETY: returns a static string (may be called before hackrf_init).
    unsafe { CStr::from_ptr(ffi::hackrf_library_release()) }
        .to_string_lossy()
        .into_owned()
}

/// Oldest libhackrf release this crate supports (year, month): the first
/// whose `hackrf_stop_tx` / `hackrf_stop_rx` wait for in-flight transfers.
pub const MIN_LIBRARY_RELEASE: (u32, u32) = (2022, 9);

/// `YYYY.MM[.N]` → (year, month); anything else → `None`.
fn parse_release(s: &str) -> Option<(u32, u32)> {
    let mut it = s.trim().split('.');
    let y: u32 = it.next()?.parse().ok()?;
    let m: u32 = it.next()?.parse().ok()?;
    ((1900..10000).contains(&y) && (1..=12).contains(&m)).then_some((y, m))
}

/// Refuse a libhackrf release older than [`MIN_LIBRARY_RELEASE`].
fn check_release(release: &str) -> Result<()> {
    match parse_release(release) {
        Some(v) if v < MIN_LIBRARY_RELEASE => Err(Error {
            call: "hackrf_library_release",
            code: ffi::HACKRF_ERROR_OTHER,
            name: format!(
                "libhackrf {release} is older than {}.{:02}.1: its hackrf_stop_tx/rx do not wait for in-flight transfers",
                MIN_LIBRARY_RELEASE.0, MIN_LIBRARY_RELEASE.1
            ),
        }),
        _ => Ok(()),
    }
}

/// Streaming counters shared with the USB callback thread.
#[derive(Debug, Default)]
pub struct Counters {
    /// Buffers transferred.
    pub buffers: AtomicU64,
    /// TX: buffers sent as zeros because no data was queued.
    pub underruns: AtomicU64,
    /// RX: buffers dropped because the consumer was slow.
    pub overruns: AtomicU64,
    /// Set when the stream ended (source closed or stop requested).
    pub finished: AtomicBool,
}

struct TxCtx {
    rx: Receiver<Vec<i8>>,
    pending: Vec<i8>,
    pos: usize,
    counters: Arc<Counters>,
}

struct RxCtx {
    tx: SyncSender<Vec<i8>>,
    counters: Arc<Counters>,
}

unsafe extern "C" fn tx_cb(t: *mut ffi::hackrf_transfer) -> c_int {
    // SAFETY: libhackrf passes a valid transfer whose tx_ctx is the TxCtx we
    // registered and which outlives streaming (freed only after stop).
    let t = unsafe { &mut *t };
    let ctx = unsafe { &mut *(t.tx_ctx as *mut TxCtx) };
    let len = usize::try_from(t.buffer_length).unwrap_or(0);
    // SAFETY: `buffer` holds `buffer_length` writable bytes.
    let out = unsafe { std::slice::from_raw_parts_mut(t.buffer as *mut i8, len) };
    let mut filled = 0;
    let mut starved = false;
    while filled < out.len() {
        if ctx.pos == ctx.pending.len() {
            match ctx.rx.try_recv() {
                Ok(v) => {
                    ctx.pending = v;
                    ctx.pos = 0;
                    continue;
                }
                Err(TryRecvError::Empty) => {
                    starved = true;
                    break;
                }
                Err(TryRecvError::Disconnected) => {
                    out[filled..].fill(0);
                    t.valid_length = t.buffer_length;
                    ctx.counters.finished.store(true, Ordering::SeqCst);
                    return -1; // end of stream: stop transmitting
                }
            }
        }
        let n = (ctx.pending.len() - ctx.pos).min(out.len() - filled);
        out[filled..filled + n].copy_from_slice(&ctx.pending[ctx.pos..ctx.pos + n]);
        ctx.pos += n;
        filled += n;
    }
    if starved {
        out[filled..].fill(0);
        ctx.counters.underruns.fetch_add(1, Ordering::Relaxed);
    }
    t.valid_length = t.buffer_length;
    ctx.counters.buffers.fetch_add(1, Ordering::Relaxed);
    0
}

unsafe extern "C" fn rx_cb(t: *mut ffi::hackrf_transfer) -> c_int {
    // SAFETY: as in tx_cb, rx_ctx is our live RxCtx. The RX transfer is
    // read-only for the callback.
    let t = unsafe { &*t };
    let ctx = unsafe { &*(t.rx_ctx as *const RxCtx) };
    let len = usize::try_from(t.valid_length)
        .unwrap_or(0)
        .min(usize::try_from(t.buffer_length).unwrap_or(0));
    // SAFETY: `buffer` holds `valid_length` (≤ `buffer_length`) received bytes.
    let data = unsafe { std::slice::from_raw_parts(t.buffer as *const i8, len) };
    ctx.counters.buffers.fetch_add(1, Ordering::Relaxed);
    match ctx.tx.try_send(data.to_vec()) {
        Ok(()) => 0,
        Err(TrySendError::Full(_)) => {
            ctx.counters.overruns.fetch_add(1, Ordering::Relaxed);
            0
        }
        Err(TrySendError::Disconnected(_)) => {
            ctx.counters.finished.store(true, Ordering::SeqCst);
            -1
        }
    }
}

enum Stream {
    None,
    Tx(*mut TxCtx),
    Rx(*mut RxCtx),
}

impl Stream {
    /// Free the context.
    ///
    /// # Safety
    ///
    /// libhackrf must no longer call the callback with it: after a
    /// successful `hackrf_stop_*` (libhackrf ≥ [`MIN_LIBRARY_RELEASE`]),
    /// a `hackrf_start_*` that never started, or `hackrf_close`.
    unsafe fn free(self) {
        match self {
            Stream::None => {}
            Stream::Tx(c) => drop(unsafe { Box::from_raw(c) }),
            Stream::Rx(c) => drop(unsafe { Box::from_raw(c) }),
        }
    }
}

/// An open HackRF.
pub struct Device {
    dev: *mut ffi::hackrf_device,
    stream: Stream,
    /// Contexts of streams whose stop (or start) failed: transfers may
    /// still be in flight, so they are freed only after `hackrf_close`
    /// has joined libhackrf's transfer thread.
    retired: Vec<Stream>,
}

impl std::fmt::Debug for Device {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Device")
            .field(
                "stream",
                &match self.stream {
                    Stream::None => "none",
                    Stream::Tx(_) => "tx",
                    Stream::Rx(_) => "rx",
                },
            )
            .field("retired_contexts", &self.retired.len())
            .finish_non_exhaustive()
    }
}

// SAFETY: libhackrf device handles may be used from any single thread at a
// time; Device is not Sync.
unsafe impl Send for Device {}

/// Producer side of a TX stream.
pub struct TxStream {
    /// Queue of interleaved I/Q buffers to transmit; drop it to end the stream.
    pub sender: SyncSender<Vec<i8>>,
    /// Live counters.
    pub counters: Arc<Counters>,
}

/// Consumer side of an RX stream.
pub struct RxStream {
    /// Received interleaved I/Q buffers.
    pub receiver: Receiver<Vec<i8>>,
    /// Live counters.
    pub counters: Arc<Counters>,
}

impl std::fmt::Debug for TxStream {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TxStream")
            .field("counters", &self.counters)
            .finish_non_exhaustive()
    }
}

impl std::fmt::Debug for RxStream {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RxStream")
            .field("counters", &self.counters)
            .finish_non_exhaustive()
    }
}

impl Device {
    /// Open the first HackRF, or the one with `serial`.
    ///
    /// Fails (without opening anything) when libhackrf is older than
    /// [`MIN_LIBRARY_RELEASE`] or `serial` contains a NUL byte.
    pub fn open(serial: Option<&str>) -> Result<Self> {
        check_release(&library_release())?;
        let serial = serial
            .map(|s| {
                CString::new(s).map_err(|_| Error {
                    call: "hackrf_open_by_serial",
                    code: ffi::HACKRF_ERROR_INVALID_PARAM,
                    name: "serial number contains a NUL byte".into(),
                })
            })
            .transpose()?;
        lib_acquire()?;
        let mut dev = std::ptr::null_mut();
        let rc = match &serial {
            // SAFETY: valid out pointer.
            None => unsafe { ffi::hackrf_open(&mut dev) },
            // SAFETY: valid C string and out pointer.
            Some(s) => unsafe { ffi::hackrf_open_by_serial(s.as_ptr(), &mut dev) },
        };
        if let Err(e) = check("hackrf_open", rc) {
            lib_release();
            return Err(e);
        }
        Ok(Self {
            dev,
            stream: Stream::None,
            retired: Vec::new(),
        })
    }

    /// Board id byte.
    pub fn board_id(&mut self) -> Result<u8> {
        let mut v = 0u8;
        check("hackrf_board_id_read", unsafe {
            ffi::hackrf_board_id_read(self.dev, &mut v)
        })?;
        Ok(v)
    }

    /// Firmware version string.
    pub fn firmware_version(&mut self) -> Result<String> {
        let mut buf = [0 as std::os::raw::c_char; 255];
        check("hackrf_version_string_read", unsafe {
            ffi::hackrf_version_string_read(self.dev, buf.as_mut_ptr(), 254)
        })?;
        Ok(unsafe { CStr::from_ptr(buf.as_ptr()) }
            .to_string_lossy()
            .into_owned())
    }

    /// Centre frequency in Hz.
    pub fn set_freq(&mut self, hz: u64) -> Result<()> {
        check("hackrf_set_freq", unsafe {
            ffi::hackrf_set_freq(self.dev, hz)
        })
    }

    /// Exact rational sample rate `freq_hz / divider` (e.g. 512 MHz / 63 for ISDB-T).
    pub fn set_sample_rate_manual(&mut self, freq_hz: u32, divider: u32) -> Result<()> {
        check("hackrf_set_sample_rate_manual", unsafe {
            ffi::hackrf_set_sample_rate_manual(self.dev, freq_hz, divider)
        })
    }

    /// Baseband filter: the largest supported MAX2837 bandwidth ≤ `hz`
    /// (the smallest one if `hz` is below it), as computed by libhackrf's
    /// `hackrf_compute_baseband_filter_bw` (which rounds down despite the
    /// header's "nearest"); returns the bandwidth set.
    pub fn set_baseband_filter(&mut self, hz: u32) -> Result<u32> {
        let bw = unsafe { ffi::hackrf_compute_baseband_filter_bw(hz) };
        check("hackrf_set_baseband_filter_bandwidth", unsafe {
            ffi::hackrf_set_baseband_filter_bandwidth(self.dev, bw)
        })?;
        Ok(bw)
    }

    /// RF amplifier (TX PA / RX LNA front-end amp, ~11 dB).
    pub fn set_amp_enable(&mut self, on: bool) -> Result<()> {
        check("hackrf_set_amp_enable", unsafe {
            ffi::hackrf_set_amp_enable(self.dev, u8::from(on))
        })
    }

    /// TX IF gain 0–47 dB.
    pub fn set_txvga_gain(&mut self, db: u32) -> Result<()> {
        check("hackrf_set_txvga_gain", unsafe {
            ffi::hackrf_set_txvga_gain(self.dev, db)
        })
    }

    /// RX LNA gain 0–40 dB, 8 dB steps.
    pub fn set_lna_gain(&mut self, db: u32) -> Result<()> {
        check("hackrf_set_lna_gain", unsafe {
            ffi::hackrf_set_lna_gain(self.dev, db)
        })
    }

    /// RX baseband (VGA) gain 0–62 dB, 2 dB steps.
    pub fn set_vga_gain(&mut self, db: u32) -> Result<()> {
        check("hackrf_set_vga_gain", unsafe {
            ffi::hackrf_set_vga_gain(self.dev, db)
        })
    }

    /// Antenna-port bias tee; keep off unless powering an active antenna.
    pub fn set_antenna_enable(&mut self, on: bool) -> Result<()> {
        check("hackrf_set_antenna_enable", unsafe {
            ffi::hackrf_set_antenna_enable(self.dev, u8::from(on))
        })
    }

    /// True while libhackrf reports the device streaming.
    pub fn is_streaming(&mut self) -> bool {
        unsafe { ffi::hackrf_is_streaming(self.dev) == ffi::HACKRF_TRUE }
    }

    /// Start transmitting; `depth` buffers may be queued ahead.
    pub fn start_tx(&mut self, depth: usize) -> Result<TxStream> {
        self.stop()?;
        let (sender, rx) = sync_channel(depth);
        let counters = Arc::new(Counters::default());
        let ctx = Box::into_raw(Box::new(TxCtx {
            rx,
            pending: Vec::new(),
            pos: 0,
            counters: counters.clone(),
        }));
        if let Err(e) = check("hackrf_start_tx", unsafe {
            ffi::hackrf_start_tx(self.dev, tx_cb, ctx as *mut c_void)
        }) {
            // libhackrf may have submitted transfers before failing: keep
            // the context until close.
            self.retired.push(Stream::Tx(ctx));
            return Err(e);
        }
        self.stream = Stream::Tx(ctx);
        Ok(TxStream { sender, counters })
    }

    /// Start receiving; up to `depth` buffers are queued before dropping.
    pub fn start_rx(&mut self, depth: usize) -> Result<RxStream> {
        self.stop()?;
        let (tx, receiver) = sync_channel(depth);
        let counters = Arc::new(Counters::default());
        let ctx = Box::into_raw(Box::new(RxCtx {
            tx,
            counters: counters.clone(),
        }));
        if let Err(e) = check("hackrf_start_rx", unsafe {
            ffi::hackrf_start_rx(self.dev, rx_cb, ctx as *mut c_void)
        }) {
            // as in start_tx
            self.retired.push(Stream::Rx(ctx));
            return Err(e);
        }
        self.stream = Stream::Rx(ctx);
        Ok(RxStream { receiver, counters })
    }

    /// Stop any active stream and free its callback context.
    ///
    /// On success the context is freed at once (libhackrf ≥
    /// [`MIN_LIBRARY_RELEASE`] has waited for every in-flight transfer).
    /// If `hackrf_stop_tx` / `hackrf_stop_rx` fails, transfers may still
    /// complete and call back, so the context (and with it the stream's
    /// channel end) is kept until the device is closed.
    pub fn stop(&mut self) -> Result<()> {
        let stream = std::mem::replace(&mut self.stream, Stream::None);
        let (call, rc) = match stream {
            Stream::None => return Ok(()),
            // SAFETY: valid device handle.
            Stream::Tx(_) => ("hackrf_stop_tx", unsafe { ffi::hackrf_stop_tx(self.dev) }),
            // SAFETY: valid device handle.
            Stream::Rx(_) => ("hackrf_stop_rx", unsafe { ffi::hackrf_stop_rx(self.dev) }),
        };
        let r = check(call, rc);
        if r.is_ok() {
            // SAFETY: the stop succeeded and waited for in-flight
            // transfers; the callback no longer runs.
            unsafe { stream.free() };
        } else {
            self.retired.push(stream);
        }
        r
    }
}

impl Drop for Device {
    /// Stop the stream, switch the RF amplifier off, close the device
    /// (which joins libhackrf's transfer thread), then free any context
    /// a failed stop kept, then release the library.
    fn drop(&mut self) {
        let _ = self.stop();
        // SAFETY: valid device handle; closed exactly once, after which
        // no transfer or callback is left (kill_transfer_thread joins).
        unsafe {
            // Leave the PA off on close regardless of what the caller did.
            ffi::hackrf_set_amp_enable(self.dev, 0);
            ffi::hackrf_close(self.dev);
        }
        for s in self.retired.drain(..) {
            // SAFETY: the device is closed; libhackrf holds no reference.
            unsafe { s.free() };
        }
        lib_release();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn release_strings() {
        assert_eq!(parse_release("2026.01.3"), Some((2026, 1)));
        assert_eq!(parse_release("2022.09.1"), Some((2022, 9)));
        assert_eq!(parse_release("unknown"), None);
        assert_eq!(parse_release("git-1234abcd"), None);
        assert!(check_release("2022.09.1").is_ok());
        assert!(check_release("2026.01.3").is_ok());
        assert!(check_release("unknown").is_ok());
        let e = check_release("2021.03.1").unwrap_err();
        assert!(e.name.contains("older than 2022.09.1"), "{e}");
    }
}

// Compile the README's code blocks as doctests.
#[cfg(doctest)]
#[doc = include_str!("../README.md")]
struct ReadmeDoctests;
