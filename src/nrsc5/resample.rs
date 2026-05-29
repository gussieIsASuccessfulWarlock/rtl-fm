//! Complex rational resampler for the NRSC-5 chain.

use num_complex::Complex;

use crate::nrsc5::consts::{RESAMPLE_DECIM, RESAMPLE_INTERP};

/// Windowed-sinc lowpass used by the polyphase converter.
fn design_resample_taps() -> Vec<f32> {
    let phases = RESAMPLE_INTERP;
    let taps_per_phase = 32usize;
    let ntaps = phases * taps_per_phase;
    let cutoff = 0.5f32 / (RESAMPLE_INTERP.max(RESAMPLE_DECIM) as f32);
    let mid = (ntaps as f32 - 1.0) * 0.5;
    let mut taps = Vec::with_capacity(ntaps);
    for n in 0..ntaps {
        let x = n as f32 - mid;
        let sinc = if x.abs() < 1e-8 {
            2.0 * cutoff
        } else {
            (2.0 * std::f32::consts::PI * cutoff * x).sin() / (std::f32::consts::PI * x)
        };
        let win = 0.5 - 0.5 * (2.0 * std::f32::consts::PI * n as f32 / (ntaps as f32 - 1.0)).cos();
        taps.push(sinc * win * RESAMPLE_INTERP as f32);
    }
    taps
}

/// Stateful rational complex resampler.
pub struct ComplexResampler {
    taps: Vec<f32>,
    hist: Vec<Complex<f32>>,
    phase: usize,
    in_idx: usize,
    taps_per_phase: usize,
}

impl ComplexResampler {
    pub fn new() -> Self {
        let taps = design_resample_taps();
        let taps_per_phase = taps.len() / RESAMPLE_INTERP;
        Self {
            taps,
            hist: Vec::with_capacity(8192),
            phase: 0,
            in_idx: 0,
            taps_per_phase,
        }
    }

    pub fn process(&mut self, input: &[Complex<f32>], out: &mut Vec<Complex<f32>>) {
        self.hist.extend_from_slice(input);
        while self.in_idx + self.taps_per_phase <= self.hist.len() {
            let mut acc = Complex::<f32>::new(0.0, 0.0);
            let phase_taps = &self.taps[self.phase..];
            for k in 0..self.taps_per_phase {
                let x = self.hist[self.in_idx + self.taps_per_phase - 1 - k];
                let h = phase_taps[k * RESAMPLE_INTERP];
                acc += x * h;
            }
            out.push(acc);

            self.phase += RESAMPLE_DECIM;
            let adv = self.phase / RESAMPLE_INTERP;
            self.phase %= RESAMPLE_INTERP;
            self.in_idx += adv;
        }

        let keep_from = self.in_idx.saturating_sub(self.taps_per_phase);
        if keep_from > 0 {
            self.hist.drain(..keep_from);
            self.in_idx -= keep_from;
        }
    }
}

impl Default for ComplexResampler {
    fn default() -> Self {
        Self::new()
    }
}
