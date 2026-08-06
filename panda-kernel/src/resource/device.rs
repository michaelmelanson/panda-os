//! The `Device`-typed resource: an owned device handle from `OP_DEVICE_CLAIM`.
//!
//! This is the root capability for the Phase 6 hardware-access syscalls
//! (`OP_DEVICE_MAP_MMIO`/`OP_DMA_ALLOC`/`OP_DMA_FREE`/
//! `OP_DEVICE_SUBSCRIBE_IRQ` — see `panda-kernel/src/syscall/device.rs`).
//! It tracks every MMIO mapping, DMA allocation, and IRQ subscription made
//! against the claimed device, and tears all of it down in `Drop` — which
//! runs whether the handle is closed explicitly (`OP_FILE_CLOSE`-style
//! `handles_mut().remove`) or implicitly via process exit (dropping the
//! process's `HandleTable`), the same one cleanup path every other
//! `Drop`-based resource in this module uses (see `ChannelEndpoint`,
//! `DisplayDevice`).
//!
//! IRQ subscriptions: see [`subscribe_irq`] for the real (and limited)
//! capabilities of the backing mechanism — a small fixed pool of MSI-X
//! vectors, not general dynamic MSI-X.

use alloc::vec::Vec;

use iommu::DomainId;
use panda_abi::device::{EVENT_DEVICE_IRQ, PciAddress};
use spinning_top::Spinlock;
use x86_64::PhysAddr;
use x86_64::structures::idt::InterruptStackFrame;

use crate::device::DeviceId;
use crate::interrupts::{self, IrqHandlerFunc};
use crate::iommu::manager;
use crate::memory::dma::DmaBuffer;
use crate::memory::{Mapping, MemoryMappingOptions, map_external};
use crate::pci::device::PciDevice;
use crate::process::Process;

use super::{MailboxRef, Resource};

/// Errors returned by [`DeviceResource`]'s hardware-access methods.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceResourceError {
    /// The device has no PCI address on record (not a PCI device, or the
    /// `PciDevice` config-space accessor could not be re-located).
    NoPciDevice,
    /// `bar_index` is out of range (must be 0-5) or names an I/O BAR.
    InvalidBar,
    /// The caller's virtual address space has no room left for this
    /// mapping.
    OutOfVirtualSpace,
    /// No free IRQ slot in the fixed MSI-X pool, or the device has no
    /// MSI-X capability (see [`subscribe_irq`]'s doc comment).
    IrqUnavailable,
    /// No DMA allocation matches the given virtual address.
    NoSuchAllocation,
}

/// Converts a `panda_abi::device::PciAddress` to the `iommu` crate's
/// `PciAddress` (the same fields, different crate — kept as two distinct
/// types because `panda_abi` cannot depend on `iommu`).
fn to_iommu_address(addr: PciAddress) -> iommu::PciAddress {
    iommu::PciAddress::new(addr.segment, addr.bus, addr.device, addr.function)
}

/// One MMIO BAR mapped into the owning process's address space, tracked so
/// it can be unmapped by `DeviceResource::drop`.
struct MmioMapping {
    /// Sole strong reference to the mapping's RAII guard — dropping it
    /// unmaps the page-table entries (see `memory::mapping::MappingInner`'s
    /// `Drop` impl). Deliberately *not* also stored in the process's
    /// `mappings` list the way `DisplayDevice::map_into_process` does,
    /// because that list is only ever torn down at process exit — storing
    /// a second strong reference there would keep the mapping alive past
    /// this handle being closed, which is the whole point of tracking it
    /// here instead.
    _mapping: Mapping,
}

/// One DMA buffer allocated for the device, tracked so it can be freed by
/// `OP_DMA_FREE` or `DeviceResource::drop`.
struct DmaAllocation {
    virt_addr: u64,
    iova: u64,
    size: usize,
    /// Owns the physically-contiguous backing memory; freed on drop.
    _buffer: DmaBuffer,
    /// Unmaps the userspace view onto `_buffer` on drop (see `MmioMapping`
    /// for why this isn't also pinned in the process's `mappings` list).
    _mapping: Mapping,
}

/// Number of dynamically-assignable MSI-X interrupt vectors available to
/// `OP_DEVICE_SUBSCRIBE_IRQ`. See [`subscribe_irq`]'s doc comment for why
/// this is a small fixed pool rather than a general allocator.
const IRQ_SLOT_COUNT: usize = 8;

/// First interrupt vector in the fixed pool `OP_DEVICE_SUBSCRIBE_IRQ` draws
/// from. Chosen to sit above the fixed vectors existing kernel-resident
/// drivers already claim (e.g. `VIRTIO_BLOCK_MSIX_VECTOR` = 0x30) with
/// headroom to spare.
const IRQ_VECTOR_BASE: u8 = 0x50;

