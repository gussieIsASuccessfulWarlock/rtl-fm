//! NRSC-5 Layer-2 frame parser.
//!
//! Port of `nrsc5/src/frame.c` (theori/nrsc5, GPLv3) — Rust-only.
//! Splits a descrambled + Viterbi-decoded P1 frame into:
//!   * per-program HDC audio PDUs,
//!   * per-program PSD HDLC payloads (PSD = program service data, ID3-like),
//!   * data-services (AAS) PDUs extracted from the fixed-data side.
//!
//! ## Input contract
//!
//! `process()` receives the **packed-byte** output of
//! [`crate::nrsc5::l1::extract_p1_payload`] — that is, 1 byte per 8 bits,
//! MSB-first, with the 24-bit PCI already stripped. The PCI is passed in
//! as a parameter (deviation from C, where it sits on the `frame_t`).
//!
//! ## Out of scope
//!
//! * HDC audio decoding — we expose PDU bytes only.
//!
//! Reed-Solomon correction uses [`crate::nrsc5::rs::RsDecoder`] (8-byte parity,
//! RS(255,247) shortened to (96, 88) with 159 zero pad); blocks beyond the
//! decoder's correction capacity are dropped, matching C's `fix_header` ⇒ 0
//! behaviour.
//!
//! ## Module wiring note
//!
//! Not yet registered in `mod.rs` — `#![allow(dead_code)]` keeps the
//! release build clean. See the bottom of the file for unit tests.

#![allow(dead_code)]

use crate::nrsc5::consts::P1_FRAME_LEN_FM;
use crate::nrsc5::rs::RsDecoder;

// ---- Sizing constants ------------------------------------------------------

pub const MAX_PROGRAMS: usize = 8;
pub const NUM_LOGICAL_CHANNELS: usize = 4;
pub const MAX_STREAMS: usize = 2;

pub const MAX_AAS_LEN: usize = 8212;
pub const RS_BLOCK_LEN: usize = 255;
pub const RS_CODEWORD_LEN: usize = 96;

/// Max L2 PDU size (bytes), FM mode. `(P1_FRAME_LEN_FM - PCI_LEN) / 8`.
pub const MAX_PDU_LEN: usize = (P1_FRAME_LEN_FM - 24) / 8;
/// L2 PDU size (bytes) in a P1 frame, AM mode.
pub const P1_PDU_LEN_AM: usize = 466;

const MAX_AUDIO_PACKETS: usize = 64;

// ---- PCI sentinels ---------------------------------------------------------

const PCI_AUDIO: u32 = 0x38D8D3;
const PCI_AUDIO_OPP: u32 = 0xCE3634;
const PCI_AUDIO_FIXED: u32 = 0xE3634C;
const PCI_AUDIO_FIXED_OPP: u32 = 0x8D8D33;
const PCI_FIXED: u32 = 0x3634CE;
const PCI_MASK: u32 = 0xFFFFFC;

const VALIDFCS16: u16 = 0xf0b8;

// ---- CRC8 (NRSC-5 audio packet, poly 0x31, init 0xFF) ----------------------

static CRC8_TAB: [u8; 256] = [
    0x00, 0x31, 0x62, 0x53, 0xC4, 0xF5, 0xA6, 0x97, 0xB9, 0x88, 0xDB, 0xEA, 0x7D, 0x4C, 0x1F, 0x2E,
    0x43, 0x72, 0x21, 0x10, 0x87, 0xB6, 0xE5, 0xD4, 0xFA, 0xCB, 0x98, 0xA9, 0x3E, 0x0F, 0x5C, 0x6D,
    0x86, 0xB7, 0xE4, 0xD5, 0x42, 0x73, 0x20, 0x11, 0x3F, 0x0E, 0x5D, 0x6C, 0xFB, 0xCA, 0x99, 0xA8,
    0xC5, 0xF4, 0xA7, 0x96, 0x01, 0x30, 0x63, 0x52, 0x7C, 0x4D, 0x1E, 0x2F, 0xB8, 0x89, 0xDA, 0xEB,
    0x3D, 0x0C, 0x5F, 0x6E, 0xF9, 0xC8, 0x9B, 0xAA, 0x84, 0xB5, 0xE6, 0xD7, 0x40, 0x71, 0x22, 0x13,
    0x7E, 0x4F, 0x1C, 0x2D, 0xBA, 0x8B, 0xD8, 0xE9, 0xC7, 0xF6, 0xA5, 0x94, 0x03, 0x32, 0x61, 0x50,
    0xBB, 0x8A, 0xD9, 0xE8, 0x7F, 0x4E, 0x1D, 0x2C, 0x02, 0x33, 0x60, 0x51, 0xC6, 0xF7, 0xA4, 0x95,
    0xF8, 0xC9, 0x9A, 0xAB, 0x3C, 0x0D, 0x5E, 0x6F, 0x41, 0x70, 0x23, 0x12, 0x85, 0xB4, 0xE7, 0xD6,
    0x7A, 0x4B, 0x18, 0x29, 0xBE, 0x8F, 0xDC, 0xED, 0xC3, 0xF2, 0xA1, 0x90, 0x07, 0x36, 0x65, 0x54,
    0x39, 0x08, 0x5B, 0x6A, 0xFD, 0xCC, 0x9F, 0xAE, 0x80, 0xB1, 0xE2, 0xD3, 0x44, 0x75, 0x26, 0x17,
    0xFC, 0xCD, 0x9E, 0xAF, 0x38, 0x09, 0x5A, 0x6B, 0x45, 0x74, 0x27, 0x16, 0x81, 0xB0, 0xE3, 0xD2,
    0xBF, 0x8E, 0xDD, 0xEC, 0x7B, 0x4A, 0x19, 0x28, 0x06, 0x37, 0x64, 0x55, 0xC2, 0xF3, 0xA0, 0x91,
    0x47, 0x76, 0x25, 0x14, 0x83, 0xB2, 0xE1, 0xD0, 0xFE, 0xCF, 0x9C, 0xAD, 0x3A, 0x0B, 0x58, 0x69,
    0x04, 0x35, 0x66, 0x57, 0xC0, 0xF1, 0xA2, 0x93, 0xBD, 0x8C, 0xDF, 0xEE, 0x79, 0x48, 0x1B, 0x2A,
    0xC1, 0xF0, 0xA3, 0x92, 0x05, 0x34, 0x67, 0x56, 0x78, 0x49, 0x1A, 0x2B, 0xBC, 0x8D, 0xDE, 0xEF,
    0x82, 0xB3, 0xE0, 0xD1, 0x46, 0x77, 0x24, 0x15, 0x3B, 0x0A, 0x59, 0x68, 0xFF, 0xCE, 0x9D, 0xAC,
];

