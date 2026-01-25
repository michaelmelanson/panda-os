//! Virtio block device driver with async I/O support.
//!
//! This driver supports both synchronous (busy-wait) and asynchronous
//! (interrupt-driven) I/O. Async I/O allows the calling process to be
//! blocked while other processes run.

use alloc::boxed::Box;
use alloc::collections::{BTreeMap, BTreeSet, VecDeque};
use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;
use async_trait::async_trait;
use core::future::Future;
use core::pin::Pin;
use core::task::{Context, Poll, Waker as TaskWaker};
use log::debug;
use spinning_top::{RwSpinlock, Spinlock};
use virtio_drivers::{
    Error as VirtioError, PhysAddr as VirtioPhysAddr,
    device::blk::{BlkReq as BlockRequest, BlkResp as BlockResponse, VirtIOBlk},
    transport::pci::{PciTransport, bus::PciRoot},
    transport::{DeviceStatus, DeviceType, Transport},
};
use x86_64::structures::idt::InterruptStackFrame;

/// A token representing a pending virtio request.
///
/// This wraps the raw `u16` token returned by virtio-drivers to provide
/// type safety and prevent accidental misuse of token values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct VirtioToken(u16);

impl VirtioToken {
    /// Create a new token from a raw virtio descriptor index.
    fn new(raw: u16) -> Self {
        Self(raw)
    }

    /// Get the raw token value for passing to virtio-drivers.
    fn raw(self) -> u16 {
        self.0
    }
}

use crate::apic;
use crate::device_address::DeviceAddress;
use crate::interrupts::{self, IrqHandlerFunc};
use crate::memory::dma::DmaBuffer;
use crate::pci::device::{PciDevice, VirtioCommonConfig};
use crate::process::ProcessId;
use crate::process::waker::Waker;
use crate::resource::{BlockDevice, BlockError};

use super::virtio_hal::VirtioHal;

/// A wrapper around PciTransport that configures MSI-X vectors before enabling queues.
///
/// The virtio spec requires MSI-X vectors to be set before queue_enable is written.
/// Since virtio-drivers doesn't expose hooks for this, we intercept queue_set to
/// configure MSI-X vectors at the right time.
pub struct MsixPciTransport {
    inner: PciTransport,
    common_config: Option<VirtioCommonConfig>,
    msix_vector: u16,
}

impl MsixPciTransport {
    /// Create a new MSI-X aware transport wrapper.
    pub fn new(
        inner: PciTransport,
        common_config: Option<VirtioCommonConfig>,
        msix_vector: u16,
    ) -> Self {
        Self {
            inner,
            common_config,
            msix_vector,
        }
    }
}

impl Transport for MsixPciTransport {
    fn device_type(&self) -> DeviceType {
        self.inner.device_type()
    }

    fn read_device_features(&mut self) -> u64 {
        self.inner.read_device_features()
    }

    fn write_driver_features(&mut self, driver_features: u64) {
        self.inner.write_driver_features(driver_features)
    }

    fn max_queue_size(&mut self, queue: u16) -> u32 {
        self.inner.max_queue_size(queue)
    }

    fn notify(&mut self, queue: u16) {
        self.inner.notify(queue);
    }

    fn get_status(&self) -> DeviceStatus {
        self.inner.get_status()
    }

    fn set_status(&mut self, status: DeviceStatus) {
        // Before setting DRIVER_OK, configure the msix_config vector
        if status.contains(DeviceStatus::DRIVER_OK) {
            if let Some(ref common_config) = self.common_config {
                debug!(
                    "MsixPciTransport: Setting msix_config to {} before DRIVER_OK",
                    self.msix_vector
                );
                common_config.set_config_msix_vector(self.msix_vector);
            }
        }
        self.inner.set_status(status)
    }

    fn set_guest_page_size(&mut self, guest_page_size: u32) {
        self.inner.set_guest_page_size(guest_page_size)
    }

    fn requires_legacy_layout(&self) -> bool {
        self.inner.requires_legacy_layout()
    }

