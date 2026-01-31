//! Shared buffer operations.
//!
//! Shared buffers provide page-aligned memory regions that can be efficiently
//! shared between userspace and kernel for zero-copy I/O operations.

use crate::handle::Handle;
use crate::sys;
use core::slice;
use panda_abi::BufferAllocInfo;

/// A shared buffer for zero-copy I/O operations.
///
/// The buffer is allocated by the kernel and mapped into the process's address space.
/// It can be used for efficient file I/O without copying data through syscall boundaries.
pub struct Buffer {
    handle: Handle,
    addr: *mut u8,
    size: usize,
}

impl Buffer {
    /// Allocate a new shared buffer with at least the specified size.
    ///
    /// The actual size may be rounded up to page alignment.
    /// Returns `None` if allocation fails.
    pub fn alloc(size: usize) -> Option<Self> {
        let mut info = BufferAllocInfo { addr: 0, size: 0 };
        let result = sys::buffer::alloc(size, Some(&mut info));

        if result < 0 {
            return None;
        }

        Some(Self {
            handle: Handle::from(result as u64),
            addr: info.addr as *mut u8,
            size: info.size,
        })
    }

    /// Get the buffer's handle.
    pub fn handle(&self) -> Handle {
        self.handle
    }

    /// Get the buffer's size in bytes.
    pub fn size(&self) -> usize {
        self.size
    }

    /// Get the buffer contents as a slice.
    pub fn as_slice(&self) -> &[u8] {
        // SAFETY: `addr` and `size` come from a successful kernel allocation
        // (sys::buffer::alloc returned a positive handle). The kernel guarantees
        // the memory is valid and mapped for the lifetime of the handle. The
        // borrow is tied to `&self`, ensuring no mutable access while active.
        unsafe { slice::from_raw_parts(self.addr, self.size) }
    }

    /// Get the buffer contents as a mutable slice.
    pub fn as_mut_slice(&mut self) -> &mut [u8] {
        // SAFETY: Same as `as_slice`, plus `&mut self` ensures exclusive access.
        unsafe { slice::from_raw_parts_mut(self.addr, self.size) }
    }

    /// Resize the buffer.
    ///
    /// Returns the new address on success. The buffer contents may be moved,
    /// so any existing slices become invalid.
    pub fn resize(&mut self, new_size: usize) -> Option<()> {
        let mut info = BufferAllocInfo { addr: 0, size: 0 };
        let result = sys::buffer::resize(self.handle, new_size, Some(&mut info));

        if result < 0 {
            return None;
        }

        self.addr = info.addr as *mut u8;
        self.size = info.size;

        Some(())
    }

    /// Read from a file into this buffer.
    ///
    /// Returns the number of bytes read.
    pub fn read_from(&mut self, file_handle: Handle) -> Option<usize> {
        let result = sys::buffer::read_from_file(file_handle, self.handle);
        if result < 0 {
            None
        } else {
            Some(result as usize)
        }
    }

    /// Write from this buffer to a file.
    ///
    /// Returns the number of bytes written.
    pub fn write_to(&self, file_handle: Handle, len: usize) -> Option<usize> {
        let result = sys::buffer::write_to_file(file_handle, self.handle, len);
        if result < 0 {
            None
        } else {
            Some(result as usize)
        }
    }
}

impl Drop for Buffer {
    fn drop(&mut self) {
        let _ = sys::buffer::free(self.handle);
    }
}
