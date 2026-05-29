//! USB layer: open RTL2838, detach kernel driver, claim interface 0.
//!
//! No libusb. Uses `nusb` which talks to Linux usbfs directly.

pub mod transfer;

use nusb::{Device, Interface};
use tracing::{debug, info, warn};

use crate::error::RtlError;

pub const VENDOR_ID: u16 = 0x0bda;
pub const PRODUCT_ID: u16 = 0x2838;
pub const BULK_ENDPOINT: u8 = 0x81;
pub const INTERFACE_NUM: u8 = 0;

/// Open the first RTL2838 device, detach the DVB-T kernel driver if
/// attached, and claim interface 0.
pub fn open() -> Result<(Device, Interface, String), RtlError> {
    let devs = nusb::list_devices()
        .map_err(|e| RtlError::Usb(format!("list_devices: {e}")))?;
    let info = devs
        .into_iter()
        .find(|d| d.vendor_id() == VENDOR_ID && d.product_id() == PRODUCT_ID)
        .ok_or(RtlError::NoDevice)?;

    let manufacturer = info.manufacturer_string().unwrap_or("").to_string();
    let product = info.product_string().unwrap_or("").to_string();
    let serial = info.serial_number().unwrap_or("").to_string();
    let descr = format!("{manufacturer} {product} (sn={serial})");
    info!("found device: {descr}");

    let device = info
        .open()
        .map_err(|e| RtlError::Usb(format!("open: {e}")))?;

    // The dvb_usb_rtl28xxu kernel driver auto-binds; detach it for our
    // session only. nusb's detach_kernel_driver is per-claim and
    // re-attaches automatically on drop.
    match device.detach_kernel_driver(INTERFACE_NUM) {
        Ok(()) => debug!("detached kernel driver from interface {INTERFACE_NUM}"),
        Err(e) => {
            // If not attached, this is fine.
            debug!("detach_kernel_driver: {e} (often benign if not attached)");
        }
    }

    let iface = device
        .claim_interface(INTERFACE_NUM)
        .map_err(|e| RtlError::Usb(format!("claim_interface({INTERFACE_NUM}): {e}")))?;
    info!("claimed interface {INTERFACE_NUM}");

    Ok((device, iface, descr))
}

/// Try claiming the interface, falling back to detach-and-retry.
pub fn reset_endpoint(iface: &Interface) {
    if let Err(e) = iface.clear_halt(BULK_ENDPOINT) {
        warn!("clear_halt({BULK_ENDPOINT:#x}): {e}");
    }
}
