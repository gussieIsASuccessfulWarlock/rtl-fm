//! Windowed-sinc FIR designer + decimating real/complex filter.

use num_complex::Complex;

/// Design a low-pass FIR using a Kaiser window. `cutoff_hz` is the
/// 6 dB cutoff, `fs` is the sample rate the filter will run at,
/// `n` is the tap count (odd preferred for linear phase).
pub fn design_lowpass_kaiser(cutoff_hz: f32, fs: f32, n: usize, beta: f32) -> Vec<f32> {
    assert!(n > 0);
    let m = n as i64 - 1;
    let fc = cutoff_hz / fs; // 0..0.5
    let pi = std::f32::consts::PI;
    let mut taps = Vec::with_capacity(n);
    let i0_beta = bessel_i0(beta);
    for k in 0..n {
        let kk = k as i64 - m / 2;
        let sinc = if kk == 0 {
            2.0 * fc
        } else {
            ((2.0 * pi * fc * kk as f32).sin()) / (pi * kk as f32)
        };
        let win_arg = 1.0 - (2.0 * k as f32 / m as f32 - 1.0).powi(2);
        let win = if win_arg < 0.0 {
            0.0
        } else {
            bessel_i0(beta * win_arg.sqrt()) / i0_beta
        };
        taps.push(sinc * win);
    }
    // Normalize for unity DC gain.
    let s: f32 = taps.iter().sum();
    if s.abs() > 1e-12 {
        for t in taps.iter_mut() {
            *t /= s;
        }
    }
    taps
}

fn bessel_i0(x: f32) -> f32 {
    // Series expansion sufficient for typical Kaiser betas (4-14).
    let mut sum = 1.0f32;
    let mut term = 1.0f32;
    let xh2 = (x * 0.5).powi(2);
    for k in 1..50 {
        term *= xh2 / (k as f32 * k as f32);
        sum += term;
        if term < 1e-12 * sum {
            break;
        }
    }
    sum
}

/// Decimating complex FIR (taps are real). Filter then keep 1/decim
/// samples.
pub struct ComplexDecimFir {
    taps: Vec<f32>,
    state: Vec<Complex<f32>>,
    decim: usize,
    counter: usize,
}

impl ComplexDecimFir {
    pub fn new(taps: Vec<f32>, decim: usize) -> Self {
        let n = taps.len();
        Self {
            taps,
            state: vec![Complex::new(0.0, 0.0); n],
            decim,
            counter: 0,
        }
    }

    /// Process input; append decimated outputs to `out`.
    pub fn process(&mut self, input: &[Complex<f32>], out: &mut Vec<Complex<f32>>) {
        let n = self.taps.len();
        for &x in input {
            // Shift state left, append x at the end.
            self.state.copy_within(1..n, 0);
            self.state[n - 1] = x;
            self.counter += 1;
            if self.counter >= self.decim {
                self.counter = 0;
                // Convolution sum.
                let mut acc = Complex::new(0.0f32, 0.0);
                for (k, t) in self.taps.iter().enumerate() {
                    let s = self.state[k];
                    acc.re += s.re * t;
                    acc.im += s.im * t;
                }
                out.push(acc);
            }
        }
    }
}

/// Real-valued decimating FIR.
pub struct RealDecimFir {
    taps: Vec<f32>,
    state: Vec<f32>,
    decim: usize,
    counter: usize,
}

impl RealDecimFir {
    pub fn new(taps: Vec<f32>, decim: usize) -> Self {
        let n = taps.len();
        Self {
            taps,
            state: vec![0.0; n],
            decim,
            counter: 0,
        }
    }

    pub fn process(&mut self, input: &[f32], out: &mut Vec<f32>) {
        let n = self.taps.len();
        for &x in input {
            self.state.copy_within(1..n, 0);
            self.state[n - 1] = x;
            self.counter += 1;
            if self.counter >= self.decim {
                self.counter = 0;
                let mut acc = 0.0f32;
                for (k, t) in self.taps.iter().enumerate() {
                    acc += self.state[k] * t;
                }
                out.push(acc);
            }
        }
    }
}

/// Non-decimating real FIR (same-rate output, used for narrowband
/// extracts like 19 kHz pilot tone and 23 kHz subcarrier slot).
pub struct RealFir {
    taps: Vec<f32>,
    state: Vec<f32>,
}

impl RealFir {
    pub fn new(taps: Vec<f32>) -> Self {
        let n = taps.len();
        Self { taps, state: vec![0.0; n] }
    }
    pub fn process_sample(&mut self, x: f32) -> f32 {
        let n = self.taps.len();
        self.state.copy_within(1..n, 0);
        self.state[n - 1] = x;
        let mut acc = 0.0f32;
        for (k, t) in self.taps.iter().enumerate() {
            acc += self.state[k] * t;
        }
        acc
    }
}
