//! Buffer interface for shared memory regions.
//!
//! SharedBuffers are page-aligned memory regions that can be:
//! - Mapped into userspace for direct access
//! - Accessed by the kernel for zero-copy I/O
//! - Transferred between processes (a handle to the same buffer, via channel
//!   handle transfer — see docs/SYSCALLS.md "Handle transfer")
//! - Mapped into more than one process at once (`OP_BUFFER_MAP`, handled by
//!   [`SharedBuffer::map_into_process`])
//!
//! # SMAP Safety
//!
//! Buffer memory is mapped into userspace. With SMAP enabled, kernel access
//! to these pages requires `stac`/`clac` bracketing. Use the safe
//! `with_slice` / `with_mut_slice` convenience methods instead of the raw
//! `as_slice` / `as_mut_slice` methods which are unsafe.
//!
//! # Process-context safety (`as_slice` / `as_mut_slice` / `mapped_addr`)
//!
//! `SharedBuffer::user_vaddr` is fixed at allocation time and always refers
//! to the *allocating* process's mapping — it does NOT change when
//! `OP_BUFFER_MAP` creates additional mappings of the same frames in other
//! processes (see "Cross-process mapping safety" below). `as_slice`,
//! `as_mut_slice`, and `mapped_addr` all dereference/report `user_vaddr`
//! directly, so they are only valid to call while the **allocating
//! process's** page table is active (i.e. from within that process's own
//! syscall context) — calling them while a *different* process's page table
//! is active would read/write whatever happens to be mapped at that same
//! numeric address in the wrong address space.
//!
//! Enforced call sites: `syscall/buffer.rs::handle_read_buffer` /
//! `handle_write_buffer` compare `SharedBuffer::owner()` against
//! `scheduler::current_process_id()` before touching the buffer's slice at
//! all, and reject the call with `ErrorCode::PermissionDenied` on mismatch —
//! so `with_slice`/`with_mut_slice` are only ever reached from the
//! allocating process, regardless of whether the caller merely holds a
//! *transferred* handle to the buffer (M1.1 handle transfer installs the
//! `SharedBuffer` resource into the receiver's handle table without
//! requiring `OP_BUFFER_MAP`, so a non-owning process can otherwise reach
//! these call sites with a valid-looking `handle_id`). `handle_free`
//! performs the same ownership check before reclaiming `user_vaddr` via
//! `proc.free_buffer_vaddr` — a non-owner's free still drops the handle
//! (and, if it was the last reference, the buffer itself) but skips
//! reclaiming vaddr space that belongs to a different process's allocator.
//! `OP_BUFFER_MAP` (`handle_map`) is deliberately exempt: it is specifically
//! the sanctioned way for a non-owner to get a valid mapping of a
//! transferred buffer, and it never calls `as_slice`/`with_slice` — it maps
//! frames directly via `SharedBuffer::map_page_range`. Any new caller of
//! `as_slice`/`with_slice` must independently enforce (or knowingly not
//! enforce) this ownership check.

use alloc::sync::{Arc, Weak};
use alloc::vec::Vec;
use core::sync::atomic::{AtomicUsize, Ordering};

use x86_64::VirtAddr;

use crate::memory::{self, Frame, Mapping, MappingBacking, MemoryMappingOptions, map_external};
use crate::memory::smap;
use crate::process::{Process, ProcessId};

use super::Resource;

/// Errors that can occur during buffer operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BufferError {
    /// Failed to allocate memory.
    AllocationFailed,
    /// Failed to map buffer into address space.
    MappingFailed,
    /// Invalid size (e.g., zero).
    InvalidSize,
}

/// Interface for buffer resources.
///
/// # Safety
///
/// `as_slice` and `as_mut_slice` access user-mapped pages. With SMAP enabled,
/// callers must bracket access with `smap::with_userspace_access` or use
/// the safe `with_slice` / `with_mut_slice` extension methods.
pub trait Buffer: Send + Sync {
    /// Get the logical size in bytes.
    fn size(&self) -> usize;

    /// Get a slice of the buffer contents for reading.
    ///
    /// # Safety
    /// The returned slice references user-mapped pages. With SMAP enabled,
    /// the caller must ensure access is within a `stac`/`clac` window.
    /// Prefer `with_slice` for safe access.
    unsafe fn as_slice(&self) -> &[u8];

