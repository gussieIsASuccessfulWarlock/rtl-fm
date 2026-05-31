//! rtl-fm: pure-Rust RTL-SDR FM streaming server + offline NRSC-5
//! decoder harness.
//!
//! Default `serve` subcommand: one tokio task drives a single RTL-SDR
//! over nusb, broadcasting raw IQ chunks to subscribers. A channelizer
//! spawns per-station DSP tasks on demand which produce 24-bit /
//! 44.1 kHz stereo PCM, encoded to streaming AAC/WAV and served via axum.
//! All channels are discovered at startup by an energy scanner that
//! sweeps the 88-108 MHz broadcast band; nothing is hard-coded.
//!
//! `replay --file <cu8> --freq <hz>` runs the same NRSC-5 decoder
//! against a recorded raw CU8 IQ file. No hardware, no USB.
//! Deterministic; the only way to iterate on the album-art chain
//! without re-tuning the radio every iteration.
//!
//! `hd --freq <hz> [--seconds N] [--record <cu8>]` tunes the SDR
//! directly to one HD station, drives the decoder for `seconds`, and
//! writes any decoded album art + station name. `--record` tees raw
//! IQ to disk so the capture can be re-decoded via `replay`.

// The driver/DSP modules expose more API surface than the binary
// currently exercises (e.g. manual-gain selection, 75 µs deemphasis,
// utility helpers retained for future endpoints). Keep them in the
// build without warning noise.
#![allow(dead_code)]

mod api;
mod channelizer;
mod config;
mod dsp;
mod encode;
mod error;
mod nrsc5;
mod rtlsdr;
mod usb;
mod voting;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use clap::Parser;
use indicatif::{ProgressBar, ProgressStyle};
use tokio::sync::broadcast;
use tracing::{info, warn};
use tracing_subscriber::filter::LevelFilter;
use tracing_subscriber::prelude::*;
use tracing_subscriber::{fmt, EnvFilter};

use crate::channelizer::metadata::StationMetadata;
use crate::channelizer::{Channelizer, ScanProgress};
use crate::config::{Args, Command};
use crate::nrsc5::consts::NRSC5_RTL_RATE;
use crate::nrsc5::Nrsc5Decoder;
use crate::rtlsdr::RtlSdr;
use crate::usb::transfer::URB_SIZE;
use crate::voting::VoteRegistry;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();

    // `replay`/`hd` print their results through `info!`, so stay verbose for
    // them. `serve` is quiet by default (warnings + errors only) so the scan
    // progress bar and the listener status line are the whole console; pass
    // `--debug` (or set RUST_LOG) to get the full DSP/decoder trace back.
    let verbose = args.debug || !matches!(args.command, None | Some(Command::Serve));
    let default_filter = if verbose { "info,rtl_fm=debug" } else { "warn" };
    let console = fmt::layer().with_writer(std::io::stderr).with_filter(
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(default_filter)),
    );
    // Mirror WARN+ERROR (every problem this codebase logs) into error.log,
    // appending across runs. If the file can't be opened, carry on with the
    // console only.
    let error_log = match std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open("error.log")
    {
        Ok(f) => Some(
            fmt::layer()
                .with_ansi(false)
                .with_writer(Arc::new(f))
                .with_filter(LevelFilter::WARN),
        ),
        Err(e) => {
            eprintln!("warning: could not open error.log ({e}); logging to console only");
            None
        }
    };
    tracing_subscriber::registry()
        .with(console)
        .with(error_log)
        .init();

    match args.command.clone() {
        None | Some(Command::Serve) => serve(args).await,
        Some(Command::Replay { file, freq }) => replay(file, freq).await,
        Some(Command::Hd {
            freq,
            seconds,
            record,
        }) => hd_capture(freq, seconds, record).await,
    }
}

