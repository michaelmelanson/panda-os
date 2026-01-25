//! File operations using the send-based API

use crate::handle::Handle;
use crate::syscall::send;
use panda_abi::*;

/// Read from a file handle into a buffer (blocking)
///
/// Returns number of bytes read, or negative error code
#[inline(always)]
pub fn read(handle: Handle, buf: &mut [u8]) -> isize {
    send(
        handle,
        OP_FILE_READ,
        buf.as_mut_ptr() as usize,
        buf.len(),
        0,
        0,
    )
}

/// Try to read from a file handle (non-blocking)
///
/// Returns number of bytes read, 0 if no data available, or negative error code.
/// Unlike `read`, this never blocks waiting for data.
#[inline(always)]
pub fn try_read(handle: Handle, buf: &mut [u8]) -> isize {
    send(
        handle,
        OP_FILE_READ,
        buf.as_mut_ptr() as usize,
        buf.len(),
        FILE_NONBLOCK as usize,
        0,
    )
}

/// Write to a file handle from a buffer
///
/// Returns number of bytes written, or negative error code
#[inline(always)]
pub fn write(handle: Handle, buf: &[u8]) -> isize {
    send(
        handle,
        OP_FILE_WRITE,
        buf.as_ptr() as usize,
        buf.len(),
        0,
        0,
    )
}

/// Seek within a file
///
/// Returns new position, or negative error code
#[inline(always)]
pub fn seek(handle: Handle, offset: i64, whence: u32) -> isize {
    send(handle, OP_FILE_SEEK, offset as usize, whence as usize, 0, 0)
}

/// Get file statistics
///
/// Returns 0 on success, or negative error code
#[inline(always)]
pub fn stat(handle: Handle, stat_buf: &mut FileStat) -> isize {
    send(
        handle,
        OP_FILE_STAT,
        stat_buf as *mut FileStat as usize,
        0,
        0,
        0,
    )
}

/// Close a file handle
///
/// Returns 0 on success, or negative error code
#[inline(always)]
pub fn close(handle: Handle) -> isize {
    send(handle, OP_FILE_CLOSE, 0, 0, 0, 0)
}

/// Read the next directory entry from a directory handle
///
/// Returns 1 if an entry was read, 0 if end of directory, or negative error code
#[inline(always)]
pub fn readdir(handle: Handle, entry: &mut panda_abi::DirEntry) -> isize {
    send(
        handle,
        OP_FILE_READDIR,
        entry as *mut panda_abi::DirEntry as usize,
        0,
        0,
        0,
    )
}
