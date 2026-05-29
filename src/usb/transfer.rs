//! Bulk-IN URB pump with ~16 transfers in flight.
//!
//! librtlsdr uses 15 transfers of 16 KiB by default. We use the same
//! geometry to keep the FIFO healthy and broadcast each completed
//! chunk as a shared `Arc<[u8]>`.

use std::sync::Arc;

use nusb::Interface;
use nusb::transfer::RequestBuffer;
use tokio::sync::broadcast;
use tracing::{debug, warn};

use crate::error::RtlError;
use crate::usb::BULK_ENDPOINT;

/// Default bulk transfer size used by librtlsdr (16 KiB).
pub const URB_SIZE: usize = 16 * 1024;
/// In-flight URB count. librtlsdr defaults to 15.
pub const NUM_URBS: usize = 16;

pub type IqChunk = Arc<[u8]>;

/// Run the bulk pump until the interface is closed or all subscribers
/// have dropped. Each completed transfer is re-queued immediately to
/// keep the host controller's pipeline full.
pub async fn run_bulk_pump(
    iface: Interface,
    tx: broadcast::Sender<IqChunk>,
) -> Result<(), RtlError> {
    let mut queue = iface.bulk_in_queue(BULK_ENDPOINT);
    // Prime the pipeline.
    for _ in 0..NUM_URBS {
        queue.submit(RequestBuffer::new(URB_SIZE));
    }
    debug!("primed {NUM_URBS} bulk URBs of {URB_SIZE} bytes");

    let mut dropped = 0u64;
    loop {
        let completion = queue.next_complete().await;
        match completion.status {
            Ok(()) => {
                let bytes: Arc<[u8]> = Arc::from(completion.data.into_boxed_slice());
                if let Err(_e) = tx.send(bytes) {
                    // No subscribers — fine; keep pumping so the HW
                    // does not back-pressure into overruns.
                }
            }
            Err(e) => {
                dropped += 1;
                if dropped % 64 == 1 {
                    warn!("bulk transfer error: {e} (dropped so far: {dropped})");
                }
            }
        }
        queue.submit(RequestBuffer::new(URB_SIZE));
    }
}
