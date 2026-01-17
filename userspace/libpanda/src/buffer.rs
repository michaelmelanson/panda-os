//! Shared buffer operations.
//!
//! Shared buffers provide page-aligned memory regions that can be efficiently
//! shared between userspace and kernel for zero-copy I/O operations.

use crate::handle::Handle;
use crate::syscall::send;
use core::slice;
use panda_abi::{
    OP_BUFFER_ALLOC, OP_BUFFER_FREE, OP_BUFFER_RESIZE, OP_FILE_READ_BUFFER,
    OP_FILE_WRITE_BUFFER,
};

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
        use panda_abi::BufferAllocInfo;

        let mut info = BufferAllocInfo { addr: 0, size: 0 };
        let result = send(
            Handle::ENVIRONMENT,
            OP_BUFFER_ALLOC,
            size,
            &mut info as *mut BufferAllocInfo as usize,
            0,
            0,
        );

        if result < 0 {
            return None;
        }

        Some(Self {
            handle: Handle::from(result as u32),
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
        unsafe { slice::from_raw_parts(self.addr, self.size) }
    }

    /// Get the buffer contents as a mutable slice.
    pub fn as_mut_slice(&mut self) -> &mut [u8] {
        unsafe { slice::from_raw_parts_mut(self.addr, self.size) }
    }

    /// Resize the buffer.
    ///
    /// Returns the new address on success. The buffer contents may be moved,
    /// so any existing slices become invalid.
    pub fn resize(&mut self, new_size: usize) -> Option<()> {
        use panda_abi::BufferAllocInfo;

        let mut info = BufferAllocInfo { addr: 0, size: 0 };
        let result = send(
            self.handle,
            OP_BUFFER_RESIZE,
            new_size,
            &mut info as *mut BufferAllocInfo as usize,
            0,
            0,
        );

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
        let result = send(
            file_handle,
            OP_FILE_READ_BUFFER,
            u32::from(self.handle) as usize,
            0,
            0,
            0,
        );
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
        let result = send(
            file_handle,
            OP_FILE_WRITE_BUFFER,
            u32::from(self.handle) as usize,
            len,
            0,
            0,
        );
        if result < 0 {
            None
        } else {
            Some(result as usize)
        }
    }
}

impl Drop for Buffer {
    fn drop(&mut self) {
        let _ = send(self.handle, OP_BUFFER_FREE, 0, 0, 0, 0);
    }
}
