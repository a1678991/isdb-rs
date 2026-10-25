//! The eleven LDPC codes of ISDB-S3 (ARIB STD-B44 v2.1 §3.4 (2)), built
//! once from the transcribed address tables ([`crate::ldpc_tables`]).

use crate::params::{CodeRate, N_LDPC};
use isdb_fec::ldpc::Code;
use std::sync::OnceLock;

/// The code of rate `r` (built on first use, shared afterwards).
pub fn code(r: CodeRate) -> &'static Code {
    static CODES: [OnceLock<Code>; 11] = [const { OnceLock::new() }; 11];
    let i = crate::params::RATES
        .iter()
        .position(|&x| x == r.numerator())
        .expect("rate");
    CODES[i].get_or_init(|| {
        let (num, rows) = crate::ldpc_tables::TABLES[i];
        assert_eq!(num, u32::from(r.numerator()));
        Code::new(N_LDPC, rows)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spec_k_matches_table_3_2_1() {
        for r in crate::params::RATES {
            let r = CodeRate::new(r).unwrap();
            assert_eq!(code(r).k(), r.k_ldpc());
        }
    }

    #[test]
    fn spec_example_81_120_first_parity_addresses() {
        // §3.4 (2) worked example: i0 of rate 81/120 hits p4958 … p13460,
        // and i1 the same positions + q = 39.
        let (_, rows) = crate::ldpc_tables::TABLES[4];
        assert_eq!(
            rows[0],
            &[
                4958, 6639, 6721, 8238, 9540, 9550, 10491, 11742, 11641, 12092, 13056, 13460
            ]
        );
        assert_eq!(CodeRate::new(81).unwrap().q(), 39);
    }
}
