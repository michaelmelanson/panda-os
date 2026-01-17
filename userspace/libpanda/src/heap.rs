//! Userspace heap allocator using brk syscall with demand paging.
//!
//! This implements a simple bump allocator that grows the heap via brk().
//! The kernel handles page faults within the heap region by allocating
//! physical frames on demand.

use core::alloc::{GlobalAlloc, Layout};
use core::ptr;
use core::sync::atomic::{AtomicUsize, Ordering};

use panda_abi::{HEAP_BASE, HEAP_MAX_SIZE, OP_PROCESS_BRK};

use crate::handle::Handle;
use crate::syscall::send;

/// Simple bump allocator for userspace heap.
///
/// This allocator is simple but doesn't support deallocation.
/// For a production system, you'd want a more sophisticated allocator
/// like dlmalloc or a buddy allocator.
pub struct BumpAllocator {
    /// Current allocation pointer (next free address)
    next: AtomicUsize,
    /// Current program break (end of heap)
    brk: AtomicUsize,
}

impl BumpAllocator {
    pub const fn new() -> Self {
        Self {
            next: AtomicUsize::new(HEAP_BASE),
            brk: AtomicUsize::new(HEAP_BASE),
        }
    }

    /// Ensure the heap has at least `size` bytes available from `next`.
    /// Grows the heap via brk() if needed.
    fn ensure_capacity(&self, needed_end: usize) -> bool {
        loop {
            let current_brk = self.brk.load(Ordering::Acquire);

            if needed_end <= current_brk {
                return true;
            }

            // Need to grow - round up to page boundary for efficiency
            let new_brk = (needed_end + 0xFFF) & !0xFFF;

            // Check bounds
            if new_brk > HEAP_BASE + HEAP_MAX_SIZE {
                return false;
            }

            // Try to set the new brk via syscall
            let result = send(Handle::SELF, OP_PROCESS_BRK, new_brk, 0, 0, 0);

            if result as usize != new_brk {
                // brk failed
                return false;
            }

            // Update our cached brk
            // Use compare_exchange to handle concurrent updates
            match self.brk.compare_exchange(
                current_brk,
                new_brk,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return true,
                Err(_) => continue, // Someone else updated it, retry
            }
        }
    }
}

unsafe impl GlobalAlloc for BumpAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let size = layout.size();
        let align = layout.align();

        loop {
            let current = self.next.load(Ordering::Acquire);

            // Align up
            let aligned = (current + align - 1) & !(align - 1);
            let end = match aligned.checked_add(size) {
                Some(end) => end,
                None => return ptr::null_mut(),
            };

            // Ensure we have capacity
            if !self.ensure_capacity(end) {
                return ptr::null_mut();
            }

            // Try to claim this region
            match self.next.compare_exchange(
                current,
                end,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return aligned as *mut u8,
                Err(_) => continue, // Retry
            }
        }
    }

    unsafe fn dealloc(&self, _ptr: *mut u8, _layout: Layout) {
        // Bump allocator doesn't support deallocation.
        // Memory is reclaimed when the process exits.
        // A real allocator would track free blocks here.
    }
}

#[global_allocator]
static ALLOCATOR: BumpAllocator = BumpAllocator::new();
