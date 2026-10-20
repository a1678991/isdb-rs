//! Safe, minimal wrapper over the system `libiio` (0.x API) for attribute
//! access and continuous TX/RX streaming of 16-bit I/Q, as used with the
//! ADALM-Pluto family of AD936x radios.
//!
//! A [`Context`] is one connection to an IIO context (`ip:192.168.2.1`,
//! `usb:1.2.5`, `local:`, …). Attributes are read and written through
//! [`Device`] and [`Channel`] handles borrowed from it. Streaming runs on a
//! thread per stream that owns the IIO buffer and exchanges interleaved
//! `i16` buffers with the caller over bounded channels, as `isdb-hackrf`
//! does with its USB callbacks: TX pulls buffers and pushes zeros on
//! underrun (counted); RX pushes buffers and drops on overrun (counted).
//! Dropping a [`Context`] stops its streams before destroying it, so an
//! early return or an unwinding panic never leaves a stream thread using a
//! freed context.
//!
//! # Transmitting
//!
//! This crate has no RF gate of its own: [`Context::start_tx`] sends
//! whatever the caller queues to the DAC. Transmitting on broadcast
//! frequencies needs a licence (in Japan under the Radio Act); use a
//! conducted or shielded setup. `isdb-cli` adds the `--rf-enable` /
//! `--dry-run` gate.
//!
//! # libiio version
//!
//! Built against the libiio **0.x** API (0.21 or later; tested with 0.26
//! over the network backend against iiod 0.25). libiio 1.x replaced these
//! calls (`iio_create_context` with parameters, block-based streams) and
//! is not supported.
//!
//! # Threads
//!
//! [`Context`] is `Send` but not `Sync`: libiio does not document its
//! contexts as thread-safe, so attribute calls are made from one thread at
//! a time. A stream thread uses only its own `iio_buffer`; libiio's
//! backends serve buffers apart from attribute requests (the network and
//! USB backends open a separate pipe per buffer; the local backend reads
//! the character device while attributes go through sysfs), which is the
//! same concurrency SoapyPlutoSDR and pyadi-iio rely on. Stopping a stream
//! uses `iio_buffer_cancel`, which iio.h documents as thread-safe.
//!
//! # Example
//!
//! ```no_run
//! let mut ctx = isdb_iio::Context::open("ip:192.168.2.1")?;
//! println!("{}", ctx.description());
//! let phy = ctx.device("ad9361-phy")?;
//! phy.channel("voltage0", false)?.write_i64("sampling_frequency", 8_126_984)?;
//! let rx = ctx.start_rx("cf-ad9361-lpc", &["voltage0", "voltage1"], 262_144, 4, 16)?;
//! let iq: Vec<i16> = rx.receiver.recv().expect("stream running"); // I, Q, I, Q, …
//! println!("{} samples", iq.len() / 2);
//! ctx.stop()?;
//! # Ok::<(), isdb_iio::Error>(())
//! ```

#![allow(unsafe_code)]

mod ffi;

use std::ffi::{CStr, CString, c_char};
use std::marker::PhantomData;
use std::ptr::NonNull;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{
    Receiver, RecvTimeoutError, SyncSender, TryRecvError, TrySendError, sync_channel,
};
use std::thread::JoinHandle;
use std::time::Duration;

/// libiio error.
#[derive(Debug, Clone, thiserror::Error)]
#[error("{call} {what}: {message} (errno {code})")]
pub struct Error {
    /// Function that failed.
    pub call: &'static str,
    /// What it was called on (URI, device, channel and attribute).
    pub what: String,
    /// errno value (positive).
    pub code: i32,
    /// `iio_strerror` text.
    pub message: String,
}

/// Result alias.
pub type Result<T> = std::result::Result<T, Error>;

fn strerror(code: i32) -> String {
    let mut buf = [0 as c_char; 256];
    // SAFETY: buf is writable for its length; iio_strerror NUL-terminates.
    unsafe { ffi::iio_strerror(code, buf.as_mut_ptr(), buf.len()) };
    // SAFETY: NUL-terminated by iio_strerror.
    unsafe { CStr::from_ptr(buf.as_ptr()) }
        .to_string_lossy()
        .into_owned()
}

