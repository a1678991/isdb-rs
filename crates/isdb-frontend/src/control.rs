//! How a host configures and discovers the FPGA front end (format 2):
//! the register map and the command words, written through the general
//! purpose output of the AD9361 interface core (`axi_ad9361`, register
//! word 0x2F `up_adc_gpio_out`, libiio debug address 0x800000BC) and read
//! back through its general purpose input (word 0x2E `up_adc_gpio_in`,
//! 0x800000B8). No kernel driver or device-tree change is needed: libiio
//! reaches both registers through iiod's register access on the
//! `cf-ad9361-lpc` device, which `isdb-iio` already uses for the ADC
//! status.
//!
//! **Command word** (`gpio_out`): bit 31 toggles to execute a command,
//! bits 30–28 are the operation, bits 27–1 its argument; bit 0 is left to
//! the bitstream's own use (Tezuka: the 12-bit stream's sync control) and
//! is kept as it was.
//!
//! | Op | Name | Effect |
//! |---|---|---|
//! | 0 | NOP | `gpio_in` shows the ID word again |
//! | 1 | ADDR | address ← argument bits 15–0 |
//! | 2 | HI | staged high half ← argument bits 15–0 (kept until changed) |
//! | 3 | WRITE | register\[address\] ← staged << 16 \| argument bits 15–0; address += 1 |
//! | 4 | READ | `gpio_in` ← register\[address\] (until the next command) |
//! | 5 | CTRL | argument bit 0: run; bit 1: start (reset the state, one pulse); bit 2: symbols to the DMA instead of IQ |
//! | 6 | WRITE_S | register\[address\] ← the argument sign-extended from 27 bits; address += 1 |
//!
//! **ID word** (`gpio_in` after any command but READ, and after reset):
//! bits 31–16 [`ID_MAGIC`], bits 15–12 the format version, bits 7–0 the
//! taps per phase. A bitstream without the front end reads 0 there
//! (Tezuka v0.3.24 leaves the input unconnected), so discovery is one
//! register read and changes nothing.

use crate::params::{FORMAT_VERSION, MIN_ARITH_VERSION, PHASES, Params};
use crate::shape::fixed::TxParams;

/// Magic of the ID word, bits 31–16.
pub const ID_MAGIC: u16 = 0x15DB;
/// libiio debug address of `up_adc_gpio_out` on `cf-ad9361-lpc`.
pub const GPIO_OUT_REG: u32 = 0x8000_00BC;
/// libiio debug address of `up_adc_gpio_in` on `cf-ad9361-lpc`.
pub const GPIO_IN_REG: u32 = 0x8000_00B8;

/// Register addresses.
pub mod reg {
    /// `Params::nco_step`.
    pub const NCO_STEP: u16 = 0x00;
    /// `Params::period_nominal`.
    pub const PERIOD_NOMINAL: u16 = 0x01;
    /// `Params::period_min`.
    pub const PERIOD_MIN: u16 = 0x02;
    /// `Params::period_max`.
    pub const PERIOD_MAX: u16 = 0x03;
    /// `Params::ted_shift`.
    pub const TED_SHIFT: u16 = 0x04;
    /// `Params::kp`.
    pub const KP: u16 = 0x05;
    /// `Params::kp_shift`.
    pub const KP_SHIFT: u16 = 0x06;
    /// `Params::ki`.
    pub const KI: u16 = 0x07;
    /// `Params::ki_shift`.
    pub const KI_SHIFT: u16 = 0x08;
    /// `Params::integrator_max`.
    pub const INTEGRATOR_MAX: u16 = 0x09;
    /// `Params::agc_init`.
    pub const AGC_INIT: u16 = 0x0A;
    /// `Params::agc_target`.
    pub const AGC_TARGET: u16 = 0x0B;
    /// `Params::agc_shift`.
    pub const AGC_SHIFT: u16 = 0x0C;
    /// `Params::bits`.
    pub const BITS: u16 = 0x0D;
    /// `Params::q_shift`.
    pub const Q_SHIFT: u16 = 0x0E;
    /// `Params::block_log2`.
    pub const BLOCK_LOG2: u16 = 0x0F;
    /// Read only: bit 0 run, bit 1 symbols selected.
    pub const STATUS: u16 = 0x10;
    /// Read only: symbols emitted since the last start (low 32 bits).
    pub const SYMBOLS: u16 = 0x11;
    /// Matched-filter bank: `H_BASE + phase·64 + tap`.
    pub const H_BASE: u16 = 0x1000;
    /// Derivative bank: `G_BASE + phase·64 + tap`.
    pub const G_BASE: u16 = 0x2000;
    /// Output bank (format 2): `K_BASE + phase·64 + tap`. Writing it alone
    /// while running changes the trim without restarting.
    pub const K_BASE: u16 = 0x3000;
}

