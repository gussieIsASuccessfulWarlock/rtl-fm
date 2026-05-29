//! Phase-discriminator FM demodulator:
//!   y[n] = arg(z[n] * conj(z[n-1]))
//!
//! For a peak deviation of 75 kHz at 240 kS/s baseband the angle is
//! within ±π * 2 * 75e3 / fs which is well below π, so atan2 is fine
//! without phase unwrapping.

use num_complex::Complex;

pub struct FmDemod {
    prev: Complex<f32>,
    /// Output scale so a ±75 kHz deviation maps to ±1.0.
    scale: f32,
}

impl FmDemod {
    /// `fs` is the baseband sample rate.
    pub fn new(fs: f32, max_deviation_hz: f32) -> Self {
        let scale = fs / (2.0 * std::f32::consts::PI * max_deviation_hz);
        Self {
            prev: Complex::new(1.0, 0.0),
            scale,
        }
    }

    pub fn process(&mut self, input: &[Complex<f32>], out: &mut Vec<f32>) {
        for &z in input {
            let prod = z * self.prev.conj();
            let phi = prod.im.atan2(prod.re);
            out.push(phi * self.scale);
            self.prev = z;
        }
    }
}