    fn queue_set(
        &mut self,
        queue: u16,
        size: u32,
        descriptors: VirtioPhysAddr,
        driver_area: VirtioPhysAddr,
        device_area: VirtioPhysAddr,
    ) {
        // Configure MSI-X vector for this queue BEFORE the inner transport enables it
        if let Some(ref common_config) = self.common_config {
            debug!(
                "MsixPciTransport: Setting queue {} MSI-X vector to {} before enable",
                queue, self.msix_vector
            );
            common_config.set_queue_select(queue);
            common_config.set_queue_msix_vector(self.msix_vector);
        }

        // Now delegate to inner transport which will enable the queue
        self.inner
            .queue_set(queue, size, descriptors, driver_area, device_area)
    }

    fn queue_unset(&mut self, queue: u16) {
        self.inner.queue_unset(queue)
    }

    fn queue_used(&mut self, queue: u16) -> bool {
        self.inner.queue_used(queue)
    }

    fn ack_interrupt(&mut self) -> virtio_drivers::transport::InterruptStatus {
        self.inner.ack_interrupt()
    }

    fn read_config_generation(&self) -> u32 {
        self.inner.read_config_generation()
    }

    fn read_config_space<T: zerocopy::FromBytes + zerocopy::IntoBytes>(
        &self,
        offset: usize,
    ) -> virtio_drivers::Result<T> {
        self.inner.read_config_space(offset)
    }

    fn write_config_space<T: zerocopy::IntoBytes + zerocopy::Immutable>(
        &mut self,
        offset: usize,
        value: T,
    ) -> virtio_drivers::Result<()> {
        self.inner.write_config_space(offset, value)
    }
}

/// Operation type for pending/queued requests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockRequestOperation {
    Read,
    Write,
}

/// A pending request that has been submitted to the virtio device.
struct PendingBlockRequest {
    /// Virtio descriptor token (stored for reference, used as map key).
    #[allow(dead_code)]
    token: VirtioToken,
    /// Operation type.
    operation: BlockRequestOperation,
    /// Starting sector (stored for debugging/future use).
    #[allow(dead_code)]
    sector: u64,
    /// Process ID of the requesting process.
    process_id: ProcessId,
    /// Waker for the blocked process.
    waker: Arc<Waker>,
    /// DMA buffer owned by kernel.
    dma_buffer: DmaBuffer,
    /// Virtio request header.
    request_header: BlockRequest,
    /// Virtio response status.
    response_status: BlockResponse,
}

/// A queued request waiting for virtio queue space.
struct QueuedBlockRequest {
    /// Operation type.
    operation: BlockRequestOperation,
    /// Starting sector.
    sector: u64,
    /// Process ID of the requesting process.
    process_id: ProcessId,
    /// Waker for the blocked process.
    waker: Arc<Waker>,
    /// DMA buffer with data (for writes, contains data to write).
    dma_buffer: DmaBuffer,
}

/// A virtio block device with async I/O support.
pub struct VirtioBlockDevice {
    device: VirtIOBlk<VirtioHal, MsixPciTransport>,
    address: DeviceAddress,
    capacity_sectors: u64,
    sector_size: u32,
    /// Active requests submitted to virtio, keyed by token.
    pending_requests: BTreeMap<VirtioToken, PendingBlockRequest>,
    /// Queued requests waiting for virtio queue space (FIFO).
    queued_requests: VecDeque<QueuedBlockRequest>,
    // =========================================================================
    // Async future-based I/O state (new model)
    // =========================================================================
    /// Wakers for futures waiting on I/O completion, keyed by virtio token.
    /// The future owns its DMA buffer; the device just needs to wake it.
    async_wakers: BTreeMap<VirtioToken, TaskWaker>,
    /// Tokens that have completed and are ready to be picked up by futures.
    completed_tokens: BTreeSet<VirtioToken>,
}

impl VirtioBlockDevice {
    /// Get the device address.
    pub fn address(&self) -> &DeviceAddress {
        &self.address
    }

    /// Get the sector size in bytes.
    pub fn sector_size(&self) -> u32 {
        self.sector_size
    }

    /// Get the total capacity in sectors.
    pub fn capacity_sectors(&self) -> u64 {
        self.capacity_sectors
    }

    /// Disable device interrupts (for sync I/O to avoid deadlock).
    pub fn disable_interrupts(&mut self) {
        self.device.disable_interrupts();
    }

    /// Enable device interrupts.
    pub fn enable_interrupts(&mut self) {
        self.device.enable_interrupts();
    }

