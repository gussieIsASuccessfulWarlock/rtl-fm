//! Channelizer: owns the per-station DSP tasks and the tuner policy.
//!
//! A single RTL-SDR samples a 2.4 MHz contiguous slice of spectrum
//! at any one time. Multiple simultaneous channels are supported
//! only when they all fit within the same tuner window.

pub mod channel;
pub mod metadata;

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

pub use metadata::{HdAlbumArt, RdsMetadataDto, StationMetadata};

use num_complex::Complex;
use parking_lot::Mutex;
use rustfft::FftPlanner;
use serde::Serialize;
use tokio::sync::broadcast;
use tracing::{info, warn};

use crate::dsp::u8_iq_to_complex;
use crate::encode::PcmBlock;
use crate::error::RtlError;
use crate::rtlsdr::{DEFAULT_SAMPLE_RATE, RtlSdr};
use crate::usb::transfer::IqChunk;

/// Half-bandwidth of the RTL-SDR tuner window we trust as "usable"
/// (excludes the edges where the analog filters roll off).
const USABLE_HALF_BW_HZ: u32 = 1_000_000;
/// Exclusion radius around the DC spike at the local oscillator.
/// The LO feedthrough on a typical RTL2832U is only a couple of kHz
/// wide. We used to exclude ±50 kHz here, which is wide enough to
/// swallow a real FM channel whose carrier is exactly 100 kHz off the
/// LO grid (e.g., a station at 89.1 MHz with LO=89.0).
const DC_EXCLUSION_HZ: u32 = 20_000;
/// FM broadcast channel spacing in Europe (100 kHz; tighter than US 200 kHz).
const FM_CHANNEL_STEP_HZ: u32 = 100_000;

#[derive(Debug, Clone, Serialize)]
pub struct ScannedChannel {
    pub name: String,
    pub freq_hz: u32,
    pub power_db: f32,
}

struct ActiveChannel {
    tx: broadcast::Sender<PcmBlock>,
    task: tokio::task::JoinHandle<()>,
    hd_task: tokio::task::JoinHandle<()>,
}

#[derive(Default)]
struct ActiveChannels {
    map: HashMap<u32, ActiveChannel>,
}

pub struct Channelizer {
    rtl: Arc<RtlSdr>,
    iq_tx: broadcast::Sender<IqChunk>,
    center_hz: Mutex<u32>,
    active: Mutex<ActiveChannels>,
    scanned: Mutex<Vec<ScannedChannel>>,
    /// Per-station RDS metadata that survives across channel task
    /// lifetimes — once we've ever decoded a PS/RT on this frequency,
    /// the snapshot remains queryable even after the listener leaves.
    metadata: Mutex<HashMap<u32, StationMetadata>>,
    /// Frequency the idle metadata refresher is currently parked on,
    /// if it is active. None means it's paused (e.g. a real listener
    /// is active).
    idle_refresher: Mutex<Option<u32>>,
    /// Serializes `tune` so two simultaneous `?retune=true` requests
    /// can't each enter the retune block and abort each other's task.
    tune_lock: tokio::sync::Mutex<()>,
}

impl Channelizer {
    pub fn new(rtl: Arc<RtlSdr>, iq_tx: broadcast::Sender<IqChunk>, center_hz: u32) -> Self {
        Self {
            rtl,
            iq_tx,
            center_hz: Mutex::new(center_hz),
            active: Mutex::new(ActiveChannels::default()),
            scanned: Mutex::new(Vec::new()),
            metadata: Mutex::new(HashMap::new()),
            idle_refresher: Mutex::new(None),
            tune_lock: tokio::sync::Mutex::new(()),
        }
    }

    pub fn idle_refresher_freq(&self) -> Option<u32> {
        *self.idle_refresher.lock()
    }

    pub fn set_idle_refresher_freq(&self, hz: Option<u32>) {
        *self.idle_refresher.lock() = hz;
    }

    /// True iff at least one listener is currently subscribed to any
    /// per-station broadcast.
    pub fn any_real_listeners(&self) -> bool {
        self.active
            .lock()
            .map
            .values()
            .any(|c| c.tx.receiver_count() > 0)
    }

