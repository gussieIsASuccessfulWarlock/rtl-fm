//! Time/frequency synchronization and reference subcarrier processing.

use num_complex::Complex;

use crate::nrsc5::consts::{
    FRAME_SYMBOLS, NUM_REF, REF_PATTERN, REF_SUBCARRIERS, bin_idx,
};

const FRAME_LOCK_THRESHOLD: f32 = 0.18;

pub struct Sync {
    frame_count: u64,
    pub frame_aligned: bool,
    pub frame_metric: f32,
    pub frame_offset: usize,
    pub channel_estimates: Vec<Complex<f32>>,
    ref_buf: Vec<Complex<f32>>,
    symbol_count: u64,
}

impl Sync {
    pub fn new() -> Self {
        Self {
            frame_count: 0,
            frame_aligned: false,
            frame_metric: 0.0,
            frame_offset: 0,
            channel_estimates: vec![Complex::new(0.0, 0.0); NUM_REF],
            ref_buf: Vec::with_capacity(FRAME_SYMBOLS * NUM_REF),
            symbol_count: 0,
        }
    }

    pub fn process_symbol(&mut self, fft_bins: &[Complex<f32>]) {
        self.symbol_count += 1;
        let _sym_idx = (self.symbol_count - 1) % FRAME_SYMBOLS as u64;
        let pat = &REF_PATTERN;

        let ref_vals: Vec<Complex<f32>> = REF_SUBCARRIERS.iter().take(NUM_REF).map(|&sc| {
            let idx = bin_idx(sc);
            fft_bins[idx]
        }).collect();

        if !self.frame_aligned {
            self.ref_buf.extend(ref_vals);
            if self.ref_buf.len() >= NUM_REF * FRAME_SYMBOLS {
                self.detect_frame(pat);
            }
            return;
        }

        for (ri, (rv, ce)) in ref_vals.iter().zip(self.channel_estimates.iter_mut()).enumerate() {
            let pat_idx = (self.frame_count % FRAME_SYMBOLS as u64) * NUM_REF as u64 + ri as u64;
            let sign = pat[pat_idx as usize];
            *ce = if sign > 0 { *rv } else { -*rv };
        }

        self.frame_count += 1;
    }

    pub fn reset(&mut self) {
        self.frame_count = 0;
        self.frame_aligned = false;
        self.frame_metric = 0.0;
        self.frame_offset = 0;
        self.ref_buf.clear();
        self.symbol_count = 0;
        for ce in &mut self.channel_estimates {
            *ce = Complex::new(0.0, 0.0);
        }
    }

    fn detect_frame(&mut self, pat: &[i8]) {
        let mut best_metric = 0.0f32;
        let mut best_off = 0usize;
        for off in 0..FRAME_SYMBOLS {
            let mut corr = 0.0f32;
            let mut energy = 0.0f32;
            for ri in 0..NUM_REF {
                let mut acc = Complex::new(0.0f32, 0.0);
                for sym in 0..FRAME_SYMBOLS {
                    let val = self.ref_buf[sym * NUM_REF + ri];
                    let pat_idx = ((sym + off) % FRAME_SYMBOLS) * NUM_REF + ri;
                    let sign = pat[pat_idx];
                    acc += if sign > 0 { val } else { -val };
                    energy += val.norm();
                }
                corr += acc.norm();
            }
            let metric = corr / energy.max(1e-12);
            if metric > best_metric {
                best_metric = metric;
                best_off = off;
            }
        }

        self.frame_metric = best_metric;
        self.frame_offset = best_off;
        if best_metric > FRAME_LOCK_THRESHOLD {
            self.frame_aligned = true;
            self.frame_count = 0;
            let drop = best_off * NUM_REF;
            if drop <= self.ref_buf.len() {
                self.ref_buf.drain(..drop);
            }
        }
        self.ref_buf.clear();
    }
}

impl Default for Sync {
    fn default() -> Self { Self::new() }
}
