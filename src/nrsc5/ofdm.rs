//! OFDM symbol framer + FFT for NRSC-5 samples.

use std::sync::Arc;

use num_complex::Complex;
use rustfft::{Fft, FftPlanner};

use crate::nrsc5::consts::{NRSC5_IQ_RATE, OFDM_CP_LEN, OFDM_FFT_LEN, OFDM_SYMBOL_LEN};
use crate::nrsc5::sync::Sync;

#[derive(Debug, Clone, Copy, Default)]
pub struct AcquisitionStats {
    pub timing_offset: usize,
    pub cp_metric: f32,
    pub coarse_cfo_hz: f32,
    pub locked: bool,
}

pub struct OfdmFramer {
    buf: Vec<Complex<f32>>,
    fft: Arc<dyn Fft<f32>>,
    fft_in: Vec<Complex<f32>>,
    fft_buf: Vec<Complex<f32>>,
    symbols_seen: u64,
    acquisition: AcquisitionStats,
    timing_locked: bool,
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
            acquisition: AcquisitionStats::default(),
            timing_locked: false,
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

        let Some(acq) = self.acquire_symbol() else {
            return false;
        };
        self.acquisition = acq;
        if !self.timing_locked && !acq.locked {
            if self.buf.len() > OFDM_SYMBOL_LEN + OFDM_CP_LEN {
                self.buf.drain(..OFDM_CP_LEN);
            }
            return false;
        }

        if !self.timing_locked && acq.timing_offset > 0 {
            self.buf.drain(..acq.timing_offset);
        }
        self.timing_locked = true;

        let sym: Vec<Complex<f32>> = self.buf.drain(..OFDM_SYMBOL_LEN).collect();
        let body = &sym[OFDM_CP_LEN..];

        let phase_step = -2.0 * std::f32::consts::PI * acq.coarse_cfo_hz / NRSC5_IQ_RATE;
        let mut phase = phase_step * OFDM_CP_LEN as f32;
        for (dst, &src) in self.fft_in.iter_mut().zip(body.iter()) {
            let rot = Complex::new(phase.cos(), phase.sin());
            *dst = src * rot;
            phase += phase_step;
        }
        self.fft.process(&mut self.fft_in);

        self.fft_buf.clear();
        self.fft_buf.extend(self.fft_in.iter().map(|z| {
            Complex::new(z.re / OFDM_FFT_LEN as f32, z.im / OFDM_FFT_LEN as f32)
        }));

        self.sync.process_symbol(&self.fft_buf);
        self.symbols_seen += 1;
        true
    }

    fn acquire_symbol(&self) -> Option<AcquisitionStats> {
        if self.buf.len() < OFDM_SYMBOL_LEN {
            return None;
        }
        if self.timing_locked {
            let mut stats = self.cp_stats_at(0);
            stats.locked = true;
            return Some(stats);
        }

        let search = (self.buf.len() - OFDM_SYMBOL_LEN).min(OFDM_SYMBOL_LEN - 1);
        let mut best = AcquisitionStats::default();
        for off in 0..=search {
            let stats = self.cp_stats_at(off);
            if stats.cp_metric > best.cp_metric {
                best = stats;
            }
        }
        best.locked = best.cp_metric > 0.25;
        Some(best)
    }

    fn cp_stats_at(&self, off: usize) -> AcquisitionStats {
        let mut corr = Complex::new(0.0f32, 0.0);
        let mut p0 = 0.0f32;
        let mut p1 = 0.0f32;
        for i in 0..OFDM_CP_LEN {
            let a = self.buf[off + i];
            let b = self.buf[off + OFDM_FFT_LEN + i];
            corr += a.conj() * b;
            p0 += a.norm_sqr();
            p1 += b.norm_sqr();
        }
        let denom = (p0 * p1).sqrt().max(1e-12);
        let metric = corr.norm() / denom;
        let angle = corr.im.atan2(corr.re);
        AcquisitionStats {
            timing_offset: off,
            cp_metric: metric,
            coarse_cfo_hz: angle * NRSC5_IQ_RATE / (2.0 * std::f32::consts::PI * OFDM_FFT_LEN as f32),
            locked: metric > 0.12,
        }
    }

    pub fn symbols_seen(&self) -> u64 {
        self.symbols_seen
    }

    pub fn acquisition(&self) -> AcquisitionStats {
        self.acquisition
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
