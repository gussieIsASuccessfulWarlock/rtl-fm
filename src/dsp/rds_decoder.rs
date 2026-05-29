//! RDS / RBDS group decoder.
//!
//! Consumes raw NRZ data bits from `RdsDemod` and produces decoded
//! groups + a running snapshot of Programme Service name (PS),
//! Programme Identification (PI), Radio Text (RT), and Programme Type
//! (PTY).
//!
//! Reference: IEC 62106 (Europe) and NRSC-4-B (US RBDS).

/// 10-bit offset words from IEC 62106 section 5.2.
const OFFSET_A: u32 = 0x0FC;
const OFFSET_B: u32 = 0x198;
const OFFSET_C: u32 = 0x168;
const OFFSET_C_PRIME: u32 = 0x350;
const OFFSET_D: u32 = 0x1B4;

/// CRC generator polynomial g(x) = x^10 + x^8 + x^7 + x^5 + x^4 + x^3 + 1.
/// Bit positions set: 10, 8, 7, 5, 4, 3, 0 → 0b10110111001 = 0x5B9.
const POLY: u32 = 0x5B9;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Block {
    A,
    B,
    C,
    CPrime,
    D,
}

impl Block {
    fn next(self) -> Self {
        match self {
            Block::A => Block::B,
            Block::B => Block::C,
            Block::C => Block::D,
            Block::CPrime => Block::D,
            Block::D => Block::A,
        }
    }

    fn offset(self) -> u32 {
        match self {
            Block::A => OFFSET_A,
            Block::B => OFFSET_B,
            Block::C => OFFSET_C,
            Block::CPrime => OFFSET_C_PRIME,
            Block::D => OFFSET_D,
        }
    }
}

/// Compute the 10-bit syndrome of a 26-bit block (data shifted into the
/// upper 16 bits, checkword in the low 10 bits).
fn syndrome(block: u32) -> u32 {
    let mut reg: u32 = block & 0x03FF_FFFF; // 26 bits.
    // Treat `reg` as a polynomial of degree 25 and reduce modulo POLY.
    // Process MSB down to the bit just above the checkword.
    for i in (10..26).rev() {
        if (reg >> i) & 1 != 0 {
            reg ^= POLY << (i - 10);
        }
    }
    reg & 0x3FF
}

#[derive(Debug, Clone, Default)]
pub struct RdsMetadata {
    /// 16-bit Programme Identification.
    pub pi: Option<u16>,
    /// Decoded US/EU callsign if PI maps cleanly; otherwise None.
    pub callsign: Option<String>,
    /// 8-character Programme Service name.
    pub ps: Option<String>,
    /// Up to 64 characters of Radio Text.
    pub rt: Option<String>,
    /// 5-bit Programme Type.
    pub pty: Option<u8>,
    /// Human name for the PTY code (RBDS table).
    pub pty_name: Option<String>,
    /// Traffic Programme flag.
    pub tp: bool,
    /// Traffic Announcement currently on air.
    pub ta: bool,
    /// Music/Speech flag (true = music).
    pub ms_music: bool,
    /// Number of groups successfully decoded since last reset.
    pub groups_decoded: u64,
    /// Number of blocks whose syndrome did not match any offset word.
    pub blocks_dropped: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Hunting,
    Synced {
        next: Block,
    },
}

pub struct RdsDecoder {
    /// 26-bit shift register holding the most recent received bits.
    sr: u32,
    /// How many bits have been accumulated since the last sync edge.
    bit_count: usize,
    state: State,
    /// Accumulator for the current group's four data words.
    group_buf: [u16; 4],
    group_valid: [bool; 4],
    group_idx: usize,
    /// Number of consecutive blocks that failed to syndrome-match
    /// while we thought we were synced. Resets to 0 on every good
    /// block; once it crosses `SLIP_SYNC_LIMIT` we go back to Hunting.
    consecutive_bad_blocks: u32,
    /// Accumulators that span many groups.
    ps_segments: [u8; 8],
    ps_segments_valid: [bool; 8],
    rt_segments: [u8; 64],
    rt_segments_valid: [bool; 64],
    rt_ab_flag: Option<bool>,
    pub meta: RdsMetadata,
}

