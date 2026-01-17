//! Buffer interface for shared memory regions.
//!
//! SharedBuffers are page-aligned memory regions that can be:
//! - Mapped into userspace for direct access
//! - Accessed by the kernel for zero-copy I/O
//! - Transferred between processes (ownership moves)

use alloc::vec::Vec;

use x86_64::VirtAddr;

use crate::memory::{self, Frame, MemoryMappingOptions, map, physical_address_to_virtual, unmap_page};
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
    fn as_mut_slice(&mut self) -> &mut [u8];

    /// Resize the buffer. Returns the new mapped address.
    fn resize(&mut self, new_size: usize) -> Result<usize, BufferError>;

    /// Get the current mapped address in userspace.
    fn mapped_addr(&self) -> usize;
}

/// A shared buffer backed by physical pages.
pub struct SharedBuffer {
    /// Physical frames backing this buffer.
    frames: Vec<Frame>,
    /// Logical size in bytes (may be less than allocated pages).
    logical_size: usize,
    /// Base virtual address for userspace mapping.
    user_vaddr: VirtAddr,
    /// Whether currently mapped into userspace.
    is_mapped: bool,
}

impl SharedBuffer {
    /// Allocate a new shared buffer with the given size.
    ///
    /// The buffer will be mapped into the process's address space.
    /// Returns the buffer and its mapped address.
    pub fn alloc(process: &mut Process, size: usize) -> Result<(Self, usize), BufferError> {
        if size == 0 {
            return Err(BufferError::InvalidSize);
        }

        let page_size = 4096usize;
        let num_pages = (size + page_size - 1) / page_size;

        // Allocate physical frames
        let mut frames = Vec::with_capacity(num_pages);
        for _ in 0..num_pages {
            let frame = memory::allocate_frame();
            frames.push(frame);
        }

        // Allocate virtual address range from the process
        let user_vaddr = process
            .alloc_buffer_vaddr(num_pages)
            .ok_or(BufferError::AllocationFailed)?;

        // Map the pages into userspace
        Self::map_frames(&frames, user_vaddr);

        let mapped_addr = user_vaddr.as_u64() as usize;

        Ok((
            Self {
                frames,
                logical_size: size,
                user_vaddr,
                is_mapped: true,
            },
            mapped_addr,
        ))
    }

    /// Map frames into userspace at the given virtual address.
    fn map_frames(frames: &[Frame], vaddr: VirtAddr) {
        let mut current_vaddr = vaddr;

        for frame in frames {
            let options = MemoryMappingOptions {
                user: true,
                executable: false,
                writable: true,
            };
            // Map each frame as a single page (using non-RAII map function)
            map(frame.start_address(), current_vaddr, 4096, options);
            current_vaddr += 4096u64;
        }
    }

    /// Get access to the buffer's physical memory (for kernel use).
    fn get_kernel_ptr(&self) -> *const u8 {
        if self.frames.is_empty() {
            return core::ptr::null();
        }
        let phys_addr = self.frames[0].start_address();
        let virt_addr = physical_address_to_virtual(phys_addr);
        virt_addr.as_ptr()
    }

    /// Get mutable access to the buffer's physical memory (for kernel use).
    fn get_kernel_ptr_mut(&mut self) -> *mut u8 {
        if self.frames.is_empty() {
            return core::ptr::null_mut();
        }
        let phys_addr = self.frames[0].start_address();
        let virt_addr = physical_address_to_virtual(phys_addr);
        virt_addr.as_mut_ptr()
    }
}

impl Buffer for SharedBuffer {
    fn size(&self) -> usize {
        self.logical_size
    }

    fn as_slice(&self) -> &[u8] {
        let ptr = self.get_kernel_ptr();
        if ptr.is_null() {
            return &[];
        }
        unsafe { core::slice::from_raw_parts(ptr, self.logical_size) }
    }

    fn as_mut_slice(&mut self) -> &mut [u8] {
        let ptr = self.get_kernel_ptr_mut();
        if ptr.is_null() {
            return &mut [];
        }
        unsafe { core::slice::from_raw_parts_mut(ptr, self.logical_size) }
    }

    fn resize(&mut self, new_size: usize) -> Result<usize, BufferError> {
        if new_size == 0 {
            return Err(BufferError::InvalidSize);
        }

        let page_size = 4096usize;
        let new_num_pages = (new_size + page_size - 1) / page_size;
        let old_num_pages = self.frames.len();

        if new_num_pages == old_num_pages {
            // Same number of pages, just update logical size
            self.logical_size = new_size;
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
}

impl Drop for SharedBuffer {
    fn drop(&mut self) {
        // Unmap pages from userspace
        if self.is_mapped {
            let mut current_vaddr = self.user_vaddr;
            for _ in &self.frames {
                unmap_page(current_vaddr);
                current_vaddr += 4096u64;
            }
        }
        // Frames are dropped automatically, releasing physical memory
    }
}
