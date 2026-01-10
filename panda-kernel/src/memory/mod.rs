//! Memory management.
//!
//! This module handles physical and virtual memory:
//! - Frame allocation with RAII guards
//! - Page table management and mapping
//! - Heap allocation for the kernel

use core::alloc::Layout;

use x86_64::{PhysAddr, structures::paging::PhysFrame};

mod address;
mod frame;
pub mod global_alloc;
pub mod heap_allocator;
mod mapping;
mod paging;

pub use address::{inspect_virtual_address, physical_address_to_virtual};
pub use frame::Frame;
pub use mapping::{Mapping, MappingBacking};
pub use paging::{
    allocate_and_map, create_user_page_table, current_page_table_phys, free_region, map,
    map_external, switch_page_table, try_handle_heap_page_fault, try_handle_stack_page_fault,
    unmap_region, without_write_protection,
};

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
