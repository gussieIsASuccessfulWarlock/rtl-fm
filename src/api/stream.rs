//! GET /stream/{khz}.aac and /stream/{khz}.wav.

use std::collections::HashMap;
use std::convert::Infallible;

use axum::body::Body;
use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use bytes::Bytes;
use futures_util::Stream;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::Command;
use tokio::sync::broadcast;
use tracing::warn;

use crate::api::AppState;
use crate::encode::wav;

const PCM_BLOCK_SECONDS: f32 =
    crate::encode::flac::BLOCK_SIZE as f32 / crate::encode::PCM_SAMPLE_RATE as f32;
const PCM_SAMPLE_RATE: u32 = crate::encode::PCM_SAMPLE_RATE;
const HIGH_SAMPLE_RATE: u32 = 192_000;
const AAC_KBPS: u32 = 320;

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

#[derive(Debug, Clone, Copy)]
enum AudioEncoding {
    Aac { kbps: u32 },
    Wav { bits: u16, sample_rate: u32 },
}

#[derive(Debug, Clone, Copy)]
struct StreamOptions {
    encoding: AudioEncoding,
    profile: &'static str,
    prebuffer_blocks: usize,
    estimated_kbps: u32,
}

impl StreamOptions {
    fn for_request(ext: &str, params: &HashMap<String, String>) -> Option<Self> {
        let profile = params
            .get("profile")
            .map(|s| s.as_str())
            .unwrap_or(match ext {
                "aac" => "low",
                "wav" => "medium",
                _ => "low",
            });
        let opts = match profile {
            "low" => Self {
                encoding: AudioEncoding::Aac { kbps: AAC_KBPS },
                profile: "low",
                prebuffer_blocks: 0,
                estimated_kbps: AAC_KBPS,
            },
            "medium" => Self {
                encoding: AudioEncoding::Wav {
                    bits: 16,
                    sample_rate: PCM_SAMPLE_RATE,
                },
                profile: "medium",
                prebuffer_blocks: 0,
                estimated_kbps: 1_411,
            },
            "high" => Self {
                encoding: AudioEncoding::Wav {
                    bits: 24,
                    sample_rate: HIGH_SAMPLE_RATE,
                },
                profile: "high",
                prebuffer_blocks: 0,
                estimated_kbps: 9_216,
            },
            _ => return None,
        };

        match (ext, opts.encoding) {
            ("aac", AudioEncoding::Aac { .. }) | ("wav", AudioEncoding::Wav { .. }) => Some(opts),
            _ => None,
        }
    }

    fn headers(&self, content_type: &'static str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
        headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
        headers.insert(header::ACCEPT_RANGES, HeaderValue::from_static("none"));
        headers.insert("x-stream-profile", HeaderValue::from_static(self.profile));
        if let Ok(v) = HeaderValue::from_str(&self.estimated_kbps.to_string()) {
            headers.insert("x-audio-estimated-kbps", v);
        }
        let buffer = format!("{:.1}", self.prebuffer_blocks as f32 * PCM_BLOCK_SECONDS);
        if let Ok(v) = HeaderValue::from_str(&buffer) {
            headers.insert("x-audio-prebuffer-seconds", v);
        }
        headers
    }
}

/// AAC stream of the currently voted-on station.
///
/// On each request we ask the vote registry for the winner and tune
/// to it. When the winner shifts, the vote daemon aborts the active
/// channel; this stream sees `Closed` and EOFs, prompting the browser
/// `<audio>` element to reconnect — at which point we resolve the new
/// winner and start streaming that.
pub async fn current(
    Query(params): Query<HashMap<String, String>>,
    State(state): State<AppState>,
) -> Response {
    current_with_ext("aac", params, state).await
}

pub async fn current_aac(
    Query(params): Query<HashMap<String, String>>,
    State(state): State<AppState>,
) -> Response {
    current_with_ext("aac", params, state).await
}

pub async fn current_wav(
    Query(params): Query<HashMap<String, String>>,
    State(state): State<AppState>,
) -> Response {
    current_with_ext("wav", params, state).await
}

async fn current_with_ext(ext: &str, params: HashMap<String, String>, state: AppState) -> Response {
    if let Some(fp) = params.get("fp") {
        state.votes.mark_seen(fp);
    }
    let Some(winner) = state.votes.winner() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "no station currently voted for",
        )
            .into_response();
    };
    let rx = match open_pcm_stream(&state, winner).await {
        Ok(r) => r,
        Err(resp) => return resp,
    };
    let Some(opts) = StreamOptions::for_request(ext, &params) else {
        return (StatusCode::BAD_REQUEST, "unknown stream profile").into_response();
    };
    stream_response(rx, opts)
}

/// Single handler for `/stream/{khz}.aac` and `/stream/{khz}.wav`.
/// axum 0.7 doesn't let two routes share a path-param prefix that
/// differ only by literal suffix, so we route on `:file` and split
/// the extension ourselves.
pub async fn audio(
    Path(file): Path<String>,
    Query(params): Query<HashMap<String, String>>,
    State(state): State<AppState>,
) -> Response {
    let Some(dot) = file.rfind('.') else {
        return (
            StatusCode::BAD_REQUEST,
            "missing extension; use .aac or .wav",
        )
            .into_response();
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
        "aac" | "wav" => {
            let Some(opts) = StreamOptions::for_request(ext, &params) else {
                return (StatusCode::BAD_REQUEST, "unknown stream profile").into_response();
            };
            stream_response(rx, opts)
        }
        _ => (
            StatusCode::BAD_REQUEST,
            "unknown extension; use .aac or .wav",
        )
            .into_response(),
    }
}

