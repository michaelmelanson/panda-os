//! MMIO (Memory-Mapped I/O) access with RAII wrappers.
//!
//! This module provides type-safe, volatile access to device memory through RAII
//! wrappers. All device register access should go through these types to ensure
//! proper memory ordering and eventual migration to the dedicated MMIO region.

use core::ptr::{read_volatile, write_volatile};

use x86_64::{PhysAddr, VirtAddr};

use super::{MemoryMappingOptions, map};

/// RAII wrapper for accessing a memory-mapped I/O region.
///
/// Provides volatile read/write access to device registers. The mapping is
/// created when the wrapper is constructed and remains valid for the lifetime
/// of the wrapper.
///
/// # Example
///
/// ```ignore
/// let mmio = MmioMapping::new(pci_bar_phys_addr, 4096);
/// let status: u32 = mmio.read(0x10);
/// mmio.write(0x14, 0x1234u32);
/// ```
pub struct MmioMapping {
    virt_addr: VirtAddr,
    size: usize,
}

impl MmioMapping {
    /// Create a new MMIO mapping for a device region.
    ///
    /// # Arguments
    ///
    /// * `phys_addr` - Physical address of the MMIO region
    /// * `size` - Size of the region in bytes
    ///
    /// # Panics
    ///
    /// Panics if the physical address is not page-aligned (for now).
    pub fn new(phys_addr: PhysAddr, size: usize) -> Self {
        // For now, use identity mapping. Later this will allocate from MMIO region.
        let virt_addr = VirtAddr::new(phys_addr.as_u64());

        // Align to page boundaries
        let aligned_phys = phys_addr.align_down(4096u64);
        let aligned_virt = virt_addr.align_down(4096u64);
        let offset = phys_addr.as_u64() - aligned_phys.as_u64();
        let aligned_size = ((size as u64 + offset + 4095) & !4095) as usize;

        // Map the region with appropriate flags for device memory
        map(
            aligned_phys,
            aligned_virt,
            aligned_size,
            MemoryMappingOptions {
                writable: true,
                executable: false,
                user: false,
            },
        );

        Self { virt_addr, size }
    }

    /// Get the virtual address of the mapping.
    pub fn virt_addr(&self) -> VirtAddr {
        self.virt_addr
    }

    /// Get the size of the mapping in bytes.
    pub fn size(&self) -> usize {
        self.size
    }

    /// Read a value from the MMIO region at the given byte offset.
    ///
    /// # Panics
    ///
    /// Panics if the offset plus the size of T exceeds the mapping size.
    pub fn read<T: Copy>(&self, offset: usize) -> T {
        assert!(
            offset + core::mem::size_of::<T>() <= self.size,
            "MMIO read out of bounds"
        );
        let ptr = (self.virt_addr.as_u64() + offset as u64) as *const T;
        unsafe { read_volatile(ptr) }
    }

    /// Write a value to the MMIO region at the given byte offset.
    ///
    /// # Panics
    ///
    /// Panics if the offset plus the size of T exceeds the mapping size.
    pub fn write<T: Copy>(&self, offset: usize, value: T) {
        assert!(
            offset + core::mem::size_of::<T>() <= self.size,
            "MMIO write out of bounds"
        );
        let ptr = (self.virt_addr.as_u64() + offset as u64) as *mut T;
        unsafe { write_volatile(ptr, value) }
    }

    /// Read a value from the MMIO region without bounds checking.
    ///
    /// # Safety
    ///
    /// The caller must ensure the offset is within bounds.
    pub unsafe fn read_unchecked<T: Copy>(&self, offset: usize) -> T {
        let ptr = (self.virt_addr.as_u64() + offset as u64) as *const T;
        unsafe { read_volatile(ptr) }
    }

    /// Write a value to the MMIO region without bounds checking.
    ///
    /// # Safety
    ///
    /// The caller must ensure the offset is within bounds.
    pub unsafe fn write_unchecked<T: Copy>(&self, offset: usize, value: T) {
        let ptr = (self.virt_addr.as_u64() + offset as u64) as *mut T;
        unsafe { write_volatile(ptr, value) }
    }

    /// Get a raw pointer to an offset within the mapping.
    ///
    /// # Safety
    ///
    /// The caller must ensure proper volatile access semantics.
    pub unsafe fn ptr<T>(&self, offset: usize) -> *const T {
        (self.virt_addr.as_u64() + offset as u64) as *const T
    }

    /// Get a raw mutable pointer to an offset within the mapping.
    ///
    /// # Safety
    ///
    /// The caller must ensure proper volatile access semantics.
    pub unsafe fn ptr_mut<T>(&self, offset: usize) -> *mut T {
        (self.virt_addr.as_u64() + offset as u64) as *mut T
    }
}

// Note: We intentionally don't implement Drop to unmap the region for now.
// In the current identity-mapped design, the mapping persists. When we migrate
// to the dedicated MMIO region, we'll add proper cleanup.
