//! NRSC-5 AAS (Advanced Application Services) packet demux.
//!
//! Consumes the HDLC-framed AAS PDUs that the L2 frame parser emits
//! between 0x7E flag bytes and routes them to the right AAS consumer
//! (LOT / SIG / other) by port ID.
//!
//! Mirrors `aas_push` in nrsc5/src/frame.c and the port classifier in
//! `output_aas_push` (nrsc5/src/output.c). Per the canonical C, the
//! AAS layer carries no Reed-Solomon parity of its own — RS protection
//! lives at the L2 fixed-subchannel layer below. The HDLC FCS-16 alone
//! gates AAS PDUs.

#![allow(dead_code)]

use tracing::{debug, trace};

const AAS_PROTOCOL_BYTE: u8 = 0x21;
const HDLC_FLAG: u8 = 0x7E;
const HDLC_ESCAPE: u8 = 0x7D;
const FCS16_GOOD: u16 = 0xF0B8;

/// One reassembled AAS packet routed by port classification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AasPacket {
    /// LOT/stream/packet data port (0x0401..=0x50FF). The SIG table
    /// disambiguates LOT vs. stream vs. packet at the application
    /// layer; until SIG state is wired in, treat the whole range as
    /// LOT-bearing and let [`LotAssembler`](super::lot::LotAssembler)
    /// reject non-LOT payloads.
    Lot {
        port: u16,
        seq: u16,
        payload: Vec<u8>,
    },
    /// Station Information Guide (port 0x0020).
    Sig {
        port: u16,
        seq: u16,
        payload: Vec<u8>,
    },
    /// Any other port (PSD ID3 ports 0x5100 / 0x5201..=0x5207, etc.).
    Other {
        port: u16,
        seq: u16,
        payload: Vec<u8>,
    },
}

pub struct AasDemux {
    scratch: Vec<u8>,
    stream_buf: Vec<u8>,
    stream_active: bool,
}

impl AasDemux {
    pub fn new() -> Self {
        Self {
            scratch: Vec::with_capacity(8 * 1024),
            stream_buf: Vec::with_capacity(8 * 1024),
            stream_active: false,
        }
    }

    pub fn reset(&mut self) {
        self.scratch.clear();
        self.stream_buf.clear();
        self.stream_active = false;
    }

    /// Process one HDLC-bracketed AAS PDU (the bytes between the
    /// surrounding 0x7E flags, still escape-encoded with 0x7D and
    /// still carrying the protocol byte and 2-byte FCS).
    pub fn process(&mut self, pdu: &[u8]) -> Vec<AasPacket> {
        let mut out = Vec::new();
        if let Some(pkt) = self.process_one(pdu) {
            out.push(pkt);
        }
        out
    }

    /// Feed a raw byte stream that still carries 0x7E flag delimiters.
    /// Each complete inter-flag segment is dispatched as one PDU.
    pub fn feed_stream(&mut self, bytes: &[u8]) -> Vec<AasPacket> {
        let mut out = Vec::new();
        for &b in bytes {
            if b == HDLC_FLAG {
                if self.stream_active && !self.stream_buf.is_empty() {
                    let frame = std::mem::take(&mut self.stream_buf);
                    if let Some(pkt) = self.process_one(&frame) {
                        out.push(pkt);
                    }
                }
                self.stream_active = true;
                self.stream_buf.clear();
            } else if self.stream_active {
                self.stream_buf.push(b);
            }
        }
        out
    }

    fn process_one(&mut self, frame: &[u8]) -> Option<AasPacket> {
        if frame.is_empty() {
            return None;
        }
        self.scratch.clear();
        unescape_hdlc(frame, &mut self.scratch);
        if self.scratch.len() < 1 + 4 + 2 {
            trace!("AAS PDU too short ({} bytes)", self.scratch.len());
            return None;
        }
        if fcs16(&self.scratch) != FCS16_GOOD {
            trace!("AAS FCS-16 mismatch");
            return None;
        }
        if self.scratch[0] != AAS_PROTOCOL_BYTE {
            debug!("unknown AAS protocol byte 0x{:02X}", self.scratch[0]);
            return None;
        }
        let inner_end = self.scratch.len() - 2;
        let inner = &self.scratch[1..inner_end];
        if inner.len() < 4 {
            return None;
        }
        let port = u16::from_le_bytes([inner[0], inner[1]]);
        let seq = u16::from_le_bytes([inner[2], inner[3]]);
        let payload = inner[4..].to_vec();
        Some(classify(port, seq, payload))
    }
}

impl Default for AasDemux {
    fn default() -> Self {
        Self::new()
    }
}

fn classify(port: u16, seq: u16, payload: Vec<u8>) -> AasPacket {
    match port {
        0x0020 => AasPacket::Sig { port, seq, payload },
        0x0401..=0x50FF => AasPacket::Lot { port, seq, payload },
        _ => AasPacket::Other { port, seq, payload },
    }
}

