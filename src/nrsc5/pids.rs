//! NRSC-5 PIDS / SIS (Station Information Service) decoder.
//!
//! Port of the relevant parts of theori/nrsc5 `src/pids.c` (GPLv3,
//! re-implemented in Rust). Each PIDS frame is 80 bits, decoded once per L1
//! block from the PIDS logical channel. After a per-byte bit-order swap and
//! a CRC-12 gate, an SIS frame (`type == 0`) carries one or two payloads;
//! we extract the **short station name** (msg id 1), the four-character
//! call-sign-style identifier shown next to album art.
//!
//! Album art does not flow through PIDS — this only supplies the station
//! name surfaced alongside it — so non-name SIS payloads are skipped (their
//! fixed widths are still consumed so a second payload in the same frame
//! parses correctly).

#![allow(dead_code)]

const PIDS_FRAME_LEN: usize = 80;
const PIDS_TYPE_SIS: u8 = 0;

/// 5-bit character alphabet for short names (pids.c: `chars`).
const CHARS5: &[u8; 32] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ ?-*$ ";

/// Per-msg-id SIS payload width in bits (pids.c `payload_sizes`); -1 marks
/// an undefined id, which aborts the frame.
const PAYLOAD_SIZES: [i32; 16] = [
    32, 22, 58, 32, 27, 58, 27, 22, 58, 58, 27, -1, -1, -1, -1, -1,
];

#[derive(Default)]
pub struct PidsDecoder {
    /// Most recent decoded short station name (e.g. "KSYM" / "WXYZ-FM").
    pub station_name: String,
    /// Count of CRC-valid SIS frames seen (diagnostics).
    pub valid_frames: u64,
}

impl PidsDecoder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn reset(&mut self) {
        self.station_name.clear();
        self.valid_frames = 0;
    }

    /// Process one 80-bit PIDS frame (one bit per `u8`, value 0/1), already
    /// descrambled. Updates [`Self::station_name`] when a new short name
    /// decodes.
    pub fn process(&mut self, bits: &[u8]) {
        if bits.len() < PIDS_FRAME_LEN {
            return;
        }

        // Swap bit order within each 8-bit group (pids.c `pids_frame_push`).
        let mut pids = [0u8; PIDS_FRAME_LEN];
        for i in 0..PIDS_FRAME_LEN {
            pids[i] = bits[((i >> 3) << 3) + 7 - (i & 7)] & 1;
        }

        if !check_crc12(&pids) {
            return;
        }
        self.valid_frames += 1;

        if pids[0] == PIDS_TYPE_SIS {
            self.sis_decode(&pids[1..]);
        }
    }

    /// Decode an SIS payload region (pids.c `sis_decode`). `bits` begins at
    /// PIDS bit 1; it spans the 67-bit SIS field (indices 0..67 here).
    fn sis_decode(&mut self, bits: &[u8]) {
        let payloads = bits[0] as usize + 1;
        let mut off: usize = 1;

        for _ in 0..payloads {
            if off > 59 {
                break;
            }
            let msg_id = decode_int(bits, &mut off, 4) as usize;
            let payload_size = PAYLOAD_SIZES[msg_id & 0xf];
            if payload_size < 0 {
                break;
            }
            if off as i32 > 63 - payload_size {
                break;
            }

            if msg_id == 1 {
                // SIS_MSG_ID_STATION_NAME_SHORT
                self.decode_station_name_short(&bits[off..]);
            }
            // All msg ids (decoded or not) consume their fixed width.
            off += payload_size as usize;
        }
    }

    /// pids.c `sis_decode_station_name_short`: four 5-bit chars, then a
    /// 2-bit FM-suffix indicator (`0b01` ⇒ append "-FM").
    fn decode_station_name_short(&mut self, bits: &[u8]) {
        if bits.len() < 22 {
            return;
        }
        let mut name = String::with_capacity(8);
        let mut off = 0usize;
        for _ in 0..4 {
            let idx = decode_int(bits, &mut off, 5) as usize;
            name.push(CHARS5[idx] as char);
        }
        if bits[off] == 0 && bits[off + 1] == 1 {
            name.push_str("-FM");
        }
        // Trim the space-padding the 5-bit alphabet uses for short names.
        let trimmed = name.trim().to_string();
        if !trimmed.is_empty() && trimmed != self.station_name {
            self.station_name = trimmed;
        }
    }
}

