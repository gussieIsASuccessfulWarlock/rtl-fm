//! Channel equalization and soft demapping for NRSC-5.

use std::sync::LazyLock;

use num_complex::Complex;

use crate::nrsc5::consts::{OFDM_FFT_LEN, REF_SUBCARRIERS};

/// Soft demap QPSK symbol to two 8-bit LLRs (0..255, 128 = erasure).
pub fn soft_demap_qpsk(z: Complex<f32>) -> (u8, u8) {
    let scale = 90.0f32;
    let i = 127.5 + z.re * scale;
    let q = 127.5 + z.im * scale;
    (i.clamp(0.0, 255.0) as u8, q.clamp(0.0, 255.0) as u8)
}

/// All P1 data subcarrier indices (signed, 0-centered) for FM Hybrid mode.
pub static P1_DATA_SC: LazyLock<Vec<i16>> = LazyLock::new(|| {
    let ref_set: std::collections::HashSet<i16> = REF_SUBCARRIERS.iter().copied().collect();
    let mut sc = Vec::new();
    for s in -1092i16..=-225 {
        if !ref_set.contains(&s) {
            sc.push(s);
        }
    }
    for s in 225..=1092 {
        if !ref_set.contains(&s) {
            sc.push(s);
        }
    }
    sc
});

fn bin_idx(sc: i16) -> usize {
    if sc >= 0 { sc as usize } else { (OFDM_FFT_LEN as i16 + sc) as usize }
}

/// Equalize data subcarriers and produce soft bits for one symbol.
pub fn equalize_and_demap(
    fft_bins: &[Complex<f32>],
    ref_estimates: &[Complex<f32>],
    data_sc: &[i16],
    out_soft: &mut Vec<u8>,
) {
    for &sc in data_sc {
        let idx = bin_idx(sc);
        if idx >= fft_bins.len() {
            continue;
        }
        let z = fft_bins[idx];
        let ch = interpolate_channel(sc, ref_estimates);
        let w = ch.norm_sqr().max(1e-10);
        let eq = Complex::new(
            (ch.re * z.re + ch.im * z.im) / w,
            (ch.re * z.im - ch.im * z.re) / w,
        );
        let (si, sq) = soft_demap_qpsk(eq);
        out_soft.push(si);
        out_soft.push(sq);
    }
}

fn interpolate_channel(sc: i16, ref_est: &[Complex<f32>]) -> Complex<f32> {
    let mut best_d = i16::MAX;
    let mut best = Complex::new(1.0, 0.0);
    for (&rs, &ch) in REF_SUBCARRIERS.iter().zip(ref_est.iter()) {
        let d = (sc - rs).abs();
        if d < best_d {
            best_d = d;
            best = ch;
        }
    }
    best
}
