//! USB control-transfer primitives that target the RTL2832U's vendor
//! request bridge.
//!
//! Mirrors librtlsdr/src/librtlsdr.c rtlsdr_read_array / rtlsdr_write_array
//! / rtlsdr_demod_read_reg / rtlsdr_demod_write_reg / rtlsdr_i2c_*
//! (lines ~290..370 of librtlsdr.c).
//!
//! Encoding:
//! * Direct block read:  bmReqType=Vendor|IN,  bReq=0, wValue=addr, wIndex=block<<8
//! * Direct block write: bmReqType=Vendor|OUT, bReq=0, wValue=addr, wIndex=(block<<8)|0x10
//! * Demod read:         wValue=(addr<<8)|0x20, wIndex=page
//! * Demod write:        wValue=(addr<<8)|0x20, wIndex=0x10|page
//! * I2C is just a direct block transfer with block=IICB and addr=slave.

use nusb::Interface;
use nusb::transfer::{ControlIn, ControlOut, ControlType, Recipient};

use crate::error::RtlError;

// Block / page constants (librtlsdr.c lines ~70-80)
pub const DEMODB: u16 = 0;
pub const USBB: u16 = 1;
pub const SYSB: u16 = 2;
pub const ROMB: u16 = 3;
pub const IRB: u16 = 4;
pub const IICB: u16 = 6;

const VENDOR_REQ: u8 = 0;

fn ctl_in(value: u16, index: u16, length: u16) -> ControlIn {
    ControlIn {
        control_type: ControlType::Vendor,
        recipient: Recipient::Device,
        request: VENDOR_REQ,
        value,
        index,
        length,
    }
}

fn ctl_out<'a>(value: u16, index: u16, data: &'a [u8]) -> ControlOut<'a> {
    ControlOut {
        control_type: ControlType::Vendor,
        recipient: Recipient::Device,
        request: VENDOR_REQ,
        value,
        index,
        data,
    }
}

/// Read `len` bytes from a direct block address.
pub async fn read_array(
    iface: &Interface,
    block: u16,
    addr: u16,
    len: u16,
) -> Result<Vec<u8>, RtlError> {
    let r = iface
        .control_in(ctl_in(addr, block << 8, len))
        .await
        .into_result()?;
    Ok(r)
}

/// Write a buffer to a direct block address.
pub async fn write_array(
    iface: &Interface,
    block: u16,
    addr: u16,
    data: &[u8],
) -> Result<(), RtlError> {
    iface
        .control_out(ctl_out(addr, (block << 8) | 0x10, data))
        .await
        .into_result()?;
    Ok(())
}

/// Read a single demod register (1 or 2 bytes; we return as u16).
pub async fn demod_read_reg(
    iface: &Interface,
    page: u8,
    addr: u16,
    len: u16,
) -> Result<u16, RtlError> {
    let value = (addr << 8) | 0x20;
    let index = page as u16;
    let data = iface
        .control_in(ctl_in(value, index, len))
        .await
        .into_result()?;
    let reg = match data.len() {
        1 => data[0] as u16,
        2 => ((data[1] as u16) << 8) | (data[0] as u16),
        _ => 0,
    };
    Ok(reg)
}

/// Write a demod register (1 or 2 bytes).
pub async fn demod_write_reg(
    iface: &Interface,
    page: u8,
    addr: u16,
    val: u16,
    len: u16,
) -> Result<(), RtlError> {
    let value = (addr << 8) | 0x20;
    let index = 0x10 | page as u16;

    let mut buf = [0u8; 2];
    if len == 1 {
        buf[0] = (val & 0xff) as u8;
        iface
            .control_out(ctl_out(value, index, &buf[..1]))
            .await
            .into_result()?;
    } else {
        buf[0] = (val >> 8) as u8;
        buf[1] = (val & 0xff) as u8;
        iface
            .control_out(ctl_out(value, index, &buf[..2]))
            .await
            .into_result()?;
    }

    // librtlsdr does a dummy read after every demod write to flush.
    let _ = demod_read_reg(iface, 0x0a, 0x01, 1).await?;
    Ok(())
}

/// Read from an I2C slave on the demod's I2C bus (e.g. the R820T2 tuner).
pub async fn i2c_read(
    iface: &Interface,
    i2c_addr: u8,
    len: u16,
) -> Result<Vec<u8>, RtlError> {
    read_array(iface, IICB, i2c_addr as u16, len).await
}

/// Write to an I2C slave on the demod's I2C bus.
pub async fn i2c_write(
    iface: &Interface,
    i2c_addr: u8,
    data: &[u8],
) -> Result<(), RtlError> {
    write_array(iface, IICB, i2c_addr as u16, data).await
}
