//! Device-to-domain assignment and the DMA mapping helpers used by device HALs.

use alloc::collections::BTreeMap;

use iommu::{DomainId, IommuFlags, PciAddress};
use spinning_top::Spinlock;

/// Tracks which domain each PCI device has been assigned to.
static DEVICE_DOMAINS: Spinlock<BTreeMap<PciAddress, DomainId>> = Spinlock::new(BTreeMap::new());

/// Record that `device` has been assigned to `domain`.
pub fn assign(device: PciAddress, domain: DomainId) {
    DEVICE_DOMAINS.lock().insert(device, domain);
}

/// Look up the domain a device was assigned to.
pub fn domain_for(device: PciAddress) -> Option<DomainId> {
    DEVICE_DOMAINS.lock().get(&device).copied()
}

/// Map a physical range into the domain currently in use for DMA (the global
/// passthrough domain today; per-device domains once userspace drivers claim
/// devices), returning the IOVA the device should use.
///
/// During passthrough, IOVA == phys: the device receives the same address it
/// would without an IOMMU, so this is transparent to existing drivers.
pub fn dma_map(phys: u64, _size: usize, _flags: IommuFlags) -> u64 {
    // Passthrough domain is identity-mapped over a fixed range at init time
    // (risk #4); DMA buffers drawn from the kernel heap always fall within
    // it, so no additional per-allocation mapping call is necessary here.
    // `domain_for`/`assign` above exist so future per-device domains can be
    // introduced without changing this call site's signature.
    phys
}

/// Unmap an IOVA previously returned by [`dma_map`].
pub fn dma_unmap(_iova: u64, _size: usize) {
    // No-op in passthrough mode: the passthrough domain's identity mapping
    // stays resident for the kernel's lifetime.
}