    /// Read a single block synchronously (busy-wait).
    /// The buffer must be exactly sector_size bytes.
    pub fn read_block_sync(&mut self, sector: u64, buf: &mut [u8]) -> Result<(), ()> {
        self.device
            .read_blocks(sector as usize, buf)
            .map_err(|_| ())
    }

    /// Write a single block synchronously (busy-wait).
    /// The buffer must be exactly sector_size bytes.
    pub fn write_block_sync(&mut self, sector: u64, buf: &[u8]) -> Result<(), ()> {
        self.device
            .write_blocks(sector as usize, buf)
            .map_err(|_| ())
    }

    /// Try to submit a queued request to the device.
    /// Returns the token if successful, or an error.
    fn try_submit_queued(
        &mut self,
        queued: &mut QueuedBlockRequest,
        request_header: &mut BlockRequest,
        response_status: &mut BlockResponse,
    ) -> Result<VirtioToken, VirtioError> {
        let raw_token = match queued.operation {
            BlockRequestOperation::Read => unsafe {
                self.device.read_blocks_nb(
                    queued.sector as usize,
                    request_header,
                    queued.dma_buffer.as_mut_slice(),
                    response_status,
                )
            },
            BlockRequestOperation::Write => unsafe {
                self.device.write_blocks_nb(
                    queued.sector as usize,
                    request_header,
                    queued.dma_buffer.as_slice(),
                    response_status,
                )
            },
        }?;
        Ok(VirtioToken::new(raw_token))
    }

    /// Peek at the next completed token without consuming it.
    fn peek_completed_token(&mut self) -> Option<VirtioToken> {
        self.device.peek_used().map(VirtioToken::new)
    }

    /// Process completed requests and try to submit queued ones.
    /// Called from IRQ handler.
    pub fn process_completions(&mut self) {
        // Always acknowledge the interrupt first to de-assert the interrupt line
        // (critical for level-triggered interrupts to avoid interrupt storms)
        let isr = self.device.ack_interrupt();

        // Check if any request is pending
        let has_pending = !self.pending_requests.is_empty();
        let has_async = !self.async_wakers.is_empty();
        let peek = self.peek_completed_token();
        if (has_pending || has_async) && peek.is_some() {
            debug!(
                "process_completions: has_pending={}, has_async={}, peek_used={:?}, isr_bits={:#x}",
                has_pending,
                has_async,
                peek,
                isr.bits()
            );
        }

        // Wake processes/futures with completed requests
        while let Some(token) = self.peek_completed_token() {
            debug!("process_completions: found completed token {:?}", token);

            // Check old-style pending requests (process-based)
            if let Some(pending) = self.pending_requests.get(&token) {
                debug!(
                    "process_completions: waking process {:?}",
                    pending.process_id
                );
                pending.waker.wake();
                // Don't remove from pending_requests here - the woken process
                // will do that in complete_pending_read/write
                break; // Only process one per call to avoid holding lock too long
            }

            // Check new-style async wakers (future-based)
            if let Some(waker) = self.async_wakers.remove(&token) {
                debug!(
                    "process_completions: waking async future for token {:?}",
                    token
                );
                self.completed_tokens.insert(token);
                waker.wake();
                break; // Only process one per call
            }

            debug!(
                "process_completions: token {:?} not in pending_requests or async_wakers",
                token
            );
            break;
        }

        // Try to submit queued requests
        while let Some(mut queued) = self.queued_requests.pop_front() {
            let mut request_header = BlockRequest::default();
            let mut response_status = BlockResponse::default();

            match self.try_submit_queued(&mut queued, &mut request_header, &mut response_status) {
                Ok(token) => {
                    // Successfully submitted - move to pending
                    self.pending_requests.insert(
                        token,
                        PendingBlockRequest {
                            token,
                            operation: queued.operation,
                            sector: queued.sector,
                            process_id: queued.process_id,
                            waker: queued.waker,
                            dma_buffer: queued.dma_buffer,
                            request_header,
                            response_status,
                        },
                    );
                    // Continue trying to submit more
                }
                Err(VirtioError::QueueFull) => {
                    // Queue still full - put it back and stop
                    self.queued_requests.push_front(queued);
                    break;
                }
                Err(_) => {
                    // Other error - wake the process so it sees the error on retry
                    queued.waker.wake();
                }
            }
        }
    }
}

