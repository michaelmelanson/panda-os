//! Virtio block device driver with async I/O support.
//!
//! This driver supports both synchronous (busy-wait) and asynchronous
//! (interrupt-driven) I/O. Async I/O allows the calling process to be
//! blocked while other processes run.
//!
//! # Module Structure
//!
//! - [`transport`]: MSI-X aware PCI transport wrapper
//! - [`request`]: Request types for I/O operations
//! - [`futures`]: Async futures for block I/O

mod futures;
mod request;
mod transport;

use alloc::boxed::Box;
use alloc::collections::{BTreeMap, BTreeSet, VecDeque};
use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;
use async_trait::async_trait;
use core::task::Waker as TaskWaker;
use log::debug;
use spinning_top::{RwSpinlock, Spinlock};
use virtio_drivers::{
    Error as VirtioError,
    device::blk::{BlkReq as BlockRequest, BlkResp as BlockResponse, VirtIOBlk},
    transport::pci::{PciTransport, bus::PciRoot},
};
use x86_64::structures::idt::InterruptStackFrame;

use crate::apic;
use crate::device_address::DeviceAddress;
use crate::interrupts::{self, IrqHandlerFunc};
use crate::memory::dma::DmaBuffer;
use crate::pci::VirtioCommonConfig;
use crate::pci::device::PciDevice;
use crate::resource::{BlockDevice, BlockError};

use super::virtio_hal::VirtioHal;

pub use self::futures::{ReadOp, VirtioBlockFuture, WriteOp};
pub use self::request::BlockRequestOperation;
pub use self::transport::MsixPciTransport;

use self::request::{CancelledRequest, PendingBlockRequest, QueuedBlockRequest};

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

/// Inner state for a virtio block device.
///
/// This is wrapped by `VirtioBlockDevice` which provides the public async API.
pub(crate) struct VirtioBlockDeviceInner {
    pub(crate) device: VirtIOBlk<VirtioHal, MsixPciTransport>,
    /// Device address (stored for future device management APIs)
    #[allow(dead_code)]
    address: DeviceAddress,
    capacity_sectors: u64,
    sector_size: u32,
    /// Active requests submitted to virtio, keyed by token.
    pending_requests: BTreeMap<VirtioToken, PendingBlockRequest>,
    /// Queued requests waiting for virtio queue space (FIFO).
    queued_requests: VecDeque<QueuedBlockRequest>,
    /// Wakers for futures waiting on I/O completion, keyed by virtio token.
    pub(crate) async_wakers: BTreeMap<VirtioToken, TaskWaker>,
    /// Tokens that have completed and are ready to be picked up by futures.
    pub(crate) completed_tokens: BTreeSet<VirtioToken>,
    /// Cancelled requests with in-flight I/O.
    cancelled_requests: BTreeMap<VirtioToken, CancelledRequest>,
    /// Wakers for futures that hit QueueFull and need to retry submission.
    pub(crate) queue_full_wakers: Vec<TaskWaker>,
}

impl VirtioBlockDeviceInner {
    /// Get the device address.
    #[allow(dead_code)]
    pub fn address(&self) -> &DeviceAddress {
        &self.address
    }

    /// Get the sector size in bytes.
    #[allow(dead_code)]
    pub fn sector_size(&self) -> u32 {
        self.sector_size
    }

    /// Get the total capacity in sectors.
    #[allow(dead_code)]
    pub fn capacity_sectors(&self) -> u64 {
        self.capacity_sectors
    }

    /// Disable device interrupts (for sync I/O to avoid deadlock).
    #[allow(dead_code)]
    pub fn disable_interrupts(&mut self) {
        self.device.disable_interrupts();
    }

    /// Enable device interrupts.
    #[allow(dead_code)]
    pub fn enable_interrupts(&mut self) {
        self.device.enable_interrupts();
    }

