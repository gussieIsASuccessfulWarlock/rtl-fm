//! Top-level RTL-SDR handle. Wraps the open USB interface, the RTL2832U
//! demodulator and the R820T2 tuner behind an async API.

pub mod i2c;
pub mod r820t2;
pub mod rtl2832;

use std::sync::Arc;

use nusb::{Device, Interface};
use parking_lot::Mutex;
use tokio::sync::broadcast;
use tracing::{info, warn};

use crate::error::RtlError;
use crate::usb;
use crate::usb::transfer::IqChunk;

pub const DEFAULT_SAMPLE_RATE: u32 = 2_400_000;

#[derive(Debug, Clone, Copy)]
pub struct TunerState {
    pub center_hz: u32,
    pub sample_rate: u32,
    pub gain_tenth_db: Option<i32>, // None = AGC
}

pub struct RtlSdr {
    _device: Device,
    iface: Interface,
    tuner: r820t2::R820T2,
    state: Mutex<TunerState>,
    descr: String,
}

impl RtlSdr {
    /// Open the first connected RTL2838 and bring the demod+tuner up.
    pub async fn open() -> Result<Self, RtlError> {
        let (device, iface, descr) = usb::open()?;

        // Bring the demod up.
        rtl2832::init_baseband(&iface).await?;

        // Hand the interface to the tuner.
        let tuner = r820t2::R820T2::new(iface.clone());

        // Probe & init the tuner. We tolerate a soft ID-check failure
        // for clones that don't match the canonical 0x96 chipid.
        match tuner.check_id().await {
            Ok(id) => info!("R820T2 chip id (bit-reversed) = {id:#x}"),
            Err(e) => warn!("R820T2 ID read failed (continuing): {e}"),
        }
        tuner.init().await?;

        // R820T2-specific demod config: tell the RTL2832U what IF the
        // tuner emits (3.57 MHz) and that the spectrum is inverted.
        // Without these the IQ pump delivers a signal that's aliased
        // off-center and our DSP demodulates noise.
        rtl2832::set_if_freq(&iface, r820t2::R820T_IF_FREQ_HZ).await?;
        rtl2832::set_spectrum_inversion(&iface, true).await?;

        let state = TunerState {
            center_hz: 100_000_000,
            sample_rate: DEFAULT_SAMPLE_RATE,
            gain_tenth_db: None,
        };

        Ok(Self {
            _device: device,
            iface,
            tuner,
            state: Mutex::new(state),
            descr,
        })
    }

    pub fn describe(&self) -> &str {
        &self.descr
    }

    pub fn state(&self) -> TunerState {
        *self.state.lock()
    }

    pub async fn set_sample_rate(&self, hz: u32) -> Result<(), RtlError> {
        rtl2832::set_sample_rate(&self.iface, hz).await?;
        self.state.lock().sample_rate = hz;
        Ok(())
    }

    pub async fn set_center_freq(&self, hz: u32) -> Result<(), RtlError> {
        let actual = self.tuner.set_freq(hz).await?;
        self.state.lock().center_hz = actual;
        Ok(())
    }

    pub async fn set_tuner_gain_auto(&self, enable: bool) -> Result<(), RtlError> {
        self.tuner.set_agc(enable).await?;
        if enable {
            self.state.lock().gain_tenth_db = None;
        }
        Ok(())
    }

    pub async fn set_tuner_gain(&self, tenth_db: i32) -> Result<i32, RtlError> {
        self.tuner.set_agc(false).await?;
        let chosen = self.tuner.set_gain(tenth_db).await?;
        self.state.lock().gain_tenth_db = Some(chosen);
        Ok(chosen)
    }

    /// Run the bulk-IN pump forever, broadcasting IQ chunks.
    ///
    /// Resets the endpoint FIFO before starting so the first chunk is
    /// aligned to an I-then-Q boundary.
    pub async fn run_iq_pump(self: Arc<Self>, tx: broadcast::Sender<IqChunk>) -> Result<(), RtlError> {
        rtl2832::reset_buffer(&self.iface).await?;
        usb::transfer::run_bulk_pump(self.iface.clone(), tx).await
    }

    /// Snapshot N raw bytes for the energy scanner (used at startup).
    /// Returns when at least `min_bytes` have been collected from a fresh
    /// subscription to `iq_tx`.
    pub async fn collect_iq(
        iq_tx: &broadcast::Sender<IqChunk>,
        min_bytes: usize,
    ) -> Vec<u8> {
        let mut rx = iq_tx.subscribe();
        let mut out: Vec<u8> = Vec::with_capacity(min_bytes + 16_384);
        while out.len() < min_bytes {
            match rx.recv().await {
                Ok(chunk) => out.extend_from_slice(&chunk),
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
        out
    }
}