fn crc8(pkt: &[u8]) -> u8 {
    let mut crc: u8 = 0xFF;
    for &b in pkt {
        crc = CRC8_TAB[(crc ^ b) as usize];
    }
    crc
}

// ---- FCS-16 (HDLC frame check sequence, poly 0x8408 reversed) --------------

static FCS_TAB: [u16; 256] = [
    0x0000, 0x1189, 0x2312, 0x329b, 0x4624, 0x57ad, 0x6536, 0x74bf, 0x8c48, 0x9dc1, 0xaf5a, 0xbed3,
    0xca6c, 0xdbe5, 0xe97e, 0xf8f7, 0x1081, 0x0108, 0x3393, 0x221a, 0x56a5, 0x472c, 0x75b7, 0x643e,
    0x9cc9, 0x8d40, 0xbfdb, 0xae52, 0xdaed, 0xcb64, 0xf9ff, 0xe876, 0x2102, 0x308b, 0x0210, 0x1399,
    0x6726, 0x76af, 0x4434, 0x55bd, 0xad4a, 0xbcc3, 0x8e58, 0x9fd1, 0xeb6e, 0xfae7, 0xc87c, 0xd9f5,
    0x3183, 0x200a, 0x1291, 0x0318, 0x77a7, 0x662e, 0x54b5, 0x453c, 0xbdcb, 0xac42, 0x9ed9, 0x8f50,
    0xfbef, 0xea66, 0xd8fd, 0xc974, 0x4204, 0x538d, 0x6116, 0x709f, 0x0420, 0x15a9, 0x2732, 0x36bb,
    0xce4c, 0xdfc5, 0xed5e, 0xfcd7, 0x8868, 0x99e1, 0xab7a, 0xbaf3, 0x5285, 0x430c, 0x7197, 0x601e,
    0x14a1, 0x0528, 0x37b3, 0x263a, 0xdecd, 0xcf44, 0xfddf, 0xec56, 0x98e9, 0x8960, 0xbbfb, 0xaa72,
    0x6306, 0x728f, 0x4014, 0x519d, 0x2522, 0x34ab, 0x0630, 0x17b9, 0xef4e, 0xfec7, 0xcc5c, 0xddd5,
    0xa96a, 0xb8e3, 0x8a78, 0x9bf1, 0x7387, 0x620e, 0x5095, 0x411c, 0x35a3, 0x242a, 0x16b1, 0x0738,
    0xffcf, 0xee46, 0xdcdd, 0xcd54, 0xb9eb, 0xa862, 0x9af9, 0x8b70, 0x8408, 0x9581, 0xa71a, 0xb693,
    0xc22c, 0xd3a5, 0xe13e, 0xf0b7, 0x0840, 0x19c9, 0x2b52, 0x3adb, 0x4e64, 0x5fed, 0x6d76, 0x7cff,
    0x9489, 0x8500, 0xb79b, 0xa612, 0xd2ad, 0xc324, 0xf1bf, 0xe036, 0x18c1, 0x0948, 0x3bd3, 0x2a5a,
    0x5ee5, 0x4f6c, 0x7df7, 0x6c7e, 0xa50a, 0xb483, 0x8618, 0x9791, 0xe32e, 0xf2a7, 0xc03c, 0xd1b5,
    0x2942, 0x38cb, 0x0a50, 0x1bd9, 0x6f66, 0x7eef, 0x4c74, 0x5dfd, 0xb58b, 0xa402, 0x9699, 0x8710,
    0xf3af, 0xe226, 0xd0bd, 0xc134, 0x39c3, 0x284a, 0x1ad1, 0x0b58, 0x7fe7, 0x6e6e, 0x5cf5, 0x4d7c,
    0xc60c, 0xd785, 0xe51e, 0xf497, 0x8028, 0x91a1, 0xa33a, 0xb2b3, 0x4a44, 0x5bcd, 0x6956, 0x78df,
    0x0c60, 0x1de9, 0x2f72, 0x3efb, 0xd68d, 0xc704, 0xf59f, 0xe416, 0x90a9, 0x8120, 0xb3bb, 0xa232,
    0x5ac5, 0x4b4c, 0x79d7, 0x685e, 0x1ce1, 0x0d68, 0x3ff3, 0x2e7a, 0xe70e, 0xf687, 0xc41c, 0xd595,
    0xa12a, 0xb0a3, 0x8238, 0x93b1, 0x6b46, 0x7acf, 0x4854, 0x59dd, 0x2d62, 0x3ceb, 0x0e70, 0x1ff9,
    0xf78f, 0xe606, 0xd49d, 0xc514, 0xb1ab, 0xa022, 0x92b9, 0x8330, 0x7bc7, 0x6a4e, 0x58d5, 0x495c,
    0x3de3, 0x2c6a, 0x1ef1, 0x0f78,
];

fn fcs16(cp: &[u8]) -> u16 {
    let mut crc: u16 = 0xFFFF;
    for &b in cp {
        crc = (crc >> 8) ^ FCS_TAB[((crc ^ b as u16) & 0xFF) as usize];
    }
    crc
}

// ---- PCI category helpers --------------------------------------------------

fn has_audio(pci: u32) -> bool {
    (pci & PCI_MASK) != (PCI_FIXED & PCI_MASK)
}

fn has_fixed(pci: u32) -> bool {
    let m = pci & PCI_MASK;
    m == (PCI_AUDIO_FIXED & PCI_MASK)
        || m == (PCI_AUDIO_FIXED_OPP & PCI_MASK)
        || m == (PCI_FIXED & PCI_MASK)
}

// ---- Bitfield-extracted header (private, mirrors `frame_header_t`) ---------

#[derive(Default, Debug, Clone, Copy)]
struct FrameHeader {
    codec_mode: u32,
    stream_id: u32,
    pdu_seq: u32,
    blend_control: u32,
    per_stream_delay: u32,
    common_delay: u32,
    latency: u32,
    pfirst: u32,
    plast: u32,
    seq: u32,
    nop: u32,
    hef: u32,
    la_location: u32,
}

fn parse_header(buf: &[u8]) -> FrameHeader {
    FrameHeader {
        codec_mode: (buf[8] & 0xf) as u32,
        stream_id: ((buf[8] >> 4) & 0x3) as u32,
        pdu_seq: ((buf[8] >> 6) as u32) | (((buf[9] & 1) as u32) << 2),
        blend_control: ((buf[9] >> 1) & 0x3) as u32,
        per_stream_delay: (buf[9] >> 3) as u32,
        common_delay: (buf[10] & 0x3f) as u32,
        latency: ((buf[10] >> 6) as u32) | (((buf[11] & 1) as u32) << 2),
        pfirst: ((buf[11] >> 1) & 1) as u32,
        plast: ((buf[11] >> 2) & 1) as u32,
        seq: ((buf[11] >> 3) as u32) | (((buf[12] & 1) as u32) << 5),
        nop: ((buf[12] >> 1) & 0x3f) as u32,
        hef: (buf[12] >> 7) as u32,
        la_location: buf[13] as u32,
    }
}

