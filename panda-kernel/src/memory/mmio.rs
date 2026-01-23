//! MMIO (Memory-Mapped I/O) access with RAII wrappers.
//!
//! This module provides type-safe, volatile access to device memory through RAII
//! wrappers. All device register access should go through these types to ensure
//! proper memory ordering.
//!
//! MMIO regions are mapped in a dedicated virtual address region starting at
//! `MMIO_REGION_BASE` (0xffff_9000_0000_0000), separate from the physical memory
//! window. Virtual address allocation uses a simple bump allocator.

use core::ptr::{read_volatile, write_volatile};
use core::sync::atomic::{AtomicU64, Ordering};

use log::debug;
use x86_64::{PhysAddr, VirtAddr};

use super::address_space::MMIO_REGION_BASE;
use super::paging::map_external;
use super::{Mapping, MemoryMappingOptions};

/// Next available virtual address in the MMIO region.
/// This is a simple bump allocator - MMIO mappings are typically not freed.
static MMIO_NEXT_ADDR: AtomicU64 = AtomicU64::new(MMIO_REGION_BASE);

/// Allocate virtual address space in the MMIO region.
///
/// Returns the base virtual address of the allocated region.
/// The allocation is page-aligned.
fn allocate_mmio_vaddr(size: usize) -> VirtAddr {
    let aligned_size = ((size as u64 + 4095) & !4095) as u64;
    let addr = MMIO_NEXT_ADDR.fetch_add(aligned_size, Ordering::SeqCst);
    VirtAddr::new(addr)
}

/// RAII wrapper for accessing a memory-mapped I/O region.
///
/// Provides volatile read/write access to device registers. The mapping is
/// created when the wrapper is constructed and remains valid for the lifetime
/// of the wrapper.
///
/// MMIO regions are allocated from a dedicated higher-half region starting at
/// `MMIO_REGION_BASE`, separate from the physical memory window.
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
    // The underlying Mapping handles the page table entries.
    // We leak it on drop since MMIO regions typically persist.
    _mapping: Mapping,
}

impl MmioMapping {
    /// Create a new MMIO mapping for a device region.
    ///
    /// The physical region is mapped to a virtual address in the dedicated MMIO
    /// region at `MMIO_REGION_BASE`.
    ///
    /// # Arguments
    ///
    /// * `phys_addr` - Physical address of the MMIO region
    /// * `size` - Size of the region in bytes
    pub fn new(phys_addr: PhysAddr, size: usize) -> Self {
        // Align physical address down and calculate offset
        let aligned_phys = phys_addr.align_down(4096u64);
        let offset = (phys_addr.as_u64() - aligned_phys.as_u64()) as usize;
        let aligned_size = (size + offset + 4095) & !4095;

        // Allocate virtual address space in the MMIO region
        let aligned_virt = allocate_mmio_vaddr(aligned_size);

        // Create the mapping using map_external (returns Mapping with Mmio backing)
        let mapping = map_external(
            aligned_phys,
            aligned_virt,
            aligned_size,
            MemoryMappingOptions {
                writable: true,
                executable: false,
                user: false,
            },
        );

        // The actual virt_addr includes the offset from page alignment
        let virt_addr = VirtAddr::new(aligned_virt.as_u64() + offset as u64);

        debug!(
            "MMIO: mapped phys {:#x} -> virt {:#x} (size {})",
            phys_addr.as_u64(),
            virt_addr.as_u64(),
            size
        );

        Self {
            virt_addr,
            size,
            _mapping: mapping,
        }
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

// MmioMapping stores the Mapping, so when MmioMapping is dropped, the Mapping
// is dropped too and the region is unmapped. If you need the mapping to persist,
// use core::mem::forget(mmio_mapping).
