pub mod aas;
pub mod block_eq;
pub mod consts;
pub mod demap;
pub mod frame;
pub mod interleave;
pub mod l1;
pub mod lot;
pub mod ofdm;
pub mod pids;
pub mod resample;
pub mod rs;
pub mod sync;
pub mod viterbi;

use rustfft::num_complex::Complex;
use std::sync::Arc;
use tokio::sync::broadcast;
use tracing::info;

use crate::channelizer::metadata::StationMetadata;
use crate::dsp::nco::Nco;
use crate::nrsc5::aas::{AasDemux, AasPacket};
use crate::nrsc5::block_eq::BlockEqualizer;
use crate::nrsc5::consts::BLKSZ;
use crate::nrsc5::frame::FrameParser;
use crate::nrsc5::lot::LotAssembler;
use crate::nrsc5::ofdm::OfdmFramer;
use crate::nrsc5::pids::PidsDecoder;
use crate::nrsc5::resample::{ComplexResampler, HalfbandDecimator};
use crate::nrsc5::sync::Sync;
use crate::nrsc5::viterbi::Viterbi;
use crate::usb::transfer::IqChunk;

pub fn spawn(
    iq_rx: broadcast::Receiver<IqChunk>,
    metadata: StationMetadata,
    station_hz: u32,
    center_hz: u32,
    input_rate: u32,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        if let Err(e) = run(iq_rx, Arc::new(metadata), station_hz, center_hz, input_rate).await {
            tracing::warn!("NRSC-5 task exit: {e:#}");
        }
    })
}

pub async fn run(
    mut iq_rx: broadcast::Receiver<IqChunk>,
    metadata: Arc<StationMetadata>,
    station_hz: u32,
    center_hz: u32,
    input_rate: u32,
) -> anyhow::Result<()> {
    let mut decoder = Nrsc5Decoder::new(metadata, station_hz, center_hz, input_rate);
    info!(
        "NRSC-5 decoder started (input_rate={input_rate} dedicated={})",
        decoder.dedicated()
    );

    loop {
        let chunk = match iq_rx.recv().await {
            Ok(c) => c,
            Err(broadcast::error::RecvError::Lagged(n)) => {
                info!("NRSC-5 lagged {n} IQ chunks; resetting decoder state");
                let meta = decoder.metadata.clone();
                decoder = Nrsc5Decoder::new(meta, station_hz, center_hz, input_rate);
                continue;
            }
            Err(broadcast::error::RecvError::Closed) => break,
        };
        decoder.process_chunk(&chunk);
    }
    info!(
        "NRSC-5 decoder exiting after {} P1 frames",
        decoder.frames_processed()
    );
    Ok(())
}

/// Self-contained NRSC-5 FM P1 decoder.
///
/// Owns all per-station DSP state: NCO, resampler, OFDM framer, frame
/// sync, block equalizer, both Viterbi instances (P1 + PIDS), the L2
/// frame parser, AAS demultiplexer, and LOT reassembler. Drives the
/// shared [`StationMetadata`] when station-short-name or album art
/// surface.
///
/// Use [`process_chunk`](Self::process_chunk) to feed raw u8 IQ samples
/// (interleaved I/Q at the configured `input_rate`). The same struct
/// powers both the live HD scan path and the offline replay harness so
/// there is one decode chain to debug, not two.
pub struct Nrsc5Decoder {
    pub metadata: Arc<StationMetadata>,
    station_hz: u32,
    center_hz: u32,
    input_rate: u32,
    dedicated: bool,

    nco: Nco,
    resamp_poly: Option<ComplexResampler>,
    resamp_half: Option<HalfbandDecimator>,
    framer: OfdmFramer,
    sync: Sync,
    block_eq: BlockEqualizer,
    viterbi: Viterbi,
    pids_viterbi: Viterbi,
    pids_decoder: PidsDecoder,
    frame_parser: FrameParser,
    aas_demux: AasDemux,
    lot_assembler: LotAssembler,

    iq_complex: Vec<Complex<f32>>,
    mixed: Vec<Complex<f32>>,
    resampled: Vec<Complex<f32>>,
    pm_buf: Vec<i8>,
    depunctured: Vec<i8>,
    decoded_bits: Vec<u8>,
    viterbi_pids_buf: Vec<i8>,
    decoded_pids_bits: Vec<u8>,
    block_sym_buf: Vec<Vec<Complex<f32>>>,

