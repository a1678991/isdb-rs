//! Bit interleaving of the LDPC codeword for 8PSK, 16APSK and 32APSK
//! (ARIB STD-B44 v2.1 §3.7, Tables 3.7-1/2, Figs. 3.7-1 to 3.7-6).
//!
//! The 44 880-bit codeword is written column by column, top to bottom,
//! into a block of `bits` columns (3, 4, 5) and 44 880 / `bits` rows, then
//! read row by row: forward (column 1 = MSB first) or backward (the last
//! column first). Each row is one symbol's label, MSB first. π/2-BPSK and
//! QPSK slots are not interleaved: consecutive codeword bits fill the
//! label MSB first.
//!
//! Table 3.7-1 prints 9 876 rows for 32APSK; Figs. 3.7-5/6 print 8 976 =
//! 44 880 / 5, which is used (SPEC-NOTES "Interleaver rows").

use crate::params::{CodeRate, Modulation, N_LDPC};

/// Read-out direction (Table 3.7-2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// No interleaving (π/2-BPSK, QPSK).
    None,
    /// Row by row, column 1 (MSB) first.
    Forward,
    /// Row by row, the last column first.
    Backward,
}

/// Table 3.7-2: backward for the rates "below 2/5" (41/120 and 49/120, the
/// two rates left of the 1/2 column), and for 32APSK also at 7/9
/// (93/120); forward otherwise.
pub fn direction(m: Modulation, r: CodeRate) -> Direction {
    match m {
        Modulation::Bpsk | Modulation::Qpsk => Direction::None,
        _ if r.numerator() <= 49 => Direction::Backward,
        Modulation::Apsk32 if r.numerator() == 93 => Direction::Backward,
        _ => Direction::Forward,
    }
}

/// Permutation `perm` with `labels_bit[i] = codeword[perm[i]]`: the bit
/// order in which a codeword's bits leave the interleaver, symbol label
/// bits MSB first.
pub fn permutation(m: Modulation, r: CodeRate) -> Vec<u32> {
    let c = m.bits();
    let rows = N_LDPC / c;
    match direction(m, r) {
        Direction::None => (0..N_LDPC as u32).collect(),
        dir => {
            let mut p = Vec::with_capacity(N_LDPC);
            for row in 0..rows {
                for j in 0..c {
                    let col = if dir == Direction::Forward {
                        j
                    } else {
                        c - 1 - j
                    };
                    p.push((col * rows + row) as u32);
                }
            }
            p
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spec_table_3_7_1_rows() {
        assert_eq!(N_LDPC / 3, 14_960);
        assert_eq!(N_LDPC / 4, 11_220);
        assert_eq!(N_LDPC / 5, 8_976);
    }

    #[test]
    fn permutations_are_bijections() {
        for m in [Modulation::Psk8, Modulation::Apsk16, Modulation::Apsk32] {
            for r in [41, 93, 109] {
                let r = CodeRate::new(r).unwrap();
                let mut p = permutation(m, r);
                p.sort_unstable();
                assert!(p.iter().enumerate().all(|(i, &v)| i as u32 == v));
            }
        }
    }

    #[test]
    fn first_symbol_takes_the_first_bit_of_each_column() {
        let r = CodeRate::new(93).unwrap();
        let p = permutation(Modulation::Apsk16, r);
        assert_eq!(&p[..4], &[0, 11_220, 22_440, 33_660]);
        let p = permutation(Modulation::Apsk32, r); // backward at 7/9
        assert_eq!(&p[..2], &[4 * 8_976, 3 * 8_976]);
    }
}
