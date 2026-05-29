//! Streaming WAV writer (debug endpoint).
//!
//! Emits a RIFF header with a "huge" data-size so browsers/players
//! treat the stream as open-ended. Body is 24-bit little-endian
//! signed interleaved stereo at 44.1 kHz.

use crate::encode::{PCM_BITS, PCM_CHANNELS, PCM_SAMPLE_RATE};

/// Build the WAV header. `data_size` is a fake huge value used so
/// that the WAV header reads as a near-infinite stream.
pub fn header() -> Vec<u8> {
    let sample_rate = PCM_SAMPLE_RATE;
    let channels = PCM_CHANNELS;
    let bits = PCM_BITS;
    let block_align: u16 = channels * (bits / 8);
    let byte_rate = sample_rate * u32::from(block_align);
    // Fake data size: large but valid; reserve room so the RIFF total
    // (data + 36 header bytes) still fits in u32.
    let fake_data: u32 = 0xFFFF_FFE0u32.saturating_sub(36);
    let fake_riff: u32 = fake_data + 36;

    let mut h = Vec::with_capacity(44);
    h.extend_from_slice(b"RIFF");
    h.extend_from_slice(&fake_riff.to_le_bytes());
    h.extend_from_slice(b"WAVE");
    h.extend_from_slice(b"fmt ");
    h.extend_from_slice(&16u32.to_le_bytes()); // PCM fmt chunk size
    h.extend_from_slice(&1u16.to_le_bytes()); // PCM format
    h.extend_from_slice(&channels.to_le_bytes());
    h.extend_from_slice(&sample_rate.to_le_bytes());
    h.extend_from_slice(&byte_rate.to_le_bytes());
    h.extend_from_slice(&block_align.to_le_bytes());
    h.extend_from_slice(&bits.to_le_bytes());
    h.extend_from_slice(b"data");
    h.extend_from_slice(&fake_data.to_le_bytes());
    h
}

/// Encode a slice of i32 samples (lower 24 bits significant, sign-
/// extended) to 24-bit little-endian PCM bytes.
pub fn encode_block(samples: &[i32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(samples.len() * 3);
    for &s in samples {
        let s = s.clamp(-(1 << 23), (1 << 23) - 1);
        let b = s.to_le_bytes();
        out.extend_from_slice(&b[0..3]);
    }
    out
}