    symbols_since_lock: u64,
    frames_processed: u64,
    chunks_seen: u64,
    last_log: u64,
    first_lock: bool,
}

impl Nrsc5Decoder {
    pub fn new(
        metadata: Arc<StationMetadata>,
        station_hz: u32,
        center_hz: u32,
        input_rate: u32,
    ) -> Self {
        let dedicated = input_rate == crate::nrsc5::consts::NRSC5_RTL_RATE;
        let offset_hz = station_hz as f32 - center_hz as f32;
        let nco_rate = if dedicated {
            crate::nrsc5::consts::NRSC5_RTL_RATE as f32
        } else {
            crate::nrsc5::consts::RTL_IQ_RATE
        };
        let nco = Nco::new(-offset_hz, nco_rate);
        let resamp_poly = if !dedicated {
            Some(ComplexResampler::new())
        } else {
            None
        };
        let resamp_half = if dedicated {
            Some(HalfbandDecimator::new())
        } else {
            None
        };

        Self {
            metadata,
            station_hz,
            center_hz,
            input_rate,
            dedicated,
            nco,
            resamp_poly,
            resamp_half,
            framer: OfdmFramer::new(),
            sync: Sync::new(),
            block_eq: BlockEqualizer::new(),
            viterbi: Viterbi::new(),
            pids_viterbi: Viterbi::new(),
            pids_decoder: PidsDecoder::new(),
            frame_parser: FrameParser::default(),
            aas_demux: AasDemux::default(),
            lot_assembler: LotAssembler::default(),
            iq_complex: Vec::with_capacity(16_384),
            mixed: Vec::with_capacity(16_384),
            resampled: Vec::with_capacity(8_192),
            pm_buf: vec![0i8; 16 * 32 * 720],
            depunctured: Vec::new(),
            decoded_bits: Vec::new(),
            viterbi_pids_buf: Vec::with_capacity(240),
            decoded_pids_bits: Vec::with_capacity(80),
            block_sym_buf: Vec::with_capacity(BLKSZ),
            symbols_since_lock: 0,
            frames_processed: 0,
            chunks_seen: 0,
            last_log: 0,
            first_lock: true,
        }
    }

    pub fn dedicated(&self) -> bool {
        self.dedicated
    }
    pub fn station_hz(&self) -> u32 {
        self.station_hz
    }
    pub fn center_hz(&self) -> u32 {
        self.center_hz
    }
    pub fn input_rate(&self) -> u32 {
        self.input_rate
    }
    pub fn frames_processed(&self) -> u64 {
        self.frames_processed
    }
    pub fn chunks_seen(&self) -> u64 {
        self.chunks_seen
    }

