//! File operation syscall handlers (OP_FILE_*).

use core::slice;

use panda_abi::*;

use crate::{scheduler, vfs};

use super::SyscallContext;

/// Handle file read operation.
pub fn handle_read(ctx: &SyscallContext, handle: u32, buf_ptr: usize, buf_len: usize) -> isize {
    let buf_ptr = buf_ptr as *mut u8;
    let result = scheduler::with_current_process(|proc| {
        if let Some(file) = proc.handles_mut().get_file_mut(handle) {
            let buf = unsafe { slice::from_raw_parts_mut(buf_ptr, buf_len) };
            file.read(buf)
        } else {
            Err(vfs::FsError::NotFound)
        }
    });
    match result {
        Ok(n) => n as isize,
        Err(vfs::FsError::WouldBlock(waker)) => {
            ctx.block_on(waker);
        }
        Err(_) => -1,
    }
}

/// Handle file write operation.
pub fn handle_write(handle: u32, buf_ptr: usize, buf_len: usize) -> isize {
    let buf_ptr = buf_ptr as *const u8;
    scheduler::with_current_process(|proc| {
        if let Some(file) = proc.handles_mut().get_file_mut(handle) {
            let buf = unsafe { slice::from_raw_parts(buf_ptr, buf_len) };
            match file.write(buf) {
                Ok(n) => n as isize,
                Err(_) => -1,
            }
        } else {
            -1
        }
    })
}

/// Handle file seek operation.
pub fn handle_seek(handle: u32, offset_lo: usize, offset_hi: usize) -> isize {
    let offset = ((offset_hi as u64) << 32 | offset_lo as u64) as i64;
    let whence = offset_hi;
    let seek_from = match whence {
        SEEK_SET => vfs::SeekFrom::Start(offset as u64),
        SEEK_CUR => vfs::SeekFrom::Current(offset),
        SEEK_END => vfs::SeekFrom::End(offset),
        _ => return -1,
    };
    scheduler::with_current_process(|proc| {
        if let Some(file) = proc.handles_mut().get_file_mut(handle) {
            match file.seek(seek_from) {
                Ok(pos) => pos as isize,
                Err(_) => -1,
            }
        } else {
            -1
        }
    })
}

/// Handle file stat operation.
pub fn handle_stat(handle: u32, stat_ptr: usize) -> isize {
    let stat_ptr = stat_ptr as *mut FileStat;
    scheduler::with_current_process(|proc| {
        if let Some(file) = proc.handles_mut().get_file_mut(handle) {
            let stat = file.stat();
            unsafe {
                (*stat_ptr).size = stat.size;
                (*stat_ptr).is_dir = stat.is_dir;
            }
            0
        } else {
            -1
        }
    })
}

/// Handle file close operation.
pub fn handle_close(handle: u32) -> isize {
    scheduler::with_current_process(|proc| {
        if proc.handles_mut().remove(handle).is_some() {
            0
        } else {
            -1
        }
    })
}

/// Handle directory read operation (read next entry from directory handle).
pub fn handle_readdir(handle: u32, entry_ptr: usize) -> isize {
    let entry_ptr = entry_ptr as *mut panda_abi::DirEntry;

    scheduler::with_current_process(|proc| {
        if let Some(dir) = proc.handles_mut().get_directory_mut(handle) {
            if let Some(entry) = dir.next() {
                let name_bytes = entry.name.as_bytes();
                let name_len = name_bytes.len().min(panda_abi::DIRENT_NAME_MAX);

                unsafe {
                    let out_entry = &mut *entry_ptr;
                    out_entry.name_len = name_len as u8;
                    out_entry.is_dir = entry.is_dir;
                    out_entry.name[..name_len].copy_from_slice(&name_bytes[..name_len]);
                }
                1 // Entry read
            } else {
                0 // End of directory
            }
        } else {
            -1 // Invalid handle
        }
    })
}
