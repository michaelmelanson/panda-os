//! File operation syscall handlers (OP_FILE_*).

use alloc::sync::Arc;
use core::slice;

use panda_abi::*;

use crate::process::ProcessId;
use crate::process::waker::Waker;
use crate::resource::{BlockDevice, BlockError};
use crate::scheduler;

use super::SyscallContext;

/// Result from attempting an async block I/O operation.
enum AsyncIoResult {
    /// I/O completed immediately or pending request completed.
    Completed(usize),
    /// Request submitted, need to block.
    WouldBlock(Arc<Waker>),
    /// No async support, fall back to sync.
    NotSupported,
    /// Error occurred.
    Error,
}

/// Try to perform an async block device read.
fn try_async_read(
    block_device: &dyn BlockDevice,
    offset: u64,
    buf: &mut [u8],
    process_id: ProcessId,
) -> AsyncIoResult {
    let sector_size = block_device.sector_size() as u64;

    // Require sector-aligned I/O for async path
    if offset % sector_size != 0 || buf.len() as u64 % sector_size != 0 {
        return AsyncIoResult::NotSupported;
    }

    let start_sector = offset / sector_size;

    // Check for completed pending request
    match block_device.complete_pending_read(process_id, buf) {
        Ok(Some(())) => return AsyncIoResult::Completed(buf.len()),
        Ok(None) => {}
        Err(_) => return AsyncIoResult::Error,
    }

    // Submit new async request
    let waker = Waker::new();
    match block_device.read_sectors_async(start_sector, buf, process_id, waker.clone()) {
        Ok(()) => AsyncIoResult::Completed(buf.len()),
        Err(BlockError::WouldBlock) => AsyncIoResult::WouldBlock(waker),
        Err(_) => AsyncIoResult::Error,
    }
}

/// Try to perform an async block device write.
fn try_async_write(
    block_device: &dyn BlockDevice,
    offset: u64,
    buf: &[u8],
    process_id: ProcessId,
) -> AsyncIoResult {
    let sector_size = block_device.sector_size() as u64;

    // Require sector-aligned I/O for async path
    if offset % sector_size != 0 || buf.len() as u64 % sector_size != 0 {
        return AsyncIoResult::NotSupported;
    }

    let start_sector = offset / sector_size;

    // Check for completed pending request
    match block_device.complete_pending_write(process_id) {
        Ok(Some(())) => return AsyncIoResult::Completed(buf.len()),
        Ok(None) => {}
        Err(_) => return AsyncIoResult::Error,
    }

    // Submit new async request
    let waker = Waker::new();
    match block_device.write_sectors_async(start_sector, buf, process_id, waker.clone()) {
        Ok(()) => AsyncIoResult::Completed(buf.len()),
        Err(BlockError::WouldBlock) => AsyncIoResult::WouldBlock(waker),
        Err(_) => AsyncIoResult::Error,
    }
}

/// Handle file read operation.
pub fn handle_read(ctx: &SyscallContext, handle_id: u32, buf_ptr: usize, buf_len: usize) -> isize {
    let buf_ptr = buf_ptr as *mut u8;
    let process_id = scheduler::current_process_id();

    // Try async block device I/O first
    let async_result = scheduler::with_current_process(|proc| {
        let handle = proc.handles_mut().get_mut(handle_id)?;
        let block_device = handle.as_block_device()?;

        if !block_device.supports_async() {
            return Some(AsyncIoResult::NotSupported);
        }

        let buf = unsafe { slice::from_raw_parts_mut(buf_ptr, buf_len) };
        let offset = handle.offset();
        let result = try_async_read(block_device, offset, buf, process_id);

        // Update offset on success
        if let AsyncIoResult::Completed(n) = &result {
            handle.set_offset(offset + *n as u64);
        }
        Some(result)
    });

    // Handle async result
    if let Some(result) = async_result {
        match result {
            AsyncIoResult::Completed(n) => return n as isize,
            AsyncIoResult::WouldBlock(waker) => ctx.block_on(waker),
            AsyncIoResult::Error => return -1,
            AsyncIoResult::NotSupported => {}
        }
    }

    // Sync path
    let result = scheduler::with_current_process(|proc| {
        let handle = proc.handles_mut().get_mut(handle_id)?;

        if let Some(block) = handle.as_block() {
            let buf = unsafe { slice::from_raw_parts_mut(buf_ptr, buf_len) };
            let offset = handle.offset();
            match block.read_at(offset, buf) {
                Ok(n) => {
                    handle.set_offset(offset + n as u64);
                    Some(Ok(n as isize))
                }
                Err(e) => Some(Err(e)),
            }
        } else if let Some(event_source) = handle.as_event_source() {
            if let Some(event) = event_source.poll() {
                let buf = unsafe { slice::from_raw_parts_mut(buf_ptr, buf_len) };
                let event_bytes = match event {
                    crate::resource::Event::Key(key) => {
                        // struct InputEvent { event_type: u16, code: u16, value: u32 }
                        let mut bytes = [0u8; 8];
                        bytes[0..2].copy_from_slice(&0x01u16.to_ne_bytes()); // EV_KEY
                        bytes[2..4].copy_from_slice(&key.code.to_ne_bytes());
                        bytes[4..8].copy_from_slice(&key.value.to_ne_bytes());
                        bytes
                    }
                    _ => return Some(Ok(0)),
                };
                let n = event_bytes.len().min(buf.len());
                buf[..n].copy_from_slice(&event_bytes[..n]);
                Some(Ok(n as isize))
            } else {
                None // No event available - need to block
            }
        } else {
            Some(Ok(0))
        }
    });

    match result {
        Some(Ok(n)) => n,
        Some(Err(_)) => -1,
        None => {
            let waker = scheduler::with_current_process(|proc| {
                proc.handles().get(handle_id).and_then(|h| h.waker())
            });
            if let Some(waker) = waker {
                ctx.block_on(waker);
            } else {
                -1
            }
        }
    }
}

