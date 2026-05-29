//! NRSC-5 numeric constants used by the decoder pipeline.

/// RTL2832U complex sample rate used by this project.
pub const RTL_IQ_RATE: f32 = 2_400_000.0;

/// NRSC-5 baseband processing rate used by common open implementations.
///
/// We convert 2.4 MS/s -> 744 kS/s with ratio 31/100.
pub const NRSC5_IQ_RATE: f32 = 744_000.0;
pub const RESAMPLE_INTERP: usize = 31;
pub const RESAMPLE_DECIM: usize = 100;

/// OFDM framing constants (FM hybrid mode).
///
/// These are used only for symbol slicing at this stage; exact pilot/
/// reference positioning and mode-specific interpretation are added by
/// sync + demapper stages.
pub const OFDM_FFT_LEN: usize = 2048;
pub const OFDM_CP_LEN: usize = 112;
pub const OFDM_SYMBOL_LEN: usize = OFDM_FFT_LEN + OFDM_CP_LEN;
