//! OFDM symbol framer for NRSC-5 FM.
//!
//! Matches the C reference `acquire.c` approach: batch timing acquisition over
//! ACQUIRE_SYMBOLS=10 consecutive symbols using multi-symbol CP correlation
//! (summed over 10 symbols before searching the peak). This averages out the
//! strong analog-FM carrier interference that would otherwise swamp a
//! single-symbol CP correlator.

use std::collections::VecDeque;
use std::sync::Arc;

use num_complex::Complex;
use rustfft::{Fft, FftPlanner};

use crate::nrsc5::consts::{CP_FM, FFTCP_FM, FFT_FM, NRSC5_IQ_RATE};

/// Number of OFDM symbols processed per timing-acquisition batch (C: ACQUIRE_SYMBOLS).
const ACQUIRE_SYMBOLS: usize = 10;

/// Total samples required before each batch.
const ACQUIRE_BUF: usize = FFTCP_FM * (ACQUIRE_SYMBOLS + 1);

/// Timing offset compensation matching C reference FILTER_DELAY.
const FILTER_DELAY: usize = 15;

#[derive(Debug, Clone, Copy, Default)]
pub struct AcquisitionStats {
    pub timing_offset: usize,
    pub cp_metric: f32,
    pub cfo_hz: f32,
    pub locked: bool,
}

/// Raised-cosine pulse-shaping window (C: shape_fm[]).
/// shape[i]:  i < CP_FM  → sin(π/2 · i/CP_FM)
///            CP_FM ≤ i < FFT_FM → 1.0
///            i ≥ FFT_FM → cos(π/2 · (i−FFT_FM)/CP_FM)
fn make_shape_fm() -> Vec<f32> {
    let mut s = vec![0.0f32; FFTCP_FM];
    for i in 0..FFTCP_FM {
        s[i] = if i < CP_FM {
            (std::f32::consts::FRAC_PI_2 * i as f32 / CP_FM as f32).sin()
        } else if i < FFT_FM {
            1.0
        } else {
            (std::f32::consts::FRAC_PI_2 * (i - FFT_FM) as f32 / CP_FM as f32).cos()
        };
    }
    s
}

pub struct OfdmFramer {
    /// Raw IQ sample accumulator.
    buf: Vec<Complex<f32>>,
    /// Processed FFT snapshots waiting to be delivered one at a time.
    symbol_queue: VecDeque<Vec<Complex<f32>>>,
    fft: Arc<dyn Fft<f32>>,
    fft_in: Vec<Complex<f32>>,
    /// Raised-cosine pulse-shaping window (length FFTCP_FM).
    shape: Vec<f32>,
    /// Cumulative CFO phase across symbols.
    cfo_phase: f32,
    /// Tracked fractional CFO in Hz.
    cfo_hz: f32,
    /// Latest FFT output (delivered via fft_buf()).
    fft_buf_out: Vec<Complex<f32>>,
    pub acquisition: AcquisitionStats,
    pub symbols_seen: u64,
}

impl OfdmFramer {
    pub fn new() -> Self {
        let mut planner = FftPlanner::<f32>::new();
        let fft = planner.plan_fft_forward(FFT_FM);
        Self {
            buf: Vec::with_capacity(ACQUIRE_BUF + FFTCP_FM),
            symbol_queue: VecDeque::with_capacity(ACQUIRE_SYMBOLS),
            fft,
            fft_in: vec![Complex::new(0.0, 0.0); FFT_FM],
            shape: make_shape_fm(),
            cfo_phase: 0.0,
            cfo_hz: 0.0,
            fft_buf_out: vec![Complex::new(0.0, 0.0); FFT_FM],
            acquisition: AcquisitionStats::default(),
            symbols_seen: 0,
        }
    }

    pub fn feed(&mut self, samples: &[Complex<f32>]) {
        self.buf.extend_from_slice(samples);
    }