    /// Park the tuner on `freq_hz`, spawn (or reuse) its channel task
    /// for `duration`, and let it accumulate RDS into our metadata
    /// store. Used by the idle refresher to keep all stations' info
    /// fresh while nobody is actively streaming.
    pub async fn refresh_metadata(
        self: &Arc<Self>,
        freq_hz: u32,
        duration: Duration,
    ) -> Result<(), RtlError> {
        // tune() drops any other in-flight channel; we hold the
        // receiver for `duration` so the channel task stays alive
        // long enough to decode RDS into the shared metadata store.
        let _holder = self.tune(freq_hz).await?;
        tokio::time::sleep(duration).await;
        Ok(())
    }

    /// Get-or-create the metadata slot for `freq_hz`. The slot lives
    /// for the lifetime of the Channelizer so a snapshot is still
    /// queryable after the channel task ends.
    pub fn metadata_for(&self, freq_hz: u32) -> StationMetadata {
        let mut meta = self.metadata.lock();
        meta.entry(freq_hz).or_default().clone()
    }

    pub fn metadata_snapshot(&self, freq_hz: u32) -> Option<RdsMetadataDto> {
        self.metadata.lock().get(&freq_hz).map(|m| m.snapshot())
    }

    pub fn album_art(&self, freq_hz: u32) -> Option<HdAlbumArt> {
        self.metadata.lock().get(&freq_hz).and_then(|m| m.album_art())
    }

    pub fn all_metadata_snapshots(&self) -> HashMap<u32, RdsMetadataDto> {
        self.metadata
            .lock()
            .iter()
            .map(|(k, v)| (*k, v.snapshot()))
            .collect()
    }

    pub fn center_hz(&self) -> u32 {
        *self.center_hz.lock()
    }

    pub fn set_center(&self, hz: u32) {
        *self.center_hz.lock() = hz;
    }

    pub fn scanned(&self) -> Vec<ScannedChannel> {
        self.scanned.lock().clone()
    }

    pub fn active_listeners(&self) -> HashMap<u32, usize> {
        self.active
            .lock()
            .map
            .iter()
            .map(|(k, v)| (*k, v.tx.receiver_count()))
            .collect()
    }

    /// Returns the band edge frequencies the current tuner window
    /// covers (taking into account DC exclusion and roll-off).
    pub fn window(&self) -> (u32, u32) {
        let c = *self.center_hz.lock();
        (c.saturating_sub(USABLE_HALF_BW_HZ), c + USABLE_HALF_BW_HZ)
    }

    /// Is `freq_hz` within the current tuner window and not blocked
    /// by the DC-spike exclusion zone?
    pub fn fits_window(&self, freq_hz: u32) -> bool {
        let c = self.center_hz();
        let dist = (freq_hz as i64 - c as i64).unsigned_abs() as u32;
        (DC_EXCLUSION_HZ..=USABLE_HALF_BW_HZ).contains(&dist)
    }

    /// Tune to `freq_hz` and return a PCM broadcast receiver.
    ///
    /// Single-station policy: at most one frequency is active at a
    /// time. A request for a different frequency drops the currently
    /// playing one (its HTTP listeners see EOF on their FLAC body).
    /// Multiple HTTP clients listening to the *same* freq share one
    /// DSP task via the broadcast channel.
    pub async fn tune(&self, freq_hz: u32) -> Result<broadcast::Receiver<PcmBlock>, RtlError> {
        let _serialize = self.tune_lock.lock().await;

        // Drop any other active channels — only one station plays at a
        // time. Same-freq listeners stay attached via get_or_spawn.
        {
            let mut active = self.active.lock();
            let to_drop: Vec<u32> = active
                .map
                .keys()
                .copied()
                .filter(|&f| f != freq_hz)
                .collect();
            for f in &to_drop {
                if let Some(ch) = active.map.remove(f) {
                    ch.task.abort();
                }
            }
            if !to_drop.is_empty() {
                info!(
                    "switching to {} kHz; dropped {} previous channel(s)",
                    freq_hz / 1000,
                    to_drop.len()
                );
            }
        }

        // Park the LO ~200 kHz off the requested station if we're not
        // already in a window that covers it.
        if !self.fits_window(freq_hz) {
            let new_center = freq_hz.saturating_sub(200_000);
            self.rtl.set_center_freq(new_center).await?;
            self.set_center(self.rtl.state().center_hz);
        }

        Ok(self.get_or_spawn(freq_hz))
    }