/// Operations of a command word.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Op {
    /// Nothing; `gpio_in` shows the ID word.
    Nop = 0,
    /// Set the address.
    Addr = 1,
    /// Stage the high half of the next write.
    Hi = 2,
    /// Write and advance the address.
    Write = 3,
    /// Show a register on `gpio_in`.
    Read = 4,
    /// Run / start / output selection.
    Ctrl = 5,
    /// Write a sign-extended 27-bit value and advance the address.
    WriteS = 6,
}

/// CTRL argument bits.
pub mod ctrl {
    /// Process input samples.
    pub const RUN: u32 = 1;
    /// Reset the state to its initial values (one pulse).
    pub const START: u32 = 2;
    /// Send symbols to the DMA instead of IQ samples.
    pub const SYMBOLS: u32 = 4;
}

/// The front end a bitstream reports in its ID word.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Identity {
    /// Format version.
    pub version: u16,
    /// Taps per phase.
    pub taps: usize,
}

impl Identity {
    /// Parse a `gpio_in` value; `None` without the magic.
    pub fn parse(word: u32) -> Option<Self> {
        ((word >> 16) as u16 == ID_MAGIC).then_some(Self {
            version: ((word >> 12) & 0xF) as u16,
            taps: (word & 0xFF) as usize,
        })
    }

    /// The ID word.
    pub fn word(&self) -> u32 {
        u32::from(ID_MAGIC) << 16
            | u32::from(self.version & 0xF) << 12
            | u32::from(self.taps.to_le_bytes()[0])
    }

    /// Whether this host can drive it: the same arithmetic (versions
    /// [`MIN_ARITH_VERSION`] to [`FORMAT_VERSION`]: the devices whose
    /// arithmetic is the model's) and `p`'s taps.
    pub fn supports(&self, p: &Params) -> bool {
        (MIN_ARITH_VERSION..=FORMAT_VERSION).contains(&self.version) && self.taps == p.taps()
    }
}

/// Encodes command words, keeping the toggle bit, the staged high half
/// and the bitstream's own bit 0.
#[derive(Debug, Clone)]
pub struct Encoder {
    toggle: bool,
    staged: Option<u16>,
    bit0: u32,
}

impl Encoder {
    /// An encoder continuing from the current `gpio_out` value.
    pub fn new(current: u32) -> Self {
        Self {
            toggle: current >> 31 != 0,
            staged: None,
            bit0: current & 1,
        }
    }

    /// The word for one command (toggles bit 31).
    pub fn word(&mut self, op: Op, arg: u32) -> u32 {
        self.toggle = !self.toggle;
        u32::from(self.toggle) << 31 | (op as u32) << 28 | (arg & 0x07FF_FFFF) << 1 | self.bit0
    }

    /// Words writing `values` from `addr` on: one WRITE_S each when the
    /// value fits 27 signed bits, else HI (when it changes) and WRITE.
    pub fn write(&mut self, addr: u16, values: &[u32], out: &mut Vec<u32>) {
        out.push(self.word(Op::Addr, u32::from(addr)));
        for &v in values {
            let sv = v as i32;
            if (-(1 << 26)..1 << 26).contains(&sv) {
                out.push(self.word(Op::WriteS, v & 0x07FF_FFFF));
                continue;
            }
            let hi = (v >> 16) as u16;
            if self.staged != Some(hi) {
                out.push(self.word(Op::Hi, u32::from(hi)));
                self.staged = Some(hi);
            }
            out.push(self.word(Op::Write, v & 0xFFFF));
        }
    }