fn error(call: &'static str, what: impl Into<String>, code: i32) -> Error {
    let code = code.abs();
    Error {
        call,
        what: what.into(),
        code,
        message: strerror(code),
    }
}

/// An error from a negative libiio return value.
fn check(call: &'static str, what: impl FnOnce() -> String, rc: isize) -> Result<usize> {
    if rc < 0 {
        Err(error(call, what(), rc as i32))
    } else {
        Ok(rc as usize)
    }
}

/// The error of a call that returned NULL and set errno.
fn last_error(call: &'static str, what: impl Into<String>) -> Error {
    let code = std::io::Error::last_os_error()
        .raw_os_error()
        .filter(|&c| c != 0)
        .unwrap_or(EIO);
    error(call, what, code)
}

/// EIO, for a NULL return without errno.
const EIO: i32 = 5;

/// EINVAL, for arguments refused before calling libiio.
const EINVAL: i32 = 22;

fn cstring(call: &'static str, s: &str) -> Result<CString> {
    CString::new(s).map_err(|_| Error {
        call,
        what: format!("{s:?}"),
        code: EINVAL,
        message: "contains a NUL byte".into(),
    })
}

/// Read an attribute through `read` (a libiio `*_attr_read`) into a String.
fn read_string(
    call: &'static str,
    what: impl FnOnce() -> String,
    read: impl FnOnce(*mut c_char, usize) -> isize,
) -> Result<String> {
    let mut buf = vec![0u8; 4096];
    let n = check(call, what, read(buf.as_mut_ptr().cast(), buf.len()))?;
    // The count includes the terminating NUL when there is one.
    buf.truncate(n.min(buf.len()));
    while buf.last() == Some(&0) {
        buf.pop();
    }
    Ok(String::from_utf8_lossy(&buf).trim_end().to_string())
}

/// libiio version: `0.26 (v0.26)`.
pub fn library_version() -> String {
    let (mut major, mut minor) = (0, 0);
    let mut tag = [0 as c_char; 8];
    // SAFETY: valid out pointers; git_tag holds 8 bytes, NUL-terminated.
    unsafe { ffi::iio_library_get_version(&mut major, &mut minor, tag.as_mut_ptr()) };
    tag[7] = 0;
    // SAFETY: NUL-terminated above.
    let tag = unsafe { CStr::from_ptr(tag.as_ptr()) }.to_string_lossy();
    format!("{major}.{minor} ({tag})")
}

/// Streaming counters shared with a stream thread.
#[derive(Debug, Default)]
pub struct Counters {
    /// Buffers pushed (TX) or refilled (RX).
    pub buffers: AtomicU64,
    /// TX: buffers padded with zeros because no data was queued.
    pub underruns: AtomicU64,
    /// RX: buffers dropped because the consumer was slow.
    pub overruns: AtomicU64,
    /// Set when the stream thread ended (queue closed, stop requested or
    /// an error, which [`Context::stop`] returns).
    pub finished: AtomicBool,
}

/// Producer side of a TX stream.
pub struct TxStream {
    /// Queue of interleaved buffers (one `i16` per enabled channel per
    /// sample, in channel order, any length); drop it to end the stream.
    pub sender: SyncSender<Vec<i16>>,
    /// Live counters.
    pub counters: Arc<Counters>,
}

