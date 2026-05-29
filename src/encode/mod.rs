pub mod flac;
pub mod wav;

/// A block of finished stereo PCM, in 24-bit-in-i32 (sign-extended)
/// interleaved L,R format at 44.1 kHz. Shared by the channelizer with
/// HTTP listeners via a tokio broadcast channel.
pub type PcmBlock = std::sync::Arc<[i32]>;

pub const PCM_SAMPLE_RATE: u32 = 44_100;
pub const PCM_CHANNELS: u16 = 2;
pub const PCM_BITS: u16 = 24;
