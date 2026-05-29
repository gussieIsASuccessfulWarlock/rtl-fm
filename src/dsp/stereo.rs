//! MPX → stereo decoder.
//!
//! Input is the demodulated FM composite signal at 240 kS/s. It
//! contains:
//!   * L+R mono sum (0-15 kHz)
//!   * 19 kHz pilot tone
//!   * 23-53 kHz L-R DSB-SC subcarrier centred on 38 kHz
//!   * 57 kHz RDS PSK subcarrier (decoded elsewhere)
//!   * HD Radio digital sidebands (handled by being filtered out
//!     in the RF DDC LPF)
//!
//! This module decodes only the analog stereo audio. It takes the
//! pilot-PLL reference `cos(2·φ)` from outside so the channel task
//! can share one PLL instance with the RDS demodulator.

use crate::dsp::fir::{RealFir, design_lowpass_kaiser};

pub struct StereoDecoder {
    sum_lpf: RealFir,
    diff_lpf: RealFir,
}

impl StereoDecoder {
    pub fn new(fs: f32) -> Self {
        // 15 kHz is the canonical audio limit for broadcast FM.
        let lpf = design_lowpass_kaiser(15_000.0, fs, 63, 8.0);
        Self {
            sum_lpf: RealFir::new(lpf.clone()),
            diff_lpf: RealFir::new(lpf),
        }
    }

    /// Decode one MPX sample with the corresponding cos(2·φ) reference
    /// and lock-strength mix factor (0..=1). Emits interleaved L,R.
    #[inline]
    pub fn push_sample(&mut self, mpx_sample: f32, cos_2phi: f32, mix: f32, lr: &mut Vec<f32>) {
        let lpr = self.sum_lpf.process_sample(mpx_sample);
        let demod = mpx_sample * cos_2phi * 2.0;
        let lmr_raw = self.diff_lpf.process_sample(demod);
        let lmr = lmr_raw * mix;
        lr.push(lpr + lmr);
        lr.push(lpr - lmr);
    }
}