/// Per-slot mailbox to post `EVENT_DEVICE_IRQ` to, `None` when the slot is
/// free. Indexed by `vector - IRQ_VECTOR_BASE`.
static IRQ_SLOTS: [Spinlock<Option<MailboxRef>>; IRQ_SLOT_COUNT] =
    [const { Spinlock::new(None) }; IRQ_SLOT_COUNT];

/// Common body for every generated `irq_slot_N` handler below: post
/// `EVENT_DEVICE_IRQ` to whichever mailbox owns this slot, then EOI.
fn dynamic_irq_common(slot: usize) {
    if let Some(mailbox_ref) = IRQ_SLOTS[slot].lock().as_ref() {
        mailbox_ref.post_event(EVENT_DEVICE_IRQ);
    }
    crate::apic::eoi();
}

/// `extern "x86-interrupt"` handlers don't carry their own vector number
/// (`InterruptStackFrame` doesn't include it), so one distinct function per
/// slot is generated here and installed with `interrupts::set_interrupt_handler`.
macro_rules! irq_slot_handler {
    ($name:ident, $slot:expr) => {
        extern "x86-interrupt" fn $name(_frame: InterruptStackFrame) {
            dynamic_irq_common($slot);
        }
    };
}

irq_slot_handler!(irq_slot_0, 0);
irq_slot_handler!(irq_slot_1, 1);
irq_slot_handler!(irq_slot_2, 2);
irq_slot_handler!(irq_slot_3, 3);
irq_slot_handler!(irq_slot_4, 4);
irq_slot_handler!(irq_slot_5, 5);
irq_slot_handler!(irq_slot_6, 6);
irq_slot_handler!(irq_slot_7, 7);

static IRQ_HANDLERS: [IrqHandlerFunc; IRQ_SLOT_COUNT] = [
    irq_slot_0, irq_slot_1, irq_slot_2, irq_slot_3, irq_slot_4, irq_slot_5, irq_slot_6,
    irq_slot_7,
];

/// Claim a free slot in the fixed IRQ pool and attach `mailbox_ref` to it.
/// Returns the slot index (also its vector offset from `IRQ_VECTOR_BASE`).
fn claim_irq_slot(mailbox_ref: MailboxRef) -> Option<usize> {
    for (slot, entry) in IRQ_SLOTS.iter().enumerate() {
        let mut guard = entry.lock();
        if guard.is_none() {
            *guard = Some(mailbox_ref);
            return Some(slot);
        }
    }
    None
}

/// This device's active IRQ subscription, if any, tracked for teardown.
struct IrqSubscription {
    slot: usize,
    vector: u8,
}

/// An owned device handle: the resource installed in the caller's handle
/// table by `OP_DEVICE_CLAIM`. See the module doc comment.
pub struct DeviceResource {
    device_id: DeviceId,
    pci_address: Option<PciAddress>,
    /// The IOMMU domain this device was assigned to by
    /// `iommu::manager::create_device_domain`, if hardware IOMMU support
    /// was available at claim time (see that function's doc comment).
    domain: Option<DomainId>,
    mmio: Spinlock<Vec<MmioMapping>>,
    dma: Spinlock<Vec<DmaAllocation>>,
    irq: Spinlock<Option<IrqSubscription>>,
}

impl DeviceResource {
    pub fn new(device_id: DeviceId, pci_address: Option<PciAddress>, domain: Option<DomainId>) -> Self {
        Self {
            device_id,
            pci_address,
            domain,
            mmio: Spinlock::new(Vec::new()),
            dma: Spinlock::new(Vec::new()),
            irq: Spinlock::new(None),
        }
    }

    pub fn device_id(&self) -> DeviceId {
        self.device_id
    }

    fn pci_device(&self) -> Result<PciDevice, DeviceResourceError> {
        let address = self.pci_address.ok_or(DeviceResourceError::NoPciDevice)?;
        crate::device::pci::find_by_address(address).ok_or(DeviceResourceError::NoPciDevice)
    }

    /// Read `width` (1, 2, or 4) bytes of raw PCI config space at `offset`.
    /// Backs `OP_DEVICE_PCI_CONFIG_READ` — see that op's doc comment in
    /// `panda_abi::device` for why userspace drivers need this (walking a
    /// virtio-pci capability list without an in-kernel virtio transport).
    pub fn pci_config_read(&self, offset: u8, width: u32) -> Result<u32, DeviceResourceError> {
        let pci_device = self.pci_device()?;
        Ok(match width {
            1 => pci_device.read::<u8>(offset) as u32,
            2 => pci_device.read::<u16>(offset) as u32,
            4 => pci_device.read::<u32>(offset),
            _ => return Err(DeviceResourceError::InvalidBar),
        })
    }

