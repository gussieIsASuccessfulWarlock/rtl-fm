//! NRSC-5 sample-rate conversion.
//!
//! Two modes:
//!
//! 1. **Wideband (legacy)**: 2.4 MS/s RTL-SDR → 744 187.5 S/s.
//!    Rational polyphase resampler INTERP/DECIM = 3969/12800.
//!    (Not the primary path — kept for reference.)
//!
//! 2. **Dedicated (preferred)**: 1 488 375 S/s RTL-SDR → 744 187.5 S/s.
//!    Simple decimate-by-2 with a 63-tap Kaiser anti-alias filter.
//!    This is how the C nrsc5 reference operates; tuning the RTL-SDR
//!    directly onto the target station at 1.488 MS/s ensures the NRSC-5
//!    IBOC sidebands receive full 8-bit ADC dynamic range, with the
//!    dominant analog-FM carrier occupying the correct portion of the
//!    ADC range rather than multiple wideband stations competing for it.

use num_complex::Complex;

use crate::dsp::fir::design_lowpass_kaiser;
use crate::nrsc5::consts::{NRSC5_IQ_RATE, RESAMPLE_DECIM, RESAMPLE_INTERP, RTL_IQ_RATE};

// ── Wideband (polyphase) resampler ───────────────────────────────────────────

/// Number of taps per polyphase branch. Runtime cost is this many complex
/// MACs per output sample; the large interpolation factor only increases
/// the precomputed branch table.
const TAPS_PER_ARM: usize = 16;
const FILTER_LEN: usize = RESAMPLE_INTERP * TAPS_PER_ARM;

pub struct ComplexResampler {
    branches: Vec<Vec<f32>>,
    /// Delay line, newest-first: `delay[0]` is the most recent input.
    delay: Vec<Complex<f32>>,
    n_in: u64,
    m_out: u64,
}

impl ComplexResampler {
    pub fn new() -> Self {
        // Design the prototype at the upsampled rate (fs_in × INTERP),
        // cut just below output Nyquist, scale by INTERP to compensate
        // upsample zero-stuffing.
        let fs_up = RTL_IQ_RATE * RESAMPLE_INTERP as f32;
        let cutoff = NRSC5_IQ_RATE * 0.45;
        let mut taps = design_lowpass_kaiser(cutoff, fs_up, FILTER_LEN, 9.0);
        let scale = RESAMPLE_INTERP as f32;
        for t in taps.iter_mut() {
            *t *= scale;
        }

        let mut branches: Vec<Vec<f32>> = (0..RESAMPLE_INTERP)
            .map(|_| Vec::with_capacity(TAPS_PER_ARM))
            .collect();
        // Polyphase decomposition: tap i goes to branch (i mod INTERP).
        for (i, t) in taps.into_iter().enumerate() {
            branches[i % RESAMPLE_INTERP].push(t);
        }

        Self {
            branches,
            delay: vec![Complex::new(0.0, 0.0); TAPS_PER_ARM],
            n_in: 0,
            m_out: 0,
        }
    }

    pub fn process(&mut self, input: &[Complex<f32>], output: &mut Vec<Complex<f32>>) {
        for &x in input {
            // Push input into the newest-first delay line.
            for k in (1..TAPS_PER_ARM).rev() {
                self.delay[k] = self.delay[k - 1];
            }
            self.delay[0] = x;
            self.n_in += 1;

            // Emit any outputs whose anchor input has now arrived.
            loop {
                let m_decim = self.m_out * RESAMPLE_DECIM as u64;
                let anchor = m_decim / RESAMPLE_INTERP as u64;
                if anchor + 1 > self.n_in {
                    break;
                }
                let branch = (m_decim % RESAMPLE_INTERP as u64) as usize;
                let taps = &self.branches[branch];
                let offset = (self.n_in - 1 - anchor) as usize;

                let mut acc_re = 0.0f32;
                let mut acc_im = 0.0f32;
                for (j, &t) in taps.iter().enumerate() {
                    let k = offset + j;
                    if k < self.delay.len() {
                        acc_re += self.delay[k].re * t;
                        acc_im += self.delay[k].im * t;
                    }
                }
                output.push(Complex::new(acc_re, acc_im));
                self.m_out += 1;
            }
        }
    }
}

impl Default for ComplexResampler {
    fn default() -> Self {
        Self::new()
    }
}

// ── Dedicated 1 488 375 → 744 187.5 S/s decimator ───────────────────────────

/// Number of taps for the halfband anti-alias filter.
const HALFBAND_TAPS: usize = 63;

/// Decimate-by-2 lowpass FIR for the dedicated NRSC-5 path.
///
/// Input: 1 488 375 S/s (RTL-SDR centered directly on target station).
/// Output: 744 187.5 S/s (NRSC-5 processing rate).
///
/// Cutoff is set to 0.45 × 744 187.5 = ~335 kHz, well within the
/// NRSC-5 IBOC sideband extent (~400 kHz) but rejecting out-of-band
/// interference from adjacent FM stations.
pub struct HalfbandDecimator {
    taps: Vec<f32>,
    /// Circular delay line (length = HALFBAND_TAPS).
    delay: Vec<Complex<f32>>,
    head: usize,
    /// Toggles each input sample; output produced when phase == 0.
    phase: usize,
}

impl HalfbandDecimator {
    pub fn new() -> Self {
        // fs_in = 1 488 375, cutoff = 0.45 × 744 187.5 = 334 884 Hz.
        let taps = design_lowpass_kaiser(334_884.0, 1_488_375.0, HALFBAND_TAPS, 9.0);
        Self {
            taps,
            delay: vec![Complex::new(0.0, 0.0); HALFBAND_TAPS],
            head: 0,
            phase: 0,
        }
    }

    pub fn process(&mut self, input: &[Complex<f32>], output: &mut Vec<Complex<f32>>) {
        for &x in input {
            // Insert into circular delay line.
            self.delay[self.head] = x;
            self.head = (self.head + 1) % HALFBAND_TAPS;

            // Emit one output every two inputs.
            if self.phase == 0 {
                let mut acc = Complex::new(0.0f32, 0.0f32);
                for (i, &t) in self.taps.iter().enumerate() {
                    let idx = (self.head + HALFBAND_TAPS - 1 - i) % HALFBAND_TAPS;
                    acc.re += self.delay[idx].re * t;
                    acc.im += self.delay[idx].im * t;
                }
                output.push(acc);
            }
            self.phase ^= 1;
        }
    }
}

impl Default for HalfbandDecimator {
    fn default() -> Self {
        Self::new()
    }
}
