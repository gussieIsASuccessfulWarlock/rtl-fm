use num_complex::Complex;

use crate::nrsc5::consts::{rsid_for_partition, BLKSZ, NUM_REF_FM, PM_PARTITIONS, REF_BINS_FM};

const FRAME_LOCK_REFS_REQUIRED: usize = 3;
const CFO_SEARCH_BINS: i32 = 2 * crate::nrsc5::consts::PARTITION_WIDTH_FM as i32;

pub struct Sync {
    pub symbol_buf: Vec<Vec<Complex<f32>>>,
    pub costas_phase: Vec<f32>,
    pub costas_freq: Vec<f32>,
    alpha: f32,
    beta: f32,
    pub frame_aligned: bool,
    pub frame_metric: f32,
    pub frame_offset: usize,
    pub best_count_recent: usize,
    pub best_offset_recent: usize,
    pub integer_cfo_bins: i32,
    pub channel_estimates: Vec<Complex<f32>>,
    sym_count: u64,
    pub block_count: u8,
    pub psmi: u8,
    pub first_sync: bool,
}

impl Sync {
    pub fn new() -> Self {
        let loop_bw = 0.05f32;
        let damping = std::f32::consts::FRAC_1_SQRT_2;
        let denom = 1.0 + (2.0 * damping * loop_bw) + (loop_bw * loop_bw);
        Self {
            symbol_buf: Vec::with_capacity(BLKSZ),
            costas_phase: vec![0.0; NUM_REF_FM],
            costas_freq: vec![0.0; NUM_REF_FM],
            alpha: (4.0 * damping * loop_bw) / denom,
            beta: (4.0 * loop_bw * loop_bw) / denom,
            frame_aligned: false,
            frame_metric: 0.0,
            frame_offset: 0,
            best_count_recent: 0,
            best_offset_recent: 0,
            integer_cfo_bins: 0,
            channel_estimates: vec![Complex::new(1.0, 0.0); NUM_REF_FM],
            sym_count: 0,
            block_count: 0,
            psmi: 0,
            first_sync: false,
        }
    }

    pub fn reset(&mut self) {
        self.symbol_buf.clear();
        self.frame_aligned = false;
        self.frame_metric = 0.0;
        self.frame_offset = 0;
        self.best_count_recent = 0;
        self.best_offset_recent = 0;
        self.integer_cfo_bins = 0;
        self.sym_count = 0;
        self.block_count = 0;
        self.psmi = 0;
        self.first_sync = false;
        for ce in &mut self.channel_estimates {
            *ce = Complex::new(1.0, 0.0);
        }
        for phase in &mut self.costas_phase {
            *phase = 0.0;
        }
        for freq in &mut self.costas_freq {
            *freq = 0.0;
        }
    }

    pub fn current_row(&self) -> usize {
        (31 + BLKSZ - self.frame_offset) % BLKSZ
    }

    pub fn process_symbol(&mut self, fft_bins: &[Complex<f32>]) {
        if !self.frame_aligned {
            self.symbol_buf.push(fft_bins.to_vec());
            if self.symbol_buf.len() > BLKSZ {
                self.symbol_buf.remove(0);
            }
            if self.symbol_buf.len() == BLKSZ && self.sym_count % 8 == 0 {
                self.detect_frame();
            }
        } else {
            let cfo_freq = 2.0
                * std::f32::consts::PI
                * (self.integer_cfo_bins as f32)
                * (crate::nrsc5::consts::CP_FM as f32)
                / (crate::nrsc5::consts::FFT_FM as f32);

            for (ri, &bin) in REF_BINS_FM.iter().enumerate() {
                if let Some(bin) = shifted_bin(bin, self.integer_cfo_bins, fft_bins.len()) {
                    let z = fft_bins[bin];
                    let phase = self.costas_phase[ri];
                    let doubled = z * z * cis(-2.0 * phase);
                    let error = 0.5 * doubled.im.atan2(doubled.re);

                    self.costas_freq[ri] =
                        (self.costas_freq[ri] + self.beta * error).clamp(-0.5, 0.5);
                    self.costas_phase[ri] += self.costas_freq[ri] + cfo_freq + self.alpha * error;
                    wrap_pi(&mut self.costas_phase[ri]);

                    let derotated = z * cis(-phase);
                    let sign = if derotated.re > 0.0 { 1.0 } else { -1.0 };
                    let est = Complex::new(z.re * sign, z.im * sign);
                    self.channel_estimates[ri] = self.channel_estimates[ri] * 0.9 + est * 0.1;
                }
            }
        }
        self.sym_count += 1;
    }

