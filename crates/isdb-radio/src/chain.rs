//! Driving the ISDB-S3 receive chain in a radio's FPGA
//! ([`crate::Offload::RxSlots`]), device-neutral: the open sequence and
//! the equaliser's slow loop, through the command protocol of
//! `isdb_frontend::control` on any [`RegisterAccess`] (libiio's register
//! access on a Pluto, a model of the block in the tests).
//!
//! The register map is `isdb_frontend::control::chain`'s, whose addresses
//! are placeholders until the chain's HDL is final; this module uses its
//! names only.
//!
//! **Open** ([`ChainControl::open`]): CTRL 0 (a chain left running by a
//! process that did not close stops); VERSION and FEATURES read (the
//! `Slots` format must be this host's); the front end configured as for
//! symbols (`Encoder::configure`, KP read back); CARRIER and LAYOUT_LAG;
//! the host's taps, constellation and n0 when it has them and FEATURES
//! allows (else the block starts on the identity taps, the nominal points
//! and the default n0); INIT; CTRL START, then RUN | SYMBOLS | SLOTS; the
//! front end's STATUS must read run, symbols and slots.
//!
//! **Slow loop** ([`SlowLoop::tick`], on a timer thread by
//! [`spawn_slow_loop`]): SUMS_FRAME, SUMS_COUNT, CARRIER, TAPS_FROM and
//! the chain's STATUS read; when a frame's sums are latched that were not
//! solved yet, the 308 sum registers, SUMS_FRAME again (a different value:
//! the latch moved during the read; tried again at the next tick), the
//! solve (the caller's [`Solver`]: `isdbs3_phy::chain::eq::solve_training`,
//! the model's `f64` solve and Q2.14 rounding), the 11 taps and TAPS_FROM
//! (which commits them). Until the first solve it polls every
//! [`SlowLoopOptions::poll`]; then every [`SlowLoopOptions::every`]. The
//! carrier register is read every tick and reported (informational: the
//! FPGA tracks the carrier itself).

use crate::{RadioError, Result};
use isdb_frontend::control::chain::{
    self as cc, ChainInit, NONE, Taps, TrainingSums, features, init, pack_point, reg,
};
use isdb_frontend::control::{self, Encoder, GPIO_IN_REG, GPIO_OUT_REG, Op};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use tracing::{debug, info, warn};

/// Register access to the core whose general purpose pair carries the
/// command words (`cf-ad9361-lpc`'s `gpio_out` / `gpio_in`).
pub trait RegisterAccess {
    /// Read the register at debug address `addr`.
    fn read(&self, addr: u32) -> Result<u32>;
    /// Write `value` to the register at debug address `addr`.
    fn write(&self, addr: u32, value: u32) -> Result<()>;
}

/// The host's slow-loop solve: the 11 taps (Q2.14) from a frame's sums,
/// `None` when they do not solve.
pub type Solver = fn(&TrainingSums) -> Option<Taps>;

/// The chain as the host drives it: the command encoder (its toggle bit
/// and `gpio_out` bit 0 carried on) and what the open found.
#[derive(Debug, Clone)]
pub struct ChainControl {
    backend: &'static str,
    enc: Encoder,
    /// FEATURES ([`features`]).
    pub features: u32,
    /// The `Slots` format version the chain reports.
    pub version: u16,
}

/// What [`ChainControl::open`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenReport {
    /// The chain's `Slots` format version.
    pub version: u16,
    /// Its FEATURES.
    pub features: u32,
    /// The INIT bits written ([`init`]): what the host loaded.
    pub loaded: u32,
    /// What the host had but FEATURES did not allow (INIT bits).
    pub refused: u32,
    /// Command words written.
    pub commands: usize,
}

impl OpenReport {
    /// `taps, constellation (16APSK) loaded; n0 default` and the like.
    pub fn describe(&self) -> String {
        let what = |bit: u32, name: &str| -> String {
            if self.loaded & bit != 0 {
                format!("{name} from the host")
            } else if self.refused & bit != 0 {
                format!("{name} default (the image does not take it)")
            } else {
                format!("{name} default")
            }
        };
        format!(
            "Slots format {}, features {:#x} (LDPC limit {}, step {}); {}, {}, {}",
            self.version,
            self.features,
            features::ldpc_limit(self.features),
            features::ldpc_step(self.features),
            what(init::TAPS, "taps"),
            what(init::CONSTELLATION, "constellation"),
            what(init::N0, "n0")
        )
    }
}

impl ChainControl {
    /// The chain behind `port`, its encoder continuing from `gpio_out`
    /// (nothing written).
    ///
    /// # Errors
    ///
    /// A failed register read.
    pub fn attach(backend: &'static str, port: &dyn RegisterAccess) -> Result<Self> {
        Ok(Self {
            backend,
            enc: Encoder::new(port.read(GPIO_OUT_REG)?),
            features: 0,
            version: 0,
        })
    }

