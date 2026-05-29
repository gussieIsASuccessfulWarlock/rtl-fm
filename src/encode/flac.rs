//! Streaming FLAC encoder.
//!
//! Strategy
//! --------
//! `flacenc` exposes `encode_fixed_size_frame`, which encodes a single
//! FLAC frame from a `FrameBuf` with a caller-supplied frame number.
//! We:
//!
//! 1. Hand-write the FLAC stream header (the `fLaC` marker + one
//!    STREAMINFO metadata block) once at the start of each HTTP
//!    response.
//! 2. For every block of `BLOCK_SIZE` interleaved L/R samples, encode
//!    it as one frame with our own monotonically-increasing frame
//!    counter, and append the frame bytes.
//!
//! Decoders see a single, coherent native-FLAC stream with sequential
//! frame numbers (no "sample/frame number mismatch" warnings).

use anyhow::{Result, anyhow};
use flacenc::component::{BitRepr, StreamInfo};
use flacenc::error::Verify;
use flacenc::source::{FrameBuf, MemSource, Source};

use crate::encode::{PCM_BITS, PCM_CHANNELS, PCM_SAMPLE_RATE};

/// FLAC block size. 4096 frames = ~93 ms at 44.1 kHz.
pub const BLOCK_SIZE: usize = 4096;

pub struct FlacEncoder {
    config: flacenc::error::Verified<flacenc::config::Encoder>,
    stream_info: StreamInfo,
    frame_buf: FrameBuf,
    next_frame_idx: usize,
    header_emitted: bool,
}

impl FlacEncoder {
    pub fn new() -> Self {
        let config = flacenc::config::Encoder::default()
            .into_verified()
            .map_err(|(_, e)| e)
            .expect("default flacenc encoder config must verify");
        let stream_info = StreamInfo::new(
            PCM_SAMPLE_RATE as usize,
            PCM_CHANNELS as usize,
            PCM_BITS as usize,
        )
        .expect("FLAC StreamInfo for 24-bit 44.1 kHz stereo must verify");
        let frame_buf = FrameBuf::with_size(PCM_CHANNELS as usize, BLOCK_SIZE)
            .expect("FLAC FrameBuf with BLOCK_SIZE must verify");
        Self {
            config,
            stream_info,
            frame_buf,
            next_frame_idx: 0,
            header_emitted: false,
        }
    }

    /// Encode a single fixed-size block of interleaved L/R 24-bit
    /// samples. On the first call the returned buffer is preceded by
    /// the FLAC header; on subsequent calls it is just the next frame.
    pub fn encode_block(&mut self, interleaved: &[i32]) -> Result<Vec<u8>> {
        if interleaved.len() != BLOCK_SIZE * PCM_CHANNELS as usize {
            return Err(anyhow!(
                "encode_block: expected {} samples, got {}",
                BLOCK_SIZE * PCM_CHANNELS as usize,
                interleaved.len()
            ));
        }

        // Pump samples into the FrameBuf via MemSource.
        let mut src = MemSource::from_samples(
            interleaved,
            PCM_CHANNELS as usize,
            PCM_BITS as usize,
            PCM_SAMPLE_RATE as usize,
        );
        src.read_samples(BLOCK_SIZE, &mut self.frame_buf)
            .map_err(|e| anyhow!("flacenc read_samples: {e:?}"))?;

        let frame = flacenc::encode_fixed_size_frame(
            &self.config,
            &self.frame_buf,
            self.next_frame_idx,
            &self.stream_info,
        )
        .map_err(|e| anyhow!("flacenc encode_fixed_size_frame: {e:?}"))?;
        self.next_frame_idx = self.next_frame_idx.wrapping_add(1);

        let mut sink = flacenc::bitsink::ByteSink::new();
        frame
            .write(&mut sink)
            .map_err(|e| anyhow!("flacenc frame write: {e:?}"))?;
        let frame_bytes = sink.into_inner();

        let mut out = Vec::with_capacity(if self.header_emitted { 0 } else { 42 } + frame_bytes.len());
        if !self.header_emitted {
            out.extend_from_slice(&build_stream_header());
            self.header_emitted = true;
        }
        out.extend_from_slice(&frame_bytes);
        Ok(out)
    }
}

/// Hand-write a minimal FLAC stream header: the `fLaC` marker plus a
/// "last" STREAMINFO metadata block describing the format. Unknown
/// fields (frame sizes, total samples, MD5) are zero, which is the
/// FLAC-spec way of saying "streaming".
fn build_stream_header() -> Vec<u8> {
    let mut h = Vec::with_capacity(4 + 4 + 34);
    h.extend_from_slice(b"fLaC");
    // metadata block header: last=1, type=0 (STREAMINFO)
    h.push(0x80);
    // 24-bit length = 34
    h.extend_from_slice(&[0, 0, 34]);

    let min_block = BLOCK_SIZE as u16;
    let max_block = BLOCK_SIZE as u16;
    h.extend_from_slice(&min_block.to_be_bytes());
    h.extend_from_slice(&max_block.to_be_bytes());
    // min/max frame size: 24-bit zeros = unknown.
    h.extend_from_slice(&[0, 0, 0]);
    h.extend_from_slice(&[0, 0, 0]);

    // Bits 80-143 of STREAMINFO body, packed into 8 bytes big-endian:
    //   sample_rate (20) | channels-1 (3) | bps-1 (5) | total_samples (36)
    let sr: u64 = (PCM_SAMPLE_RATE as u64) & 0xf_ffff;
    let ch: u64 = (PCM_CHANNELS as u64 - 1) & 0x7;
    let bps: u64 = (PCM_BITS as u64 - 1) & 0x1f;
    let total: u64 = 0; // streaming: total samples unknown
    let packed: u64 = (sr << 44) | (ch << 41) | (bps << 36) | total;
    h.extend_from_slice(&packed.to_be_bytes());

    // MD5 of the unencoded audio: 16 zero bytes (unknown for streaming).
    h.extend_from_slice(&[0u8; 16]);

    debug_assert_eq!(h.len(), 4 + 4 + 34);
    h
}
