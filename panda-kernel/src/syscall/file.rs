//! File operation syscall handlers (OP_FILE_*).

use core::slice;

use panda_abi::*;

use crate::scheduler;

use super::SyscallContext;

/// Handle file read operation.
pub fn handle_read(ctx: &SyscallContext, handle_id: u32, buf_ptr: usize, buf_len: usize) -> isize {
    let buf_ptr = buf_ptr as *mut u8;

    // First, try to read using the Block interface
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
            // For event sources, try to poll for an event
            if let Some(event) = event_source.poll() {
                let buf = unsafe { slice::from_raw_parts_mut(buf_ptr, buf_len) };
                // Serialize the event to the buffer
                let event_bytes = match event {
                    crate::resource::Event::Key(key) => {
                        // Match the InputEvent structure from virtio_keyboard
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
                // No event available - need to block
                None
            }
        } else {
            Some(Ok(0)) // Resource doesn't support reading
        }
    });

    match result {
        Some(Ok(n)) => n,
        Some(Err(_)) => -1,
        None => {
            // Need to block - get the waker
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
pub fn handle_write(handle_id: u32, buf_ptr: usize, buf_len: usize) -> isize {
    let buf_ptr = buf_ptr as *const u8;

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
            -1 // Resource doesn't support writing
        }
    })
}

/// Handle file seek operation.
pub fn handle_seek(handle_id: u32, offset_lo: usize, offset_hi: usize) -> isize {
    let offset = ((offset_hi as u64) << 32) | (offset_lo as u64);
    let whence = (offset_hi >> 32) as usize;

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