    fn detect_frame(&mut self) {
        let mut best_cfo = 0i32;
        let mut best_off = 0usize;
        let mut best_count = 0usize;
        let mut best_refs_matched = 0usize;

        let sync_bits: [i8; BLKSZ] = [
            -1, 1, -1, -1, -1, 1, 1, 0, 1, -1, 0, 0, 0, -1, -1, 0, 0, 0, 0, 0, -1, 1, -1, 0, 0, 0,
            0, 0, 0, 0, 0, -1,
        ];

        let mut best_costas_phase = vec![0.0f32; NUM_REF_FM];
        let mut best_costas_freq = vec![0.0f32; NUM_REF_FM];
        let mut best_seen_bc = [0usize; 16];
        let mut best_seen_psmi = [0usize; 64];

        for cfo in -CFO_SEARCH_BINS..=CFO_SEARCH_BINS {
            let mut offset_votes = [0usize; BLKSZ];
            let mut seen_bc = [0usize; 16];
            let mut seen_psmi = [0usize; 64];
            let mut refs_matched = 0usize;
            let cfo_freq =
                2.0 * std::f32::consts::PI * (cfo as f32) * (crate::nrsc5::consts::CP_FM as f32)
                    / (crate::nrsc5::consts::FFT_FM as f32);

            let mut cand_costas_phase = vec![0.0f32; NUM_REF_FM];
            let mut cand_costas_freq = vec![0.0f32; NUM_REF_FM];

            for ri in 0..NUM_REF_FM {
                let Some(bin) = shifted_bin(REF_BINS_FM[ri], cfo, self.symbol_buf[0].len()) else {
                    continue;
                };

                let partition = if ri <= PM_PARTITIONS {
                    ri
                } else {
                    ri - (PM_PARTITIONS + 1)
                };
                let rsid = rsid_for_partition(partition);
                let needle = build_needle(rsid);

                let mut phase = 0.0f32;
                let mut freq = 0.0f32;
                let mut adjusted_buf = [Complex::new(0.0, 0.0); BLKSZ];

                for n in 0..BLKSZ {
                    let z = self.symbol_buf[n][bin];
                    let doubled = z * z * cis(-2.0 * phase);
                    let error = 0.5 * doubled.im.atan2(doubled.re);

                    adjusted_buf[n] = z * cis(-phase);

                    freq = (freq + self.beta * error).clamp(-0.5, 0.5);
                    phase += freq + cfo_freq + self.alpha * error;
                    wrap_pi(&mut phase);
                }

                cand_costas_phase[ri] = phase;
                cand_costas_freq[ri] = freq;

                let mut x = 0.0f32;
                for n in 0..BLKSZ {
                    x += adjusted_buf[n].re * (sync_bits[n] as f32);
                }
                if x < 0.0 {
                    for n in 0..BLKSZ {
                        adjusted_buf[n] = -adjusted_buf[n];
                    }
                    cand_costas_phase[ri] += std::f32::consts::PI;
                    wrap_pi(&mut cand_costas_phase[ri]);
                }

                let mut bits = [0u8; BLKSZ];
                for n in 0..BLKSZ {
                    bits[n] = if adjusted_buf[n].re > 0.0 { 1 } else { 0 };
                }

                let mut matched_off = None;
                let mut matched_inverted = false;
                if let Some(off) = fuzzy_match(&needle, &bits) {
                    matched_off = Some(off);
                } else {
                    let mut bits_inv = [0u8; BLKSZ];
                    for n in 0..BLKSZ {
                        bits_inv[n] = bits[n] ^ 1;
                    }
                    if let Some(off) = fuzzy_match(&needle, &bits_inv) {
                        matched_off = Some(off);
                        matched_inverted = true;
                    }
                }

                if let Some(off) = matched_off {
                    offset_votes[off] += 1;
                    refs_matched += 1;

                    let mut dbpsk = [0u8; BLKSZ];
                    let mut prev = 0u8;
                    for i in 0..BLKSZ {
                        let mut bit = bits[(off + i) % BLKSZ];
                        if matched_inverted {
                            bit ^= 1;
                        }
                        dbpsk[i] = bit ^ prev;
                        prev = bit;
                    }
                    let bc = (dbpsk[16] << 3) | (dbpsk[17] << 2) | (dbpsk[18] << 1) | dbpsk[19];
                    let psmi = (dbpsk[25] << 5)
                        | (dbpsk[26] << 4)
                        | (dbpsk[27] << 3)
                        | (dbpsk[28] << 2)
                        | (dbpsk[29] << 1)
                        | dbpsk[30];
                    seen_bc[bc as usize] += 1;
                    seen_psmi[psmi as usize] += 1;
                }
            }

            let (off, count) = offset_votes
                .iter()
                .copied()
                .enumerate()
                .max_by_key(|&(_, c)| c)
                .unwrap_or((0, 0));

            if count > best_count || (count == best_count && refs_matched > best_refs_matched) {
                best_cfo = cfo;
                best_off = off;
                best_count = count;
                best_refs_matched = refs_matched;
                best_costas_phase = cand_costas_phase;
                best_costas_freq = cand_costas_freq;
                best_seen_bc = seen_bc;
                best_seen_psmi = seen_psmi;
            }
        }

        // Prefer strict majorities, but fall back to the most-voted bc/psmi
        // so the rest of the decoder can be exercised on marginal signals.
        let mut majority_bc: i32 = -1;
        for bc in 0..16usize {
            if best_seen_bc[bc] * 2 > best_refs_matched {
                majority_bc = bc as i32;
                break;
            }
        }
        if majority_bc < 0 {
            let mut max_v = 0;
            for bc in 0..16usize {
                if best_seen_bc[bc] > max_v {
                    max_v = best_seen_bc[bc];
                    majority_bc = bc as i32;
                }
            }
        }
        let mut majority_psmi: i32 = -1;
        for psmi in 0..64usize {
            if best_seen_psmi[psmi] * 2 > best_refs_matched {
                majority_psmi = psmi as i32;
                break;
            }
        }
        if majority_psmi < 0 {
            let mut max_v = 0;
            for psmi in 0..64usize {
                if best_seen_psmi[psmi] > max_v {
                    max_v = best_seen_psmi[psmi];
                    majority_psmi = psmi as i32;
                }
            }
        }
        self.frame_metric = best_refs_matched as f32 / NUM_REF_FM as f32;
        self.best_count_recent = best_count;
        self.best_offset_recent = best_off;
        self.integer_cfo_bins = best_cfo;
        self.frame_aligned =
            best_count >= FRAME_LOCK_REFS_REQUIRED && majority_bc >= 0 && majority_psmi >= 0;

        if self.frame_aligned {
            self.frame_offset = best_off;
            self.block_count = majority_bc as u8;
            self.psmi = majority_psmi as u8;
            self.first_sync = true;
            self.costas_phase = best_costas_phase;
            self.costas_freq = best_costas_freq;

            for ri in 0..NUM_REF_FM {
                if let Some(bin) = shifted_bin(
                    REF_BINS_FM[ri],
                    self.integer_cfo_bins,
                    self.symbol_buf[0].len(),
                ) {
                    let z = self.symbol_buf[31][bin];
                    let phase = self.costas_phase[ri];
                    let derotated = z * cis(-phase);
                    let sign = if derotated.re > 0.0 { 1.0 } else { -1.0 };
                    self.channel_estimates[ri] = Complex::new(z.re * sign, z.im * sign);
                }
            }
        }
    }
}