    /// Return one FFT snapshot (via `fft_buf()`), or false if insufficient data.
    ///
    /// Internally processes in batches of `ACQUIRE_SYMBOLS` symbols using
    /// multi-symbol CP correlation for timing, matching C `acquire_process()`.
    pub fn process_one_symbol(&mut self) -> bool {
        // Deliver from queue if available.
        if let Some(sym) = self.symbol_queue.pop_front() {
            self.fft_buf_out = sym;
            self.symbols_seen += 1;
            return true;
        }

        // Need a full acquisition batch worth of samples.
        if self.buf.len() < ACQUIRE_BUF {
            return false;
        }

        // ── Step 1: Multi-symbol CP correlation ──────────────────────────────
        // sums[i] = Σ_{j=0..ACQUIRE_SYMBOLS-1}  buf[i+j·FFTCP] · conj(buf[i+j·FFTCP + FFT])
        let mut sums = vec![Complex::new(0.0f32, 0.0f32); FFTCP_FM];
        for i in 0..FFTCP_FM {
            for j in 0..ACQUIRE_SYMBOLS {
                let a = self.buf[i + j * FFTCP_FM];
                let b = self.buf[i + j * FFTCP_FM + FFT_FM];
                sums[i].re += a.re * b.re + a.im * b.im;
                sums[i].im += a.re * b.im - a.im * b.re;
            }
        }

        // ── Step 2: Apply window, find peak timing offset ─────────────────────
        let mut best_mag2 = -1.0f32;
        let mut best_v = Complex::new(0.0f32, 0.0f32);
        let mut best_i = 0usize;
        for i in 0..FFTCP_FM {
            let mut v = Complex::new(0.0f32, 0.0f32);
            for j in 0..CP_FM {
                let s = sums[(i + j) % FFTCP_FM];
                let w = self.shape[j] * self.shape[j + FFT_FM];
                v.re += s.re * w;
                v.im += s.im * w;
            }
            let mag2 = v.re * v.re + v.im * v.im;
            if mag2 > best_mag2 {
                best_mag2 = mag2;
                best_v = v;
                best_i = i;
            }
        }
        // Compensate FILTER_DELAY (C: samperr = (i + fftcp - FILTER_DELAY) % fftcp).
        let samperr = (best_i + FFTCP_FM - FILTER_DELAY) % FFTCP_FM;

        // ── Step 3: CFO estimate from peak phase ──────────────────────────────
        let angle = best_v.im.atan2(best_v.re);
        // Matches C: angle_factor = 0.25 after first lock, 1.0 initially.
        let cfo_hz_new = angle * NRSC5_IQ_RATE / (2.0 * std::f32::consts::PI * FFT_FM as f32);
        self.cfo_hz = if self.acquisition.locked {
            self.cfo_hz + (cfo_hz_new - self.cfo_hz) * 0.25
        } else {
            cfo_hz_new
        };

        self.acquisition = AcquisitionStats {
            timing_offset: samperr,
            cp_metric: best_mag2.sqrt() / (ACQUIRE_SYMBOLS as f32 * CP_FM as f32),
            cfo_hz: self.cfo_hz,
            locked: true,
        };

        // ── Step 4: Drain to timing offset, then process ACQUIRE_SYMBOLS syms ─
        if samperr > 0 && samperr < self.buf.len() {
            self.buf.drain(..samperr);
        }
        if self.buf.len() < ACQUIRE_SYMBOLS * FFTCP_FM {
            return false;
        }

        let phase_step = -2.0 * std::f32::consts::PI * self.cfo_hz / NRSC5_IQ_RATE;
        let scale = 1.0 / FFT_FM as f32;

        for sym_idx in 0..ACQUIRE_SYMBOLS {
            let base = sym_idx * FFTCP_FM;

            // Zero accumulator.
            for x in &mut self.fft_in {
                *x = Complex::new(0.0, 0.0);
            }

            // Phase at start of FFTCP period (j=0), matching C's temp_phase = st->phase.
            let mut phase = self.cfo_phase;

            for j in 0..FFTCP_FM {
                let src = self.buf[base + j];
                let (s, c) = phase.sin_cos();
                let rotated = Complex::new(src.re * c - src.im * s, src.re * s + src.im * c);
                let w = self.shape[j];
                // Windowed overlap-add matching C acquire.c (offset=0 for FM):
                //   fftin[(j) % FFT_FM]
                //   j < CP_FM:           fftin[j]  = shape[j]       * sample  (CP into start)
                //   CP_FM ≤ j < FFT_FM:  fftin[j]  = sample          (body, no window)
                //   j ≥ FFT_FM:          fftin[j%FFT_FM] += shape[j] * sample (tail overlap into start)
                let idx = j % FFT_FM;
                if j < CP_FM {
                    self.fft_in[idx].re = w * rotated.re;
                    self.fft_in[idx].im = w * rotated.im;
                } else if j < FFT_FM {
                    self.fft_in[idx].re = rotated.re;
                    self.fft_in[idx].im = rotated.im;
                } else {
                    self.fft_in[idx].re += w * rotated.re;
                    self.fft_in[idx].im += w * rotated.im;
                }
                phase += phase_step;
            }

            // Advance cumulative CFO phase by one full FFTCP period.
            self.cfo_phase += phase_step * FFTCP_FM as f32;
            wrap_pi(&mut self.cfo_phase);

            // Forward FFT.
            self.fft.process(&mut self.fft_in);

            // fftshift + normalize: DC at bin FFT_FM/2, matching consts.rs bin tables.
            let mut out = vec![Complex::new(0.0f32, 0.0f32); FFT_FM];
            for i in 0..FFT_FM {
                let src = (i + FFT_FM / 2) % FFT_FM;
                out[i] = self.fft_in[src] * scale;
            }

            self.symbol_queue.push_back(out);
        }

        // Consume the ACQUIRE_SYMBOLS symbols from the buffer.
        let consumed = ACQUIRE_SYMBOLS * FFTCP_FM;
        if consumed <= self.buf.len() {
            self.buf.drain(..consumed);
        } else {
            self.buf.clear();
        }

        // Deliver first symbol from queue.
        if let Some(sym) = self.symbol_queue.pop_front() {
            self.fft_buf_out = sym;
            self.symbols_seen += 1;
            true
        } else {
            false
        }
    }

    pub fn fft_buf(&self) -> &[Complex<f32>] {
        &self.fft_buf_out
    }
}

impl Default for OfdmFramer {
    fn default() -> Self {
        Self::new()
    }
}

fn wrap_pi(phase: &mut f32) {
    while *phase > std::f32::consts::PI {
        *phase -= 2.0 * std::f32::consts::PI;
    }
    while *phase < -std::f32::consts::PI {
        *phase += 2.0 * std::f32::consts::PI;
    }
}