    /// Get a mutable slice of the buffer contents for writing.
    ///
    /// # Safety
    /// The returned slice references user-mapped pages. With SMAP enabled,
    /// the caller must ensure access is within a `stac`/`clac` window.
    /// Prefer `with_mut_slice` for safe access.
    unsafe fn as_mut_slice(&self) -> &mut [u8];

    /// Resize the buffer. Returns the new mapped address.
    /// Uses interior mutability.
    fn resize(&self, new_size: usize) -> Result<usize, BufferError>;

    /// Get the current mapped address in userspace.
    fn mapped_addr(&self) -> usize;
}

/// Extension methods for safe SMAP-bracketed access to buffer contents.
pub trait BufferExt: Buffer {
    /// Access the buffer contents for reading via a closure.
    /// SMAP is temporarily disabled for the duration of the closure.
    fn with_slice<R>(&self, f: impl FnOnce(&[u8]) -> R) -> R {
        smap::with_userspace_access(|| {
            let slice = unsafe { self.as_slice() };
            f(slice)
        })
    }

    /// Access the buffer contents for writing via a closure.
    /// SMAP is temporarily disabled for the duration of the closure.
    fn with_mut_slice<R>(&self, f: impl FnOnce(&mut [u8]) -> R) -> R {
        smap::with_userspace_access(|| {
            let slice = unsafe { self.as_mut_slice() };
            f(slice)
        })
    }
}

// Blanket implementation for all Buffer types
impl<T: Buffer + ?Sized> BufferExt for T {}

/// A shared buffer backed by physical pages.
///
/// # Cross-process mapping safety
///
/// `frames` is this buffer's sole physical-memory identity. It is dropped
/// (freeing the physical frames) only when the last `Arc<SharedBuffer>`
/// strong reference goes away. Every *mapping* of those frames, in every
/// process, holds its own clone of that `Arc`:
///
/// - The allocating process's own mapping (`_mapping`, backed by
///   `MappingBacking::Mmio`) is a field *of* `SharedBuffer` itself — it
///   necessarily lives exactly as long as the struct that owns `frames`, so
///   it can neither outlive them nor be outlived by them.
/// - Every other process's mapping, created via `OP_BUFFER_MAP` ->
///   [`map_into_process`](SharedBuffer::map_into_process), is a `Mapping`
///   stored in that process's own `Process::mappings`, backed by
///   `MappingBacking::ExternalFrames(Arc<SharedBuffer>)`. That `Arc` clone
///   is a strong reference, so `frames` cannot be freed while this
///   `Mapping` exists — independent of what happens to the *handle* that
///   was used to create the mapping.
///
/// Walking through the three ways a mapping's lifetime can end:
///
/// - **Handle close** (`OP_BUFFER_FREE`, or a transferred handle's table
///   entry going away). This drops (at most) one `Arc<dyn Resource>` from
///   one process's *handle table* — it does not touch `Process::mappings`
///   in any process. If any `Mapping::ExternalFrames` (or the allocator's
///   own `_mapping`, implicitly via `SharedBuffer` staying alive) still
///   holds a clone, the buffer isn't dropped and every remaining mapping
///   stays valid. This is why "a closed handle with a still-live mapping"
///   is safe: the *mapping*, not the handle, is what keeps the memory
///   alive. `OP_BUFFER_FREE` does not (yet) walk other processes' mappings
///   to unmap them — teaching handle-close to unmap is future work.
/// - **Process exit.** `Process::mappings` (and `SharedBuffer::_mapping`,
///   for the allocating process, via normal struct-field drop order) is
///   dropped when the `Process` is dropped. Dropping a `Mapping` drops its
///   `MappingBacking`, which for `ExternalFrames` drops exactly the one
///   `Arc<SharedBuffer>` clone that process was holding — other processes'
///   mappings hold independent clones untouched by this.
/// - **Other-process behaviour** (e.g. `OP_BUFFER_RESIZE`'s reallocation
///   path replacing a handle's resource with a brand new `SharedBuffer`).
///   This only swaps the `Arc` stored in one handle-table entry
///   (`Handle::replace_resource`); it never mutates an existing
///   `SharedBuffer`'s `frames` or reaches into any `Mapping` elsewhere. A
///   process with a mapping into the *old* buffer keeps its own `Arc` to
///   the *old* `SharedBuffer` alive (now unreachable via the resized
///   handle, but still perfectly valid) — stale data, never a dangling
///   pointer.
///
/// In short: the frame `Vec` is freed only when no `Arc<SharedBuffer>`
/// survives, every live mapping (in every process, including the
/// allocator's own) holds one, and a `Mapping`'s keepalive clone is itself
/// a strong reference — so "the last `Arc` drops" and "a `Mapping` still
/// references the frames" are mutually exclusive by construction. No
/// interleaving of handle close, process exit, or another process's buffer
/// operations can produce a mapping into freed memory.
pub struct SharedBuffer {
    /// Physical frames backing this buffer.
    frames: Vec<Frame>,
    /// Logical size in bytes (may be less than allocated pages).
    /// Uses AtomicUsize for interior mutability.
    logical_size: AtomicUsize,
    /// Base virtual address for userspace mapping.
    user_vaddr: VirtAddr,
    /// The mapping for the userspace virtual address range.
    /// When dropped, this unmaps the pages.
    _mapping: Mapping,
    /// Weak self-reference for returning Arc<SharedBuffer> from trait methods.
    self_ref: Weak<SharedBuffer>,
    /// The process that allocated this buffer — the only process in which
    /// `user_vaddr` is meaningful. See the module-level "Process-context
    /// safety" doc comment.
    owner: ProcessId,
}

