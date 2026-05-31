//! NRSC-5 Layer-1 → Layer-2 handoff: PCI extraction + payload packing.
//!
//! Port of `frame_push` in theori/nrsc5 `src/frame.c` (GPLv3, re-implemented
//! in Rust). After Viterbi decode + descrambling, the P1 logical-channel
//! frame is `P1_FRAME_LEN_FM` soft-decided **bits** (one bit per `u8`, value
//! 0/1). This module splits that bitstream into:
//!
//!   * the 24-bit **PCI** (Primary service Control Information / protocol
//!     class indicator), whose bits are *interspersed* through the frame —
//!     NOT a contiguous header — starting at `P1_FRAME_LEN_FM - 30000` and
//!     repeating every `offset = 1248` bits, and
//!   * the **payload**, every other bit packed MSB-first into bytes, fed
//!     verbatim to [`crate::nrsc5::frame::FrameParser::process`].
//!
//! The C reference also performs a per-8-bit-group bit-order swap before the
//! split (`bits[byte_start + byte_len - 1 - (i & 7)]`); we reproduce it
//! exactly so the packed bytes line up with the L2 parser's expectations.

use crate::nrsc5::consts::P1_FRAME_LEN_FM;

/// Number of payload bytes emitted for an FM P1 frame:
/// `(P1_FRAME_LEN_FM - 24) / 8`.
pub const P1_PAYLOAD_BYTES_FM: usize = (P1_FRAME_LEN_FM - 24) / 8;

/// Split a descrambled P1 frame `bits` (one bit per `u8`) into the 24-bit
/// PCI (returned) and the packed payload bytes (appended to `payload`,
/// which is cleared first). If `bits` is shorter than a full frame the
/// function returns 0 and leaves `payload` empty.
pub fn extract_p1_payload(bits: &[u8], payload: &mut Vec<u8>) -> u32 {
    payload.clear();
    let length = P1_FRAME_LEN_FM;
    if bits.len() < length {
        return 0;
    }

    // FM P1 PCI geometry from frame.c's `switch (length)`.
    let start = length - 30000;
    let offset = 1248usize;
    let pci_len = 24usize;

    payload.reserve(P1_PAYLOAD_BYTES_FM);

    let mut header: u32 = 0;
    let mut val: u8 = 0;
    let mut j = 0usize; // bit position within the current payload byte
    let mut h = 0usize; // PCI bits gathered so far

    for i in 0..length {
        // Swap bit order within each 8-bit group (last group may be short).
        let byte_start = (i >> 3) << 3;
        let byte_len = (length - byte_start).min(8);
        let bit = bits[byte_start + byte_len - 1 - (i & 7)] & 1;

        if i >= start && (i - start) % offset == 0 && h < pci_len {
            header |= (bit as u32) << (23 - h);
            h += 1;
        } else {
            val |= bit << (7 - j);
            j += 1;
            if j == 8 {
                payload.push(val);
                val = 0;
                j = 0;
            }
        }
    }

    header
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn payload_byte_count_matches_reference() {
        let bits = vec![0u8; P1_FRAME_LEN_FM];
        let mut payload = Vec::new();
        let pci = extract_p1_payload(&bits, &mut payload);
        assert_eq!(pci, 0);
        assert_eq!(payload.len(), P1_PAYLOAD_BYTES_FM);
    }

    #[test]
    fn short_input_yields_empty() {
        let bits = vec![1u8; P1_FRAME_LEN_FM - 1];
        let mut payload = vec![0xAA];
        let pci = extract_p1_payload(&bits, &mut payload);
        assert_eq!(pci, 0);
        assert!(payload.is_empty());
    }

    /// All-ones input: every PCI slot collects a 1, so all 24 PCI bits set.
    #[test]
    fn all_ones_sets_full_pci() {
        let bits = vec![1u8; P1_FRAME_LEN_FM];
        let mut payload = Vec::new();
        let pci = extract_p1_payload(&bits, &mut payload);
        assert_eq!(pci, 0xFFFFFF);
        // Every emitted payload bit is 1 → 0xFF bytes.
        assert!(payload.iter().all(|&b| b == 0xFF));
    }
}