/// Global registry of block devices keyed by DeviceAddress.
static BLOCK_DEVICES: RwSpinlock<BTreeMap<DeviceAddress, Arc<Spinlock<VirtioBlockDevice>>>> =
    RwSpinlock::new(BTreeMap::new());

/// Get a block device by its device address.
pub fn get_device(address: &DeviceAddress) -> Option<Arc<Spinlock<VirtioBlockDevice>>> {
    BLOCK_DEVICES.read().get(address).cloned()
}

/// List all block device addresses.
pub fn list_devices() -> Vec<DeviceAddress> {
    BLOCK_DEVICES.read().keys().cloned().collect()
}

/// IRQ handler for virtio block device interrupts.
extern "x86-interrupt" fn block_irq_handler(_stack_frame: InterruptStackFrame) {
    // Try to process completions, but don't block if lock is held
    // (sync I/O holds the lock while busy-waiting)
    poll_all_nonblocking();
    // Send end-of-interrupt
    apic::eoi();
}

/// Poll all block devices for completed requests (non-blocking).
/// This version uses try_lock to avoid deadlock with sync I/O.
fn poll_all_nonblocking() {
    let devices = BLOCK_DEVICES.read();
    for device in devices.values() {
        // Use try_lock to avoid deadlock - sync I/O holds the lock
        if let Some(mut dev) = device.try_lock() {
            dev.process_completions();
        }
        // If lock is held, the holder is doing sync I/O and will
        // poll completion themselves via busy-wait
    }
}

/// Poll all block devices for completed requests.
pub fn poll_all() {
    let devices = BLOCK_DEVICES.read();
    for device in devices.values() {
        device.lock().process_completions();
    }
}

// =============================================================================
// Async Future-based I/O Implementation
// =============================================================================

/// State of an async read operation.
enum AsyncReadState {
    /// Initial state - not yet submitted to device.
    NotSubmitted,
    /// Request submitted to device, waiting for completion.
    Submitted { token: VirtioToken },
    /// Request completed, ready to copy data.
    Completed { token: VirtioToken },
}

/// Future for an async block read operation.
///
/// This future owns its DMA buffer and virtio request/response headers.
/// When polled, it submits the request (if not yet submitted) and checks
/// for completion. The IRQ handler wakes the future when I/O completes.
struct VirtioReadFuture {
    device: Arc<Spinlock<VirtioBlockDevice>>,
    sector: u64,
    buf_ptr: *mut u8,
    buf_len: usize,
    dma_buffer: Option<DmaBuffer>,
    request_header: BlockRequest,
    response_status: BlockResponse,
    state: AsyncReadState,
}

// Safety: The raw pointer is only accessed during poll while we hold the device lock.
// The DMA buffer is owned by the future and lives until the future completes.
unsafe impl Send for VirtioReadFuture {}
unsafe impl Sync for VirtioReadFuture {}

impl VirtioReadFuture {
    fn new(device: Arc<Spinlock<VirtioBlockDevice>>, sector: u64, buf: &mut [u8]) -> Self {
        Self {
            device,
            sector,
            buf_ptr: buf.as_mut_ptr(),
            buf_len: buf.len(),
            dma_buffer: None,
            request_header: BlockRequest::default(),
            response_status: BlockResponse::default(),
            state: AsyncReadState::NotSubmitted,
        }
    }
}

impl Future for VirtioReadFuture {
    type Output = Result<usize, BlockError>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = &mut *self;

