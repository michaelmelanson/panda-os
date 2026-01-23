//! Virtio block device driver with async I/O support.
//!
//! This driver supports both synchronous (busy-wait) and asynchronous
//! (interrupt-driven) I/O. Async I/O allows the calling process to be
//! blocked while other processes run.

use alloc::collections::{BTreeMap, VecDeque};
use alloc::sync::Arc;
use alloc::vec::Vec;
use log::debug;
use spinning_top::{RwSpinlock, Spinlock};
use virtio_drivers::{
    Error as VirtioError, PhysAddr as VirtioPhysAddr,
    device::blk::{BlkReq as BlockRequest, BlkResp as BlockResponse, VirtIOBlk},
    transport::pci::{PciTransport, bus::PciRoot},
    transport::{DeviceStatus, DeviceType, Transport},
};
use x86_64::structures::idt::InterruptStackFrame;

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
    token: u16,
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
    pending_requests: BTreeMap<u16, PendingBlockRequest>,
    /// Queued requests waiting for virtio queue space (FIFO).
    queued_requests: VecDeque<QueuedBlockRequest>,
}

impl VirtioBlockDevice {
    /// Get the device address.
    pub fn address(&self) -> &DeviceAddress {
        &self.address
    }

    /// Try to submit a queued request to the device.
    /// Returns the token if successful, or an error.
    fn try_submit_queued(
        &mut self,
        queued: &mut QueuedBlockRequest,
        request_header: &mut BlockRequest,
        response_status: &mut BlockResponse,
    ) -> Result<u16, VirtioError> {
        match queued.operation {
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
        }
    }

