//! Physical memory mapping with RAII wrappers.
//!
//! This module provides type-safe, volatile access to physical memory through RAII
//! wrappers. All device register access and external physical memory access should
//! go through `PhysicalMapping` to ensure proper memory ordering and lifecycle.
//!
//! Physical mappings are allocated in a dedicated virtual address region starting at
//! `MMIO_REGION_BASE` (0xffff_9000_0000_0000). Virtual address allocation supports
//! both allocation and deallocation, allowing temporary mappings to be freed.

use core::ptr::{read_volatile, write_volatile};

use alloc::collections::BTreeMap;
use log::debug;
use spinning_top::Spinlock;
use x86_64::{PhysAddr, VirtAddr};

use super::address_space::MMIO_REGION_BASE;
use super::paging::map_external;
use super::{Mapping, MemoryMappingOptions};

/// Size of the MMIO virtual address region (16 TB).
const MMIO_REGION_SIZE: u64 = 0x1000_0000_0000;

/// Virtual address allocator for physical mappings.
///
/// Uses a simple free-list approach with a BTreeMap keyed by address.
/// Free regions are coalesced when adjacent allocations are freed.
struct MmioVaddrAllocator {
    /// Map of free region start addresses to their sizes.
    free_regions: BTreeMap<u64, u64>,
    /// Next address for bump allocation when no free regions fit.
    bump_next: u64,
}

impl MmioVaddrAllocator {
    const fn new() -> Self {
        Self {
            free_regions: BTreeMap::new(),
            bump_next: MMIO_REGION_BASE,
        }
    }

    /// Allocate virtual address space of the given size (must be page-aligned).
    fn allocate(&mut self, size: u64) -> VirtAddr {
        // First-fit search in free regions
        let mut found_addr = None;
        for (&addr, &region_size) in &self.free_regions {
            if region_size >= size {
                found_addr = Some((addr, region_size));
                break;
            }
        }

        if let Some((addr, region_size)) = found_addr {
            self.free_regions.remove(&addr);
            // If there's leftover space, put it back
            if region_size > size {
                self.free_regions.insert(addr + size, region_size - size);
            }
            return VirtAddr::new(addr);
        }

        // No suitable free region - bump allocate
        let addr = self.bump_next;
        assert!(
            addr + size <= MMIO_REGION_BASE + MMIO_REGION_SIZE,
            "MMIO region exhausted"
        );
        self.bump_next = addr + size;
        VirtAddr::new(addr)
    }

    /// Free virtual address space, returning it to the allocator.
    fn deallocate(&mut self, addr: VirtAddr, size: u64) {
        let addr = addr.as_u64();

        // Try to coalesce with adjacent regions
        // Check for region immediately before
        let mut new_addr = addr;
        let mut new_size = size;

        // Find and merge with predecessor if adjacent
        let predecessor = self
            .free_regions
            .range(..addr)
            .next_back()
            .map(|(&a, &s)| (a, s));
        if let Some((pred_addr, pred_size)) = predecessor {
            if pred_addr + pred_size == addr {
                // Merge with predecessor
                self.free_regions.remove(&pred_addr);
                new_addr = pred_addr;
                new_size += pred_size;
            }
        }

        // Find and merge with successor if adjacent
        if let Some(&succ_size) = self.free_regions.get(&(new_addr + new_size)) {
            self.free_regions.remove(&(new_addr + new_size));
            new_size += succ_size;
        }

        self.free_regions.insert(new_addr, new_size);
    }
}

static MMIO_VADDR_ALLOCATOR: Spinlock<MmioVaddrAllocator> =
    Spinlock::new(MmioVaddrAllocator::new());

/// Allocate virtual address space in the physical mapping region.
fn allocate_phys_mapping_vaddr(size: usize) -> VirtAddr {
    let aligned_size = ((size as u64 + 4095) & !4095) as u64;
    MMIO_VADDR_ALLOCATOR.lock().allocate(aligned_size)
}

/// Free virtual address space in the physical mapping region.
fn deallocate_phys_mapping_vaddr(addr: VirtAddr, size: usize) {
    let aligned_size = ((size as u64 + 4095) & !4095) as u64;
    MMIO_VADDR_ALLOCATOR.lock().deallocate(addr, aligned_size)
}

