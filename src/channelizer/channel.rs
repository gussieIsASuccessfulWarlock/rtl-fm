//! Per-station DSP task: IQ → DDC → FM demod → MPX →
//! { stereo audio path, RDS metadata path }.

use std::sync::Arc;

use num_complex::Complex;
use rand::rngs::SmallRng;
use rand::Rng;
use rand::SeedableRng;
use tokio::sync::broadcast;
use tracing::{info, warn};

use crate::channelizer::metadata::StationMetadata;
use crate::dsp::fir::{design_lowpass_kaiser, ComplexDecimFir, RealFir};
use crate::dsp::fm::FmDemod;
use crate::dsp::iir::{Deemphasis, TAU_50US};
use crate::dsp::nco::Nco;
use crate::dsp::pll::PilotPll;
use crate::dsp::rds::RdsDemod;
use crate::dsp::rds_decoder::RdsDecoder;
use crate::dsp::resample::PolyphaseResamplerStereo;
use crate::dsp::stereo::StereoDecoder;
use crate::dsp::u8_iq_to_complex;
use crate::encode::PcmBlock;
use crate::usb::transfer::IqChunk;

const IQ_RATE: f32 = 2_400_000.0;
const DDC_DECIM: usize = 10;
const BASEBAND_RATE: f32 = IQ_RATE / DDC_DECIM as f32;
const DDC_CUTOFF: f32 = 120_000.0;
const DDC_TAPS: usize = 127;

/// Lock-detector thresholds used by the stereo matrix fade-in.
const LOCK_LOW: f32 = 0.02;
const LOCK_HIGH: f32 = 0.06;

/// Push the latest RDS metadata snapshot upstream every N MPX blocks.
const RDS_UPDATE_EVERY_CHUNKS: u32 = 4;

pub struct ChannelTask {
    pub station_hz: u32,
    pub center_hz: u32,
    pub iq_rx: broadcast::Receiver<IqChunk>,
    pub pcm_tx: broadcast::Sender<PcmBlock>,
    pub metadata: StationMetadata,
}

/// Build a narrow bandpass FIR centered at 19 kHz by frequency-shifting
/// a real lowpass — used to pluck the pilot out of the MPX before
/// feeding the pilot PLL.
fn bandpass_19khz(fs: f32) -> Vec<f32> {
    let lpf = design_lowpass_kaiser(2_000.0, fs, 127, 10.0);
    let omega = 2.0 * std::f32::consts::PI * 19_000.0 / fs;
    let n = lpf.len();
    let mut bp = Vec::with_capacity(n);
    for (k, &h) in lpf.iter().enumerate() {
        let kk = k as f32 - (n as f32 - 1.0) * 0.5;
        bp.push(2.0 * h * (omega * kk).cos());
    }
    bp
}

fn tone_power(samples: &[f32], fs: f32, freq_hz: f32) -> f32 {
    let n = samples.len() as f32;
    let omega = 2.0 * std::f32::consts::PI * freq_hz / fs;
    let coeff = 2.0 * omega.cos();
    let mut q0;
    let mut q1 = 0.0f32;
    let mut q2 = 0.0f32;

    for (i, &sample) in samples.iter().enumerate() {
        let w = 0.5 * (1.0 - (2.0 * std::f32::consts::PI * i as f32 / n).cos());
        q0 = sample * w + coeff * q1 - q2;
        q2 = q1;
        q1 = q0;
    }

    q1 * q1 + q2 * q2 - coeff * q1 * q2
}

fn estimate_pilot_snr_db(mpx: &[f32]) -> Option<f32> {
    if mpx.len() < 512 {
        return None;
    }
    let pilot_pow = tone_power(mpx, BASEBAND_RATE, 19_000.0);
    let ref_pow = tone_power(mpx, BASEBAND_RATE, 17_500.0);
    Some(10.0 * (pilot_pow / (ref_pow + 1e-18)).log10())
}