/// MSB-first integer decode (pids.c `decode_int`).
fn decode_int(bits: &[u8], off: &mut usize, length: usize) -> u32 {
    let mut result = 0u32;
    for _ in 0..length {
        result = (result << 1) | (bits[*off] & 1) as u32;
        *off += 1;
    }
    result
}

/// CRC-12 over the first 68 PIDS bits (pids.c `crc12`).
fn crc12(bits: &[u8; PIDS_FRAME_LEN]) -> u16 {
    let poly = 0xD010u16;
    let mut reg = 0u16;
    for i in (0..68).rev() {
        let lowbit = reg & 1;
        reg >>= 1;
        reg ^= (bits[i] as u16) << 15;
        if lowbit != 0 {
            reg ^= poly;
        }
    }
    for _ in 0..16 {
        let lowbit = reg & 1;
        reg >>= 1;
        if lowbit != 0 {
            reg ^= poly;
        }
    }
    reg ^= 0x955;
    reg & 0xfff
}

/// Compare the transmitted CRC (bits 68..80) against the computed one.
fn check_crc12(bits: &[u8; PIDS_FRAME_LEN]) -> bool {
    let mut expected = 0u16;
    for i in 68..80 {
        expected = (expected << 1) | (bits[i] & 1) as u16;
    }
    expected == crc12(bits)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build an 80-bit PIDS frame (pre-bit-swap) carrying a single SIS
    /// short-name payload for `name`, with a valid CRC-12.
    fn build_short_name_frame(name: &[u8; 4], fm: bool) -> Vec<u8> {
        // Assemble the logical (post-swap) bit array `pids`, then invert the
        // bit-swap to produce the on-wire ordering `process` expects.
        let mut pids = [0u8; PIDS_FRAME_LEN];
        let mut idx = 0usize;
        let mut put = |val: u32, len: usize, pids: &mut [u8; 80], idx: &mut usize| {
            for k in (0..len).rev() {
                pids[*idx] = ((val >> k) & 1) as u8;
                *idx += 1;
            }
        };
        // type = SIS (1 bit)
        put(0, 1, &mut pids, &mut idx);
        // SIS region: payloads-1 (1 bit) = 0 → 1 payload
        put(0, 1, &mut pids, &mut idx);
        // msg_id = 1 (short name), 4 bits
        put(1, 4, &mut pids, &mut idx);
        // four 5-bit chars
        for &c in name {
            let pos = CHARS5.iter().position(|&x| x == c).unwrap() as u32;
            put(pos, 5, &mut pids, &mut idx);
        }
        // 2-bit FM indicator: 0b01 ⇒ "-FM"
        put(if fm { 0b01 } else { 0b00 }, 2, &mut pids, &mut idx);

        // CRC-12 over bits 0..68 → store in bits 68..80.
        let crc = crc12(&pids);
        for k in (0..12).rev() {
            pids[68 + (11 - k)] = ((crc >> k) & 1) as u8;
        }

        // Invert the per-byte bit swap so `process` recovers `pids`.
        let mut wire = vec![0u8; PIDS_FRAME_LEN];
        for i in 0..PIDS_FRAME_LEN {
            wire[((i >> 3) << 3) + 7 - (i & 7)] = pids[i];
        }
        wire
    }

    #[test]
    fn decodes_short_name_with_fm() {
        let mut dec = PidsDecoder::new();
        let frame = build_short_name_frame(b"KSYM", true);
        dec.process(&frame);
        assert_eq!(dec.valid_frames, 1);
        assert_eq!(dec.station_name, "KSYM-FM");
    }

    #[test]
    fn rejects_corrupted_crc() {
        let mut dec = PidsDecoder::new();
        let mut frame = build_short_name_frame(b"WXYZ", false);
        frame[40] ^= 1; // flip a payload bit; CRC no longer matches
        dec.process(&frame);
        assert_eq!(dec.valid_frames, 0);
        assert!(dec.station_name.is_empty());
    }
}
