//! Driving the ISDB-S3 receive chain in a radio's FPGA
//! ([`crate::Offload::RxSlots`]), device-neutral: the open sequence and
//! the equaliser's slow loop, through the command protocol of
//! `isdb_frontend::control` on any [`RegisterAccess`] (libiio's register
//! access on a Pluto, a model of the block in the tests).
//!
//! The register map is `isdb_frontend::control::chain`'s (the HDL's,
//! `fpga/isdb_fpga/chain/top.py`); this module uses its names only.
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
//! [`spawn_slow_loop`]): FEATURES once; then each tick SUMS_FRAME,
//! SUMS_COUNT, CARRIER, TAPS_FROM, the chain's STATUS and FRAME read (and
//! SUMS_CAPTURE with [`features::SUMS_CAPTURE`]). With the capture: a
//! tick that has not armed one writes SUMS_CAPTURE (the next frame end
//! latches the sums and holds them); the next ticks poll it until VALID;
//! then the 308 sum registers are read (about 15 frames through iiod: the
//! hold keeps them one frame's) and SUMS_FRAME again, which must be the
//! same (else warned and counted as torn). Without it (older images) the
//! latch moves every frame: a frame's sums not solved yet are read, then
//! SUMS_FRAME again (a different value: the latch moved during the read;
//! tried again at the next tick). Then the solve (the caller's [`Solver`]:
//! `isdbs3_phy::chain::eq::solve_training`, the model's `f64` solve and
//! Q2.14 rounding), the [`Gate`], the 11 taps and TAPS_FROM (which commits
//! them). Until the first solve, and while a capture is pending, it polls
//! every [`SlowLoopOptions::poll`]; then every [`SlowLoopOptions::every`].
//! The carrier register is read every tick and reported (informational:
//! the FPGA tracks the carrier itself). Each read set of sums is logged at
//! debug level (frame, count, a digest of the 308 words, the centre's
//! power and correlation, the solved taps), so that a bad solve can be
//! traced to its sums.
//!
//! **Verified reads** (issues #27, #29): libiio 0.x reads a register as
//! two requests to the device's `direct_reg_access` (the address written,
//! then the value read), and every client of the device shares that one
//! address: a read by another thread, process or host (Tezuka's
//! `api_controller.sh` polls the ADC status register 0x80000088 every few
//! seconds) that lands between the two makes `gpio_in` return the other
//! register (almost always 0). So every register is read until two
//! `gpio_in` reads in a row, each with its own address write, agree
//! ([`ChainControl::read_regs`], [`settled`]; READ is not sent again: the
//! value stays on `readback` until the next command), at most
//! [`MAX_READS`] times; a register that never settles is [`Unsettled`]
//! (the tick is [`Tick::Torn`]). `gpio_out` is read the same way when the
//! encoder attaches. Both reads of one word are hit only if two foreign
//! reads land within the few milliseconds of one iiod round trip.
//!
//! **Checks** on the sums before their solve is written ([`SumsCheck`],
//! [`check_sums`], [`fit`]; a failed one is [`Tick::CaptureRejected`],
//! counted per reason in [`SlowLoopReport::captures_rejected_by`]): R's
//! entries are sums of `conj(x_r) · x_c` over the same training symbols
//! with the window shifted by one per row, so its diagonal is real
//! (exactly: imaginary part at most 10⁻⁶ of the mean diagonal) and nearly
//! constant (spread at most 8 %), and R is near-Toeplitz (`|R[r][c] −
//! R[r+1][c+1]|` at most 4 % of the mean diagonal); and the solved taps
//! cannot reproduce more of the references' energy than there is ([`fit`]
//! at most 1.01). On 754 frames of the chain's model over a BS7 capture
//! (`examples/sumscheck` of `isdb-cli`, the build 8 bench's `after.sym`)
//! the spread was 4.1–4.6 % (a ramp along the diagonal, the training
//! runs' ends), Toeplitz 1.7–2.2 %, the fit 0.971–0.973 (MER 15.3–15.7 dB
//! on the training symbols), the imaginary part 0. One word of 308 read
//! as 0 (#29's foreign value) moves an entry by up to 2³² ≈ 4.3·10⁹
//! against a diagonal of 6.5·10⁹: the checks caught 82 % of such sets
//! among R's words (the rest moved the taps by at most 7 %) and 73 % among
//! p's; the [`Gate`] is the second line for p.
//!
//! **Gate**: a solve is not written (the taps in use stay; counted in
//! [`SlowLoopReport::solves_rejected`]) when the normal equations' relative
//! residual `‖R w − p‖ / ‖p‖` of its Q2.14 taps exceeds
//! [`Gate::residual`] (a failed or clamped solve), or when its centre
//! tap's magnitude differs from the last written set's by more than
//! [`Gate::centre`] (relative) — unless the solve before it was rejected
//! for the same reason and this one is within the bound of that one: a
//! lasting change of the signal's level is then taken one solve later.
//! The board's one bad solve in twelve (build 7: centre 18 691 against
//! 17 325–17 848, every codeword of the next 81 frames failed) is 7.8 %
//! off; the good ones moved by up to 3 %.