/// Consumer side of an RX stream.
pub struct RxStream {
    /// Received interleaved buffers (one `i16` per enabled channel per
    /// sample, in channel order).
    pub receiver: Receiver<Vec<i16>>,
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

/// An `iio_buffer` handed to its stream thread.
#[derive(Clone, Copy)]
struct BufPtr(NonNull<ffi::iio_buffer>);

// SAFETY: the buffer is used by its stream thread only, apart from
// iio_buffer_cancel (thread-safe per iio.h) and iio_buffer_destroy, which
// runs after the thread has been joined.
unsafe impl Send for BufPtr {}

impl BufPtr {
    /// The buffer's samples as `i16` components.
    ///
    /// # Safety
    ///
    /// No other slice of the buffer may be alive; the buffer must not be
    /// pushed, refilled or destroyed while the slice is used.
    unsafe fn components<'a>(self) -> &'a mut [i16] {
        // SAFETY: start..end is the buffer's sample memory, allocated by
        // libiio with at least 2-byte alignment (every sample size is a
        // multiple of the 16-bit channel storage checked at creation).
        unsafe {
            let start = ffi::iio_buffer_start(self.0.as_ptr()).cast::<i16>();
            let end = ffi::iio_buffer_end(self.0.as_ptr()).cast::<i16>();
            let len = usize::try_from(end.offset_from(start)).unwrap_or(0);
            std::slice::from_raw_parts_mut(start, len)
        }
    }
}

/// How long [`Context::stop`] waits for a stream thread before cancelling
/// its blocked push or refill.
const STOP_TIMEOUT: Duration = Duration::from_secs(3);

/// A running stream: its thread, buffer and enabled channels.
struct Stream {
    buf: BufPtr,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<Result<()>>>,
    channels: Vec<NonNull<ffi::iio_channel>>,
    counters: Arc<Counters>,
}

/// One connection to an IIO context.
pub struct Context {
    ctx: NonNull<ffi::iio_context>,
    uri: String,
    streams: Vec<Stream>,
}

// SAFETY: a libiio context may be used from any single thread at a time;
// Context is not Sync. Stream threads use only their own buffers (see the
// crate documentation).
unsafe impl Send for Context {}

impl std::fmt::Debug for Context {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Context")
            .field("uri", &self.uri)
            .field("streams", &self.streams.len())
            .finish_non_exhaustive()
    }
}

/// A device of a [`Context`] (`ad9361-phy`, `cf-ad9361-lpc`, …).
#[derive(Debug, Clone, Copy)]
pub struct Device<'a> {
    dev: NonNull<ffi::iio_device>,
    name: &'a str,
    _ctx: PhantomData<&'a Context>,
}

/// A channel of a [`Device`] (`voltage0` input, `altvoltage1` output, …).
#[derive(Debug, Clone, Copy)]
pub struct Channel<'a> {
    chn: NonNull<ffi::iio_channel>,
    device: &'a str,
    name: &'a str,
    output: bool,
    _ctx: PhantomData<&'a Context>,
}

impl Context {
    /// Connect to the context at `uri` (`ip:HOST`, `usb:BUS.DEV.IF`,
    /// `local:`, …).
    pub fn open(uri: &str) -> Result<Self> {
        let c = cstring("iio_create_context_from_uri", uri)?;
        // SAFETY: valid C string.
        let ctx = unsafe { ffi::iio_create_context_from_uri(c.as_ptr()) };
        let ctx =
            NonNull::new(ctx).ok_or_else(|| last_error("iio_create_context_from_uri", uri))?;
        Ok(Self {
            ctx,
            uri: uri.to_string(),
            streams: Vec::new(),
        })
    }

    /// The URI this context was opened with.
    pub fn uri(&self) -> &str {
        &self.uri
    }

    /// Backend description string.
    pub fn description(&self) -> String {
        // SAFETY: valid context; returns a string owned by it (or NULL).
        let p = unsafe { ffi::iio_context_get_description(self.ctx.as_ptr()) };
        if p.is_null() {
            return String::new();
        }
        // SAFETY: NUL-terminated string owned by the live context.
        unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned()
    }

    /// A context attribute (`hw_model`, `fw_version`, `hw_serial`, …).
    pub fn attr(&self, name: &str) -> Option<String> {
        let c = CString::new(name).ok()?;
        // SAFETY: valid context and C string; the result is owned by the
        // context (or NULL).
        let p = unsafe { ffi::iio_context_get_attr_value(self.ctx.as_ptr(), c.as_ptr()) };
        if p.is_null() {
            return None;
        }
        // SAFETY: NUL-terminated string owned by the live context.
        Some(unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned())
    }

    /// Timeout of blocking operations (0 = none).
    pub fn set_timeout(&mut self, ms: u32) -> Result<()> {
        // SAFETY: valid context.
        let rc = unsafe { ffi::iio_context_set_timeout(self.ctx.as_ptr(), ms) };
        check("iio_context_set_timeout", || self.uri.clone(), rc as isize).map(|_| ())
    }