    /// Read BAR `bar_index`, size it (the standard "write all-1s, read back
    /// the size mask, restore" PCI technique), and map it into `process`'s
    /// address space as a fresh, non-overlapping mapping tracked by this
    /// resource. Returns the mapped virtual address.
    ///
    /// Caching: mapped with the same `MemoryMappingOptions` used for every
    /// other user-visible physical mapping in this kernel (see
    /// `DisplayDevice::map_into_process`) — this kernel has no PAT/PCD
    /// (non-cacheable) mapping support yet (grepped for it: none exists),
    /// so unlike the plan's description this does not force the mapping
    /// uncacheable. TODO(plans/device-driver-model.md Phase 6): add a
    /// non-cacheable `MemoryMappingOptions` variant (PAT/PCD/PWT) and use
    /// it here once it exists; until then, a driver reading/writing device
    /// registers through this mapping must not rely on write ordering or
    /// freshness the CPU cache could otherwise defer.
    pub fn map_mmio(&self, process: &mut Process, bar_index: u32) -> Result<(u64, usize), DeviceResourceError> {
        if bar_index > 5 {
            return Err(DeviceResourceError::InvalidBar);
        }
        let bar_index = bar_index as u8;
        let pci_device = self.pci_device()?;

        let bar_offset = 0x10 + bar_index * 4;
        let original: u32 = pci_device.bar(bar_index);
        if original & 1 != 0 {
            // I/O BAR, not memory-mapped.
            return Err(DeviceResourceError::InvalidBar);
        }
        // Size the BAR: write all 1s, read back the mask of settable bits,
        // restore the original value.
        let size_mask = unsafe {
            pci_device.write(bar_offset, 0xFFFF_FFFFu32);
            let mask: u32 = pci_device.bar(bar_index);
            pci_device.write(bar_offset, original);
            mask
        };
        let size = (!(size_mask & !0xF)).wrapping_add(1) as usize;
        if size == 0 {
            return Err(DeviceResourceError::InvalidBar);
        }

        let phys = PhysAddr::new(pci_device.bar_address(bar_index));
        let num_pages = size.div_ceil(4096);
        let vaddr = process
            .alloc_buffer_vaddr(num_pages)
            .ok_or(DeviceResourceError::OutOfVirtualSpace)?;

        let mapping = map_external(
            phys,
            vaddr,
            num_pages * 4096,
            MemoryMappingOptions {
                user: true,
                executable: false,
                writable: true,
            },
        );

        let virt_addr = vaddr.as_u64();
        self.mmio.lock().push(MmioMapping { _mapping: mapping });
        Ok((virt_addr, size))
    }

    /// Allocate a physically-contiguous DMA buffer, map it for the device
    /// (into its isolated IOMMU domain if it has one, the passthrough
    /// domain otherwise — see `iommu::manager::dma_map_for_device`), and
    /// map it into `process`'s address space. Returns `(virt_addr, iova)`.
    pub fn dma_alloc(&self, process: &mut Process, size: usize) -> Result<(u64, u64), DeviceResourceError> {
        let buffer = DmaBuffer::new(size);
        let phys = buffer.physical_address();

        let iova = if let Some(address) = self.pci_address {
            manager::dma_map_for_device(
                to_iommu_address(address),
                phys.as_u64(),
                size,
                iommu::IommuFlags::READ_WRITE,
            )
            .unwrap_or(phys.as_u64())
        } else {
            phys.as_u64()
        };

        let num_pages = size.div_ceil(4096).max(1);
        let vaddr = process
            .alloc_buffer_vaddr(num_pages)
            .ok_or(DeviceResourceError::OutOfVirtualSpace)?;
        let mapping = map_external(
            phys,
            vaddr,
            num_pages * 4096,
            MemoryMappingOptions {
                user: true,
                executable: false,
                writable: true,
            },
        );

        let virt_addr = vaddr.as_u64();
        self.dma.lock().push(DmaAllocation {
            virt_addr,
            iova,
            size,
            _buffer: buffer,
            _mapping: mapping,
        });
        Ok((virt_addr, iova))
    }

    /// Free a DMA allocation previously returned by [`dma_alloc`], by
    /// virtual address.
    pub fn dma_free(&self, virt_addr: u64) -> Result<(), DeviceResourceError> {
        let mut dma = self.dma.lock();
        let index = dma
            .iter()
            .position(|allocation| allocation.virt_addr == virt_addr)
            .ok_or(DeviceResourceError::NoSuchAllocation)?;
        let allocation = dma.remove(index);
        drop(dma);

        if let Some(address) = self.pci_address {
            manager::dma_unmap_for_device(to_iommu_address(address), allocation.iova, allocation.size);
        }
        // `allocation`'s `_mapping`/`_buffer` unmap and free on drop here.
        Ok(())
    }