    /// Register a cancelled request. Called when a future is dropped while I/O is in flight.
    pub fn register_cancelled(
        &mut self,
        token: VirtioToken,
        dma_buffer: DmaBuffer,
        request_header: BlockRequest,
        response_status: BlockResponse,
        is_read: bool,
    ) {
        // Remove from async_wakers since the future is gone
        self.async_wakers.remove(&token);
        // Remove from completed_tokens if it completed before we could clean up
        self.completed_tokens.remove(&token);
        // Store the cancelled request
        self.cancelled_requests.insert(
            token,
            CancelledRequest {
                dma_buffer,
                request_header,
                response_status,
                is_read,
            },
        );
    }

    /// Try to submit a queued request to the device.
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
    pub fn process_completions(&mut self) {
        // Always acknowledge the interrupt first
        let isr = self.device.ack_interrupt();

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
                break;
            }

            // Check new-style async wakers (future-based)
            if let Some(waker) = self.async_wakers.remove(&token) {
                debug!(
                    "process_completions: waking async future for token {:?}",
                    token
                );
                self.completed_tokens.insert(token);
                waker.wake();
                break;
            }

            // Check cancelled requests (futures dropped mid-flight)
            if let Some(mut cancelled) = self.cancelled_requests.remove(&token) {
                debug!(
                    "process_completions: cleaning up cancelled request for token {:?}",
                    token
                );
                let _ = if cancelled.is_read {
                    unsafe {
                        self.device.complete_read_blocks(
                            token.raw(),
                            &cancelled.request_header,
                            cancelled.dma_buffer.as_mut_slice(),
                            &mut cancelled.response_status,
                        )
                    }
                } else {
                    unsafe {
                        self.device.complete_write_blocks(
                            token.raw(),
                            &cancelled.request_header,
                            cancelled.dma_buffer.as_slice(),
                            &mut cancelled.response_status,
                        )
                    }
                };
                break;
            }

            debug!(
                "process_completions: token {:?} not found in any tracking map",
                token
            );
            break;
        }

        // Wake any futures that were blocked on a full queue — a completion
        // means there is now space to submit new requests.
        for waker in self.queue_full_wakers.drain(..) {
            waker.wake();
        }

        // Try to submit queued requests
        while let Some(mut queued) = self.queued_requests.pop_front() {
            let mut request_header = BlockRequest::default();
            let mut response_status = BlockResponse::default();

            match self.try_submit_queued(&mut queued, &mut request_header, &mut response_status) {
                Ok(token) => {
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
                }
                Err(VirtioError::QueueFull) => {
                    self.queued_requests.push_front(queued);
                    break;
                }
                Err(_) => {
                    queued.waker.wake();
                }
            }
        }
    }
}

/// Global registry of block devices keyed by DeviceAddress.
static BLOCK_DEVICES: RwSpinlock<BTreeMap<DeviceAddress, Arc<Spinlock<VirtioBlockDeviceInner>>>> =
    RwSpinlock::new(BTreeMap::new());

/// List all block device addresses.
pub fn list_devices() -> Vec<DeviceAddress> {
    BLOCK_DEVICES.read().keys().cloned().collect()
}

/// IRQ handler for virtio block device interrupts.
extern "x86-interrupt" fn block_irq_handler(_stack_frame: InterruptStackFrame) {
    poll_all_nonblocking();
    apic::eoi();
}

/// Poll all block devices for completed requests (non-blocking).
fn poll_all_nonblocking() {
    let devices = BLOCK_DEVICES.read();
    for device in devices.values() {
        if let Some(mut dev) = device.try_lock() {
            dev.process_completions();
        }
    }
}

/// Poll all block devices for completed requests.
pub fn poll_all() {
    let devices = BLOCK_DEVICES.read();
    for device in devices.values() {
        device.lock().process_completions();
    }
}

/// A virtio block device with async I/O support.
///
/// This provides byte-level async access with automatic sector alignment.
pub struct VirtioBlockDevice {
    inner: Arc<Spinlock<VirtioBlockDeviceInner>>,
}