    /// The device called `name`.
    pub fn device<'a>(&'a self, name: &'a str) -> Result<Device<'a>> {
        let c = cstring("iio_context_find_device", name)?;
        // SAFETY: valid context and C string; the device lives as long as
        // the context.
        let dev = unsafe { ffi::iio_context_find_device(self.ctx.as_ptr(), c.as_ptr()) };
        let dev = NonNull::new(dev).ok_or_else(|| Error {
            call: "iio_context_find_device",
            what: format!("{name} in {}", self.uri),
            code: 19, // ENODEV
            message: "no such device".into(),
        })?;
        Ok(Device {
            dev,
            name,
            _ctx: PhantomData,
        })
    }

    /// Enable `channels` (outputs for TX, inputs for RX) of `device` and
    /// create a buffer of `samples` samples over `kernel_buffers` kernel
    /// blocks. Every enabled channel must be stored in 16 bits.
    fn open_stream(
        &self,
        device: &str,
        channels: &[&str],
        output: bool,
        samples: usize,
        kernel_buffers: u32,
    ) -> Result<(BufPtr, Vec<NonNull<ffi::iio_channel>>)> {
        let dev = self.device(device)?;
        let what = || format!("{device} in {}", self.uri);
        if channels.is_empty() || samples == 0 {
            return Err(Error {
                call: "iio_device_create_buffer",
                what: what(),
                code: EINVAL,
                message: "no channels or an empty buffer".into(),
            });
        }
        let mut enabled = Vec::new();
        for name in channels {
            let ch = dev.channel(name, output)?;
            // SAFETY: valid channel of the live context.
            unsafe { ffi::iio_channel_enable(ch.chn.as_ptr()) };
            enabled.push(ch.chn);
        }
        let disable = |enabled: &[NonNull<ffi::iio_channel>]| {
            for c in enabled {
                // SAFETY: valid channel of the live context.
                unsafe { ffi::iio_channel_disable(c.as_ptr()) };
            }
        };
        // SAFETY: valid device.
        let size = unsafe { ffi::iio_device_get_sample_size(dev.dev.as_ptr()) };
        if size != 2 * channels.len() as isize {
            disable(&enabled);
            return Err(Error {
                call: "iio_device_get_sample_size",
                what: what(),
                code: EINVAL,
                message: format!(
                    "{size} bytes per sample for {} channels: only 16-bit channels are supported",
                    channels.len()
                ),
            });
        }
        // SAFETY: valid device.
        let rc =
            unsafe { ffi::iio_device_set_kernel_buffers_count(dev.dev.as_ptr(), kernel_buffers) };
        if let Err(e) = check("iio_device_set_kernel_buffers_count", what, rc as isize) {
            disable(&enabled);
            return Err(e);
        }
        // SAFETY: valid device with its channels enabled; not cyclic.
        let buf = unsafe { ffi::iio_device_create_buffer(dev.dev.as_ptr(), samples, false) };
        match NonNull::new(buf) {
            Some(b) => Ok((BufPtr(b), enabled)),
            None => {
                let e = last_error("iio_device_create_buffer", what());
                disable(&enabled);
                Err(e)
            }
        }
    }

    /// Start transmitting through `channels` (outputs) of `device` in
    /// buffers of `samples` samples, `kernel_buffers` of them in the
    /// kernel; `depth` caller buffers may be queued ahead.
    ///
    /// The stream thread waits for the first queued buffer before pushing
    /// anything, so the device does not start on zeros. When the sender is
    /// dropped it pads the last buffer with zeros and pushes
    /// `kernel_buffers` more zero buffers, so every queued sample has
    /// left the kernel queue before `finished` is set.
    pub fn start_tx(
        &mut self,
        device: &str,
        channels: &[&str],
        samples: usize,
        kernel_buffers: u32,
        depth: usize,
    ) -> Result<TxStream> {
        let (buf, enabled) = self.open_stream(device, channels, true, samples, kernel_buffers)?;
        let (sender, rx) = sync_channel(depth);
        let counters = Arc::new(Counters::default());
        let stop = Arc::new(AtomicBool::new(false));
        let thread = {
            let (counters, stop) = (counters.clone(), stop.clone());
            let what = format!("{device} in {}", self.uri);
            std::thread::Builder::new()
                .name("iio-tx".into())
                .spawn(move || {
                    let r = tx_loop(buf, &rx, &counters, &stop, kernel_buffers, &what);
                    counters.finished.store(true, Ordering::SeqCst);
                    r
                })
        };
        self.add_stream(buf, enabled, stop, thread, counters.clone())?;
        Ok(TxStream { sender, counters })
    }

