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

use axum::Json;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::IntoResponse;
use axum::response::sse::{Event, KeepAlive, Sse};
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
    pub metadata: Option<RdsMetadataDto>,
}

#[derive(Debug, Serialize, Clone)]
pub struct StationsResponse {
    pub scan_band_mhz: (u32, u32),
    pub center_hz: u32,
    pub window_lo_hz: u32,
    pub window_hi_hz: u32,
    pub idle_refresher_freq_hz: Option<u32>,
    pub stations: Vec<StationInfo>,
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
    let stations: Vec<StationInfo> = scanned
        .into_iter()
        .map(|c| StationInfo {
            freq_hz: c.freq_hz,
            name: c.name,
            scan_power_db: c.power_db,
            listeners: listeners.get(&c.freq_hz).copied().unwrap_or(0),
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
