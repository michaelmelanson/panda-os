//! Shared types for the IOMMU abstraction.

/// Identifier for an isolated translation domain.
pub type DomainId = u32;

/// A PCI device address (segment, bus, device, function).
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct PciAddress {
    pub segment: u16,
    pub bus: u8,
    pub device: u8,
    pub function: u8,
}

impl PciAddress {
    pub const fn new(segment: u16, bus: u8, device: u8, function: u8) -> Self {
        Self {
            segment,
            bus,
            device,
            function,
        }
    }

    /// The `device << 3 | function` byte used to index a VT-d context table.
    pub const fn devfn(&self) -> u8 {
        (self.device << 3) | (self.function & 0x7)
    }
}

/// Permission flags for an IOMMU mapping.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct IommuFlags(u8);

impl IommuFlags {
    pub const READ: Self = Self(0x01);
    pub const WRITE: Self = Self(0x02);
    pub const READ_WRITE: Self = Self(0x03);

    pub const fn bits(self) -> u8 {
        self.0
    }

    pub const fn contains(self, other: Self) -> bool {
        (self.0 & other.0) == other.0
    }
}

impl core::ops::BitOr for IommuFlags {
    type Output = Self;
    fn bitor(self, rhs: Self) -> Self {
        Self(self.0 | rhs.0)
    }
}

/// Errors returned by IOMMU operations.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum IommuError {
    /// No hardware IOMMU unit covers the requested device.
    DeviceNotFound,
    /// The requested domain does not exist.
    DomainNotFound,
    /// Out of memory allocating page table frames or domain state.
    OutOfMemory,
    /// The requested IOVA range is already mapped, misaligned, or otherwise invalid.
    InvalidMapping,
    /// The operation is not supported by this backend.
    Unsupported,
    /// The underlying hardware did not respond as expected (e.g. a command timed out).
    HardwareError,
}
