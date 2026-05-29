//! Rafael Micro R820T2 tuner.
//!
//! Port of the essential paths from librtlsdr/src/tuner_r82xx.c:
//!  - r82xx_init           (~line 1180)
//!  - r82xx_set_freq       (~line 1100)
//!  - r82xx_set_pll        (~line  650)
//!  - r82xx_set_mux        (~line  580)
//!  - gain tables          (~lines 60-90)
//!
//! Every tuner I2C transaction goes via the demod's I2C repeater, which
//! must be enabled before each burst and disabled afterwards (so the
//! I2C clock doesn't bleed into the ADC during streaming).
//! R820T2 read bytes arrive bit-reversed and must be flipped by us.

use std::sync::Arc;

use nusb::Interface;
use parking_lot::Mutex;
use tracing::debug;

use crate::error::RtlError;
use crate::rtlsdr::i2c::{i2c_read, i2c_write};
use crate::rtlsdr::rtl2832::set_i2c_repeater;

/// 7-bit I2C address shifted to the 8-bit librtlsdr convention.
pub const R820T_I2C_ADDR: u8 = 0x34;

/// R820T2 intermediate frequency. librtlsdr tuner_r82xx.c uses this
/// for both DVB-T and analog FM; the RTL2832U demod's DDC is then
/// programmed to subtract it.
pub const R820T_IF_FREQ_HZ: u32 = 3_570_000;

/// First writable register is 0x05; registers 0x00-0x04 are read-only.
const REG_SHADOW_START: usize = 5;
const NUM_REGS: usize = 32;

/// librtlsdr default register table (tuner_r82xx.c r82xx_init_array,
/// starting at 0x05).
const INIT_REGS: [u8; NUM_REGS - REG_SHADOW_START] = [
    0x83, 0x32, 0x75, // 05..07
    0xc0, 0x40, 0xd6, 0x6c, // 08..0b
    0xf5, 0x63, 0x75, 0x68, // 0c..0f
    0x6c, 0x83, 0x80, 0x00, // 10..13
    0x0f, 0x00, 0xc0, 0x30, // 14..17
    0x48, 0xcc, 0x60, 0x00, // 18..1b
    0x54, 0xae, 0x4a, 0xc0, // 1c..1f
];

/// LNA + Mixer gain table indices → dB*10 (librtlsdr tuner_r82xx.c
/// lna_gain_steps / mixer_gain_steps merged).
const GAIN_STEPS_TENTH_DB: [i32; 29] = [
    0, 9, 14, 27, 37, 77, 87, 125, 144, 157, 166, 197, 207, 229, 254, 280, 297, 328, 338, 364, 372,
    386, 402, 421, 434, 439, 445, 480, 496,
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    DigitalTv,
    AnalogFm,
}

pub struct R820T2 {
    iface: Interface,
    regs: Arc<Mutex<[u8; NUM_REGS]>>,
    xtal_hz: u32,
}

impl R820T2 {
    pub fn new(iface: Interface) -> Self {
        let mut regs = [0u8; NUM_REGS];
        for (i, b) in INIT_REGS.iter().enumerate() {
            regs[REG_SHADOW_START + i] = *b;
        }
        Self {
            iface,
            regs: Arc::new(Mutex::new(regs)),
            xtal_hz: crate::rtlsdr::rtl2832::DEF_RTL_XTAL_FREQ,
        }
    }

    /// Probe by reading register 0; original R820T2 returns 0x69.
    /// This is the first I2C transaction performed and is the canonical
    /// "is the tuner alive?" check.
    pub async fn check_id(&self) -> Result<u8, RtlError> {
        set_i2c_repeater(&self.iface, true).await?;
        let r = self.read_reg(0x00).await;
        set_i2c_repeater(&self.iface, false).await?;
        r
    }

    /// Push every shadow register to the chip in a single I2C burst.
    pub async fn write_all(&self) -> Result<(), RtlError> {
        set_i2c_repeater(&self.iface, true).await?;
        let r = self.write_all_inner().await;
        set_i2c_repeater(&self.iface, false).await?;
        r
    }

    async fn write_all_inner(&self) -> Result<(), RtlError> {
        let snapshot = *self.regs.lock();
        // librtlsdr/src/tuner_r82xx.c r82xx_write splits writes into
        // chunks of up to NUM_REGS-1 = 26 data bytes. The R820T2's
        // I2C state machine appears to have a hard 27-byte total cap
        // (address + data); 28 bytes makes the demod stall the bulk
        // endpoint. Be conservative and chunk to 16 data bytes.
        const CHUNK: usize = 8;
        let mut offset = REG_SHADOW_START;
        while offset < NUM_REGS {
            let end = (offset + CHUNK).min(NUM_REGS);
            let mut buf: Vec<u8> = Vec::with_capacity(1 + (end - offset));
            buf.push(offset as u8);
            buf.extend_from_slice(&snapshot[offset..end]);
            i2c_write(&self.iface, R820T_I2C_ADDR, &buf).await?;
            offset = end;
        }
        Ok(())
    }

