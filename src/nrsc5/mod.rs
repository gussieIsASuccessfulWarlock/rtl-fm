//! HD Radio (NRSC-5) decoder.
//!
//! Layered decoder stack:
//!
//! | Layer                    | File           | Status |
//! |--------------------------|----------------|--------|
//! | Resampler 2.4 MS/s→744k  | `resample.rs`  | done   |
//! | OFDM symbol framer + FFT | `ofdm.rs`      | done   |
//! | Reference / sync         | `sync.rs`      | done   |
//! | Constellation demap      | `demap.rs`     | done   |
//! | Block deinterleaver      | `interleave.rs`| done   |
//! | Viterbi (rate-5/8)       | `viterbi.rs`   | done   |
//! | Reed-Solomon (255,223)   | `rs.rs`        | done   |
//! | L1 framing               | `frame.rs`     | done   |
//! | AAS / PSD / SIG / LOT    | `aas.rs`       | done   |
//!
//! Reference: Theori `nrsc5` C implementation; NRSC-5-D standard.

pub mod aas;
pub mod consts;
pub mod demap;
pub mod frame;
pub mod interleave;
pub mod ofdm;
pub mod resample;
pub mod rs;
pub mod sync;
pub mod viterbi;

use std::sync::Arc;

use tokio::sync::broadcast;
use tracing::{debug, info};

use crate::channelizer::metadata::StationMetadata;
use crate::nrsc5::aas::AasDecoder;
use crate::nrsc5::consts::FRAME_SYMBOLS;
use crate::nrsc5::demap::P1_DATA_SC;
use crate::nrsc5::frame::FrameDecoder;
use crate::nrsc5::interleave::Deinterleaver;
use crate::nrsc5::ofdm::OfdmFramer;
use crate::nrsc5::resample::ComplexResampler;
use crate::nrsc5::viterbi::Viterbi;
use crate::usb::transfer::IqChunk;

/// Driver for one HD-Radio decoder instance.
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

    pub async fn run(mut self) {
        info!("NRSC-5 decoder started");
        let mut iq_complex: Vec<num_complex::Complex<f32>> = Vec::with_capacity(16_384);
        let mut resampled: Vec<num_complex::Complex<f32>> = Vec::with_capacity(8_192);

        let mut deinterleaver = Deinterleaver::new();
        let mut viterbi = Viterbi::new();
        let mut frame_dec = FrameDecoder::new();
        let mut aas = AasDecoder::new();

        let mut soft_buf: Vec<u8> = Vec::with_capacity(P1_DATA_SC.len() * 2);
        let mut deint_buf: Vec<u8> = Vec::with_capacity(P1_DATA_SC.len() * 2);
        let mut frame_soft_bits: Vec<u8> = Vec::with_capacity(P1_DATA_SC.len() * 2 * FRAME_SYMBOLS);
        let mut pdus: Vec<frame::P1Pdu> = Vec::new();

        let mut frames_processed: u64 = 0;

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

            while self.framer.process_one_symbol() {
                if !self.framer.sync.frame_aligned {
                    continue;
                }

                let fft_bins = self.framer.fft_buf();
                let ref_est = &self.framer.sync.channel_estimates;

                soft_buf.clear();
                demap::equalize_and_demap(fft_bins, ref_est, &P1_DATA_SC, &mut soft_buf);

                deint_buf.clear();
                deinterleaver.process(&soft_buf, &mut deint_buf);
                frame_soft_bits.extend_from_slice(&deint_buf);

                if frame_soft_bits.len() >= P1_DATA_SC.len() * 2 * FRAME_SYMBOLS {
                    let decoded = viterbi.decode(&frame_soft_bits);
                    frame_soft_bits.clear();

                    pdus.clear();
                    frame_dec.process_frame(decoded, &mut pdus);
                    aas.process_pdus(&pdus);
                    frames_processed += 1;

                    if frames_processed.is_multiple_of(10) {
                        let dto = aas.to_metadata_dto();
                        if dto.radiotext.is_some() || dto.ps.is_some() {
                            debug!("NRSC-5 PSD: {:?} {:?}", dto.ps, dto.radiotext);
                        }
                        if !aas.completed_objects.is_empty() {
                            for obj in aas.completed_objects.drain(..) {
                                let mime: Option<String> = if obj.data.len() > 4
                                    && obj.data[0] == 0xFF && obj.data[1] == 0xD8
                                {
                                    Some("image/jpeg".to_string())
                                } else if obj.data.len() > 8
                                    && &obj.data[0..8] == b"\x89PNG\r\n\x1a\n"
                                {
                                    Some("image/png".to_string())
                                } else {
                                    None
                                };
                                info!(
                                    "NRSC-5 LOT complete: {} bytes, mime={:?}, lot_id={}",
                                    obj.data.len(), mime, obj.lot_id
                                );
                                self.metadata.update_hd_album_art(mime, obj.data);
                            }
                        }
                    }

                    if frames_processed.is_multiple_of(54) {
                        let dto = aas.to_metadata_dto();
                        self.metadata.update_hd(aas.to_hd_metadata());
                        let rds_meta = crate::dsp::rds_decoder::RdsMetadata {
                            pi: None,
                            callsign: dto.callsign,
                            ps: dto.ps,
                            rt: dto.radiotext,
                            pty: None,
                            pty_name: None,
                            tp: false,
                            ta: false,
                            ms_music: true,
                            groups_decoded: 0,
                            blocks_dropped: 0,
                        };
                        self.metadata.update_rds(rds_meta);
                    }
                }
            }
        }

        info!("NRSC-5 decoder exiting after {frames_processed} frames");
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