// ---- Header expansion field ------------------------------------------------

#[derive(Default, Debug, Clone, Copy)]
struct HefFields {
    class_ind: u32,
    prog_num: u32,
    pdu_len: u32,
    prog_type: u32,
    access: u32,
    applied_services: u32,
    pdu_marker: u32,
}

/// Returns (parsed fields, bytes consumed).
fn parse_hef(buf: &[u8]) -> (HefFields, usize) {
    let mut hef = HefFields::default();
    let mut i: usize = 0;
    let length = buf.len();

    loop {
        if i >= length {
            return (hef, length);
        }
        let byte = buf[i];
        match (byte >> 4) & 0x7 {
            0 => {
                hef.class_ind = (byte & 0xf) as u32;
            }
            1 => {
                hef.prog_num = ((byte >> 1) & 0x7) as u32;
                if byte & 0x1 != 0 {
                    if i + 2 >= length {
                        return (hef, length);
                    }
                    i += 1;
                    hef.pdu_len = ((buf[i] & 0x7f) as u32) << 7;
                    i += 1;
                    hef.pdu_len |= (buf[i] & 0x7f) as u32;
                }
            }
            2 => {
                if i + 1 >= length {
                    return (hef, length);
                }
                hef.access = ((byte >> 3) & 0x1) as u32;
                hef.prog_type = ((byte & 0x1) as u32) << 7;
                i += 1;
                hef.prog_type |= (buf[i] & 0x7f) as u32;
            }
            3 => {
                if byte & 0x8 != 0 {
                    if i + 4 >= length {
                        return (hef, length);
                    }
                    i += 4;
                } else {
                    if i + 3 >= length {
                        return (hef, length);
                    }
                    i += 3;
                }
            }
            4 => {
                if byte & 0x8 != 0 {
                    if i + 3 >= length {
                        return (hef, length);
                    }
                    hef.applied_services = (byte & 0x7) as u32;
                    i += 1;
                    hef.pdu_marker = ((buf[i] & 0x7f) as u32) << 14;
                    i += 1;
                    hef.pdu_marker |= ((buf[i] & 0x7f) as u32) << 7;
                    i += 1;
                    hef.pdu_marker |= (buf[i] & 0x7f) as u32;
                } else {
                    if i + 1 >= length {
                        return (hef, length);
                    }
                    i += 1;
                }
            }
            _ => {
                // Unknown class — C just logs and falls through. We do the same.
            }
        }
        // Continuation: MSB of the last byte we processed.
        if i >= length {
            return (hef, length);
        }
        let cur = buf[i];
        i += 1;
        if cur & 0x80 == 0 {
            return (hef, i);
        }
    }
}

// ---- Location-table accessors ----------------------------------------------

fn calc_lc_bits(hdr: &FrameHeader) -> u32 {
    match hdr.codec_mode {
        0 => 16,
        1 | 2 | 3 => {
            if hdr.stream_id == 0 {
                12
            } else {
                16
            }
        }
        10 | 13 => 12,
        _ => 16,
    }
}

fn calc_avg_packets(hdr: &FrameHeader) -> u32 {
    match hdr.codec_mode {
        0 => 32,
        1 | 2 | 3 => {
            if hdr.stream_id == 0 {
                4
            } else {
                32
            }
        }
        10 => {
            if hdr.stream_id == 0 {
                32
            } else {
                4
            }
        }
        13 => 4,
        _ => 32,
    }
}

fn parse_location(buf: &[u8], lc_bits: u32, i: usize) -> u32 {
    if lc_bits == 16 {
        ((buf[2 * i + 1] as u32) << 8) | (buf[2 * i] as u32)
    } else if i % 2 == 0 {
        (((buf[i / 2 * 3 + 1] & 0xf) as u32) << 8) | (buf[i / 2 * 3] as u32)
    } else {
        ((buf[i / 2 * 3 + 2] as u32) << 4) | ((buf[i / 2 * 3 + 1] >> 4) as u32)
    }
}

// ---- HDLC framing utilities ------------------------------------------------

/// In-place unescape: every 0x7D `x` becomes `(x | 0x20)` (= original byte XOR 0x20).
/// Returns the new length.
fn unescape_hdlc(data: &mut [u8]) -> usize {
    let mut w = 0usize;
    let mut r = 0usize;
    let n = data.len();
    while r < n {
        if data[r] == 0x7D && r + 1 < n {
            data[w] = data[r + 1] | 0x20;
            r += 2;
        } else {
            data[w] = data[r];
            r += 1;
        }
        w += 1;
    }
    w
}

fn sync_width(byte: u8) -> u32 {
    if byte == 0x00 {
        1
    } else if (byte >> 4) == (byte & 0xf) {
        (byte & 0xf) as u32 * 2
    } else {
        0
    }
}

// ---- Public API structs ----------------------------------------------------

#[derive(Default, Debug, Clone)]
pub struct Service {
    pub access: i32,
    pub type_: i32,
    pub codec_mode: i32,
    pub blend_control: i32,
    pub digital_audio_gain: i32,
    pub common_delay: i32,
    pub latency: i32,
}

#[derive(Default, Debug, Clone)]
pub struct CccData {
    pub fixed_ready: bool,
    pub sync_width: u32,
    pub sync_count: u32,
    pub ccc_idx: i32,
}

/// One audio HDC PDU emitted by a single frame.
///
/// Deviation: the spec sketch suggested `Vec<Vec<u8>>` indexed by program,
/// but the HDC decoder needs PDU boundaries plus the stream and sequence
/// info, so we keep them as discrete records.
#[derive(Debug, Clone)]
pub struct HdcPdu {
    pub program: u8,
    pub stream_id: u8,
    pub seq: u16,
    pub flags: u8,
    pub data: Vec<u8>,
}

pub const PACKET_FLAG_NONE: u8 = 0;
pub const PACKET_FLAG_CRC_ERROR: u8 = 1 << 0;