    /// Process completed requests and try to submit queued ones.
    /// Called from IRQ handler.
    pub fn process_completions(&mut self) {
        // Always acknowledge the interrupt first to de-assert the interrupt line
        // (critical for level-triggered interrupts to avoid interrupt storms)
        let isr = self.device.ack_interrupt();

        // Check if any request is pending
        let has_pending = !self.pending_requests.is_empty();
        let peek = self.device.peek_used();
        if has_pending && peek.is_some() {
            debug!(
                "process_completions: has_pending=true, peek_used={:?}, isr_bits={:#x}",
                peek,
                isr.bits()
            );
        }

        // Wake processes with completed requests
        while let Some(token) = self.device.peek_used() {
            debug!("process_completions: found completed token {}", token);
            if let Some(pending) = self.pending_requests.get(&token) {
                debug!(
                    "process_completions: waking process {:?}",
                    pending.process_id
                );
                pending.waker.wake();
            } else {
                debug!(
                    "process_completions: token {} not in pending_requests",
                    token
                );
            }
            // Don't remove from pending_requests here - the woken process
            // will do that in complete_pending_read/write
            break; // Only process one per call to avoid holding lock too long
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

impl BlockDevice for Spinlock<VirtioBlockDevice> {
    fn read_sectors(&self, start_sector: u64, buf: &mut [u8]) -> Result<(), BlockError> {
        let mut device = self.lock();
        let sector_size = device.sector_size as usize;

        if buf.len() % sector_size != 0 {
            return Err(BlockError::IoError);
        }

        // Disable device interrupts during sync I/O to avoid deadlock.
        // The sync path busy-waits with the lock held, and the IRQ handler
        // also needs the lock, which would cause deadlock.
        device.device.disable_interrupts();

        let num_sectors = buf.len() / sector_size;
        for i in 0..num_sectors {
            let sector = start_sector + i as u64;
            let offset = i * sector_size;
            device
                .device
                .read_blocks(sector as usize, &mut buf[offset..offset + sector_size])
                .map_err(|_| BlockError::IoError)?;
        }

        // Re-enable interrupts for async I/O
        device.device.enable_interrupts();

        Ok(())
    }

    fn write_sectors(&self, start_sector: u64, buf: &[u8]) -> Result<(), BlockError> {
        let mut device = self.lock();
        let sector_size = device.sector_size as usize;

        if buf.len() % sector_size != 0 {
            return Err(BlockError::IoError);
        }

        // Disable device interrupts during sync I/O to avoid deadlock
        device.device.disable_interrupts();

        let num_sectors = buf.len() / sector_size;
        for i in 0..num_sectors {
            let sector = start_sector + i as u64;
            let offset = i * sector_size;
            device
                .device
                .write_blocks(sector as usize, &buf[offset..offset + sector_size])
                .map_err(|_| BlockError::IoError)?;
        }

        // Re-enable interrupts for async I/O
        device.device.enable_interrupts();

        Ok(())
    }

    fn sector_size(&self) -> u32 {
        self.lock().sector_size
    }

    fn sector_count(&self) -> u64 {
        self.lock().capacity_sectors
    }

    fn flush(&self) -> Result<(), BlockError> {
        // Note: virtio-drivers crate's flush() method is available if FLUSH feature
        // is negotiated. For now we rely on write-through behavior.
        Ok(())
    }

    fn supports_async(&self) -> bool {
        true
    }

    fn read_sectors_async(
        &self,
        start_sector: u64,
        buf: &mut [u8],
        process_id: ProcessId,
        waker: Arc<Waker>,
    ) -> Result<(), BlockError> {
        let mut device = self.lock();
        let sector_size = device.sector_size as usize;

        if buf.len() % sector_size != 0 {
            return Err(BlockError::IoError);
        }

        // Allocate DMA buffer
        let mut dma_buffer = DmaBuffer::new(buf.len());
        let mut request_header = BlockRequest::default();
        let mut response_status = BlockResponse::default();

        // Try to submit the request
        let token = match unsafe {
            device.device.read_blocks_nb(
                start_sector as usize,
                &mut request_header,
                dma_buffer.as_mut_slice(),
                &mut response_status,
            )
        } {
            Ok(token) => token,
            Err(VirtioError::QueueFull) => {
                // Queue full - add to wait queue
                device.queued_requests.push_back(QueuedBlockRequest {
                    operation: BlockRequestOperation::Read,
                    sector: start_sector,
                    process_id,
                    waker,
                    dma_buffer,
                });
                return Err(BlockError::WouldBlock);
            }
            Err(_) => return Err(BlockError::IoError),
        };

        // Check if it completed immediately
        let peek = device.device.peek_used();
        debug!("read_sectors_async: token={}, peek_used={:?}", token, peek);
        if peek == Some(token) {
            unsafe {
                device
                    .device
                    .complete_read_blocks(
                        token,
                        &request_header,
                        dma_buffer.as_mut_slice(),
                        &mut response_status,
                    )
                    .map_err(|_| BlockError::IoError)?;
            }
            // Copy from DMA buffer to user buffer
            buf.copy_from_slice(dma_buffer.as_slice());
            return Ok(());
        }

        // Request is pending - save state
        debug!(
            "read_sectors_async: request pending with token {}, process {:?}",
            token, process_id
        );
        device.pending_requests.insert(
            token,
            PendingBlockRequest {
                token,
                operation: BlockRequestOperation::Read,
                sector: start_sector,
                process_id,
                waker,
                dma_buffer,
                request_header,
                response_status,
            },
        );

        Err(BlockError::WouldBlock)
    }

    fn write_sectors_async(
        &self,
        start_sector: u64,
        buf: &[u8],
        process_id: ProcessId,
        waker: Arc<Waker>,
    ) -> Result<(), BlockError> {
        let mut device = self.lock();
        let sector_size = device.sector_size as usize;

        if buf.len() % sector_size != 0 {
            return Err(BlockError::IoError);
        }

        // Allocate DMA buffer and copy data
        let mut dma_buffer = DmaBuffer::new(buf.len());
        dma_buffer.as_mut_slice().copy_from_slice(buf);

        let mut request_header = BlockRequest::default();
        let mut response_status = BlockResponse::default();

        // Try to submit the request
        let token = match unsafe {
            device.device.write_blocks_nb(
                start_sector as usize,
                &mut request_header,
                dma_buffer.as_slice(),
                &mut response_status,
            )
        } {
            Ok(token) => token,
            Err(VirtioError::QueueFull) => {
                // Queue full - add to wait queue
                device.queued_requests.push_back(QueuedBlockRequest {
                    operation: BlockRequestOperation::Write,
                    sector: start_sector,
                    process_id,
                    waker,
                    dma_buffer,
                });
                return Err(BlockError::WouldBlock);
            }
            Err(_) => return Err(BlockError::IoError),
        };

        // Check if it completed immediately
        if device.device.peek_used() == Some(token) {
            unsafe {
                device
                    .device
                    .complete_write_blocks(
                        token,
                        &request_header,
                        dma_buffer.as_slice(),
                        &mut response_status,
                    )
                    .map_err(|_| BlockError::IoError)?;
            }
            return Ok(());
        }

        // Request is pending - save state
        device.pending_requests.insert(
            token,
            PendingBlockRequest {
                token,
                operation: BlockRequestOperation::Write,
                sector: start_sector,
                process_id,
                waker,
                dma_buffer,
                request_header,
                response_status,
            },
        );

        Err(BlockError::WouldBlock)
    }

    fn complete_pending_read(
        &self,
        process_id: ProcessId,
        buf: &mut [u8],
    ) -> Result<Option<()>, BlockError> {
        let mut device = self.lock();

        // Find pending request for this process
        let token = device
            .pending_requests
            .iter()
            .find(|(_, req)| {
                req.operation == BlockRequestOperation::Read && req.process_id == process_id
            })
            .map(|(token, _)| *token);

        let Some(token) = token else {
            // Check if it's in the queued list
            if device.queued_requests.iter().any(|req| {
                req.operation == BlockRequestOperation::Read && req.process_id == process_id
            }) {
                return Ok(None); // Still queued, not yet submitted
            }
            return Ok(None); // No pending request
        };

        // Check if completed
        if device.device.peek_used() != Some(token) {
            return Ok(None); // Still pending
        }

        // Remove and complete
        let mut pending = device.pending_requests.remove(&token).unwrap();
        unsafe {
            device
                .device
                .complete_read_blocks(
                    token,
                    &pending.request_header,
                    pending.dma_buffer.as_mut_slice(),
                    &mut pending.response_status,
                )
                .map_err(|_| BlockError::IoError)?;
        }

        // Copy from DMA buffer to user buffer
        buf.copy_from_slice(pending.dma_buffer.as_slice());
        Ok(Some(()))
    }

    fn complete_pending_write(&self, process_id: ProcessId) -> Result<Option<()>, BlockError> {
        let mut device = self.lock();

        // Find pending request for this process
        let token = device
            .pending_requests
            .iter()
            .find(|(_, req)| {
                req.operation == BlockRequestOperation::Write && req.process_id == process_id
            })
            .map(|(token, _)| *token);

        let Some(token) = token else {
            // Check if it's in the queued list
            if device.queued_requests.iter().any(|req| {
                req.operation == BlockRequestOperation::Write && req.process_id == process_id
            }) {
                return Ok(None); // Still queued, not yet submitted
            }
            return Ok(None); // No pending request
        };

        // Check if completed
        if device.device.peek_used() != Some(token) {
            return Ok(None); // Still pending
        }

        // Remove and complete
        let mut pending = device.pending_requests.remove(&token).unwrap();
        unsafe {
            device
                .device
                .complete_write_blocks(
                    token,
                    &pending.request_header,
                    pending.dma_buffer.as_slice(),
                    &mut pending.response_status,
                )
                .map_err(|_| BlockError::IoError)?;
        }

        Ok(Some(()))
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
