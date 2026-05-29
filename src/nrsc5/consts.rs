//! NRSC-5 numeric constants used by the decoder pipeline.

use std::sync::LazyLock;

pub const RTL_IQ_RATE: f32 = 2_400_000.0;
pub const NRSC5_IQ_RATE: f32 = 744_000.0;
pub const RESAMPLE_INTERP: usize = 31;
pub const RESAMPLE_DECIM: usize = 100;

/// OFDM framing.
pub const OFDM_FFT_LEN: usize = 2048;
pub const OFDM_CP_LEN: usize = 112;
pub const OFDM_SYMBOL_LEN: usize = OFDM_FFT_LEN + OFDM_CP_LEN;

/// OFDM symbols per service frame (32).
pub const FRAME_SYMBOLS: usize = 32;

/// Reference subcarrier indices (0-centered, signed).
pub const REF_SUBCARRIERS: &[i16] = &[
    -1025, -975, -925, -875, -825, -775, -725, -675, -625,
    -575, -525, -475, -425, -375, -325, -275, -250, -225,
    225, 250, 275, 325, 375, 425, 475, 525, 575,
    625, 675, 725, 775, 825, 875, 925, 975, 1025,
];
pub const NUM_REF: usize = 36;

/// Known BPSK sequence transmitted on reference subcarriers.
/// Generated from the polynomial x^10 + x^7 + 1, initial seed 1.
/// Only the sign pattern (0 → +1, 1 → -1) is needed.
pub static REF_PATTERN: LazyLock<Vec<i8>> = LazyLock::new(|| {
    let poly: u16 = 0x240;
    let mut reg: u16 = 1;
    let mut bits = Vec::with_capacity(NUM_REF * 32);
    for _ in 0..NUM_REF * 32 {
        let bit = (reg >> 9) & 1;
        reg = (reg ^ ((reg >> 6) & poly)) & 0x3FF;
        reg = (reg << 1) | bit;
        bits.push(if bit == 0 { 1i8 } else { -1i8 });
    }
    bits
});

/// Viterbi rate-5/8 generator polynomials (8 output bits).
pub const VITERBI_GEN: &[u8; 8] = &[0x4D, 0x57, 0x79, 0x6B, 0x73, 0x75, 0x65, 0x33];
pub const VITERBI_NUMS: usize = 64;
pub const VITERBI_INPUTS: usize = 32;

/// Reed-Solomon (255,223) over GF(256).
pub const RS_N: usize = 255;
pub const RS_K: usize = 223;
pub const RS_PARITY: usize = RS_N - RS_K;
pub const RS_GEN: u16 = 0x11D;

/// AAS application IDs for metadata.
pub const AAS_APP_PSD: u16 = 0x0001;
pub const AAS_APP_SIG: u16 = 0x0002;
pub const AAS_APP_LOT: u16 = 0x0004;

/// Convert a signed subcarrier index to FFT bin index.
pub fn bin_idx(sc_idx: i16) -> usize {
    if sc_idx >= 0 { sc_idx as usize } else { (OFDM_FFT_LEN as i16 + sc_idx) as usize }
}
