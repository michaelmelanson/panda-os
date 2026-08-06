//! RAII wrapper around a created domain.

use crate::iommu::Iommu;
use crate::types::DomainId;

/// Owns a [`DomainId`] created on some [`Iommu`] implementation and destroys it on drop.
pub struct IommuDomain<'a> {
    domain: DomainId,
    iommu: &'a dyn Iommu,
}

impl<'a> IommuDomain<'a> {
    /// Wrap an already-created domain.
    pub fn new(iommu: &'a dyn Iommu, domain: DomainId) -> Self {
        Self { domain, iommu }
    }

    pub fn id(&self) -> DomainId {
        self.domain
    }
}

impl Drop for IommuDomain<'_> {
    fn drop(&mut self) {
        self.iommu.destroy_domain(self.domain);
    }
}
