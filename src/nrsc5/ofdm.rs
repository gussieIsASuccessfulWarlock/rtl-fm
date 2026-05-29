//! OFDM symbol framer for NRSC-5 samples.

use num_complex::Complex;

use crate::nrsc5::consts::OFDM_SYMBOL_LEN;

pub struct OfdmFramer {
    buf: Vec<Complex<f32>>,
    symbols_seen: u64,
}

impl OfdmFramer {
    pub fn new() -> Self {
        Self {
            buf: Vec::with_capacity(OFDM_SYMBOL_LEN * 4),
            symbols_seen: 0,
        }
    }

    pub fn feed(&mut self, input: &[Complex<f32>]) {
        self.buf.extend_from_slice(input);
        while self.buf.len() >= OFDM_SYMBOL_LEN {
            self.consume_symbol(&self.buf[..OFDM_SYMBOL_LEN]);
            self.buf.drain(..OFDM_SYMBOL_LEN);
            self.symbols_seen += 1;
        }
    }

    pub fn symbols_seen(&self) -> u64 {
        self.symbols_seen
    }

    fn consume_symbol(&self, _sym: &[Complex<f32>]) {
        // Placeholder: sync/demap stages will consume symbols.
    }
}

impl Default for OfdmFramer {
    fn default() -> Self {
        Self::new()
    }
}