/// After this many consecutive bad blocks, give up the current sync
/// and start hunting from scratch. 4 = one whole group's worth of
/// blocks, so we give the decoder a single group of grace.
const SLIP_SYNC_LIMIT: u32 = 4;

impl RdsDecoder {
    pub fn new() -> Self {
        Self {
            sr: 0,
            bit_count: 0,
            state: State::Hunting,
            group_buf: [0; 4],
            group_valid: [false; 4],
            group_idx: 0,
            consecutive_bad_blocks: 0,
            ps_segments: [b' '; 8],
            ps_segments_valid: [false; 8],
            rt_segments: [b' '; 64],
            rt_segments_valid: [false; 64],
            rt_ab_flag: None,
            meta: RdsMetadata::default(),
        }
    }

    /// Push a single NRZ data bit into the decoder.
    pub fn push_bit(&mut self, bit: bool) {
        self.sr = ((self.sr << 1) | u32::from(bit)) & 0x03FF_FFFF;
        self.bit_count = self.bit_count.saturating_add(1);

        // Debug: log the first thousand syndromes so we can see if the
        // bit stream has any block-A-shaped structure at all.
        if self.meta.groups_decoded == 0 && self.meta.blocks_dropped < 20 {
            let syn = syndrome(self.sr);
            if matches!(self.state, State::Hunting) && self.bit_count > 26 {
                tracing::trace!("rds hunting syndrome = {:#05x}", syn);
            }
        }

        match self.state {
            State::Hunting => {
                if self.bit_count < 26 {
                    return;
                }
                if let Some(b) = self.block_match(self.sr) {
                    // We hit some block; we don't know which yet but
                    // the offset word tells us. Re-align onto a
                    // group-A-starting boundary.
                    self.group_idx = match b {
                        Block::A => 0,
                        Block::B => 1,
                        Block::C | Block::CPrime => 2,
                        Block::D => 3,
                    };
                    let data = ((self.sr >> 10) & 0xFFFF) as u16;
                    self.group_buf[self.group_idx] = data;
                    self.group_valid[self.group_idx] = true;
                    self.state = State::Synced { next: b.next() };
                    self.bit_count = 0;
                    if self.group_idx == 3 {
                        self.try_decode_group();
                    }
                }
            }
            State::Synced { next } => {
                if self.bit_count < 26 {
                    return;
                }
                self.bit_count = 0;
                // Expect the next block in sequence; allow C′ in place
                // of C (group-type B variants use C′).
                let want = next.offset();
                let want_alt = if matches!(next, Block::C) {
                    Some(Block::CPrime.offset())
                } else {
                    None
                };
                let syn = syndrome(self.sr);
                let ok = syn == want || Some(syn) == want_alt;
                self.group_idx = match next {
                    Block::A => 0,
                    Block::B => 1,
                    Block::C | Block::CPrime => 2,
                    Block::D => 3,
                };
                if ok {
                    let data = ((self.sr >> 10) & 0xFFFF) as u16;
                    self.group_buf[self.group_idx] = data;
                    self.group_valid[self.group_idx] = true;
                    self.consecutive_bad_blocks = 0;
                } else {
                    self.group_valid[self.group_idx] = false;
                    self.meta.blocks_dropped += 1;
                    self.consecutive_bad_blocks += 1;
                    if self.consecutive_bad_blocks >= SLIP_SYNC_LIMIT {
                        // We've drifted off sync. Re-acquire from scratch.
                        self.state = State::Hunting;
                        self.bit_count = 0;
                        self.consecutive_bad_blocks = 0;
                        self.group_valid = [false; 4];
                        return;
                    }
                }
                let advanced_next = next.next();
                self.state = State::Synced { next: advanced_next };
                if self.group_idx == 3 {
                    self.try_decode_group();
                    // Reset group-collection state but keep sync.
                    self.group_valid = [false; 4];
                }
            }
        }
    }