impl Default for Sync {
    fn default() -> Self {
        Self::new()
    }
}

fn build_needle(rsid: u8) -> [u8; BLKSZ] {
    let mut n = [0xFFu8; BLKSZ];
    n[0] = 0;
    n[1] = 1;
    n[2] = 0;
    n[3] = 0;
    n[4] = 0;
    n[5] = 1;
    n[6] = 1;
    n[8] = 1;
    n[9] = 0;
    n[10] = rsid >> 1;
    n[11] = (rsid >> 1) ^ (rsid & 1);
    n[13] = 0;
    n[14] = 0;
    n[20] = 0;
    n[21] = 1;
    n[22] = 0;
    n[31] = 0;
    n
}

const ALLOWED_BIT_ERRORS: u32 = 1;

fn fuzzy_match(needle: &[u8; BLKSZ], data: &[u8; BLKSZ]) -> Option<usize> {
    let mut best: Option<(u32, usize)> = None;
    for off in 0..BLKSZ {
        let mut errors = 0u32;
        for i in 0..BLKSZ {
            let exp = needle[i];
            if exp == 0xFF {
                continue;
            }
            let got = data[(off + i) % BLKSZ];
            if exp != got {
                errors += 1;
            }
        }
        if errors <= ALLOWED_BIT_ERRORS && best.map_or(true, |(e, _)| errors < e) {
            best = Some((errors, off));
        }
    }
    best.map(|(_, off)| off)
}

fn shifted_bin(bin: usize, offset: i32, len: usize) -> Option<usize> {
    let shifted = bin as i32 + offset;
    if shifted < 0 || shifted >= len as i32 {
        None
    } else {
        Some(shifted as usize)
    }
}

fn cis(phase: f32) -> Complex<f32> {
    let (s, c) = phase.sin_cos();
    Complex::new(c, s)
}

fn wrap_pi(phase: &mut f32) {
    while *phase > std::f32::consts::PI {
        *phase -= 2.0 * std::f32::consts::PI;
    }
    while *phase < -std::f32::consts::PI {
        *phase += 2.0 * std::f32::consts::PI;
    }
}
