//! AAS (Advanced Application Services) decoder for PSD, SIG, LOT.

use std::collections::HashMap;

use crate::channelizer::metadata::RdsMetadataDto;

/// Parsed AAS packet.
#[derive(Debug, Clone)]
pub struct AasPacket {
    pub app_id: u16,
    pub payload: Vec<u8>,
}

/// PSD fields.
#[derive(Debug, Clone, Default)]
pub struct PsdData {
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub genre: Option<String>,
}

/// Parsed from SIG.
#[derive(Debug, Clone, Default)]
pub struct SigData {
    pub station_name: Option<String>,
    pub slogan: Option<String>,
}

/// Partial LOT (one segment).
#[derive(Debug, Clone)]
struct LotSegment {
    total_segments: u16,
    data: Vec<u8>,
}

/// Reassembled LOT object.
#[derive(Debug, Clone)]
pub struct LotObject {
    pub lot_id: u32,
    pub total_size: u32,
    pub data: Vec<u8>,
}

/// Decode one AAS packet from a payload buffer.
pub fn parse_aas_packets(data: &[u8]) -> Vec<AasPacket> {
    let mut packets = Vec::new();
    let mut off = 0;
    while off + 6 <= data.len() {
        let app_id = u16::from_be_bytes([data[off], data[off + 1]]);
        let length = u16::from_be_bytes([data[off + 2], data[off + 3]]) as usize;
        let _flags = data[off + 4];
        let _seq = data[off + 5];
        if off + 6 + length > data.len() {
            break;
        }
        let payload = data[off + 6..off + 6 + length].to_vec();
        packets.push(AasPacket { app_id, payload });
        off += 6 + length;
    }
    packets
}

/// Parse PSD from an AAS packet payload.
pub fn parse_psd(payload: &[u8]) -> PsdData {
    let mut psd = PsdData::default();
    let mut off = 0;
    while off + 2 <= payload.len() {
        let tag = payload[off];
        let len = payload[off + 1] as usize;
        if off + 2 + len > payload.len() {
            break;
        }
        if let Ok(s) = String::from_utf8(payload[off + 2..off + 2 + len].to_vec()) {
            match tag {
                0x01 => psd.title = Some(s),
                0x02 => psd.artist = Some(s),
                0x03 => psd.album = Some(s),
                0x04 => psd.genre = Some(s),
                _ => {}
            }
        }
        off += 2 + len;
    }
    psd
}

/// Parse SIG from an AAS packet payload.
pub fn parse_sig(payload: &[u8]) -> SigData {
    let mut sig = SigData::default();
    let mut off = 0;
    while off + 2 <= payload.len() {
        let tag = payload[off];
        let len = payload[off + 1] as usize;
        if off + 2 + len > payload.len() {
            break;
        }
        if let Ok(s) = String::from_utf8(payload[off + 2..off + 2 + len].to_vec()) {
            match tag {
                0x01 => sig.station_name = Some(s),
                0x02 => sig.slogan = Some(s),
                _ => {}
            }
        }
        off += 2 + len;
    }
    sig
}

/// LOT reassembly state machine.
pub struct LotReassembler {
    segments: HashMap<u32, Vec<Option<Vec<u8>>>>,
}

impl LotReassembler {
    pub fn new() -> Self {
        Self { segments: HashMap::new() }
    }

    /// Feed one AAS LOT packet. Returns the complete object if all segments arrived.
    pub fn feed_lot(&mut self, payload: &[u8]) -> Option<LotObject> {
        if payload.len() < 12 {
            return None;
        }
        let lot_id = u32::from_be_bytes([payload[0], payload[1], payload[2], payload[3]]);
        let total_size = u32::from_be_bytes([payload[4], payload[5], payload[6], payload[7]]);
        let seg_num = u16::from_be_bytes([payload[8], payload[9]]);
        let total_seg = u16::from_be_bytes([payload[10], payload[11]]);
        let seg_data = payload[12..].to_vec();

        let segs = self.segments.entry(lot_id).or_insert_with(|| {
            (0..total_seg).map(|_| None).collect()
        });
        if (seg_num as usize) < segs.len() {
            segs[seg_num as usize] = Some(seg_data);
        }

        if segs.iter().all(|s| s.is_some()) {
            let mut full = Vec::with_capacity(total_size as usize);
            for d in segs.iter().flatten() {
                full.extend_from_slice(d);
            }
            full.truncate(total_size as usize);
            self.segments.remove(&lot_id);
            return Some(LotObject { lot_id, total_size, data: full });
        }
        None
    }
}

impl Default for LotReassembler {
    fn default() -> Self { Self::new() }
}

/// Top-level AAS decoder orchestrator.
pub struct AasDecoder {
    pub psd: PsdData,
    pub sig: SigData,
    pub lot: LotReassembler,
    pub completed_objects: Vec<LotObject>,
}

impl AasDecoder {
    pub fn new() -> Self {
        Self {
            psd: PsdData::default(),
            sig: SigData::default(),
            lot: LotReassembler::new(),
            completed_objects: Vec::new(),
        }
    }

    /// Process parsed P1 PDUs, extracting PSD/SIG/LOT data.
    pub fn process_pdus(&mut self, pdus: &[super::frame::P1Pdu]) {
        for pdu in pdus {
            let packets = parse_aas_packets(&pdu.payload);
            for pkt in packets {
                match pkt.app_id {
                    0x0001 => self.psd = parse_psd(&pkt.payload),
                    0x0002 => self.sig = parse_sig(&pkt.payload),
                    0x0004 => {
                        if let Some(obj) = self.lot.feed_lot(&pkt.payload) {
                            self.completed_objects.push(obj);
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    /// Build an RDS-style metadata DTO from decoded PSD/SIG.
    pub fn to_metadata_dto(&self) -> RdsMetadataDto {
        let radiotext = match (&self.psd.artist, &self.psd.title) {
            (Some(a), Some(t)) => format!("{a} - {t}"),
            (Some(a), None) => a.clone(),
            (None, Some(t)) => t.clone(),
            (None, None) => String::new(),
        };
        RdsMetadataDto {
            pi_hex: None,
            callsign: self.sig.station_name.clone(),
            ps: self.sig.station_name.clone().or_else(|| self.psd.artist.clone()),
            radiotext: Some(radiotext).filter(|s| !s.is_empty()),
            pty: None,
            pty_name: None,
            tp: false,
            ta: false,
            music: true,
            groups_decoded: 0,
            blocks_dropped: 0,
            last_update_unix: None,
            first_decoded_at_unix: None,
        }
    }
}

impl Default for AasDecoder {
    fn default() -> Self { Self::new() }
}
