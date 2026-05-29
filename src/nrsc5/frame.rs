//! L1 PDU framing: parse service-frame PDUs from the P1 logical channel.

use crate::nrsc5::consts::RS_K;

/// CRC-16-CCITT (x^16 + x^12 + x^5 + 1).
fn crc16(data: &[u8]) -> u16 {
    let mut crc: u16 = 0xFFFF;
    for &byte in data {
        crc ^= (byte as u16) << 8;
        for _ in 0..8 {
            if crc & 0x8000 != 0 {
                crc = (crc << 1) ^ 0x1021;
            } else {
                crc <<= 1;
            }
        }
    }
    crc ^ 0xFFFF
}

/// Parsed L1 PDU.
#[derive(Debug, Clone)]
pub struct P1Pdu {
    pub port: u8,
    pub flags: u8,
    pub payload: Vec<u8>,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct FrameStats {
    pub rs_ok: u64,
    pub rs_fail: u64,
    pub pdu_crc_ok: u64,
    pub pdu_crc_fail: u64,
}

/// Attempt to parse one or more P1 PDUs from decoded RS data.
/// Returns the parsed PDUs and any unconsumed bytes.
pub fn parse_p1_pdus(data: &[u8]) -> Vec<P1Pdu> {
    let (pdus, _) = parse_p1_pdus_with_stats(data);
    pdus
}

fn parse_p1_pdus_with_stats(data: &[u8]) -> (Vec<P1Pdu>, FrameStats) {
    let mut pdus = Vec::new();
    let mut stats = FrameStats::default();
    let mut off = 0;
    while off + 6 <= data.len() {
        let port = data[off];
        let flags = data[off + 1];
        let length = u16::from_be_bytes([data[off + 2], data[off + 3]]) as usize;
        if off + 4 + length + 2 > data.len() {
            break;
        }
        let payload = data[off + 4..off + 4 + length].to_vec();
        let crc_bytes = [data[off + 4 + length], data[off + 4 + length + 1]];
        let expected_crc = u16::from_be_bytes(crc_bytes);
        let calc_crc = crc16(&data[off..off + 4 + length]);
        if calc_crc == expected_crc {
            pdus.push(P1Pdu { port, flags, payload });
            stats.pdu_crc_ok += 1;
        } else {
            stats.pdu_crc_fail += 1;
        }
        off += 4 + length + 2;
    }
    (pdus, stats)
}

/// Service frame decoder: handles one frame of received symbols.
pub struct FrameDecoder {
    rs_buffer: Vec<u8>,
    rs_blocks: Vec<Vec<u8>>,
    stats: FrameStats,
}

impl FrameDecoder {
    pub fn new() -> Self {
        Self {
            rs_buffer: Vec::with_capacity(RS_K * 32),
            rs_blocks: Vec::new(),
            stats: FrameStats::default(),
        }
    }

    pub fn stats(&self) -> FrameStats {
        self.stats
    }

    /// Feed decoded soft bits from one service frame (32 symbols).
    /// Returns parsed PDUs.
    pub fn process_frame(&mut self, decoded_bits: &[u8], out_pdus: &mut Vec<P1Pdu>) {
        self.rs_buffer.clear();
        self.rs_buffer.extend_from_slice(decoded_bits);
        self.stats = FrameStats::default();

        // Depuncture and form RS codewords
        self.rs_blocks.clear();
        for chunk in self.rs_buffer.chunks(255) {
            if chunk.len() < 255 {
                break;
            }
            self.rs_blocks.push(chunk.to_vec());
        }

        for block in self.rs_blocks.iter_mut() {
            if let Ok(decoded) = crate::nrsc5::rs::decode_rs(block) {
                self.stats.rs_ok += 1;
                let (pdus, pdu_stats) = parse_p1_pdus_with_stats(decoded);
                self.stats.pdu_crc_ok += pdu_stats.pdu_crc_ok;
                self.stats.pdu_crc_fail += pdu_stats.pdu_crc_fail;
                out_pdus.extend(pdus);
            } else {
                self.stats.rs_fail += 1;
            }
        }
    }
}

impl Default for FrameDecoder {
    fn default() -> Self { Self::new() }
}
