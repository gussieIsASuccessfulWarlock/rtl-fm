//! Metadata endpoints.
//!
//! * `GET /api/metadata`             — all cached snapshots (single shot).
//! * `GET /api/metadata/{khz}`       — snapshot for one frequency.
//! * `GET /api/metadata/{khz}/stream` — Server-Sent-Events stream of
//!   live metadata updates for one frequency.
//! * `GET /api/stations`             — scanned-channel list joined
//!   with their current metadata + active-listener counts.
//! * `GET /api/stations/stream`      — SSE stream of the above,
//!   pushing whenever any station's metadata or active listener
//!   count changes.

use std::collections::HashMap;
use std::convert::Infallible;
use std::time::Duration;

use axum::extract::{Path, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::IntoResponse;
use axum::Json;
use futures_util::Stream;
use serde::Serialize;

use crate::api::AppState;
use crate::channelizer::{RdsMetadataDto, ScannedChannel};

#[derive(Debug, Serialize)]
pub struct AllMetadataResponse {
    pub stations: HashMap<u32, RdsMetadataDto>,
}

#[derive(Debug, Serialize, Clone)]
pub struct StationInfo {
    pub freq_hz: u32,
    pub name: String,
    pub scan_power_db: f32,
    pub listeners: usize,
    pub votes: usize,
    pub metadata: Option<RdsMetadataDto>,
}

#[derive(Debug, Serialize, Clone)]
pub struct StationsResponse {
    pub scan_band_mhz: (u32, u32),
    pub center_hz: u32,
    pub window_lo_hz: u32,
    pub window_hi_hz: u32,
    pub idle_refresher_freq_hz: Option<u32>,
    pub winner_hz: Option<u32>,
    pub active_voters: usize,
    pub stations: Vec<StationInfo>,
}

/// Site/operator metadata for `GET /meta`.
#[derive(Debug, Serialize)]
pub struct MetaResponse {
    /// Software version (Cargo package version).
    pub version: &'static str,
    pub owner: String,
    pub location: String,
    /// Number of scanned channels in the band.
    pub channels: usize,
    /// Mean per-station signal quality in dB (live decode SNR where
    /// available, otherwise the scan power), rounded to 0.1 dB.
    pub average_quality_db: f32,
    /// Distinct listeners (voted or streamed) in the last 24 hours.
    pub daily_listeners: usize,
    /// Anonymous public sessions seen in the last hour.
    pub active_sessions: usize,
}

pub async fn meta(State(state): State<AppState>) -> Json<MetaResponse> {
    let snap = build_stations(&state);
    let channels = snap.stations.len();
    let average_quality_db = if channels == 0 {
        0.0
    } else {
        let sum: f32 = snap
            .stations
            .iter()
            .map(|s| {
                s.metadata
                    .as_ref()
                    .and_then(|m| m.signal_snr_db)
                    .unwrap_or(s.scan_power_db)
            })
            .sum();
        ((sum / channels as f32) * 10.0).round() / 10.0
    };
    Json(MetaResponse {
        version: env!("CARGO_PKG_VERSION"),
        owner: state.owner.clone(),
        location: state.location.clone(),
        channels,
        average_quality_db,
        daily_listeners: state.votes.daily_listeners(),
        active_sessions: state.sessions.active(Duration::from_secs(3600)),
    })
}

pub async fn all(State(state): State<AppState>) -> Json<AllMetadataResponse> {
    Json(AllMetadataResponse {
        stations: state.channelizer.all_metadata_snapshots(),
    })
}

pub async fn one(
    Path(khz): Path<String>,
    State(state): State<AppState>,
) -> Result<Json<RdsMetadataDto>, (StatusCode, String)> {
    let Some(freq_hz) = parse_freq_hz(&khz) else {
        return Err((StatusCode::BAD_REQUEST, "invalid frequency".to_string()));
    };
    match state.channelizer.metadata_snapshot(freq_hz) {
        Some(m) => Ok(Json(m)),
        None => Err((
            StatusCode::NOT_FOUND,
            format!(
                "no metadata cached for {} kHz — open a /stream first so the channel task can decode RDS",
                freq_hz / 1000
            ),
        )),
    }
}

pub async fn hd_scan(
    Path(khz): Path<String>,
    State(state): State<AppState>,
) -> Result<Json<RdsMetadataDto>, (StatusCode, String)> {
    let Some(freq_hz) = parse_freq_hz(&khz) else {
        return Err((StatusCode::BAD_REQUEST, "invalid frequency".to_string()));
    };

    // Same-station HD-while-audio is allowed — the channel task already
    // dual-spawned an NRSC-5 decoder at 2.4 MS/s.  Cross-station is
    // rejected because honouring it would force the SDR off the audio
    // station and break the live stream.
    let listeners = state.channelizer.active_listeners();
    let here = listeners.get(&freq_hz).copied().unwrap_or(0);
    let elsewhere: usize = listeners
        .iter()
        .filter(|(&f, _)| f != freq_hz)
        .map(|(_, &n)| n)
        .sum();
    if here == 0 && elsewhere > 0 {
        return Err((
            StatusCode::CONFLICT,
            "another station is currently playing; stop it before scanning HD here".to_string(),
        ));
    }

    state
        .channelizer
        .refresh_hd_metadata(freq_hz, Duration::from_secs(75))
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    match state.channelizer.metadata_snapshot(freq_hz) {
        Some(m) => Ok(Json(m)),
        None => Err((StatusCode::NOT_FOUND, "no metadata cached yet".to_string())),
    }
}

/// SSE stream of RDS metadata updates for a single station.
/// One event per second; the client receives the full DTO each time.
pub async fn one_sse(
    Path(khz): Path<String>,
    State(state): State<AppState>,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, (StatusCode, String)> {
    let Some(freq_hz) = parse_freq_hz(&khz) else {
        return Err((StatusCode::BAD_REQUEST, "invalid frequency".to_string()));
    };
    let channelizer = state.channelizer.clone();
    let s = async_stream::stream! {
        let mut interval = tokio::time::interval(Duration::from_millis(1000));
        loop {
            interval.tick().await;
            let snap = channelizer.metadata_snapshot(freq_hz);
            let payload = serde_json::to_string(&snap).unwrap_or_else(|_| "null".to_string());
            yield Ok::<_, Infallible>(Event::default().data(payload));
        }
    };
    Ok(Sse::new(s).keep_alive(KeepAlive::default()))
}

pub async fn album_art(
    Path(khz): Path<String>,
    State(state): State<AppState>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    let Some(freq_hz) = parse_freq_hz(&khz) else {
        return Err((StatusCode::BAD_REQUEST, "invalid frequency".to_string()));
    };
    let Some(art) = state.channelizer.album_art(freq_hz) else {
        return Err((StatusCode::NOT_FOUND, "no HD album art cached".to_string()));
    };
    let mut headers = HeaderMap::new();
    let mime = art.mime.as_deref().unwrap_or("application/octet-stream");
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_str(mime).unwrap_or(HeaderValue::from_static("application/octet-stream")),
    );
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok((headers, art.bytes))
}

/// All-stations snapshot used by the home page when nothing is
/// actively playing.
pub async fn stations(State(state): State<AppState>) -> Json<StationsResponse> {
    Json(build_stations(&state))
}

/// SSE stream of the all-stations snapshot. Updates once a second.
pub async fn stations_sse(
    State(state): State<AppState>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let s = async_stream::stream! {
        let mut interval = tokio::time::interval(Duration::from_millis(1000));
        loop {
            interval.tick().await;
            let snap = build_stations(&state);
            let payload = serde_json::to_string(&snap).unwrap_or_else(|_| "{}".to_string());
            yield Ok::<_, Infallible>(Event::default().data(payload));
        }
    };
    Sse::new(s).keep_alive(KeepAlive::default())
}

fn build_stations(state: &AppState) -> StationsResponse {
    let scanned: Vec<ScannedChannel> = state.channelizer.scanned();
    let metas = state.channelizer.all_metadata_snapshots();
    let listeners = state.channelizer.active_listeners();
    let vote_snap = state.votes.snapshot();
    let vote_counts: std::collections::HashMap<u32, usize> = vote_snap
        .tallies
        .iter()
        .map(|t| (t.freq_hz, t.votes))
        .collect();
    let stations: Vec<StationInfo> = scanned
        .into_iter()
        .map(|c| StationInfo {
            freq_hz: c.freq_hz,
            name: c.name,
            scan_power_db: c.power_db,
            listeners: listeners.get(&c.freq_hz).copied().unwrap_or(0),
            votes: vote_counts.get(&c.freq_hz).copied().unwrap_or(0),
            metadata: metas.get(&c.freq_hz).cloned(),
        })
        .collect();
    let rtl_state = state.rtl.state();
    let (lo, hi) = state.channelizer.window();
    StationsResponse {
        scan_band_mhz: state.scan_band_mhz,
        center_hz: rtl_state.center_hz,
        window_lo_hz: lo,
        window_hi_hz: hi,
        idle_refresher_freq_hz: state.channelizer.idle_refresher_freq(),
        winner_hz: vote_snap.winner_hz,
        active_voters: vote_snap.active_voters,
        stations,
    }
}

fn parse_freq_hz(s: &str) -> Option<u32> {
    if let Ok(f) = s.parse::<f64>() {
        let hz = if f < 1_000.0 {
            (f * 1_000_000.0) as u32
        } else {
            (f * 1_000.0) as u32
        };
        Some(hz)
    } else {
        None
    }
}