    /// The whole configuration for `p`: scalar registers, then the three
    /// banks.
    pub fn configure(&mut self, p: &Params) -> Vec<u32> {
        let mut out = Vec::new();
        let scalars = [
            p.nco_step as u32,
            p.period_nominal,
            p.period_min,
            p.period_max,
            u32::from(p.ted_shift),
            i32::from(p.kp) as u32,
            u32::from(p.kp_shift),
            i32::from(p.ki) as u32,
            u32::from(p.ki_shift),
            p.integrator_max as u32,
            p.agc_init,
            p.agc_target,
            u32::from(p.agc_shift),
            u32::from(p.bits),
            u32::from(p.q_shift),
            u32::from(p.block_log2),
        ];
        self.write(reg::NCO_STEP, &scalars, &mut out);
        for (base, bank) in [
            (reg::H_BASE, &p.h),
            (reg::G_BASE, &p.g),
            (reg::K_BASE, &p.k),
        ] {
            self.write_bank(base, bank, &mut out);
        }
        out
    }

    /// Only the output bank of `p` (a new trim on a running front end).
    pub fn output_bank(&mut self, p: &Params) -> Vec<u32> {
        let mut out = Vec::new();
        self.write_bank(reg::K_BASE, &p.k, &mut out);
        out
    }

    fn write_bank(&mut self, base: u16, bank: &[Vec<i32>], out: &mut Vec<u32>) {
        for (ph, row) in (0u16..).zip(bank.iter().take(PHASES)) {
            let vals: Vec<u32> = row.iter().map(|&c| c as u32).collect();
            self.write(base + ph * 64, &vals, out);
        }
    }
}

/// The transmit interpolator's control ([`crate::shape::fixed`], TX format
/// version [`crate::txformat::FORMAT_VERSION`]): the same command words
/// and [`Encoder`] as the receive block's, on the **DAC** core's general
/// purpose pair (`axi_ad9361` TX, `up_dac_common` words 0x2F
/// `up_dac_gpio_out` and 0x2E `up_dac_gpio_in`, libiio debug addresses
/// 0x800000BC / 0x800000B8 on `cf-ad9361-dds-core-lpc`; both read 0 on a
/// bitstream without the block, VERIFICATION.md "ISDB-S transmitter in the
/// FPGA: board checks"). Bit 0 of `gpio_out` (the x8 TX FIR's enable in
/// Tezuka) is kept as it was, as on the RX pair.
///
/// | Address | Register |
/// |---|---|
/// | 0x00 | STEP: `TxParams::step`, Q0.24 (u32) |
/// | 0x01 | SHIFT: `TxParams::shift` |
/// | 0x02 | STATUS, read only: bit 0 run, bit 1 selected, bit 2 underrun (sticky until START) |
/// | 0x03 | CONSUMED, read only: codes entered from delivered words since START (low 32 bits) |
/// | 0x04 | MISSED, read only: zero symbols entered for stalled requests since START (low 32 bits) |
/// | 0x05 | FIRST_MISS, read only: CONSUMED at the first stalled request since START (0 before one) |
/// | 0x06 | SAMPLES, read only: outputs since START (low 32 bits) |
/// | 0x4000 + q·32 + t | bank A, row q, tap t (t < 2S), WRITE_S (sign-extended 27 bits; the block keeps 18) |
/// | 0x8000 + q·32 + t | bank B, likewise |
///
/// CTRL bits ([`tx::ctrl`]): run, START (one pulse: `frac`, the line, the
/// counters and the unpack register reset), SELECT (the block drives the
/// DAC data path instead of the DMA's IQ pass-through).
///
/// The block stores one copy of each tap pair (the banks' symmetry,
/// [`TxParams::validate`]), so [`Encoder::configure_tx`] writes taps
/// `t < S` of every row; tap `t ≥ S` of row q is tap 2S − 1 − t of row
/// P − 1 − q.
///
/// **ID word** (`gpio_in` after any command but READ, and after reset):
/// bits 31–16 [`tx::TX_ID_MAGIC`], 15–12 the format version, 11–8 log2 of
/// the phases, 7–0 the taps per row: 0xB5D1_1918 for P 512, 24 taps.
pub mod tx {
    use crate::shape::fixed::TxParams;
    use crate::txformat::FORMAT_VERSION;

