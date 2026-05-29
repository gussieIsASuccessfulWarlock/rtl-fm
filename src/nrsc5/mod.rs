//! HD Radio (NRSC-5) decoder skeleton.
//!
//! This module is being built out incrementally. The end goal is to
//! decode the digital sidebands of a hybrid-IBOC FM signal and
//! recover:
//!
//! * **PSD** (Program Service Data) — current artist / title.
//! * **SIG** (Station Information Guide) — extended station metadata.
//! * **LOT** (Large Object Transfer) — album-art JPEGs.
//!
//! The audio HDC codec is intentionally out of scope; we already have
//! a clean analog FM audio path and HDC is proprietary.
//!
//! Layered decoder stack (status):
//!
//! | Layer                    | File           | Status |
//! |--------------------------|----------------|--------|
//! | Resampler 2.4 MS/s→744k  | `resample.rs`  | done   |
//! | OFDM symbol framer       | `ofdm.rs`      | done   |
//! | Reference / sync         | `sync.rs`      | TODO   |
//! | Constellation demap      | `demap.rs`     | TODO   |
//! | Block deinterleaver      | `interleave.rs`| TODO   |
//! | Viterbi (rate-5/8)       | `viterbi.rs`   | TODO   |
//! | Reed-Solomon (255,223)   | `rs.rs`        | TODO   |
//! | L1 framing               | `frame.rs`     | TODO   |
//! | AAS / PSD / SIG / LOT    | `aas.rs`       | TODO   |
//!
//! Reference: Theori `nrsc5` C implementation; NRSC-5-D standard.
//! Every register/protocol decision cites a section.

pub mod consts;
pub mod ofdm;
pub mod resample;

use std::sync::Arc;

use tokio::sync::broadcast;
use tracing::{debug, info};

use crate::channelizer::metadata::StationMetadata;
use crate::nrsc5::ofdm::OfdmFramer;
use crate::nrsc5::resample::ComplexResampler;
use crate::usb::transfer::IqChunk;

/// Driver for one HD-Radio decoder instance. Subscribes to the
/// channel's raw 2.4 MS/s IQ stream, resamples to the OFDM symbol
/// rate, frames symbols, and (eventually) decodes data.
pub struct NrscDecoder {
    iq_rx: broadcast::Receiver<IqChunk>,
    metadata: StationMetadata,
    resamp: ComplexResampler,
    framer: OfdmFramer,
}

impl NrscDecoder {
    pub fn new(iq_rx: broadcast::Receiver<IqChunk>, metadata: StationMetadata) -> Self {
        Self {
            iq_rx,
            metadata,
            resamp: ComplexResampler::new(),
            framer: OfdmFramer::new(),
        }
    }

    /// Run the decoder loop until the IQ broadcast closes.
    pub async fn run(mut self) {
        info!("NRSC-5 decoder spawned (skeleton)");
        let mut iq_complex: Vec<num_complex::Complex<f32>> = Vec::with_capacity(16_384);
        let mut resampled: Vec<num_complex::Complex<f32>> = Vec::with_capacity(8_192);
        loop {
            let chunk = match self.iq_rx.recv().await {
                Ok(c) => c,
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => break,
            };
            crate::dsp::u8_iq_to_complex(&chunk, &mut iq_complex);
            resampled.clear();
            self.resamp.process(&iq_complex, &mut resampled);
            self.framer.feed(&resampled);
            // Future: pass extracted symbols to sync + decoder stages.
            let symbols_seen = self.framer.symbols_seen();
            if symbols_seen.is_multiple_of(1024) && symbols_seen > 0 {
                debug!("NRSC-5: {symbols_seen} OFDM symbols framed so far");
            }
        }
        let _ = &self.metadata; // will be used once we have decoded PSD
        info!("NRSC-5 decoder exiting");
    }
}

/// Convenience: spawn a decoder task for a given station.
pub fn spawn(
    iq_rx: broadcast::Receiver<IqChunk>,
    metadata: StationMetadata,
) -> tokio::task::JoinHandle<()> {
    let dec = NrscDecoder::new(iq_rx, metadata);
    tokio::spawn(async move { dec.run().await })
}

/// Reserved for downstream album-art delivery.
#[derive(Debug, Clone, Default)]
pub struct AlbumArt {
    pub mime: Option<String>,
    pub bytes: Arc<[u8]>,
}