#[derive(Default, Debug)]
pub struct ParsedFrame {
    /// Audio PDUs extracted this call, in stream order.
    pub hdc_pdus: Vec<HdcPdu>,
    /// AAS PDU bytes (from fixed-data subchannels) for this frame, concatenated.
    pub aas_pdu: Vec<u8>,
    /// PSD HDLC payloads (with the 0x21 protocol byte AND FCS stripped),
    /// keyed by program index. May contain multiple entries per call if
    /// several frames close within this PDU.
    pub psd_payloads: Vec<(usize, Vec<u8>)>,
}

// ---- Internal per-channel CCC working set (not part of public API) ---------

#[derive(Debug, Clone)]
struct FixedSubchannel {
    mode: u16,
    length: u16,
    block_idx: usize,
    blocks: Vec<u8>, // capacity 255 + 4
    idx: i32,        // HDLC state: -1 = idle, 0+ = collecting
    data: Vec<u8>,   // capacity MAX_AAS_LEN
}

impl Default for FixedSubchannel {
    fn default() -> Self {
        Self {
            mode: 0,
            length: 0,
            block_idx: 0,
            blocks: vec![0u8; 255 + 4],
            idx: -1,
            data: vec![0u8; MAX_AAS_LEN],
        }
    }
}

#[derive(Debug, Clone)]
struct CccBuffers {
    ccc_buf: [u8; 32],
    subchannel: [FixedSubchannel; 4],
}

impl Default for CccBuffers {
    fn default() -> Self {
        Self {
            ccc_buf: [0u8; 32],
            subchannel: Default::default(),
        }
    }
}

#[derive(Debug, Clone)]
struct PsdState {
    buf: Vec<u8>, // capacity MAX_AAS_LEN
    idx: i32,     // mirrors public psd_idx[prog]
}

impl Default for PsdState {
    fn default() -> Self {
        Self {
            buf: vec![0u8; MAX_AAS_LEN],
            idx: -1,
        }
    }
}

// ---- The parser itself -----------------------------------------------------

pub struct FrameParser {
    pub pci: u32,
    pub services: [Service; MAX_PROGRAMS],
    pub psd_idx: [i32; MAX_PROGRAMS],
    pub ccc_data: [CccData; NUM_LOGICAL_CHANNELS],

    /// When `true`, [`fix_header`] is bypassed. Test-only escape hatch — the
    /// real RS path requires `RsDecoder` to gain Berlekamp-Massey correction.
    pub skip_rs_check: bool,

    // --- private working state ---
    rs_dec: RsDecoder,
    buffer: Vec<u8>,
    psd: Vec<PsdState>,        // length MAX_PROGRAMS
    ccc_bufs: Vec<CccBuffers>, // length NUM_LOGICAL_CHANNELS
    out: ParsedFrame,
}

impl Default for FrameParser {
    fn default() -> Self {
        Self::new()
    }
}

impl FrameParser {
    pub fn new() -> Self {
        let mut p = Self {
            pci: 0,
            services: Default::default(),
            psd_idx: [-1; MAX_PROGRAMS],
            ccc_data: Default::default(),
            skip_rs_check: false,
            rs_dec: RsDecoder::new(),
            buffer: Vec::with_capacity(MAX_PDU_LEN),
            psd: (0..MAX_PROGRAMS).map(|_| PsdState::default()).collect(),
            ccc_bufs: (0..NUM_LOGICAL_CHANNELS)
                .map(|_| CccBuffers::default())
                .collect(),
            out: ParsedFrame::default(),
        };
        p.reset();
        p
    }

    pub fn reset(&mut self) {
        for s in self.services.iter_mut() {
            *s = Service {
                access: -1,
                type_: -1,
                codec_mode: -1,
                blend_control: -1,
                digital_audio_gain: -1,
                common_delay: -1,
                latency: -1,
            };
        }
        self.pci = 0;
        for p in self.psd_idx.iter_mut() {
            *p = -1;
        }
        for ps in self.psd.iter_mut() {
            ps.idx = -1;
        }
        for ch in 0..NUM_LOGICAL_CHANNELS {
            self.ccc_data[ch] = CccData {
                fixed_ready: false,
                sync_width: 0,
                sync_count: 0,
                ccc_idx: -1,
            };
            // Reset per-channel CCC buffer state too.
            for sub in self.ccc_bufs[ch].subchannel.iter_mut() {
                sub.mode = 0;
                sub.length = 0;
                sub.block_idx = 0;
                sub.idx = -1;
            }
        }
    }

    /// Process one descrambled P1 frame. `payload` is the packed-byte output
    /// of `l1::extract_p1_payload`; `pci` is the 24-bit PCI already extracted
    /// from the frame; `lc` is the logical channel index (0..NUM_LOGICAL_CHANNELS).
    pub fn process(&mut self, payload: &[u8], pci: u32, lc: u8) -> ParsedFrame {
        self.pci = pci;
        self.buffer.clear();
        self.buffer.extend_from_slice(payload);

        // Take ownership of the accumulator; we'll move it back at the end.
        self.out = ParsedFrame::default();
        self.frame_process(self.buffer.len(), lc as usize);
        std::mem::take(&mut self.out)
    }

    // ---------------- internal helpers (mirror frame.c) ---------------------

    /// Corrects in place the 96-byte RS block at `buf[off..off+96]`.
    /// Returns true on a successful decode (or unconditionally in skip mode).
    /// Matches C's `fix_header`: the 96 input bytes are placed reversed into a
    /// 255-byte virtual codeword (159 zero pad up front) and then decoded;
    /// corrections inside the real-data region are written back, reversed.
    fn fix_header_block(rs_dec: &RsDecoder, buf: &mut [u8], skip_rs_check: bool) -> bool {
        if skip_rs_check {
            return true;
        }
        if buf.len() < RS_CODEWORD_LEN {
            return false;
        }
        let mut tmp = [0u8; RS_CODEWORD_LEN];
        for i in 0..RS_CODEWORD_LEN {
            tmp[i] = buf[RS_CODEWORD_LEN - 1 - i];
        }
        if rs_dec
            .decode(&mut tmp, RS_BLOCK_LEN - RS_CODEWORD_LEN)
            .is_err()
        {
            return false;
        }
        for i in 0..RS_CODEWORD_LEN {
            buf[i] = tmp[RS_CODEWORD_LEN - 1 - i];
        }
        true
    }