use crate::{RadioError, Result};
use isdb_frontend::control::chain::{
    self as cc, ChainInit, NONE, Taps, TrainingSums, capture, features, init, pack_point, reg,
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

/// Register `addr` of the core (`gpio_in`, `gpio_out`) read until two
/// reads in a row agree (module documentation, "Verified reads"): the
/// value and the reads it took, or every read when none of [`MAX_READS`]
/// did.
///
/// # Errors
///
/// A failed register access.
pub fn settled(
    port: &dyn RegisterAccess,
    addr: u32,
) -> Result<std::result::Result<(u32, usize), Vec<u32>>> {
    let mut reads = vec![port.read(addr)?];
    while reads.len() < MAX_READS {
        let x = port.read(addr)?;
        let agree = reads.last() == Some(&x);
        reads.push(x);
        if agree {
            return Ok(Ok((x, reads.len())));
        }
    }
    Ok(Err(reads))
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
    /// Registers read (verified) so far.
    pub words_read: u64,
    /// Of which the first two `gpio_in` reads disagreed (read again: a
    /// foreign value caught, module documentation "Verified reads").
    pub rereads: u64,
}

/// `gpio_in` reads of one register at most before it is
/// [`Unsettled`]: two foreign reads of the device's `direct_reg_access`
/// within the few milliseconds of one read are needed to make two reads
/// in a row agree on a wrong value, and more than six in a row to
/// exhaust this.
pub const MAX_READS: usize = 8;

/// A register whose `gpio_in` reads never agreed twice in a row in
/// [`MAX_READS`] reads (something else keeps moving the device's
/// `direct_reg_access` address, or the register itself changes that
/// fast).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unsettled {
    /// The chain register.
    pub addr: u16,
    /// Every `gpio_in` read, in order.
    pub reads: Vec<u32>,
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
        // gpio_out carries the bitstream's own bit 0 and the toggle bit the
        // encoder continues: a foreign value here would corrupt every
        // command
        let out = settled(port, GPIO_OUT_REG)?.map_err(|reads| RadioError::Reply {
            backend,
            what: "gpio_out".into(),
            detail: format!(
                "read {reads:#x?}: never the same twice in a row (another client of the device's direct_reg_access moving its address: docs/receiving.md)"
            ),
        })?;
        Ok(Self {
            backend,
            enc: Encoder::new(out.0),
            features: 0,
            version: 0,
            words_read: 0,
            rereads: 0,
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

    /// Registers `regs` in order, each verified (module documentation,
    /// "Verified reads": ADDR, READ, then `gpio_in` until two reads in a
    /// row agree), then a NOP so that `gpio_in` shows the ID word again. A
    /// register that never reads the same twice in [`MAX_READS`] is an
    /// error ([`ChainControl::read_regs_checked`] returns it instead).
    ///
    /// # Errors
    ///
    /// A failed register access, or a register whose reads never agree.
    pub fn read_regs(&mut self, port: &dyn RegisterAccess, regs: &[u16]) -> Result<Vec<u32>> {
        self.read_regs_checked(port, regs)?.map_err(|u| {
            self.reply(
                &format!("chain register {:#05x}", u.addr),
                format!(
                    "gpio_in read {:#x?}: never the same twice in a row (another client of the device's direct_reg_access moving its address: docs/receiving.md)",
                    u.reads
                ),
            )
        })
    }

    /// [`ChainControl::read_regs`], a register that never settles
    /// returned as [`Unsettled`] (the registers after it are not read).
    ///
    /// # Errors
    ///
    /// A failed register access.
    pub fn read_regs_checked(
        &mut self,
        port: &dyn RegisterAccess,
        regs: &[u16],
    ) -> Result<std::result::Result<Vec<u32>, Unsettled>> {
        let mut v = Vec::with_capacity(regs.len());
        let mut bad = None;
        for &a in regs {
            self.command(port, Op::Addr, u32::from(a))?;
            self.command(port, Op::Read, 0)?;
            match self.settled(port)? {
                Ok(x) => v.push(x),
                Err(reads) => {
                    bad = Some(Unsettled { addr: a, reads });
                    break;
                }
            }
        }
        self.command(port, Op::Nop, 0)?;
        Ok(bad.map_or(Ok(v), Err))
    }

    /// `gpio_in` after a READ, read until two reads in a row agree (each
    /// read is its own `direct_reg_access` address write and read on a
    /// Pluto; the value stays on `readback` until the next command, so
    /// READ is not sent again): the value, or every read when none of
    /// [`MAX_READS`] did.
    fn settled(&mut self, port: &dyn RegisterAccess) -> Result<std::result::Result<u32, Vec<u32>>> {
        let r = settled(port, GPIO_IN_REG)?;
        self.words_read += 1;
        if r.as_ref().map_or(true, |&(_, n)| n > 2) {
            self.rereads += 1;
        }
        Ok(r.map(|(x, _)| x))
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

/// The sanity gate before taps are written (module documentation).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Gate {
    /// Largest relative change of the centre tap's magnitude from the last
    /// written set.
    pub centre: f64,
    /// Largest relative residual `‖R w − p‖ / ‖p‖` of the solved taps.
    pub residual: f64,
}

impl Default for Gate {
    /// 5 % (0.42 dB) for the centre tap; 5 % for the residual (Q2.14
    /// rounding leaves about 10⁻⁴).
    fn default() -> Self {
        Self {
            centre: 0.05,
            residual: 0.05,
        }
    }
}

/// Why a solve was not written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rejected {
    /// The normal equations' residual is above [`Gate::residual`].
    Residual,
    /// The centre tap moved by more than [`Gate::centre`].
    Centre,
}

/// The normal equations' relative residual `‖R w − p‖ / ‖p‖` of `taps`
/// (Q2.14) on `s` (R Hermitian from its upper triangle, as the solve
/// fills it); infinite for `p = 0`.
pub fn residual(s: &TrainingSums, taps: &Taps) -> f64 {
    #[allow(clippy::cast_precision_loss)]
    let c = |v: [i64; 2]| [v[0] as f64, v[1] as f64];
    let scale = f64::from(1u32 << 14);
    let w: Vec<[f64; 2]> = taps
        .iter()
        .map(|t| [f64::from(t[0]) / scale, f64::from(t[1]) / scale])
        .collect();
    let mul = |x: [f64; 2], y: [f64; 2]| [x[0] * y[0] - x[1] * y[1], x[0] * y[1] + x[1] * y[0]];
    // R w over the upper triangle: entry (r, col) in row r, its conjugate
    // in row col
    let mut rw = [[0.0; 2]; cc::TAPS];
    let pairs = (0..cc::TAPS).flat_map(|r| (r..cc::TAPS).map(move |col| (r, col)));
    for (&v, (r, col)) in s.r.iter().zip(pairs) {
        let v = c(v);
        let x = mul(v, w[col]);
        rw[r] = [rw[r][0] + x[0], rw[r][1] + x[1]];
        if col != r {
            let x = mul([v[0], -v[1]], w[r]);
            rw[col] = [rw[col][0] + x[0], rw[col][1] + x[1]];
        }
    }
    let (mut num, mut den) = (0.0, 0.0);
    for (x, &p) in rw.iter().zip(&s.p) {
        let b = c(p);
        num += (x[0] - b[0]).powi(2) + (x[1] - b[1]).powi(2);
        den += b[0] * b[0] + b[1] * b[1];
    }
    if den == 0.0 {
        f64::INFINITY
    } else {
        (num / den).sqrt()
    }
}

/// A 64-bit FNV-1a digest of the sum registers' words (for the log).
pub fn digest(words: &[u32]) -> u64 {
    words
        .iter()
        .flat_map(|w| w.to_le_bytes())
        .fold(0xcbf2_9ce4_8422_2325u64, |h, b| {
            (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
        })
}

/// Index of `R[r][c]` (r ≤ c) in the upper-triangle order: row by row.
fn r_index(r: usize, c: usize) -> usize {
    debug_assert!(r <= c && c < cc::TAPS);
    r * cc::TAPS - r * (r + 1) / 2 + c
}

#[allow(clippy::cast_precision_loss)]
fn re_im(v: [i64; 2]) -> [f64; 2] {
    [v[0] as f64, v[1] as f64]
}

/// The structure of R in one frame's sums (module documentation,
/// "Checks"): R's entries are sums of `conj(x_r) · x_c` over the same
/// training symbols with the window shifted, so its diagonal is real and
/// nearly constant and `R[r][c] ≈ R[r + 1][c + 1]` (only the symbols at
/// the ends of each run of training symbols differ).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SumsShape {
    /// The mean of R's diagonal (real parts).
    pub diagonal: f64,
    /// The largest `|Im R[k][k]|` (exactly 0 for sums of `conj(x) · x`)
    /// over the mean diagonal, and its k.
    pub imaginary: (usize, f64),
    /// The largest `|R[k][k] − mean|` over the mean diagonal, and its k.
    pub spread: (usize, f64),
    /// The largest `|R[r][c] − R[r + 1][c + 1]|` (complex) over the mean
    /// diagonal, r ≤ c < 10, and its (r, c).
    pub toeplitz: ((usize, usize), f64),
}

/// [`SumsShape`] of `s`; `None` without training symbols, of the wrong
/// shape, or with a mean diagonal that is not positive.
pub fn sums_shape(s: &TrainingSums) -> Option<SumsShape> {
    let n = cc::TAPS;
    if s.count == 0 || s.r.len() != cc::R_ENTRIES || s.p.len() != n {
        return None;
    }
    let at = |r: usize, c: usize| re_im(s.r[r_index(r, c)]);
    #[allow(clippy::cast_precision_loss)]
    let mean = (0..n).map(|k| at(k, k)[0]).sum::<f64>() / n as f64;
    if mean.is_nan() || mean <= 0.0 {
        return None;
    }
    let worst = |it: &mut dyn Iterator<Item = (usize, f64)>| {
        it.fold((0, 0.0), |b, x| if x.1 > b.1 { x } else { b })
    };
    let imaginary = worst(&mut (0..n).map(|k| (k, at(k, k)[1].abs() / mean)));
    let spread = worst(&mut (0..n).map(|k| (k, (at(k, k)[0] - mean).abs() / mean)));
    let mut toeplitz = ((0, 0), 0.0);
    for r in 0..n - 1 {
        for c in r..n - 1 {
            let (a, b) = (at(r, c), at(r + 1, c + 1));
            let d = (a[0] - b[0]).hypot(a[1] - b[1]) / mean;
            if d > toeplitz.1 {
                toeplitz = ((r, c), d);
            }
        }
    }
    Some(SumsShape {
        diagonal: mean,
        imaginary,
        spread,
        toeplitz,
    })
}

/// The energy of the references (`Σ |d|²`) the chain trains on, per
/// training symbol: its sync references `±724 ± 724j` and its pilots, the
/// nominal points of a unit-power constellation, both in Q.10 (the model,
/// `isdbs3_phy::chain`, "Stage 4"): 2²⁰ within 0.1 %.
pub const REFERENCE_ENERGY: f64 = 1_048_576.0;

/// The share of the references' energy that `taps` (Q2.14) reproduce on
/// `s`: `1 − J(w) / D`, with `J(w) = wᴴ R w − 2 Re(wᴴ p) + D` the squared
/// error of the equalised training symbols the sums give and `D = count ·`
/// [`REFERENCE_ENERGY`]. `J ≥ 0` for every `w` when R, p and D are sums
/// over the same symbols, so this is at most 1 for any taps; for the
/// least-squares taps `10 log10(1 / (1 − fit))` is their MER on the
/// training symbols (about 12 dB on the board's BS-14 at 13 dB Es/N0). A
/// word read wrong in p breaks it: an error δ in `p[k]` adds about
/// `|δ|² / R[k][k]` (`2³²` against `R ≈ 6.6·10⁹`: 0.4 of D).
pub fn fit(s: &TrainingSums, taps: &Taps) -> f64 {
    let n = cc::TAPS;
    if s.count == 0 || s.r.len() != cc::R_ENTRIES || s.p.len() != n {
        return f64::NAN;
    }
    let scale = f64::from(1u32 << 14);
    let w: Vec<[f64; 2]> = taps
        .iter()
        .map(|t| [f64::from(t[0]) / scale, f64::from(t[1]) / scale])
        .collect();
    // R[r][c] for any r, c from the upper triangle (Hermitian)
    let at = |r: usize, c: usize| {
        if r <= c {
            re_im(s.r[r_index(r, c)])
        } else {
            let v = re_im(s.r[r_index(c, r)]);
            [v[0], -v[1]]
        }
    };
    // wᴴ R w (real for a Hermitian R) and Re(wᴴ p)
    let mut quad = 0.0;
    for (r, wr) in w.iter().enumerate() {
        for (c, wc) in w.iter().enumerate() {
            let a = at(r, c);
            // conj(w_r) · R[r][c] · w_c, real part
            let x = [a[0] * wc[0] - a[1] * wc[1], a[0] * wc[1] + a[1] * wc[0]];
            quad += wr[0] * x[0] + wr[1] * x[1];
        }
    }
    let cross: f64 = w
        .iter()
        .zip(&s.p)
        .map(|(wk, &p)| {
            let p = re_im(p);
            wk[0] * p[0] + wk[1] * p[1]
        })
        .sum();
    let d = f64::from(s.count) * REFERENCE_ENERGY;
    1.0 - (quad - 2.0 * cross + d) / d
}

/// Bounds on the training sums before their solve is written (module
/// documentation, "Checks"; the defaults' reasoning there).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SumsCheck {
    /// Largest `|Im R[k][k]|` over the mean diagonal.
    pub imaginary: f64,
    /// Largest `|R[k][k] − mean|` over the mean diagonal.
    pub spread: f64,
    /// Largest `|R[r][c] − R[r + 1][c + 1]|` over the mean diagonal.
    pub toeplitz: f64,
    /// Largest [`fit`] of the solved taps.
    pub fit: f64,
}

impl Default for SumsCheck {
    /// Imaginary diagonal 10⁻⁶, spread 8 %, Toeplitz 4 %, fit 1.01
    /// (module documentation, "Checks").
    fn default() -> Self {
        Self {
            imaginary: 1e-6,
            spread: 0.08,
            toeplitz: 0.04,
            fit: 1.01,
        }
    }
}

/// Why a frame's training sums were not solved, or their solve not
/// written ([`SumsCheck`]).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SumsFault {
    /// No training symbols, or a diagonal that is not positive.
    Empty,
    /// `R[k][k]` has an imaginary part (this share of the mean diagonal).
    Imaginary {
        /// The entry.
        k: usize,
        /// `|Im R[k][k]|` over the mean diagonal.
        share: f64,
    },
    /// `R[k][k]` is off the mean diagonal by this share.
    Spread {
        /// The entry.
        k: usize,
        /// `|R[k][k] − mean|` over the mean.
        share: f64,
    },
    /// `R[r][c]` differs from `R[r + 1][c + 1]` by this share of the mean
    /// diagonal.
    Toeplitz {
        /// The row.
        r: usize,
        /// The column.
        c: usize,
        /// `|R[r][c] − R[r + 1][c + 1]|` over the mean diagonal.
        share: f64,
    },
    /// The solved taps reproduce more of the references' energy than any
    /// taps can ([`fit`]).
    Fit {
        /// Their [`fit`].
        fit: f64,
    },
}

