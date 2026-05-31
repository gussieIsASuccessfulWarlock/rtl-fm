//! NRSC-5 LOT (Large Object Transfer) reassembly.
//!
//! Port of the LOT handling in theori/nrsc5 `src/output.c` (`process_port`,
//! GPLv3, re-implemented in Rust). Album art and station logos are sent as
//! files fragmented into 256-byte pieces over an AAS data port. Each
//! fragment carries an 8-byte LOT header (`hdrlen`, `repeat`, 16-bit
//! `lot` id, 32-bit fragment `seq`); the fragment that also carries the
//! file metadata extends that header by a 16-byte fixed block (version,
//! expiry, 32-bit `size`, 32-bit `mime`) followed by the filename.
//!
//! We don't depend on the SIG table to identify LOT ports — [`super::aas`]
//! already routes the candidate port range here, and the per-file `mime`
//! discriminator plus the file's own magic bytes tell us whether the
//! completed object is a JPEG/PNG image worth surfacing as album art.
//!
//! Files are keyed by `(port, lot)` and held until complete; a bounded LRU
//! (`MAX_LOT_FILES`) caps memory. Completion fires once per object — a
//! metadata change on the same `(port, lot)` resets and re-arms it.

#![allow(dead_code)]

use std::collections::HashMap;

use tracing::{debug, trace};

/// Fragment payload size (output.h `LOT_FRAGMENT_SIZE`).
const LOT_FRAGMENT_SIZE: usize = 256;
/// Max file size we will reassemble (output.h `MAX_FILE_BYTES`).
const MAX_FILE_BYTES: usize = 65536;
/// Max fragments per file (`MAX_FILE_BYTES / LOT_FRAGMENT_SIZE`).
const MAX_LOT_FRAGMENTS: usize = MAX_FILE_BYTES / LOT_FRAGMENT_SIZE; // 256
/// Max concurrently-tracked files before LRU eviction (output.h).
const MAX_LOT_FILES: usize = 12;

// MIME discriminators (include/nrsc5.h).
const MIME_PRIMARY_IMAGE: u32 = 0xBE4B_7536;
const MIME_STATION_LOGO: u32 = 0xD9C7_2536;
const MIME_JPEG: u32 = 0x1E65_3E9C;
const MIME_PNG: u32 = 0x4F32_8CA0;
const MIME_TEXT: u32 = 0xBB49_2AAC;

/// A fully reassembled LOT object.
#[derive(Debug, Clone)]
pub struct LotObject {
    pub lot_id: u16,
    pub name: String,
    /// Resolved MIME string (e.g. `image/jpeg`), suitable for HTTP.
    pub mime: String,
    pub data: Vec<u8>,
}

struct LotFile {
    name: String,
    size: usize,
    mime: u32,
    fragments: Vec<Option<Vec<u8>>>,
    bytes_so_far: usize,
    /// Identity tag (name/size/mime/expiry packed) for change detection.
    meta_tag: Option<u64>,
    completed: bool,
    last_seen: u64,
}

impl LotFile {
    fn new() -> Self {
        Self {
            name: String::new(),
            size: 0,
            mime: 0,
            fragments: vec![None; MAX_LOT_FRAGMENTS],
            bytes_so_far: 0,
            meta_tag: None,
            completed: false,
            last_seen: 0,
        }
    }

    fn reset(&mut self) {
        for f in &mut self.fragments {
            *f = None;
        }
        self.bytes_so_far = 0;
        self.completed = false;
    }
}

#[derive(Default)]
pub struct LotAssembler {
    files: HashMap<(u16, u16), LotFile>,
    lru_counter: u64,
}

impl LotAssembler {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn reset(&mut self) {
        self.files.clear();
        self.lru_counter = 0;
    }