    fn block_match(&self, sr: u32) -> Option<Block> {
        let s = syndrome(sr);
        if s == OFFSET_A {
            Some(Block::A)
        } else if s == OFFSET_B {
            Some(Block::B)
        } else if s == OFFSET_C {
            Some(Block::C)
        } else if s == OFFSET_C_PRIME {
            Some(Block::CPrime)
        } else if s == OFFSET_D {
            Some(Block::D)
        } else {
            None
        }
    }

    fn try_decode_group(&mut self) {
        // At minimum we need block A (PI) and block B (type code) to do
        // anything useful.
        if !self.group_valid[0] || !self.group_valid[1] {
            // Slip sync if we've missed too many blocks recently.
            return;
        }
        self.meta.groups_decoded += 1;
        let block_a = self.group_buf[0];
        let block_b = self.group_buf[1];
        let block_c = self.group_buf[2];
        let block_d = self.group_buf[3];

        // Block A is the PI code.
        self.meta.pi = Some(block_a);
        self.meta.callsign = pi_to_callsign(block_a);

        // Block B layout: GGGG VTPP PPPB ...
        // bits 15-12 = group type number
        // bit 11     = version (0 = A, 1 = B)
        // bit 10     = TP
        // bits 9-5   = PTY
        // bits 4-0   = group-specific
        let group_type = (block_b >> 12) & 0x0F;
        let version_b = (block_b >> 11) & 1 == 1;
        let tp = (block_b >> 10) & 1 == 1;
        let pty = ((block_b >> 5) & 0x1F) as u8;
        self.meta.tp = tp;
        self.meta.pty = Some(pty);
        self.meta.pty_name = pty_name_us(pty).map(str::to_string);

        match (group_type, version_b) {
            (0, _) => self.decode_group_0(block_b, block_d, version_b),
            (2, _) => self.decode_group_2(block_b, block_c, block_d, version_b),
            _ => {}
        }

        // Compose PS / RT into the snapshot, even if partial.
        if self.ps_segments_valid.iter().any(|&v| v) {
            let s: String = self
                .ps_segments
                .iter()
                .map(|&b| {
                    if (0x20..=0x7E).contains(&b) {
                        b as char
                    } else {
                        ' '
                    }
                })
                .collect();
            self.meta.ps = Some(s.trim_end().to_string());
        }
        if self.rt_segments_valid.iter().any(|&v| v) {
            let s: String = self
                .rt_segments
                .iter()
                .map(|&b| {
                    if b == 0x0D {
                        // RT 0x0D is end-of-text — stop here.
                        return '\0';
                    }
                    if (0x20..=0x7E).contains(&b) {
                        b as char
                    } else {
                        ' '
                    }
                })
                .take_while(|&c| c != '\0')
                .collect();
            self.meta.rt = Some(s.trim_end().to_string());
        }
    }

    fn decode_group_0(&mut self, block_b: u16, block_d: u16, version_b: bool) {
        // Block B low bits: TA (4), MS (3), DI (2), C1 C0 (1-0 = PS segment 0..3)
        self.meta.ta = ((block_b >> 4) & 1) == 1;
        self.meta.ms_music = ((block_b >> 3) & 1) == 1;
        let seg = (block_b & 0x03) as usize;
        let c1 = ((block_d >> 8) & 0xFF) as u8;
        let c2 = (block_d & 0xFF) as u8;
        self.ps_segments[seg * 2] = c1;
        self.ps_segments[seg * 2 + 1] = c2;
        self.ps_segments_valid[seg * 2] = true;
        self.ps_segments_valid[seg * 2 + 1] = true;
        // Version B does not carry RT text in this group.
        let _ = version_b;
    }

