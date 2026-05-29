//! Per-station RDS metadata snapshot, shared between the DSP task
//! that decodes the data stream and the HTTP API that exposes it.

use std::sync::Arc;
use std::time::SystemTime;

use parking_lot::Mutex;
use serde::Serialize;

use crate::dsp::rds_decoder::RdsMetadata;

#[derive(Clone, Default)]
pub struct StationMetadata(pub Arc<Mutex<StationMetadataInner>>);

#[derive(Default, Debug)]
pub struct StationMetadataInner {
    pub rds: RdsMetadata,
    pub last_update: Option<SystemTime>,
    pub first_decoded_at: Option<SystemTime>,
}

impl StationMetadata {
    pub fn new() -> Self {
        Self(Arc::new(Mutex::new(StationMetadataInner::default())))
    }

    pub fn update_rds(&self, rds: RdsMetadata) {
        let mut g = self.0.lock();
        if g.first_decoded_at.is_none() {
            g.first_decoded_at = Some(SystemTime::now());
        }
        g.last_update = Some(SystemTime::now());
        g.rds = rds;
    }

    pub fn snapshot(&self) -> RdsMetadataDto {
        let g = self.0.lock();
        let rds = &g.rds;
        RdsMetadataDto {
            pi_hex: rds.pi.map(|p| format!("{:04X}", p)),
            callsign: rds.callsign.clone(),
            ps: rds.ps.clone(),
            radiotext: rds.rt.clone(),
            pty: rds.pty,
            pty_name: rds.pty_name.clone(),
            tp: rds.tp,
            ta: rds.ta,
            music: rds.ms_music,
            groups_decoded: rds.groups_decoded,
            blocks_dropped: rds.blocks_dropped,
            last_update_unix: g.last_update.and_then(|t| {
                t.duration_since(SystemTime::UNIX_EPOCH)
                    .ok()
                    .map(|d| d.as_secs())
            }),
            first_decoded_at_unix: g.first_decoded_at.and_then(|t| {
                t.duration_since(SystemTime::UNIX_EPOCH)
                    .ok()
                    .map(|d| d.as_secs())
            }),
        }
    }
}

#[derive(Debug, Serialize, Clone)]
pub struct RdsMetadataDto {
    pub pi_hex: Option<String>,
    pub callsign: Option<String>,
    pub ps: Option<String>,
    pub radiotext: Option<String>,
    pub pty: Option<u8>,
    pub pty_name: Option<String>,
    pub tp: bool,
    pub ta: bool,
    /// Music/speech flag: true=music, false=speech.
    pub music: bool,
    pub groups_decoded: u64,
    pub blocks_dropped: u64,
    pub last_update_unix: Option<u64>,
    pub first_decoded_at_unix: Option<u64>,
}
