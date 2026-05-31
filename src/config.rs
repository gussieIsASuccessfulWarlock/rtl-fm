//! CLI args.

use std::net::SocketAddr;
use std::path::PathBuf;

use clap::{Parser, Subcommand};

#[derive(Parser, Debug, Clone)]
#[command(
    name = "RTL FM",
    version,
    about = "RTL FM — pure-Rust RTL-SDR FM streaming radio"
)]
pub struct Args {
    #[command(subcommand)]
    pub command: Option<Command>,

    /// HTTP listen address (used by `serve`).
    #[arg(long, default_value = "0.0.0.0:8080", global = true)]
    pub listen: SocketAddr,

    /// Verbose logging. Without this the server prints only its address
    /// and live listener count; with it, the full DSP/decoder trace logs.
    #[arg(long, global = true)]
    pub debug: bool,

    /// Low edge of the band to scan, in MHz (used by `serve`).
    #[arg(long, default_value_t = 88, global = true)]
    pub scan_start_mhz: u32,

    /// High edge of the band to scan, in MHz (used by `serve`).
    #[arg(long, default_value_t = 108, global = true)]
    pub scan_end_mhz: u32,

    /// Display name of the website owner/operator. Shown as subtext under
    /// the "RTL FM" brand and reported by `GET /meta`.
    #[arg(long, default_value = "", global = true)]
    pub owner: String,

    /// Physical location of the station (e.g. "San Antonio, TX"). Shown as
    /// subtext under the brand and reported by `GET /meta`.
    #[arg(long, default_value = "", global = true)]
    pub location: String,
}

#[derive(Subcommand, Debug, Clone)]
pub enum Command {
    /// Run the streaming HTTP server (default when no subcommand is given).
    Serve,

    /// Decode a recorded raw CU8 IQ capture into station name + album art.
    ///
    /// The capture must have been recorded at NRSC5_RTL_RATE
    /// (1 488 375 S/s) — `hd --record <file>` produces such a file.
    /// Deterministic, requires no hardware.
    Replay {
        /// Raw CU8 IQ file.
        #[arg(long)]
        file: PathBuf,
        /// Tuner center frequency the capture was recorded at, in Hz.
        /// Defaults to the capture's nominal carrier; equal to the
        /// station frequency for a `hd`-produced file.
        #[arg(long)]
        freq: u32,
    },

    /// Tune the SDR directly to one HD station; print metadata and
    /// write album art to disk.  Optionally also tee raw IQ to a file
    /// so the capture can be re-decoded later via `replay`.
    Hd {
        /// Station carrier in Hz (e.g. 100300000 for 100.3 MHz).
        #[arg(long)]
        freq: u32,
        /// How many seconds to capture before exiting.
        #[arg(long, default_value_t = 90)]
        seconds: u64,
        /// Optional file to tee raw IQ samples into for later replay.
        #[arg(long)]
        record: Option<PathBuf>,
    },
}
