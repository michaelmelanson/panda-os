//! Hardware-agnostic IOMMU trait.

use crate::types::{DomainId, IommuError, IommuFlags, PciAddress};

/// A hardware-agnostic interface to an IOMMU (Intel VT-d, AMD-Vi, or future hardware).
///
/// Implementations own all hardware-specific state (page table formats, register
/// layouts, invalidation mechanisms) behind this trait so kernel code that manages
/// DMA never needs to know which IOMMU hardware is present.
pub trait Iommu: Send + Sync {
    /// Allocate a new isolated translation domain.
    fn create_domain(&self) -> Result<DomainId, IommuError>;

    /// Release a domain and all its page table mappings.
    fn destroy_domain(&self, domain: DomainId);

    /// Map a physical address range into a domain at the given I/O virtual address.
    fn map(
        &self,
        domain: DomainId,
        iova: u64,
        phys: u64,
        size: usize,
        flags: IommuFlags,
    ) -> Result<(), IommuError>;

    /// Remove a mapping from a domain.
    fn unmap(&self, domain: DomainId, iova: u64, size: usize);

    /// Assign a PCI device to a domain. Device DMA is now constrained to
    /// mappings within that domain.
    fn assign_device(&self, domain: DomainId, device: PciAddress) -> Result<(), IommuError>;

    /// Flush the IOTLB for a domain after mapping changes.
    fn flush(&self, domain: DomainId);
}
