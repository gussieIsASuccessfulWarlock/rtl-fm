//! Rate-5/8 Viterbi decoder for NRSC-5 P1 logical channel.
//!
//! Tables are generated following the Theori C implementation's
//! LFSR-based convolution; clippy's style lints are suppressed so the
//! structure stays readable against the reference.

#![allow(clippy::needless_range_loop, clippy::collapsible_if)]

use std::sync::LazyLock;

use crate::nrsc5::consts::{VITERBI_GEN, VITERBI_INPUTS, VITERBI_NUMS};

/// Precomputed tables: for each state (0..63) and each 5-bit input (0..31),
/// output[state][input] = 8-bit expected output word,
/// next_state[state][input] = next trellis state.
static OUTPUT_TABLE: LazyLock<[[u8; VITERBI_INPUTS]; VITERBI_NUMS]> = LazyLock::new(|| {
    let mut out_tab = [[0u8; VITERBI_INPUTS]; VITERBI_NUMS];
    let mut nxt_tab = [[0u8; VITERBI_INPUTS]; VITERBI_NUMS];

    // Generate init permutation using LFSR: x^6 + x^5 + x^2 + 1
    let mut init = [0u32; VITERBI_NUMS];
    let mut shift_reg = 0u32;
    for i in 0..VITERBI_NUMS {
        init[i] = shift_reg;
        shift_reg = shift_reg << 1
            | ((shift_reg >> 5) ^ (shift_reg >> 2) ^ shift_reg) & 1;
    }

    // Generate gen5 table: 8 output generators × 5 input mappings
    let mut gen5 = [0u8; 64];
    for i in 0..8 {
        gen5[8 * i] = VITERBI_GEN[i] & 1;
        for j in 1..5 {
            gen5[8 * i + j] = init[gen5[8 * i + j - 1] as usize] as u8;
        }
    }

    for state in 0..VITERBI_NUMS {
        for inp in 0..VITERBI_INPUTS {
            // Compute output bits
            let mut b = 0u8;
            for k in 0..8 {
                b = (b << 1)
                    | ((gen5[8 * k])
                        ^ (gen5[8 * k + 1] & (((inp >> 3) & 1) as u8))
                        ^ (gen5[8 * k + 2] & (((inp >> 2) & 1) as u8))
                        ^ (gen5[8 * k + 3] & (((inp >> 1) & 1) as u8))
                        ^ (gen5[8 * k + 4] & ((inp & 1) as u8)));
            }
            out_tab[state][inp] = b;

            // Compute next state
            let mut s = init[state];
            for k in 0..5 {
                s = (s << 1) | (((inp >> (4 - k)) & 1) as u32);
            }
            nxt_tab[state][inp] = (s & (VITERBI_NUMS as u32 - 1)) as u8;
        }
    }

    out_tab
});

static NXT_TABLE: LazyLock<[[u8; VITERBI_INPUTS]; VITERBI_NUMS]> = LazyLock::new(|| {
    let mut out_tab = [[0u8; VITERBI_INPUTS]; VITERBI_NUMS];
    let mut nxt_tab = [[0u8; VITERBI_INPUTS]; VITERBI_NUMS];

    let mut init = [0u32; VITERBI_NUMS];
    let mut shift_reg = 0u32;
    for i in 0..VITERBI_NUMS {
        init[i] = shift_reg;
        shift_reg = shift_reg << 1
            | ((shift_reg >> 5) ^ (shift_reg >> 2) ^ shift_reg) & 1;
    }

    let mut gen5 = [0u8; 64];
    for i in 0..8 {
        gen5[8 * i] = VITERBI_GEN[i] & 1;
        for j in 1..5 {
            gen5[8 * i + j] = init[gen5[8 * i + j - 1] as usize] as u8;
        }
    }

    for state in 0..VITERBI_NUMS {
        for inp in 0..VITERBI_INPUTS {
            let mut b = 0u8;
            for k in 0..8 {
                b = (b << 1)
                    | ((gen5[8 * k])
                        ^ (gen5[8 * k + 1] & (((inp >> 3) & 1) as u8))
                        ^ (gen5[8 * k + 2] & (((inp >> 2) & 1) as u8))
                        ^ (gen5[8 * k + 3] & (((inp >> 1) & 1) as u8))
                        ^ (gen5[8 * k + 4] & ((inp & 1) as u8)));
            }
            out_tab[state][inp] = b;

            let mut s = init[state];
            for k in 0..5 {
                s = (s << 1) | (((inp >> (4 - k)) & 1) as u32);
            }
            nxt_tab[state][inp] = (s & (VITERBI_NUMS as u32 - 1)) as u8;
        }
    }

    nxt_tab
});

