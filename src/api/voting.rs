//! Vote endpoints.
//!
//! * `POST /api/vote`   — heartbeat + cast. Body: `{ fingerprint, freq_hz }`.
//!   Every active client sends this on a short interval; the vote
//!   expires automatically a few seconds after the last heartbeat.
//! * `POST /api/unvote` — explicit drop. Body: `{ fingerprint }`.
//! * `GET  /api/vote/state` — current winner + tallies + active count.

use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;
use serde::Deserialize;

use crate::api::AppState;
use crate::voting::VoteSnapshot;

#[derive(Debug, Deserialize)]
pub struct VoteRequest {
    pub fingerprint: String,
    pub freq_hz: u32,
}

#[derive(Debug, Deserialize)]
pub struct UnvoteRequest {
    pub fingerprint: String,
}

pub async fn cast(
    State(state): State<AppState>,
    Json(req): Json<VoteRequest>,
) -> Result<StatusCode, (StatusCode, String)> {
    if req.fingerprint.is_empty() {
        return Err((StatusCode::BAD_REQUEST, "empty fingerprint".to_string()));
    }
    if req.fingerprint.len() > 256 {
        return Err((StatusCode::BAD_REQUEST, "fingerprint too long".to_string()));
    }
    // Sanity check the freq against the scanned-station list so a
    // malformed client can't poison the registry with junk votes that
    // would never win anyway but would clutter the tallies.
    let scanned = state.channelizer.scanned();
    if !scanned.iter().any(|s| s.freq_hz == req.freq_hz) {
        return Err((
            StatusCode::BAD_REQUEST,
            format!("{} Hz is not a scanned station", req.freq_hz),
        ));
    }
    state.votes.cast(req.fingerprint, req.freq_hz);
    Ok(StatusCode::NO_CONTENT)
}

pub async fn unvote(State(state): State<AppState>, Json(req): Json<UnvoteRequest>) -> StatusCode {
    state.votes.clear(&req.fingerprint);
    StatusCode::NO_CONTENT
}

pub async fn state_snapshot(State(state): State<AppState>) -> Json<VoteSnapshot> {
    Json(state.votes.snapshot())
}
