//! RTL2832U demodulator init and sample-rate config.
//!
//! Faithful (best-effort) port of librtlsdr/src/librtlsdr.c
//! rtlsdr_init_baseband (~lines 1320-1390) and rtlsdr_set_sample_rate
//! (~lines 1420-1490). Constants come from the same file.
//!
//! Note: the RTL2832U is being used in "pseudo-IQ" mode where the
//! tuner is fed to the ADC and the demodulator path is mostly bypassed;
//! we read the raw I/Q from the bulk endpoint at the requested sample
//! rate.

use nusb::Interface;
use tracing::debug;

use crate::error::RtlError;
use crate::rtlsdr::i2c::{USBB, demod_write_reg, write_array};

// USB control block registers (librtlsdr.c register defines)
const USB_SYSCTL: u16 = 0x2000;
const USB_EPA_CTL: u16 = 0x2148;
const USB_EPA_MAXPKT: u16 = 0x2158;
// const USB_EPA_FIFO_CFG: u16 = 0x2160;

// Demod control block registers (librtlsdr.c ~ line 80)
// const DEMOD_CTL: u16 = 0x3000;
// const GPO: u16 = 0x3001;
// const GPI: u16 = 0x3002;
// const GPOE: u16 = 0x3003;
// const GPD: u16 = 0x3004;

// Reference clock applied to the RTL2832U.
pub const DEF_RTL_XTAL_FREQ: u32 = 28_800_000;

/// Soft-reset the demod block via demod register 0x01 (librtlsdr.c
/// rtlsdr_init_baseband ~line 1340).
async fn demod_reset(iface: &Interface) -> Result<(), RtlError> {
    demod_write_reg(iface, 1, 0x01, 0x14, 1).await?;
    demod_write_reg(iface, 1, 0x01, 0x10, 1).await?;
    Ok(())
}

/// Run librtlsdr's init_baseband sequence. After this the demod is
/// ready to be configured for sample-rate and to deliver 8-bit IQ via
/// the bulk endpoint.
pub async fn init_baseband(iface: &Interface) -> Result<(), RtlError> {
    // initialize USB (librtlsdr.c init_baseband)
    write_array(iface, USBB, USB_SYSCTL, &[0x09]).await?;
    write_array(iface, USBB, USB_EPA_MAXPKT, &[0x00, 0x02]).await?;
    write_array(iface, USBB, USB_EPA_CTL, &[0x10, 0x02]).await?;

    // poweron demod
    write_array(iface, crate::rtlsdr::i2c::SYSB, 0x3000 + 1, &[0x22]).await?;
    write_array(iface, crate::rtlsdr::i2c::SYSB, 0x3000, &[0xe8]).await?;

    demod_reset(iface).await?;

    // disable spectrum inversion and adjacent channel rejection
    demod_write_reg(iface, 1, 0x15, 0x00, 1).await?;
    demod_write_reg(iface, 1, 0x16, 0x0000, 2).await?;

    // clear both DDC shift and IF freq registers
    for r in 0x16u16..=0x1f {
        demod_write_reg(iface, 1, r, 0x00, 1).await?;
    }

    // set FIR coefficients (librtlsdr default table)
    let fir: [u8; 20] = [
        0xca, 0xdc, 0xd7, 0xd8, 0xe0, 0xf2, 0x0e, 0x35, 0x06, 0x50, 0x9c, 0x0d, 0x71, 0x11, 0x14,
        0x71, 0x74, 0x19, 0x41, 0xa5,
    ];
    for (i, b) in fir.iter().enumerate() {
        demod_write_reg(iface, 1, 0x1c + i as u16, *b as u16, 1).await?;
    }

    // enable SDR mode, disable DAGC (bit 5)
    demod_write_reg(iface, 0, 0x19, 0x05, 1).await?;

    // init FSM state-holding register
    demod_write_reg(iface, 1, 0x93, 0xf0, 1).await?;
    demod_write_reg(iface, 1, 0x94, 0x0f, 1).await?;

    // disable AGC (en_dagc, bit 0)
    demod_write_reg(iface, 1, 0x11, 0x00, 1).await?;
    // disable RF and IF AGC loop
    demod_write_reg(iface, 1, 0x04, 0x00, 1).await?;

    // disable PID filter (enable_PID = 0)
    demod_write_reg(iface, 0, 0x61, 0x60, 1).await?;

    // opt_adc_iq = 0, default ADC_I/ADC_Q datapath
    demod_write_reg(iface, 0, 0x06, 0x80, 1).await?;

    // input is from tuner -> ADC_I/ADC_Q swap to get IQ stream
    demod_write_reg(iface, 1, 0xb1, 0x1b, 1).await?;

    // enable RIQ swap to give us correct sideband orientation
    demod_write_reg(iface, 0, 0x0d, 0x83, 1).await?;

    debug!("RTL2832U baseband initialized");
    Ok(())
}

