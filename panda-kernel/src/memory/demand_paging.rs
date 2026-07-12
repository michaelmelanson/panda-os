//! Demand paging support for userspace heap and stack.
//!
//! This module handles page faults for demand-paged regions:
//! - Heap: grows from HEAP_BASE upward toward `brk`
//! - Stack: grows downward within [STACK_BASE, STACK_BASE + STACK_MAX_SIZE)
//!
//! Frames allocated for demand paging are managed by page tables directly
//! (not RAII guards) and freed via `free_region()` when the process exits.

use x86_64::{PhysAddr, VirtAddr};

use super::paging::{map_external, unmap_and_gc};
use super::MemoryMappingOptions;

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
    unmap_and_gc(virt_addr, true);
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
