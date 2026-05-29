//! GET /stream/{khz}.flac and /stream/{khz}.wav.

use std::convert::Infallible;

use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use bytes::Bytes;
use futures_util::Stream;
use tokio::sync::broadcast;
use tracing::warn;

use crate::api::AppState;
use crate::encode::flac::FlacEncoder;
use crate::encode::wav;

fn parse_freq_hz(khz_str: &str) -> Option<u32> {
    // Accept "104.0", "104000", etc.
    if let Ok(f) = khz_str.parse::<f64>() {
        // Heuristic: if looks like MHz (e.g. < 200) treat as MHz; else kHz.
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

async fn open_pcm_stream(
    state: &AppState,
    freq_hz: u32,
) -> Result<broadcast::Receiver<crate::encode::PcmBlock>, Response> {
    state
        .channelizer
        .tune(freq_hz)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response())
}

/// Single handler for `/stream/{khz}.flac` and `/stream/{khz}.wav`.
/// axum 0.7 doesn't let two routes share a path-param prefix that
/// differ only by literal suffix, so we route on `:file` and split
/// the extension ourselves.
pub async fn audio(
    Path(file): Path<String>,
    State(state): State<AppState>,
) -> Response {
    let Some(dot) = file.rfind('.') else {
        return (StatusCode::BAD_REQUEST, "missing extension; use .flac or .wav").into_response();
    };
    let (stem, ext) = (&file[..dot], &file[dot + 1..]);
    let Some(freq_hz) = parse_freq_hz(stem) else {
        return (StatusCode::BAD_REQUEST, "invalid frequency").into_response();
    };
    let rx = match open_pcm_stream(&state, freq_hz).await {
        Ok(r) => r,
        Err(resp) => return resp,
    };
    match ext {
        "flac" => flac_response(rx),
        "wav" => wav_response(rx),
        _ => (StatusCode::BAD_REQUEST, "unknown extension; use .flac or .wav").into_response(),
    }
}

fn flac_response(rx: broadcast::Receiver<crate::encode::PcmBlock>) -> Response {
    let body = Body::from_stream(pcm_to_flac_stream(rx));
    let mut headers = HeaderMap::new();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static("audio/flac"));
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    (StatusCode::OK, headers, body).into_response()
}

fn wav_response(rx: broadcast::Receiver<crate::encode::PcmBlock>) -> Response {
    let header = wav::header();
    let stream = async_stream::stream! {
        yield Ok::<Bytes, Infallible>(Bytes::from(header));
        let mut rx = rx;
        loop {
            match rx.recv().await {
                Ok(block) => {
                    let bytes = wav::encode_block(&block);
                    yield Ok(Bytes::from(bytes));
                }
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    warn!("WAV listener lagged {n} blocks");
                    continue;
                }
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    };
    let body = Body::from_stream(stream);
    let mut headers = HeaderMap::new();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static("audio/wav"));
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    (StatusCode::OK, headers, body).into_response()
}

fn pcm_to_flac_stream(
    rx: broadcast::Receiver<crate::encode::PcmBlock>,
) -> impl Stream<Item = Result<Bytes, std::io::Error>> {
    let mut rx = rx;
    let mut encoder = FlacEncoder::new();
    async_stream::stream! {
        loop {
            match rx.recv().await {
                Ok(block) => {
                    match encoder.encode_block(&block) {
                        Ok(bytes) => yield Ok::<Bytes, std::io::Error>(Bytes::from(bytes)),
                        Err(e) => {
                            warn!("flac encode error: {e:#}");
                            break;
                        }
                    }
                }
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    warn!("FLAC listener lagged {n} blocks");
                    continue;
                }
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    }
}