    /// Read 1 byte at `reg`. Repeater must already be enabled.
    async fn read_reg(&self, reg: u8) -> Result<u8, RtlError> {
        // librtlsdr r82xx_read: write the register address as a 1-byte
        // I2C write, then issue a 1-byte I2C read.
        i2c_write(&self.iface, R820T_I2C_ADDR, &[reg]).await?;
        let buf = i2c_read(&self.iface, R820T_I2C_ADDR, 1).await?;
        Ok(bit_reverse(buf[0]))
    }

    /// Repeater-bracketed write of a single register through the shadow.
    async fn write_reg(&self, addr: u8, val: u8) -> Result<(), RtlError> {
        self.regs.lock()[addr as usize] = val;
        i2c_write(&self.iface, R820T_I2C_ADDR, &[addr, val]).await
    }

    async fn write_reg_mask(&self, addr: u8, val: u8, mask: u8) -> Result<(), RtlError> {
        let cur = self.regs.lock()[addr as usize];
        let new = (cur & !mask) | (val & mask);
        self.write_reg(addr, new).await
    }

    /// Run the init sequence, leaving the tuner in analog-FM mode
    /// (~200 kHz IF, suitable for broadcast FM).
    pub async fn init(&self) -> Result<(), RtlError> {
        set_i2c_repeater(&self.iface, true).await?;
        let r = self.init_inner().await;
        set_i2c_repeater(&self.iface, false).await?;
        r
    }

    async fn init_inner(&self) -> Result<(), RtlError> {
        self.write_all_inner().await?;
        self.set_mode(Mode::AnalogFm).await?;
        self.set_tf(106_000_000).await?;
        debug!("R820T2 initialized");
        Ok(())
    }

    async fn set_mode(&self, mode: Mode) -> Result<(), RtlError> {
        match mode {
            Mode::DigitalTv => {
                self.write_reg_mask(0x1c, 0xf8, 0xf8).await?;
                self.write_reg_mask(0x1e, 0x00, 0x60).await?;
            }
            Mode::AnalogFm => {
                self.write_reg_mask(0x1c, 0xc8, 0xf8).await?;
                self.write_reg_mask(0x1e, 0x60, 0x60).await?;
                self.write_reg_mask(0x06, 0x10, 0x30).await?;
            }
        }
        Ok(())
    }

    /// Tracking-filter (RF input bandpass) calibration based on the
    /// tuning frequency.
    async fn set_tf(&self, freq_hz: u32) -> Result<(), RtlError> {
        let mhz = freq_hz / 1_000_000;
        let (open_d, rf_mux_ploy, tf_c) = match mhz {
            0..=49 => (0x08, 0x02, 0xdf),
            50..=54 => (0x08, 0x02, 0xbe),
            55..=59 => (0x08, 0x02, 0x8b),
            60..=64 => (0x08, 0x02, 0x7b),
            65..=69 => (0x08, 0x02, 0x69),
            70..=74 => (0x08, 0x02, 0x58),
            75..=80 => (0x00, 0x02, 0x44),
            81..=89 => (0x00, 0x02, 0x34),
            90..=99 => (0x00, 0x02, 0x24),
            100..=109 => (0x00, 0x02, 0x13),
            110..=119 => (0x00, 0x02, 0x13),
            120..=139 => (0x00, 0x02, 0x11),
            140..=179 => (0x00, 0x02, 0x00),
            180..=219 => (0x00, 0x02, 0x00),
            _ => (0x00, 0x40, 0x00),
        };
        self.write_reg_mask(0x17, open_d, 0x08).await?;
        self.write_reg_mask(0x1a, rf_mux_ploy, 0xc3).await?;
        self.write_reg(0x1b, tf_c).await?;
        Ok(())
    }

    /// Program the PLL to tune to `freq_hz` (RF, in Hz).
    /// Returns the actual achievable frequency.
    pub async fn set_freq(&self, freq_hz: u32) -> Result<u32, RtlError> {
        set_i2c_repeater(&self.iface, true).await?;
        let r = self.set_freq_inner(freq_hz).await;
        set_i2c_repeater(&self.iface, false).await?;
        r
    }

    async fn set_freq_inner(&self, freq_hz: u32) -> Result<u32, RtlError> {
        // librtlsdr adds the IF offset before tuning the PLL.
        let lo_hz = freq_hz + R820T_IF_FREQ_HZ;
        let actual = self.set_pll(lo_hz).await?;
        // Reconfigure RF tracking filter for the new band.
        self.set_tf(freq_hz).await?;
        Ok(actual.saturating_sub(R820T_IF_FREQ_HZ))
    }