    /// Magic of the TX ID word, bits 31–16 (distinct from the receive
    /// block's [`super::ID_MAGIC`], so a mis-wired readback is caught).
    pub const TX_ID_MAGIC: u16 = 0xB5D1;
    /// libiio debug address of `up_dac_gpio_out` on
    /// `cf-ad9361-dds-core-lpc`.
    pub const GPIO_OUT_REG: u32 = 0x8000_00BC;
    /// libiio debug address of `up_dac_gpio_in` on
    /// `cf-ad9361-dds-core-lpc`.
    pub const GPIO_IN_REG: u32 = 0x8000_00B8;

    /// Register addresses (module documentation).
    pub mod reg {
        /// `TxParams::step`.
        pub const STEP: u16 = 0x00;
        /// `TxParams::shift`.
        pub const SHIFT: u16 = 0x01;
        /// Read only: [`super::status`] bits.
        pub const STATUS: u16 = 0x02;
        /// Read only: codes entered from delivered words since START.
        pub const CONSUMED: u16 = 0x03;
        /// Read only: zero symbols entered for stalled requests since
        /// START.
        pub const MISSED: u16 = 0x04;
        /// Read only: CONSUMED at the first stalled request since START.
        pub const FIRST_MISS: u16 = 0x05;
        /// Read only: output samples since START.
        pub const SAMPLES: u16 = 0x06;
        /// Bank A (even k): `A_BASE + q·ROW_STRIDE + t`.
        pub const A_BASE: u16 = 0x4000;
        /// Bank B (odd k): `B_BASE + q·ROW_STRIDE + t`.
        pub const B_BASE: u16 = 0x8000;
        /// Addresses per bank row.
        pub const ROW_STRIDE: u16 = 32;
    }

    /// CTRL argument bits.
    pub mod ctrl {
        /// Run: output a sample per clock.
        pub const RUN: u32 = 1;
        /// Reset `frac`, the line, the counters and the unpack register
        /// (one pulse).
        pub const START: u32 = 2;
        /// Drive the DAC data path instead of the DMA's IQ pass-through.
        pub const SELECT: u32 = 4;
    }

    /// STATUS bits.
    pub mod status {
        /// The block runs.
        pub const RUN: u32 = 1;
        /// The block drives the DAC (codes are not sent out as samples).
        pub const SELECTED: u32 = 2;
        /// A word request has stalled since START (sticky).
        pub const UNDERRUN: u32 = 4;
    }

    /// The transmit block a bitstream reports in its ID word.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct TxIdentity {
        /// Format version (the arithmetic and the code format).
        pub version: u16,
        /// log2 of the bank rows.
        pub phase_bits: u8,
        /// Taps per row.
        pub taps: usize,
    }

    impl TxIdentity {
        /// Parse a `gpio_in` value; `None` without [`TX_ID_MAGIC`].
        pub fn parse(word: u32) -> Option<Self> {
            let b = word.to_le_bytes();
            (u16::from_le_bytes([b[2], b[3]]) == TX_ID_MAGIC).then_some(Self {
                version: u16::from(b[1] >> 4),
                phase_bits: b[1] & 0xF,
                taps: usize::from(b[0]),
            })
        }

        /// The identity of a block running `p` with this model's
        /// arithmetic.
        pub fn of(p: &TxParams) -> Self {
            Self {
                version: FORMAT_VERSION,
                // log2 P <= 9 (TxParams::validate).
                #[allow(clippy::cast_possible_truncation)]
                phase_bits: p.phase_bits() as u8,
                taps: p.taps(),
            }
        }

        /// The ID word.
        pub fn word(&self) -> u32 {
            u32::from(TX_ID_MAGIC) << 16
                | u32::from(self.version & 0xF) << 12
                | u32::from(self.phase_bits & 0xF) << 8
                | u32::from(self.taps.to_le_bytes()[0])
        }

        /// Bank rows, 2^`phase_bits`.
        pub fn phases(&self) -> usize {
            1 << self.phase_bits
        }

        /// Whether this host can drive it with `p`: the model's format
        /// version and `p`'s geometry.
        pub fn supports(&self, p: &TxParams) -> bool {
            *self == Self::of(p)
        }
    }
}