/// Handle file write operation.
pub fn handle_write(ctx: &SyscallContext, handle_id: u32, buf_ptr: usize, buf_len: usize) -> isize {
    let buf_ptr = buf_ptr as *const u8;
    let process_id = scheduler::current_process_id();

    // Try async block device I/O first
    let async_result = scheduler::with_current_process(|proc| {
        let handle = proc.handles_mut().get_mut(handle_id)?;
        let block_device = handle.as_block_device()?;

        if !block_device.supports_async() {
            return Some(AsyncIoResult::NotSupported);
        }

        let buf = unsafe { slice::from_raw_parts(buf_ptr, buf_len) };
        let offset = handle.offset();
        let result = try_async_write(block_device, offset, buf, process_id);

        // Update offset on success
        if let AsyncIoResult::Completed(n) = &result {
            handle.set_offset(offset + *n as u64);
        }
        Some(result)
    });

    // Handle async result
    if let Some(result) = async_result {
        match result {
            AsyncIoResult::Completed(n) => return n as isize,
            AsyncIoResult::WouldBlock(waker) => ctx.block_on(waker),
            AsyncIoResult::Error => return -1,
            AsyncIoResult::NotSupported => {}
        }
    }

    // Sync path
    scheduler::with_current_process(|proc| {
        let Some(handle) = proc.handles_mut().get_mut(handle_id) else {
            return -1;
        };

        let buf = unsafe { slice::from_raw_parts(buf_ptr, buf_len) };

        if let Some(block) = handle.as_block() {
            let offset = handle.offset();
            match block.write_at(offset, buf) {
                Ok(n) => {
                    handle.set_offset(offset + n as u64);
                    n as isize
                }
                Err(_) => -1,
            }
        } else if let Some(char_out) = handle.as_char_output() {
            match char_out.write(buf) {
                Ok(n) => n as isize,
                Err(_) => -1,
            }
        } else {
            -1
        }
    })
}

/// Handle file seek operation.
pub fn handle_seek(handle_id: u32, offset_lo: usize, offset_hi: usize) -> isize {
    let offset = ((offset_hi as u64) << 32) | (offset_lo as u64);
    let whence = (offset_hi >> 32) as u32;

    scheduler::with_current_process(|proc| {
        let Some(handle) = proc.handles_mut().get_mut(handle_id) else {
            return -1;
        };

        // Only block resources support seeking
        let Some(block) = handle.as_block() else {
            return -1;
        };

        let size = block.size();
        let current = handle.offset();

        let new_offset = match whence {
            SEEK_SET => offset as i64,
            SEEK_CUR => current as i64 + offset as i64,
            SEEK_END => size as i64 + offset as i64,
            _ => return -1,
        };

        if new_offset < 0 {
            return -1;
        }

        handle.set_offset(new_offset as u64);
        new_offset as isize
    })
}

/// Handle file stat operation.
pub fn handle_stat(handle_id: u32, stat_ptr: usize) -> isize {
    let stat_ptr = stat_ptr as *mut FileStat;

    scheduler::with_current_process(|proc| {
        let Some(handle) = proc.handles().get(handle_id) else {
            return -1;
        };

        if let Some(block) = handle.as_block() {
            unsafe {
                (*stat_ptr).size = block.size();
                (*stat_ptr).is_dir = false;
            }
            0
        } else if handle.as_directory().is_some() {
            unsafe {
                (*stat_ptr).size = 0;
                (*stat_ptr).is_dir = true;
            }
            0
        } else {
            // For other resource types, return minimal info
            unsafe {
                (*stat_ptr).size = 0;
                (*stat_ptr).is_dir = false;
            }
            0
        }
    })
}

/// Handle file close operation.
pub fn handle_close(handle_id: u32) -> isize {
    scheduler::with_current_process(|proc| {
        if proc.handles_mut().remove(handle_id).is_some() {
            0
        } else {
            -1
        }
    })
}

/// Handle directory read operation (read next entry from directory handle).
///
/// The directory handle maintains a cursor that advances with each call.
/// The cursor is stored in the handle's offset field.
pub fn handle_readdir(handle_id: u32, entry_ptr: usize) -> isize {
    let entry_ptr = entry_ptr as *mut panda_abi::DirEntry;

    scheduler::with_current_process(|proc| {
        let Some(handle) = proc.handles_mut().get_mut(handle_id) else {
            return -1;
        };

        let Some(directory) = handle.as_directory() else {
            return -1;
        };

        // Use the handle's offset as the cursor
        let cursor = handle.offset() as usize;

        if let Some(entry) = directory.entry(cursor) {
            let name_bytes = entry.name.as_bytes();
            let name_len = name_bytes.len().min(panda_abi::DIRENT_NAME_MAX);

            unsafe {
                let out_entry = &mut *entry_ptr;
                out_entry.name_len = name_len as u8;
                out_entry.is_dir = entry.is_dir;
                out_entry.name[..name_len].copy_from_slice(&name_bytes[..name_len]);
            }

            // Advance the cursor
            handle.set_offset((cursor + 1) as u64);
            1 // Entry read
        } else {
            0 // End of directory
        }
    })
}