        match this.state {
            AsyncReadState::NotSubmitted => {
                // Allocate DMA buffer
                this.dma_buffer = Some(DmaBuffer::new(this.buf_len));

                let mut device = this.device.lock();

                // Try to submit the request
                let raw_token = match unsafe {
                    device.device.read_blocks_nb(
                        this.sector as usize,
                        &mut this.request_header,
                        this.dma_buffer.as_mut().unwrap().as_mut_slice(),
                        &mut this.response_status,
                    )
                } {
                    Ok(t) => t,
                    Err(VirtioError::QueueFull) => {
                        // Queue full - register waker and return pending
                        // The IRQ handler will wake us when there's space
                        // For now, just return pending - we'll retry on next poll
                        return Poll::Pending;
                    }
                    Err(_) => return Poll::Ready(Err(BlockError::IoError)),
                };
                let token = VirtioToken::new(raw_token);

                // Check if it completed immediately (synchronous completion)
                if device.device.peek_used() == Some(raw_token) {
                    this.state = AsyncReadState::Completed { token };
                    drop(device);
                    return self.poll(cx);
                }

                // Request is pending - register waker and transition state
                device.async_wakers.insert(token, cx.waker().clone());
                this.state = AsyncReadState::Submitted { token };
                Poll::Pending
            }

            AsyncReadState::Submitted { token } => {
                let mut device = this.device.lock();

                // Check if completed
                if device.completed_tokens.remove(&token) {
                    this.state = AsyncReadState::Completed { token };
                    drop(device);
                    return self.poll(cx);
                }

                // Still pending - re-register waker (may have changed)
                device.async_wakers.insert(token, cx.waker().clone());
                Poll::Pending
            }

            AsyncReadState::Completed { token } => {
                let mut device = this.device.lock();

                // Complete the virtio request
                let result = unsafe {
                    device.device.complete_read_blocks(
                        token.raw(),
                        &this.request_header,
                        this.dma_buffer.as_mut().unwrap().as_mut_slice(),
                        &mut this.response_status,
                    )
                };

                if result.is_err() {
                    return Poll::Ready(Err(BlockError::IoError));
                }

                // Copy from DMA buffer to user buffer
                let buf = unsafe { core::slice::from_raw_parts_mut(this.buf_ptr, this.buf_len) };
                buf.copy_from_slice(this.dma_buffer.as_ref().unwrap().as_slice());

                Poll::Ready(Ok(this.buf_len))
            }
        }
    }
}

/// State of an async write operation.
enum AsyncWriteState {
    NotSubmitted,
    Submitted { token: VirtioToken },
    Completed { token: VirtioToken },
}

/// Future for an async block write operation.
struct VirtioWriteFuture {
    device: Arc<Spinlock<VirtioBlockDevice>>,
    sector: u64,
    buf_len: usize,
    dma_buffer: Option<DmaBuffer>,
    request_header: BlockRequest,
    response_status: BlockResponse,
    state: AsyncWriteState,
}

unsafe impl Send for VirtioWriteFuture {}
unsafe impl Sync for VirtioWriteFuture {}

impl VirtioWriteFuture {
    fn new(device: Arc<Spinlock<VirtioBlockDevice>>, sector: u64, buf: &[u8]) -> Self {
        // Allocate DMA buffer and copy data immediately
        let mut dma_buffer = DmaBuffer::new(buf.len());
        dma_buffer.as_mut_slice().copy_from_slice(buf);

        Self {
            device,
            sector,
            buf_len: buf.len(),
            dma_buffer: Some(dma_buffer),
            request_header: BlockRequest::default(),
            response_status: BlockResponse::default(),
            state: AsyncWriteState::NotSubmitted,
        }
    }
}

impl Future for VirtioWriteFuture {
    type Output = Result<usize, BlockError>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = &mut *self;

        match this.state {
            AsyncWriteState::NotSubmitted => {
                let mut device = this.device.lock();

                let raw_token = match unsafe {
                    device.device.write_blocks_nb(
                        this.sector as usize,
                        &mut this.request_header,
                        this.dma_buffer.as_ref().unwrap().as_slice(),
                        &mut this.response_status,
                    )
                } {
                    Ok(t) => t,
                    Err(VirtioError::QueueFull) => {
                        return Poll::Pending;
                    }
                    Err(_) => return Poll::Ready(Err(BlockError::IoError)),
                };
                let token = VirtioToken::new(raw_token);

                if device.device.peek_used() == Some(raw_token) {
                    this.state = AsyncWriteState::Completed { token };
                    drop(device);
                    return self.poll(cx);
                }

                device.async_wakers.insert(token, cx.waker().clone());
                this.state = AsyncWriteState::Submitted { token };
                Poll::Pending
            }

            AsyncWriteState::Submitted { token } => {
                let mut device = this.device.lock();

                if device.completed_tokens.remove(&token) {
                    this.state = AsyncWriteState::Completed { token };
                    drop(device);
                    return self.poll(cx);
                }

                device.async_wakers.insert(token, cx.waker().clone());
                Poll::Pending
            }

            AsyncWriteState::Completed { token } => {
                let mut device = this.device.lock();

                let result = unsafe {
                    device.device.complete_write_blocks(
                        token.raw(),
                        &this.request_header,
                        this.dma_buffer.as_ref().unwrap().as_slice(),
                        &mut this.response_status,
                    )
                };

                if result.is_err() {
                    return Poll::Ready(Err(BlockError::IoError));
                }

                Poll::Ready(Ok(this.buf_len))
            }
        }
    }
}

