//! File operation syscall handlers (OP_FILE_*).

use alloc::boxed::Box;
use alloc::sync::Arc;
use core::slice;

use panda_abi::*;

use crate::process::PendingSyscall;
use crate::resource::VfsFile;
use crate::scheduler;
use crate::vfs::SeekFrom;

use super::SyscallContext;

/// Handle file read operation.
///
/// For VFS files, this is async and may yield to the scheduler if I/O is needed.
/// The `flags` parameter can include `FILE_NONBLOCK` to return immediately if no data.
pub fn handle_read(
    ctx: &SyscallContext,
    handle_id: u32,
    buf_ptr: usize,
    buf_len: usize,
    flags: u32,
) -> isize {
    // First, check if this is a VFS file (which needs async handling)
    let vfs_file: Option<Arc<dyn VfsFile>> = scheduler::with_current_process(|proc| {
        proc.handles()
            .get(handle_id)
            .and_then(|h| h.as_vfs_file())
            .map(|_| {
                // Get the resource Arc so we can use it in the async block
                proc.handles().get(handle_id).unwrap().resource_arc()
            })
    })
    .and_then(|res| {
        // Try to downcast to a VfsFile-implementing resource
        // We need to extract VfsFile from the resource
        if res.as_vfs_file().is_some() {
            // Create a wrapper that holds the Arc
            Some(Arc::new(VfsFileWrapper(res)) as Arc<dyn VfsFile>)
        } else {
            None
        }
    });

    if let Some(vfs_file) = vfs_file {
        handle_read_vfs(ctx, handle_id, buf_ptr, buf_len, vfs_file)
    } else {
        // Sync path for non-VFS resources (blocks, event sources, etc.)
        handle_read_sync(ctx, handle_id, buf_ptr as *mut u8, buf_len, flags)
    }
}

/// Async read path for VFS files.
fn handle_read_vfs(
    ctx: &SyscallContext,
    handle_id: u32,
    buf_ptr: usize,
    buf_len: usize,
    vfs_file: Arc<dyn VfsFile>,
) -> isize {
    // Get the current offset
    let offset = scheduler::with_current_process(|proc| {
        proc.handles()
            .get(handle_id)
            .map(|h| h.offset())
            .unwrap_or(0)
    });

    // Keep buf_ptr as usize (which is Send) rather than converting to *mut u8
    // We'll convert to a pointer only when we need to use it inside the async block

    // Async path for VFS files
    let future = Box::pin(async move {
        let file_lock = vfs_file.file();
        let mut file = file_lock.lock();

        // Seek to current offset
        let seek_result: Result<u64, crate::vfs::FsError> =
            file.seek(SeekFrom::Start(offset)).await;
        if seek_result.is_err() {
            return -1isize;
        }

        // Read data - convert usize back to pointer inside the async block
        let buf = unsafe { slice::from_raw_parts_mut(buf_ptr as *mut u8, buf_len) };
        match file.read(buf).await {
            Ok(n) => {
                // Update handle offset
                scheduler::with_current_process(|proc| {
                    if let Some(handle) = proc.handles_mut().get_mut(handle_id) {
                        handle.set_offset(offset + n as u64);
                    }
                });
                n as isize
            }
            Err(_) => -1,
        }
    });

    scheduler::with_current_process(|proc| {
        proc.set_pending_syscall(PendingSyscall::new(future));
    });

    ctx.yield_for_async()
}

/// Wrapper to allow holding an Arc<dyn Resource> as Arc<dyn VfsFile>
struct VfsFileWrapper(Arc<dyn crate::resource::Resource>);

impl VfsFile for VfsFileWrapper {
    fn file(&self) -> &spinning_top::Spinlock<Box<dyn crate::vfs::File>> {
        self.0.as_vfs_file().unwrap().file()
    }
}

// Safety: VfsFileWrapper just holds an Arc which is Send+Sync
unsafe impl Send for VfsFileWrapper {}
unsafe impl Sync for VfsFileWrapper {}