/// RAII wrapper for accessing physical memory.
///
/// Provides volatile read/write access to physical memory regions such as
/// device MMIO registers, ACPI tables, or other external physical addresses.
/// The mapping is created when the wrapper is constructed and unmapped on drop.
///
/// Physical mappings are allocated from a dedicated higher-half region starting at
/// `MMIO_REGION_BASE` (0xffff_9000_0000_0000).
///
/// # Example
///
/// ```ignore
/// let mapping = PhysicalMapping::new(pci_bar_phys_addr, 4096);
/// let status: u32 = mapping.read(0x10);
/// mapping.write(0x14, 0x1234u32);
/// ```
pub struct PhysicalMapping {
    /// Virtual address (includes offset for non-page-aligned physical addresses).
    virt_addr: VirtAddr,
    /// Size of the mapping in bytes.
    size: usize,
    /// Page-aligned virtual address for deallocation.
    aligned_virt: VirtAddr,
    /// Page-aligned size for deallocation.
    aligned_size: usize,
    /// The underlying Mapping handles the page table entries.
    _mapping: Mapping,
}

impl PhysicalMapping {
    /// Create a new physical memory mapping.
    ///
    /// The physical region is mapped to a virtual address in the dedicated
    /// physical mapping region at `MMIO_REGION_BASE`.
    ///
    /// # Arguments
    ///
    /// * `phys_addr` - Physical address to map
    /// * `size` - Size of the region in bytes
    pub fn new(phys_addr: PhysAddr, size: usize) -> Self {
        // Align physical address down and calculate offset
        let aligned_phys = phys_addr.align_down(4096u64);
        let offset = (phys_addr.as_u64() - aligned_phys.as_u64()) as usize;
        let aligned_size = (size + offset + 4095) & !4095;

        // Allocate virtual address space in the physical mapping region
        let aligned_virt = allocate_phys_mapping_vaddr(aligned_size);

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
            "PhysicalMapping: mapped phys {:#x} -> virt {:#x} (size {})",
            phys_addr.as_u64(),
            virt_addr.as_u64(),
            size
        );

        Self {
            virt_addr,
            size,
            aligned_virt,
            aligned_size,
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

    /// Read a value from the mapping at the given byte offset.
    ///
    /// # Panics
    ///
    /// Panics if the offset plus the size of T exceeds the mapping size.
    pub fn read<T: Copy>(&self, offset: usize) -> T {
        assert!(
            offset + core::mem::size_of::<T>() <= self.size,
            "PhysicalMapping read out of bounds"
        );
        let ptr = (self.virt_addr.as_u64() + offset as u64) as *const T;
        unsafe { read_volatile(ptr) }
    }

    /// Write a value to the mapping at the given byte offset.
    ///
    /// # Panics
    ///
    /// Panics if the offset plus the size of T exceeds the mapping size.
    pub fn write<T: Copy>(&self, offset: usize, value: T) {
        assert!(
            offset + core::mem::size_of::<T>() <= self.size,
            "PhysicalMapping write out of bounds"
        );
        let ptr = (self.virt_addr.as_u64() + offset as u64) as *mut T;
        unsafe { write_volatile(ptr, value) }
    }

    /// Read a value from the mapping without bounds checking.
    ///
    /// # Safety
    ///
    /// The caller must ensure the offset is within bounds.
    pub unsafe fn read_unchecked<T: Copy>(&self, offset: usize) -> T {
        let ptr = (self.virt_addr.as_u64() + offset as u64) as *const T;
        unsafe { read_volatile(ptr) }
    }

    /// Write a value to the mapping without bounds checking.
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

impl Drop for PhysicalMapping {
    fn drop(&mut self) {
        // Return virtual address space to the allocator
        deallocate_phys_mapping_vaddr(self.aligned_virt, self.aligned_size);
        debug!(
            "PhysicalMapping: unmapped virt {:#x} (size {})",
            self.aligned_virt.as_u64(),
            self.aligned_size
        );
        // The _mapping field is dropped automatically, which unmaps the page table entries
    }
}