fn unescape_hdlc(src: &[u8], dst: &mut Vec<u8>) {
    let mut i = 0;
    while i < src.len() {
        let b = src[i];
        if b == HDLC_ESCAPE && i + 1 < src.len() {
            dst.push(src[i + 1] ^ 0x20);
            i += 2;
        } else {
            dst.push(b);
            i += 1;
        }
    }
}

fn fcs16(buf: &[u8]) -> u16 {
    let mut crc: u16 = 0xFFFF;
    for &b in buf {
        crc = (crc >> 8) ^ FCS16_TAB[((crc ^ (b as u16)) & 0xFF) as usize];
    }
    crc
}

const FCS16_TAB: [u16; 256] = [
    0x0000, 0x1189, 0x2312, 0x329B, 0x4624, 0x57AD, 0x6536, 0x74BF, 0x8C48, 0x9DC1, 0xAF5A, 0xBED3,
    0xCA6C, 0xDBE5, 0xE97E, 0xF8F7, 0x1081, 0x0108, 0x3393, 0x221A, 0x56A5, 0x472C, 0x75B7, 0x643E,
    0x9CC9, 0x8D40, 0xBFDB, 0xAE52, 0xDAED, 0xCB64, 0xF9FF, 0xE876, 0x2102, 0x308B, 0x0210, 0x1399,
    0x6726, 0x76AF, 0x4434, 0x55BD, 0xAD4A, 0xBCC3, 0x8E58, 0x9FD1, 0xEB6E, 0xFAE7, 0xC87C, 0xD9F5,
    0x3183, 0x200A, 0x1291, 0x0318, 0x77A7, 0x662E, 0x54B5, 0x453C, 0xBDCB, 0xAC42, 0x9ED9, 0x8F50,
    0xFBEF, 0xEA66, 0xD8FD, 0xC974, 0x4204, 0x538D, 0x6116, 0x709F, 0x0420, 0x15A9, 0x2732, 0x36BB,
    0xCE4C, 0xDFC5, 0xED5E, 0xFCD7, 0x8868, 0x99E1, 0xAB7A, 0xBAF3, 0x5285, 0x430C, 0x7197, 0x601E,
    0x14A1, 0x0528, 0x37B3, 0x263A, 0xDECD, 0xCF44, 0xFDDF, 0xEC56, 0x98E9, 0x8960, 0xBBFB, 0xAA72,
    0x6306, 0x728F, 0x4014, 0x519D, 0x2522, 0x34AB, 0x0630, 0x17B9, 0xEF4E, 0xFEC7, 0xCC5C, 0xDDD5,
    0xA96A, 0xB8E3, 0x8A78, 0x9BF1, 0x7387, 0x620E, 0x5095, 0x411C, 0x35A3, 0x242A, 0x16B1, 0x0738,
    0xFFCF, 0xEE46, 0xDCDD, 0xCD54, 0xB9EB, 0xA862, 0x9AF9, 0x8B70, 0x8408, 0x9581, 0xA71A, 0xB693,
    0xC22C, 0xD3A5, 0xE13E, 0xF0B7, 0x0840, 0x19C9, 0x2B52, 0x3ADB, 0x4E64, 0x5FED, 0x6D76, 0x7CFF,
    0x9489, 0x8500, 0xB79B, 0xA612, 0xD2AD, 0xC324, 0xF1BF, 0xE036, 0x18C1, 0x0948, 0x3BD3, 0x2A5A,
    0x5EE5, 0x4F6C, 0x7DF7, 0x6C7E, 0xA50A, 0xB483, 0x8618, 0x9791, 0xE32E, 0xF2A7, 0xC03C, 0xD1B5,
    0x2942, 0x38CB, 0x0A50, 0x1BD9, 0x6F66, 0x7EEF, 0x4C74, 0x5DFD, 0xB58B, 0xA402, 0x9699, 0x8710,
    0xF3AF, 0xE226, 0xD0BD, 0xC134, 0x39C3, 0x284A, 0x1AD1, 0x0B58, 0x7FE7, 0x6E6E, 0x5CF5, 0x4D7C,
    0xC60C, 0xD785, 0xE51E, 0xF497, 0x8028, 0x91A1, 0xA33A, 0xB2B3, 0x4A44, 0x5BCD, 0x6956, 0x78DF,
    0x0C60, 0x1DE9, 0x2F72, 0x3EFB, 0xD68D, 0xC704, 0xF59F, 0xE416, 0x90A9, 0x8120, 0xB3BB, 0xA232,
    0x5AC5, 0x4B4C, 0x79D7, 0x685E, 0x1CE1, 0x0D68, 0x3FF3, 0x2E7A, 0xE70E, 0xF687, 0xC41C, 0xD595,
    0xA12A, 0xB0A3, 0x8238, 0x93B1, 0x6B46, 0x7ACF, 0x4854, 0x59DD, 0x2D62, 0x3CEB, 0x0E70, 0x1FF9,
    0xF78F, 0xE606, 0xD49D, 0xC514, 0xB1AB, 0xA022, 0x92B9, 0x8330, 0x7BC7, 0x6A4E, 0x58D5, 0x495C,
    0x3DE3, 0x2C6A, 0x1EF1, 0x0F78,
];