    fn frame_process(&mut self, length: usize, lc: usize) {
        let mut audio_end = length;

        if has_fixed(self.pci) && lc < NUM_LOGICAL_CHANNELS {
            audio_end = self.process_fixed_data(length, lc);
        }

        if !has_audio(self.pci) {
            return;
        }

        let mut offset: usize = 0;
        let loop_top_max = audio_end.saturating_sub(RS_CODEWORD_LEN);

        while offset < loop_top_max {
            let start = offset;

            if offset + RS_CODEWORD_LEN > self.buffer.len() {
                return;
            }
            let ok = Self::fix_header_block(
                &self.rs_dec,
                &mut self.buffer[offset..offset + RS_CODEWORD_LEN],
                self.skip_rs_check,
            );
            if !ok {
                // C also bumps the sync state on whole-frame failures; we
                // don't have access to sync here, so just drop the frame.
                return;
            }

            // Parse fixed 14-byte L2 header.
            if offset + 14 > self.buffer.len() {
                return;
            }
            let hdr = parse_header(&self.buffer[offset..]);
            offset += 14;
            let lc_bits = calc_lc_bits(&hdr);
            let loc_bytes = (((lc_bits * hdr.nop) + 4) / 8) as usize;

            if start + (hdr.la_location as usize) + 1 < offset + loc_bytes
                || start + (hdr.la_location as usize) >= audio_end
            {
                return;
            }

            // Parse location table.
            if offset + loc_bytes > self.buffer.len() {
                return;
            }
            let mut locations: [u32; MAX_AUDIO_PACKETS] = [0; MAX_AUDIO_PACKETS];
            let nop = hdr.nop as usize;
            if nop > MAX_AUDIO_PACKETS {
                return;
            }
            for j in 0..nop {
                let loc = parse_location(&self.buffer[offset..], lc_bits, j);
                if j == 0 && loc <= hdr.la_location {
                    return;
                }
                if j > 0 && loc <= locations[j - 1] {
                    return;
                }
                if start + (loc as usize) >= audio_end {
                    return;
                }
                locations[j] = loc;
            }
            offset += loc_bytes;

            if hdr.stream_id as usize >= MAX_STREAMS {
                // Skip past this header's data and try again from the next.
                offset = start + (locations[nop - 1] as usize) + 1;
                continue;
            }

            // Header expansion field.
            let mut hef = HefFields::default();
            if hdr.hef != 0 {
                let avail_end = audio_end.min(self.buffer.len());
                let avail = if offset < avail_end {
                    &self.buffer[offset..avail_end]
                } else {
                    &[][..]
                };
                let (h, consumed) = parse_hef(avail);
                hef = h;
                offset += consumed;
            }

            let prog = hef.prog_num as usize;
            if prog >= MAX_PROGRAMS {
                return;
            }

            if hdr.stream_id == 0 {
                let svc = &self.services[prog];
                if svc.access != hef.access as i32
                    || svc.type_ != hef.prog_type as i32
                    || svc.codec_mode != hdr.codec_mode as i32
                    || svc.blend_control != hdr.blend_control as i32
                    || svc.digital_audio_gain != hdr.per_stream_delay as i32
                    || svc.common_delay != hdr.common_delay as i32
                    || svc.latency != hdr.latency as i32
                {
                    let svc = &mut self.services[prog];
                    svc.access = hef.access as i32;
                    svc.type_ = hef.prog_type as i32;
                    svc.codec_mode = hdr.codec_mode as i32;
                    svc.blend_control = hdr.blend_control as i32;
                    svc.digital_audio_gain = hdr.per_stream_delay as i32;
                    svc.common_delay = hdr.common_delay as i32;
                    svc.latency = hdr.latency as i32;
                }
            }

            // ELASTIC_BUFFER_LEN math (kept for parity with C, even though we
            // don't currently use `seq` after this point — C feeds it through
            // to `output_push`, and downstream HDC framing will too).
            let elastic_len: u32 = 64;
            let mut seq = (elastic_len + hdr.seq.wrapping_sub(hdr.pfirst)) % elastic_len;

            // PSD HDLC bytes: [offset .. start + la_location + 1).
            let psd_end = start + hdr.la_location as usize + 1;
            if psd_end > self.buffer.len() {
                return;
            }
            if offset < psd_end {
                // Copy to avoid the parse_hdlc borrow vs &mut self conflict.
                let chunk: Vec<u8> = self.buffer[offset..psd_end].to_vec();
                self.parse_hdlc_psd(prog, &chunk);
            }
            offset = psd_end;

            // Audio packets.
            for j in 0..nop {
                let loc = locations[j] as usize;
                if start + loc < offset {
                    return;
                }
                let cnt = start + loc - offset;
                let end = offset + cnt + 1;
                if end > self.buffer.len() {
                    return;
                }
                let crc = crc8(&self.buffer[offset..end]);

                let mut flags = PACKET_FLAG_NONE;
                if crc != 0 {
                    flags |= PACKET_FLAG_CRC_ERROR;
                }

                self.out.hdc_pdus.push(HdcPdu {
                    program: prog as u8,
                    stream_id: hdr.stream_id as u8,
                    seq: seq as u16,
                    flags,
                    data: self.buffer[offset..offset + cnt].to_vec(),
                });

                offset += cnt + 1;
                seq = (seq + 1) % elastic_len;
            }
        }
    }

    // PSD HDLC: bytes-into-buffer; on 0x7E, validate and emit.
    fn parse_hdlc_psd(&mut self, prog: usize, input: &[u8]) {
        for &byte in input {
            if byte == 0x7E {
                if self.psd[prog].idx >= 0 {
                    let len = self.psd[prog].idx as usize;
                    self.psd_push(prog, len);
                }
                self.psd[prog].idx = 0;
                self.psd_idx[prog] = 0;
            } else if self.psd[prog].idx >= 0 {
                let idx = self.psd[prog].idx as usize;
                if idx == MAX_AAS_LEN {
                    self.psd[prog].idx = -1;
                    self.psd_idx[prog] = -1;
                    continue;
                }
                self.psd[prog].buf[idx] = byte;
                self.psd[prog].idx += 1;
                self.psd_idx[prog] += 1;
            }
        }
    }

    fn psd_push(&mut self, prog: usize, length: usize) {
        if length == 0 {
            return;
        }
        let n = unescape_hdlc(&mut self.psd[prog].buf[..length]);
        if n == 0 {
            return;
        }
        let frame = &self.psd[prog].buf[..n];
        if fcs16(frame) != VALIDFCS16 {
            return;
        }
        if frame[0] != 0x21 {
            return;
        }
        // Strip protocol byte (1) and FCS (2 trailing bytes).
        if n >= 3 {
            self.out.psd_payloads.push((prog, frame[1..n - 2].to_vec()));
        }
    }

    // ---- Fixed (CCC) side --------------------------------------------------

