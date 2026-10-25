//! Run-time choice of the instruction set for [`crate::fast::FastFrontend`]'s
//! dot products, and the crate's one `unsafe` call.
//!
//! The kernels are written once, generically, and compiled three times:
//! for the build's baseline (SSE2 on x86-64, NEON on aarch64: whatever
//! LLVM vectorises without extra features), inside
//! `#[target_feature(enable = "avx2,…")]` functions, and (when the
//! compiler supports it, `cfg(isdb_frontend_avx512)`, Rust ≥ 1.89) inside
//! `#[target_feature(enable = "avx512f,avx512bw,avx512vl,…")]` ones. Every
//! instantiation computes the same integers: the choice changes speed,
//! never the output (tested against [`crate::fixed::Frontend`] for each
//! instruction set the host runs, `tests/fast.rs`).
//!
//! A `#[target_feature]` function may only be called without `unsafe`
//! from code that has the same features, so calling one after a run-time
//! check needs one `unsafe` call. This module is the only place in the
//! crate where that happens (the private function `run`); the crate allows `unsafe_code`
//! there and nowhere else.
//!
//! [`Isa::best`] is AVX2 when the CPU (detected at run time) has it, else
//! the baseline: measured on a Zen 4 (Ryzen 9 7950X), AVX-512 is *slower*
//! than AVX2 here (ISDB-S3 0.92 vs 0.86 s, ISDB-S 0.35 vs 0.32 s per
//! 0.826 s block), because Zen 4 executes 512-bit integer operations as
//! two 256-bit halves and the 32-tap rows leave one 512-bit block plus a
//! fold. AVX-512 is kept for CPUs with full-width units and is chosen
//! only on request. The environment variable `ISDB_SIMD` (`baseline`,
//! `avx2`, `avx512`) selects the instruction set, for comparisons and to
//! rule the vector code out when debugging; a request the CPU or build
//! cannot run falls back to the widest available below it.

use std::fmt;

/// An instruction set the front end's kernels are compiled for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Isa {
    /// The build's target features only (SSE2 on x86-64 by default).
    Baseline,
    /// AVX2, FMA, BMI1/2, LZCNT, POPCNT (x86-64-v3).
    Avx2,
    /// AVX-512 F/BW/VL/DQ/CD on top of AVX2 (x86-64-v4).
    Avx512,
}

impl Isa {
    /// Every instruction set, narrowest first.
    pub const ALL: [Isa; 3] = [Isa::Baseline, Isa::Avx2, Isa::Avx512];

    /// The name `ISDB_SIMD` takes.
    pub fn name(self) -> &'static str {
        match self {
            Self::Baseline => "baseline",
            Self::Avx2 => "avx2",
            Self::Avx512 => "avx512",
        }
    }

    /// Parse an `ISDB_SIMD` value.
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "baseline" | "scalar" | "none" | "sse2" => Some(Self::Baseline),
            "avx2" => Some(Self::Avx2),
            "avx512" | "avx-512" => Some(Self::Avx512),
            _ => None,
        }
    }

    /// Whether this CPU runs it and this build has kernels for it.
    pub fn available(self) -> bool {
        match self {
            Self::Baseline => true,
            Self::Avx2 => avx2_detected(),
            Self::Avx512 => cfg!(isdb_frontend_avx512) && avx2_detected() && avx512_detected(),
        }
    }

    /// The widest available instruction set.
    pub fn detected() -> Self {
        Self::ALL
            .into_iter()
            .rev()
            .find(|i| i.available())
            .unwrap_or(Self::Baseline)
    }

    /// The instruction set the front end uses: the one `ISDB_SIMD` names
    /// (or the widest available below it), else AVX2 if available, else
    /// the baseline (module documentation: AVX-512 only on request). An
    /// unknown `ISDB_SIMD` value is ignored.
    pub fn best() -> Self {
        let d = Self::detected();
        match std::env::var("ISDB_SIMD")
            .ok()
            .as_deref()
            .and_then(Self::parse)
        {
            Some(want) => want.min(d),
            None => d.min(Self::Avx2),
        }
    }
}

