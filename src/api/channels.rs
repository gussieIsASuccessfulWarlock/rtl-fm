//! GET /api/channels, /api/state, /api/rescan.

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use serde::Serialize;
use tracing::info;

use crate::api::AppState;
use crate::channelizer::ScannedChannel;

#[derive(Debug, Serialize)]
pub struct StateResponse {
    pub device: String,
    pub center_hz: u32,
    pub window_lo_hz: u32,
    pub window_hi_hz: u32,
    pub sample_rate: u32,
    pub active_listeners: Vec<ActiveStation>,
    pub scan_band_mhz: (u32, u32),
}

#[derive(Debug, Serialize)]
pub struct ActiveStation {
    pub freq_hz: u32,
    pub listeners: usize,
}

pub async fn list(State(state): State<AppState>) -> Json<Vec<ScannedChannel>> {
    Json(state.channelizer.scanned())
}

pub async fn state(State(state): State<AppState>) -> Json<StateResponse> {
    let s = state.rtl.state();
    let (lo, hi) = state.channelizer.window();
    let active = state
        .channelizer
        .active_listeners()
        .into_iter()
        .map(|(freq_hz, listeners)| ActiveStation { freq_hz, listeners })
        .collect();
    Json(StateResponse {
        device: state.rtl.describe().to_string(),
        center_hz: s.center_hz,
        window_lo_hz: lo,
        window_hi_hz: hi,
        sample_rate: s.sample_rate,
        active_listeners: active,
        scan_band_mhz: state.scan_band_mhz,
    })
}

pub async fn rescan(
    State(state): State<AppState>,
) -> Result<Json<Vec<ScannedChannel>>, (StatusCode, String)> {
    let n = state
        .channelizer
        .active_listeners()
        .values()
        .copied()
        .sum::<usize>();
    if n > 0 {
        info!("rescan requested while {n} listener(s) active; dropping them");
    } else {
        info!("rescan requested");
    }
    // Scanning hops the tuner across the band, so any in-flight
    // channel tasks would produce garbled audio. Drop them cleanly
    // before we start.
    state.channelizer.drop_all_channels();
    let (lo, hi) = state.scan_band_mhz;
    state
        .channelizer
        .scan_band(lo, hi, |_| {})
        .await
        .map(Json)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}