async fn serve(args: Args) -> anyhow::Result<()> {
    info!(
        "rtl-fm serve: listen={} band=[{}..{}] MHz",
        args.listen, args.scan_start_mhz, args.scan_end_mhz
    );

    let rtl = RtlSdr::open()
        .await
        .context("opening RTL-SDR device (is it plugged in and is the udev rule installed?)")?;
    info!("RTL-SDR opened: {}", rtl.describe());

    // Initial center freq: scan start + half a tuner window.
    let initial_center_hz = (args.scan_start_mhz + 1) * 1_000_000;
    rtl.set_sample_rate(rtlsdr::DEFAULT_SAMPLE_RATE)
        .await
        .context("set initial sample rate")?;
    rtl.set_center_freq(initial_center_hz)
        .await
        .context("set initial center freq")?;
    rtl.set_tuner_gain_auto(true).await.ok();

    // Spawn the IQ producer task: it owns the RTL handle's bulk pump
    // and broadcasts u8 IQ chunks to all subscribers.
    let rtl = Arc::new(rtl);
    let (iq_tx, _rx0) = tokio::sync::broadcast::channel(64);
    let producer_rtl = rtl.clone();
    let producer_tx = iq_tx.clone();
    let iq_task = tokio::spawn(async move {
        if let Err(e) = producer_rtl.run_iq_pump(producer_tx).await {
            warn!("IQ pump exited: {e:#}");
        }
    });

    // Channelizer holds the tuner policy state and per-station tasks.
    let channelizer = Arc::new(Channelizer::new(
        rtl.clone(),
        iq_tx.clone(),
        initial_center_hz,
    ));

    // Initial energy scan to populate channels. In debug mode the info!
    // logs narrate it; otherwise render a progress bar.
    info!("starting initial band scan...");
    let scanned = if args.debug {
        channelizer
            .scan_band(args.scan_start_mhz, args.scan_end_mhz, |_| {})
            .await
    } else {
        let pb = ProgressBar::new_spinner();
        pb.set_style(ProgressStyle::with_template("  {spinner:.yellow} {msg}").unwrap());
        pb.enable_steady_tick(Duration::from_millis(120));
        pb.set_message("Scanning FM band…");
        let bar = pb.clone();
        let res = channelizer
            .scan_band(args.scan_start_mhz, args.scan_end_mhz, move |p| match p {
                ScanProgress::Sweep { done, total } => {
                    bar.set_message(format!("Scanning FM band — energy sweep {done}/{total}"));
                }
                ScanProgress::Verify { done, total } => {
                    if done == 0 {
                        bar.set_length(total);
                        bar.set_style(
                            ProgressStyle::with_template(
                                "  Verifying stations [{bar:28.yellow/dim}] {pos}/{len}",
                            )
                            .unwrap()
                            .progress_chars("=>-"),
                        );
                    }
                    bar.set_position(done);
                }
            })
            .await;
        pb.finish_and_clear();
        res
    }
    .context("initial energy scan")?;
    info!("scan complete: found {} stations", scanned.len());
    if !args.debug {
        println!(
            "  RTL FM v{}: found {} stations in {}–{} MHz",
            env!("CARGO_PKG_VERSION"),
            scanned.len(),
            args.scan_start_mhz,
            args.scan_end_mhz
        );
    }

    // Re-park tuner at the strongest cluster center.
    if let Some(center) = channelizer.pick_default_window(&scanned) {
        rtl.set_center_freq(center).await.ok();
        channelizer.set_center(center);
    }

    // Vote registry: per-fingerprint heartbeats elected to one
    // station-of-the-moment via majority.
    let votes = VoteRegistry::new();

    // Spawn the idle metadata refresher: while no real listeners are
    // streaming AND no votes are active, it cycles through the scanned
    // stations and parks the tuner on each for a few seconds so RDS
    // metadata stays fresh on the home page.
    let refresher_chan = channelizer.clone();
    let refresher_votes = votes.clone();
    let refresher_task = tokio::spawn(async move {
        run_idle_refresher(refresher_chan, refresher_votes).await;
    });

    // Vote daemon: watches the vote registry; whenever the winning
    // frequency changes, drops all active audio channels so listeners
    // reconnect to the stream endpoint and pick up the new winner.
    let vote_chan = channelizer.clone();
    let vote_registry = votes.clone();
    let vote_task = tokio::spawn(async move {
        run_vote_daemon(vote_registry, vote_chan).await;
    });

    // Mint the admin token for this run. The operator reaches the privileged
    // UI + full API via the printed admin link; everyone else is anonymous.
    let admin_token: Arc<str> = Arc::from(api::auth::gen_token());

    // Build axum app state + router.
    let state = api::AppState {
        rtl: rtl.clone(),
        channelizer: channelizer.clone(),
        votes: votes.clone(),
        scan_band_mhz: (args.scan_start_mhz, args.scan_end_mhz),
        owner: args.owner.clone(),
        location: args.location.clone(),
        admin_token: admin_token.clone(),
        sessions: api::auth::SessionRegistry::new(),
    };
    let router = api::router(state);

    let listener = tokio::net::TcpListener::bind(args.listen)
        .await
        .with_context(|| format!("binding {}", args.listen))?;
    info!("HTTP listening on http://{}", args.listen);

    // Always surface both URLs to the operator (the admin link carries the
    // secret token; keep it private).
    println!("\n  RTL FM v{} ready", env!("CARGO_PKG_VERSION"));
    println!("    Public URL: http://{}/", args.listen);
    println!("    Admin URL:  http://{}/?token={}\n", args.listen, admin_token);

    // Quiet mode: a single live status line showing where we're served and
    // how many listeners are currently connected.
    let (status_pb, status_task) = if args.debug {
        (None, None)
    } else {
        let pb = ProgressBar::new_spinner();
        pb.set_style(ProgressStyle::with_template("  {spinner:.green} {msg}").unwrap());
        pb.enable_steady_tick(Duration::from_millis(200));
        let pbc = pb.clone();
        let ch = channelizer.clone();
        let addr = args.listen;
        let task = tokio::spawn(async move {
            loop {
                let n: usize = ch.active_listeners().values().sum();
                pbc.set_message(format!(
                    "Serving http://{addr} · {n} listener{}",
                    if n == 1 { "" } else { "s" }
                ));
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
        });
        (Some(pb), Some(task))
    };

    // Don't use axum's `with_graceful_shutdown` — it waits for in-flight
    // requests to finish, but our audio streams are open-ended and would
    // hang shutdown forever. Instead race the server against the signal
    // and abort on Ctrl+C / SIGTERM.
    let server = tokio::spawn(async move {
        if let Err(e) = axum::serve(listener, router).await {
            warn!("axum serve exited: {e:#}");
        }
    });

    wait_for_shutdown().await;
    info!("shutdown signal received; aborting tasks");
    if let Some(task) = status_task {
        task.abort();
    }
    if let Some(pb) = status_pb {
        pb.finish_and_clear();
    }
    channelizer.drop_all_channels();
    refresher_task.abort();
    vote_task.abort();
    server.abort();
    iq_task.abort();
    // give tokio a tick to actually deliver the aborts
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    Ok(())
}

/// Replay a recorded CU8 IQ file through the NRSC-5 decoder.
///
/// `file` must contain raw interleaved u8 I/Q samples captured at
/// [`NRSC5_RTL_RATE`].  Reads in [`URB_SIZE`]-byte chunks (same shape
/// the bulk pump produces) and feeds each into a
/// [`Nrsc5Decoder`].  Any surfaced station name / album art is logged
/// and written to disk.
async fn replay(file: PathBuf, freq: u32) -> anyhow::Result<()> {
    use std::fs::File;
    use std::io::Read;

    info!(
        "replay: file={} freq={} Hz rate={} (CU8 samples = bytes/2)",
        file.display(),
        freq,
        NRSC5_RTL_RATE
    );

    let meta = StationMetadata::new();
    let mut decoder = Nrsc5Decoder::new(Arc::new(meta.clone()), freq, freq, NRSC5_RTL_RATE);

    let mut f = File::open(&file).with_context(|| format!("opening {}", file.display()))?;
    let mut buf = vec![0u8; URB_SIZE];
    let mut total = 0u64;
    loop {
        let n = f.read(&mut buf).context("reading replay file")?;
        if n == 0 {
            break;
        }
        decoder.process_chunk(&buf[..n]);
        total += n as u64;
    }
    let seconds = (total as f64 / 2.0) / NRSC5_RTL_RATE as f64;
    info!(
        "replay: processed {} bytes (~{:.1}s); chunks={} P1 frames={}",
        total,
        seconds,
        decoder.chunks_seen(),
        decoder.frames_processed()
    );

    write_decoder_results(&meta, freq)?;
    Ok(())
}

/// Tune the SDR straight to one HD station and drive a
/// [`Nrsc5Decoder`] against the live IQ stream for `seconds`.  No
/// channelizer, no HTTP server.  Optionally tees the raw IQ to
/// `record` so the capture can be reanalysed offline via `replay`.
async fn hd_capture(freq: u32, seconds: u64, record: Option<PathBuf>) -> anyhow::Result<()> {
    let rtl = RtlSdr::open()
        .await
        .context("opening RTL-SDR (is it plugged in and is the udev rule installed?)")?;
    info!("RTL-SDR opened: {}", rtl.describe());
    rtl.set_sample_rate(NRSC5_RTL_RATE)
        .await
        .context("set NRSC-5 sample rate")?;
    rtl.set_center_freq(freq).await.context("set center freq")?;
    rtl.set_tuner_gain_auto(true).await.ok();

    let rtl = Arc::new(rtl);
    let (iq_tx, mut iq_rx) = broadcast::channel(64);
    let producer = rtl.clone();
    let pump = tokio::spawn(async move {
        if let Err(e) = producer.run_iq_pump(iq_tx).await {
            warn!("IQ pump exited: {e:#}");
        }
    });

    let meta = StationMetadata::new();
    let mut decoder = Nrsc5Decoder::new(Arc::new(meta.clone()), freq, freq, NRSC5_RTL_RATE);

    let mut recorder = match record.as_ref() {
        Some(p) => {
            info!("recording IQ to {}", p.display());
            Some(std::fs::File::create(p).with_context(|| format!("opening {}", p.display()))?)
        }
        None => None,
    };

    info!(
        "HD capture started; freq={} Hz, duration={} s",
        freq, seconds
    );
    let deadline = tokio::time::Instant::now() + Duration::from_secs(seconds);

    loop {
        let chunk = match tokio::time::timeout_at(deadline, iq_rx.recv()).await {
            Ok(Ok(c)) => c,
            Ok(Err(broadcast::error::RecvError::Lagged(n))) => {
                info!("HD capture lagged {n} chunks; resetting decoder state");
                decoder = Nrsc5Decoder::new(Arc::new(meta.clone()), freq, freq, NRSC5_RTL_RATE);
                continue;
            }
            Ok(Err(broadcast::error::RecvError::Closed)) => break,
            Err(_) => break, // deadline elapsed
        };
        if let Some(f) = recorder.as_mut() {
            use std::io::Write;
            if let Err(e) = f.write_all(&chunk) {
                warn!("recorder write failed: {e}");
                recorder = None;
            }
        }
        decoder.process_chunk(&chunk);
    }
    pump.abort();
    info!(
        "HD capture done: chunks={} P1 frames={}",
        decoder.chunks_seen(),
        decoder.frames_processed()
    );

    write_decoder_results(&meta, freq)?;
    Ok(())
}

/// Log decoded station name and write any surfaced album art to disk
/// as `album_art_<freq>.<ext>`.  Shared between `replay` and `hd`.
fn write_decoder_results(meta: &StationMetadata, freq: u32) -> anyhow::Result<()> {
    let snap = meta.snapshot();
    if let Some(hd) = snap.hd.as_ref() {
        if let Some(name) = hd.station_name.as_ref() {
            info!("station name: {}", name);
        }
    }
    match meta.album_art() {
        Some(art) => {
            let ext = match art.mime.as_deref() {
                Some("image/png") => "png",
                Some("image/jpeg") => "jpg",
                _ => "bin",
            };
            let out = format!("album_art_{freq}.{ext}");
            std::fs::write(&out, &art.bytes).with_context(|| format!("writing {out}"))?;
            info!(
                "wrote album art: {} ({} bytes, mime={})",
                out,
                art.bytes.len(),
                art.mime.as_deref().unwrap_or("<unknown>")
            );
        }
        None => info!("no album art surfaced in this capture"),
    }
    Ok(())
}

/// Idle metadata refresher.
///
/// Cycles through the scanned stations, parking the tuner on each
/// for `STATION_DWELL` so its channel task can accumulate RDS into
/// the shared metadata store. Pauses whenever a real listener is
/// streaming or any vote is active — in either case the tuner is
/// already (or about to be) parked on a station the user cares
/// about, and cycling would just interrupt them.
async fn run_idle_refresher(
    channelizer: std::sync::Arc<Channelizer>,
    votes: std::sync::Arc<VoteRegistry>,
) {
    use std::time::Duration;

    const STATION_DWELL: Duration = Duration::from_secs(4);
    const IDLE_PAUSE: Duration = Duration::from_millis(750);
    const BUSY_PAUSE: Duration = Duration::from_secs(2);

    let mut idx = 0usize;
    loop {
        let scanned = channelizer.scanned();
        if scanned.is_empty() {
            channelizer.set_idle_refresher_freq(None);
            tokio::time::sleep(BUSY_PAUSE).await;
            continue;
        }
        if channelizer.hd_scan_active() {
            tokio::time::sleep(BUSY_PAUSE).await;
            continue;
        }
        if channelizer.any_real_listeners() || votes.winner().is_some() {
            channelizer.set_idle_refresher_freq(None);
            tokio::time::sleep(BUSY_PAUSE).await;
            continue;
        }
        let station = scanned[idx % scanned.len()].clone();
        idx = idx.wrapping_add(1);

        channelizer.set_idle_refresher_freq(Some(station.freq_hz));
        // Keep the idle pass non-disruptive: do not switch RTL sample rates
        // in the background, because that races with live audio playback.
        if let Err(e) = channelizer
            .refresh_metadata(station.freq_hz, STATION_DWELL)
            .await
        {
            warn!("idle refresh {} kHz: {e}", station.freq_hz / 1000);
        }
        channelizer.set_idle_refresher_freq(None);
        tokio::time::sleep(IDLE_PAUSE).await;
    }
}

/// Vote daemon: tracks the winning station and forces listeners onto
/// it whenever the winner changes.
///
/// The actual audio path is driven by HTTP stream handlers,
/// which on each request call `channelizer.tune(winner)`. When the
/// winner shifts we abort the active audio channel; the HTTP body
/// stream EOFs, the browser `<audio>` element fires `ended`/`error`,
/// and the home-page script reopens the stream endpoint, which tunes to
/// the new winner. The whole switch takes a few hundred ms.
async fn run_vote_daemon(
    votes: std::sync::Arc<VoteRegistry>,
    channelizer: std::sync::Arc<Channelizer>,
) {
    use std::time::Duration;

    // Cap the wait so stale votes get pruned (and their absence
    // detected) even if nobody is sending new heartbeats.
    const POLL_INTERVAL: Duration = Duration::from_millis(500);

    let mut current_winner: Option<u32> = None;
    loop {
        votes.prune_stale();
        let winner = votes.winner();
        if winner != current_winner {
            match winner {
                Some(freq_hz) => {
                    info!("vote winner changed: {} kHz", freq_hz / 1000);
                }
                None => info!("no active voters; releasing tuner"),
            }
            // Dropping the current channel kicks live listeners; their
            // browsers will reconnect and the next stream request
            // tunes to the new winner. We don't proactively re-tune
            // here because the channel task only stays alive while
            // listeners are attached.
            channelizer.drop_all_channels();
            current_winner = winner;
        }
        tokio::select! {
            _ = votes.notify.notified() => {}
            _ = tokio::time::sleep(POLL_INTERVAL) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::Read;
    use std::sync::Arc;

    use tracing_subscriber::filter::LevelFilter;
    use tracing_subscriber::prelude::*;
    use tracing_subscriber::{fmt, Layer};

    /// The error.log layer must capture WARN+ERROR and drop lower levels.
    #[test]
    fn error_log_layer_captures_warn_and_error_only() {
        let path = std::env::temp_dir().join(format!("rtlfm-errlog-{}.log", std::process::id()));
        let _ = std::fs::remove_file(&path);
        {
            let f = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
                .unwrap();
            let layer = fmt::layer()
                .with_ansi(false)
                .with_writer(Arc::new(f))
                .with_filter(LevelFilter::WARN);
            let sub = tracing_subscriber::registry().with(layer.boxed());
            tracing::subscriber::with_default(sub, || {
                tracing::info!("INFO_should_be_hidden");
                tracing::warn!("WARN_should_appear");
                tracing::error!("ERROR_should_appear");
            });
        }
        let mut s = String::new();
        std::fs::File::open(&path)
            .unwrap()
            .read_to_string(&mut s)
            .unwrap();
        let _ = std::fs::remove_file(&path);
        assert!(s.contains("WARN_should_appear"), "warn missing: {s:?}");
        assert!(s.contains("ERROR_should_appear"), "error missing: {s:?}");
        assert!(!s.contains("INFO_should_be_hidden"), "info leaked: {s:?}");
    }
}

async fn wait_for_shutdown() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        let mut term = signal(SignalKind::terminate()).expect("install SIGTERM handler");
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = term.recv() => {}
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}