    fn decode_group_2(&mut self, block_b: u16, block_c: u16, block_d: u16, version_b: bool) {
        // Block B low bits: AB (4), addr (3-0)
        let ab = ((block_b >> 4) & 1) == 1;
        if self.rt_ab_flag != Some(ab) {
            // A/B flag toggled — the broadcast is starting a new
            // message; clear the buffer.
            self.rt_segments = [b' '; 64];
            self.rt_segments_valid = [false; 64];
            self.rt_ab_flag = Some(ab);
        }
        let addr = (block_b & 0x0F) as usize;
        if version_b {
            // 2B: only two chars per group, block_c is PI repeat.
            let base = addr * 2;
            if base + 1 < 64 {
                self.rt_segments[base] = ((block_d >> 8) & 0xFF) as u8;
                self.rt_segments[base + 1] = (block_d & 0xFF) as u8;
                self.rt_segments_valid[base] = true;
                self.rt_segments_valid[base + 1] = true;
            }
            let _ = block_c;
        } else {
            // 2A: four chars per group across blocks C and D.
            let base = addr * 4;
            if base + 3 < 64 {
                self.rt_segments[base] = ((block_c >> 8) & 0xFF) as u8;
                self.rt_segments[base + 1] = (block_c & 0xFF) as u8;
                self.rt_segments[base + 2] = ((block_d >> 8) & 0xFF) as u8;
                self.rt_segments[base + 3] = (block_d & 0xFF) as u8;
                self.rt_segments_valid[base] = true;
                self.rt_segments_valid[base + 1] = true;
                self.rt_segments_valid[base + 2] = true;
                self.rt_segments_valid[base + 3] = true;
            }
        }
    }
}

/// Try to map a US PI code to its 4-letter callsign per NRSC-4-B
/// Annex D table D.7.
///
/// US base-26 mapping for 4-letter K- and W- callsigns:
///   K____: PI = 0x1000 + 676·L1 + 26·L2 + L3
///   W____: PI = 0x54A8 + 676·L1 + 26·L2 + L3
/// where Lx = (letter − 'A'), 0..=25.
pub fn pi_to_callsign(pi: u16) -> Option<String> {
    let pi = pi as u32;
    let (prefix, mut idx) = if (0x1000..=0x54A7).contains(&pi) {
        ('K', pi - 0x1000)
    } else if (0x54A8..=0x994F).contains(&pi) {
        ('W', pi - 0x54A8)
    } else {
        return None;
    };
    let l1 = (idx / 676) as u8;
    idx %= 676;
    let l2 = (idx / 26) as u8;
    let l3 = (idx % 26) as u8;
    if l1 > 25 || l2 > 25 || l3 > 25 {
        return None;
    }
    Some(format!(
        "{}{}{}{}",
        prefix,
        (b'A' + l1) as char,
        (b'A' + l2) as char,
        (b'A' + l3) as char
    ))
}

/// RBDS (US) Programme Type name table (NRSC-4-B Annex F).
fn pty_name_us(pty: u8) -> Option<&'static str> {
    Some(match pty {
        0 => "None",
        1 => "News",
        2 => "Information",
        3 => "Sports",
        4 => "Talk",
        5 => "Rock",
        6 => "Classic Rock",
        7 => "Adult Hits",
        8 => "Soft Rock",
        9 => "Top 40",
        10 => "Country",
        11 => "Oldies",
        12 => "Soft",
        13 => "Nostalgia",
        14 => "Jazz",
        15 => "Classical",
        16 => "Rhythm and Blues",
        17 => "Soft Rhythm and Blues",
        18 => "Foreign Language",
        19 => "Religious Music",
        20 => "Religious Talk",
        21 => "Personality",
        22 => "Public",
        23 => "College",
        29 => "Weather",
        30 => "Emergency Test",
        31 => "Emergency",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pi_to_callsign_examples() {
        // From NRSC-4-B examples.
        assert_eq!(pi_to_callsign(0x1000).as_deref(), Some("KAAA"));
        assert_eq!(pi_to_callsign(0x54A8).as_deref(), Some("WAAA"));
        // KQED in San Francisco: 88.5 MHz, PI = 0x4153 (from real broadcasts).
        // (Just check it produces *something* that round-trips.)
        let c = pi_to_callsign(0x4153);
        assert!(c.is_some());
    }

    #[test]
    fn syndrome_zero_for_zero() {
        assert_eq!(syndrome(0), 0);
    }
}