    /// Feed one LOT fragment (the AAS payload *after* the 4-byte port/seq
    /// header has been stripped — i.e. starting at the `hdrlen` byte).
    /// Returns a completed [`LotObject`] exactly once when the last missing
    /// fragment of a sized file arrives.
    pub fn push(&mut self, port: u16, buf: &[u8]) -> Option<LotObject> {
        if buf.len() < 8 {
            trace!("LOT: short fragment ({} bytes) on port {:04X}", buf.len(), port);
            return None;
        }

        let hdrlen = buf[0] as usize;
        let _repeat = buf[1];
        let lot = u16::from_le_bytes([buf[2], buf[3]]);
        let seq = u32::from_le_bytes([buf[4], buf[5], buf[6], buf[7]]) as usize;

        if hdrlen < 8 || hdrlen > buf.len() {
            trace!("LOT: bad hdrlen {} (len {}) on port {:04X}", hdrlen, buf.len(), port);
            return None;
        }
        if seq >= MAX_LOT_FRAGMENTS {
            debug!("LOT: seq {} too large", seq);
            return None;
        }

        self.lru_counter += 1;
        let counter = self.lru_counter;
        self.evict_if_needed();

        let file = self
            .files
            .entry((port, lot))
            .or_insert_with(LotFile::new);
        file.last_seen = counter;

        // --- Metadata header present in this fragment? -----------------
        // hdrlen counts the 8-byte fragment header + (optional) 16-byte
        // fixed header + filename. A value > 8 means metadata is attached.
        if hdrlen > 8 {
            if hdrlen < 24 {
                trace!("LOT: metadata header too short ({}) on port {:04X}", hdrlen, port);
                return None;
            }
            let hb = &buf[8..]; // fixed header begins right after frag header
            let _version = u32::from_le_bytes([hb[0], hb[1], hb[2], hb[3]]);
            // hb[4..8] = packed expiry (min/hour/mday/mon/year); folded into
            // the change-detection tag rather than interpreted.
            let expiry = u32::from_le_bytes([hb[4], hb[5], hb[6], hb[7]]);
            let size = u32::from_le_bytes([hb[8], hb[9], hb[10], hb[11]]) as usize;
            let mime = u32::from_le_bytes([hb[12], hb[13], hb[14], hb[15]]);

            let name_len = hdrlen - 24;
            let name = String::from_utf8_lossy(&buf[24..24 + name_len]).into_owned();

            let tag = meta_tag(&name, size, mime, expiry);
            match file.meta_tag {
                // Metadata changed on a reused (port, lot) → new song/art,
                // discard the partially-collected previous object.
                Some(old) if old != tag => {
                    file.reset();
                    file.name = name;
                    file.size = size.min(MAX_FILE_BYTES);
                    file.mime = mime;
                    file.meta_tag = Some(tag);
                }
                // First time we learn this object's metadata. Do NOT clear
                // fragments — data fragments may have arrived out of order
                // before the header-bearing one.
                None => {
                    file.name = name;
                    file.size = size.min(MAX_FILE_BYTES);
                    file.mime = mime;
                    file.meta_tag = Some(tag);
                }
                _ => {}
            }
        }

        // --- Store the fragment payload (bytes after the full header). ---
        let frag = &buf[hdrlen..];
        if frag.len() > LOT_FRAGMENT_SIZE {
            debug!("LOT: fragment too large ({})", frag.len());
            return None;
        }
        if file.fragments[seq].is_none() {
            file.bytes_so_far += frag.len();
            file.fragments[seq] = Some(frag.to_vec());
        }

        // --- Completion check. ------------------------------------------
        if file.completed || file.size == 0 {
            return None;
        }
        let num_fragments = file.size.div_ceil(LOT_FRAGMENT_SIZE);
        if num_fragments == 0 || num_fragments > MAX_LOT_FRAGMENTS {
            return None;
        }
        if !(0..num_fragments).all(|i| file.fragments[i].is_some()) {
            return None;
        }

        // Assemble in fragment order, truncated to the declared size.
        let mut data = Vec::with_capacity(num_fragments * LOT_FRAGMENT_SIZE);
        for i in 0..num_fragments {
            data.extend_from_slice(file.fragments[i].as_ref().unwrap());
        }
        data.truncate(file.size);
        file.completed = true;

        let mime = resolve_mime(file.mime, &data);
        Some(LotObject {
            lot_id: lot,
            name: file.name.clone(),
            mime,
            data,
        })
    }

    fn evict_if_needed(&mut self) {
        while self.files.len() >= MAX_LOT_FILES {
            if let Some(key) = self
                .files
                .iter()
                .min_by_key(|(_, f)| f.last_seen)
                .map(|(k, _)| *k)
            {
                self.files.remove(&key);
            } else {
                break;
            }
        }
    }
}

fn meta_tag(name: &str, size: usize, mime: u32, expiry: u32) -> u64 {
    // Cheap order-independent-enough fingerprint for change detection.
    let mut h: u64 = 1469598103934665603; // FNV-1a offset basis
    let mut mix = |b: u8| {
        h ^= b as u64;
        h = h.wrapping_mul(1099511628211);
    };
    for b in name.bytes() {
        mix(b);
    }
    for b in (size as u64).to_le_bytes() {
        mix(b);
    }
    for b in mime.to_le_bytes() {
        mix(b);
    }
    for b in expiry.to_le_bytes() {
        mix(b);
    }
    h
}

