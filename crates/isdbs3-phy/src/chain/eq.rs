//! Stage 4: the 11-tap equaliser FIR and its training sums (module docs
//! of [`crate::chain`], "Stage 4"), and the host-side solve.

use super::fx::{round_shift, sat16};
use crate::Cplx;
use crate::eq::TAPS;

/// Fractional bits of a tap (Q2.14).
pub const TAP_FRAC: u32 = 14;
/// Centre tap index (the output is aligned with its input: delay 5).
pub const CENTRE: usize = TAPS / 2;
/// Upper-triangle entries of R (r ≤ c): 66.
pub const R_ENTRIES: usize = TAPS * (TAPS + 1) / 2;

/// The identity taps (1.0 on the centre tap).
pub fn identity() -> [[i16; 2]; TAPS] {
    let mut w = [[0; 2]; TAPS];
    w[CENTRE] = [1 << TAP_FRAC, 0];
    w
}

/// The (r, c) of R's 66 entries in their order ([`r_index`]).
pub fn upper() -> Vec<(usize, usize)> {
    (0..TAPS)
        .flat_map(|r| (r..TAPS).map(move |c| (r, c)))
        .collect()
}

/// Index of `R[r][c]` (r ≤ c) in the upper-triangle order: row by row.
pub fn r_index(r: usize, c: usize) -> usize {
    debug_assert!(r <= c && c < TAPS);
    r * TAPS - r * (r + 1) / 2 + c
}

/// The normal-equation sums of one frame's training symbols, as the HDL
/// latches them for the host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sums {
    /// Frame counter of the frame summed.
    pub frame: u32,
    /// Training symbols summed.
    pub count: u32,
    /// `R[r][c] = Σ conj(x_r) · x_c` for r ≤ c, row by row (66 complex,
    /// 48-bit signed in the HDL).
    pub r: Vec<[i64; 2]>,
    /// `p[r] = Σ conj(x_r) · d` (11 complex, 48 bits).
    pub p: Vec<[i64; 2]>,
}

impl Sums {
    fn new(frame: u32) -> Self {
        Self {
            frame,
            count: 0,
            r: vec![[0; 2]; R_ENTRIES],
            p: vec![[0; 2]; TAPS],
        }
    }
}

/// `conj(a) · b` in integers.
fn cmul_conj(a: [i16; 2], b: [i16; 2]) -> [i64; 2] {
    let (ar, ai) = (i64::from(a[0]), i64::from(a[1]));
    let (br, bi) = (i64::from(b[0]), i64::from(b[1]));
    [ar * br + ai * bi, ar * bi - ai * br]
}

/// Stage 4's state: the delay line, the taps in use and the training
/// accumulators.
#[derive(Debug, Clone)]
pub struct Equaliser {
    /// The last 11 inputs, newest at index 0 after a push.
    line: [[i16; 2]; TAPS],
    /// Inputs since the restart.
    filled: u64,
    /// The taps in use (Q2.14).
    pub taps: [[i16; 2]; TAPS],
    /// The frame whose sums produced them (`None`: identity).
    pub taps_from: Option<u32>,
    acc: Option<Sums>,
}

impl Default for Equaliser {
    fn default() -> Self {
        Self::new()
    }
}

impl Equaliser {
    /// Identity taps, nothing summed.
    pub fn new() -> Self {
        Self {
            line: [[0; 2]; TAPS],
            filled: 0,
            taps: identity(),
            taps_from: None,
            acc: None,
        }
    }

    /// A restart: the line cleared, an open sum dropped; the taps stay.
    pub fn restart(&mut self) {
        self.line = [[0; 2]; TAPS];
        self.filled = 0;
        self.acc = None;
    }

    /// Push one input `y` (index n + 5); returns the output for index n
    /// (`z_n = sat16(round_shift(Σ_k w_k · y_{n+5−k}, 14))`, complex) once
    /// 6 inputs are in (the line's older end is zeros until 11 are).
    pub fn push(&mut self, y: [i16; 2]) -> Option<[i16; 2]> {
        self.line.copy_within(0..TAPS - 1, 1);
        self.line[0] = y;
        self.filled += 1;
        (self.filled > CENTRE as u64).then(|| self.output())
    }

    /// The output for the line as it is (index n = newest − 5).
    pub fn output(&self) -> [i16; 2] {
        let (mut re, mut im) = (0i64, 0i64);
        for (w, x) in self.taps.iter().zip(&self.line) {
            let (wr, wi) = (i64::from(w[0]), i64::from(w[1]));
            let (xr, xi) = (i64::from(x[0]), i64::from(x[1]));
            re += wr * xr - wi * xi;
            im += wr * xi + wi * xr;
        }
        [
            sat16(round_shift(re, TAP_FRAC)),
            sat16(round_shift(im, TAP_FRAC)),
        ]
    }

    /// Whether the line holds 11 inputs since the restart (training is
    /// summed only then).
    pub fn full(&self) -> bool {
        self.filled >= TAPS as u64
    }

