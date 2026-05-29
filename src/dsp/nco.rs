//! Numerically controlled oscillator producing a complex exponential.

use num_complex::Complex;

pub struct Nco {
    phase: f32,
    step: f32,
}

impl Nco {
    /// `freq_hz` is the desired (possibly negative) tone, `fs` is the
    /// sample rate in Hz.
    pub fn new(freq_hz: f32, fs: f32) -> Self {
        let step = 2.0 * std::f32::consts::PI * freq_hz / fs;
        Self { phase: 0.0, step }
    }

    pub fn set_freq(&mut self, freq_hz: f32, fs: f32) {
        self.step = 2.0 * std::f32::consts::PI * freq_hz / fs;
    }

    #[inline]
    pub fn step(&mut self) -> Complex<f32> {
        let s = Complex::new(self.phase.cos(), self.phase.sin());
        self.phase += self.step;
        // Wrap to keep precision.
        if self.phase > std::f32::consts::PI {
            self.phase -= 2.0 * std::f32::consts::PI;
        } else if self.phase < -std::f32::consts::PI {
            self.phase += 2.0 * std::f32::consts::PI;
        }
        s
    }
}