    /// Start receiving from `channels` (inputs) of `device` in buffers of
    /// `samples` samples, `kernel_buffers` of them in the kernel; up to
    /// `depth` buffers are queued before dropping.
    pub fn start_rx(
        &mut self,
        device: &str,
        channels: &[&str],
        samples: usize,
        kernel_buffers: u32,
        depth: usize,
    ) -> Result<RxStream> {
        let (buf, enabled) = self.open_stream(device, channels, false, samples, kernel_buffers)?;
        let (tx, receiver) = sync_channel(depth);
        let counters = Arc::new(Counters::default());
        let stop = Arc::new(AtomicBool::new(false));
        let thread = {
            let (counters, stop) = (counters.clone(), stop.clone());
            let what = format!("{device} in {}", self.uri);
            std::thread::Builder::new()
                .name("iio-rx".into())
                .spawn(move || {
                    let r = rx_loop(buf, &tx, &counters, &stop, &what);
                    counters.finished.store(true, Ordering::SeqCst);
                    r
                })
        };
        self.add_stream(buf, enabled, stop, thread, counters.clone())?;
        Ok(RxStream { receiver, counters })
    }

    fn add_stream(
        &mut self,
        buf: BufPtr,
        channels: Vec<NonNull<ffi::iio_channel>>,
        stop: Arc<AtomicBool>,
        thread: std::io::Result<JoinHandle<Result<()>>>,
        counters: Arc<Counters>,
    ) -> Result<()> {
        let spawn_error = thread
            .as_ref()
            .err()
            .map(|e| e.raw_os_error().unwrap_or(11));
        // Kept even when the spawn failed, so that stop() destroys the
        // buffer and disables the channels.
        self.streams.push(Stream {
            buf,
            stop,
            thread: thread.ok(),
            channels,
            counters,
        });
        if let Some(code) = spawn_error {
            let s = self.streams.pop().expect("just pushed");
            let _ = Self::end(s);
            return Err(error("std::thread::spawn", "stream thread", code));
        }
        Ok(())
    }

    /// True while a stream thread is running.
    pub fn is_streaming(&self) -> bool {
        self.streams
            .iter()
            .any(|s| !s.counters.finished.load(Ordering::SeqCst))
    }

    /// Stop one stream: ask the thread to stop (it does after the push or
    /// refill in progress, one buffer of signal), cancel the buffer if it
    /// does not within [`STOP_TIMEOUT`], join the thread, destroy the
    /// buffer and disable its channels. Returns the thread's error.
    ///
    /// The cancel is the fallback only: with the network backend a
    /// cancelled buffer's pipe is shut down, and destroying it then makes
    /// libiio print read errors to stderr.
    fn end(mut s: Stream) -> Result<()> {
        s.stop.store(true, Ordering::SeqCst);
        let t = std::time::Instant::now();
        while !s.counters.finished.load(Ordering::SeqCst)
            && s.thread.is_some()
            && t.elapsed() < STOP_TIMEOUT
        {
            std::thread::sleep(Duration::from_millis(2));
        }
        if !s.counters.finished.load(Ordering::SeqCst) {
            // SAFETY: iio_buffer_cancel is thread-safe; the buffer is live.
            unsafe { ffi::iio_buffer_cancel(s.buf.0.as_ptr()) };
        }
        let r = match s.thread.take().map(JoinHandle::join) {
            Some(Ok(r)) => r,
            Some(Err(p)) => std::panic::resume_unwind(p),
            None => Ok(()),
        };
        // SAFETY: the thread has ended; nothing else uses the buffer.
        unsafe { ffi::iio_buffer_destroy(s.buf.0.as_ptr()) };
        for c in &s.channels {
            // SAFETY: valid channel of the live context.
            unsafe { ffi::iio_channel_disable(c.as_ptr()) };
        }
        r
    }

