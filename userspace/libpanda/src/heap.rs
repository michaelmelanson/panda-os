//! Userspace heap allocator using the `talc` crate with `brk()` growth.
//!
//! This wraps the [`talc`] free-list allocator with a custom OOM handler
//! that extends the heap via the `brk()` syscall. The allocator properly
//! tracks free blocks and coalesces adjacent freed regions, so memory is
//! reused after deallocation.
//!
//! ## Growth policy
//!
//! When the allocator cannot satisfy a request from its free list, the
//! [`BrkGrower`] OOM handler is invoked. It rounds the needed size up to
//! the next page boundary (4 KiB) and calls `brk()` to extend the heap.
//! The new memory is added to the allocator's managed region via
//! [`Talc::extend`] (if contiguous with the existing heap) or
//! [`Talc::claim`] (for the initial allocation).

use core::alloc::Layout;

use panda_abi::{HEAP_BASE, HEAP_MAX_SIZE, OP_PROCESS_BRK};
use spinning_top::RawSpinlock;
use talc::{OomHandler, Span, Talc, Talck};

use crate::handle::Handle;
use crate::sys::send;

const PAGE_SIZE: usize = 4096;

/// OOM handler that grows the heap via the `brk()` syscall.
///
/// Maintains the current program break and arena span so it can extend
/// the heap when the allocator runs out of free memory.
struct BrkGrower {
    /// Current program break (end of committed heap memory).
    current_brk: usize,
    /// The current arena span managed by the allocator, tracked here
    /// so we can pass it to [`Talc::extend`] on subsequent growths.
    arena: Span,
}

impl BrkGrower {
    const fn new() -> Self {
        Self {
            current_brk: HEAP_BASE,
            arena: Span::empty(),
        }
    }
}

/// Grow the heap by at least `min_size` bytes via `brk()`.
/// Returns the (start, end) of the newly committed region, or `None` on failure.
fn brk_grow(current_brk: &mut usize, min_size: usize) -> Option<(usize, usize)> {
    let new_brk = align_up(*current_brk + min_size, PAGE_SIZE);

    // Check bounds
    if new_brk > HEAP_BASE + HEAP_MAX_SIZE {
        return None;
    }

    // Request the kernel to extend the program break
    let result = send(Handle::SELF, OP_PROCESS_BRK, new_brk, 0, 0, 0);

    if result as usize != new_brk {
        return None;
    }

    let old_brk = *current_brk;
    *current_brk = new_brk;
    Some((old_brk, new_brk))
}

impl OomHandler for BrkGrower {
    fn handle_oom(talc: &mut Talc<Self>, layout: Layout) -> Result<(), ()> {
        // Extract state from the OOM handler, then release the borrow
        // so we can call talc.claim()/talc.extend() below.
        let min_size = layout.size().max(PAGE_SIZE);

        let (start, end) = {
            let grower = talc.oom_handler_mut();
            brk_grow(&mut grower.current_brk, min_size).ok_or(())?
        };

        let new_memory = unsafe { Span::from_base_size(start as *mut u8, end - start) };

        let old_arena = talc.oom_handler_mut().arena;
        if old_arena.is_empty() {
            // First allocation — claim the initial region.
            let arena = unsafe { talc.claim(new_memory).map_err(|_| ())? };
            talc.oom_handler_mut().arena = arena;
        } else {
            // Extend the existing heap region. Since we always grow
            // contiguously from current_brk, this extends the arena.
            let arena = unsafe { talc.extend(old_arena, new_memory) };
            talc.oom_handler_mut().arena = arena;
        }

        Ok(())
    }
}

/// Align `val` up to the next multiple of `align`.
const fn align_up(val: usize, align: usize) -> usize {
    (val + align - 1) & !(align - 1)
}

#[cfg(feature = "os")]
#[global_allocator]
static ALLOCATOR: Talck<RawSpinlock, BrkGrower> = Talc::new(BrkGrower::new()).lock();