    fn reply(&self, what: &str, detail: String) -> RadioError {
        RadioError::Reply {
            backend: self.backend,
            what: what.into(),
            detail,
        }
    }

    /// One command word.
    fn command(&mut self, port: &dyn RegisterAccess, op: Op, arg: u32) -> Result<()> {
        port.write(GPIO_OUT_REG, self.enc.word(op, arg))
    }

    /// Write `values` from `addr` on.
    ///
    /// # Errors
    ///
    /// A failed register write.
    pub fn write(&mut self, port: &dyn RegisterAccess, addr: u16, values: &[u32]) -> Result<usize> {
        let mut words = Vec::new();
        self.enc.write(addr, values, &mut words);
        for &w in &words {
            port.write(GPIO_OUT_REG, w)?;
        }
        Ok(words.len())
    }

    /// Registers `regs` in order (ADDR, READ and `gpio_in` each), then a
    /// NOP so that `gpio_in` shows the ID word again.
    ///
    /// # Errors
    ///
    /// A failed register access.
    pub fn read_regs(&mut self, port: &dyn RegisterAccess, regs: &[u16]) -> Result<Vec<u32>> {
        let mut v = Vec::with_capacity(regs.len());
        for &a in regs {
            self.command(port, Op::Addr, u32::from(a))?;
            self.command(port, Op::Read, 0)?;
            v.push(port.read(GPIO_IN_REG)?);
        }
        self.command(port, Op::Nop, 0)?;
        Ok(v)
    }

    /// CTRL with `bits` (`isdb_frontend::control::ctrl`).
    ///
    /// # Errors
    ///
    /// A failed register write.
    pub fn ctrl(&mut self, port: &dyn RegisterAccess, bits: u32) -> Result<()> {
        self.command(port, Op::Ctrl, bits)
    }

    /// The open sequence (module documentation) with the front end's
    /// `params` and the host's `init`. After a failed step the block is
    /// sent CTRL 0 (best effort).
    ///
    /// # Errors
    ///
    /// A failed register access, a `Slots` format this host does not read
    /// ([`RadioError::Chain`]), KP not reading back, or STATUS not showing
    /// the chain selected.
    pub fn open(
        &mut self,
        port: &dyn RegisterAccess,
        params: &isdb_frontend::Params,
        init: &ChainInit,
    ) -> Result<OpenReport> {
        let r = self.open_steps(port, params, init);
        if r.is_err() {
            let _ = self.ctrl(port, 0);
        }
        r
    }

    fn open_steps(
        &mut self,
        port: &dyn RegisterAccess,
        params: &isdb_frontend::Params,
        init: &ChainInit,
    ) -> Result<OpenReport> {
        let mut n = 0usize;
        self.ctrl(port, 0)?;
        n += 1;
        let v = self.read_regs(port, &[reg::VERSION, reg::FEATURES])?;
        n += 5;
        self.version = u16::try_from(v[0]).unwrap_or(u16::MAX);
        self.features = v[1];
        if self.version != cc::SLOTS_VERSION {
            return Err(RadioError::Chain {
                backend: self.backend,
                found: Some(self.version),
                need: cc::SLOTS_VERSION,
            });
        }
        // the front end, as for symbols
        let cmds = self.enc.configure(params);
        for &w in &cmds {
            port.write(GPIO_OUT_REG, w)?;
        }
        n += cmds.len();
        let kp = self.read_regs(port, &[control::reg::KP])?[0];
        n += 3;
        if kp != i32::from(params.kp).cast_unsigned() {
            return Err(self.reply(
                "front end register kp",
                format!("reads {kp:#x} after writing {:#x}", params.kp),
            ));
        }
        n += self.write(
            port,
            reg::CARRIER,
            &[init.carrier.cast_unsigned(), init.layout_lag],
        )?;
        let (mut loaded, mut refused) = (0u32, 0u32);
        if let Some(t) = &init.taps {
            if self.features & features::TAPS != 0 {
                let w: Vec<u32> = t.iter().map(|&p| pack_point(p)).collect();
                n += self.write(port, reg::TAPS, &w)?;
                loaded |= init::TAPS;
            } else {
                refused |= init::TAPS;
            }
        }
        if init.constellation.iter().any(Option::is_some) {
            if self.features & features::CONSTELLATION != 0 {
                let mut mask = 0u32;
                for (m, pts) in init.constellation.iter().enumerate() {
                    let Some(pts) = pts else { continue };
                    let w: Vec<u32> = pts
                        .iter()
                        .take(cc::MAX_POINTS)
                        .map(|&p| pack_point(p))
                        .collect();
                    let a = reg::CONST + u16::try_from(cc::MAX_POINTS * m).expect("< 160");
                    n += self.write(port, a, &w)?;
                    mask |= 1 << m;
                }
                n += self.write(port, reg::CONST_MASK, &[mask])?;
                loaded |= init::CONSTELLATION;
            } else {
                refused |= init::CONSTELLATION;
            }
        }
        if let Some(n0) = init.n0 {
            if self.features & features::N0 != 0 {
                n += self.write(port, reg::N0, &[n0])?;
                loaded |= init::N0;
            } else {
                refused |= init::N0;
            }
        }
        n += self.write(port, reg::INIT, &[loaded])?;
        self.ctrl(port, control::ctrl::START)?;
        self.ctrl(
            port,
            control::ctrl::RUN | control::ctrl::SYMBOLS | control::ctrl::SLOTS,
        )?;
        n += 2;
        let st = self.read_regs(port, &[control::reg::STATUS])?[0];
        n += 3;
        let want = 1 | 2 | cc::STATUS_SLOTS;
        if st & want != want {
            return Err(self.reply(
                "front end STATUS",
                format!(
                    "reads {st:#x} after CTRL RUN|SYMBOLS|SLOTS, not running with the chain's Slots selected"
                ),
            ));
        }
        if refused != 0 {
            warn!(
                backend = self.backend,
                refused,
                features = self.features,
                "the chain's FEATURES {:#x} do not take some of the host's start values (INIT bits {refused:#x}): it starts on its defaults there",
                self.features
            );
        }
        Ok(OpenReport {
            version: self.version,
            features: self.features,
            loaded,
            refused,
            commands: n,
        })
    }