    /// Stop every stream (without waiting for queued TX buffers) and
    /// return the first error a stream thread ended with.
    pub fn stop(&mut self) -> Result<()> {
        let mut first = Ok(());
        for s in std::mem::take(&mut self.streams) {
            let r = Self::end(s);
            if first.is_ok() {
                first = r;
            }
        }
        first
    }
}

impl Drop for Context {
    /// Stop the streams, then destroy the context.
    fn drop(&mut self) {
        let _ = self.stop();
        // SAFETY: valid context, destroyed once; no buffer or stream
        // thread is left.
        unsafe { ffi::iio_context_destroy(self.ctx.as_ptr()) };
    }
}

/// TX stream thread: fill the buffer from the queue and push it.
fn tx_loop(
    buf: BufPtr,
    rx: &Receiver<Vec<i16>>,
    counters: &Counters,
    stop: &AtomicBool,
    drain_buffers: u32,
    what: &str,
) -> Result<()> {
    let push = || {
        // SAFETY: the buffer is live and no slice of it is held.
        let rc = unsafe { ffi::iio_buffer_push(buf.0.as_ptr()) };
        check("iio_buffer_push", || what.to_string(), rc)
    };
    let mut pending: Vec<i16> = Vec::new();
    let mut pos = 0;
    let mut started = false;
    loop {
        // SAFETY: the only slice of the buffer, dropped before the push.
        let out = unsafe { buf.components() };
        let mut filled = 0;
        let mut starved = false;
        let mut ended = false;
        while filled < out.len() {
            if pos == pending.len() {
                let next = if started {
                    match rx.try_recv() {
                        Ok(v) => Some(v),
                        Err(TryRecvError::Empty) => {
                            starved = true;
                            break;
                        }
                        Err(TryRecvError::Disconnected) => None,
                    }
                } else {
                    // Nothing pushed yet: wait for data (or a stop).
                    loop {
                        match rx.recv_timeout(Duration::from_millis(50)) {
                            Ok(v) => break Some(v),
                            Err(RecvTimeoutError::Timeout) if stop.load(Ordering::SeqCst) => {
                                return Ok(());
                            }
                            Err(RecvTimeoutError::Timeout) => {}
                            Err(RecvTimeoutError::Disconnected) => break None,
                        }
                    }
                };
                match next {
                    Some(v) => {
                        pending = v;
                        pos = 0;
                        started = true;
                        continue;
                    }
                    None => {
                        ended = true;
                        break;
                    }
                }
            }
            let n = (pending.len() - pos).min(out.len() - filled);
            for (o, &x) in out[filled..filled + n]
                .iter_mut()
                .zip(&pending[pos..pos + n])
            {
                *o = x.to_le();
            }
            pos += n;
            filled += n;
        }
        if ended && !started {
            return Ok(()); // closed before any data
        }
        out[filled..].fill(0);
        if starved {
            counters.underruns.fetch_add(1, Ordering::Relaxed);
        }
        if stop.load(Ordering::SeqCst) {
            return Ok(());
        }
        push()?;
        counters.buffers.fetch_add(1, Ordering::Relaxed);
        if ended {
            // Push the last samples out of the kernel queue.
            for _ in 0..drain_buffers {
                // SAFETY: as above.
                unsafe { buf.components() }.fill(0);
                if stop.load(Ordering::SeqCst) {
                    return Ok(());
                }
                push()?;
            }
            return Ok(());
        }
    }
}

