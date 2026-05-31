//! Axum HTTP layer.

pub mod auth;
pub mod channels;
pub mod metadata;
pub mod static_page;
pub mod stream;
pub mod voting;

use std::sync::Arc;

use axum::routing::{get, post};
use axum::Router;

use crate::api::auth::SessionRegistry;
use crate::channelizer::Channelizer;
use crate::rtlsdr::RtlSdr;
use crate::voting::VoteRegistry;

#[derive(Clone)]
pub struct AppState {
    pub rtl: Arc<RtlSdr>,
    pub channelizer: Arc<Channelizer>,
    pub votes: Arc<VoteRegistry>,
    pub scan_band_mhz: (u32, u32),
    /// Operator-supplied site identity, surfaced under the brand and at `/meta`.
    pub owner: String,
    pub location: String,
    /// Random admin token minted at startup; presented via `?token=` then
    /// stored as a cookie. Gates the admin-only routes.
    pub admin_token: Arc<str>,
    /// Live registry of anonymous public sessions.
    pub sessions: Arc<SessionRegistry>,
}

pub fn router(state: AppState) -> Router {
    // Admin-only: rescan + the full debug/metadata API + arbitrary-station
    // audio. Gated by the admin token (cookie or ?token=).
    let admin = Router::new()
        .route("/api/rescan", get(channels::rescan))
        .route("/api/channels", get(channels::list))
        .route("/api/state", get(channels::state))
        .route("/api/metadata", get(metadata::all))
        .route("/api/metadata/:khz", get(metadata::one))
        .route("/api/metadata/:khz/hdscan", get(metadata::hd_scan))
        .route("/api/metadata/:khz/stream", get(metadata::one_sse))
        .route("/api/albumart/:khz", get(metadata::album_art))
        .route("/stream/:file", get(stream::audio))
        .route_layer(axum::middleware::from_fn_with_state(
            state.clone(),
            auth::require_admin,
        ));

    // Public (anonymous): the page, site meta, voting, the info SSE/snapshot
    // feeds, and the voted-winner audio stream. Nothing else.
    let public = Router::new()
        .route("/", get(static_page::index))
        .route("/meta", get(metadata::meta))
        .route("/api/whoami", get(auth::whoami))
        .route("/api/stations", get(metadata::stations))
        .route("/api/stations/stream", get(metadata::stations_sse))
        .route("/api/vote", post(voting::cast))
        .route("/api/unvote", post(voting::unvote))
        .route("/api/vote/state", get(voting::state_snapshot))
        .route("/stream.aac", get(stream::current_aac))
        .route("/stream.wav", get(stream::current_wav));

    public
        .merge(admin)
        // Account for every anonymous session on every request.
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            auth::track_session,
        ))
        .with_state(state)
}