impl fmt::Display for Isa {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

#[cfg(target_arch = "x86_64")]
fn avx2_detected() -> bool {
    std::arch::is_x86_feature_detected!("avx2")
        && std::arch::is_x86_feature_detected!("fma")
        && std::arch::is_x86_feature_detected!("bmi1")
        && std::arch::is_x86_feature_detected!("bmi2")
        && std::arch::is_x86_feature_detected!("lzcnt")
        && std::arch::is_x86_feature_detected!("popcnt")
}

#[cfg(not(target_arch = "x86_64"))]
fn avx2_detected() -> bool {
    false
}

#[cfg(all(target_arch = "x86_64", isdb_frontend_avx512))]
fn avx512_detected() -> bool {
    std::arch::is_x86_feature_detected!("avx512f")
        && std::arch::is_x86_feature_detected!("avx512bw")
        && std::arch::is_x86_feature_detected!("avx512vl")
        && std::arch::is_x86_feature_detected!("avx512dq")
        && std::arch::is_x86_feature_detected!("avx512cd")
}

#[cfg(not(all(target_arch = "x86_64", isdb_frontend_avx512)))]
fn avx512_detected() -> bool {
    false
}

/// A front-end step compiled for one instruction set: a plain function,
/// or a `#[target_feature]` one coerced to an `unsafe fn` pointer.
pub(crate) type Kernel<S, A> = unsafe fn(&mut S, A);

/// A kernel for each instruction set, built by [`crate::fast`].
pub(crate) struct Kernels<S, A> {
    pub(crate) baseline: fn(&mut S, A),
    #[cfg(target_arch = "x86_64")]
    pub(crate) avx2: Kernel<S, A>,
    #[cfg(all(target_arch = "x86_64", isdb_frontend_avx512))]
    pub(crate) avx512: Kernel<S, A>,
}

/// Run `k`'s kernel for `isa` on `(s, a)`. `isa` must come from
/// [`Isa::best`] (or [`Isa::available`] be true for it): that is the
/// check the `unsafe` call relies on.
#[allow(unsafe_code)]
pub(crate) fn run<S, A>(k: &Kernels<S, A>, isa: Isa, s: &mut S, a: A) {
    match isa {
        Isa::Baseline => (k.baseline)(s, a),
        #[cfg(target_arch = "x86_64")]
        Isa::Avx2 if avx2_detected() => {
            // SAFETY: `k.avx2` is a `#[target_feature(enable = "avx2,fma,
            // bmi1,bmi2,lzcnt,popcnt")]` function; the guard has just
            // detected every one of those features on this CPU, which is
            // the only requirement of calling it.
            unsafe { (k.avx2)(s, a) }
        }
        #[cfg(all(target_arch = "x86_64", isdb_frontend_avx512))]
        Isa::Avx512 if avx2_detected() && avx512_detected() => {
            // SAFETY: `k.avx512` is a `#[target_feature]` function on the
            // AVX2 features above plus avx512f/bw/vl/dq/cd; the guard has
            // just detected all of them on this CPU.
            unsafe { (k.avx512)(s, a) }
        }
        // a narrower request than the kernels exist for, or features not
        // present: the baseline kernel (same output)
        _ => (k.baseline)(s, a),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_round_trip_and_order() {
        for i in Isa::ALL {
            assert_eq!(Isa::parse(i.name()), Some(i));
        }
        assert_eq!(Isa::parse("AVX-512"), Some(Isa::Avx512));
        assert_eq!(Isa::parse("nope"), None);
        assert!(Isa::Baseline < Isa::Avx2 && Isa::Avx2 < Isa::Avx512);
        assert!(Isa::Baseline.available());
        assert!(Isa::detected().available());
    }
}
