//! The lane-parallel kernels of the fixed-point LDPC decoder
//! ([`crate::ldpc::fixed`]): one `i8` per lane, `W` lanes.
//!
//! Two back ends with the same per-lane arithmetic:
//!
//! - **portable** (any compiler): fixed-width `[i8; W]` arrays processed
//!   by branch-free loops that LLVM vectorises for the target (SSE2 on
//!   plain x86-64, NEON on aarch64, AVX2 / AVX-512 when built for them);
//! - **`std::simd`** (`cfg(isdb_fec_portable_simd)`, set by the build
//!   script when the compiler accepts `#![feature(portable_simd)]`, i.e.
//!   a nightly toolchain, unless `ISDB_FEC_NO_NIGHTLY_SIMD` is set): the
//!   same operations on `Simd<i8, W>`.
//!
//! Both saturate the same way and pick the same min1 edge, so their
//! results are equal lane for lane (tested in both builds against the
//! scalar `W` = 1 decode).

/// One value per lane.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(transparent)]
pub struct Lane<const W: usize>(pub [i8; W]);

impl<const W: usize> Lane<W> {
    /// Every lane `x`.
    pub const fn splat(x: i8) -> Self {
        Self([x; W])
    }
}

/// Which back end this build uses.
pub const BACKEND: &str = if cfg!(isdb_fec_portable_simd) {
    "std::simd (nightly)"
} else {
    "portable arrays (auto-vectorised)"
};

#[cfg(not(isdb_fec_portable_simd))]
mod imp {
    use super::Lane;

    /// One check node: see `ldpc::fixed`. `vars` are the variable indices
    /// of the check's edges, `r` their messages (updated), `t` scratch.
    ///
    /// Branch-free per lane so that LLVM vectorises the lane loops:
    /// min2 = max(min1, min(min2, a)), min1 = min(min1, a) keeps the two
    /// smallest (min1 ≤ min2 throughout), and the min1 edge moves only on a
    /// strictly smaller value (the first edge wins ties).
    #[inline]
    pub fn check_node<const W: usize>(
        vars: &[u32],
        l: &mut [Lane<W>],
        r: &mut [Lane<W>],
        t: &mut Vec<Lane<W>>,
        offset: i8,
    ) {
        t.clear();
        let mut min1 = [i8::MAX; W];
        let mut min2 = [i8::MAX; W];
        let mut imin = [0i8; W];
        let mut neg = [0i8; W]; // sign parity: 0 or -1
        for (e, (&v, re)) in vars.iter().zip(r.iter()).enumerate() {
            let lv = &l[v as usize].0;
            // an edge of one check: the B44 check degree is at most 32
            // (tools/ldpc-check-degree.py), inside i8
            #[allow(clippy::cast_possible_truncation)]
            let e = e as i8;
            let mut te = [0i8; W];
            for i in 0..W {
                let x = lv[i].saturating_sub(re.0[i]);
                te[i] = x;
                let a = x.saturating_abs();
                neg[i] ^= x >> 7;
                // all ones where a < min1: a blend LLVM vectorises
                let lt = -i8::from(a < min1[i]);
                imin[i] = (imin[i] & !lt) | (e & lt);
                min2[i] = min1[i].max(min2[i].min(a));
                min1[i] = min1[i].min(a);
            }
            t.push(Lane(te));
        }
        let mut m1 = [0i8; W];
        let mut m2 = [0i8; W];
        for i in 0..W {
            m1[i] = min1[i].saturating_sub(offset).max(0);
            m2[i] = min2[i].saturating_sub(offset).max(0);
        }
        for (e, ((&v, re), te)) in vars.iter().zip(r.iter_mut()).zip(t.iter()).enumerate() {
            let lv = &mut l[v as usize].0;
            // an edge of one check: the B44 check degree is at most 32
            // (tools/ldpc-check-degree.py), inside i8
            #[allow(clippy::cast_possible_truncation)]
            let e = e as i8;
            for i in 0..W {
                let at = -i8::from(imin[i] == e); // all ones on the min1 edge
                let mag = (m1[i] & !at) | (m2[i] & at);
                let s = neg[i] ^ (te.0[i] >> 7); // -1: negative
                let m = (mag ^ s).wrapping_sub(s); // ±mag
                re.0[i] = m;
                lv[i] = te.0[i].saturating_add(m);
            }
        }
    }