    fn get_or_spawn(&self, freq_hz: u32) -> broadcast::Receiver<PcmBlock> {
        let mut active = self.active.lock();
        // Reuse the existing channel only if its DSP task is still
        // running. After the last listener leaves, the task exits and
        // its broadcast::Sender clone drops, but our copy in the map
        // is still there; honouring it would hand the new listener a
        // dead stream.
        if let Some(existing) = active.map.get(&freq_hz) {
            if !existing.task.is_finished() {
                return existing.tx.subscribe();
            }
            active.map.remove(&freq_hz);
        }

        let (tx, rx) = broadcast::channel::<PcmBlock>(64);
        let task_tx = tx.clone();
        let iq_rx = self.iq_tx.subscribe();
        let iq_rx_hd = self.iq_tx.subscribe();
        let center_hz = self.center_hz();
        drop(active);
        let meta_slot = self.metadata_for(freq_hz);
        let meta_slot_audio = meta_slot.clone();
        let mut active = self.active.lock();
        let task = tokio::spawn(async move {
            channel::run(channel::ChannelTask {
                station_hz: freq_hz,
                center_hz,
                iq_rx,
                pcm_tx: task_tx,
                metadata: meta_slot_audio,
            })
            .await;
        });
        let hd_task = crate::nrsc5::spawn(iq_rx_hd, meta_slot);
        active.map.insert(freq_hz, ActiveChannel { tx, task, hd_task });
        rx
    }

    /// Reap channels whose DSP task has finished (no listeners, no
    /// producer). Called by the API layer occasionally.
    pub fn reap_idle(&self) {
        let mut active = self.active.lock();
        active.map.retain(|_, ch| {
            if ch.task.is_finished() {
                ch.hd_task.abort();
                false
            } else {
                true
            }
        });
    }

    /// Abort every active channel task. Used by /api/rescan and the
    /// retune path — both cause the tuner to hop windows, so any
    /// running DSP tasks would emit garbage.
    pub fn drop_all_channels(&self) {
        let mut active = self.active.lock();
        for (_, ch) in active.map.drain() {
            ch.task.abort();
            ch.hd_task.abort();
        }
    }

    /// Energy-scan the band by retuning across it in 2.4 MHz windows
    /// and FFT-thresholding each window. Replaces the cached channel
    /// list on success.
    pub async fn scan_band(&self, start_mhz: u32, end_mhz: u32) -> Result<Vec<ScannedChannel>, RtlError> {
        // Use a fixed mid-high tuner gain during the scan. AGC dynamically
        // backs the gain off when strong signals are present, which in
        // dense FM markets compresses the dynamic range and hides weak
        // stations behind the broadcast noise floor. After the scan we
        // restore AGC for normal streaming.
        let restore_agc = self.rtl.state().gain_tenth_db.is_none();
        if let Err(e) = self.rtl.set_tuner_gain(280).await {
            warn!("scan: setting manual gain failed (continuing on AGC): {e}");
        }

        // Two-pass energy scan with LOs offset by 1 MHz, intersected to
        // remove IQ-imbalance mirrors.
        info!("scan pass 1 ({start_mhz}..{end_mhz} MHz, even LOs)");
        let pass_a = self.scan_pass(start_mhz, end_mhz, 0).await?;
        info!("scan pass 2 ({start_mhz}..{end_mhz} MHz, odd LOs)");
        let pass_b = self.scan_pass(start_mhz, end_mhz, 1).await?;

        let candidates = intersect_scans(&pass_a, &pass_b);
        info!(
            "scan energy: pass1={} pass2={} intersected={}",
            pass_a.len(),
            pass_b.len(),
            candidates.len()
        );

        // Per-candidate verification: tune precisely, demodulate, and
        // look for the 19 kHz analog stereo pilot tone. Real broadcast
        // FM (in both US and EU) carries a strong pilot regardless of
        // whether it also has HD/DAB digital sidebands. Mirrors,
        // narrowband telemetry, and most data signals do not have a
        // pilot at exactly 19 kHz in the demodulated MPX.
        info!("verifying {} candidates via 19 kHz pilot detection", candidates.len());
        let mut found: Vec<ScannedChannel> = Vec::new();
        for c in candidates {
            match self.verify_pilot(c.freq_hz).await {
                Ok(Some(pilot_snr)) => {
                    tracing::debug!(
                        "verify {} kHz: pilot SNR {:+.1} dB → keep",
                        c.freq_hz / 1000,
                        pilot_snr
                    );
                    found.push(ScannedChannel {
                        power_db: c.power_db,
                        ..c
                    });
                }
                Ok(None) => {
                    tracing::debug!(
                        "verify {} kHz: no pilot → drop",
                        c.freq_hz / 1000
                    );
                }
                Err(e) => {
                    warn!("verify {} kHz failed: {e}", c.freq_hz / 1000);
                }
            }
        }
        found.sort_by_key(|c| c.freq_hz);
        info!("scan: kept {} pilot-verified stations", found.len());
        *self.scanned.lock() = found.clone();

        if restore_agc {
            if let Err(e) = self.rtl.set_tuner_gain_auto(true).await {
                warn!("scan: restoring AGC failed: {e}");
            }
        }
        Ok(found)
    }

