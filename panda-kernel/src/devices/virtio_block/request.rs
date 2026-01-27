//! Request types for virtio block I/O operations.

use alloc::sync::Arc;
use virtio_drivers::device::blk::{BlkReq as BlockRequest, BlkResp as BlockResponse};

use crate::memory::dma::DmaBuffer;
use crate::process::ProcessId;
use crate::process::waker::Waker;

use super::VirtioToken;

/// Operation type for pending/queued requests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockRequestOperation {
    Read,
    Write,
}

/// A pending request that has been submitted to the virtio device.
///
/// Some fields are stored for use when the async I/O completes but aren't
/// directly read by code (they're passed to virtio's complete_* functions).
#[allow(dead_code)]
pub(super) struct PendingBlockRequest {
    /// Virtio descriptor token (stored for reference, used as map key).
    pub token: VirtioToken,
    /// Operation type.
    pub operation: BlockRequestOperation,
    /// Starting sector (stored for debugging/future use).
    pub sector: u64,
    /// Process ID of the requesting process.
    pub process_id: ProcessId,
    /// Waker for the blocked process.
    pub waker: Arc<Waker>,
    /// DMA buffer owned by kernel.
    pub dma_buffer: DmaBuffer,
    /// Virtio request header.
    pub request_header: BlockRequest,
    /// Virtio response status.
    pub response_status: BlockResponse,
}

/// A queued request waiting for virtio queue space.
pub(super) struct QueuedBlockRequest {
    /// Operation type.
    pub operation: BlockRequestOperation,
    /// Starting sector.
    pub sector: u64,
    /// Process ID of the requesting process.
    pub process_id: ProcessId,
    /// Waker for the blocked process.
    pub waker: Arc<Waker>,
    /// DMA buffer with data (for writes, contains data to write).
    pub dma_buffer: DmaBuffer,
}

/// A cancelled async request that still has in-flight I/O.
///
/// When a future is dropped while I/O is in flight, we move the DMA buffer
/// here so it stays valid until the I/O completes. The IRQ handler will
/// clean these up when the I/O finishes.
pub(super) struct CancelledRequest {
    /// DMA buffer that must stay alive until I/O completes.
    pub dma_buffer: DmaBuffer,
    /// Virtio request header (needed for complete_*_blocks).
    pub request_header: BlockRequest,
    /// Virtio response status (needed for complete_*_blocks).
    pub response_status: BlockResponse,
    /// Whether this was a read or write (determines which complete function to call).
    pub is_read: bool,
}