#[cfg(test)]
mod tests {
    use super::*;

    /// Compute the FCS field that, when appended LE, satisfies the
    /// receiver's `fcs16(frame) == FCS16_GOOD` check.
    fn fcs_field(payload: &[u8]) -> u16 {
        !fcs16(payload)
    }

    /// Build a complete HDLC frame body (no surrounding 0x7E flags) for
    /// the given AAS port/seq/payload, ready to feed into `process`.
    fn build_frame(port: u16, seq: u16, payload: &[u8]) -> Vec<u8> {
        let mut inner = Vec::new();
        inner.push(AAS_PROTOCOL_BYTE);
        inner.extend_from_slice(&port.to_le_bytes());
        inner.extend_from_slice(&seq.to_le_bytes());
        inner.extend_from_slice(payload);
        let fcs = fcs_field(&inner);
        inner.extend_from_slice(&fcs.to_le_bytes());
        assert_eq!(fcs16(&inner), FCS16_GOOD, "test FCS construction wrong");

        let mut esc = Vec::new();
        for &b in &inner {
            if b == HDLC_FLAG || b == HDLC_ESCAPE {
                esc.push(HDLC_ESCAPE);
                esc.push(b ^ 0x20);
            } else {
                esc.push(b);
            }
        }
        esc
    }

    #[test]
    fn dispatch_by_port_classification() {
        let mut dx = AasDemux::new();
        let lot_frame = build_frame(0x1001, 0, &[0xAA, 0xBB, 0xCC]);
        let sig_frame = build_frame(0x0020, 1, &[0x40, 0x01, 0x02, 0x03]);
        let psd_frame = build_frame(0x5100, 7, &[b'I', b'D', b'3']);

        let lot_out = dx.process(&lot_frame);
        let sig_out = dx.process(&sig_frame);
        let psd_out = dx.process(&psd_frame);

        assert!(matches!(
            lot_out[0],
            AasPacket::Lot {
                port: 0x1001,
                seq: 0,
                ..
            }
        ));
        assert!(matches!(
            sig_out[0],
            AasPacket::Sig {
                port: 0x0020,
                seq: 1,
                ..
            }
        ));
        assert!(matches!(
            psd_out[0],
            AasPacket::Other {
                port: 0x5100,
                seq: 7,
                ..
            }
        ));
    }

    #[test]
    fn rejects_bad_fcs() {
        let mut dx = AasDemux::new();
        let mut frame = build_frame(0x1001, 0, &[0xAA, 0xBB]);
        let last = frame.len() - 1;
        frame[last] ^= 0xFF;
        assert!(dx.process(&frame).is_empty());
    }

    #[test]
    fn rejects_wrong_protocol_byte() {
        let mut dx = AasDemux::new();
        // Build a frame with the wrong protocol byte (0x22 instead of 0x21).
        let mut inner = vec![0x22];
        inner.extend_from_slice(&0x1001u16.to_le_bytes());
        inner.extend_from_slice(&0u16.to_le_bytes());
        inner.extend_from_slice(&[0xAAu8, 0xBB]);
        let fcs = fcs_field(&inner);
        inner.extend_from_slice(&fcs.to_le_bytes());
        // FCS is valid, but proto byte should be rejected before classification.
        assert!(dx.process(&inner).is_empty());
    }

    #[test]
    fn reassembles_hdlc_escapes() {
        // Payload contains both reserved bytes so the wire form must escape them.
        let payload = [HDLC_FLAG, HDLC_ESCAPE, 0xAB, HDLC_FLAG, HDLC_ESCAPE];
        let mut dx = AasDemux::new();
        let frame = build_frame(0x1234, 0xBEEF, &payload);
        assert!(
            frame.windows(2).any(|w| w[0] == HDLC_ESCAPE),
            "expected escape sequences in wire form"
        );
        let out = dx.process(&frame);
        match &out[0] {
            AasPacket::Lot {
                port,
                seq,
                payload: pl,
            } => {
                assert_eq!(*port, 0x1234);
                assert_eq!(*seq, 0xBEEF);
                assert_eq!(pl, &payload);
            }
            other => panic!("expected LOT packet, got {other:?}"),
        }
    }

    #[test]
    fn feed_stream_segments_on_flags() {
        let mut dx = AasDemux::new();
        let f1 = build_frame(0x1001, 0, &[1, 2, 3]);
        let f2 = build_frame(0x0020, 9, &[4, 5, 6]);
        let mut stream = vec![HDLC_FLAG];
        stream.extend_from_slice(&f1);
        stream.push(HDLC_FLAG);
        stream.extend_from_slice(&f2);
        stream.push(HDLC_FLAG);
        let out = dx.feed_stream(&stream);
        assert_eq!(out.len(), 2);
        assert!(matches!(out[0], AasPacket::Lot { port: 0x1001, .. }));
        assert!(matches!(out[1], AasPacket::Sig { port: 0x0020, .. }));
    }

    #[test]
    fn empty_padding_frame_yields_no_packet() {
        let mut dx = AasDemux::new();
        assert!(dx.process(&[]).is_empty());
    }
}
