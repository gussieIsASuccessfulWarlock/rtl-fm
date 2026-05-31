//! Channel equalization + soft QPSK demap for NRSC-5 P1 data.
//!
//! For each partition, linearly interpolate the channel estimate
//! between the lower and upper reference (matching sync.c's
//! `(19+19j) / (k*smag19*upper + (19-k)*smag0*lower)` equalizer), then
//! divide the data sample by the interpolated channel and rotate by
//! +45° so the on-axis QPSK constellation lands on the C reference's
//! real/imaginary soft-decision axes.
//! Per data carrier we emit two i8 LLRs (real then imag), giving 360
//! carriers × 2 bits = 720 soft bits per OFDM symbol in the order the
//! PM interleaver expects.

use num_complex::Complex;

use crate::nrsc5::consts::{LB_START, PARTITION_WIDTH_FM, PM_PARTITIONS, UB_END};

pub fn equalize_and_demap(
    fft_bins: &[Complex<f32>],
    ref_est: &[Complex<f32>],
    bin_offset: i32,
    out_soft: &mut Vec<i8>,
) {
    let pw = PARTITION_WIDTH_FM;
    // LB partitions p=0..9: lower ref at REF_BINS_FM[p], upper at p+1.
    for p in 0..PM_PARTITIONS {
        let lower_ref = ref_est[p];
        let upper_ref = ref_est[p + 1];
        let lower_bin = LB_START + p * pw;
        emit_partition(
            fft_bins, lower_bin, lower_ref, upper_ref, bin_offset, out_soft,
        );
    }
    // UB partitions p=0..9: lower ref at REF_BINS_FM[21-p], upper at 20-p,
    // partition starts at bin UB_END - (10-p)*pw.
    for p in 0..PM_PARTITIONS {
        let lower_ref = ref_est[21 - p];
        let upper_ref = ref_est[20 - p];
        let lower_bin = UB_END - (PM_PARTITIONS - p) * pw;
        emit_partition(
            fft_bins, lower_bin, lower_ref, upper_ref, bin_offset, out_soft,
        );
    }
}

fn emit_partition(
    fft_bins: &[Complex<f32>],
    lower_bin: usize,
    lower_ref: Complex<f32>,
    upper_ref: Complex<f32>,
    bin_offset: i32,
    out_soft: &mut Vec<i8>,
) {
    let pw = PARTITION_WIDTH_FM;
    let p_f = pw as f32;
    for k in 1..pw {
        let bin = lower_bin + k;
        let Some(bin) = shifted_bin(bin, bin_offset, fft_bins.len()) else {
            out_soft.push(0);
            out_soft.push(0);
            continue;
        };
        if bin >= fft_bins.len() {
            out_soft.push(0);
            out_soft.push(0);
            continue;
        }
        let z = fft_bins[bin];
        let k_f = k as f32;
        // Linear-interpolated channel estimate at this carrier
        // (normalized by partition width so |ch| ≈ |channel|, not 19×).
        let ch = Complex::new(
            (k_f * upper_ref.re + (p_f - k_f) * lower_ref.re) / p_f,
            (k_f * upper_ref.im + (p_f - k_f) * lower_ref.im) / p_f,
        );
        let w = ch.norm_sqr().max(1e-10);
        // eq = z / ch
        let eq_re = (z.re * ch.re + z.im * ch.im) / w;
        let eq_im = (z.im * ch.re - z.re * ch.im) / w;
        // +45° rotation: multiply by (1+j), matching sync.c's
        // `(19+19i) / (...)` equalizer factor.
        let rot_re = eq_re - eq_im;
        let rot_im = eq_re + eq_im;
        let scale = 64.0f32;
        out_soft.push((rot_re * scale).clamp(-127.0, 127.0) as i8);
        out_soft.push((rot_im * scale).clamp(-127.0, 127.0) as i8);
    }
}

fn shifted_bin(bin: usize, offset: i32, len: usize) -> Option<usize> {
    let shifted = bin as i32 + offset;
    if shifted < 0 || shifted >= len as i32 {
        None
    } else {
        Some(shifted as usize)
    }
}
