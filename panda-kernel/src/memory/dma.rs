//! DMA buffer abstraction for device I/O.
//!
//! Provides a safe wrapper around physical memory suitable for DMA operations.
//! Uses kernel identity mapping (virt == phys) for CPU access to DMA buffers.

use core::alloc::Layout;
use core::slice;

use x86_64::{PhysAddr, VirtAddr};

use super::{Frame, allocate_physical};

/// A buffer suitable for DMA operations.
///
/// Guarantees:
/// - Memory is physically contiguous
/// - Memory remains valid and at fixed physical address until dropped
/// - Properly aligned for DMA (page-aligned)
///
/// Uses kernel identity mapping where virtual address equals physical address.
/// This is correct for kernel-allocated DMA buffers. If IOMMU support is added
/// in the future, the IOMMU would handle device-side address translation
/// separately from CPU-side access.
pub struct DmaBuffer {
    frame: Frame,
    len: usize,
}

impl DmaBuffer {
    /// Allocate a new DMA buffer of at least `size` bytes.
    ///
    /// The actual allocation is rounded up to page alignment for DMA safety.
    pub fn new(size: usize) -> Self {
        // Round up to page alignment for DMA safety
        let aligned_size = size.max(4096).next_power_of_two().max(4096);
        let layout = Layout::from_size_align(aligned_size, 4096).unwrap();
        let frame = allocate_physical(layout);
        Self { frame, len: size }
    }

    /// Get the physical address for DMA descriptor setup.
    pub fn physical_address(&self) -> PhysAddr {
        self.frame.start_address()
    }

    /// Get the virtual address for CPU access.
    ///
    /// Returns the identity-mapped virtual address (virt == phys).
    pub fn virtual_address(&self) -> VirtAddr {
        VirtAddr::new(self.frame.start_address().as_u64())
    }

    /// Get a mutable slice of the buffer contents.
    pub fn as_mut_slice(&mut self) -> &mut [u8] {
        let ptr = self.virtual_address().as_mut_ptr();
        unsafe { slice::from_raw_parts_mut(ptr, self.len) }
    }

    /// Get a slice of the buffer contents.
    pub fn as_slice(&self) -> &[u8] {
        let ptr = self.virtual_address().as_ptr();
        unsafe { slice::from_raw_parts(ptr, self.len) }
    }

    /// Length of the buffer in bytes.
    pub fn len(&self) -> usize {
        self.len
    }

    /// Returns true if the buffer has zero length.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
}

// Frame's Drop handles deallocation automatically