impl SumsFault {
    /// A short name (the report's count per reason).
    pub fn name(&self) -> &'static str {
        match self {
            Self::Empty => "empty",
            Self::Imaginary { .. } => "imaginary diagonal",
            Self::Spread { .. } => "diagonal spread",
            Self::Toeplitz { .. } => "not Toeplitz",
            Self::Fit { .. } => "fit above 1",
        }
    }
}

impl std::fmt::Display for SumsFault {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match *self {
            Self::Empty => write!(f, "no training symbols, or R's diagonal is not positive"),
            Self::Imaginary { k, share } => write!(
                f,
                "R[{k}][{k}] has an imaginary part of {share:.2e} of the mean diagonal (a sum of |x|² is real)"
            ),
            Self::Spread { k, share } => write!(
                f,
                "R[{k}][{k}] is {:.2} % off the mean diagonal",
                100.0 * share
            ),
            Self::Toeplitz { r, c, share } => write!(
                f,
                "R[{r}][{c}] and R[{}][{}] differ by {:.2} % of the mean diagonal (R is near-Toeplitz)",
                r + 1,
                c + 1,
                100.0 * share
            ),
            Self::Fit { fit } => write!(
                f,
                "the solved taps would reproduce {:.1} % of the training references' energy (at most 100 % for consistent sums: p does not belong to R)",
                100.0 * fit
            ),
        }
    }
}

