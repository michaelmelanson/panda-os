//! Device-to-domain assignment and the DMA mapping helpers used by device HALs.

use alloc::collections::BTreeMap;

use iommu::{DomainId, IommuError, IommuFlags, PciAddress};
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

// =============================================================================
// Per-device domains (Phase 6: userspace device claim/MMIO/DMA syscalls)
// =============================================================================
//
// `Iommu::create_domain`/`destroy_domain`/`assign_device` (see
// `crates/iommu/src/iommu.rs`) already support allocating additional,
// isolated domains beyond the single global passthrough domain — this is
// real per-device isolation, not a passthrough-domain reuse shim.

/// Create a fresh, isolated IOMMU domain for `device` and assign the device
/// to it, so its DMA is confined to mappings explicitly made in that domain
/// (no access to any other device's or the passthrough domain's memory).
///
/// Called by `OP_DEVICE_CLAIM` once a device is claimed by a userspace
/// driver. Returns `IommuError::Unsupported` if no IOMMU hardware was found
/// at boot (`iommu::get()` is `None`) — claim still succeeds in that case
/// (see the call site), it just proceeds without hardware-enforced DMA
/// isolation, same as the kernel-resident drivers already do today.
pub fn create_device_domain(device: PciAddress) -> Result<DomainId, IommuError> {
    let iommu = crate::iommu::get().ok_or(IommuError::Unsupported)?;
    let domain = iommu.create_domain()?;
    iommu.assign_device(domain, device)?;
    assign(device, domain);
    Ok(domain)
}

/// Destroy a domain created by [`create_device_domain`], releasing its page
/// tables. Does not touch the `device -> domain` assignment record (the
/// caller is tearing down the device's claim entirely).
pub fn destroy_device_domain(domain: DomainId) {
    if let Some(iommu) = crate::iommu::get() {
        iommu.destroy_domain(domain);
    }
}

/// Map a physical range for `device`'s DMA use into whichever domain it is
/// currently assigned to (its isolated per-device domain from
/// [`create_device_domain`] if claimed, the global passthrough domain
/// otherwise), identity-mapping IOVA == phys. Returns the IOVA to hand the
/// device for its descriptor rings.
///
/// A no-op returning `phys` unchanged if `device` has no domain assignment
/// recorded, or is assigned to the passthrough domain (already
/// identity-mapped at init time — see [`dma_map`] above) — mirrors the
/// existing passthrough behaviour for kernel-resident drivers.
pub fn dma_map_for_device(
    device: PciAddress,
    phys: u64,
    size: usize,
    flags: IommuFlags,
) -> Result<u64, IommuError> {
    let Some(domain) = domain_for(device) else {
        return Ok(phys);
    };
    if Some(domain) == crate::iommu::passthrough_domain() {
        return Ok(phys);
    }
    let iommu = crate::iommu::get().ok_or(IommuError::Unsupported)?;
    iommu.map(domain, phys, phys, size, flags)?;
    iommu.flush(domain);
    Ok(phys)
}

/// Unmap an IOVA previously returned by [`dma_map_for_device`] from
/// `device`'s domain. No-op under the same conditions [`dma_map_for_device`]
/// treats as a no-op (no domain assignment, or passthrough domain).
pub fn dma_unmap_for_device(device: PciAddress, iova: u64, size: usize) {
    let Some(domain) = domain_for(device) else {
        return;
    };
    if Some(domain) == crate::iommu::passthrough_domain() {
        return;
    }
    if let Some(iommu) = crate::iommu::get() {
        iommu.unmap(domain, iova, size);
        iommu.flush(domain);
    }
}
