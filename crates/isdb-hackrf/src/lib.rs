//! Safe, minimal wrapper over the system `libhackrf` for continuous TX/RX.
//!
//! Samples are interleaved signed 8-bit I/Q (the HackRF wire format).
//! Streaming uses bounded channels: TX pulls buffers and transmits zeros on
//! underrun (counted); RX pushes buffers and drops on overrun (counted).
//! Dropping a [`Device`] stops any stream and closes the device, so a
//! panic or early return never leaves the radio transmitting.
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
    let out =
        unsafe { std::slice::from_raw_parts_mut(t.buffer as *mut i8, t.buffer_length as usize) };
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
    // SAFETY: as in tx_cb, rx_ctx is our live RxCtx.
    let t = unsafe { &mut *t };
    let ctx = unsafe { &*(t.rx_ctx as *const RxCtx) };
    let data =
        unsafe { std::slice::from_raw_parts(t.buffer as *const i8, t.valid_length as usize) };
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

/// An open HackRF.
pub struct Device {
    dev: *mut ffi::hackrf_device,
    stream: Stream,
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

impl Device {
    /// Open the first HackRF, or the one with `serial`.
    pub fn open(serial: Option<&str>) -> Result<Self> {
        lib_acquire()?;
        let mut dev = std::ptr::null_mut();
        let rc = match serial {
            None => unsafe { ffi::hackrf_open(&mut dev) },
            Some(s) => {
                let s = CString::new(s).expect("serial without NUL");
                // SAFETY: valid C string and out pointer.
                unsafe { ffi::hackrf_open_by_serial(s.as_ptr(), &mut dev) }
            }
        };
        if let Err(e) = check("hackrf_open", rc) {
            lib_release();
            return Err(e);
        }
        Ok(Self {
            dev,
            stream: Stream::None,
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
            ffi::hackrf_set_amp_enable(self.dev, on as u8)
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
            ffi::hackrf_set_antenna_enable(self.dev, on as u8)
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
            drop(unsafe { Box::from_raw(ctx) });
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
            drop(unsafe { Box::from_raw(ctx) });
            return Err(e);
        }
        self.stream = Stream::Rx(ctx);
        Ok(RxStream { receiver, counters })
    }

    /// Stop any active stream and free its context.
    pub fn stop(&mut self) -> Result<()> {
        match std::mem::replace(&mut self.stream, Stream::None) {
            Stream::None => Ok(()),
            Stream::Tx(ctx) => {
                let rc = unsafe { ffi::hackrf_stop_tx(self.dev) };
                // SAFETY: streaming stopped; the callback no longer runs.
                drop(unsafe { Box::from_raw(ctx) });
                check("hackrf_stop_tx", rc)
            }
            Stream::Rx(ctx) => {
                let rc = unsafe { ffi::hackrf_stop_rx(self.dev) };
                drop(unsafe { Box::from_raw(ctx) });
                check("hackrf_stop_rx", rc)
            }
        }
    }
}

impl Drop for Device {
    fn drop(&mut self) {
        let _ = self.stop();
        // Leave the PA off on close regardless of what the caller did.
        unsafe {
            ffi::hackrf_set_amp_enable(self.dev, 0);
            ffi::hackrf_close(self.dev);
        }
        lib_release();
    }
}

// Compile the README's code blocks as doctests.
#[cfg(doctest)]
#[doc = include_str!("../README.md")]
struct ReadmeDoctests;