impl VirtioBlockDevice {
    /// Create a new virtio block device wrapper.
    fn new(inner: Arc<Spinlock<VirtioBlockDeviceInner>>) -> Self {
        Self { inner }
    }
}

#[async_trait]
impl BlockDevice for VirtioBlockDevice {
    async fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<usize, BlockError> {
        if buf.is_empty() {
            return Ok(0);
        }

        let (sector_size, total_size) = {
            let dev = self.inner.lock();
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
            VirtioBlockFuture::<ReadOp>::new_read(
                self.inner.clone(),
                start_sector,
                &mut buf[..to_read],
            )
            .await?;
            return Ok(to_read);
        }

        // Slow path: unaligned read
        let mut sector_buf = vec![0u8; (num_sectors * sector_size) as usize];
        VirtioBlockFuture::<ReadOp>::new_read(self.inner.clone(), start_sector, &mut sector_buf)
            .await?;
        buf[..to_read].copy_from_slice(&sector_buf[offset_in_sector..offset_in_sector + to_read]);

        Ok(to_read)
    }

    async fn write_at(&self, offset: u64, buf: &[u8]) -> Result<usize, BlockError> {
        if buf.is_empty() {
            return Ok(0);
        }

        let (sector_size, total_size) = {
            let dev = self.inner.lock();
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
            VirtioBlockFuture::<WriteOp>::new_write(
                self.inner.clone(),
                start_sector,
                &buf[..to_write],
            )
            .await?;
            return Ok(to_write);
        }

        // Slow path: unaligned write (read-modify-write)
        let mut sector_buf = vec![0u8; (num_sectors * sector_size) as usize];
        VirtioBlockFuture::<ReadOp>::new_read(self.inner.clone(), start_sector, &mut sector_buf)
            .await?;
        sector_buf[offset_in_sector..offset_in_sector + to_write].copy_from_slice(&buf[..to_write]);
        VirtioBlockFuture::<WriteOp>::new_write(self.inner.clone(), start_sector, &sector_buf)
            .await?;

        Ok(to_write)
    }

    fn size(&self) -> u64 {
        let dev = self.inner.lock();
        dev.capacity_sectors * dev.sector_size as u64
    }

    fn sector_size(&self) -> u32 {
        self.inner.lock().sector_size
    }

    async fn sync(&self) -> Result<(), BlockError> {
        Ok(()) // virtio-blk is write-through
    }
}

/// Get a block device by its device address.
pub fn get_device(address: &DeviceAddress) -> Option<VirtioBlockDevice> {
    BLOCK_DEVICES
        .read()
        .get(address)
        .cloned()
        .map(VirtioBlockDevice::new)
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
        cap.configure_entry(0, VIRTIO_BLOCK_MSIX_VECTOR, 0);
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

    let cmd = pci_device.command();
    debug!(
        "PCI command register: {:#06x} (bus master = {})",
        cmd,
        (cmd & 0x4) != 0
    );

    let capacity_sectors = device.capacity();
    let sector_size = 512u32;

    debug!(
        "Virtio block device: {} sectors, {} bytes/sector, total {} bytes",
        capacity_sectors,
        sector_size,
        capacity_sectors * sector_size as u64
    );

    let block_device = VirtioBlockDeviceInner {
        device,
        address: address.clone(),
        capacity_sectors,
        sector_size,
        pending_requests: BTreeMap::new(),
        queued_requests: VecDeque::new(),
        async_wakers: BTreeMap::new(),
        completed_tokens: BTreeSet::new(),
        cancelled_requests: BTreeMap::new(),
        queue_full_wakers: Vec::new(),
    };

    let block_device = Arc::new(Spinlock::new(block_device));
    BLOCK_DEVICES.write().insert(address, block_device);

    // Set up interrupt handler for legacy INTx if not using MSI-X
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
