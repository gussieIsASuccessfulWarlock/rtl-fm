//! OFDM symbol framer + FFT for NRSC-5 samples.

use std::sync::Arc;

use num_complex::Complex;
use rustfft::{Fft, FftPlanner};

use crate::nrsc5::consts::{OFDM_CP_LEN, OFDM_FFT_LEN, OFDM_SYMBOL_LEN};
use crate::nrsc5::sync::Sync;

pub struct OfdmFramer {
    buf: Vec<Complex<f32>>,
    fft: Arc<dyn Fft<f32>>,
    fft_in: Vec<Complex<f32>>,
    fft_buf: Vec<Complex<f32>>,
    symbols_seen: u64,
    pub sync: Sync,
}

impl OfdmFramer {
    pub fn new() -> Self {
        let mut planner = FftPlanner::new();
        let fft = planner.plan_fft_forward(OFDM_FFT_LEN);
        Self {
            buf: Vec::with_capacity(OFDM_SYMBOL_LEN * 4),
            fft,
            fft_in: vec![Complex::new(0.0, 0.0); OFDM_FFT_LEN],
            fft_buf: Vec::with_capacity(OFDM_FFT_LEN),
            symbols_seen: 0,
            sync: Sync::new(),
        }
    }

    /// Add time-domain samples to the internal buffer.
    pub fn feed(&mut self, input: &[Complex<f32>]) {
        self.buf.extend_from_slice(input);
    }

    /// Process one OFDM symbol from the buffer. Returns true if a
    /// symbol was available and processed.
    pub fn process_one_symbol(&mut self) -> bool {
        if self.buf.len() < OFDM_SYMBOL_LEN {
            return false;
        }
        let sym: Vec<Complex<f32>> = self.buf.drain(..OFDM_SYMBOL_LEN).collect();
        let body = &sym[OFDM_CP_LEN..];

        self.fft_in[..OFDM_FFT_LEN].copy_from_slice(&body[..OFDM_FFT_LEN]);
        self.fft.process(&mut self.fft_in);

        self.fft_buf.clear();
        self.fft_buf.extend(self.fft_in.iter().map(|z| {
            Complex::new(z.re / OFDM_FFT_LEN as f32, z.im / OFDM_FFT_LEN as f32)
        }));

        self.sync.process_symbol(&self.fft_buf);
        self.symbols_seen += 1;
        true
    }

    pub fn symbols_seen(&self) -> u64 {
        self.symbols_seen
    }

    /// Current frequency-domain symbol (length OFDM_FFT_LEN).
    pub fn fft_buf(&self) -> &[Complex<f32>] {
        &self.fft_buf
    }
}

impl Default for OfdmFramer {
    fn default() -> Self {
        Self::new()
    }
}
