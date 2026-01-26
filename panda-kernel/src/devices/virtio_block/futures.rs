//! Async futures for virtio block I/O operations.

use alloc::sync::Arc;
use core::future::Future;
use core::marker::PhantomData;
use core::pin::Pin;
use core::task::{Context, Poll};
use spinning_top::Spinlock;
use virtio_drivers::Error as VirtioError;
use virtio_drivers::device::blk::{BlkReq as BlockRequest, BlkResp as BlockResponse};

use crate::memory::dma::DmaBuffer;
use crate::resource::BlockError;

use super::{VirtioBlockDeviceInner, VirtioToken};

/// Marker trait for I/O operation types.
pub trait IoOperation: Send + Sync {
    /// Whether this operation is a read (true) or write (false).
    const IS_READ: bool;
}

/// Marker type for read operations.
pub struct ReadOp;
impl IoOperation for ReadOp {
    const IS_READ: bool = true;
}

/// Marker type for write operations.
pub struct WriteOp;
impl IoOperation for WriteOp {
    const IS_READ: bool = false;
}

/// State of an async I/O operation.
enum AsyncIoState {
    /// Initial state - not yet submitted to device.
    NotSubmitted,
    /// Request submitted to device, waiting for completion.
    Submitted { token: VirtioToken },
    /// Request completed, ready to finalize.
    Completed { token: VirtioToken },
}

/// Future for an async block I/O operation.
///
/// This future owns its DMA buffer and virtio request/response headers.
/// When polled, it submits the request (if not yet submitted) and checks
/// for completion. The IRQ handler wakes the future when I/O completes.
///
/// The `Op` type parameter determines whether this is a read or write operation.
pub struct VirtioBlockFuture<Op: IoOperation> {
    device: Arc<Spinlock<VirtioBlockDeviceInner>>,
    sector: u64,
    /// For reads: pointer to destination buffer. For writes: unused (None).
    dst_ptr: Option<*mut u8>,
    buf_len: usize,
    dma_buffer: Option<DmaBuffer>,
    request_header: BlockRequest,
    response_status: BlockResponse,
    state: AsyncIoState,
    _marker: PhantomData<Op>,
}

// Safety: The raw pointer is only accessed during poll while we hold the device lock.
// The DMA buffer is owned by the future and lives until the future completes.
unsafe impl<Op: IoOperation> Send for VirtioBlockFuture<Op> {}
unsafe impl<Op: IoOperation> Sync for VirtioBlockFuture<Op> {}

// The future doesn't rely on pinning for safety - the raw pointer points to external memory,
// not self-referential data. Implementing Unpin allows us to use get_mut() on Pin<&mut Self>.
impl<Op: IoOperation> Unpin for VirtioBlockFuture<Op> {}

impl VirtioBlockFuture<ReadOp> {
    /// Create a new async read operation.
    pub fn new_read(
        device: Arc<Spinlock<VirtioBlockDeviceInner>>,
        sector: u64,
        buf: &mut [u8],
    ) -> Self {
        Self {
            device,
            sector,
            dst_ptr: Some(buf.as_mut_ptr()),
            buf_len: buf.len(),
            dma_buffer: None,
            request_header: BlockRequest::default(),
            response_status: BlockResponse::default(),
            state: AsyncIoState::NotSubmitted,
            _marker: PhantomData,
        }
    }
}

impl VirtioBlockFuture<WriteOp> {
    /// Create a new async write operation.
    ///
    /// The data is copied to a DMA buffer immediately.
    pub fn new_write(
        device: Arc<Spinlock<VirtioBlockDeviceInner>>,
        sector: u64,
        buf: &[u8],
    ) -> Self {
        // Allocate DMA buffer and copy data immediately
        let mut dma_buffer = DmaBuffer::new(buf.len());
        dma_buffer.as_mut_slice().copy_from_slice(buf);

        Self {
            device,
            sector,
            dst_ptr: None,
            buf_len: buf.len(),
            dma_buffer: Some(dma_buffer),
            request_header: BlockRequest::default(),
            response_status: BlockResponse::default(),
            state: AsyncIoState::NotSubmitted,
            _marker: PhantomData,
        }
    }
}