    /// Tune to `station_hz`, demodulate ~50 ms of audio MPX, and
    /// FFT-look for a 19 kHz pilot tone that is significantly above
    /// the noise floor at the surrounding 14-18 kHz band. Returns the
    /// pilot's SNR in dB, or None if no pilot is present.
    async fn verify_pilot(&self, station_hz: u32) -> Result<Option<f32>, RtlError> {
        use crate::dsp::fir::{ComplexDecimFir, design_lowpass_kaiser};
        use crate::dsp::fm::FmDemod;
        use crate::dsp::nco::Nco;
        use crate::dsp::u8_iq_to_complex;
        use num_complex::Complex;

        let center_hz = station_hz.saturating_sub(300_000);
        self.rtl.set_center_freq(center_hz).await?;
        let actual_lo = self.rtl.state().center_hz;
        self.set_center(actual_lo);
        tokio::time::sleep(Duration::from_millis(15)).await;

        let bytes = RtlSdr::collect_iq(&self.iq_tx, 360_000).await; // ~75 ms
        let mut iq: Vec<Complex<f32>> = Vec::with_capacity(bytes.len() / 2);
        u8_iq_to_complex(&bytes, &mut iq);

        let offset_hz = station_hz as f32 - actual_lo as f32;
        let mut nco = Nco::new(-offset_hz, 2_400_000.0);
        let mixed: Vec<Complex<f32>> =
            iq.iter().map(|&z| z * nco.step()).collect();

        let taps = design_lowpass_kaiser(120_000.0, 2_400_000.0, 127, 9.0);
        let mut ddc = ComplexDecimFir::new(taps, 10);
        let mut bb: Vec<Complex<f32>> = Vec::with_capacity(mixed.len() / 10);
        ddc.process(&mixed, &mut bb);

        let mut fm = FmDemod::new(240_000.0, 75_000.0);
        let mut mpx: Vec<f32> = Vec::with_capacity(bb.len());
        fm.process(&bb, &mut mpx);

        if mpx.len() < 8192 {
            return Ok(None);
        }
        let n: usize = 8192;
        let win: Vec<f32> = (0..n)
            .map(|i| 0.5 * (1.0 - (2.0 * std::f32::consts::PI * i as f32 / n as f32).cos()))
            .collect();
        let mut buf: Vec<Complex<f32>> = mpx[..n]
            .iter()
            .zip(win.iter())
            .map(|(&s, &w)| Complex::new(s * w, 0.0))
            .collect();
        let mut planner = rustfft::FftPlanner::<f32>::new();
        let fft = planner.plan_fft_forward(n);
        fft.process(&mut buf);

        let bin_hz = 240_000.0_f32 / n as f32;
        let pilot_bin = (19_000.0 / bin_hz).round() as usize;
        let half = (300.0 / bin_hz).ceil() as usize; // ±300 Hz around 19 kHz
        let pilot_lo = pilot_bin.saturating_sub(half);
        let pilot_hi = (pilot_bin + half).min(n / 2) + 1;
        let pilot_pow: f32 = buf[pilot_lo..pilot_hi]
            .iter()
            .map(|c| c.norm_sqr())
            .sum();

        // Reference band: a few hundred Hz around 17.5 kHz, well away
        // from pilot and from the 19 kHz pilot's image bin, and below
        // the L-R subcarrier at 23 kHz.
        let ref_bin = (17_500.0 / bin_hz).round() as usize;
        let ref_lo = ref_bin.saturating_sub(half);
        let ref_hi = (ref_bin + half).min(n / 2) + 1;
        let ref_pow: f32 = buf[ref_lo..ref_hi]
            .iter()
            .map(|c| c.norm_sqr())
            .sum();

        let pilot_snr_db = 10.0 * (pilot_pow / (ref_pow + 1e-18)).log10();
        // ≥10 dB above the ref band reliably means there is a real
        // 19 kHz tone present. Pure noise / digital signals come in
        // around 0–3 dB.
        Ok(if pilot_snr_db >= 10.0 {
            Some(pilot_snr_db)
        } else {
            None
        })
    }

