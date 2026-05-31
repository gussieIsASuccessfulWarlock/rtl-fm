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
    pub hd: HdMetadataDto,
    pub hd_album_art: Option<HdAlbumArt>,
    pub signal_snr_db: Option<f32>,
    pub last_update: Option<SystemTime>,
    pub first_decoded_at: Option<SystemTime>,
}

impl StationMetadata {
    pub fn new() -> Self {
        Self(Arc::new(Mutex::new(StationMetadataInner::default())))
    }

    pub fn update_rds(&self, rds: RdsMetadata, signal_snr_db: Option<f32>) {
        let mut g = self.0.lock();
        if g.first_decoded_at.is_none() {
            g.first_decoded_at = Some(SystemTime::now());
        }
        g.last_update = Some(SystemTime::now());
        g.rds = rds;
        if signal_snr_db.is_some() {
            g.signal_snr_db = signal_snr_db;
        }
    }

    pub fn update_hd(&self, hd: HdMetadataDto) {
        let mut g = self.0.lock();
        if g.first_decoded_at.is_none() {
            g.first_decoded_at = Some(SystemTime::now());
        }
        g.last_update = Some(SystemTime::now());
        let album_art_mime = g.hd.album_art_mime.clone();
        let album_art_len = g.hd.album_art_len;
        let album_art_updated_unix = g.hd.album_art_updated_unix;
        g.hd = HdMetadataDto {
            album_art_mime,
            album_art_len,
            album_art_updated_unix,
            ..hd
        };
    }

    pub fn update_hd_album_art(&self, mime: Option<String>, bytes: Vec<u8>) {
        let mut g = self.0.lock();
        if g.first_decoded_at.is_none() {
            g.first_decoded_at = Some(SystemTime::now());
        }
        g.last_update = Some(SystemTime::now());
        g.hd.album_art_mime = mime.clone();
        g.hd.album_art_len = Some(bytes.len());
        g.hd.album_art_updated_unix = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .ok()
            .map(|d| d.as_secs());
        g.hd_album_art = Some(HdAlbumArt { mime, bytes });
    }

    pub fn album_art(&self) -> Option<HdAlbumArt> {
        self.0.lock().hd_album_art.clone()
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
            signal_snr_db: g.signal_snr_db,
            hd: Some(g.hd.clone()).filter(|hd| hd.has_data()),
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
    pub signal_snr_db: Option<f32>,
    pub hd: Option<HdMetadataDto>,
    pub last_update_unix: Option<u64>,
    pub first_decoded_at_unix: Option<u64>,
}

#[derive(Debug, Serialize, Clone, Default)]
pub struct HdMetadataDto {
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub genre: Option<String>,
    pub station_name: Option<String>,
    pub slogan: Option<String>,
    pub album_art_mime: Option<String>,
    pub album_art_len: Option<usize>,
    pub album_art_updated_unix: Option<u64>,
}

impl HdMetadataDto {
    pub fn has_data(&self) -> bool {
        self.title.is_some()
            || self.artist.is_some()
            || self.album.is_some()
            || self.genre.is_some()
            || self.station_name.is_some()
            || self.slogan.is_some()
            || self.album_art_len.is_some()
    }
}

#[derive(Debug, Clone)]
pub struct HdAlbumArt {
    pub mime: Option<String>,
    pub bytes: Vec<u8>,
}
