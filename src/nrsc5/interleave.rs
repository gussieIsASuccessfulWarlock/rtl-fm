//! P1 PM block deinterleaver for NRSC-5 FM.
//!
//! Ports `interleaver_i` from theori/nrsc5 src/decode.c (J=20, B=16,
//! C=36, M=1) and inserts depuncture zeros every 6th output to undo
//! the rate 1/3 → 2/5 puncturing.

use crate::nrsc5::consts::{BLKSZ, P1_FRAME_LEN_ENCODED_FM};

const J: usize = 20;
const B: usize = 16;
const C: usize = 36;

/// PM_V partition permutation (decode.c:34).
const PM_V: [u8; 20] = [
    10, 2, 18, 6, 14, 8, 16, 0, 12, 4, 11, 3, 19, 7, 15, 9, 17, 1, 13, 5,
];

/// Input: 16 × PM_BLOCK_SIZE = 368640 soft bits accumulated across one
/// P1 frame. Output: depunctured rate-1/3 stream of length
/// P1_FRAME_LEN_FM × 3 = 438528, ready for Viterbi.
pub fn deinterleave_p1_fm(pm_buf: &[i8], out: &mut Vec<i8>) {
    out.clear();
    let mut written = 0usize;
    for i in 0..P1_FRAME_LEN_ENCODED_FM {
        let partition = PM_V[i % 20] as usize;
        let block = ((i / J) + partition * 7) % B;
        let k = i / (J * B);
        let row = (k * 11) % BLKSZ;
        let column = (k * 11 + k / (BLKSZ * 9)) % C;
        let idx = block * BLKSZ * J * C + row * J * C + partition * C + column;
        let v = if idx < pm_buf.len() { pm_buf[idx] } else { 0 };
        out.push(v);
        written += 1;
        if written % 6 == 5 {
            out.push(0);
            written += 1;
        }
    }
}

pub fn deinterleave_pids_fm(pm_buf: &[i8], viterbi: &mut Vec<i8>, bc: usize) {
    let b = 200;
    let i0 = 365440;
    let j_val = 20;
    let b_val = 16;
    let c_val = 36;

    viterbi.clear();
    for i in (bc * b)..((bc + 1) * b) {
        let partition = PM_V[i % 20] as usize;
        let block = i / b;
        let k = ((i / j_val) % (b / j_val)) + (i0 / (j_val * b_val));
        let row = (k * 11) % 32;
        let column = (k * 11 + k / (32 * 9)) % c_val;
        viterbi.push(pm_buf[(block * 32 + row) * (j_val * c_val) + partition * c_val + column]);
        if viterbi.len() % 6 == 5 {
            viterbi.push(0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nrsc5::consts::P1_FRAME_LEN_FM;

    /// The puncture pattern is `{1,1,1,1,1,0}` (decode.c:263), so every
    /// 6th element of the depunctured output (indices 5, 11, 17, …) must
    /// be the inserted zero. The five preceding elements come from
    /// pm_buf via `interleaver_i` — when we fill pm_buf with all `+1`,
    /// every non-puncture slot must read `+1`.
    #[test]
    fn deinterleave_p1_inserts_zero_at_every_sixth() {
        let pm = vec![1i8; 16 * 32 * 720];
        let mut out = Vec::with_capacity(P1_FRAME_LEN_FM * 3);
        deinterleave_p1_fm(&pm, &mut out);

        assert_eq!(out.len(), P1_FRAME_LEN_FM * 3);
        for (i, &v) in out.iter().enumerate() {
            if i % 6 == 5 {
                assert_eq!(v, 0, "expected puncture zero at index {}", i);
            } else {
                assert_eq!(v, 1, "expected data bit (+1) at index {}", i);
            }
        }
    }

    /// Same test as `decode_process_pids` for PIDS deinterleaver,
    /// excluding the trailing wraparound: depunctured stream length is
    /// `PIDS_FRAME_LEN_ENCODED_FM * 6/5 = 240`.
    #[test]
    fn deinterleave_pids_inserts_zero_at_every_sixth() {
        let pm = vec![1i8; 16 * 32 * 720];
        let mut out = Vec::with_capacity(240);
        deinterleave_pids_fm(&pm, &mut out, 0);

        assert_eq!(out.len(), 240);
        for (i, &v) in out.iter().enumerate() {
            if i % 6 == 5 {
                assert_eq!(v, 0, "expected puncture zero at index {}", i);
            } else {
                assert_eq!(v, 1, "expected data bit (+1) at index {}", i);
            }
        }
    }
}
