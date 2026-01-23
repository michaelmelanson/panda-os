//! Buffer interface for shared memory regions.
//!
//! SharedBuffers are page-aligned memory regions that can be:
//! - Mapped into userspace for direct access
//! - Accessed by the kernel for zero-copy I/O
//! - Transferred between processes (ownership moves)

use alloc::sync::{Arc, Weak};
use alloc::vec::Vec;
use core::sync::atomic::{AtomicUsize, Ordering};

use x86_64::VirtAddr;

use crate::memory::{self, Frame, Mapping, MappingBacking, MemoryMappingOptions, map_external};
use crate::process::Process;

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
pub trait Buffer: Send + Sync {
    /// Get the logical size in bytes.
    fn size(&self) -> usize;

    /// Get a slice of the buffer contents for reading.
    fn as_slice(&self) -> &[u8];

    /// Get a mutable slice of the buffer contents for writing.
    /// Uses interior mutability.
    fn as_mut_slice(&self) -> &mut [u8];

    /// Resize the buffer. Returns the new mapped address.
    /// Uses interior mutability.
    fn resize(&self, new_size: usize) -> Result<usize, BufferError>;

    /// Get the current mapped address in userspace.
    fn mapped_addr(&self) -> usize;
}

/// A shared buffer backed by physical pages.
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
}

impl SharedBuffer {
    /// Allocate a new shared buffer with the given size.
    ///
    /// The buffer will be mapped into the process's address space.
    /// Returns the buffer Arc and its mapped address.
    pub fn alloc(process: &mut Process, size: usize) -> Result<(Arc<Self>, usize), BufferError> {
        if size == 0 {
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

        let buffer = Arc::new_cyclic(|weak| Self {
            frames,
            logical_size: AtomicUsize::new(size),
            user_vaddr,
            _mapping: mapping,
            self_ref: weak.clone(),
        });

        Ok((buffer, mapped_addr))
    }

    /// Map frames into userspace at the given virtual address.
    /// Returns a Mapping that will unmap the region when dropped.
    fn map_frames(frames: &[Frame], vaddr: VirtAddr) -> Mapping {
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

        // Return a single Mapping covering the entire region
        // Using Mmio backing since frames are owned separately
        Mapping::new(vaddr, frames.len() * 4096, MappingBacking::Mmio)
    }
}

impl Buffer for SharedBuffer {
    fn size(&self) -> usize {
        self.logical_size.load(Ordering::Relaxed)
    }

    fn as_slice(&self) -> &[u8] {
        let ptr = self.user_vaddr.as_u64() as *const u8;
        let size = self.logical_size.load(Ordering::Relaxed);
        unsafe { core::slice::from_raw_parts(ptr, size) }
    }

    fn as_mut_slice(&self) -> &mut [u8] {
        let ptr = self.user_vaddr.as_u64() as *mut u8;
        let size = self.logical_size.load(Ordering::Relaxed);
        unsafe { core::slice::from_raw_parts_mut(ptr, size) }
    }

    fn resize(&self, new_size: usize) -> Result<usize, BufferError> {
        if new_size == 0 {
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
    fn as_buffer(&self) -> Option<&dyn Buffer> {
        Some(self)
    }

    fn as_buffer_mut(&mut self) -> Option<&mut dyn Buffer> {
        Some(self)
    }

    fn as_shared_buffer(&self) -> Option<Arc<SharedBuffer>> {
        self.self_ref.upgrade()
    }
}

// Drop is handled automatically:
// - _mapping is dropped, which unmaps the pages
// - frames are dropped, which deallocates physical memory
