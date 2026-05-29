//! Polyphase rational resampler.
//!
//! Used to convert 240 kS/s → 44.1 kS/s (interp 147, decim 800).
//! Operates on interleaved stereo (2 channels) for efficiency.

use crate::dsp::fir::design_lowpass_kaiser;

pub struct PolyphaseResamplerStereo {
    /// `phases[k]` is taps for phase k (length n_per_phase).
    phases: Vec<Vec<f32>>,
    interp: usize,
    decim: usize,
    /// Per-channel delay lines (length n_per_phase).
    delay_l: Vec<f32>,
    delay_r: Vec<f32>,
    /// Current write index in delay line.
    head: usize,
    /// Phase accumulator: emits an output sample whenever it crosses interp.
    phase: usize,
}

impl PolyphaseResamplerStereo {
    /// `fs_in` and `fs_out` are the rates in Hz. They must be such that
    /// `fs_in * interp == fs_out * decim` for an exact ratio.
    pub fn new(fs_in: f32, fs_out: f32, interp: usize, decim: usize, taps_per_phase: usize) -> Self {
        let n_taps = interp * taps_per_phase;
        // Design the prototype LPF at interp * fs_in. Cutoff is at
        // min(fs_in, fs_out) / 2 with some guard.
        let proto_fs = interp as f32 * fs_in;
        let cutoff = 0.45 * fs_out.min(fs_in);
        let mut proto = design_lowpass_kaiser(cutoff, proto_fs, n_taps, 10.0);
        // Polyphase scaling: multiply by interp so per-phase DC gain = 1.
        for t in proto.iter_mut() {
            *t *= interp as f32;
        }
        // Decompose into `interp` sub-filters.
        let mut phases = vec![Vec::with_capacity(taps_per_phase); interp];
        for (k, h) in proto.into_iter().enumerate() {
            phases[k % interp].push(h);
        }
        Self {
            phases,
            interp,
            decim,
            delay_l: vec![0.0; taps_per_phase],
            delay_r: vec![0.0; taps_per_phase],
            head: 0,
            phase: 0,
        }
    }

    /// Process interleaved L/R input → interleaved L/R output appended
    /// to `out`. Input length must be even.
    pub fn process(&mut self, lr_in: &[f32], out: &mut Vec<f32>) {
        debug_assert!(lr_in.len().is_multiple_of(2));
        let n = self.delay_l.len();
        let mut i = 0;
        while i < lr_in.len() {
            // Insert a new sample into the delay lines.
            self.delay_l[self.head] = lr_in[i];
            self.delay_r[self.head] = lr_in[i + 1];
            self.head = (self.head + 1) % n;
            i += 2;
            // The new input sample corresponds to `interp` virtual
            // upsampled samples; phase advances by `interp` per input.
            self.phase += self.interp;
            while self.phase >= self.decim {
                self.phase -= self.decim;
                // Which polyphase to use? The fractional position
                // within the upsampled stream — phase here is the
                // remainder *after* the output sample, so use the
                // complement.
                let phase_idx = (self.decim - 1 - self.phase) % self.interp;
                let taps = &self.phases[phase_idx];
                let mut acc_l = 0.0f32;
                let mut acc_r = 0.0f32;
                // Convolve newest-first.
                let mut idx = if self.head == 0 { n - 1 } else { self.head - 1 };
                for &t in taps {
                    acc_l += self.delay_l[idx] * t;
                    acc_r += self.delay_r[idx] * t;
                    idx = if idx == 0 { n - 1 } else { idx - 1 };
                }
                out.push(acc_l);
                out.push(acc_r);
            }
        }
    }
}