impl SharedBuffer {
    /// Allocate a new shared buffer with the given size.
    ///
    /// The buffer will be mapped into the process's address space.
    /// Returns the buffer Arc and its mapped address.
    pub fn alloc(process: &mut Process, size: usize) -> Result<(Arc<Self>, usize), BufferError> {
        if size == 0 || size > panda_abi::MAX_BUFFER_SIZE {
            return Err(BufferError::InvalidSize);
        }

        let page_size = 4096usize;
        let num_pages = (size + page_size - 1) / page_size;

        // Allocate physical frames (already zeroed by allocator)
        let mut frames = Vec::with_capacity(num_pages);
        for _ in 0..num_pages {
            let frame = memory::allocate_frame();
            frames.push(frame);
        }

        // Allocate virtual address range from the process
        let user_vaddr = process
            .alloc_buffer_vaddr(num_pages)
            .ok_or(BufferError::AllocationFailed)?;

        // Map all pages into userspace as a contiguous region
        let mapping = Self::map_frames(&frames, user_vaddr);

        let mapped_addr = user_vaddr.as_u64() as usize;

        let owner = process.id();

        let buffer = Arc::new_cyclic(|weak| Self {
            frames,
            logical_size: AtomicUsize::new(size),
            user_vaddr,
            _mapping: mapping,
            self_ref: weak.clone(),
            owner,
        });

        Ok((buffer, mapped_addr))
    }

    /// The process that allocated this buffer. `user_vaddr` (and therefore
    /// `as_slice`/`as_mut_slice`/`mapped_addr`) is only meaningful while
    /// this process's page table is active.
    pub fn owner(&self) -> ProcessId {
        self.owner
    }

    /// Map each frame individually into the CURRENT process's address space
    /// (whichever page table is active — see the module-level "Process-context
    /// safety" doc comment) at consecutive pages starting at `vaddr`.
    ///
    /// This only installs page-table entries; it does not construct a
    /// `Mapping`. Callers wrap the resulting pages with whichever
    /// `MappingBacking` matches their ownership model — see `map_frames`
    /// (allocator's own mapping, `Mmio` backing) and `map_into_process`
    /// (a second process's mapping, `ExternalFrames` backing).
    fn map_page_range(frames: &[Frame], vaddr: VirtAddr) {
        let options = MemoryMappingOptions {
            user: true,
            executable: false,
            writable: true,
        };

        // Map each frame individually (they may not be physically contiguous)
        let mut current_vaddr = vaddr;
        for frame in frames {
            // Use map_external for each page - it returns a Mapping but we'll
            // create our own combined Mapping at the end
            let page_mapping = map_external(frame.start_address(), current_vaddr, 4096, options);
            // Leak individual page mappings - we'll track the whole region
            core::mem::forget(page_mapping);
            current_vaddr += 4096u64;
        }
    }

    /// Map frames into userspace at the given virtual address.
    /// Returns a Mapping that will unmap the region when dropped.
    ///
    /// Used only for the allocating process's own mapping, created at
    /// `alloc()` time and stored in `self._mapping`. Uses `Mmio` backing
    /// (frames are owned separately, by `self.frames`) rather than
    /// `ExternalFrames`, since a keepalive here would be self-referential:
    /// `SharedBuffer` would hold a `Mapping` whose backing holds an
    /// `Arc<SharedBuffer>` back to itself, which would never drop. That
    /// keepalive is unnecessary anyway — `_mapping` is a field *of*
    /// `SharedBuffer`, so it already can't outlive `frames`.
    fn map_frames(frames: &[Frame], vaddr: VirtAddr) -> Mapping {
        Self::map_page_range(frames, vaddr);

        // Return a single Mapping covering the entire region
        // Using Mmio backing since frames are owned separately
        Mapping::new(vaddr, frames.len() * 4096, MappingBacking::Mmio)
    }