impl<Op: IoOperation> Future for VirtioBlockFuture<Op> {
    type Output = Result<usize, BlockError>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = &mut *self;

        match this.state {
            AsyncIoState::NotSubmitted => {
                // For reads, allocate DMA buffer now
                if Op::IS_READ && this.dma_buffer.is_none() {
                    this.dma_buffer = Some(DmaBuffer::new(this.buf_len));
                }

                let mut device = this.device.lock();

                // Try to submit the request
                let raw_token = if Op::IS_READ {
                    unsafe {
                        device.device.read_blocks_nb(
                            this.sector as usize,
                            &mut this.request_header,
                            this.dma_buffer.as_mut().unwrap().as_mut_slice(),
                            &mut this.response_status,
                        )
                    }
                } else {
                    unsafe {
                        device.device.write_blocks_nb(
                            this.sector as usize,
                            &mut this.request_header,
                            this.dma_buffer.as_ref().unwrap().as_slice(),
                            &mut this.response_status,
                        )
                    }
                };

                let raw_token = match raw_token {
                    Ok(t) => t,
                    Err(VirtioError::QueueFull) => {
                        // Queue full - register waker and return pending
                        return Poll::Pending;
                    }
                    Err(_) => return Poll::Ready(Err(BlockError::IoError)),
                };
                let token = VirtioToken::new(raw_token);

                // Check if it completed immediately (synchronous completion)
                if device.device.peek_used() == Some(raw_token) {
                    this.state = AsyncIoState::Completed { token };
                    drop(device);
                    return self.poll(cx);
                }

                // Request is pending - register waker and transition state
                device.async_wakers.insert(token, cx.waker().clone());
                this.state = AsyncIoState::Submitted { token };
                Poll::Pending
            }

            AsyncIoState::Submitted { token } => {
                let mut device = this.device.lock();

                // Check if completed
                if device.completed_tokens.remove(&token) {
                    this.state = AsyncIoState::Completed { token };
                    drop(device);
                    return self.poll(cx);
                }

                // Still pending - re-register waker (may have changed)
                device.async_wakers.insert(token, cx.waker().clone());
                Poll::Pending
            }

            AsyncIoState::Completed { token } => {
                let mut device = this.device.lock();

                // Complete the virtio request
                let result = if Op::IS_READ {
                    unsafe {
                        device.device.complete_read_blocks(
                            token.raw(),
                            &this.request_header,
                            this.dma_buffer.as_mut().unwrap().as_mut_slice(),
                            &mut this.response_status,
                        )
                    }
                } else {
                    unsafe {
                        device.device.complete_write_blocks(
                            token.raw(),
                            &this.request_header,
                            this.dma_buffer.as_ref().unwrap().as_slice(),
                            &mut this.response_status,
                        )
                    }
                };

                if result.is_err() {
                    return Poll::Ready(Err(BlockError::IoError));
                }

                // For reads, copy from DMA buffer to user buffer
                if Op::IS_READ {
                    if let Some(dst_ptr) = this.dst_ptr {
                        let buf = unsafe { core::slice::from_raw_parts_mut(dst_ptr, this.buf_len) };
                        buf.copy_from_slice(this.dma_buffer.as_ref().unwrap().as_slice());
                    }
                }

                Poll::Ready(Ok(this.buf_len))
            }
        }
    }
}

impl<Op: IoOperation> Drop for VirtioBlockFuture<Op> {
    fn drop(&mut self) {
        // If we have a submitted request, we need to keep the DMA buffer alive
        // until the I/O completes. Register it with the device.
        if let AsyncIoState::Submitted { token } = self.state {
            if let Some(dma_buffer) = self.dma_buffer.take() {
                let mut device = self.device.lock();
                device.register_cancelled(
                    token,
                    dma_buffer,
                    core::mem::take(&mut self.request_header),
                    core::mem::take(&mut self.response_status),
                    Op::IS_READ,
                );
            }
        }
        // For NotSubmitted or Completed states, normal drop is fine
    }
}