/// RX stream thread: refill the buffer and queue a copy of it.
fn rx_loop(
    buf: BufPtr,
    tx: &SyncSender<Vec<i16>>,
    counters: &Counters,
    stop: &AtomicBool,
    what: &str,
) -> Result<()> {
    loop {
        if stop.load(Ordering::SeqCst) {
            return Ok(());
        }
        // SAFETY: the buffer is live and no slice of it is held.
        let rc = unsafe { ffi::iio_buffer_refill(buf.0.as_ptr()) };
        if rc < 0 && stop.load(Ordering::SeqCst) {
            return Ok(()); // cancelled by stop()
        }
        check("iio_buffer_refill", || what.to_string(), rc)?;
        // SAFETY: the only slice of the buffer, dropped before the refill.
        let data: Vec<i16> = unsafe { buf.components() }
            .iter()
            .map(|&x| i16::from_le(x))
            .collect();
        counters.buffers.fetch_add(1, Ordering::Relaxed);
        match tx.try_send(data) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) => {
                counters.overruns.fetch_add(1, Ordering::Relaxed);
            }
            Err(TrySendError::Disconnected(_)) => return Ok(()),
        }
    }
}

impl<'a> Device<'a> {
    /// The device name.
    pub fn name(&self) -> &'a str {
        self.name
    }

    /// The channel `name` (an output channel if `output`).
    pub fn channel(&self, name: &'a str, output: bool) -> Result<Channel<'a>> {
        let c = cstring("iio_device_find_channel", name)?;
        // SAFETY: valid device and C string; the channel lives as long as
        // the context.
        let chn = unsafe { ffi::iio_device_find_channel(self.dev.as_ptr(), c.as_ptr(), output) };
        let chn = NonNull::new(chn).ok_or_else(|| Error {
            call: "iio_device_find_channel",
            what: format!(
                "{} {name} ({})",
                self.name,
                if output { "output" } else { "input" }
            ),
            code: 19, // ENODEV
            message: "no such channel".into(),
        })?;
        Ok(Channel {
            chn,
            device: self.name,
            name,
            output,
            _ctx: PhantomData,
        })
    }

    fn what(&self, attr: &str) -> String {
        format!("{} {attr}", self.name)
    }

    /// Read a device attribute.
    pub fn read(&self, attr: &str) -> Result<String> {
        let a = cstring("iio_device_attr_read", attr)?;
        read_string(
            "iio_device_attr_read",
            || self.what(attr),
            // SAFETY: valid device, C string and destination buffer.
            |dst, len| unsafe {
                ffi::iio_device_attr_read(self.dev.as_ptr(), a.as_ptr(), dst, len)
            },
        )
    }

    /// Write a device attribute.
    pub fn write(&self, attr: &str, value: &str) -> Result<()> {
        let (a, v) = (
            cstring("iio_device_attr_write", attr)?,
            cstring("iio_device_attr_write", value)?,
        );
        // SAFETY: valid device and C strings.
        let rc = unsafe { ffi::iio_device_attr_write(self.dev.as_ptr(), a.as_ptr(), v.as_ptr()) };
        check("iio_device_attr_write", || self.what(attr), rc).map(|_| ())
    }

    /// Read a debug attribute.
    pub fn read_debug(&self, attr: &str) -> Result<String> {
        let a = cstring("iio_device_debug_attr_read", attr)?;
        read_string(
            "iio_device_debug_attr_read",
            || self.what(attr),
            // SAFETY: valid device, C string and destination buffer.
            |dst, len| unsafe {
                ffi::iio_device_debug_attr_read(self.dev.as_ptr(), a.as_ptr(), dst, len)
            },
        )
    }

    /// Write a debug attribute.
    pub fn write_debug(&self, attr: &str, value: &str) -> Result<()> {
        let (a, v) = (
            cstring("iio_device_debug_attr_write", attr)?,
            cstring("iio_device_debug_attr_write", value)?,
        );
        // SAFETY: valid device and C strings.
        let rc =
            unsafe { ffi::iio_device_debug_attr_write(self.dev.as_ptr(), a.as_ptr(), v.as_ptr()) };
        check("iio_device_debug_attr_write", || self.what(attr), rc).map(|_| ())
    }
}