pub struct Viterbi {
    path_metrics: [i32; VITERBI_NUMS],
    traceback: Vec<[u8; VITERBI_NUMS]>, // best previous state for each state at each step
    out_buf: Vec<u8>,
}

impl Viterbi {
    pub fn new() -> Self {
        let mut pm = [0i32; VITERBI_NUMS];
        pm[0] = 0;
        for i in 1..VITERBI_NUMS {
            pm[i] = i32::MIN / 2;
        }
        Self {
            path_metrics: pm,
            traceback: Vec::with_capacity(1024),
            out_buf: Vec::with_capacity(4096),
        }
    }

    /// Decode soft bits (0..255, 128 = erasure).
    /// Returns decoded bytes (5 bits packed per step → groups, output bytes).
    pub fn decode(&mut self, soft_bits: &[u8]) -> &[u8] {
        let out_tab = &*OUTPUT_TABLE;
        let nxt_tab = &*NXT_TABLE;

        let steps = soft_bits.len() / 8;
        self.traceback.clear();
        self.traceback.reserve(steps);

        let mut pm = [i32::MIN / 2; VITERBI_NUMS];
        pm[0] = 0;

        let mut tb_step: [u8; VITERBI_NUMS] = [0; VITERBI_NUMS];

        for step in 0..steps {
            let base = step * 8;
            let rx0 = soft_bits[base] as i32;
            let rx1 = soft_bits[base + 1] as i32;
            let rx2 = soft_bits[base + 2] as i32;
            let rx3 = soft_bits[base + 3] as i32;
            let rx4 = soft_bits[base + 4] as i32;
            let rx5 = soft_bits[base + 5] as i32;
            let rx6 = soft_bits[base + 6] as i32;
            let rx7 = soft_bits[base + 7] as i32;
            let rx = [rx0, rx1, rx2, rx3, rx4, rx5, rx6, rx7];

            let mut new_pm = [i32::MIN / 2; VITERBI_NUMS];

            for state in 0..VITERBI_NUMS {
                let cur_pm = pm[state];
                if cur_pm < i32::MIN / 4 {
                    continue;
                }
                for inp in 0..VITERBI_INPUTS {
                    let exp = out_tab[state][inp];
                    let ns = nxt_tab[state][inp] as usize;

                    let mut bm = 0i32;
                    for bit in 0..8 {
                        let ebit = ((exp >> (7 - bit)) & 1) as i32;
                        bm += if ebit == 1 { rx[bit] } else { 255 - rx[bit] };
                    }

                    let cand = cur_pm + bm;
                    if cand > new_pm[ns] {
                        new_pm[ns] = cand;
                        tb_step[ns] = state as u8;
                    }
                }
            }

            pm = new_pm;
            self.traceback.push(tb_step);
        }

        self.path_metrics = pm;

        // Traceback: find best final state, then trace back through steps
        let best_end = self.path_metrics.iter().enumerate()
            .max_by_key(|&(_, &v)| v)
            .map(|(i, _)| i)
            .unwrap_or(0);

        let mut state = best_end as u8;
        let mut decoded = vec![0u8; steps * 5 / 8 + 1];
        let mut bit_pos = 0;

        for step in (0..steps).rev() {
            let prev_state = self.traceback[step][state as usize];

            // Extract the 5 input bits that caused transition prev_state -> state
            let inp = find_input(prev_state as usize, state as usize);

            for b in (0..5).rev() {
                let byte_idx = bit_pos / 8;
                let bit_off = bit_pos % 8;
                if byte_idx < decoded.len() {
                    if (inp >> b) & 1 == 1 {
                        decoded[byte_idx] |= 1 << (7 - bit_off);
                    }
                }
                bit_pos += 1;
            }

            state = prev_state;
        }

        decoded.reverse();
        let byte_len = (steps * 5).div_ceil(8);
        decoded.truncate(byte_len);
        self.out_buf = decoded;
        &self.out_buf
    }
}

fn find_input(prev_state: usize, next_state: usize) -> u8 {
    let nxt_tab = &*NXT_TABLE;
    for inp in 0..VITERBI_INPUTS {
        if nxt_tab[prev_state][inp] as usize == next_state {
            return inp as u8;
        }
    }
    0
}

impl Default for Viterbi {
    fn default() -> Self { Self::new() }
}