/// R's structure of `s` against `check` (before the solve).
pub fn check_sums(s: &TrainingSums, check: &SumsCheck) -> Option<SumsFault> {
    let Some(sh) = sums_shape(s) else {
        return Some(SumsFault::Empty);
    };
    if sh.imaginary.1 > check.imaginary {
        return Some(SumsFault::Imaginary {
            k: sh.imaginary.0,
            share: sh.imaginary.1,
        });
    }
    if sh.spread.1 > check.spread {
        return Some(SumsFault::Spread {
            k: sh.spread.0,
            share: sh.spread.1,
        });
    }
    if sh.toeplitz.1 > check.toeplitz {
        let (r, c) = sh.toeplitz.0;
        return Some(SumsFault::Toeplitz {
            r,
            c,
            share: sh.toeplitz.1,
        });
    }
    None
}

fn magnitude(t: [i16; 2]) -> f64 {
    f64::from(t[0]).hypot(f64::from(t[1]))
}

/// Registers that identify the held capture besides SUMS_FRAME: read with
/// it before the 308 sum registers and again after them, all equal or the
/// read is torn. None on today's map; with the HDL's capture sequence
/// number (in progress) its register goes here.
const HELD_EXTRA: &[u16] = &[];

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
    /// The sanity gate (`None`: every solve is written).
    pub gate: Option<Gate>,
    /// The checks on the sums themselves (`None`: none).
    pub check: Option<SumsCheck>,
}

impl SlowLoopOptions {
    /// Every 2 s, polling every 200 ms until the first solve; the default
    /// [`Gate`] and [`SumsCheck`].
    pub fn new(solve: Solver, symbol_rate_hz: f64) -> Self {
        Self {
            every: Duration::from_secs(2),
            poll: Duration::from_millis(200),
            solve,
            symbol_rate_hz,
            gate: Some(Gate::default()),
            check: Some(SumsCheck::default()),
        }
    }
}

/// What one tick of the slow loop did.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Tick {
    /// No sums latched yet (no frame with a layout since the start).
    NoSums,
    /// The latched sums are of a frame already solved.
    Same,
    /// The latch moved while the sums were read, or a register's reads
    /// never agreed ([`Unsettled`]): tried again later.
    Torn,
    /// The sums of `frame` failed a [`SumsCheck`]: not solved, or their
    /// solve not written (the taps in use stay).
    CaptureRejected {
        /// The frame summed.
        frame: u32,
        /// Why.
        why: SumsFault,
    },
    /// A capture armed ([`reg::SUMS_CAPTURE`] written) or still waiting
    /// for its frame end.
    Capturing,
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
    /// Taps solved from `frame` not written: the [`Gate`] rejected them
    /// (the taps in use stay).
    Rejected {
        /// The frame summed.
        frame: u32,
        /// Why.
        why: Rejected,
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
    /// Reads torn by the latch moving, or by a register whose reads never
    /// agreed (`unsettled`).
    pub torn: u64,
    /// Of `torn`, the reads with a register that never read the same twice
    /// in a row ([`Unsettled`]).
    pub unsettled: u64,
    /// Registers read (verified), and of them the ones whose first two
    /// `gpio_in` reads disagreed (a foreign value caught and read again:
    /// module documentation, "Verified reads").
    pub words_read: u64,
    /// See `words_read`.
    pub rereads: u64,
    /// Captures whose sums failed a [`SumsCheck`] (not solved or not
    /// written), and how many for each reason ([`SumsFault::name`]).
    pub captures_rejected: u64,
    /// See `captures_rejected`.
    pub captures_rejected_by: std::collections::BTreeMap<String, u64>,
    /// Solves the [`Gate`] rejected (not written).
    pub solves_rejected: u64,
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
    /// The image holds captured sums ([`features::SUMS_CAPTURE`]; read at
    /// the first tick).
    capture: Option<bool>,
    /// A capture is armed and not read yet.
    armed: bool,
    /// The centre tap's magnitude of the last solve the gate rejected for
    /// it, if the last solve was that.
    pending_centre: Option<f64>,
}

impl SlowLoop {
    /// A loop that has solved nothing.
    pub fn new(opts: SlowLoopOptions) -> Self {
        Self {
            opts,
            report: SlowLoopReport::default(),
            capture: None,
            armed: false,
            pending_centre: None,
        }
    }

