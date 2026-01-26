//! Memory management.
//!
//! This module handles physical and virtual memory:
//! - Frame allocation with RAII guards
//! - Page table management and mapping
//! - Heap allocation for the kernel

use core::alloc::Layout;

use x86_64::structures::paging::PhysFrame;

mod address;
mod address_space;
pub mod dma;
mod frame;
pub mod global_alloc;
pub mod heap_allocator;
mod mapping;
mod mmio;
mod paging;
pub mod recursive;

pub use address::{inspect_virtual_address, virtual_address_to_physical};
pub use address_space::{
    KERNEL_HEAP_BASE, KERNEL_IMAGE_BASE, MMIO_REGION_BASE, get_kernel_image_phys_base,
    identity_to_higher_half, jump_to_higher_half, relocate_kernel_to_higher_half,
    remove_identity_mapping,
};
pub use frame::Frame;
pub use mapping::{Mapping, MappingBacking};
pub use mmio::PhysicalMapping;
pub use paging::{
    allocate_and_map, create_user_page_table, current_page_table_phys, free_region, map_external,
    switch_page_table, try_handle_heap_page_fault, try_handle_stack_page_fault, unmap_page,
    unmap_region, update_permissions, without_write_protection,
};

/// Mapping options for memory regions.
#[derive(Clone, Copy)]
pub struct MemoryMappingOptions {
    pub user: bool,
    pub executable: bool,
    pub writable: bool,
}

/// Size of memory reserved for early page table allocations before heap is ready.
const EARLY_RESERVE: usize = 2 * 1024 * 1024;

/// Initialize memory subsystem from UEFI info.
///
/// # Safety
/// Must be called exactly once during kernel initialization.
pub unsafe fn init_from_uefi(uefi_info: &crate::uefi::UefiInfo) {
    let (heap_phys_base, heap_size) = heap_allocator::init_from_uefi(&uefi_info.memory_map);
    unsafe {
        x86_64::registers::control::Efer::update(|efer| {
            efer.insert(x86_64::registers::control::EferFlags::NO_EXECUTE_ENABLE)
        });

        // Set up recursive page tables FIRST, before anything else.
        // This is needed because map_heap_region and other functions use
        // current_page_table() which relies on recursive page table mapping.
        address_space::init(&uefi_info.memory_map);

        // Store heap physical base for address translation
        address::set_heap_phys_base(heap_phys_base as u64);

        // Map heap to KERNEL_HEAP_BASE before initializing the allocator.
        // This way the heap uses higher-half addresses from the start.
        // Reserve memory at the end for early page table allocations.
        address_space::map_heap_region(heap_phys_base as u64, heap_size as u64);

        global_alloc::init(
            address_space::KERNEL_HEAP_BASE as usize,
            heap_size - EARLY_RESERVE,
        );

        // Relocate kernel to higher-half (Phase 4)
        address_space::relocate_kernel_to_higher_half(&uefi_info.kernel_image);
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
    let phys_addr = virtual_address_to_physical(virt_addr);
    let frame = PhysFrame::from_start_address(phys_addr).unwrap();
    unsafe { Frame::new(frame, virt_addr, layout) }
}

/// Allocate a raw frame without RAII (for page table internals).
fn allocate_frame_raw() -> PhysFrame {
    let layout = Layout::from_size_align(4096, 4096).unwrap();
    let virt_addr = global_alloc::allocate(layout);
    let phys_addr = virtual_address_to_physical(virt_addr);
    PhysFrame::from_start_address(phys_addr).unwrap()
}

/// Deallocate a raw frame.
///
/// # Safety
/// The frame must have been allocated with allocate_frame_raw().
unsafe fn deallocate_frame_raw(frame: PhysFrame) {
    let layout = Layout::from_size_align(4096, 4096).unwrap();
    // Must use the heap virtual address, not the physical window address
    let ptr = address::heap_phys_to_virt(frame.start_address()).as_mut_ptr();
    unsafe {
        alloc::alloc::dealloc(ptr, layout);
    }
}
