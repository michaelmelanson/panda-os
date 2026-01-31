//! Low-level buffer operations.
//!
//! These functions provide direct syscall access for shared memory buffers.
//! For RAII wrappers, use `crate::mem::Buffer`.

use super::{Handle, send};
use panda_abi::*;

/// Allocate a shared buffer.
///
/// If `info` is provided, writes the buffer address and size to it.
/// Returns buffer handle on success, or negative error code.
#[inline(always)]
pub fn alloc(size: usize, info: Option<&mut BufferAllocInfo>) -> isize {
    let info_ptr = match info {
        Some(i) => i as *mut BufferAllocInfo as usize,
        None => 0,
    };
    send(Handle::ENVIRONMENT, OP_BUFFER_ALLOC, size, info_ptr, 0, 0)
}

/// Resize a buffer.
///
/// If `info` is provided, writes the new buffer address and size to it.
/// Returns 0 on success, or negative error code.
#[inline(always)]
pub fn resize(handle: Handle, new_size: usize, info: Option<&mut BufferAllocInfo>) -> isize {
    let info_ptr = match info {
        Some(i) => i as *mut BufferAllocInfo as usize,
        None => 0,
    };
    send(handle, OP_BUFFER_RESIZE, new_size, info_ptr, 0, 0)
}

/// Free a buffer.
///
/// Returns 0 on success, or negative error code.
#[inline(always)]
pub fn free(handle: Handle) -> isize {
    send(handle, OP_BUFFER_FREE, 0, 0, 0, 0)
}

/// Read from a file into a buffer.
///
/// Returns number of bytes read, or negative error code.
#[inline(always)]
pub fn read_from_file(file_handle: Handle, buffer_handle: Handle) -> isize {
    send(
        file_handle,
        OP_FILE_READ_BUFFER,
        u64::from(buffer_handle) as usize,
        0,
        0,
        0,
    )
}

/// Write from a buffer to a file.
///
/// Returns number of bytes written, or negative error code.
#[inline(always)]
pub fn write_to_file(file_handle: Handle, buffer_handle: Handle, len: usize) -> isize {
    send(
        file_handle,
        OP_FILE_WRITE_BUFFER,
        u64::from(buffer_handle) as usize,
        len,
        0,
        0,
    )
}
