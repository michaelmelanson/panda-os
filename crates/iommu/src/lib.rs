//! Hardware-agnostic IOMMU abstraction: `Iommu` trait, domain management, and
//! backends for Intel VT-d (implemented) and AMD-Vi (stub).
//!
//! This crate has no dependency on any kernel-private types. Kernel-specific
//! operations (frame allocation, MMIO access) are injected via the HAL traits
//! in [`hal`].

#![no_std]
#[cfg(feature = "alloc")]
extern crate alloc;

pub mod amd_vi;
pub mod domain;
pub mod hal;
pub mod iommu;
pub mod iova;
pub mod types;
pub mod vtd;

pub use domain::IommuDomain;
pub use hal::{FrameAllocator, MmioAccess};
pub use iommu::Iommu;
pub use iova::IovaAllocator;
pub use types::{DomainId, IommuError, IommuFlags, PciAddress};