/// Resolve the LOT `mime` discriminator to an HTTP MIME string, falling
/// back to the file's own magic bytes (album-art LOTs commonly carry the
/// generic `PRIMARY_IMAGE`/`STATION_LOGO` discriminator with JPEG or PNG
/// payloads).
fn resolve_mime(mime: u32, data: &[u8]) -> String {
    match mime {
        MIME_JPEG => return "image/jpeg".into(),
        MIME_PNG => return "image/png".into(),
        MIME_TEXT => return "text/plain".into(),
        MIME_PRIMARY_IMAGE | MIME_STATION_LOGO => {}
        _ => {}
    }
    if data.len() >= 3 && data[0] == 0xFF && data[1] == 0xD8 && data[2] == 0xFF {
        "image/jpeg".into()
    } else if data.len() >= 8 && data[..8] == [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A] {
        "image/png".into()
    } else if matches!(mime, MIME_PRIMARY_IMAGE | MIME_STATION_LOGO) {
        // Known-image discriminator but unrecognized magic — assume JPEG.
        "image/jpeg".into()
    } else {
        "application/octet-stream".into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a LOT fragment. `meta` carries (size, mime, name) for the
    /// header-bearing fragment; pass `None` for plain data fragments.
    fn fragment(lot: u16, seq: u32, meta: Option<(u32, u32, &str)>, data: &[u8]) -> Vec<u8> {
        let mut v = Vec::new();
        let hdrlen = match meta {
            Some((_, _, name)) => 8 + 16 + name.len(),
            None => 8,
        };
        v.push(hdrlen as u8); // hdrlen
        v.push(0); // repeat
        v.extend_from_slice(&lot.to_le_bytes());
        v.extend_from_slice(&seq.to_le_bytes());
        if let Some((size, mime, name)) = meta {
            v.extend_from_slice(&1u32.to_le_bytes()); // version
            v.extend_from_slice(&0u32.to_le_bytes()); // expiry
            v.extend_from_slice(&size.to_le_bytes());
            v.extend_from_slice(&mime.to_le_bytes());
            v.extend_from_slice(name.as_bytes());
        }
        v.extend_from_slice(data);
        v
    }

    #[test]
    fn reassembles_two_fragment_jpeg() {
        let mut asm = LotAssembler::new();
        // 300-byte JPEG: fragment 0 (256 B, with header), fragment 1 (44 B).
        let mut jpeg = vec![0xFFu8, 0xD8, 0xFF];
        jpeg.resize(300, 0x5A);
        let size = jpeg.len() as u32;

        let f0 = fragment(7, 0, Some((size, MIME_PRIMARY_IMAGE, "cover.jpg")), &jpeg[..256]);
        let f1 = fragment(7, 1, None, &jpeg[256..]);

        assert!(asm.push(0x1001, &f0).is_none(), "incomplete after frag 0");
        let obj = asm.push(0x1001, &f1).expect("complete after frag 1");
        assert_eq!(obj.name, "cover.jpg");
        assert_eq!(obj.mime, "image/jpeg");
        assert_eq!(obj.data, jpeg);
    }

    #[test]
    fn out_of_order_then_complete() {
        let mut asm = LotAssembler::new();
        let mut png = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
        png.resize(260, 0x11);
        let size = png.len() as u32;
        let f1 = fragment(3, 1, None, &png[256..]);
        let f0 = fragment(3, 0, Some((size, MIME_PNG, "art.png")), &png[..256]);
        assert!(asm.push(0x0401, &f1).is_none());
        let obj = asm.push(0x0401, &f0).expect("complete");
        assert_eq!(obj.mime, "image/png");
        assert_eq!(obj.data.len(), 260);
    }

    #[test]
    fn completes_only_once() {
        let mut asm = LotAssembler::new();
        let data = vec![0xFFu8, 0xD8, 0xFF, 0x01];
        let f0 = fragment(1, 0, Some((data.len() as u32, MIME_JPEG, "a.jpg")), &data);
        assert!(asm.push(0x1001, &f0).is_some());
        // A duplicate of the same completing fragment must not re-fire.
        assert!(asm.push(0x1001, &f0).is_none());
    }
}