    /// Map this buffer's frames into `process` (the CURRENT process — the
    /// caller must ensure `process` is the process whose page table is
    /// active, since this installs page-table entries directly) at a
    /// freshly allocated virtual address range, and registers the mapping
    /// with `process` so it is torn down automatically on process exit.
    ///
    /// This is the kernel side of `OP_BUFFER_MAP`: unlike `alloc`, it does
    /// not create a new buffer or take frames from anywhere — it creates an
    /// additional view onto the *same* frames already owned by `self`.
    ///
    /// # Idempotence policy
    ///
    /// Calling this more than once for the same buffer in the same process
    /// (including the allocating process, which already has the buffer
    /// mapped from `alloc()`) is **not** deduplicated: each call allocates a
    /// fresh vaddr range and creates an independent mapping. This keeps the
    /// implementation simple — no per-process "which buffers are already
    /// mapped" tracking is needed — at the cost of burning extra buffer
    /// vaddr space (reclaimed at process exit) if a caller maps the same
    /// buffer redundantly. See `syscall/buffer.rs::handle_map` for the
    /// syscall-level documentation of this policy, and
    /// `buffer_test.rs`/`buffer_transfer_test` for coverage.
    ///
    /// See the struct-level "Cross-process mapping safety" doc comment for
    /// why the resulting mapping can never dangle.
    pub fn map_into_process(self: &Arc<Self>, process: &mut Process) -> Result<usize, BufferError> {
        let num_pages = self.frames.len();

        let vaddr = process
            .alloc_buffer_vaddr(num_pages)
            .ok_or(BufferError::AllocationFailed)?;

        Self::map_page_range(&self.frames, vaddr);

        // Keepalive: clone the Arc so the frames cannot be freed while this
        // process's mapping exists, regardless of what happens to whatever
        // handle was used to reach this buffer. See the struct doc comment.
        let mapping = Mapping::new(
            vaddr,
            num_pages * 4096,
            MappingBacking::ExternalFrames(self.clone()),
        );
        process.add_mapping(mapping);

        Ok(vaddr.as_u64() as usize)
    }
}

impl Buffer for SharedBuffer {
    fn size(&self) -> usize {
        self.logical_size.load(Ordering::Relaxed)
    }

    unsafe fn as_slice(&self) -> &[u8] {
        let ptr = self.user_vaddr.as_u64() as *const u8;
        let size = self.logical_size.load(Ordering::Relaxed);
        unsafe { core::slice::from_raw_parts(ptr, size) }
    }

    unsafe fn as_mut_slice(&self) -> &mut [u8] {
        let ptr = self.user_vaddr.as_u64() as *mut u8;
        let size = self.logical_size.load(Ordering::Relaxed);
        unsafe { core::slice::from_raw_parts_mut(ptr, size) }
    }

    fn resize(&self, new_size: usize) -> Result<usize, BufferError> {
        if new_size == 0 || new_size > panda_abi::MAX_BUFFER_SIZE {
            return Err(BufferError::InvalidSize);
        }

        let page_size = 4096usize;
        let new_num_pages = (new_size + page_size - 1) / page_size;
        let old_num_pages = self.frames.len();

        if new_num_pages == old_num_pages {
            // Same number of pages, just update logical size
            self.logical_size.store(new_size, Ordering::Relaxed);
            return Ok(self.user_vaddr.as_u64() as usize);
        }

        // Reallocation needed - not supported in trait method
        // Syscall handler must handle create-copy-replace logic
        Err(BufferError::AllocationFailed)
    }

    fn mapped_addr(&self) -> usize {
        self.user_vaddr.as_u64() as usize
    }
}

impl Resource for SharedBuffer {
    fn handle_type(&self) -> panda_abi::HandleType {
        panda_abi::HandleType::Buffer
    }

    fn as_buffer(&self) -> Option<&dyn Buffer> {
        Some(self)
    }

    fn as_shared_buffer(&self) -> Option<Arc<SharedBuffer>> {
        self.self_ref.upgrade()
    }
}

// Drop is handled automatically:
// - _mapping is dropped, which unmaps the pages
// - frames are dropped, which deallocates physical memory