/// Wrapper that implements AsyncBlockDevice for a virtio block device.
///
/// This provides byte-level async access with automatic sector alignment.
pub struct AsyncVirtioBlockDevice {
    device: Arc<Spinlock<VirtioBlockDevice>>,
}

impl AsyncVirtioBlockDevice {
    /// Create a new async wrapper around a virtio block device.
    pub fn new(device: Arc<Spinlock<VirtioBlockDevice>>) -> Self {
        Self { device }
    }
}

#[async_trait]
impl BlockDevice for AsyncVirtioBlockDevice {
    async fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<usize, BlockError> {
        if buf.is_empty() {
            return Ok(0);
        }

        let (sector_size, total_size) = {
            let dev = self.device.lock();
            (
                dev.sector_size as u64,
                dev.capacity_sectors * dev.sector_size as u64,
            )
        };

        if offset >= total_size {
            return Ok(0);
        }

        let available = total_size - offset;
        let to_read = (buf.len() as u64).min(available) as usize;

        let start_sector = offset / sector_size;
        let offset_in_sector = (offset % sector_size) as usize;
        let end_offset = offset + to_read as u64;
        let end_sector = (end_offset + sector_size - 1) / sector_size;
        let num_sectors = end_sector - start_sector;

        // Fast path: aligned read
        if offset_in_sector == 0 && to_read % sector_size as usize == 0 {
            VirtioReadFuture::new(self.device.clone(), start_sector, &mut buf[..to_read]).await?;
            return Ok(to_read);
        }

        // Slow path: unaligned read
        let mut sector_buf = vec![0u8; (num_sectors * sector_size) as usize];
        VirtioReadFuture::new(self.device.clone(), start_sector, &mut sector_buf).await?;
        buf[..to_read].copy_from_slice(&sector_buf[offset_in_sector..offset_in_sector + to_read]);

        Ok(to_read)
    }

    async fn write_at(&self, offset: u64, buf: &[u8]) -> Result<usize, BlockError> {
        if buf.is_empty() {
            return Ok(0);
        }

        let (sector_size, total_size) = {
            let dev = self.device.lock();
            (
                dev.sector_size as u64,
                dev.capacity_sectors * dev.sector_size as u64,
            )
        };

        if offset >= total_size {
            return Err(BlockError::InvalidOffset);
        }

        let available = total_size - offset;
        let to_write = (buf.len() as u64).min(available) as usize;

        let start_sector = offset / sector_size;
        let offset_in_sector = (offset % sector_size) as usize;
        let end_offset = offset + to_write as u64;
        let end_sector = (end_offset + sector_size - 1) / sector_size;
        let num_sectors = end_sector - start_sector;

        // Fast path: aligned write
        if offset_in_sector == 0 && to_write % sector_size as usize == 0 {
            VirtioWriteFuture::new(self.device.clone(), start_sector, &buf[..to_write]).await?;
            return Ok(to_write);
        }

        // Slow path: unaligned write (read-modify-write)
        let mut sector_buf = vec![0u8; (num_sectors * sector_size) as usize];
        VirtioReadFuture::new(self.device.clone(), start_sector, &mut sector_buf).await?;
        sector_buf[offset_in_sector..offset_in_sector + to_write].copy_from_slice(&buf[..to_write]);
        VirtioWriteFuture::new(self.device.clone(), start_sector, &sector_buf).await?;

        Ok(to_write)
    }

    fn size(&self) -> u64 {
        let dev = self.device.lock();
        dev.capacity_sectors * dev.sector_size as u64
    }

    fn sector_size(&self) -> u32 {
        self.device.lock().sector_size
    }

    async fn sync(&self) -> Result<(), BlockError> {
        Ok(()) // virtio-blk is write-through
    }
}

/// Get an async block device wrapper by device address.
pub fn get_async_device(address: &DeviceAddress) -> Option<AsyncVirtioBlockDevice> {
    get_device(address).map(AsyncVirtioBlockDevice::new)
}

