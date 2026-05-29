//! rtl-fm: pure-Rust RTL-SDR FM streaming server.
//!
//! Architecture: one tokio task drives a single RTL-SDR over nusb,
//! broadcasting raw IQ chunks to subscribers. A channelizer spawns
//! per-station DSP tasks on demand which produce 24-bit / 44.1 kHz
//! stereo PCM, which is encoded to streaming FLAC and served via axum.
//!
//! All channels are discovered at startup by an energy scanner that
//! sweeps the 88-108 MHz broadcast band; nothing is hard-coded.

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

use std::sync::Arc;

use anyhow::Context;
use clap::Parser;
use tracing::{info, warn};
use tracing_subscriber::{EnvFilter, fmt};

use crate::channelizer::Channelizer;
use crate::config::Args;
use crate::rtlsdr::RtlSdr;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();

    fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info,rtl_fm=debug")),
        )
        .init();

    info!(
        "rtl-fm starting; listen={} band=[{}..{}] MHz",
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

    // Initial energy scan to populate channels.
    info!("starting initial band scan...");
    let scanned = channelizer
        .scan_band(args.scan_start_mhz, args.scan_end_mhz)
        .await
        .context("initial energy scan")?;
    info!("scan complete: found {} stations", scanned.len());

    // Re-park tuner at the strongest cluster center.
    if let Some(center) = channelizer.pick_default_window(&scanned) {
        rtl.set_center_freq(center).await.ok();
        channelizer.set_center(center);
    }

    // Spawn the idle metadata refresher: while no real listeners are
    // streaming, it cycles through the scanned stations and parks the
    // tuner on each for a few seconds so RDS metadata stays fresh on
    // the home page.
    let refresher_chan = channelizer.clone();
    let refresher_task = tokio::spawn(async move {
        run_idle_refresher(refresher_chan).await;
    });

    // Build axum app state + router.
    let state = api::AppState {
        rtl: rtl.clone(),
        channelizer: channelizer.clone(),
        scan_band_mhz: (args.scan_start_mhz, args.scan_end_mhz),
    };
    let router = api::router(state);

    let listener = tokio::net::TcpListener::bind(args.listen)
        .await
        .with_context(|| format!("binding {}", args.listen))?;
    info!("HTTP listening on http://{}", args.listen);

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
    channelizer.drop_all_channels();
    refresher_task.abort();
    server.abort();
    iq_task.abort();
    // give tokio a tick to actually deliver the aborts
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    Ok(())
}

/// Idle metadata refresher.
///
/// Cycles through the scanned stations, parking the tuner on each
/// for `STATION_DWELL` so its channel task can accumulate RDS into
/// the shared metadata store. As soon as any real listener appears
/// the refresher backs off so it doesn't fight them for the tuner.
async fn run_idle_refresher(channelizer: std::sync::Arc<Channelizer>) {
    use std::time::Duration;

    // HD Radio needs enough dwell to acquire OFDM timing, frame sync,
    // FEC, and AAS/LOT metadata. Four seconds is enough for RDS PS/RT
    // refreshes but routinely aborts NRSC-5 before it can prove lock.
    const STATION_DWELL: Duration = Duration::from_secs(30);
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
        if channelizer.any_real_listeners() {
            channelizer.set_idle_refresher_freq(None);
            tokio::time::sleep(BUSY_PAUSE).await;
            continue;
        }
        let station = scanned[idx % scanned.len()].clone();
        idx = idx.wrapping_add(1);

        channelizer.set_idle_refresher_freq(Some(station.freq_hz));
        if let Err(e) = channelizer
            .refresh_metadata(station.freq_hz, STATION_DWELL)
            .await
        {
            warn!(
                "idle refresh {} kHz: {e}",
                station.freq_hz / 1000
            );
        }
        channelizer.set_idle_refresher_freq(None);
        tokio::time::sleep(IDLE_PAUSE).await;
    }
}

async fn wait_for_shutdown() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
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