    /// One scan pass at 2 MHz LO step, starting at
    /// `start_mhz + lo_offset_mhz`.
    async fn scan_pass(
        &self,
        start_mhz: u32,
        end_mhz: u32,
        lo_offset_mhz: u32,
    ) -> Result<Vec<ScannedChannel>, RtlError> {
        let mut found: Vec<ScannedChannel> = Vec::new();
        let mut center_mhz = start_mhz + lo_offset_mhz;
        while center_mhz < end_mhz {
            let center_hz = center_mhz * 1_000_000;
            self.rtl.set_center_freq(center_hz).await?;
            self.set_center(self.rtl.state().center_hz);
            tokio::time::sleep(Duration::from_millis(15)).await;
            let bytes = RtlSdr::collect_iq(&self.iq_tx, 600_000).await;
            // Use the *actual* achieved LO, not the request, so the
            // absolute slot grid lines up correctly.
            let actual_lo = self.rtl.state().center_hz;
            let stations = analyse_window(&bytes, actual_lo);
            for s in stations {
                if start_mhz * 1_000_000 <= s.freq_hz
                    && s.freq_hz <= end_mhz * 1_000_000
                    && !found
                        .iter()
                        .any(|f| f.freq_hz.abs_diff(s.freq_hz) < FM_CHANNEL_STEP_HZ / 2)
                {
                    found.push(s);
                }
            }
            center_mhz += 2;
        }
        Ok(found)
    }

    /// Pick a tuner center that captures as many of the strong scanned
    /// stations as possible inside a single 2 MHz usable window.
    pub fn pick_default_window(&self, scanned: &[ScannedChannel]) -> Option<u32> {
        if scanned.is_empty() {
            return None;
        }
        // Greedy: find the window that contains the most stations.
        let mut best_count = 0usize;
        let mut best_center = scanned[0].freq_hz;
        for cand in scanned {
            let cnt = scanned
                .iter()
                .filter(|s| s.freq_hz.abs_diff(cand.freq_hz) <= USABLE_HALF_BW_HZ)
                .count();
            if cnt > best_count {
                best_count = cnt;
                best_center = cand.freq_hz;
            }
        }
        Some(best_center)
    }
}

/// Intersect two scan passes done with LOs offset by 1 MHz.
/// A real station has a fixed RF frequency, so it appears in both
/// passes (within slot tolerance). A mirror's RF frequency depends on
/// the LO, so it appears in only one pass and is dropped.
fn intersect_scans(a: &[ScannedChannel], b: &[ScannedChannel]) -> Vec<ScannedChannel> {
    const TOL_HZ: u32 = 60_000;
    let mut out: Vec<ScannedChannel> = Vec::new();
    for ca in a {
        if let Some(cb) = b.iter().find(|cb| cb.freq_hz.abs_diff(ca.freq_hz) <= TOL_HZ) {
            let avg_freq = ((ca.freq_hz as u64 + cb.freq_hz as u64) / 2) as u32;
            let snapped = ((avg_freq + 500) / 1_000) * 1_000;
            out.push(ScannedChannel {
                name: format!("FM {:.1}", snapped as f64 / 1e6),
                freq_hz: snapped,
                power_db: ca.power_db.max(cb.power_db),
            });
        }
    }
    out.sort_by_key(|c| c.freq_hz);
    out.dedup_by(|a, b| a.freq_hz.abs_diff(b.freq_hz) < TOL_HZ);
    out
}