/// Synchronous read path for non-VFS resources (event sources, etc.).
///
/// If `flags` includes `FILE_NONBLOCK`, returns 0 immediately when no data is available
/// instead of blocking.
fn handle_read_sync(
    ctx: &SyscallContext,
    handle_id: u32,
    buf_ptr: *mut u8,
    buf_len: usize,
    flags: u32,
) -> isize {
    let result: Option<isize> = scheduler::with_current_process(|proc| {
        let handle = proc.handles_mut().get_mut(handle_id)?;

        if let Some(event_source) = handle.as_event_source() {
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
                    _ => return Some(0),
                };
                let n = event_bytes.len().min(buf.len());
                buf[..n].copy_from_slice(&event_bytes[..n]);
                Some(n as isize)
            } else {
                None // No event available - need to block (or return 0 if non-blocking)
            }
        } else {
            Some(0)
        }
    });

    match result {
        Some(n) => n,
        None => {
            // No data available
            if flags & FILE_NONBLOCK != 0 {
                // Non-blocking: return 0 to indicate no data
                return 0;
            }

            // Blocking: wait for data
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
///
/// For VFS files, this is async and may yield to the scheduler if I/O is needed.
pub fn handle_write(ctx: &SyscallContext, handle_id: u32, buf_ptr: usize, buf_len: usize) -> isize {
    // Check if this is a VFS file (which needs async handling)
    let vfs_file: Option<Arc<dyn VfsFile>> = scheduler::with_current_process(|proc| {
        proc.handles()
            .get(handle_id)
            .and_then(|h| h.as_vfs_file())
            .map(|_| proc.handles().get(handle_id).unwrap().resource_arc())
    })
    .and_then(|res| {
        if res.as_vfs_file().is_some() {
            Some(Arc::new(VfsFileWrapper(res)) as Arc<dyn VfsFile>)
        } else {
            None
        }
    });

    if let Some(vfs_file) = vfs_file {
        handle_write_vfs(ctx, handle_id, buf_ptr, buf_len, vfs_file)
    } else {
        // Sync path for non-VFS resources (char output, etc.)
        handle_write_sync(handle_id, buf_ptr as *const u8, buf_len)
    }
}

/// Async write path for VFS files.
fn handle_write_vfs(
    ctx: &SyscallContext,
    handle_id: u32,
    buf_ptr: usize,
    buf_len: usize,
    vfs_file: Arc<dyn VfsFile>,
) -> isize {
    let offset = scheduler::with_current_process(|proc| {
        proc.handles()
            .get(handle_id)
            .map(|h| h.offset())
            .unwrap_or(0)
    });

    let future = Box::pin(async move {
        let file_lock = vfs_file.file();
        let mut file = file_lock.lock();

        // Seek to current offset
        if file.seek(SeekFrom::Start(offset)).await.is_err() {
            return -1isize;
        }

        // Write data
        let buf = unsafe { slice::from_raw_parts(buf_ptr as *const u8, buf_len) };
        match file.write(buf).await {
            Ok(n) => {
                scheduler::with_current_process(|proc| {
                    if let Some(handle) = proc.handles_mut().get_mut(handle_id) {
                        handle.set_offset(offset + n as u64);
                    }
                });
                n as isize
            }
            Err(_) => -1,
        }
    });

    scheduler::with_current_process(|proc| {
        proc.set_pending_syscall(PendingSyscall::new(future));
    });

    ctx.yield_for_async()
}

/// Synchronous write path for non-VFS resources.
fn handle_write_sync(handle_id: u32, buf_ptr: *const u8, buf_len: usize) -> isize {
    scheduler::with_current_process(|proc| {
        let Some(handle) = proc.handles_mut().get_mut(handle_id) else {
            return -1;
        };

        let buf = unsafe { slice::from_raw_parts(buf_ptr, buf_len) };

        if let Some(char_out) = handle.as_char_output() {
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
///
/// For VFS files, this updates the handle offset and performs an async stat to get size.
/// For simplicity, we manage the offset in the handle rather than seeking in the file.
pub fn handle_seek(
    ctx: &SyscallContext,
    handle_id: u32,
    offset_lo: usize,
    offset_hi: usize,
) -> isize {
    let offset = ((offset_hi as u64) << 32) | (offset_lo as u64);
    let whence = (offset_hi >> 32) as u32;

    // Check if this is a VFS file
    let vfs_file: Option<Arc<dyn VfsFile>> = scheduler::with_current_process(|proc| {
        proc.handles()
            .get(handle_id)
            .and_then(|h| h.as_vfs_file())
            .map(|_| proc.handles().get(handle_id).unwrap().resource_arc())
    })
    .and_then(|res| {
        if res.as_vfs_file().is_some() {
            Some(Arc::new(VfsFileWrapper(res)) as Arc<dyn VfsFile>)
        } else {
            None
        }
    });

    let Some(vfs_file) = vfs_file else {
        return -1; // Only VFS files support seeking
    };

    // Get current offset
    let current = scheduler::with_current_process(|proc| {
        proc.handles()
            .get(handle_id)
            .map(|h| h.offset())
            .unwrap_or(0)
    });

    // For SEEK_SET and SEEK_CUR, we can compute the new offset synchronously
    if whence == SEEK_SET {
        let new_offset = offset as i64;
        if new_offset < 0 {
            return -1;
        }
        scheduler::with_current_process(|proc| {
            if let Some(handle) = proc.handles_mut().get_mut(handle_id) {
                handle.set_offset(new_offset as u64);
            }
        });
        return new_offset as isize;
    }

    if whence == SEEK_CUR {
        let new_offset = current as i64 + offset as i64;
        if new_offset < 0 {
            return -1;
        }
        scheduler::with_current_process(|proc| {
            if let Some(handle) = proc.handles_mut().get_mut(handle_id) {
                handle.set_offset(new_offset as u64);
            }
        });
        return new_offset as isize;
    }

    if whence == SEEK_END {
        // Need to get file size via async stat
        let future = Box::pin(async move {
            let file_lock = vfs_file.file();
            let file = file_lock.lock();
            let stat = file.stat().await;
            drop(file);

            match stat {
                Ok(s) => {
                    let new_offset = s.size as i64 + offset as i64;
                    if new_offset < 0 {
                        return -1isize;
                    }
                    scheduler::with_current_process(|proc| {
                        if let Some(handle) = proc.handles_mut().get_mut(handle_id) {
                            handle.set_offset(new_offset as u64);
                        }
                    });
                    new_offset as isize
                }
                Err(_) => -1,
            }
        });

        scheduler::with_current_process(|proc| {
            proc.set_pending_syscall(PendingSyscall::new(future));
        });

        ctx.yield_for_async()
    } else {
        -1 // Invalid whence
    }
}

/// Handle file stat operation.
///
/// For VFS files, this performs an async stat operation.
pub fn handle_stat(ctx: &SyscallContext, handle_id: u32, stat_ptr: usize) -> isize {
    // Check if this is a VFS file
    let vfs_file: Option<Arc<dyn VfsFile>> = scheduler::with_current_process(|proc| {
        proc.handles()
            .get(handle_id)
            .and_then(|h| h.as_vfs_file())
            .map(|_| proc.handles().get(handle_id).unwrap().resource_arc())
    })
    .and_then(|res| {
        if res.as_vfs_file().is_some() {
            Some(Arc::new(VfsFileWrapper(res)) as Arc<dyn VfsFile>)
        } else {
            None
        }
    });

    if let Some(vfs_file) = vfs_file {
        // Async stat for VFS files
        let future = Box::pin(async move {
            let file_lock = vfs_file.file();
            let file = file_lock.lock();
            let stat = file.stat().await;
            drop(file);

            match stat {
                Ok(s) => {
                    let stat_ptr = stat_ptr as *mut FileStat;
                    unsafe {
                        (*stat_ptr).size = s.size;
                        (*stat_ptr).is_dir = s.is_dir;
                    }
                    0isize
                }
                Err(_) => -1,
            }
        });

        scheduler::with_current_process(|proc| {
            proc.set_pending_syscall(PendingSyscall::new(future));
        });

        ctx.yield_for_async()
    } else {
        // Sync path for non-VFS resources
        let stat_ptr = stat_ptr as *mut FileStat;
        scheduler::with_current_process(|proc| {
            let Some(handle) = proc.handles().get(handle_id) else {
                return -1;
            };

            if handle.as_directory().is_some() {
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
