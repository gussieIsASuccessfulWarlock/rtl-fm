//! Viterbi convolutional decoder for NRSC-5 P1 / PIDS streams.
//!
//! Rate-1/3 K=7 code with generators (0o133, 0o171, 0o165) and
//! tail-biting termination. Re-implements the same algorithm as the C
//! reference (`conv_dec.c`): right-shift state convention, 32-state
//! ACS butterfly that exploits LSB-odd generators to negate the branch
//! metric between sibling predecessors, and the proper tail-biting
//! pre-roll / post-roll wrapping (32 extra trellis steps on each end,
//! cyclic over the input).

use crate::nrsc5::consts::{CONV_K7, CONV_K7_GEN};

const STATES: usize = 1 << (CONV_K7 - 1); // 64
const HALF: usize = STATES / 2; // 32
const EXTRA: usize = 32; // TAIL_BITING_EXTRA, matches conv_dec.c

pub struct Viterbi {
    metrics: Vec<i32>,
    new_metrics: Vec<i32>,
    traceback: Vec<[u8; STATES]>,
}

impl Viterbi {
    pub fn new() -> Self {
        Self {
            metrics: vec![0; STATES],
            new_metrics: vec![0; STATES],
            traceback: Vec::new(),
        }
    }

    /// Decode `frame_len` bits from `depunctured` (rate-1/3 soft bits,
    /// 3 entries per trellis step). Output is written into `decoded`.
    pub fn decode(&mut self, depunctured: &[i8], decoded: &mut Vec<u8>, frame_len: usize) {
        decoded.clear();
        decoded.resize(frame_len, 0);
        if depunctured.len() < frame_len * 3 {
            return;
        }

        let total = frame_len + 2 * EXTRA;
        self.traceback.clear();
        self.traceback.resize(total, [0u8; STATES]);
        self.metrics.fill(0);

        // Pre-roll: feed the last EXTRA input bits to warm up the
        // trellis, then the main frame_len bits, then post-roll EXTRA
        // wrapped from the start. Total iterations = len + 64.
        let mut j = frame_len - EXTRA;
        for i in 0..total {
            if j == frame_len {
                j = 0;
            }
            self.step(depunctured, j, i);
            j += 1;
        }

        // Find best terminal state.
        let mut s = self.best_state();

        // Walk back through the post-roll EXTRA paths to recover the
        // state at the end of the main frame (step EXTRA + frame_len - 1).
        for i in ((EXTRA + frame_len)..total).rev() {
            let path = self.traceback[i][s];
            s = ((s & 0x1f) << 1) | (path as usize);
        }

        // Main traceback over the frame_len paths, skipping the
        // pre-roll EXTRA at the head of the paths array.
        for t in (0..frame_len).rev() {
            decoded[t] = (s >> 5) as u8;
            let path = self.traceback[EXTRA + t][s];
            s = ((s & 0x1f) << 1) | (path as usize);
        }
    }

    fn best_state(&self) -> usize {
        let mut max = i32::MIN;
        let mut best = 0;
        for (i, &m) in self.metrics.iter().enumerate() {
            if m > max {
                max = m;
                best = i;
            }
        }
        best
    }

    /// Process input position `j` (3 soft bits at j*3..j*3+3) and
    /// store the path decisions at traceback index `store_idx`.
    #[inline(always)]
    fn step(&mut self, soft: &[i8], j: usize, store_idx: usize) {
        let r0 = soft[j * 3] as i32;
        let r1 = soft[j * 3 + 1] as i32;
        let r2 = soft[j * 3 + 2] as i32;

        let mut new_tb = [0u8; STATES];

        // Match conv_dec.c's gen_state_info(): the branch output for
        // butterfly state s is computed from the previous register after
        // vstate_lshift(s, k, 0), plus the bit that wraps from s bit 5
        // into generator bit 6.
        for s in 0..HALF {
            let history = ((s << 1) & 0x3e) | ((s >> 5) << 6);
            let p0 = (history & CONV_K7_GEN[0] as usize).count_ones() & 1;
            let p1 = (history & CONV_K7_GEN[1] as usize).count_ones() & 1;
            let p2 = (history & CONV_K7_GEN[2] as usize).count_ones() & 1;

            let m0 = if p0 == 0 { -r0 } else { r0 };
            let m1 = if p1 == 0 { -r1 } else { r1 };
            let m2 = if p2 == 0 { -r2 } else { r2 };
            let metric = m0 + m1 + m2;

            let sum_a = self.metrics[2 * s];
            let sum_b = self.metrics[2 * s + 1];

            // next_state = s (new_bit = 0)
            let cand0 = sum_a + metric;
            let cand1 = sum_b - metric;
            if cand0 > cand1 {
                self.new_metrics[s] = cand0;
                new_tb[s] = 0;
            } else {
                self.new_metrics[s] = cand1;
                new_tb[s] = 1;
            }

            // next_state = s + 32 (new_bit = 1)
            let cand2 = sum_a - metric;
            let cand3 = sum_b + metric;
            if cand2 > cand3 {
                self.new_metrics[s + HALF] = cand2;
                new_tb[s + HALF] = 0;
            } else {
                self.new_metrics[s + HALF] = cand3;
                new_tb[s + HALF] = 1;
            }
        }

        self.metrics.copy_from_slice(&self.new_metrics);
        self.traceback[store_idx] = new_tb;
    }
}

impl Default for Viterbi {
    fn default() -> Self {
        Self::new()
    }
}