    /// CTRL 0: back to IQ samples.
    ///
    /// # Errors
    ///
    /// A failed register write.
    pub fn release(&mut self, port: &dyn RegisterAccess) -> Result<()> {
        self.ctrl(port, 0)
    }

    /// FEATURES, read (nothing else changes).
    ///
    /// # Errors
    ///
    /// A failed register access.
    pub fn read_features(&mut self, port: &dyn RegisterAccess) -> Result<u32> {
        self.features = self.read_regs(port, &[reg::FEATURES])?[0];
        Ok(self.features)
    }

    /// The counters ([`cc::Counters`]).
    ///
    /// # Errors
    ///
    /// A failed register access.
    pub fn counters(&mut self, port: &dyn RegisterAccess) -> Result<cc::Counters> {
        let v = self.read_regs(port, &cc::Counters::registers())?;
        Ok(cc::Counters::from_words(
            v.try_into().expect("one value per register"),
        ))
    }
}

/// How the slow loop runs.
#[derive(Debug, Clone, Copy)]
pub struct SlowLoopOptions {
    /// Solve at most this often once the first taps are written (the
    /// plan's "every few seconds").
    pub every: Duration,
    /// Poll for the first sums this often (the first solve is at lock).
    pub poll: Duration,
    /// The solve.
    pub solve: Solver,
    /// Symbols per second, for the carrier register in Hz.
    pub symbol_rate_hz: f64,
}

impl SlowLoopOptions {
    /// Every 2 s, polling every 200 ms until the first solve.
    pub fn new(solve: Solver, symbol_rate_hz: f64) -> Self {
        Self {
            every: Duration::from_secs(2),
            poll: Duration::from_millis(200),
            solve,
            symbol_rate_hz,
        }
    }
}

/// What one tick of the slow loop did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tick {
    /// No sums latched yet (no frame with a layout since the start).
    NoSums,
    /// The latched sums are of a frame already solved.
    Same,
    /// The latch moved while the sums were read: tried again later.
    Torn,
    /// The sums did not solve (singular, or empty).
    Singular {
        /// The frame summed.
        frame: u32,
    },
    /// Taps solved from `frame` written and committed.
    Solved {
        /// The frame summed.
        frame: u32,
    },
}

/// The slow loop's totals, for the summary.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SlowLoopReport {
    /// Ticks run.
    pub ticks: u64,
    /// Taps written.
    pub solves: u64,
    /// Sums that did not solve.
    pub singular: u64,
    /// Reads torn by the latch moving.
    pub torn: u64,
    /// The frame of the last taps written.
    pub last_frame: Option<u32>,
    /// The last taps written.
    pub last_taps: Option<Taps>,
    /// The carrier register at the last tick (2⁻³² turn per symbol) and
    /// its extremes over the run.
    pub carrier: Option<i32>,
    /// See `carrier`.
    pub carrier_range: Option<(i32, i32)>,
    /// The error that ended the loop, if one did.
    pub error: Option<String>,
}

/// The equaliser's slow loop (module documentation).
#[derive(Debug, Clone)]
pub struct SlowLoop {
    opts: SlowLoopOptions,
    /// Totals so far.
    pub report: SlowLoopReport,
}