    /// Start summing frame `frame` (an open sum is dropped).
    pub fn open(&mut self, frame: u32) {
        self.acc = Some(Sums::new(frame));
    }

    /// The symbol at the line's centre (index n) is a training symbol
    /// with reference `d`: add its products to the open sum (none open,
    /// or the line not full: nothing). x_k = y_{n+5−k} = `line[k]`.
    pub fn train(&mut self, d: [i16; 2]) {
        if !self.full() {
            return;
        }
        let Some(s) = self.acc.as_mut() else {
            return;
        };
        let mut i = 0;
        for r in 0..TAPS {
            for c in r..TAPS {
                let v = cmul_conj(self.line[r], self.line[c]);
                s.r[i][0] += v[0];
                s.r[i][1] += v[1];
                i += 1;
            }
            let v = cmul_conj(self.line[r], d);
            s.p[r][0] += v[0];
            s.p[r][1] += v[1];
        }
        s.count += 1;
    }

    /// Close the open sum and return it.
    pub fn close(&mut self) -> Option<Sums> {
        self.acc.take()
    }

    /// Load taps solved from frame `from`'s sums.
    pub fn load(&mut self, taps: [[i16; 2]; TAPS], from: u32) {
        self.taps = taps;
        self.taps_from = Some(from);
    }
}

/// The host side: solve the integer sums in `f64` (`eq::solve`, the
/// float receiver's Gaussian elimination; the lower triangle filled as
/// the conjugate of the upper) and round the taps to Q2.14, saturated.
/// `None` for a singular system or an empty sum.
pub fn solve_sums(s: &Sums) -> Option<[[i16; 2]; TAPS]> {
    let w = solve_float(s)?;
    let q = |v: f64| -> i16 {
        // rounded and clamped to the i16 range, so the cast is exact
        #[allow(clippy::cast_possible_truncation)]
        let x = (v * f64::from(1u32 << TAP_FRAC))
            .round()
            .clamp(f64::from(i16::MIN), f64::from(i16::MAX)) as i16;
        x
    };
    Some(std::array::from_fn(|k| [q(w[k].re), q(w[k].im)]))
}

/// [`solve_sums`] before the rounding to Q2.14.
pub fn solve_float(s: &Sums) -> Option<Vec<Cplx>> {
    if s.count == 0 {
        return None;
    }
    // sums of 48 bits at most: exact in f64 up to 2⁵³
    #[allow(clippy::cast_precision_loss)]
    let c = |v: [i64; 2]| Cplx::new(v[0] as f64, v[1] as f64);
    let mut a = vec![vec![Cplx::new(0.0, 0.0); TAPS]; TAPS];
    for (i, &(r, col)) in upper().iter().enumerate() {
        let v = c(s.r[i]);
        a[r][col] = v;
        a[col][r] = v.conj();
    }
    let b = s.p.iter().map(|&v| c(v)).collect();
    crate::eq::solve(a, b)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_passes_the_input_through_with_delay_five() {
        let mut e = Equaliser::new();
        let mut out = Vec::new();
        for n in 0..40i16 {
            if let Some(z) = e.push([n, -n]) {
                out.push(z);
            }
        }
        assert_eq!(out.len(), 35);
        assert!(
            out.iter()
                .enumerate()
                .all(|(i, z)| *z == [i16::try_from(i).unwrap(), -i16::try_from(i).unwrap()])
        );
    }

    #[test]
    fn r_index_is_row_major_upper_triangle() {
        let mut i = 0;
        for r in 0..TAPS {
            for c in r..TAPS {
                assert_eq!(r_index(r, c), i);
                i += 1;
            }
        }
        assert_eq!(i, R_ENTRIES);
    }

    /// A channel y = x + 0.2·x(−1) is inverted by the trained taps.
    #[test]
    fn training_on_a_known_channel_inverts_it() {
        let mut s = 7u32;
        let mut rnd = || {
            s = s.wrapping_mul(1_103_515_245).wrapping_add(12_345);
            if s >> 31 == 0 { 724i16 } else { -724 }
        };
        let x: Vec<[i16; 2]> = (0..3000).map(|_| [rnd(), rnd()]).collect();
        let y: Vec<[i16; 2]> = (0..x.len())
            .map(|n| {
                let p = if n > 0 { x[n - 1] } else { [0, 0] };
                [x[n][0] + p[0] / 5, x[n][1] + p[1] / 5]
            })
            .collect();
        let mut e = Equaliser::new();
        e.open(0);
        for n in 0..y.len() {
            e.push(y[n]);
            if n >= 5 {
                e.train(x[n - 5]);
            }
        }
        let sums = e.close().unwrap();
        let w = solve_sums(&sums).unwrap();
        // centre ≈ 1, the next tap ≈ −0.2
        assert!((i32::from(w[CENTRE][0]) - 16_384).abs() < 300, "{w:?}");
        assert!((i32::from(w[CENTRE + 1][0]) + 3_277).abs() < 300, "{w:?}");
    }
}