/// Configure the demod's resampler so the bulk endpoint delivers IQ
/// at `sample_hz`. Mirrors librtlsdr rtlsdr_set_sample_rate.
pub async fn set_sample_rate(iface: &Interface, sample_hz: u32) -> Result<(), RtlError> {
    if !(225_001..=3_200_000).contains(&sample_hz)
        || (300_001..=900_000).contains(&sample_hz)
    {
        return Err(RtlError::InvalidArg(format!(
            "sample rate {sample_hz} Hz outside RTL2832U supported set"
        )));
    }

    let rsamp_ratio: u32 = ((DEF_RTL_XTAL_FREQ as u64 * (1u64 << 22)) / sample_hz as u64) as u32;
    let rsamp_ratio = rsamp_ratio & 0x0ffffffc;

    demod_write_reg(iface, 1, 0x9f, ((rsamp_ratio >> 16) & 0xffff) as u16, 2).await?;
    demod_write_reg(iface, 1, 0xa1, (rsamp_ratio & 0xffff) as u16, 2).await?;

    // recalibrate the resampler
    demod_reset(iface).await?;

    debug!("sample rate set to {sample_hz} Hz (ratio {rsamp_ratio:#x})");
    Ok(())
}

/// Reset the bulk-IN endpoint FIFO so the next stream starts clean.
pub async fn reset_buffer(iface: &Interface) -> Result<(), RtlError> {
    write_array(iface, USBB, USB_EPA_CTL, &[0x10, 0x02]).await?;
    write_array(iface, USBB, USB_EPA_CTL, &[0x00, 0x00]).await?;
    Ok(())
}

/// Program the demodulator's internal DDC with the IF the tuner is
/// outputting, so that after the DDC the baseband is centered at DC.
///
/// librtlsdr/src/librtlsdr.c rtlsdr_set_if_freq (~line 1090).
pub async fn set_if_freq(iface: &Interface, freq_hz: u32) -> Result<(), RtlError> {
    let rtl_xtal = DEF_RTL_XTAL_FREQ as i64;
    // if_freq = -(freq * 2^22 / rtl_xtal). librtlsdr negates so the
    // DDC subtracts the IF rather than adding.
    let if_freq = -((freq_hz as i64 * (1i64 << 22)) / rtl_xtal);
    // Sign-extended into the demod's 22-bit if_freq register field.
    let tmp_hi = ((if_freq >> 16) as u16) & 0x003f;
    let tmp_mid = ((if_freq >> 8) as u16) & 0x00ff;
    let tmp_lo = (if_freq as u16) & 0x00ff;
    demod_write_reg(iface, 1, 0x19, tmp_hi, 1).await?;
    demod_write_reg(iface, 1, 0x1a, tmp_mid, 1).await?;
    demod_write_reg(iface, 1, 0x1b, tmp_lo, 1).await?;
    Ok(())
}

/// Tell the demodulator that the tuner output is spectrum-inverted
/// (true for the R820T/R820T2 zero-IF path). librtlsdr writes 0x01
/// here in its tuner_init for R820T after init_baseband had cleared
/// it.
pub async fn set_spectrum_inversion(iface: &Interface, inverted: bool) -> Result<(), RtlError> {
    let v = if inverted { 0x01 } else { 0x00 };
    demod_write_reg(iface, 1, 0x15, v, 1).await
}

/// Enable / disable the demod's I2C repeater. The demod sits between
/// the USB control endpoint and the tuner's I2C bus; the repeater must
/// be ON for any tuner I2C transaction to reach the chip and OFF
/// during streaming (so the I2C clock doesn't add noise into the ADC).
///
/// librtlsdr/src/librtlsdr.c rtlsdr_set_i2c_repeater (~line 870).
pub async fn set_i2c_repeater(iface: &Interface, on: bool) -> Result<(), RtlError> {
    let v = if on { 0x18 } else { 0x10 };
    demod_write_reg(iface, 1, 0x01, v, 1).await
}