    fn process_fixed_data(&mut self, length: usize, lc: usize) -> usize {
        const BBM: [u8; 4] = [0x7D, 0x3A, 0xE2, 0x42];

        if length == 0 {
            return 0;
        }
        let mut p = length - 1;

        if self.ccc_data[lc].sync_count < 2 {
            let width = sync_width(self.buffer[p]);
            if width > 0 && self.ccc_data[lc].sync_width == width {
                self.ccc_data[lc].sync_count += 1;
            } else {
                self.ccc_data[lc].sync_count = 0;
            }
            self.ccc_data[lc].sync_width = width;

            if self.ccc_data[lc].sync_count < 2 {
                return p;
            }
        }

        let sw = self.ccc_data[lc].sync_width as usize;
        if p < sw {
            return p;
        }
        p -= sw;

        // CCC packet lives in the sync_width bytes starting at p.
        let ccc_bytes: Vec<u8> = self.buffer[p..p + sw].to_vec();
        self.parse_hdlc_ccc(lc, &ccc_bytes);

        if !self.ccc_data[lc].fixed_ready {
            return p;
        }

        for i in (0..4).rev() {
            let sub_len = self.ccc_bufs[lc].subchannel[i].length as usize;
            if sub_len == 0 {
                continue;
            }
            if p < sub_len {
                return p;
            }
            p -= sub_len;

            // Read each block byte into the subchannel block buffer; on every
            // boundary check the BBM marker and align.
            let chunk: Vec<u8> = self.buffer[p..p + sub_len].to_vec();
            for &byte in chunk.iter() {
                let sub = &mut self.ccc_bufs[lc].subchannel[i];
                let bi = sub.block_idx;
                if bi < sub.blocks.len() {
                    sub.blocks[bi] = byte;
                    sub.block_idx += 1;
                }
                if sub.block_idx == 4 && sub.blocks[..4] != BBM {
                    // Misaligned — shift left by one and retry.
                    sub.blocks.copy_within(1..4, 0);
                    sub.block_idx -= 1;
                }
                if sub.block_idx == 255 + 4 {
                    // Full block. C deinterleaves here (mode 0 only) and
                    // hands the 255 payload bytes after the BBM to HDLC.
                    let payload_start = 4usize;
                    let payload_len = 255usize;
                    let bytes: Vec<u8> =
                        sub.blocks[payload_start..payload_start + payload_len].to_vec();
                    self.parse_hdlc_subchannel(lc, i, &bytes);
                    self.ccc_bufs[lc].subchannel[i].block_idx = 0;
                }
            }
        }

        p
    }

    fn parse_hdlc_ccc(&mut self, lc: usize, input: &[u8]) {
        for &byte in input {
            if byte == 0x7E {
                if self.ccc_data[lc].ccc_idx >= 0 {
                    let len = self.ccc_data[lc].ccc_idx as usize;
                    self.ccc_push(lc, len);
                }
                self.ccc_data[lc].ccc_idx = 0;
            } else if self.ccc_data[lc].ccc_idx >= 0 {
                let idx = self.ccc_data[lc].ccc_idx as usize;
                if idx == self.ccc_bufs[lc].ccc_buf.len() {
                    self.ccc_data[lc].ccc_idx = -1;
                    continue;
                }
                self.ccc_bufs[lc].ccc_buf[idx] = byte;
                self.ccc_data[lc].ccc_idx += 1;
            }
        }
    }

    fn ccc_push(&mut self, lc: usize, length: usize) {
        if length == 0 {
            return;
        }
        // Operate on a private copy so unescape can mutate without
        // touching shared state we still need.
        let mut local = self.ccc_bufs[lc].ccc_buf[..length].to_vec();
        let n = unescape_hdlc(&mut local);
        if n == 0 {
            return;
        }
        if self.ccc_data[lc].fixed_ready {
            return; // CCC packets shouldn't change
        }
        if fcs16(&local[..n]) != VALIDFCS16 {
            return;
        }
        for i in 0..4 {
            let off = 1 + i * 4;
            let sub = &mut self.ccc_bufs[lc].subchannel[i];
            sub.mode = 0;
            sub.length = 0;
            if 5 + i * 4 <= n {
                let mode = local[off] as u16 | ((local[off + 1] as u16) << 8);
                let len = local[off + 2] as u16 | ((local[off + 3] as u16) << 8);
                if mode == 0 {
                    sub.mode = mode;
                    sub.length = len;
                    sub.block_idx = 0;
                    sub.idx = -1;
                }
            }
        }
        self.ccc_data[lc].fixed_ready = true;
    }

    fn parse_hdlc_subchannel(&mut self, lc: usize, sub_idx: usize, input: &[u8]) {
        for &byte in input {
            if byte == 0x7E {
                let idx = self.ccc_bufs[lc].subchannel[sub_idx].idx;
                if idx >= 0 {
                    let len = idx as usize;
                    self.subchannel_push(lc, sub_idx, len);
                }
                self.ccc_bufs[lc].subchannel[sub_idx].idx = 0;
            } else if self.ccc_bufs[lc].subchannel[sub_idx].idx >= 0 {
                let sub = &mut self.ccc_bufs[lc].subchannel[sub_idx];
                let idx = sub.idx as usize;
                if idx == sub.data.len() {
                    sub.idx = -1;
                    continue;
                }
                sub.data[idx] = byte;
                sub.idx += 1;
            }
        }
    }

    fn subchannel_push(&mut self, lc: usize, sub_idx: usize, length: usize) {
        if length == 0 {
            return;
        }
        let mut local = self.ccc_bufs[lc].subchannel[sub_idx].data[..length].to_vec();
        let n = unescape_hdlc(&mut local);
        if n == 0 {
            return;
        }
        if fcs16(&local[..n]) != VALIDFCS16 {
            return;
        }
        if local[0] != 0x21 {
            return;
        }
        if n >= 3 {
            self.out.aas_pdu.extend_from_slice(&local[1..n - 2]);
        }
    }
}

