//! CLI args.

use std::net::SocketAddr;

use clap::Parser;

#[derive(Parser, Debug, Clone)]
#[command(version, about = "Pure-Rust RTL-SDR FM streaming server")]
pub struct Args {
    /// HTTP listen address.
    #[arg(long, default_value = "0.0.0.0:8080")]
    pub listen: SocketAddr,

    /// Low edge of the band to scan, in MHz.
    #[arg(long, default_value_t = 88)]
    pub scan_start_mhz: u32,

    /// High edge of the band to scan, in MHz.
    #[arg(long, default_value_t = 108)]
    pub scan_end_mhz: u32,
}
