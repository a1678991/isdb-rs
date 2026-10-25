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

use crate::params::{FORMAT_VERSION, PHASES, Params};

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
        u32::from(ID_MAGIC) << 16 | u32::from(self.version & 0xF) << 12 | (self.taps as u32 & 0xFF)
    }

    /// Whether this host can drive it (same format, enough taps for `p`).
    pub fn supports(&self, p: &Params) -> bool {
        self.version == FORMAT_VERSION && self.taps == p.taps()
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
        for (ph, row) in bank.iter().enumerate().take(PHASES) {
            let vals: Vec<u32> = row.iter().map(|&c| c as u32).collect();
            self.write(base + (ph as u16) * 64, &vals, out);
        }
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
}