// ============================================================================
//                                  TESTS
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc8_basic() {
        // Empty input: initial 0xFF, no updates.
        assert_eq!(crc8(&[]), 0xFF);
        // Single zero byte: CRC8_TAB[0xFF ^ 0] = CRC8_TAB[0xFF].
        assert_eq!(crc8(&[0]), CRC8_TAB[0xFF]);
        assert_eq!(crc8(&[0]), 0xAC);
        // Round-trip: appending a "make-it-zero" byte should yield 0.
        let prefix = [0x11, 0x22, 0x33, 0x44, 0x55];
        let c = crc8(&prefix);
        let mut full = prefix.to_vec();
        full.push(c); // CRC8_TAB[0] = 0, so appending `c` gives final crc 0
        assert_eq!(crc8(&full), 0);
    }

    #[test]
    fn fcs16_known_values() {
        // Hand-derived: fcs16([]) = 0xFFFF (initial state).
        assert_eq!(fcs16(&[]), 0xFFFF);
        // fcs16([0x00]) = (0xFFFF >> 8) ^ FCS_TAB[0xFF]
        //               = 0x00FF ^ 0x0F78 = 0x0F87.
        assert_eq!(fcs16(&[0]), 0x00FF ^ 0x0F78);
    }

    #[test]
    fn fcs16_hdlc_round_trip() {
        // Build a payload, append the HDLC FCS, verify it yields VALIDFCS16.
        let payload = [0x21u8, 0xDE, 0xAD];
        let fcs = !fcs16(&payload);
        let mut framed = payload.to_vec();
        framed.push((fcs & 0xFF) as u8);
        framed.push((fcs >> 8) as u8);
        assert_eq!(fcs16(&framed), VALIDFCS16);
    }

    #[test]
    fn unescape_hdlc_basic() {
        let mut data = [0x7D, 0x5E, 0x42];
        let n = unescape_hdlc(&mut data);
        assert_eq!(n, 2);
        assert_eq!(&data[..n], &[0x7E, 0x42]);

        let mut data2 = [0xAA, 0xBB];
        let n = unescape_hdlc(&mut data2);
        assert_eq!(n, 2);
        assert_eq!(&data2[..n], &[0xAA, 0xBB]);

        // Trailing 0x7D with no follow-up byte: passes through.
        let mut data3 = [0x7D];
        let n = unescape_hdlc(&mut data3);
        assert_eq!(n, 1);
        assert_eq!(data3[0], 0x7D);
    }

    #[test]
    fn sync_width_cases() {
        assert_eq!(sync_width(0x00), 1);
        assert_eq!(sync_width(0x11), 2);
        assert_eq!(sync_width(0x44), 8);
        assert_eq!(sync_width(0xFF), 30);
        assert_eq!(sync_width(0x12), 0);
    }

    #[test]
    fn has_audio_has_fixed() {
        // PCI_AUDIO: audio yes, fixed no.
        assert!(has_audio(0x38D8D3));
        assert!(!has_fixed(0x38D8D3));
        // PCI_AUDIO_OPP: audio yes, fixed no.
        assert!(has_audio(0xCE3634));
        assert!(!has_fixed(0xCE3634));
        // PCI_AUDIO_FIXED: audio yes, fixed yes.
        assert!(has_audio(0xE3634C));
        assert!(has_fixed(0xE3634C));
        // PCI_FIXED: audio no, fixed yes.
        assert!(!has_audio(0x3634CE));
        assert!(has_fixed(0x3634CE));
        // Low 2 bits ignored.
        assert!(has_audio(0x38D8D3 | 0x3));
    }

    #[test]
    fn parse_header_fields() {
        // Construct a header byte by byte and verify each bitfield.
        // codec_mode=1, stream_id=0, pdu_seq=0:
        //   buf[8] = 0b00_00_0001 = 0x01
        // pdu_seq[2]=0, blend_control=0, per_stream_delay=0:
        //   buf[9] = 0
        // common_delay=0, latency[1:0]=0:
        //   buf[10] = 0
        // latency[2]=0, pfirst=0, plast=0, seq[4:0]=0:
        //   buf[11] = 0
        // seq[5]=0, nop=1, hef=1:
        //   buf[12] = 0b1_000001_0 = 0x82
        // la_location = 16:
        //   buf[13] = 16
        let mut buf = [0u8; 14];
        buf[8] = 0x01;
        buf[12] = 0x82;
        buf[13] = 16;
        let hdr = parse_header(&buf);
        assert_eq!(hdr.codec_mode, 1);
        assert_eq!(hdr.stream_id, 0);
        assert_eq!(hdr.pdu_seq, 0);
        assert_eq!(hdr.blend_control, 0);
        assert_eq!(hdr.per_stream_delay, 0);
        assert_eq!(hdr.common_delay, 0);
        assert_eq!(hdr.latency, 0);
        assert_eq!(hdr.pfirst, 0);
        assert_eq!(hdr.plast, 0);
        assert_eq!(hdr.seq, 0);
        assert_eq!(hdr.nop, 1);
        assert_eq!(hdr.hef, 1);
        assert_eq!(hdr.la_location, 16);

        // Now flip several fields to test the boundary-spanning ones.
        // pdu_seq = 0b110 = 6. low 2 bits in buf[8] high bits, bit 2 in buf[9] low.
        let mut buf = [0u8; 14];
        // codec_mode=0, stream_id=1, pdu_seq_lo=0b10:
        //   buf[8] = 0b10_01_0000 = 0x90
        buf[8] = 0x90;
        // pdu_seq_hi=1, blend_control=0b10, per_stream_delay=0b10101:
        //   buf[9] = 0b10101_10_1 = 0xAD
        buf[9] = 0xAD;
        let hdr = parse_header(&buf);
        assert_eq!(hdr.codec_mode, 0);
        assert_eq!(hdr.stream_id, 1);
        assert_eq!(hdr.pdu_seq, 0b110);
        assert_eq!(hdr.blend_control, 0b10);
        assert_eq!(hdr.per_stream_delay, 0b10101);
    }

    #[test]
    fn parse_hef_class_indication() {
        // Class 0 with class_ind=5, no continuation.
        // byte = 0b0 000 0101 = 0x05
        let buf = [0x05u8];
        let (h, n) = parse_hef(&buf);
        assert_eq!(n, 1);
        assert_eq!(h.class_ind, 5);
    }

    #[test]
    fn parse_hef_program_info() {
        // Class 1 with prog_num=3, no extension, no continuation.
        // byte = 0b0_001_011_0 = 0x16  (top 4 bits: 0001 → class 1)
        // Wait: layout is `(byte >> 4) & 0x7` == 1, then `(byte >> 1) & 0x7` is prog_num,
        // and `byte & 0x1` is the extension bit.
        // prog_num=3 → bits 3:1 = 011, extension=0 → bit 0 = 0
        // class=1 → bits 6:4 = 001 (bit 7 = continuation = 0)
        // byte = 0b 0 001 011 0 = 0x16
        let buf = [0x16u8];
        let (h, n) = parse_hef(&buf);
        assert_eq!(n, 1);
        assert_eq!(h.prog_num, 3);
        assert_eq!(h.pdu_len, 0);
    }

    #[test]
    fn parse_hef_program_type_access() {
        // Class 2 (prog type), access=1, prog_type=0x42 (bit7 from first byte bit0, low7 from byte2).
        // byte0: class bits 6:4 = 010, access bit 3 = 1, reserved, prog_type bit7 → bit 0
        // We want prog_type=0x42 → bit 7 = 0, low 7 bits = 0x42 (0b1000010).
        // byte0 = 0b 0 010 1 000 = 0x28  (continuation=0)
        // byte1 = 0x42 — but the while-condition reads byte1's MSB. 0x42 & 0x80 = 0 → exit OK.
        let buf = [0x28u8, 0x42];
        let (h, n) = parse_hef(&buf);
        assert_eq!(n, 2);
        assert_eq!(h.access, 1);
        assert_eq!(h.prog_type, 0x42);
    }

    #[test]
    fn parse_location_12_and_16() {
        // 16-bit, little-endian pairs.
        let buf = [0x34, 0x12, 0x78, 0x56];
        assert_eq!(parse_location(&buf, 16, 0), 0x1234);
        assert_eq!(parse_location(&buf, 16, 1), 0x5678);

        // 12-bit packed: every 2 entries fit in 3 bytes.
        //   loc[0] = buf[0] | (buf[1] & 0xf) << 8
        //   loc[1] = (buf[2] << 4) | (buf[1] >> 4)
        // Pick loc[0]=0x123, loc[1]=0x456.
        //   buf[0] = 0x23, buf[1] low nibble = 0x1, buf[1] high nibble = 0x6, buf[2] = 0x45
        let buf = [0x23u8, 0x61, 0x45];
        assert_eq!(parse_location(&buf, 12, 0), 0x123);
        assert_eq!(parse_location(&buf, 12, 1), 0x456);
    }

    #[test]
    fn calc_helpers() {
        let mut hdr = FrameHeader::default();
        hdr.codec_mode = 0;
        assert_eq!(calc_lc_bits(&hdr), 16);
        assert_eq!(calc_avg_packets(&hdr), 32);

        hdr.codec_mode = 1;
        hdr.stream_id = 0;
        assert_eq!(calc_lc_bits(&hdr), 12);
        assert_eq!(calc_avg_packets(&hdr), 4);
        hdr.stream_id = 1;
        assert_eq!(calc_lc_bits(&hdr), 16);
        assert_eq!(calc_avg_packets(&hdr), 32);

        hdr.codec_mode = 10;
        hdr.stream_id = 0;
        assert_eq!(calc_lc_bits(&hdr), 12);
        assert_eq!(calc_avg_packets(&hdr), 32);
        hdr.stream_id = 1;
        assert_eq!(calc_avg_packets(&hdr), 4);

        hdr.codec_mode = 13;
        assert_eq!(calc_lc_bits(&hdr), 12);
        assert_eq!(calc_avg_packets(&hdr), 4);
    }

    #[test]
    fn process_returns_empty_when_not_audio() {
        let mut p = FrameParser::new();
        // PCI_FIXED → has_audio=false, has_fixed=true. No fixed data
        // present (sync_count stays at 0), so output is empty.
        let buf = vec![0u8; 200];
        let out = p.process(&buf, 0x3634CE, 0);
        assert!(out.hdc_pdus.is_empty());
        assert!(out.aas_pdu.is_empty());
        assert!(out.psd_payloads.is_empty());
    }

    #[test]
    fn process_minimal_audio_frame() {
        // End-to-end: hand-build a one-program, one-packet P1 frame and
        // verify the HDC PDU plus the Service-table update.
        //
        // Layout (offsets within buffer):
        //   [0..14)  L2 header
        //   [14..16) location table (12-bit × 1 packet → 2 bytes)
        //   [16..17) HEF: class 1, prog_num=0, no extension
        //   [17..31) audio packet (cnt=13 + 1 CRC = 14 bytes)
        //   [31..127) filler
        // la_location = 16 → PSD region is empty.
        // Buffer length 127 → audio_end - 96 = 31 → loop exits after one pass.
        let mut buf = vec![0u8; 127];

        // Header (codec_mode=1, stream_id=0, nop=1, hef=1, la_location=16).
        buf[8] = 0x01;
        buf[12] = 0x82;
        buf[13] = 16;

        // Location table: loc[0] = 30. 12-bit: buf[14]=30, buf[15]=0.
        buf[14] = 30;
        buf[15] = 0;

        // HEF: class=1, prog_num=0, no extension, no continuation → 0x10.
        buf[16] = 0x10;

        // Audio packet: 13 arbitrary bytes + 1 CRC byte to make crc8 == 0.
        for i in 0..13 {
            buf[17 + i] = (i as u8).wrapping_add(0x40);
        }
        let c = crc8(&buf[17..17 + 13]);
        buf[30] = c; // CRC8_TAB[0] = 0, so appending `c` (== running crc) drives final to 0.
        assert_eq!(crc8(&buf[17..31]), 0);

        let mut p = FrameParser::new();
        p.skip_rs_check = true;

        // PCI_AUDIO_OPP → has_audio=true, has_fixed=false.
        let out = p.process(&buf, 0xCE3634, 0);

        // One PDU expected.
        assert_eq!(out.hdc_pdus.len(), 1);
        let pdu = &out.hdc_pdus[0];
        assert_eq!(pdu.program, 0);
        assert_eq!(pdu.stream_id, 0);
        assert_eq!(pdu.flags, PACKET_FLAG_NONE);
        // The "data" slice is cnt = 13 bytes; the 14th (CRC) is consumed but not included.
        assert_eq!(pdu.data.len(), 13);
        assert_eq!(pdu.data, buf[17..30].to_vec());

        // Service updated from defaults (-1) to actual values.
        let svc = &p.services[0];
        assert_eq!(svc.codec_mode, 1);
        assert_eq!(svc.access, 0);
        assert_eq!(svc.type_, 0);
        assert_eq!(svc.blend_control, 0);
        assert_eq!(svc.common_delay, 0);
        assert_eq!(svc.latency, 0);
        assert_eq!(svc.digital_audio_gain, 0);

        // No PSD, no fixed data.
        assert!(out.aas_pdu.is_empty());
        assert!(out.psd_payloads.is_empty());
    }

    #[test]
    fn process_crc_error_flagged() {
        // Same layout as `process_minimal_audio_frame`, but corrupt the CRC byte.
        let mut buf = vec![0u8; 127];
        buf[8] = 0x01;
        buf[12] = 0x82;
        buf[13] = 16;
        buf[14] = 30;
        buf[16] = 0x10;
        for i in 0..13 {
            buf[17 + i] = (i as u8).wrapping_add(0x40);
        }
        // Intentionally wrong CRC.
        buf[30] = 0xAA;

        let mut p = FrameParser::new();
        p.skip_rs_check = true;
        let out = p.process(&buf, 0xCE3634, 0);
        assert_eq!(out.hdc_pdus.len(), 1);
        assert_eq!(
            out.hdc_pdus[0].flags & PACKET_FLAG_CRC_ERROR,
            PACKET_FLAG_CRC_ERROR
        );
    }
}