impl SlowLoop {
    /// A loop that has solved nothing.
    pub fn new(opts: SlowLoopOptions) -> Self {
        Self {
            opts,
            report: SlowLoopReport::default(),
        }
    }

    /// The wait before the next tick: [`SlowLoopOptions::poll`] until the
    /// first solve (and after a torn read), then
    /// [`SlowLoopOptions::every`].
    pub fn wait(&self, last: Tick) -> Duration {
        match last {
            Tick::Torn | Tick::NoSums => self.opts.poll,
            _ if self.report.solves == 0 => self.opts.poll,
            _ => self.opts.every,
        }
    }

    /// One tick (module documentation).
    ///
    /// # Errors
    ///
    /// A failed register access.
    pub fn tick(&mut self, c: &mut ChainControl, port: &dyn RegisterAccess) -> Result<Tick> {
        self.report.ticks += 1;
        let v = c.read_regs(
            port,
            &[
                reg::SUMS_FRAME,
                reg::SUMS_COUNT,
                reg::CARRIER,
                reg::TAPS_FROM,
                reg::STATUS,
            ],
        )?;
        let (frame, count, carrier) = (v[0], v[1], v[2].cast_signed());
        self.report.carrier = Some(carrier);
        self.report.carrier_range = Some(
            self.report
                .carrier_range
                .map_or((carrier, carrier), |(a, b)| {
                    (a.min(carrier), b.max(carrier))
                }),
        );
        debug!(
            target: "isdb::chain",
            sums_frame = frame,
            count,
            carrier,
            taps_from = v[3],
            status = v[4],
            "chain: sums of frame {frame:#x} ({count} symbols), carrier register {carrier} ({:+.1} Hz, informational: the FPGA tracks it), taps from {:#x}, status {:#x}",
            cc::carrier_hz(carrier, self.opts.symbol_rate_hz),
            v[3],
            v[4]
        );
        if frame == NONE {
            return Ok(Tick::NoSums);
        }
        if self.report.last_frame == Some(frame) {
            return Ok(Tick::Same);
        }
        let words = c.read_regs(port, &cc::all_sum_registers())?;
        let again = c.read_regs(port, &[reg::SUMS_FRAME])?[0];
        if again != frame {
            self.report.torn += 1;
            debug!(
                target: "isdb::chain",
                frame,
                again,
                "chain: the training sums moved from frame {frame} to {again} while they were read; reading them again"
            );
            return Ok(Tick::Torn);
        }
        let sums = cc::sums_from_words(frame, count, &words);
        let Some(taps) = (self.opts.solve)(&sums) else {
            self.report.singular += 1;
            // the frame is not solved again; the next one may be
            self.report.last_frame = Some(frame);
            warn!(
                target: "isdb::chain",
                frame,
                count,
                "chain: the training sums of frame {frame} ({count} symbols) do not solve; the taps in use stay"
            );
            return Ok(Tick::Singular { frame });
        };
        let w: Vec<u32> = taps.iter().map(|&p| pack_point(p)).collect();
        c.write(port, reg::TAPS, &w)?;
        c.write(port, reg::TAPS_FROM, &[frame])?;
        self.report.solves += 1;
        self.report.last_frame = Some(frame);
        self.report.last_taps = Some(taps);
        info!(
            target: "isdb::chain",
            frame,
            count,
            solves = self.report.solves,
            centre_re = taps[cc::TAPS / 2][0],
            centre_im = taps[cc::TAPS / 2][1],
            carrier_hz = cc::carrier_hz(carrier, self.opts.symbol_rate_hz),
            "chain: equaliser taps solved from frame {frame} ({count} training symbols) written (centre tap {:?} in Q2.14); the chain's carrier register {:+.1} Hz",
            taps[cc::TAPS / 2],
            cc::carrier_hz(carrier, self.opts.symbol_rate_hz)
        );
        Ok(Tick::Solved { frame })
    }
}

