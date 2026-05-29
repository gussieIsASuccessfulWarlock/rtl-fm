//! DSP primitives used by the per-channel pipeline.

pub mod fir;
pub mod fm;
pub mod iir;
pub mod nco;
pub mod pll;
pub mod rds;
pub mod rds_decoder;
pub mod resample;
pub mod stereo;

/// Convert RTL2832U u8 IQ pairs to centered Complex<f32> samples.
/// The chip outputs unsigned 8-bit samples centered at 127.5; we
/// rescale to roughly [-1, 1].
pub fn u8_iq_to_complex(bytes: &[u8], out: &mut Vec<num_complex::Complex<f32>>) {
    out.clear();
    out.reserve(bytes.len() / 2);
    let scale = 1.0f32 / 127.5;
    let mut iter = bytes.chunks_exact(2);
    for ch in &mut iter {
        let i = (ch[0] as f32 - 127.5) * scale;
        let q = (ch[1] as f32 - 127.5) * scale;
        out.push(num_complex::Complex::new(i, q));
    }
}