    /// Subscribe to this device's IRQ, posting `EVENT_DEVICE_IRQ` to
    /// `mailbox_ref` on every interrupt.
    ///
    /// Real capabilities: this kernel's interrupt infrastructure
    /// (`interrupts::set_interrupt_handler`) only supports installing a
    /// fixed, statically-known `extern "x86-interrupt"` function per
    /// vector — there is no general mechanism for a dynamically-chosen
    /// vector to carry per-call context (see `crate::interrupts`). Rather
    /// than fabricate a general dynamic-MSI-X facility that doesn't exist,
    /// this draws from a small fixed pool of [`IRQ_SLOT_COUNT`]
    /// pre-generated handler functions (`irq_slot_0`..`irq_slot_7`), each
    /// hardcoded to a fixed vector and slot index, and configures MSI-X
    /// table entry 0 (only) to target the claimed slot's vector. This
    /// means: only MSI-X-capable devices are supported (no legacy/IOAPIC
    /// fallback), only one interrupt source per device (MSI-X table entry
    /// 0), and at most [`IRQ_SLOT_COUNT`] devices system-wide can have an
    /// active subscription at once.
    pub fn subscribe_irq(&self, mailbox_ref: MailboxRef) -> Result<(), DeviceResourceError> {
        let pci_device = self.pci_device()?;
        let cap = pci_device
            .enable_msix()
            .ok_or(DeviceResourceError::IrqUnavailable)?;

        let slot = claim_irq_slot(mailbox_ref).ok_or(DeviceResourceError::IrqUnavailable)?;
        let vector = IRQ_VECTOR_BASE + slot as u8;

        cap.configure_entry(0, vector, 0);
        interrupts::set_interrupt_handler(vector, Some(IRQ_HANDLERS[slot]));

        *self.irq.lock() = Some(IrqSubscription { slot, vector });
        Ok(())
    }

    /// Tear down MMIO mappings, DMA allocations, the IRQ subscription, and
    /// the IOMMU domain — everything except releasing the device claim
    /// itself, which the caller ([`Drop`]) does separately since it must
    /// stay idempotent against `DeviceRegistry::release_all_owned_by`
    /// running first at process exit (see that function's doc comment).
    fn teardown(&self) {
        self.mmio.lock().clear();

        let mut dma = self.dma.lock();
        if let Some(address) = self.pci_address {
            for allocation in dma.iter() {
                manager::dma_unmap_for_device(to_iommu_address(address), allocation.iova, allocation.size);
            }
        }
        dma.clear();
        drop(dma);

        if let Some(subscription) = self.irq.lock().take() {
            interrupts::set_interrupt_handler(subscription.vector, None);
            *IRQ_SLOTS[subscription.slot].lock() = None;
        }

        if let Some(domain) = self.domain {
            manager::destroy_device_domain(domain);
        }
    }
}

impl Drop for DeviceResource {
    fn drop(&mut self) {
        self.teardown();
        crate::device::DEVICE_REGISTRY.lock().release(self.device_id);
    }
}

impl Resource for DeviceResource {
    fn handle_type(&self) -> panda_abi::HandleType {
        panda_abi::HandleType::Device
    }

    fn as_device(&self) -> Option<&DeviceResource> {
        Some(self)
    }
}

// =============================================================================
// SubscriptionResource: OP_DEVICE_SUBSCRIBE's handle
// =============================================================================

/// Opaque handle installed in the caller's table when `OP_DEVICE_SUBSCRIBE`
/// succeeds. Owns the [`crate::device::subscription::EventQueue`] that
/// `OP_DEVICE_POLL_EVENT` drains — the mailbox bit posted alongside
/// `EVENT_DEVICE_ADDED`/`EVENT_DEVICE_REMOVED` only wakes the caller; the
/// actual `DeviceEvent` payload (bus/identity/claim token) lives here.
pub struct SubscriptionResource {
    queue: crate::device::subscription::EventQueue,
}

impl SubscriptionResource {
    pub fn new(queue: crate::device::subscription::EventQueue) -> Self {
        Self { queue }
    }

    /// Pop the oldest pending `DeviceEvent`, if any.
    pub fn poll_event(&self) -> Option<panda_abi::device::DeviceEvent> {
        self.queue.lock().pop_front()
    }
}

impl Resource for SubscriptionResource {
    fn handle_type(&self) -> panda_abi::HandleType {
        panda_abi::HandleType::DeviceSubscription
    }

    fn as_device_subscription(&self) -> Option<&SubscriptionResource> {
        Some(self)
    }
}