    /// The wait before the next tick: [`SlowLoopOptions::poll`] until the
    /// first solve (and after a torn read), then
    /// [`SlowLoopOptions::every`].
    pub fn wait(&self, last: Tick) -> Duration {
        match last {
            Tick::Torn | Tick::NoSums | Tick::Capturing => self.opts.poll,
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
        let capture = match self.capture {
            Some(x) => x,
            None => {
                let f = c.read_regs(port, &[reg::FEATURES])?[0];
                let x = f & features::SUMS_CAPTURE != 0;
                self.capture = Some(x);
                x
            }
        };
        let mut regs = vec![
            reg::SUMS_FRAME,
            reg::SUMS_COUNT,
            reg::CARRIER,
            reg::TAPS_FROM,
            reg::STATUS,
            reg::FRAME,
        ];
        // last: compared again after the sums are read
        regs.extend_from_slice(HELD_EXTRA);
        if capture {
            // first: VALID read before the frame it holds
            regs.insert(0, reg::SUMS_CAPTURE);
        }
        let read = match self.read_checked(c, port, &regs)? {
            Ok(r) => r,
            Err(t) => return Ok(t),
        };
        let (cap, v) = if capture {
            (Some(read[0]), &read[1..])
        } else {
            (None, &read[..])
        };
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
        if let Some(cap) = cap {
            if !self.armed {
                c.write(port, reg::SUMS_CAPTURE, &[1])?;
                self.armed = true;
                return Ok(Tick::Capturing);
            }
            if cap & capture::VALID == 0 {
                return Ok(Tick::Capturing);
            }
            // held: read it now; the next tick arms another
            self.armed = false;
        }
        if frame == NONE {
            return Ok(Tick::NoSums);
        }
        if self.report.last_frame == Some(frame) {
            return Ok(Tick::Same);
        }
        let words = match self.read_checked(c, port, &cc::all_sum_registers())? {
            Ok(w) => w,
            Err(t) => {
                // the capture still holds: read it again at the next tick
                self.armed = capture;
                return Ok(t);
            }
        };
        let held: Vec<u16> = std::iter::once(reg::SUMS_FRAME)
            .chain(HELD_EXTRA.iter().copied())
            .collect();
        let after = match self.read_checked(c, port, &held)? {
            Ok(w) => w,
            Err(t) => {
                self.armed = capture;
                return Ok(t);
            }
        };
        let again = after[0];
        if again != frame || after[1..] != v[v.len() - HELD_EXTRA.len()..] {
            self.report.torn += 1;
            if capture {
                warn!(
                    target: "isdb::chain",
                    frame,
                    again,
                    "chain: the held training sums moved from frame {frame} to {again} while they were read (the capture did not hold); capturing again"
                );
            } else {
                debug!(
                    target: "isdb::chain",
                    frame,
                    again,
                    "chain: the training sums moved from frame {frame} to {again} while they were read; reading them again"
                );
            }
            return Ok(Tick::Torn);
        }
        let sums = cc::sums_from_words(frame, count, &words);
        let centre = cc::TAPS / 2;
        let rc = r_index(centre, centre);
        let shape = sums_shape(&sums);
        let fault = self.opts.check.and_then(|k| check_sums(&sums, &k));
        let solved = if fault.is_none() {
            (self.opts.solve)(&sums)
        } else {
            None
        };
        let fitted = solved.map(|t| fit(&sums, &t));
        debug!(
            target: "isdb::chain",
            frame,
            count,
            digest = digest(&words),
            "chain: training sums of frame {frame} ({count} symbols): digest {:016x}, R[{centre}][{centre}] {:?}, p[{centre}] {:?}; R's diagonal {:.4e}, spread {:.2e}, Toeplitz {:.2e}, imaginary {:.1e}; solved taps {:?}, fit {:?} (MER {:.2} dB on the training symbols)",
            digest(&words),
            sums.r[rc],
            sums.p[centre],
            shape.map_or(f64::NAN, |s| s.diagonal),
            shape.map_or(f64::NAN, |s| s.spread.1),
            shape.map_or(f64::NAN, |s| s.toeplitz.1),
            shape.map_or(f64::NAN, |s| s.imaginary.1),
            solved,
            fitted,
            fitted.map_or(f64::NAN, |f| -10.0 * (1.0 - f).log10())
        );
        let fault = fault.or_else(|| {
            let (k, f) = (self.opts.check?, fitted?);
            (f.is_nan() || f > k.fit).then_some(SumsFault::Fit { fit: f })
        });
        if let Some(why) = fault {
            self.report.captures_rejected += 1;
            *self
                .report
                .captures_rejected_by
                .entry(why.name().to_string())
                .or_default() += 1;
            // the frame is not solved again; the next capture may be
            self.report.last_frame = Some(frame);
            warn!(
                target: "isdb::chain",
                frame,
                count,
                rejected = self.report.captures_rejected,
                reason = why.name(),
                "chain: the training sums of frame {frame} are not used: {why} (a register read wrong, or not the sums of one frame); the taps in use stay"
            );
            return Ok(Tick::CaptureRejected { frame, why });
        }
        let Some(taps) = solved else {
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
        if let Some(why) = self.gate(&sums, &taps) {
            self.report.solves_rejected += 1;
            // the frame is not solved again; the next one may be
            self.report.last_frame = Some(frame);
            warn!(
                target: "isdb::chain",
                frame,
                count,
                rejected = self.report.solves_rejected,
                "chain: the taps solved from frame {frame} (centre tap {:?}, residual {:.2e}) are not written ({}); the taps in use stay",
                taps[centre],
                residual(&sums, &taps),
                match why {
                    Rejected::Residual => "the normal equations' residual is too large".to_string(),
                    Rejected::Centre => format!(
                        "the centre tap moved from {:?}",
                        self.report.last_taps.map(|t| t[centre])
                    ),
                }
            );
            return Ok(Tick::Rejected { frame, why });
        }
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

    /// `regs` read verified ([`ChainControl::read_regs_checked`]), the
    /// read counts kept; a register that never settles is a
    /// [`Tick::Torn`] (counted, warned).
    fn read_checked(
        &mut self,
        c: &mut ChainControl,
        port: &dyn RegisterAccess,
        regs: &[u16],
    ) -> Result<std::result::Result<Vec<u32>, Tick>> {
        let r = c.read_regs_checked(port, regs);
        self.report.words_read = c.words_read;
        self.report.rereads = c.rereads;
        match r? {
            Ok(v) => Ok(Ok(v)),
            Err(u) => {
                self.report.torn += 1;
                self.report.unsettled += 1;
                warn!(
                    target: "isdb::chain",
                    addr = u.addr,
                    unsettled = self.report.unsettled,
                    "chain: register {:#05x} read {:#x?}: never the same twice in a row (another client of the device's direct_reg_access, e.g. a board-side poller: docs/receiving.md); reading again later",
                    u.addr,
                    u.reads
                );
                Ok(Err(Tick::Torn))
            }
        }
    }

    /// The [`Gate`] on a solve: `None` to write it.
    fn gate(&mut self, sums: &TrainingSums, taps: &Taps) -> Option<Rejected> {
        let g = self.opts.gate?;
        let r = residual(sums, taps);
        if r.is_nan() || r > g.residual {
            self.pending_centre = None;
            return Some(Rejected::Residual);
        }
        let c = magnitude(taps[cc::TAPS / 2]);
        let off = |from: f64| (c - from).abs() > g.centre * from;
        let moved = self
            .report
            .last_taps
            .is_some_and(|t| off(magnitude(t[cc::TAPS / 2])));
        if !moved {
            self.pending_centre = None;
            return None;
        }
        if self.pending_centre.is_some_and(|p| !off(p)) {
            // the second solve in a row at the new level: a lasting change
            self.pending_centre = None;
            return None;
        }
        self.pending_centre = Some(c);
        Some(Rejected::Centre)
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
        /// SUMS_CAPTURE: armed, valid, holding.
        pub cap: (bool, bool, bool),
        /// A STATUS that never shows the chain selected.
        pub deaf: bool,
        pub bus_writes: usize,
        /// The register the last command READ (until the next command).
        read_addr: Option<u16>,
        /// Foreign values: the next `gpio_in` read after a READ of the
        /// register returns the value instead (once each, in order): what
        /// another client of `direct_reg_access` makes a read return.
        pub foreign: Vec<(u16, u32)>,
        /// A register whose `gpio_in` reads never agree twice in a row.
        pub flaky: Option<u16>,
        /// `gpio_in` reads.
        pub gpio_in_reads: usize,
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

        /// A frame end with these sums: latched unless a capture holds
        /// the last one; an armed capture latches and holds.
        pub fn frame_end(&mut self, s: (u32, u32, Vec<u32>)) {
            let (armed, _, hold) = self.cap;
            if !hold || armed {
                self.sums = Some(s);
                if armed {
                    self.cap = (false, true, true);
                }
            }
        }

        fn store(&mut self, v: u32) {
            self.regs.insert(self.addr, v);
            self.writes.push((self.addr, v));
            if self.addr == reg::SUMS_CAPTURE {
                self.cap.0 = true;
                self.cap.1 = false;
            }
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
            if a == reg::SUMS_CAPTURE {
                return u32::from(self.cap.1) | u32::from(self.cap.0) << 1;
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
            self.read_addr = None;
            match (w >> 28) & 7 {
                0 => {}
                1 => self.addr = u16::try_from(lo).unwrap(),
                2 => self.staged = lo,
                3 => self.store(self.staged << 16 | lo),
                4 => {
                    self.read_addr = Some(self.addr);
                    self.gpio_in = self.read_reg();
                }
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
            let mut d = self.get();
            Ok(match addr {
                GPIO_IN_REG => {
                    d.gpio_in_reads += 1;
                    let (n, v) = (d.gpio_in_reads, d.gpio_in);
                    match d.read_addr {
                        Some(a) if d.flaky == Some(a) => v ^ u32::from(n % 2 == 0),
                        Some(a) => match d.foreign.iter().position(|f| f.0 == a) {
                            Some(i) => d.foreign.remove(i).1,
                            None => v,
                        },
                        None => v,
                    }
                }
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

    /// A solver that marks the frame in the centre tap (its taps do not
    /// solve the sums: the tests that use it run without the gate).
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
        let m = Mock::new(Model::new(
            id_word(),
            cc::SLOTS_VERSION,
            features::ALL & !features::SUMS_CAPTURE,
        ));
        m.get().gpio_out = 1;
        let mut c = ChainControl::attach("mock", &m).unwrap();
        c.open(&m, &params(), &ChainInit::default()).unwrap();
        let opts = SlowLoopOptions {
            gate: None,
            check: None,
            ..SlowLoopOptions::new(fake_solve, 33.7561e6)
        };
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

    /// The slow loop on an image with SUMS_CAPTURE: a tick arms a capture
    /// (SUMS_CAPTURE written once), ticks poll until the frame end holds
    /// one; frames ending meanwhile do not move it; the 308 registers are
    /// read without a retry and solved; the next tick arms again.
    #[test]
    fn slow_loop_capture_sequence() {
        let m = chain();
        let mut c = ChainControl::attach("mock", &m).unwrap();
        c.open(&m, &params(), &ChainInit::default()).unwrap();
        let opts = SlowLoopOptions {
            gate: None,
            check: None,
            ..SlowLoopOptions::new(fake_solve, 33.7561e6)
        };
        let mut l = SlowLoop::new(opts);
        let strobes = |m: &Mock| {
            m.get()
                .writes
                .iter()
                .filter(|w| w.0 == reg::SUMS_CAPTURE)
                .count()
        };
        // free-running before the first capture: frame 40 latched
        m.get().frame_end(sums(40, 6_720));
        assert_eq!(l.tick(&mut c, &m).unwrap(), Tick::Capturing);
        assert_eq!(strobes(&m), 1);
        assert_eq!(l.wait(Tick::Capturing), opts.poll);
        // no frame end yet: still waiting, no second strobe
        assert_eq!(l.tick(&mut c, &m).unwrap(), Tick::Capturing);
        assert_eq!(strobes(&m), 1);
        // frame 50 ends (captured and held), then 51 (summed, not latched)
        m.get().frame_end(sums(50, 6_720));
        m.get().frame_end(sums(51, 6_720));
        assert_eq!(m.get().sums.as_ref().unwrap().0, 50);
        assert_eq!(l.tick(&mut c, &m).unwrap(), Tick::Solved { frame: 50 });
        assert_eq!(m.get().taps.unwrap().1, 50);
        assert_eq!(l.wait(Tick::Solved { frame: 50 }), opts.every);
        // the next tick arms again; frame 52 is captured
        assert_eq!(l.tick(&mut c, &m).unwrap(), Tick::Capturing);
        assert_eq!(strobes(&m), 2);
        m.get().frame_end(sums(52, 6_720));
        assert_eq!(l.tick(&mut c, &m).unwrap(), Tick::Solved { frame: 52 });
        assert_eq!((l.report.solves, l.report.torn), (2, 0));
        assert_eq!(m.get().gpio_in, id_word());
    }

    /// Sums whose solve is `taps`: R diagonal (2³⁰ on each tap), p = R w.
    fn sums_for(frame: u32, taps: &Taps) -> TrainingSums {
        let mut r = vec![[0i64; 2]; cc::R_ENTRIES];
        let mut i = 0;
        for row in 0..cc::TAPS {
            r[i] = [1 << 30, 0];
            i += cc::TAPS - row;
        }
        TrainingSums {
            frame,
            count: 6_720,
            r,
            p: taps
                .iter()
                .map(|t| [i64::from(t[0]) << 16, i64::from(t[1]) << 16])
                .collect(),
        }
    }

    /// Taps with the centre tap `centre` (the board's, build 7) and two
    /// small taps beside it.
    fn board_taps(centre: [i16; 2]) -> Taps {
        let mut t = cc::Taps::default();
        t[cc::TAPS / 2] = centre;
        t[cc::TAPS / 2 + 1] = [-410, 95];
        t[cc::TAPS / 2 - 1] = [233, -61];
        t
    }

    /// The exact solve of [`sums_for`]'s diagonal sums (frame 575: a
    /// clamped centre tap, as a failed solve would give).
    fn diag_solve(s: &TrainingSums) -> Option<Taps> {
        let mut i = 0;
        let mut t = cc::Taps::default();
        for (row, w) in t.iter_mut().enumerate() {
            let d = s.r[i][0];
            if d == 0 {
                return None;
            }
            *w = [
                i16::try_from(s.p[row][0] * (1 << 14) / d).ok()?,
                i16::try_from(s.p[row][1] * (1 << 14) / d).ok()?,
            ];
            i += cc::TAPS - row;
        }
        if s.frame == 575 {
            t[cc::TAPS / 2] = [i16::MAX, 0];
        }
        Some(t)
    }

    /// The residual: about 0 for the solve of the sums, large for other
    /// taps; infinite for p = 0.
    #[test]
    fn residual_of_the_solve_and_of_other_taps() {
        let w = board_taps([17_338, -269]);
        let s = sums_for(5, &w);
        assert!(residual(&s, &w) < 1e-12);
        assert_eq!(diag_solve(&s), Some(w));
        let off = board_taps([18_691, -285]);
        let r = residual(&s, &off);
        assert!((0.07..0.09).contains(&r), "{r}");
        let mut z = s.clone();
        z.p = vec![[0, 0]; cc::TAPS];
        assert!(residual(&z, &w).is_infinite());
        // an off-diagonal entry (R[4][5], upper-triangle index 39) enters
        // rows 4 and 5
        let mut h = s;
        h.r[39] = [1 << 28, 1 << 27];
        assert!(residual(&h, &w) > 0.01);
        assert_eq!(digest(&[]), 0xcbf2_9ce4_8422_2325);
        assert_ne!(digest(&[1, 2]), digest(&[2, 1]));
    }

    /// The gate on the board's sequence of solves (build 7): frame 250's
    /// centre tap 18 691 against 17 325 is not written (the taps of frame
    /// 168 stay), frame 331's 17 848 is; a lasting change of level is
    /// written at its second solve; a clamped solve is not written. The
    /// rejected solves are counted and not retried.
    #[test]
    fn slow_loop_gate() {
        let m = Mock::new(Model::new(
            id_word(),
            cc::SLOTS_VERSION,
            features::ALL & !features::SUMS_CAPTURE,
        ));
        m.get().gpio_out = 1;
        let mut c = ChainControl::attach("mock", &m).unwrap();
        c.open(&m, &params(), &ChainInit::default()).unwrap();
        let mut l = SlowLoop::new(SlowLoopOptions::new(diag_solve, 33.7561e6));
        use Rejected::{Centre, Residual};
        let expect = [
            (5, [17_338, -269], Tick::Solved { frame: 5 }, 5),
            (87, [17_332, -271], Tick::Solved { frame: 87 }, 87),
            (168, [17_325, -236], Tick::Solved { frame: 168 }, 168),
            (
                250,
                [18_691, -285],
                Tick::Rejected {
                    frame: 250,
                    why: Centre,
                },
                168,
            ),
            (331, [17_848, -197], Tick::Solved { frame: 331 }, 331),
            (
                413,
                [19_000, -250],
                Tick::Rejected {
                    frame: 413,
                    why: Centre,
                },
                331,
            ),
            (494, [19_100, -240], Tick::Solved { frame: 494 }, 494),
            (
                575,
                [19_050, -240],
                Tick::Rejected {
                    frame: 575,
                    why: Residual,
                },
                494,
            ),
            (656, [19_050, -240], Tick::Solved { frame: 656 }, 656),
        ];
        let mut written = std::collections::HashMap::new();
        for (frame, centre, tick, from) in expect {
            written.insert(frame, centre);
            let s = sums_for(frame, &board_taps(centre));
            m.get().sums = Some((frame, s.count, cc::sums_to_words(&s)));
            let t = l.tick(&mut c, &m).unwrap();
            let now = m.get().taps.map(|(t, from)| (t[cc::TAPS / 2], from));
            assert_eq!(t, tick, "frame {frame}");
            assert_eq!(now, Some((written[&from], from)), "frame {frame}");
            // a rejected frame is not solved again
            assert_eq!(l.tick(&mut c, &m).unwrap(), Tick::Same);
        }
        assert_eq!(
            (l.report.solves, l.report.solves_rejected, l.report.singular),
            (6, 3, 0)
        );
        assert_eq!(l.report.last_frame, Some(656));
    }

    /// The least-squares solve of the normal equations (Gaussian
    /// elimination with partial pivoting, complex, f64) rounded to Q2.14:
    /// the model's `solve_training` for these tests.
    #[allow(clippy::needless_range_loop)]
    pub(crate) fn ls_solve(s: &TrainingSums) -> Option<Taps> {
        let n = cc::TAPS;
        if s.count == 0 {
            return None;
        }
        let mut a = vec![vec![[0.0f64; 2]; n + 1]; n];
        for r in 0..n {
            for c in 0..n {
                let v = if r <= c {
                    re_im(s.r[r_index(r, c)])
                } else {
                    let v = re_im(s.r[r_index(c, r)]);
                    [v[0], -v[1]]
                };
                a[r][c] = v;
            }
            a[r][n] = re_im(s.p[r]);
        }
        let mul = |x: [f64; 2], y: [f64; 2]| [x[0] * y[0] - x[1] * y[1], x[0] * y[1] + x[1] * y[0]];
        let div = |x: [f64; 2], y: [f64; 2]| {
            let d = y[0] * y[0] + y[1] * y[1];
            [
                (x[0] * y[0] + x[1] * y[1]) / d,
                (x[1] * y[0] - x[0] * y[1]) / d,
            ]
        };
        let abs = |x: [f64; 2]| x[0].hypot(x[1]);
        for col in 0..n {
            let piv = (col..n).max_by(|&i, &j| abs(a[i][col]).total_cmp(&abs(a[j][col])))?;
            if abs(a[piv][col]) == 0.0 {
                return None;
            }
            a.swap(col, piv);
            for r in col + 1..n {
                let f = div(a[r][col], a[col][col]);
                for c in col..=n {
                    let x = mul(f, a[col][c]);
                    a[r][c] = [a[r][c][0] - x[0], a[r][c][1] - x[1]];
                }
            }
        }
        let mut w = vec![[0.0; 2]; n];
        for r in (0..n).rev() {
            let mut acc = a[r][n];
            for c in r + 1..n {
                let x = mul(a[r][c], w[c]);
                acc = [acc[0] - x[0], acc[1] - x[1]];
            }
            w[r] = div(acc, a[r][r]);
        }
        let q = |v: f64| -> i16 {
            #[allow(clippy::cast_possible_truncation)]
            let x = (v * f64::from(1u32 << 14))
                .round()
                .clamp(f64::from(i16::MIN), f64::from(i16::MAX)) as i16;
            x
        };
        Some(std::array::from_fn(|k| [q(w[k][0]), q(w[k][1])]))
    }

    /// Sums with the board's numbers (#27: BS-14 at 13 dB, R ≈ 6.6·10⁹ on
    /// the diagonal): a near-Toeplitz R (diagonal 6.5·10⁹ with 1 % edge
    /// terms, its first and second off-diagonals) and p = R w for taps w
    /// like the board's; p[3].re is negative, so its high word is
    /// 0xFFFF_FFFF.
    pub(crate) fn board_sums(frame: u32) -> TrainingSums {
        let n = cc::TAPS;
        let w = board_like_taps();
        let wf: Vec<[f64; 2]> = w
            .iter()
            .map(|t| [f64::from(t[0]) / 16_384.0, f64::from(t[1]) / 16_384.0])
            .collect();
        let entry = |r: usize, c: usize| -> [i64; 2] {
            match c - r {
                #[allow(clippy::cast_possible_wrap)]
                0 => [6_500_000_000 + 13_000_000 * (r as i64 % 3), 0],
                1 => [140_000_000, -38_000_000],
                2 => [-30_000_000, 9_000_000],
                _ => [0, 0],
            }
        };
        let mut r_ = vec![[0i64; 2]; cc::R_ENTRIES];
        for r in 0..n {
            for c in r..n {
                r_[r_index(r, c)] = entry(r, c);
            }
        }
        let full = |r: usize, c: usize| {
            let v = if r <= c { entry(r, c) } else { entry(c, r) };
            #[allow(clippy::cast_precision_loss)]
            let v = [v[0] as f64, v[1] as f64];
            if r <= c { v } else { [v[0], -v[1]] }
        };
        let p = (0..n)
            .map(|r| {
                let mut acc = [0.0, 0.0];
                for (c, x) in wf.iter().enumerate() {
                    let a = full(r, c);
                    acc[0] += a[0] * x[0] - a[1] * x[1];
                    acc[1] += a[0] * x[1] + a[1] * x[0];
                }
                #[allow(clippy::cast_possible_truncation)]
                [acc[0].round() as i64, acc[1].round() as i64]
            })
            .collect();
        TrainingSums {
            frame,
            count: 6_720,
            r: r_,
            p,
        }
    }

    /// Taps like the board's (build 8: centre ≈ 1.01, small side taps).
    fn board_like_taps() -> Taps {
        let mut t = cc::Taps::default();
        t[5] = [16_550, -260];
        t[4] = [-1_200, 150];
        t[6] = [-1_350, 90];
        t[3] = [-330, 40];
        t[7] = [250, 60];
        t[2] = [-120, 20];
        t
    }

    fn latch(m: &Mock, s: &TrainingSums) {
        m.get().sums = Some((s.frame, s.count, cc::sums_to_words(s)));
    }

    fn opened(features: u32) -> (Mock, ChainControl) {
        let m = Mock::new(Model::new(id_word(), cc::SLOTS_VERSION, features));
        m.get().gpio_out = 1;
        let mut c = ChainControl::attach("mock", &m).unwrap();
        c.open(&m, &params(), &ChainInit::default()).unwrap();
        (m, c)
    }

    /// The board-like sums pass the checks, their solve is the taps they
    /// were made from and its fit is the plausible one (≈ 0.94: 12 dB).
    #[test]
    fn board_sums_pass_the_checks() {
        let s = board_sums(9);
        assert_eq!(check_sums(&s, &SumsCheck::default()), None);
        let sh = sums_shape(&s).unwrap();
        assert!(sh.spread.1 < 0.01 && sh.toeplitz.1 < 0.03, "{sh:?}");
        let t = ls_solve(&s).unwrap();
        for (a, b) in t.iter().zip(&board_like_taps()) {
            assert!(
                (a[0] - b[0]).abs() <= 1 && (a[1] - b[1]).abs() <= 1,
                "{t:?}"
            );
        }
        let f = fit(&s, &t);
        assert!((0.90..0.97).contains(&f), "{f}");
    }

    /// #27/#29: a foreign value (0) on one `gpio_in` read of one sum word,
    /// the high word of p[3].re (0xFFFF_FFFF), is caught by the agreement
    /// rule: the word is read again, the taps written are the clean
    /// solve's (with one read per word, p[3] would be off by 2³², tap 3
    /// by ≈ 0.65), and the reread is counted.
    #[test]
    fn a_foreign_read_is_caught_by_the_agreement_rule() {
        let (m, mut c) = opened(features::ALL & !features::SUMS_CAPTURE);
        let s = board_sums(70);
        latch(&m, &s);
        let p3_re_hi = cc::sum_registers(1, 3)[1];
        assert_eq!(
            cc::sums_to_words(&s)[4 * cc::R_ENTRIES + 4 * 3 + 1],
            u32::MAX
        );
        m.get().foreign.push((p3_re_hi, 0));
        let mut l = SlowLoop::new(SlowLoopOptions::new(ls_solve, 33.7561e6));
        assert_eq!(l.tick(&mut c, &m).unwrap(), Tick::Solved { frame: 70 });
        assert!(m.get().foreign.is_empty(), "the foreign value was read");
        let (t, from) = m.get().taps.unwrap();
        assert_eq!(from, 70);
        assert_eq!(Some(t), ls_solve(&s), "the clean solve written");
        assert_eq!(l.report.rereads, 1);
        assert_eq!(l.report.torn, 0);
        // every register read at least twice
        let reads = m.get().gpio_in_reads;
        assert!(reads as u64 >= 2 * l.report.words_read, "{reads}");
    }

    /// A register that never reads the same twice in a row: the tick is
    /// torn (counted as unsettled), nothing is written, and the next tick
    /// reads the frame and solves it.
    #[test]
    fn a_register_that_never_settles_is_a_torn_read() {
        let (m, mut c) = opened(features::ALL & !features::SUMS_CAPTURE);
        latch(&m, &board_sums(80));
        m.get().flaky = Some(cc::sum_registers(0, 17)[2]);
        let mut l = SlowLoop::new(SlowLoopOptions::new(ls_solve, 33.7561e6));
        assert_eq!(l.tick(&mut c, &m).unwrap(), Tick::Torn);
        assert_eq!((l.report.torn, l.report.unsettled), (1, 1));
        assert!(m.get().taps.is_none());
        assert_eq!(m.get().gpio_in, id_word(), "the NOP after the reads");
        m.get().flaky = None;
        assert_eq!(l.tick(&mut c, &m).unwrap(), Tick::Solved { frame: 80 });
        // the open's and the attach's reads are verified too
        m.get().flaky = Some(reg::FEATURES);
        let e = ChainControl::attach("mock", &m)
            .unwrap()
            .read_features(&m)
            .unwrap_err();
        assert!(e.to_string().contains("never the same twice"), "{e}");
    }

    /// #27: a consistent but corrupted set (the word read as 0 on every
    /// read, as a latch would hold it) is caught by the checks on the sums:
    /// p[3].re's high word 0 (an error of 2³² against R ≈ 6.5·10⁹: the fit
    /// of the solve exceeds 1), R[2][2]'s high word 0 (the diagonal's
    /// spread) and R[3][5].re's high word 0 (+2³²: not Toeplitz). Nothing
    /// is written; each is counted with its reason; a clean capture after
    /// them is solved.
    #[test]
    fn a_corrupted_set_is_caught_by_the_sums_checks() {
        let (m, mut c) = opened(features::ALL & !features::SUMS_CAPTURE);
        let mut l = SlowLoop::new(SlowLoopOptions::new(ls_solve, 33.7561e6));
        let cases: [(u32, usize, &str); 3] = [
            (100, 4 * cc::R_ENTRIES + 4 * 3 + 1, "fit above 1"),
            (101, 4 * r_index(2, 2) + 1, "diagonal spread"),
            (102, 4 * r_index(3, 5) + 1, "not Toeplitz"),
        ];
        for (frame, word, why) in cases {
            let s = board_sums(frame);
            let mut w = cc::sums_to_words(&s);
            assert_ne!(w[word], 0);
            w[word] = 0;
            m.get().sums = Some((frame, s.count, w));
            let t = l.tick(&mut c, &m).unwrap();
            let Tick::CaptureRejected {
                frame: f,
                why: fault,
            } = t
            else {
                panic!("frame {frame}: {t:?}");
            };
            assert_eq!((f, fault.name()), (frame, why), "{fault}");
            assert!(m.get().taps.is_none(), "frame {frame}: nothing written");
            // not read again
            assert_eq!(l.tick(&mut c, &m).unwrap(), Tick::Same);
        }
        assert_eq!(l.report.captures_rejected, 3);
        assert_eq!(
            l.report
                .captures_rejected_by
                .values()
                .copied()
                .collect::<Vec<_>>(),
            [1, 1, 1]
        );
        // the issue's numbers: tap 3 would have moved by ≈ 2³² / R[3][3]
        let mut bad = board_sums(100);
        bad.p[3][0] += 1 << 32;
        let t = ls_solve(&bad).unwrap();
        let moved = f64::from(t[3][0] - board_like_taps()[3][0]) / 16_384.0;
        assert!((0.6..0.7).contains(&moved), "{moved}");
        assert!(fit(&bad, &t) > 1.2);
        latch(&m, &board_sums(103));
        assert_eq!(l.tick(&mut c, &m).unwrap(), Tick::Solved { frame: 103 });
    }

    /// The timer thread: solves the latched frames as they come and stops
    /// at once on `stop`, its encoder continuing the open's toggle bit.
    #[test]
    fn slow_loop_thread() {
        let m = chain();
        let mut c = ChainControl::attach("mock", &m).unwrap();
        c.open(&m, &params(), &ChainInit::default()).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let opts = SlowLoopOptions {
            every: Duration::from_millis(20),
            poll: Duration::from_millis(5),
            gate: None,
            check: None,
            ..SlowLoopOptions::new(fake_solve, 33.7561e6)
        };
        let h = spawn_slow_loop("mock", Box::new(m.clone()), opts, Arc::clone(&stop)).unwrap();
        // frames end every 2 ms: 7 until it is solved, then 8, 9, ...
        let t = Instant::now();
        while m.get().taps.is_none_or(|t| t.1 != 7) {
            assert!(t.elapsed() < Duration::from_secs(10), "no solve");
            m.get().frame_end(sums(7, 6_720));
            std::thread::sleep(Duration::from_millis(2));
        }
        let mut f = 8;
        while m.get().taps.is_none_or(|t| t.1 < 8) {
            assert!(t.elapsed() < Duration::from_secs(10), "no second solve");
            m.get().frame_end(sums(f, 6_720));
            f += 1;
            std::thread::sleep(Duration::from_millis(2));
        }
        stop.store(true, Ordering::SeqCst);
        let r = h.join().unwrap();
        assert!(r.solves >= 2);
        assert!(r.last_frame >= Some(8));
        assert_eq!(r.torn, 0, "the capture holds the frame read");
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
