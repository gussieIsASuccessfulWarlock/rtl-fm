//! Axum HTTP layer.

pub mod channels;
pub mod metadata;
pub mod static_page;
pub mod stream;

use std::sync::Arc;

use axum::Router;
use axum::routing::get;

use crate::channelizer::Channelizer;
use crate::rtlsdr::RtlSdr;

#[derive(Clone)]
pub struct AppState {
    pub rtl: Arc<RtlSdr>,
    pub channelizer: Arc<Channelizer>,
    pub scan_band_mhz: (u32, u32),
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/", get(static_page::index))
        .route("/api/channels", get(channels::list))
        .route("/api/state", get(channels::state))
        .route("/api/rescan", get(channels::rescan))
        .route("/api/metadata", get(metadata::all))
        .route("/api/metadata/:khz", get(metadata::one))
        .route("/api/metadata/:khz/stream", get(metadata::one_sse))
        .route("/api/albumart/:khz", get(metadata::album_art))
        .route("/api/stations", get(metadata::stations))
        .route("/api/stations/stream", get(metadata::stations_sse))
        .route("/stream/:file", get(stream::audio))
        .with_state(state)
}