/// Run the slow loop on its own thread over `port` (which must not be
/// used by another thread for commands meanwhile: the command protocol is
/// stateful) until `stop`. Its encoder continues from `gpio_out`. A failed
/// register access ends the loop (warned, and kept in
/// [`SlowLoopReport::error`]); the reception goes on with the taps in use.
///
/// # Errors
///
/// The thread could not be spawned.
pub fn spawn_slow_loop(
    backend: &'static str,
    port: Box<dyn RegisterAccess + Send>,
    opts: SlowLoopOptions,
    stop: Arc<AtomicBool>,
) -> std::io::Result<std::thread::JoinHandle<SlowLoopReport>> {
    std::thread::Builder::new()
        .name("chain-slow-loop".into())
        .spawn(move || {
            let mut l = SlowLoop::new(opts);
            let r = (|| -> Result<()> {
                let mut c = ChainControl::attach(backend, port.as_ref())?;
                let mut last = Tick::NoSums;
                loop {
                    let due = Instant::now() + l.wait(last);
                    while Instant::now() < due {
                        if stop.load(Ordering::SeqCst) {
                            return Ok(());
                        }
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    if stop.load(Ordering::SeqCst) {
                        return Ok(());
                    }
                    last = l.tick(&mut c, port.as_ref())?;
                }
            })();
            if let Err(e) = r {
                warn!(
                    target: "isdb::chain",
                    "chain: the equaliser's slow loop stopped: {e}; the taps in use stay"
                );
                l.report.error = Some(e.to_string());
            }
            l.report
        })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use isdb_frontend::control::Identity;
    use isdb_frontend::params::{Design, Params};
    use std::collections::BTreeMap;
    use std::sync::Mutex;

    /// A model of the front end with the chain behind its command pair:
    /// the command decoding of `isdb_frontend::control`, the chain's
    /// registers, a sums latch the test sets, the taps committed by
    /// TAPS_FROM.
    #[derive(Debug, Default)]
    pub(crate) struct Model {
        pub id: u32,
        pub gpio_out: u32,
        pub gpio_in: u32,
        addr: u16,
        staged: u32,
        pub regs: BTreeMap<u16, u32>,
        /// Every CTRL argument, in order.
        pub ctrl: Vec<u32>,
        /// Every register write (address, value), in order.
        pub writes: Vec<(u16, u32)>,
        pub run: bool,
        pub symbols: bool,
        pub slots: bool,
        /// The latch: the frame and its register values.
        pub sums: Option<(u32, u32, Vec<u32>)>,
        /// Move the latch to this frame after this many sum reads (a torn
        /// read).
        pub move_after: Option<(usize, u32)>,
        sum_reads: usize,
        /// Taps in use and the frame they came from.
        pub taps: Option<(Taps, u32)>,
        /// A STATUS that never shows the chain selected.
        pub deaf: bool,
        pub bus_writes: usize,
    }

    impl Model {
        pub fn new(id: u32, version: u16, features: u32) -> Self {
            let mut m = Self {
                id,
                gpio_in: id,
                ..Self::default()
            };
            m.regs.insert(reg::VERSION, u32::from(version));
            m.regs.insert(reg::FEATURES, features);
            m.regs.insert(reg::CARRIER, 0);
            m
        }

        fn store(&mut self, v: u32) {
            self.regs.insert(self.addr, v);
            self.writes.push((self.addr, v));
            if self.addr == reg::TAPS_FROM {
                let t: Taps = std::array::from_fn(|k| {
                    cc::unpack_point(self.regs[&(reg::TAPS + u16::try_from(k).unwrap())])
                });
                self.taps = Some((t, v));
            }
            self.addr += 1;
        }

        fn read_reg(&mut self) -> u32 {
            let a = self.addr;
            if a == control::reg::STATUS {
                return u32::from(self.run)
                    | u32::from(self.symbols) << 1
                    | if self.slots && !self.deaf {
                        cc::STATUS_SLOTS
                    } else {
                        0
                    };
            }
            if a == reg::SUMS_FRAME {
                return self.sums.as_ref().map_or(NONE, |s| s.0);
            }
            if a == reg::SUMS_COUNT {
                return self.sums.as_ref().map_or(0, |s| s.1);
            }
            let sums: Vec<u16> = cc::all_sum_registers();
            if let Some(i) = sums.iter().position(|&r| r == a) {
                self.sum_reads += 1;
                let v = self.sums.as_ref().map_or(0, |s| s.2[i]);
                if let Some((n, f)) = self.move_after
                    && self.sum_reads == n
                    && let Some(s) = self.sums.as_mut()
                {
                    s.0 = f;
                    self.move_after = None;
                }
                return v;
            }
            self.regs.get(&a).copied().unwrap_or(0)
        }

        fn execute(&mut self, w: u32) {
            let arg = (w >> 1) & 0x07FF_FFFF;
            let lo = arg & 0xFFFF;
            self.gpio_in = self.id;
            match (w >> 28) & 7 {
                0 => {}
                1 => self.addr = u16::try_from(lo).unwrap(),
                2 => self.staged = lo,
                3 => self.store(self.staged << 16 | lo),
                4 => self.gpio_in = self.read_reg(),
                5 => {
                    self.ctrl.push(arg);
                    self.run = arg & control::ctrl::RUN != 0;
                    self.symbols = arg & control::ctrl::SYMBOLS != 0;
                    self.slots = arg & control::ctrl::SLOTS != 0;
                }
                6 => self.store(((arg << 5).cast_signed() >> 5).cast_unsigned()),
                op => panic!("{w:#010x}: op {op}"),
            }
        }
    }

    /// The model behind a mutex: usable from the slow loop's thread.
    #[derive(Debug, Clone)]
    pub(crate) struct Mock(pub Arc<Mutex<Model>>);

    impl Mock {
        pub fn new(m: Model) -> Self {
            Self(Arc::new(Mutex::new(m)))
        }
        pub fn get(&self) -> std::sync::MutexGuard<'_, Model> {
            self.0.lock().unwrap()
        }
    }

    impl RegisterAccess for Mock {
        fn read(&self, addr: u32) -> Result<u32> {
            let d = self.get();
            Ok(match addr {
                GPIO_IN_REG => d.gpio_in,
                GPIO_OUT_REG => d.gpio_out,
                _ => panic!("read of {addr:#010x}"),
            })
        }
        fn write(&self, addr: u32, value: u32) -> Result<()> {
            let mut d = self.get();
            assert_eq!(addr, GPIO_OUT_REG, "write of {addr:#010x}");
            assert_eq!(value & 1, d.gpio_out & 1, "bit 0 of gpio_out changed");
            d.bus_writes += 1;
            let toggled = (value ^ d.gpio_out) >> 31 != 0;
            d.gpio_out = value;
            if toggled {
                d.execute(value);
            }
            Ok(())
        }
    }

    pub(crate) fn params() -> Params {
        let mut d = Design::new(40e6, 33.7561e6, 0.03);
        d.taps = 32;
        Params::design(&d).unwrap()
    }

    pub(crate) fn id_word() -> u32 {
        cc::id_word(
            &Identity {
                version: isdb_frontend::params::FORMAT_VERSION,
                taps: 32,
            },
            cc::SLOTS_VERSION,
        )
    }

    fn chain() -> Mock {
        let m = Model::new(id_word(), cc::SLOTS_VERSION, features::ALL);
        let mock = Mock::new(m);
        mock.get().gpio_out = 1; // the bitstream's own bit 0, kept
        mock
    }

    /// A solver that marks the frame in the centre tap.
    fn fake_solve(s: &TrainingSums) -> Option<Taps> {
        if s.count == 0 {
            return None;
        }
        let mut t = cc::Taps::default();
        t[cc::TAPS / 2] = [16_000, i16::try_from(s.frame % 1000).unwrap()];
        t[0] = [i16::try_from(s.r[0][0] % 30_000).unwrap(), 0];
        Some(t)
    }

    fn sums(frame: u32, count: u32) -> (u32, u32, Vec<u32>) {
        let s = TrainingSums {
            frame,
            count,
            r: (0..cc::R_ENTRIES)
                .map(|i| [i64::try_from(i).unwrap() * 3 + 7, -1])
                .collect(),
            p: vec![[1 << 40, -(1 << 41)]; cc::TAPS],
        };
        (frame, count, cc::sums_to_words(&s))
    }

    /// The open sequence on an image with every feature, nothing from the
    /// host: CTRL 0; VERSION, FEATURES; the front end's configuration
    /// with KP read back; CARRIER and LAYOUT_LAG; INIT 0; START; RUN |
    /// SYMBOLS | SLOTS; STATUS. Then release.
    #[test]
    fn open_with_nothing_from_the_host() {
        let p = params();
        let m = chain();
        let mut c = ChainControl::attach("mock", &m).unwrap();
        let init = ChainInit {
            carrier: -1234,
            ..ChainInit::default()
        };
        let r = c.open(&m, &p, &init).unwrap();
        assert_eq!(
            (r.version, r.features, r.loaded, r.refused),
            (1, features::ALL, 0, 0)
        );
        {
            let d = m.get();
            use control::ctrl::{RUN, SLOTS, START, SYMBOLS};
            assert_eq!(d.ctrl, [0, START, RUN | SYMBOLS | SLOTS]);
            assert!(d.run && d.symbols && d.slots);
            assert_eq!(d.regs[&control::reg::KP], i32::from(p.kp).cast_unsigned());
            assert_eq!(d.regs[&reg::CARRIER].cast_signed(), -1234);
            assert_eq!(d.regs[&reg::LAYOUT_LAG], 2);
            assert_eq!(d.regs[&reg::INIT], 0);
            assert!(!d.regs.contains_key(&reg::TAPS), "no taps written");
            assert!(!d.regs.contains_key(&reg::CONST_MASK));
            assert_eq!(d.gpio_in, id_word(), "the ID word after the last read");
            assert_eq!(d.bus_writes, r.commands);
            // CARRIER, LAYOUT_LAG, INIT: the last chain writes, after the
            // front end's banks
            let n = d.writes.len();
            assert_eq!(
                d.writes[n - 3..],
                [
                    (reg::CARRIER, (-1234i32).cast_unsigned()),
                    (reg::LAYOUT_LAG, 2),
                    (reg::INIT, 0)
                ]
            );
        }
        c.release(&m).unwrap();
        assert_eq!(m.get().ctrl.last(), Some(&0));
        assert!(!m.get().slots);
    }

    /// With the host's taps, a constellation and n0: each written and
    /// selected in INIT before START; an image without the features
    /// starts on its defaults and says so.
    #[test]
    fn open_with_the_hosts_start_values() {
        let p = params();
        let mut taps = isdb_frontend::control::chain::Taps::default();
        taps[5] = [16_384, 0];
        taps[6] = [-300, 77];
        let pts: Vec<[i16; 2]> = (0..16).map(|k| [k * 60 - 480, 5 - k]).collect();
        let mut init = ChainInit {
            taps: Some(taps),
            n0: Some(40_000),
            ..ChainInit::default()
        };
        init.constellation[3] = Some(pts.clone());
        let m = chain();
        let mut c = ChainControl::attach("mock", &m).unwrap();
        let r = c.open(&m, &p, &init).unwrap();
        assert_eq!(r.loaded, init::TAPS | init::CONSTELLATION | init::N0);
        assert!(
            r.describe().contains("taps from the host"),
            "{}",
            r.describe()
        );
        {
            let d = m.get();
            for (k, t) in taps.iter().enumerate() {
                let a = reg::TAPS + u16::try_from(k).unwrap();
                assert_eq!(cc::unpack_point(d.regs[&a]), *t);
            }
            for (k, q) in pts.iter().enumerate() {
                let a = reg::CONST + 3 * 32 + u16::try_from(k).unwrap();
                assert_eq!(cc::unpack_point(d.regs[&a]), *q);
            }
            assert_eq!(d.regs[&reg::CONST_MASK], 1 << 3);
            assert_eq!(d.regs[&reg::N0], 40_000);
            assert_eq!(
                d.regs[&reg::INIT],
                init::TAPS | init::CONSTELLATION | init::N0
            );
            // INIT is written before START
            let init_at = d.writes.iter().position(|w| w.0 == reg::INIT).unwrap();
            assert_eq!(init_at, d.writes.len() - 1);
            // the taps are not committed by TAPS_FROM (they are START's)
            assert!(d.taps.is_none());
        }
        // an image without the features
        let m = Mock::new(Model::new(id_word(), 1, features::SUMS));
        let mut c = ChainControl::attach("mock", &m).unwrap();
        let r = c.open(&m, &p, &init).unwrap();
        assert_eq!(
            (r.loaded, r.refused),
            (0, init::TAPS | init::CONSTELLATION | init::N0)
        );
        assert!(r.describe().contains("does not take it"));
        assert!(!m.get().regs.contains_key(&reg::TAPS));
        assert_eq!(m.get().regs[&reg::INIT], 0);
    }

    /// A chain of another Slots format, or one that does not take the
    /// selection, is refused and sent CTRL 0.
    #[test]
    fn open_refuses_another_format_and_a_deaf_block() {
        let p = params();
        let m = Mock::new(Model::new(id_word(), 2, features::ALL));
        let mut c = ChainControl::attach("mock", &m).unwrap();
        let e = c.open(&m, &p, &ChainInit::default()).unwrap_err();
        assert!(
            matches!(
                e,
                RadioError::Chain {
                    found: Some(2),
                    need: 1,
                    ..
                }
            ),
            "{e}"
        );
        assert_eq!(m.get().ctrl, [0, 0]);
        let mut model = Model::new(id_word(), 1, features::ALL);
        model.deaf = true;
        let m = Mock::new(model);
        let mut c = ChainControl::attach("mock", &m).unwrap();
        let e = c.open(&m, &p, &ChainInit::default()).unwrap_err();
        assert!(e.to_string().contains("STATUS"), "{e}");
        assert_eq!(m.get().ctrl.last(), Some(&0));
    }

    /// The slow loop: nothing before the first sums; a frame's sums read,
    /// solved and its taps written and committed (TAPS_FROM last); the
    /// same frame not solved twice; a torn read retried; the carrier
    /// register followed; poll until the first solve, then `every`.
    #[test]
    fn slow_loop_sequence() {
        let m = chain();
        let mut c = ChainControl::attach("mock", &m).unwrap();
        c.open(&m, &params(), &ChainInit::default()).unwrap();
        let opts = SlowLoopOptions::new(fake_solve, 33.7561e6);
        let mut l = SlowLoop::new(opts);
        assert_eq!(l.tick(&mut c, &m).unwrap(), Tick::NoSums);
        assert_eq!(l.wait(Tick::NoSums), opts.poll);
        {
            let mut d = m.get();
            d.sums = Some(sums(42, 6_720));
            d.regs.insert(reg::CARRIER, (-5_000i32).cast_unsigned());
        }
        let writes = m.get().writes.len();
        assert_eq!(l.tick(&mut c, &m).unwrap(), Tick::Solved { frame: 42 });
        {
            let d = m.get();
            let (t, from) = d.taps.unwrap();
            assert_eq!(from, 42);
            assert_eq!(t[5], [16_000, 42]);
            assert_eq!(t[0], [7, 0], "solved from the sums as read");
            // the 11 taps then TAPS_FROM, nothing else written
            let w = &d.writes[writes..];
            assert_eq!(w.len(), 12);
            assert_eq!(w[11], (reg::TAPS_FROM, 42));
            assert!(
                w[..11]
                    .iter()
                    .all(|(a, _)| (reg::TAPS..reg::TAPS + 11).contains(a))
            );
        }
        assert_eq!(l.report.carrier, Some(-5_000));
        assert_eq!(l.wait(Tick::Solved { frame: 42 }), opts.every);
        assert_eq!(l.tick(&mut c, &m).unwrap(), Tick::Same);
        // the latch moves during the read: retried, then solved
        {
            let mut d = m.get();
            d.sums = Some(sums(43, 6_720));
            d.sum_reads = 0;
            d.move_after = Some((100, 44));
        }
        assert_eq!(l.tick(&mut c, &m).unwrap(), Tick::Torn);
        assert_eq!(l.wait(Tick::Torn), opts.poll);
        assert_eq!(
            m.get().taps.unwrap().1,
            42,
            "nothing written on a torn read"
        );
        m.get().sums = Some(sums(44, 6_720));
        assert_eq!(l.tick(&mut c, &m).unwrap(), Tick::Solved { frame: 44 });
        // sums that do not solve: the taps stay, the frame is not retried
        m.get().sums = Some(sums(45, 0));
        assert_eq!(l.tick(&mut c, &m).unwrap(), Tick::Singular { frame: 45 });
        assert_eq!(l.tick(&mut c, &m).unwrap(), Tick::Same);
        assert_eq!(m.get().taps.unwrap().1, 44);
        assert_eq!(
            (l.report.solves, l.report.torn, l.report.singular),
            (2, 1, 1)
        );
        m.get().regs.insert(reg::CARRIER, 9_000);
        l.tick(&mut c, &m).unwrap();
        assert_eq!(l.report.carrier_range, Some((-5_000, 9_000)));
        // the ID word in gpio_in after every read sequence
        assert_eq!(m.get().gpio_in, id_word());
    }

    /// The timer thread: solves the latched frames as they come and stops
    /// at once on `stop`, its encoder continuing the open's toggle bit.
    #[test]
    fn slow_loop_thread() {
        let m = chain();
        let mut c = ChainControl::attach("mock", &m).unwrap();
        c.open(&m, &params(), &ChainInit::default()).unwrap();
        m.get().sums = Some(sums(7, 6_720));
        let stop = Arc::new(AtomicBool::new(false));
        let opts = SlowLoopOptions {
            every: Duration::from_millis(20),
            poll: Duration::from_millis(5),
            ..SlowLoopOptions::new(fake_solve, 33.7561e6)
        };
        let h = spawn_slow_loop("mock", Box::new(m.clone()), opts, Arc::clone(&stop)).unwrap();
        let t = Instant::now();
        while m.get().taps.is_none_or(|t| t.1 != 7) {
            assert!(t.elapsed() < Duration::from_secs(10), "no solve");
            std::thread::sleep(Duration::from_millis(2));
        }
        m.get().sums = Some(sums(8, 6_720));
        while m.get().taps.is_none_or(|t| t.1 != 8) {
            assert!(t.elapsed() < Duration::from_secs(10), "no second solve");
            std::thread::sleep(Duration::from_millis(2));
        }
        stop.store(true, Ordering::SeqCst);
        let r = h.join().unwrap();
        assert_eq!(r.solves, 2);
        assert_eq!(r.last_frame, Some(8));
        assert!(r.error.is_none());
        // the main thread takes over the commands after the join
        let mut c = ChainControl::attach("mock", &m).unwrap();
        c.release(&m).unwrap();
        assert_eq!(m.get().ctrl.last(), Some(&0));
    }

    /// A register access that fails ends the loop with the error kept.
    #[test]
    fn slow_loop_thread_ends_on_an_error() {
        struct Broken;
        impl RegisterAccess for Broken {
            fn read(&self, _: u32) -> Result<u32> {
                Ok(0)
            }
            fn write(&self, _: u32, _: u32) -> Result<()> {
                Err(RadioError::io("write", std::io::Error::other("link down")))
            }
        }
        let opts = SlowLoopOptions {
            poll: Duration::from_millis(1),
            ..SlowLoopOptions::new(fake_solve, 33.7561e6)
        };
        let stop = Arc::new(AtomicBool::new(false));
        let r = spawn_slow_loop("mock", Box::new(Broken), opts, stop)
            .unwrap()
            .join()
            .unwrap();
        assert!(r.error.unwrap().contains("write"));
        assert_eq!(r.solves, 0);
    }
}