    /// Feed one bulk-URB-sized chunk of raw u8 IQ samples (interleaved
    /// I/Q at `input_rate`). Drives the full PHY → L1 → L2 → AAS → LOT
    /// pipeline and updates [`Self::metadata`] when station name or
    /// album-art surface.
    pub fn process_chunk(&mut self, chunk: &[u8]) {
        crate::dsp::u8_iq_to_complex(chunk, &mut self.iq_complex);
        self.chunks_seen += 1;

        self.mixed.clear();
        for &z in &self.iq_complex {
            self.mixed.push(z * self.nco.step());
        }
        self.resampled.clear();
        if let Some(r) = self.resamp_poly.as_mut() {
            r.process(&self.mixed, &mut self.resampled);
        } else if let Some(r) = self.resamp_half.as_mut() {
            r.process(&self.mixed, &mut self.resampled);
        }
        self.framer.feed(&self.resampled);

        while self.framer.process_one_symbol() {
            self.sync.process_symbol(self.framer.fft_buf());

            if self.framer.symbols_seen - self.last_log >= 256 {
                self.last_log = self.framer.symbols_seen;
                info!(
                    "NRSC-5 status: chunks={} symbols={} cp_metric={:.3} cfo={:.1}Hz frame_aligned={} frame_metric={:.3} best_vote={}@{} bin_cfo={} bc={}",
                    self.chunks_seen,
                    self.framer.symbols_seen,
                    self.framer.acquisition.cp_metric,
                    self.framer.acquisition.cfo_hz,
                    self.sync.frame_aligned,
                    self.sync.frame_metric,
                    self.sync.best_count_recent,
                    self.sync.best_offset_recent,
                    self.sync.integer_cfo_bins,
                    self.sync.block_count,
                );
            }

            if !self.sync.frame_aligned {
                self.first_lock = true;
                self.block_sym_buf.clear();
                continue;
            }

            if self.first_lock {
                self.symbols_since_lock = self.sync.current_row() as u64;
                self.block_eq.init_costas(
                    &self.sync.costas_phase,
                    &self.sync.costas_freq,
                    self.sync.integer_cfo_bins,
                );
                self.block_sym_buf.clear();
                self.first_lock = false;
            }

            let row = (self.symbols_since_lock % BLKSZ as u64) as usize;
            if row == 0 {
                self.block_sym_buf.clear();
            }
            self.block_sym_buf.push(self.framer.fft_buf().to_vec());
            self.symbols_since_lock += 1;

            if row == BLKSZ - 1 && self.block_sym_buf.len() == BLKSZ {
                let block = ((self.sync.block_count as u64
                    + (self.symbols_since_lock / BLKSZ as u64).saturating_sub(1))
                    % 16) as usize;

                let dst_start = block * BLKSZ * 720;
                self.block_eq.process_block(
                    &self.block_sym_buf,
                    &mut self.pm_buf[dst_start..dst_start + BLKSZ * 720],
                );

                // PIDS decode (every block)
                crate::nrsc5::interleave::deinterleave_pids_fm(
                    &self.pm_buf,
                    &mut self.viterbi_pids_buf,
                    block,
                );
                self.pids_viterbi
                    .decode(&self.viterbi_pids_buf, &mut self.decoded_pids_bits, 80);
                descramble_bits(&mut self.decoded_pids_bits);
                self.pids_decoder.process(&self.decoded_pids_bits);
                if !self.pids_decoder.station_name.is_empty() {
                    let hd = crate::channelizer::metadata::HdMetadataDto {
                        station_name: Some(self.pids_decoder.station_name.clone()),
                        ..Default::default()
                    };
                    self.metadata.update_hd(hd);
                }

                // P1 decode once per full 16-block superframe.
                if block == 15 {
                    crate::nrsc5::interleave::deinterleave_p1_fm(
                        &self.pm_buf,
                        &mut self.depunctured,
                    );
                    self.viterbi.decode(
                        &self.depunctured,
                        &mut self.decoded_bits,
                        crate::nrsc5::consts::P1_FRAME_LEN_FM,
                    );
                    let raw_first32: String = self
                        .decoded_bits
                        .iter()
                        .take(32)
                        .map(|&b| if b != 0 { '1' } else { '0' })
                        .collect();
                    descramble_bits(&mut self.decoded_bits);
                    self.frames_processed += 1;
                    let desc_first32: String = self
                        .decoded_bits
                        .iter()
                        .take(32)
                        .map(|&b| if b != 0 { '1' } else { '0' })
                        .collect();
                    let soft_sample: Vec<i8> = (0..12).map(|i| self.depunctured[i]).collect();
                    let mut p1_payload = Vec::new();
                    let pci =
                        crate::nrsc5::l1::extract_p1_payload(&self.decoded_bits, &mut p1_payload);
                    info!(
                        "NRSC-5 P1 #{} PCI={:06X} soft[0..12]={:?} raw32={} desc32={}",
                        self.frames_processed, pci, soft_sample, raw_first32, desc_first32
                    );

                    let parsed = self.frame_parser.process(&p1_payload, pci, 0);
                    let aas_pkts = self.aas_demux.feed_stream(&parsed.aas_pdu);
                    for pkt in aas_pkts {
                        if let AasPacket::Lot { port, payload, .. } = pkt {
                            if let Some(obj) = self.lot_assembler.push(port, &payload) {
                                info!(
                                    "NRSC-5 LOT object: name={} mime={} size={}",
                                    obj.name,
                                    obj.mime,
                                    obj.data.len()
                                );
                                if obj.mime.starts_with("image/") {
                                    self.metadata.update_hd_album_art(Some(obj.mime), obj.data);
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

fn descramble_bits(bits: &mut [u8]) {
    let mut val: u16 = 0x3FF;
    for b in bits.iter_mut() {
        let bit = (((val >> 9) ^ val) & 1) as u16;
        val |= bit << 11;
        val >>= 1;
        *b ^= bit as u8;
    }
}