    /// Lock the PLL to `lo_hz`. Mirrors r82xx_set_pll (tuner_r82xx.c
    /// ~line 650). Returns the actual locked LO in Hz.
    async fn set_pll(&self, lo_hz: u32) -> Result<u32, RtlError> {
        // Power up VCO
        self.write_reg_mask(0x10, 0x00, 0x10).await?;
        self.write_reg_mask(0x1a, 0x00, 0x0c).await?;
        // Set ref divider 2
        self.write_reg_mask(0x10, 0x00, 0x10).await?;

        let pll_ref = self.xtal_hz;
        let mut mix_div: u8 = 2;
        let mut div_buf: u8 = 0;
        for d in &[2u8, 4, 8, 16, 32, 64] {
            let vco_freq = (lo_hz as u64) * (*d as u64);
            if (1_750_000_000..=3_900_000_000).contains(&vco_freq) {
                mix_div = *d;
                div_buf = match *d {
                    2 => 0,
                    4 => 1,
                    8 => 2,
                    16 => 3,
                    32 => 4,
                    64 => 5,
                    _ => 0,
                };
                break;
            }
        }
        self.write_reg_mask(0x10, div_buf << 5, 0xe0).await?;

        let vco_freq = (lo_hz as u64) * (mix_div as u64);
        let nint = (vco_freq / (2 * pll_ref as u64)) as u32;
        let vco_fra = vco_freq - 2 * pll_ref as u64 * nint as u64;

        if !(13..=76).contains(&nint) {
            return Err(RtlError::Tuner(format!(
                "nint={nint} out of range for LO {lo_hz} Hz"
            )));
        }

        let ni = (nint - 13) / 4;
        let si = nint - 4 * ni - 13;
        self.write_reg(0x14, (ni as u8) + ((si as u8) << 6)).await?;

        // sigma-delta on/off
        let sdm_on = if vco_fra != 0 { 0x00 } else { 0x08 };
        self.write_reg_mask(0x12, sdm_on, 0x08).await?;

        // Fractional 16-bit SDM. librtlsdr/src/tuner_r82xx.c (~line 720)
        // compares vco_fra against (2 * pll_ref / n_sdm) — not
        // pll_ref / n_sdm — so each iteration "spends" half of the
        // pll_ref-relative bit weight. Using pll_ref/n_sdm doubles
        // the threshold mass and tunes ~2x the requested fractional
        // offset, shifting the LO by hundreds of kHz.
        let mut vco_fra = vco_fra;
        let mut sdm: u32 = 0;
        let mut n_sdm: u64 = 2;
        let pll_ref_x2 = 2 * pll_ref as u64;
        while vco_fra > 1 {
            let thresh = pll_ref_x2 / n_sdm;
            if vco_fra > thresh {
                sdm += 0x8000u32 >> (n_sdm.trailing_zeros() - 1);
                vco_fra -= thresh;
            }
            n_sdm <<= 1;
            if n_sdm > 0x8000 {
                break;
            }
        }
        self.write_reg(0x16, (sdm >> 8) as u8).await?;
        self.write_reg(0x15, (sdm & 0xff) as u8).await?;

        let actual_vco = 2 * pll_ref as u64 * nint as u64
            + 2 * pll_ref as u64 * sdm as u64 / 0x10000;
        let actual_lo = (actual_vco / mix_div as u64) as u32;
        Ok(actual_lo)
    }

    /// Set tuner gain in tenths of dB.
    pub async fn set_gain(&self, gain_tenth_db: i32) -> Result<i32, RtlError> {
        set_i2c_repeater(&self.iface, true).await?;
        let r = self.set_gain_inner(gain_tenth_db).await;
        set_i2c_repeater(&self.iface, false).await?;
        r
    }

    async fn set_gain_inner(&self, gain_tenth_db: i32) -> Result<i32, RtlError> {
        let mut idx = 0usize;
        for (i, g) in GAIN_STEPS_TENTH_DB.iter().enumerate() {
            if *g <= gain_tenth_db {
                idx = i;
            } else {
                break;
            }
        }
        let lna_idx = (idx / 2) & 0x0f;
        let mix_idx = (idx - lna_idx).min(15) & 0x0f;
        self.write_reg_mask(0x05, lna_idx as u8, 0x0f).await?;
        self.write_reg_mask(0x07, mix_idx as u8, 0x0f).await?;
        Ok(GAIN_STEPS_TENTH_DB[idx])
    }

    /// Enable per-stage AGC (LNA + Mixer + VGA).
    pub async fn set_agc(&self, enable: bool) -> Result<(), RtlError> {
        set_i2c_repeater(&self.iface, true).await?;
        let r = self.set_agc_inner(enable).await;
        set_i2c_repeater(&self.iface, false).await?;
        r
    }

    async fn set_agc_inner(&self, enable: bool) -> Result<(), RtlError> {
        if enable {
            self.write_reg_mask(0x05, 0x00, 0xb0).await?; // LNA AGC on
            self.write_reg_mask(0x07, 0x10, 0x10).await?; // Mixer AGC on
            self.write_reg_mask(0x0c, 0x0b, 0x9f).await?; // VGA auto, ~16.3 dB
        } else {
            self.write_reg_mask(0x05, 0x10, 0xb0).await?;
            self.write_reg_mask(0x07, 0x00, 0x10).await?;
            self.write_reg_mask(0x0c, 0x08, 0x9f).await?;
        }
        Ok(())
    }
}

fn bit_reverse(b: u8) -> u8 {
    let mut r = 0u8;
    for i in 0..8 {
        if (b >> i) & 1 == 1 {
            r |= 1 << (7 - i);
        }
    }
    r
}