pub async fn run(mut task: ChannelTask) {
    let offset_hz = task.station_hz as f32 - task.center_hz as f32;
    info!(
        "channel task spawned: station={} kHz, center={} kHz, offset={offset_hz} Hz",
        task.station_hz / 1000,
        task.center_hz / 1000
    );

    let mut nco = Nco::new(-offset_hz, IQ_RATE);

    let taps = design_lowpass_kaiser(DDC_CUTOFF, IQ_RATE, DDC_TAPS, 9.0);
    let mut ddc = ComplexDecimFir::new(taps, DDC_DECIM);
    let mut fm = FmDemod::new(BASEBAND_RATE, 75_000.0);

    let mut pilot_bp = RealFir::new(bandpass_19khz(BASEBAND_RATE));
    let mut pll = PilotPll::new(BASEBAND_RATE);
    let mut stereo = StereoDecoder::new(BASEBAND_RATE);
    let mut rds_demod = RdsDemod::new();
    // Two parallel decoders, one for each biphase pair alignment. The
    // demodulator emits bits onto both streams; whichever decoder
    // achieves block sync first is the one we pick up metadata from.
    let mut rds_decoder_a = RdsDecoder::new();
    let mut rds_decoder_b = RdsDecoder::new();
    let mut chosen_decoder: Option<u8> = None;

    let mut deem_l = Deemphasis::new(TAU_50US, BASEBAND_RATE);
    let mut deem_r = Deemphasis::new(TAU_50US, BASEBAND_RATE);

    let mut resamp = PolyphaseResamplerStereo::new(BASEBAND_RATE, 44_100.0, 147, 800, 32);

    let mut iq_complex: Vec<Complex<f32>> = Vec::with_capacity(16_384);
    let mut mixed: Vec<Complex<f32>> = Vec::with_capacity(16_384);
    let mut baseband: Vec<Complex<f32>> = Vec::with_capacity(2_048);
    let mut mpx: Vec<f32> = Vec::with_capacity(2_048);
    let mut lr_240: Vec<f32> = Vec::with_capacity(4_096);
    let mut lr_44: Vec<f32> = Vec::with_capacity(2_048);
    let mut block_buf: Vec<i32> = Vec::with_capacity(crate::encode::flac::BLOCK_SIZE * 2);
    let mut rds_bits_a: Vec<bool> = Vec::with_capacity(256);
    let mut rds_bits_b: Vec<bool> = Vec::with_capacity(256);

    let mut rng = SmallRng::from_entropy();
    let mut chunk_counter: u32 = 0;

    loop {
        if task.pcm_tx.receiver_count() == 0 {
            break;
        }

        let chunk = match task.iq_rx.recv().await {
            Ok(c) => c,
            Err(broadcast::error::RecvError::Lagged(n)) => {
                warn!("channel {} lagged {n} IQ chunks", task.station_hz);
                continue;
            }
            Err(broadcast::error::RecvError::Closed) => {
                warn!("channel {} IQ broadcast closed", task.station_hz);
                return;
            }
        };

        u8_iq_to_complex(&chunk, &mut iq_complex);

        mixed.clear();
        for &z in &iq_complex {
            let osc = nco.step();
            mixed.push(z * osc);
        }

        baseband.clear();
        ddc.process(&mixed, &mut baseband);

        mpx.clear();
        fm.process(&baseband, &mut mpx);

        // Drive the pilot PLL + stereo + RDS in lock-step through MPX.
        lr_240.clear();
        rds_bits_a.clear();
        rds_bits_b.clear();
        for &m in &mpx {
            let pilot = pilot_bp.process_sample(m);
            let refs = pll.step(pilot);
            let lock = pll.lock_metric();
            let mix = ((lock - LOCK_LOW) / (LOCK_HIGH - LOCK_LOW)).clamp(0.0, 1.0);
            stereo.push_sample(m, refs.cos_2phi, mix, &mut lr_240);
            rds_demod.push_sample(m, refs.sin_3phi, &mut rds_bits_a, &mut rds_bits_b);
        }

        for &bit in &rds_bits_a {
            rds_decoder_a.push_bit(bit);
        }
        for &bit in &rds_bits_b {
            rds_decoder_b.push_bit(bit);
        }
        // Lock onto whichever alignment has seen a group decode first.
        if chosen_decoder.is_none() {
            if rds_decoder_a.meta.groups_decoded > 0 {
                chosen_decoder = Some(0);
                info!("RDS alignment A chosen for station {}", task.station_hz);
            } else if rds_decoder_b.meta.groups_decoded > 0 {
                chosen_decoder = Some(1);
                info!("RDS alignment B chosen for station {}", task.station_hz);
            }
        }

        chunk_counter = chunk_counter.wrapping_add(1);
        if chunk_counter.is_multiple_of(RDS_UPDATE_EVERY_CHUNKS) {
            let meta = if chosen_decoder == Some(1) {
                rds_decoder_b.meta.clone()
            } else {
                rds_decoder_a.meta.clone()
            };
            task.metadata.update_rds(meta, estimate_pilot_snr_db(&mpx));
        }

        // Per-channel deemphasis (interleaved L,R).
        let mut k = 0usize;
        while k < lr_240.len() {
            lr_240[k] = deem_l.process_sample(lr_240[k]);
            lr_240[k + 1] = deem_r.process_sample(lr_240[k + 1]);
            k += 2;
        }

        lr_44.clear();
        resamp.process(&lr_240, &mut lr_44);

        for v in lr_44.iter() {
            let scaled = (v * 0.5) * ((1i32 << 23) - 1) as f32;
            let tpdf: f32 = rng.gen::<f32>() - rng.gen::<f32>();
            let dithered = scaled + tpdf;
            let s = dithered.round() as i32;
            block_buf.push(s.clamp(-(1 << 23), (1 << 23) - 1));
        }

        let chunk_target = crate::encode::flac::BLOCK_SIZE * 2;
        while block_buf.len() >= chunk_target {
            let block: Arc<[i32]> = Arc::from(&block_buf[..chunk_target]);
            block_buf.drain(..chunk_target);
            let _ = task.pcm_tx.send(block);
        }
    }
    info!("channel task ending for station {}", task.station_hz);
    let final_meta = match chosen_decoder {
        Some(1) => rds_decoder_b.meta.clone(),
        _ => rds_decoder_a.meta.clone(),
    };
    task.metadata.update_rds(final_meta, None);
}
