//! Demand paging support for userspace heap and stack.
//!
//! This module handles page faults for demand-paged regions:
//! - Heap: grows from HEAP_BASE upward toward `brk`
//! - Stack: grows downward within [STACK_BASE, STACK_BASE + STACK_MAX_SIZE)
//!
//! Frames allocated for demand paging are managed by page tables directly
//! (not RAII guards) and freed via `free_region()` when the process exits.

use log::debug;
use x86_64::{
    PhysAddr, VirtAddr,
    instructions::tlb,
    structures::paging::{PageTableFlags, PhysFrame, page_table::PageTableLevel},
};

use super::paging::{map_external, without_write_protection};
use super::recursive;
use super::{MemoryMappingOptions, deallocate_frame_raw};

/// Free a region by walking page tables, deallocating mapped frames, and clearing PTEs.
///
/// Unlike `unmap_region`, this also deallocates the physical frames.
/// Used for demand-paged regions where frames aren't tracked separately.
pub fn free_region(base_virt: VirtAddr, size_bytes: usize) {
    for offset in (0..size_bytes).step_by(4096) {
        let virt_addr = base_virt + offset as u64;
        free_page(virt_addr);
    }
}

/// Free a single page: deallocate its frame (if mapped) and clear the PTE.
///
/// Unlike `unmap_page`, this also deallocates the physical frame.
fn free_page(virt_addr: VirtAddr) {
    let levels = [
        PageTableLevel::Four,
        PageTableLevel::Three,
        PageTableLevel::Two,
        PageTableLevel::One,
    ];

    // Walk down to find the L1 entry
    for level in levels.iter() {
        let table = unsafe { recursive::table_for_addr(virt_addr, *level) };
        let index = virt_addr.page_table_index(*level);
        let entry = &table[index];

        if !entry.flags().contains(PageTableFlags::PRESENT) {
            return; // Not mapped, nothing to free
        }

        if *level == PageTableLevel::One {
            // Found L1 entry - get the frame address before clearing
            let frame_addr = entry.addr();
            let frame = PhysFrame::from_start_address(frame_addr).unwrap();

            // Clear the entry
            let table = unsafe { recursive::table_for_addr_mut(virt_addr, *level) };
            without_write_protection(|| {
                table[index].set_unused();
            });
            tlb::flush(virt_addr);

            // Deallocate the frame
            unsafe {
                deallocate_frame_raw(frame);
            }
            break;
        }

        // Handle huge pages at level 2 (not expected for heap, but handle anyway)
        if *level == PageTableLevel::Two && entry.flags().contains(PageTableFlags::HUGE_PAGE) {
            let table = unsafe { recursive::table_for_addr_mut(virt_addr, *level) };
            without_write_protection(|| {
                table[index].set_unused();
            });
            tlb::flush(virt_addr);
            // Note: huge page frame deallocation not implemented
            return;
        }
    }

    // Walk back up and free empty intermediate tables
    for level in [
        PageTableLevel::One,
        PageTableLevel::Two,
        PageTableLevel::Three,
    ] {
        let child_table = unsafe { recursive::table_for_addr(virt_addr, level) };

        let is_empty = child_table
            .iter()
            .all(|entry| !entry.flags().contains(PageTableFlags::PRESENT));

        if !is_empty {
            break;
        }

        // Safe to unwrap: levels 1, 2, 3 all have a higher level
        let parent_level = level.next_higher_level().unwrap();
        let parent_table = unsafe { recursive::table_for_addr_mut(virt_addr, parent_level) };
        let parent_index = virt_addr.page_table_index(parent_level);

        let child_frame_addr = parent_table[parent_index].addr();
        let child_frame = PhysFrame::from_start_address(child_frame_addr).unwrap();

        without_write_protection(|| {
            parent_table[parent_index].set_unused();
        });

        unsafe {
            deallocate_frame_raw(child_frame);
        }

        debug!(
            "Freed empty page table at {:?} (level {:?})",
            child_frame_addr, level
        );
    }
}

/// Try to handle a page fault for userspace heap demand paging.
///
/// Returns true if handled, false if fault should be treated as error.
///
/// The allocated frame is intentionally leaked (not tracked by RAII) because
/// heap frames are managed by the page tables themselves and freed via `free_region()`.
pub fn try_handle_heap_page_fault(fault_addr: VirtAddr, brk: VirtAddr) -> bool {
    let heap_base = panda_abi::HEAP_BASE as u64;

    // Check if fault address is within the valid heap region [HEAP_BASE, brk)
    if fault_addr.as_u64() < heap_base || fault_addr.as_u64() >= brk.as_u64() {
        return false;
    }

    // Page-align the fault address
    let page_addr = VirtAddr::new(fault_addr.as_u64() & !0xFFF);

    // Allocate a physical frame (already zeroed by alloc_zeroed)
    let frame = super::allocate_frame();
    let phys_addr = PhysAddr::new(frame.phys_frame().start_address().as_u64());

    // Map it to the faulting address (user, writable, no-execute)
    let mapping = map_external(
        phys_addr,
        page_addr,
        4096,
        MemoryMappingOptions {
            user: true,
            writable: true,
            executable: false,
        },
    );

    // Intentionally leak the frame and mapping - they're now owned by the page tables
    // and will be freed when the heap shrinks or process exits via free_region()
    core::mem::forget(frame);
    core::mem::forget(mapping);

    true
}

/// Try to handle a page fault for userspace stack demand paging.
///
/// Returns true if handled, false if fault should be treated as error.
/// Stack grows downward within [STACK_BASE, STACK_BASE + STACK_MAX_SIZE).
pub fn try_handle_stack_page_fault(fault_addr: VirtAddr) -> bool {
    let stack_base = panda_abi::STACK_BASE as u64;
    let stack_end = stack_base + panda_abi::STACK_MAX_SIZE as u64;

    // Check if fault address is within the stack region
    if fault_addr.as_u64() < stack_base || fault_addr.as_u64() >= stack_end {
        return false;
    }

    // Page-align the fault address
    let page_addr = VirtAddr::new(fault_addr.as_u64() & !0xFFF);

    // Allocate a physical frame (already zeroed by alloc_zeroed)
    let frame = super::allocate_frame();
    let phys_addr = PhysAddr::new(frame.phys_frame().start_address().as_u64());

    // Map it to the faulting address (user, writable, no-execute)
    let mapping = map_external(
        phys_addr,
        page_addr,
        4096,
        MemoryMappingOptions {
            user: true,
            writable: true,
            executable: false,
        },
    );

    // Intentionally leak the frame and mapping - they're now owned by the page tables
    // and will be freed when the process exits via free_region()
    core::mem::forget(frame);
    core::mem::forget(mapping);

    true
}