/// The interrupt vector used for virtio block MSI-X interrupts.
const VIRTIO_BLOCK_MSIX_VECTOR: u8 = 0x30;

/// Initialize a virtio block device from a PCI device.
pub fn init_from_pci_device(pci_device: PciDevice) {
    let pci_address = pci_device.address();
    let address = DeviceAddress::Pci {
        bus: pci_address.bus,
        device: pci_address.slot,
        function: pci_address.function,
    };

    debug!("Initializing virtio block device at {}", address);

    // Try to enable MSI-X before creating the transport
    let msix_cap = pci_device.enable_msix();
    let virtio_common_config = VirtioCommonConfig::find(&pci_device);

    if let Some(ref cap) = msix_cap {
        debug!("MSI-X enabled with {} vectors", cap.table_size());
        // Configure MSI-X table entry 0 to deliver VIRTIO_BLOCK_MSIX_VECTOR to CPU 0
        cap.configure_entry(0, VIRTIO_BLOCK_MSIX_VECTOR, 0);

        // Register interrupt handler BEFORE any I/O that might trigger interrupts
        debug!(
            "Registering MSI-X handler for vector {:#x}",
            VIRTIO_BLOCK_MSIX_VECTOR
        );
        interrupts::set_interrupt_handler(
            VIRTIO_BLOCK_MSIX_VECTOR,
            Some(block_irq_handler as IrqHandlerFunc),
        );
    } else {
        debug!("MSI-X not available, will use legacy interrupts");
    }

    // Create the PCI transport
    let mut root = PciRoot::new(pci_device.clone());
    let device_function = pci_address.into();
    let inner_transport = PciTransport::new::<VirtioHal, PciDevice>(&mut root, device_function)
        .expect("Could not create PCI transport for virtio block device");

    // Wrap in MsixPciTransport to configure MSI-X vectors at the right time
    // The wrapper intercepts queue_set and set_status to configure vectors before
    // queue_enable and DRIVER_OK respectively (as required by virtio spec)
    let transport = MsixPciTransport::new(inner_transport, virtio_common_config.clone(), 0);

    let device = VirtIOBlk::<VirtioHal, MsixPciTransport>::new(transport)
        .expect("Could not initialize virtio block device");

    // Verify MSI-X vectors were configured correctly
    if let Some(ref common_config) = virtio_common_config {
        let status_after = common_config.read_device_status();
        let current_config = common_config.read_msix_config();
        let current_queue_vec = common_config.read_queue_msix_vector(0);
        let num_queues = common_config.read_num_queues();
        debug!(
            "After device init: device_status={:#x}, num_queues={}, msix_config={:#x}, queue_msix_vector={:#x}",
            status_after, num_queues, current_config, current_queue_vec
        );
    }

    // Debug: read back PCI command register to check bus master bit
    let cmd = pci_device.command();
    debug!(
        "PCI command register: {:#06x} (bus master = {})",
        cmd,
        (cmd & 0x4) != 0
    );

    let capacity_sectors = device.capacity();
    // VirtIO block devices use 512-byte sectors by default
    let sector_size = 512u32;

    debug!(
        "Virtio block device: {} sectors, {} bytes/sector, total {} bytes",
        capacity_sectors,
        sector_size,
        capacity_sectors * sector_size as u64
    );

    let block_device = VirtioBlockDevice {
        device,
        address: address.clone(),
        capacity_sectors,
        sector_size,
        pending_requests: BTreeMap::new(),
        queued_requests: VecDeque::new(),
        async_wakers: BTreeMap::new(),
        completed_tokens: BTreeSet::new(),
    };

    let block_device = Arc::new(Spinlock::new(block_device));

    // Register in global map
    BLOCK_DEVICES.write().insert(address, block_device);

    // Set up interrupt handler for legacy INTx if not using MSI-X
    // (MSI-X handler was registered earlier, before device init)
    if msix_cap.is_none() {
        let irq_line = pci_device.interrupt_line();
        let irq_pin = pci_device.interrupt_pin();
        if irq_pin != 0 && irq_line != 0 && irq_line != 0xFF {
            debug!("Registering legacy IRQ handler for IRQ {}", irq_line);
            interrupts::set_irq_handler(irq_line, Some(block_irq_handler as IrqHandlerFunc));
        }
    }

    debug!("Virtio block device initialized with async I/O support");
}
