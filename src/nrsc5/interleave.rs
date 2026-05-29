//! Frequency and time deinterleaving for NRSC-5 P1 logical channel.

use crate::nrsc5::demap::P1_DATA_SC;

/// Frequency deinterleaver permutation.
/// Reverse the transmit interleaver mapping for P1 data subcarriers.
/// The interleaver permutes within each partition.
struct FreqDeinterleaver {
    perm: Vec<usize>,
}

impl FreqDeinterleaver {
    fn new() -> Self {
        let n = P1_DATA_SC.len();
        let mut perm = vec![0usize; n];
        let mut idx = 0usize;
        for p in 0..n {
            let step = 107;
            idx = (idx + step) % n;
            perm[idx] = p;
        }
        Self { perm }
    }

    fn deinterleave(&self, src: &[u8], dst: &mut Vec<u8>) {
        dst.clear();
        dst.resize(src.len(), 0);
        for (i, &p) in self.perm.iter().enumerate() {
            if p < src.len() {
                dst[i] = src[p];
            }
        }
    }
}

static FREQ_DEINT: std::sync::LazyLock<FreqDeinterleaver> =
    std::sync::LazyLock::new(FreqDeinterleaver::new);

/// Time deinterleaver with configurable delay (32 symbols for P1).
pub struct TimeDeinterleaver {
    delay: usize,
    buf: Vec<Vec<u8>>,
    wptr: usize,
    symbol_size: usize,
}

impl TimeDeinterleaver {
    pub fn new(delay: usize, symbol_size: usize) -> Self {
        Self {
            delay,
            buf: vec![vec![0u8; symbol_size]; delay],
            wptr: 0,
            symbol_size,
        }
    }

    /// Write one symbol of soft bits, read out the deinterleaved output.
    pub fn process(&mut self, symbol_soft: &[u8], out: &mut Vec<u8>) {
        let len = symbol_soft.len().min(self.symbol_size);
        self.buf[self.wptr][..len].copy_from_slice(&symbol_soft[..len]);
        let rptr = (self.wptr + 1) % self.delay;
        out.clear();
        out.extend_from_slice(&self.buf[rptr]);
        self.wptr = (self.wptr + 1) % self.delay;
    }
}

/// Deinterleave one symbol's worth of soft bits.
/// Applies frequency deinterleaving then time deinterleaving.
pub struct Deinterleaver {
    time: TimeDeinterleaver,
    freq_buf: Vec<u8>,
}

impl Deinterleaver {
    pub fn new() -> Self {
        let symbol_soft_bits = P1_DATA_SC.len() * 2;
        Self {
            time: TimeDeinterleaver::new(32, symbol_soft_bits),
            freq_buf: Vec::with_capacity(symbol_soft_bits),
        }
    }

    pub fn process(&mut self, symbol_soft: &[u8], out: &mut Vec<u8>) {
        FREQ_DEINT.deinterleave(symbol_soft, &mut self.freq_buf);
        self.time.process(&self.freq_buf, out);
    }
}

impl Default for Deinterleaver {
    fn default() -> Self { Self::new() }
}
