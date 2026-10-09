//! Conformance findings shared by every `check` function of this crate.
//!
//! Codecs in this crate are lossless and accept values the spec forbids;
//! the `check` functions (e.g. [`crate::packet::check`],
//! [`crate::psi::check_pmt`]) report such values as [`Violation`]s with the
//! H.222.0 clause that states the rule.

/// One violated rule of ITU-T H.222.0, with the clause stating it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Violation {
    /// Clause/table of H.222.0 (05/2006) stating the rule, e.g. `"§2.4.3.5"`.
    pub clause: &'static str,
    /// Human-readable description of the offending value.
    pub message: String,
}

impl Violation {
    /// Construct a violation.
    pub fn new(clause: &'static str, message: impl Into<String>) -> Self {
        Self { clause, message: message.into() }
    }
}

impl std::fmt::Display for Violation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "H.222.0 {}: {}", self.clause, self.message)
    }
}

/// Push a violation unless `ok`.
pub(crate) fn require(out: &mut Vec<Violation>, ok: bool, clause: &'static str, msg: impl FnOnce() -> String) {
    if !ok {
        out.push(Violation::new(clause, msg()));
    }
}

/// H.222.0 §2.1.64: "Unless otherwise specified … all reserved bits shall
/// be set to '1'" — push a violation if the `bits`-wide `value` is not all
/// ones.
pub(crate) fn reserved_ones(out: &mut Vec<Violation>, value: u64, bits: u32, name: &str) {
    let ones = if bits == 64 { u64::MAX } else { (1u64 << bits) - 1 };
    require(out, value == ones, "§2.1.64", || {
        format!("{name}: reserved bits are {value:#b}, shall be all '1' ({bits} bits)")
    });
}
