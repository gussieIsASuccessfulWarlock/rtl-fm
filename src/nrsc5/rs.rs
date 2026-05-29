//! Reed-Solomon (255,223) decoder over GF(256).

use crate::nrsc5::consts::{RS_K, RS_N, RS_PARITY, RS_GEN};

// GF(256) tables: log, antilog
static GF_LOG: std::sync::LazyLock<[i16; 256]> = std::sync::LazyLock::new(|| {
    let mut log = [-1i16; 256];
    let mut val = 1u16;
    for i in 0..255 {
        log[val as usize] = i as i16;
        val <<= 1;
        if val & 0x100 != 0 {
            val ^= RS_GEN;
        }
    }
    log[0] = -1;
    log
});

static GF_EXP: std::sync::LazyLock<[u8; 512]> = std::sync::LazyLock::new(|| {
    let mut exp = [0u8; 512];
    let mut val = 1u16;
    for exp_i in exp.iter_mut().take(255) {
        *exp_i = val as u8;
        val <<= 1;
        if val & 0x100 != 0 {
            val ^= RS_GEN;
        }
    }
    for i in 255..511 {
        exp[i] = exp[i - 255];
    }
    exp
});

fn gf_mul(a: u8, b: u8) -> u8 {
    if a == 0 || b == 0 {
        return 0;
    }
    let sum = GF_LOG[a as usize] as i32 + GF_LOG[b as usize] as i32;
    GF_EXP[sum as usize]
}

fn gf_inv(a: u8) -> u8 {
    if a == 0 { return 0; }
    GF_EXP[255 - GF_LOG[a as usize] as usize]
}

fn gf_pow(a: u8, n: i32) -> u8 {
    if a == 0 { return 0; }
    if n == 0 { return 1; }
    let log_a = GF_LOG[a as usize] as i32;
    let mut sum = log_a * n;
    sum %= 255;
    if sum < 0 { sum += 255; }
    GF_EXP[sum as usize]
}

/// Compute syndromes for received codeword (length RS_N).
fn syndromes(data: &[u8]) -> [u8; RS_PARITY] {
    let mut syn = [0u8; RS_PARITY];
    for (i, syn_i) in syn.iter_mut().enumerate() {
        let mut s = 0u8;
        let alpha = gf_pow(2, i as i32);
        for &byte in data.iter() {
            s = gf_mul(s, alpha) ^ byte;
        }
        *syn_i = s;
    }
    syn
}

/// Berlekamp-Massey algorithm to find error locator polynomial.
#[allow(non_snake_case)]
fn berlekamp_massey(syn: &[u8; RS_PARITY]) -> [u8; RS_PARITY + 1] {
    let mut C = [0u8; RS_PARITY + 1];
    let mut B = [0u8; RS_PARITY + 1];
    C[0] = 1;
    B[0] = 1;
    let mut L = 0;
    let mut m = 1;
    let mut b = 1u8;

    for n in 0..RS_PARITY {
        let mut d = syn[n];
        for i in 1..=L {
            d ^= gf_mul(C[i], syn[n - i]);
        }
        if d == 0 {
            m += 1;
        } else {
            let T = C;
            let factor = gf_mul(d, gf_inv(b));
            for i in m..=RS_PARITY {
                C[i] ^= gf_mul(factor, B[i - m]);
            }
            if 2 * L <= n {
                L = n + 1 - L;
                B = T;
                b = d;
                m = 1;
            } else {
                m += 1;
            }
        }
    }
    C
}

/// Chien search: find roots (error positions) of the error locator polynomial.
fn chien_search(locator: &[u8; RS_PARITY + 1]) -> Vec<usize> {
    let mut errors = Vec::new();
    for i in 0..RS_N {
        let x = gf_pow(2, i as i32);
        let mut val = locator[0];
        for (j, &loc_j) in locator.iter().enumerate().skip(1) {
            val ^= gf_mul(loc_j, gf_pow(x, j as i32));
        }
        if val == 0 {
            errors.push(RS_N - 1 - i);
        }
    }
    errors
}

fn locator_degree(locator: &[u8; RS_PARITY + 1]) -> usize {
    locator.iter().rposition(|&c| c != 0).unwrap_or(0)
}

/// Forney algorithm: compute error values at the error positions.
fn forney(syn: &[u8; RS_PARITY], error_pos: &[usize]) -> Vec<u8> {
    let mut values = Vec::with_capacity(error_pos.len());
    for &pos in error_pos {
        let x = gf_pow(2, (RS_N - 1 - pos) as i32);
        let mut num = 0u8;
        let mut den = 0u8;
        for i in 0..RS_PARITY {
            num = gf_mul(num, x) ^ syn[RS_PARITY - 1 - i];
        }
        for &pos2 in error_pos.iter() {
            if pos2 != pos {
                let xj = gf_pow(2, (RS_N - 1 - pos2) as i32);
                den = gf_mul(den, x ^ xj);
            }
        }
        let val = gf_mul(num, gf_inv(den));
        values.push(val);
    }
    values
}

/// Decode a received RS(255,223) codeword.
/// Returns Ok((data, corrected)) on success, Err on failure.
pub fn decode_rs(data: &mut [u8]) -> Result<&[u8], ()> {
    debug_assert_eq!(data.len(), RS_N);
    let syn = syndromes(data);
    if syn.iter().all(|&s| s == 0) {
        return Ok(&data[..RS_K]);
    }

    let locator = berlekamp_massey(&syn);
    let degree = locator_degree(&locator);
    if degree == 0 || degree > RS_PARITY / 2 {
        return Err(());
    }
    let error_pos = chien_search(&locator);
    if error_pos.len() != degree || error_pos.len() > RS_PARITY / 2 {
        return Err(());
    }

    let error_vals = forney(&syn, &error_pos);
    for (&pos, &val) in error_pos.iter().zip(error_vals.iter()) {
        if pos < RS_N {
            data[pos] ^= val;
        }
    }

    // Verify corrected codeword
    let syn2 = syndromes(data);
    if syn2.iter().all(|&s| s == 0) {
        Ok(&data[..RS_K])
    } else {
        Err(())
    }
}

/// Depuncture and deframe raw P1 blocks into RS(255,223) codewords.
/// The raw P1 blocks are received from the Viterbi decoder.
pub fn depuncture_p1(raw: &[u8]) -> Vec<Vec<u8>> {
    let mut blocks = Vec::new();
    let block_size = RS_N; // 255 bytes per RS codeword
    for chunk in raw.chunks(block_size) {
        if chunk.len() < block_size {
            break;
        }
        blocks.push(chunk.to_vec());
    }
    blocks
}

/// Decode a sequence of RS blocks. Returns Ok with concatenated decoded data.
pub fn decode_blocks(blocks: &mut [Vec<u8>]) -> Result<Vec<u8>, ()> {
    let mut result = Vec::with_capacity(blocks.len() * RS_K);
    for block in blocks.iter_mut() {
        let decoded = decode_rs(block)?;
        result.extend_from_slice(decoded);
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_arbitrary_codeword() {
        let mut data = [0u8; RS_N];
        for (i, b) in data.iter_mut().enumerate() {
            *b = (i.wrapping_mul(37).wrapping_add(11) & 0xff) as u8;
        }
        assert!(decode_rs(&mut data).is_err());
    }

    #[test]
    fn accepts_zero_codeword() {
        let mut data = [0u8; RS_N];
        assert!(decode_rs(&mut data).is_ok());
    }
}