/// Channelized energy detector tuned to FM broadcast signatures.
///
/// 1. Long integration: 16 averaged 16 k-point FFTs over ~110 ms of IQ.
///    Bin resolution ≈ 146 Hz, slot resolution = 100 kHz (~684 bins).
/// 2. Noise floor = 10th-percentile of all slots (not median): a band
///    full of real stations would otherwise raise the median and hide
///    the weaker ones.
/// 3. A slot only counts as a station when:
///      * it is ≥ `STATION_MIN_SNR_DB` above the noise floor,
///      * it is a local maximum vs. its two neighbours,
///      * at least one neighbour is also `NEIGHBOUR_MIN_SNR_DB` above
///        the floor (real FM occupies ≥ ±100 kHz; pure noise spikes
///        die off in one bin).
/// 4. Stations are at least 300 kHz apart (broadcast planning + a
///    safety guard).
fn analyse_window(bytes: &[u8], center_hz: u32) -> Vec<ScannedChannel> {
    const FFT_SIZE: usize = 16384;
    const NUM_AVGS: usize = 16;
    // In a dense FM market (San Antonio, Dublin, London, ...) more than
    // half of the 100 kHz slots in any 2 MHz window contain a real
    // station. The 10th-percentile noise floor then sits *on* a weak
    // station's skirt, hiding everything weaker. We use the mean of
    // the bottom 25% of slots — that bottom quartile is broadcast guard
    // band almost everywhere on Earth, so it tracks the true noise.
    const NOISE_FRACTION: f32 = 0.25;
    const STATION_MIN_SNR_DB: f32 = 8.0;
    const NEIGHBOUR_MIN_SNR_DB: f32 = 4.0;
    const MIN_SPACING_HZ: u32 = 300_000;

    let need = FFT_SIZE * 2 * NUM_AVGS;
    if bytes.len() < need {
        return Vec::new();
    }

    let mut planner = FftPlanner::<f32>::new();
    let fft = planner.plan_fft_forward(FFT_SIZE);
    let mut psd = vec![0.0f32; FFT_SIZE];
    let mut iq: Vec<Complex<f32>> = Vec::with_capacity(FFT_SIZE);
    let mut buf: Vec<Complex<f32>> = vec![Complex::new(0.0, 0.0); FFT_SIZE];

    for k in 0..NUM_AVGS {
        let off = k * FFT_SIZE * 2;
        u8_iq_to_complex(&bytes[off..off + FFT_SIZE * 2], &mut iq);
        for (i, z) in iq.iter_mut().enumerate() {
            let w = 0.5 * (1.0 - (2.0 * std::f32::consts::PI * i as f32 / FFT_SIZE as f32).cos());
            z.re *= w;
            z.im *= w;
        }
        buf.copy_from_slice(&iq);
        fft.process(&mut buf);
        for (i, p) in psd.iter_mut().enumerate() {
            let shifted = (i + FFT_SIZE / 2) % FFT_SIZE;
            let c = buf[shifted];
            *p += c.re * c.re + c.im * c.im;
        }
    }
    for v in psd.iter_mut() {
        *v = 10.0 * (1e-18_f32 + *v / NUM_AVGS as f32).log10();
    }

    let bin_hz = DEFAULT_SAMPLE_RATE as f32 / FFT_SIZE as f32;
    let bins_per_slot = (FM_CHANNEL_STEP_HZ as f32 / bin_hz).round() as usize;
    let center_bin = FFT_SIZE / 2;

    // Use an ABSOLUTE 100 kHz channel grid anchored to integer
    // multiples of FM_CHANNEL_STEP_HZ. This way every window-LO sees
    // the same channel boundaries, so a station at 89.1 MHz lives in
    // the slot CENTERED on 89.1 MHz regardless of where the tuner is.
    let win_lo_hz = center_hz.saturating_sub(USABLE_HALF_BW_HZ);
    let win_hi_hz = center_hz + USABLE_HALF_BW_HZ;
    let first_k = win_lo_hz.div_ceil(FM_CHANNEL_STEP_HZ);
    let last_k = win_hi_hz / FM_CHANNEL_STEP_HZ;

    let mut slot_pow_lin: Vec<f32> = Vec::new();
    let mut slot_center_hz: Vec<u32> = Vec::new();
    let mut slot_peak_hz: Vec<u32> = Vec::new();

    for k in first_k..=last_k {
        let f_hz = k * FM_CHANNEL_STEP_HZ;
        let offset_hz = f_hz as i64 - center_hz as i64;
        if offset_hz.unsigned_abs() < DC_EXCLUSION_HZ as u64 {
            continue;
        }
        let mid_bin = center_bin as isize + (offset_hz as f32 / bin_hz).round() as isize;
        let bin_lo = mid_bin - (bins_per_slot as isize) / 2;
        let bin_hi = bin_lo + bins_per_slot as isize;

        let mut acc = 0.0f32;
        let mut peak_lin = f32::NEG_INFINITY;
        let mut peak_bin = mid_bin;
        for b in bin_lo.max(0)..bin_hi.min(FFT_SIZE as isize) {
            let p_lin = 10f32.powf(psd[b as usize] / 10.0);
            acc += p_lin;
            if psd[b as usize] > peak_lin {
                peak_lin = psd[b as usize];
                peak_bin = b;
            }
        }
        let peak_offset_hz = (peak_bin - center_bin as isize) as f32 * bin_hz;
        let peak_freq_hz = (center_hz as i64 + peak_offset_hz.round() as i64).max(0) as u32;

        slot_pow_lin.push(acc);
        slot_center_hz.push(f_hz);
        slot_peak_hz.push(peak_freq_hz);
    }
    if slot_pow_lin.is_empty() {
        return Vec::new();
    }

    let slot_db: Vec<f32> = slot_pow_lin
        .iter()
        .map(|p| 10.0 * (1e-18 + p).log10())
        .collect();
    let mut sorted = slot_db.clone();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    // Mean of bottom NOISE_FRACTION of slots = noise floor.
    let n_noise = ((sorted.len() as f32 * NOISE_FRACTION).round() as usize).max(1);
    let noise_floor = sorted[..n_noise].iter().sum::<f32>() / n_noise as f32;
    let peak = sorted[sorted.len() - 1];
    tracing::debug!(
        "window {} kHz: noise floor {:.1} dB (bottom {n_noise}/{}), peak {:.1} dB, gap {:.1} dB",
        center_hz / 1000,
        sorted.len(),
        noise_floor,
        peak,
        peak - noise_floor
    );

    let mut idx: Vec<usize> = (0..slot_db.len()).collect();
    idx.sort_by_key(|&i| slot_center_hz[i]);

    let mut stations: Vec<ScannedChannel> = Vec::new();
    let mut last_kept_hz: Option<u32> = None;
    for i in idx {
        let p = slot_db[i];
        let snr = p - noise_floor;
        if snr < STATION_MIN_SNR_DB {
            continue;
        }
        let prev = if i == 0 { f32::NEG_INFINITY } else { slot_db[i - 1] };
        let next = if i + 1 >= slot_db.len() {
            f32::NEG_INFINITY
        } else {
            slot_db[i + 1]
        };
        // Local-max check.
        if p < prev || p < next {
            continue;
        }
        // FM-bandwidth signature: at least one neighbour must also be
        // well above the noise floor.
        let prev_snr = prev - noise_floor;
        let next_snr = next - noise_floor;
        if prev_snr < NEIGHBOUR_MIN_SNR_DB && next_snr < NEIGHBOUR_MIN_SNR_DB {
            continue;
        }
        // Mirror rejection is handled at the scan_band level via a
        // dual-LO pass — comparing slot powers within a single LO
        // capture can't tell a real station from its image when the
        // surrounding band also has real stations.

        // Report the slot's grid frequency (the FM channel center),
        // not the FFT peak bin — peak bins shift by tens of kHz between
        // passes due to FM modulation, which breaks dual-LO matching.
        // We use the slot center; the demodulator will tune to that
        // exact frequency and the IF offset takes care of small lock
        // errors.
        let carrier_hz = slot_center_hz[i];
        let _ = &slot_peak_hz;
        if let Some(lk) = last_kept_hz {
            if carrier_hz.abs_diff(lk) < MIN_SPACING_HZ {
                if let Some(last) = stations.last_mut() {
                    if snr > last.power_db {
                        last.freq_hz = carrier_hz;
                        last.power_db = snr;
                        last.name = format!("FM {:.1}", carrier_hz as f64 / 1e6);
                        last_kept_hz = Some(carrier_hz);
                    }
                }
                continue;
            }
        }
        stations.push(ScannedChannel {
            name: format!("FM {:.1}", carrier_hz as f64 / 1e6),
            freq_hz: carrier_hz,
            power_db: snr,
        });
        last_kept_hz = Some(carrier_hz);
    }
    if stations.is_empty() {
        warn!(
            "no stations in window {} kHz (noise floor {:.1} dB, needed +{:.0} dB)",
            center_hz / 1000,
            noise_floor,
            STATION_MIN_SNR_DB
        );
    }
    stations
}
