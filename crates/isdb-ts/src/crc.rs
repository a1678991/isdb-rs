//! CRC_32 of ITU-T H.222.0 Annex A (the "CRC-32/MPEG-2" model).
//!
//! Annex A defines a 32-bit shift-register decoder with generator
//! polynomial x^32+x^26+x^23+x^22+x^16+x^12+x^11+x^10+x^8+x^7+x^5+x^4+x^2+x+1
//! (0x04C1_1DB7), registers initialised to all ones, input processed MSB
//! first, no output reflection or final XOR.  A section is valid when the
//! decoder output over the whole section *including* CRC_32 is zero.

/// Generator polynomial of Annex A (x^32 term implicit).
pub const POLY: u32 = 0x04C1_1DB7;

const fn table() -> [u32; 256] {
    let mut t = [0u32; 256];
    let mut i = 0;
    while i < 256 {
        let mut c = (i as u32) << 24;
        let mut k = 0;
        while k < 8 {
            c = if c & 0x8000_0000 != 0 {
                (c << 1) ^ POLY
            } else {
                c << 1
            };
            k += 1;
        }
        t[i] = c;
        i += 1;
    }
    t
}

static TABLE: [u32; 256] = table();

/// Continue a CRC computation from register state `crc`.
pub fn update(mut crc: u32, data: &[u8]) -> u32 {
    for &b in data {
        crc = (crc << 8) ^ TABLE[((crc >> 24) as u8 ^ b) as usize];
    }
    crc
}

/// CRC_32 of `data` with the Annex A initial state (all ones).
pub fn crc32(data: &[u8]) -> u32 {
    update(0xFFFF_FFFF, data)
}

/// Bit-serial reference implementation of the Annex A register model,
/// used only by tests to cross-check the table-driven [`crc32`].
pub fn crc32_bitwise(data: &[u8]) -> u32 {
    let mut reg = 0xFFFF_FFFFu32;
    for &byte in data {
        for i in (0..8).rev() {
            let bit = ((byte >> i) & 1) as u32;
            let fb = (reg >> 31) ^ bit;
            reg <<= 1;
            if fb != 0 {
                reg ^= POLY;
            }
        }
    }
    reg
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Standard check value of the CRC-32/MPEG-2 model (init FFFFFFFF,
    /// poly 04C11DB7, no reflection, no xorout) over ASCII "123456789".
    #[test]
    fn check_value() {
        assert_eq!(crc32(b"123456789"), 0x0376_E6E7);
        assert_eq!(crc32_bitwise(b"123456789"), 0x0376_E6E7);
    }

    /// Annex A: appending the CRC yields a zero register (decoder model).
    #[test]
    fn residue_is_zero() {
        let mut v = b"ARIB STD-B10".to_vec();
        let c = crc32(&v);
        v.extend_from_slice(&c.to_be_bytes());
        assert_eq!(crc32(&v), 0);
    }

    #[test]
    fn table_matches_bitwise() {
        let data: Vec<u8> = (0..=255u8).cycle().take(1000).collect();
        for n in [0, 1, 7, 188, 1000] {
            assert_eq!(crc32(&data[..n]), crc32_bitwise(&data[..n]));
        }
    }
}
