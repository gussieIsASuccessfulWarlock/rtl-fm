//! RDS BPSK demodulator.
//!
//! Pipeline (at MPX rate 240 kHz):
//!   1. Mix MPX by sin(3·φ_pilot) — the in-phase reference for the
//!      57 kHz RDS subcarrier in the standard FM-stereo phase
//!      convention.
//!   2. Anti-alias LPF (~5 kHz), decimate by 25 → 9.6 kHz baseband.
//!   3. Per-bit matched filter: integrate the first half-symbol,
//!      integrate the second half-symbol, decide on sign of
//!      (first − second). This is the classic Manchester-symbol
//!      detector and is much more SNR-robust than picking a single
//!      sample.
//!   4. Differential decode: data[n] = symbol[n] XOR symbol[n-1].
//!   5. The receiver cannot know which half is the "first" half of a
//!      data bit, so we run two parallel decoders offset by one
//!      half-symbol. The caller feeds both to RDS group decoders and
//!      uses whichever one syncs.

use crate::dsp::fir::{RealFir, design_lowpass_kaiser};

const DECIM: usize = 25;
const MPX_RATE: f32 = 240_000.0;
const BB_RATE: f32 = MPX_RATE / DECIM as f32;
const BIT_RATE: f32 = 1187.5;
const SAMPLES_PER_BIT: f32 = BB_RATE / BIT_RATE; // ~8.084
const SAMPLES_PER_HALF: f32 = SAMPLES_PER_BIT / 2.0; // ~4.042

#[derive(Default)]
struct BitChannel {
    /// Fractional sample position within the current bit (0..SAMPLES_PER_BIT).
    pos: f32,
    /// Sum of samples in the first half-symbol.
    int_first: f32,
    /// Sum of samples in the second half-symbol.
    int_second: f32,
    /// Last raw biphase symbol, for differential decode.
    prev_symbol: bool,
}

impl BitChannel {
    fn push_sample(&mut self, sample: f32, out: &mut Vec<bool>) {
        // Bin this sample into first vs. second half of the current bit.
        if self.pos < SAMPLES_PER_HALF {
            self.int_first += sample;
        } else {
            self.int_second += sample;
        }
        self.pos += 1.0;
        if self.pos >= SAMPLES_PER_BIT {
            // Symbol decision. Biphase '0' = (+, -) so first > second.
            let symbol = self.int_first > self.int_second;
            let bit = symbol != self.prev_symbol;
            out.push(bit);
            self.prev_symbol = symbol;
            self.pos -= SAMPLES_PER_BIT;
            // Carry residual sample energy from the end of this bit
            // into the start of the next, so we don't lose a partial
            // sample to fractional drift.
            self.int_first = 0.0;
            self.int_second = 0.0;
        }
    }
}

pub struct RdsDemod {
    aa: RealFir,
    decim_counter: usize,
    /// Two parallel matched filters, offset by SAMPLES_PER_HALF so we
    /// don't have to guess which biphase pairing is the right one.
    a: BitChannel,
    b: BitChannel,
}

impl RdsDemod {
    pub fn new() -> Self {
        // 5 kHz cutoff comfortably passes the 1187.5 baud BPSK
        // raised-cosine spectrum (extends to ~2.4 kHz) and rejects the
        // 113.5 kHz image from the cos(3·φ) mixer.
        let aa = design_lowpass_kaiser(5_000.0, MPX_RATE, 127, 9.0);
        // Channel B starts a half-bit ahead of channel A.
        let b = BitChannel {
            pos: SAMPLES_PER_HALF,
            ..Default::default()
        };
        Self {
            aa: RealFir::new(aa),
            decim_counter: 0,
            a: BitChannel::default(),
            b,
        }
    }

    #[inline]
    pub fn push_sample(
        &mut self,
        mpx_sample: f32,
        ref_57khz: f32,
        out_a: &mut Vec<bool>,
        out_b: &mut Vec<bool>,
    ) {
        let demod = 2.0 * mpx_sample * ref_57khz;
        let filt = self.aa.process_sample(demod);
        self.decim_counter += 1;
        if self.decim_counter < DECIM {
            return;
        }
        self.decim_counter = 0;
        self.a.push_sample(filt, out_a);
        self.b.push_sample(filt, out_b);
    }
}