impl Encoder {
    /// The whole transmit configuration for `p` ([`tx`]): STEP and SHIFT,
    /// then banks A and B, row by row, taps `0..S` (the stored half; one
    /// ADDR per row). Follow it with CTRL START | SELECT (the block owns
    /// the DAC and the DMA, outputs zero and requests nothing), start the
    /// TX DMA, then, once half its queue is filled, CTRL RUN | SELECT: the
    /// block counts every word requested before the first arrives as
    /// missed, and before SELECT the pass-through sends the DMA's words to
    /// the DAC as samples (fpga/README.md "The transmit block"; the
    /// `isdb-radio` Pluto backend does this). CTRL 0 makes it transparent
    /// again.
    pub fn configure_tx(&mut self, p: &TxParams) -> Vec<u32> {
        let mut out = Vec::new();
        self.write(tx::reg::STEP, &[p.step, u32::from(p.shift)], &mut out);
        let span = p.span();
        for (base, bank) in [(tx::reg::A_BASE, &p.a), (tx::reg::B_BASE, &p.b)] {
            for (q, row) in (0u16..).zip(bank.iter()) {
                let vals: Vec<u32> = row[..span].iter().map(|&c| c as u32).collect();
                self.write(base + q * tx::reg::ROW_STRIDE, &vals, &mut out);
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::params::Design;

    #[test]
    fn spec_command_word_layout() {
        let mut e = Encoder::new(0x0000_0001);
        // ADDR 0x1234: toggle 1, op 1, arg << 1, bit 0 kept.
        assert_eq!(
            e.word(Op::Addr, 0x1234),
            0x8000_0000 | 1 << 28 | 0x1234 << 1 | 1
        );
        // Next command toggles back to 0.
        assert_eq!(e.word(Op::Nop, 0), 1);
    }

    #[test]
    fn identity_round_trip_and_absent_front_end() {
        let id = Identity {
            version: 2,
            taps: 32,
        };
        assert_eq!(id.word(), 0x15DB_2020);
        assert_eq!(Identity::parse(id.word()), Some(id));
        assert_eq!(Identity::parse(0), None);
    }

    #[test]
    fn only_version_4_has_the_models_arithmetic() {
        let p = Params::design(&Design::new(40e6, 28.86e6, 0.35)).unwrap();
        let id = |version| Identity {
            version,
            taps: p.taps(),
        };
        assert!(id(4).supports(&p));
        // 2 and 3: the loop of the former 4-cycles-per-sample design
        for v in [1, 2, 3, 5] {
            assert!(!id(v).supports(&p), "version {v}");
        }
        assert!(
            !Identity {
                version: 4,
                taps: p.taps() + 2
            }
            .supports(&p)
        );
    }

    #[test]
    fn configuration_is_one_command_per_coefficient() {
        let p = Params::design(&Design::new(40e6, 28.86e6, 0.35)).unwrap();
        let words = Encoder::new(0).configure(&p);
        let op = |w: u32| (w >> 28) & 7;
        let writes = words
            .iter()
            .filter(|&&w| op(w) == Op::Write as u32 || op(w) == Op::WriteS as u32)
            .count();
        assert_eq!(writes, 16 + 3 * PHASES * p.taps());
        // Only the 32-bit scalars (NCO step, periods) need the high half.
        let his = words.iter().filter(|&&w| op(w) == Op::Hi as u32).count();
        assert!(his <= 4, "{his} HI commands");
        assert_eq!(words.len(), 1 + 3 * PHASES + writes + his);
    }

    /// The command protocol as a device decodes it (module documentation):
    /// a command executes when bit 31 toggles; bit 0 is never touched.
    #[derive(Default)]
    struct Device {
        last: Option<u32>,
        addr: u16,
        staged: u32,
        regs: std::collections::BTreeMap<u16, u32>,
        ctrl: Vec<u32>,
    }

    impl Device {
        fn apply(&mut self, w: u32) {
            if let Some(prev) = self.last {
                assert_ne!(prev >> 31, w >> 31, "{w:#010x}: bit 31 does not toggle");
                assert_eq!(prev & 1, w & 1, "{w:#010x}: bit 0 changed");
            }
            self.last = Some(w);
            let arg = (w >> 1) & 0x07FF_FFFF;
            let lo16 = u16::try_from(arg & 0xFFFF).unwrap();
            match (w >> 28) & 7 {
                0 => {}
                1 => self.addr = lo16,
                2 => self.staged = u32::from(lo16),
                3 => {
                    self.regs
                        .insert(self.addr, self.staged << 16 | u32::from(lo16));
                    self.addr += 1;
                }
                5 => self.ctrl.push(arg),
                6 => {
                    // sign-extend 27 bits
                    let v = ((arg << 5).cast_signed() >> 5).cast_unsigned();
                    self.regs.insert(self.addr, v);
                    self.addr += 1;
                }
                op => panic!("{w:#010x}: op {op} in a configuration"),
            }
        }
    }

    /// `configure_tx` decodes back to the parameters: STEP, SHIFT and the
    /// stored half of both banks, whose mirror is the rest; every write is
    /// at an address of the map, and bit 0 of `gpio_out` is kept.
    #[test]
    fn configure_tx_decodes_back() {
        use crate::shape::fixed::TxDesign;
        for (fs, backoff) in [(40e6, 10.0), (61.44e6, 3.0)] {
            let mut d = TxDesign::new(fs, 28.86e6, 0.35);
            d.backoff_db = backoff;
            let p = TxParams::design(&d).unwrap();
            for bit0 in [0, 1] {
                let mut enc = Encoder::new(0x8000_0000 | bit0);
                let mut words = enc.configure_tx(&p);
                words.push(enc.word(Op::Ctrl, tx::ctrl::START));
                words.push(enc.word(Op::Ctrl, tx::ctrl::RUN | tx::ctrl::SELECT));
                let mut dev = Device {
                    last: Some(0x8000_0000 | bit0),
                    ..Device::default()
                };
                for &w in &words {
                    dev.apply(w);
                }
                assert_eq!(dev.ctrl, [2, 5]);
                let reg = |a: u16| dev.regs[&a];
                assert_eq!(reg(tx::reg::STEP), p.step);
                assert_eq!(reg(tx::reg::SHIFT), u32::from(p.shift));
                let (rows, taps, span) = (p.phases(), p.taps(), p.span());
                assert_eq!(dev.regs.len(), 2 + 2 * rows * span);
                for (base, bank) in [(tx::reg::A_BASE, &p.a), (tx::reg::B_BASE, &p.b)] {
                    let mut back = vec![vec![0i32; taps]; rows];
                    for q in 0..rows {
                        for t in 0..span {
                            let a = base
                                + u16::try_from(q).unwrap() * tx::reg::ROW_STRIDE
                                + u16::try_from(t).unwrap();
                            let v = reg(a).cast_signed();
                            back[q][t] = v;
                            back[rows - 1 - q][taps - 1 - t] = v;
                        }
                    }
                    assert_eq!(&back, bank);
                }
                // only the stored half: one WRITE_S per coefficient, no HI
                let op = |w: u32| (w >> 28) & 7;
                assert_eq!(
                    words
                        .iter()
                        .filter(|&&w| op(w) == Op::WriteS as u32)
                        .count(),
                    2 + 2 * rows * span
                );
                assert_eq!(words.iter().filter(|&&w| op(w) == Op::Hi as u32).count(), 0);
                assert_eq!(words.len(), 1 + 2 + 2 * rows * (1 + span) + 2);
            }
        }
    }

    /// The TX ID word: 0xB5D1_1918 for the FPGA's P 512 and 24 taps; it
    /// parses back, a word without the magic (the receive block's, or 0
    /// from a bitstream without the block) does not, and only the model's
    /// version and geometry are supported.
    #[test]
    fn tx_identity() {
        use crate::shape::fixed::TxDesign;
        let p = TxParams::design(&TxDesign::new(40e6, 28.86e6, 0.35)).unwrap();
        assert_eq!(p.id_word(), 0xB5D1_1918);
        let id = tx::TxIdentity::parse(p.id_word()).unwrap();
        assert_eq!(
            id,
            tx::TxIdentity {
                version: 1,
                phase_bits: 9,
                taps: 24
            }
        );
        assert_eq!(id.word(), p.id_word());
        assert_eq!(id.phases(), 512);
        assert!(id.supports(&p));
        assert_eq!(tx::TxIdentity::parse(0), None);
        assert_eq!(tx::TxIdentity::parse(0x15DB_4020), None);
        for bad in [
            tx::TxIdentity { version: 2, ..id },
            tx::TxIdentity {
                phase_bits: 8,
                ..id
            },
            tx::TxIdentity { taps: 32, ..id },
        ] {
            assert!(!bad.supports(&p), "{bad:?}");
        }
    }
}
