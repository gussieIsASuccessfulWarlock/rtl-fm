//! Shared error types.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum RtlError {
    #[error("no RTL2838 device found (vendor 0x0bda, product 0x2838)")]
    NoDevice,

    #[error("USB error: {0}")]
    Usb(String),

    #[error("I2C error: {0}")]
    I2c(String),

    #[error("tuner error: {0}")]
    Tuner(String),

    #[error("invalid argument: {0}")]
    InvalidArg(String),
}

impl From<nusb::Error> for RtlError {
    fn from(e: nusb::Error) -> Self {
        RtlError::Usb(e.to_string())
    }
}

impl From<nusb::transfer::TransferError> for RtlError {
    fn from(e: nusb::transfer::TransferError) -> Self {
        RtlError::Usb(e.to_string())
    }
}