    /// Per lane: −1 if the hard decisions of `vars` have odd parity, else 0.
    #[inline]
    pub fn sign_parity<const W: usize>(vars: &[u32], l: &[Lane<W>]) -> Lane<W> {
        let mut neg = [0i8; W];
        for &v in vars {
            let lv = &l[v as usize].0;
            for i in 0..W {
                neg[i] ^= lv[i] >> 7;
            }
        }
        Lane(neg)
    }
}

#[cfg(isdb_fec_portable_simd)]
mod imp {
    use super::Lane;
    use std::simd::cmp::{SimdOrd, SimdPartialEq, SimdPartialOrd};
    use std::simd::num::SimdInt;
    use std::simd::{Select, Simd};

    #[inline(always)]
    fn v<const W: usize>(x: &Lane<W>) -> Simd<i8, W> {
        Simd::from_array(x.0)
    }

    /// One check node: see `ldpc::fixed` (same arithmetic as the portable
    /// back end).
    #[inline]
    pub fn check_node<const W: usize>(
        vars: &[u32],
        l: &mut [Lane<W>],
        r: &mut [Lane<W>],
        t: &mut Vec<Lane<W>>,
        offset: i8,
    ) {
        t.clear();
        let mut min1 = Simd::<i8, W>::splat(i8::MAX);
        let mut min2 = Simd::<i8, W>::splat(i8::MAX);
        let mut imin = Simd::<i8, W>::splat(0);
        let mut neg = Simd::<i8, W>::splat(0);
        let seven = Simd::<i8, W>::splat(7);
        for (e, (&var, re)) in vars.iter().zip(r.iter()).enumerate() {
            // an edge of one check: the B44 check degree is at most 32
            // (tools/ldpc-check-degree.py), inside i8
            #[allow(clippy::cast_possible_truncation)]
            let e = e as i8;
            let x = v(&l[var as usize]).saturating_sub(v(re));
            let a = x.saturating_abs();
            neg ^= x >> seven;
            imin = a.simd_lt(min1).select(Simd::splat(e), imin);
            min2 = min1.simd_max(min2.simd_min(a));
            min1 = min1.simd_min(a);
            t.push(Lane(x.to_array()));
        }
        let zero = Simd::<i8, W>::splat(0);
        let off = Simd::<i8, W>::splat(offset);
        let m1 = min1.saturating_sub(off).simd_max(zero);
        let m2 = min2.saturating_sub(off).simd_max(zero);
        for (e, ((&var, re), te)) in vars.iter().zip(r.iter_mut()).zip(t.iter()).enumerate() {
            // an edge of one check: the B44 check degree is at most 32
            // (tools/ldpc-check-degree.py), inside i8
            #[allow(clippy::cast_possible_truncation)]
            let e = e as i8;
            let tv = v(te);
            let mag = imin.simd_eq(Simd::splat(e)).select(m2, m1);
            let s = neg ^ (tv >> seven);
            let m = (mag ^ s) - s;
            re.0 = m.to_array();
            l[var as usize].0 = tv.saturating_add(m).to_array();
        }
    }

    /// Per lane: −1 if the hard decisions of `vars` have odd parity, else 0.
    #[inline]
    pub fn sign_parity<const W: usize>(vars: &[u32], l: &[Lane<W>]) -> Lane<W> {
        let seven = Simd::<i8, W>::splat(7);
        let mut neg = Simd::<i8, W>::splat(0);
        for &var in vars {
            neg ^= v(&l[var as usize]) >> seven;
        }
        Lane(neg.to_array())
    }
}

pub use imp::{check_node, sign_parity};
