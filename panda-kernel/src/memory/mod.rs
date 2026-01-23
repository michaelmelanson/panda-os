//! Memory management.
//!
//! This module handles physical and virtual memory:
//! - Frame allocation with RAII guards
//! - Page table management and mapping
//! - Heap allocation for the kernel

use core::alloc::Layout;

use x86_64::{PhysAddr, structures::paging::PhysFrame};

mod address;
mod address_space;
pub mod dma;
mod frame;
pub mod global_alloc;
pub mod heap_allocator;
mod mapping;
mod mmio;
mod paging;
mod phys;

pub use address::{
    get_phys_map_base, inspect_virtual_address, physical_address_to_virtual, set_phys_map_base,
    virtual_address_to_physical,
};
pub use address_space::{KERNEL_HEAP_BASE, KERNEL_IMAGE_BASE, MMIO_REGION_BASE, PHYS_WINDOW_BASE};
pub use frame::Frame;
pub use mapping::{Mapping, MappingBacking};
pub use mmio::MmioMapping;
pub use paging::{
    allocate_and_map, create_user_page_table, current_page_table_phys, free_region, map,
    map_external, switch_page_table, try_handle_heap_page_fault, try_handle_stack_page_fault,
    unmap_page, unmap_region, update_permissions, without_write_protection,
};
pub use phys::{PhysicalMapping, PhysicalSlice};

/// Mapping options for memory regions.
pub struct MemoryMappingOptions {
    pub user: bool,
    pub executable: bool,
    pub writable: bool,
}

/// Initialize memory subsystem from UEFI memory map.
///
/// # Safety
/// Must be called exactly once during kernel initialization.
pub unsafe fn init_from_uefi(memory_map: &uefi::mem::memory_map::MemoryMapOwned) {
    let (heap_phys_base, heap_size) = heap_allocator::init_from_uefi(memory_map);
    unsafe {
        global_alloc::init(heap_phys_base, heap_size);
        x86_64::registers::control::Efer::update(|efer| {
            efer.insert(x86_64::registers::control::EferFlags::NO_EXECUTE_ENABLE)
        });
        // Create the physical memory window in higher-half address space
        address_space::init(memory_map);
    }
}

/// Allocate a single 4KB frame with RAII guard.
pub fn allocate_frame() -> Frame {
    let layout = Layout::from_size_align(4096, 4096).unwrap();
    allocate_physical(layout)
}

/// Allocate physical memory with RAII guard.
pub fn allocate_physical(layout: Layout) -> Frame {
    let virt_addr = global_alloc::allocate(layout);
    let phys_addr = PhysAddr::new(virt_addr.as_u64());
    let frame = PhysFrame::from_start_address(phys_addr).unwrap();
    unsafe { Frame::new(frame, layout) }
}

/// Allocate a raw frame without RAII (for page table internals).
fn allocate_frame_raw() -> PhysFrame {
    let layout = Layout::from_size_align(4096, 4096).unwrap();
    let virt_addr = global_alloc::allocate(layout);
    let phys_addr = PhysAddr::new(virt_addr.as_u64());
    PhysFrame::from_start_address(phys_addr).unwrap()
}

/// Deallocate a raw frame.
///
/// # Safety
/// The frame must have been allocated with allocate_frame_raw().
unsafe fn deallocate_frame_raw(frame: PhysFrame) {
    let layout = Layout::from_size_align(4096, 4096).unwrap();
    let ptr = frame.start_address().as_u64() as *mut u8;
    unsafe {
        alloc::alloc::dealloc(ptr, layout);
    }
}

/// Map an MMIO region to the dedicated MMIO virtual address region.
///
/// Creates a mapping in the MMIO region at `MMIO_REGION_BASE` and returns
/// the virtual address. The mapping is leaked (persists until kernel shutdown).
///
/// For new code, prefer using `MmioMapping::new()` directly which provides
/// RAII cleanup.
///
/// Returns the virtual address in the MMIO region.
pub fn map_mmio(phys_addr: PhysAddr, size: usize) -> x86_64::VirtAddr {
    // Create an MmioMapping and leak it (callers expect persistent mappings)
    let mapping = MmioMapping::new(phys_addr, size);
    let virt_addr = mapping.virt_addr();
    core::mem::forget(mapping);
    virt_addr
}