fn stream_response(
    rx: broadcast::Receiver<crate::encode::PcmBlock>,
    opts: StreamOptions,
) -> Response {
    match opts.encoding {
        AudioEncoding::Aac { kbps } => aac_response(rx, kbps, opts),
        AudioEncoding::Wav { bits, sample_rate } => wav_response(rx, bits, sample_rate, opts),
    }
}

fn aac_response(
    rx: broadcast::Receiver<crate::encode::PcmBlock>,
    kbps: u32,
    opts: StreamOptions,
) -> Response {
    let body = Body::from_stream(pcm_to_aac_stream(rx, kbps));
    (StatusCode::OK, opts.headers("audio/aac"), body).into_response()
}

fn wav_response(
    rx: broadcast::Receiver<crate::encode::PcmBlock>,
    bits: u16,
    sample_rate: u32,
    opts: StreamOptions,
) -> Response {
    let header = wav::header(bits, sample_rate);
    let stream = async_stream::stream! {
        yield Ok::<Bytes, Infallible>(Bytes::from(header));
        let mut rx = rx;
        let mut resampler = PcmResamplerStereo::new(PCM_SAMPLE_RATE, sample_rate);
        let mut prebuffer = Vec::with_capacity(opts.prebuffer_blocks);
        for _ in 0..opts.prebuffer_blocks {
            match rx.recv().await {
                Ok(block) => prebuffer.push(block),
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    warn!("WAV listener lagged {n} blocks while prebuffering");
                }
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
        for block in prebuffer {
            let samples = resampler.process(&block);
            let bytes = wav::encode_block(&samples, bits);
            yield Ok(Bytes::from(bytes));
        }
        loop {
            match rx.recv().await {
                Ok(block) => {
                    let samples = resampler.process(&block);
                    let bytes = wav::encode_block(&samples, bits);
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
    (StatusCode::OK, opts.headers("audio/wav"), body).into_response()
}

fn pcm_to_aac_stream(
    rx: broadcast::Receiver<crate::encode::PcmBlock>,
    kbps: u32,
) -> impl Stream<Item = Result<Bytes, std::io::Error>> {
    async_stream::stream! {
        let mut child = match Command::new("ffmpeg")
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-f",
                "s24le",
                "-ar",
                "44100",
                "-ac",
                "2",
                "-i",
                "pipe:0",
                "-c:a",
                "aac",
                "-b:a",
                &format!("{kbps}k"),
                "-f",
                "adts",
                "pipe:1",
            ])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
        {
            Ok(child) => child,
            Err(e) => {
                warn!("failed to spawn ffmpeg for AAC stream: {e}");
                yield Err(e);
                return;
            }
        };

        let Some(mut stdin) = child.stdin.take() else {
            warn!("ffmpeg stdin unavailable");
            return;
        };
        let Some(mut stdout) = child.stdout.take() else {
            warn!("ffmpeg stdout unavailable");
            return;
        };

        let writer = tokio::spawn(async move {
            let mut rx = rx;
            loop {
                match rx.recv().await {
                    Ok(block) => {
                        let bytes = wav::encode_block(&block, 24);
                        if let Err(e) = stdin.write_all(&bytes).await {
                            warn!("ffmpeg AAC stdin write failed: {e}");
                            break;
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        warn!("AAC listener lagged {n} blocks");
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
            let _ = stdin.shutdown().await;
        });

        let mut buf = vec![0u8; 16 * 1024];
        loop {
            match stdout.read(&mut buf).await {
                Ok(0) => break,
                Ok(n) => yield Ok(Bytes::copy_from_slice(&buf[..n])),
                Err(e) => {
                    warn!("ffmpeg AAC stdout read failed: {e}");
                    yield Err(e);
                    break;
                }
            }
        }
        writer.abort();
        let _ = child.kill().await;
        let _ = child.wait().await;
    }
}

struct PcmResamplerStereo {
    in_rate: u32,
    out_rate: u32,
    pos: f64,
    prev: Option<(i32, i32)>,
}

impl PcmResamplerStereo {
    fn new(in_rate: u32, out_rate: u32) -> Self {
        Self {
            in_rate,
            out_rate,
            pos: 0.0,
            prev: None,
        }
    }

    fn process(&mut self, block: &[i32]) -> Vec<i32> {
        if self.in_rate == self.out_rate {
            return block.to_vec();
        }

        let input_frames = block.len() / 2;
        if input_frames == 0 {
            return Vec::new();
        }

        let mut frames = Vec::with_capacity(input_frames + usize::from(self.prev.is_some()));
        if let Some(prev) = self.prev {
            frames.push(prev);
        }
        frames.extend(block.chunks_exact(2).map(|s| (s[0], s[1])));

        let step = self.in_rate as f64 / self.out_rate as f64;
        let mut out = Vec::with_capacity(
            (input_frames as u64 * self.out_rate as u64 / self.in_rate as u64 * 2) as usize + 4,
        );
        while self.pos + 1.0 < frames.len() as f64 {
            let i = self.pos.floor() as usize;
            let frac = self.pos - i as f64;
            let (l0, r0) = frames[i];
            let (l1, r1) = frames[i + 1];
            out.push(lerp_i32(l0, l1, frac));
            out.push(lerp_i32(r0, r1, frac));
            self.pos += step;
        }

        self.pos -= (frames.len() - 1) as f64;
        self.prev = frames.last().copied();
        out
    }
}

fn lerp_i32(a: i32, b: i32, frac: f64) -> i32 {
    (a as f64 + (b as f64 - a as f64) * frac).round() as i32
}
