//! AMD-Vi backend stub.
//!
//! Establishes the module structure so AMD-Vi can be filled in later without
//! changing the [`Iommu`] trait or shared code. All operations currently
//! return [`IommuError::Unsupported`] or no-op.
//!
//! AMD-Vi differs from VT-d in several ways that will matter once implemented:
//! - Device lookup uses a flat device table indexed by device ID, not a
//!   root/context table hierarchy.
//! - The page table format is four-level but with different flag semantics.
//! - Invalidation goes through an IOMMU command queue rather than
//!   register-based invalidation commands.

pub mod ivrs;
pub mod page_table;

use crate::iommu::Iommu;
use crate::types::{DomainId, IommuError, IommuFlags, PciAddress};

/// Placeholder AMD-Vi implementation. Establishes the module structure only;
/// every operation is unimplemented.
pub struct AmdViIommu;

impl Iommu for AmdViIommu {
    fn create_domain(&self) -> Result<DomainId, IommuError> {
        Err(IommuError::Unsupported)
    }

    fn destroy_domain(&self, _domain: DomainId) {}

    fn map(
        &self,
        _domain: DomainId,
        _iova: u64,
        _phys: u64,
        _size: usize,
        _flags: IommuFlags,
    ) -> Result<(), IommuError> {
        Err(IommuError::Unsupported)
    }

    fn unmap(&self, _domain: DomainId, _iova: u64, _size: usize) {}

    fn assign_device(&self, _domain: DomainId, _device: PciAddress) -> Result<(), IommuError> {
        Err(IommuError::Unsupported)
    }

    fn flush(&self, _domain: DomainId) {}
}