impl Channel<'_> {
    fn what(&self, attr: &str) -> String {
        format!(
            "{} {} ({}) {attr}",
            self.device,
            self.name,
            if self.output { "output" } else { "input" }
        )
    }

    /// Read an attribute as text.
    pub fn read(&self, attr: &str) -> Result<String> {
        let a = cstring("iio_channel_attr_read", attr)?;
        read_string(
            "iio_channel_attr_read",
            || self.what(attr),
            // SAFETY: valid channel, C string and destination buffer.
            |dst, len| unsafe {
                ffi::iio_channel_attr_read(self.chn.as_ptr(), a.as_ptr(), dst, len)
            },
        )
    }

    /// Write an attribute as text.
    pub fn write(&self, attr: &str, value: &str) -> Result<()> {
        let (a, v) = (
            cstring("iio_channel_attr_write", attr)?,
            cstring("iio_channel_attr_write", value)?,
        );
        // SAFETY: valid channel and C strings.
        let rc = unsafe { ffi::iio_channel_attr_write(self.chn.as_ptr(), a.as_ptr(), v.as_ptr()) };
        check("iio_channel_attr_write", || self.what(attr), rc).map(|_| ())
    }

    /// Read an integer attribute.
    pub fn read_i64(&self, attr: &str) -> Result<i64> {
        let a = cstring("iio_channel_attr_read_longlong", attr)?;
        let mut v = 0;
        // SAFETY: valid channel, C string and out pointer.
        let rc =
            unsafe { ffi::iio_channel_attr_read_longlong(self.chn.as_ptr(), a.as_ptr(), &mut v) };
        check(
            "iio_channel_attr_read_longlong",
            || self.what(attr),
            rc as isize,
        )?;
        Ok(v)
    }

    /// Write an integer attribute.
    pub fn write_i64(&self, attr: &str, value: i64) -> Result<()> {
        let a = cstring("iio_channel_attr_write_longlong", attr)?;
        // SAFETY: valid channel and C string.
        let rc =
            unsafe { ffi::iio_channel_attr_write_longlong(self.chn.as_ptr(), a.as_ptr(), value) };
        check(
            "iio_channel_attr_write_longlong",
            || self.what(attr),
            rc as isize,
        )
        .map(|_| ())
    }

    /// Read a floating-point attribute.
    pub fn read_f64(&self, attr: &str) -> Result<f64> {
        let a = cstring("iio_channel_attr_read_double", attr)?;
        let mut v = 0.0;
        // SAFETY: valid channel, C string and out pointer.
        let rc =
            unsafe { ffi::iio_channel_attr_read_double(self.chn.as_ptr(), a.as_ptr(), &mut v) };
        check(
            "iio_channel_attr_read_double",
            || self.what(attr),
            rc as isize,
        )?;
        Ok(v)
    }

    /// Write a floating-point attribute.
    pub fn write_f64(&self, attr: &str, value: f64) -> Result<()> {
        let a = cstring("iio_channel_attr_write_double", attr)?;
        // SAFETY: valid channel and C string.
        let rc =
            unsafe { ffi::iio_channel_attr_write_double(self.chn.as_ptr(), a.as_ptr(), value) };
        check(
            "iio_channel_attr_write_double",
            || self.what(attr),
            rc as isize,
        )
        .map(|_| ())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn errors_carry_errno_text() {
        let e = error("iio_buffer_push", "cf-ad9361-dds-core-lpc", -110);
        assert_eq!(e.code, 110);
        let s = e.to_string();
        assert!(
            s.starts_with("iio_buffer_push cf-ad9361-dds-core-lpc: ") && s.ends_with("(errno 110)"),
            "{s}"
        );
        assert!(check("x", String::new, -22).is_err());
        assert_eq!(check("x", String::new, 7).unwrap(), 7);
        assert!(cstring("x", "a\0b").is_err());
    }

    #[test]
    fn version_string() {
        let v = library_version();
        assert!(v.starts_with("0."), "{v}");
    }

    #[test]
    fn unreachable_uri_is_an_error() {
        // No backend accepts this URI; nothing touches the network.
        let e = Context::open("bogus:nothing").unwrap_err();
        assert_eq!(e.call, "iio_create_context_from_uri");
        assert_eq!(e.what, "bogus:nothing");
    }
}

// Compile the README's code blocks as doctests.
#[cfg(doctest)]
#[doc = include_str!("../README.md")]
struct ReadmeDoctests;
