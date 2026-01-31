//! File operation syscall handlers (OP_FILE_*).

#![deny(unsafe_code)]

use alloc::boxed::Box;
use alloc::sync::Arc;
use alloc::vec;
use core::task::Poll;

use panda_abi::*;

use crate::resource::VfsFile;
use crate::scheduler;
use crate::vfs::SeekFrom;

use super::VfsFileWrapper;
use super::poll_fn;
use super::user_ptr::{SyscallFuture, SyscallResult, UserAccess, UserSlice};

/// Handle file read operation.
///
/// For VFS files, this is async and may yield to the scheduler if I/O is needed.
/// The `flags` parameter can include `FILE_NONBLOCK` to return immediately if no data.
pub fn handle_read(
    _ua: &UserAccess,
    handle_id: u64,
    buf_ptr: usize,
    buf_len: usize,
    flags: u32,
) -> SyscallFuture {
    let dst = UserSlice::new(buf_ptr, buf_len);

    // First, check if this is a VFS file (which needs async handling)
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
        handle_read_vfs(handle_id, buf_len, dst, vfs_file)
    } else {
        // Sync path for non-VFS resources (blocks, event sources, etc.)
        handle_read_sync(handle_id, dst, flags)
    }
}

/// Async read path for VFS files.
///
/// Reads into a kernel bounce buffer, then returns a WriteBack for the top-level
/// to copy out to userspace.
fn handle_read_vfs(
    handle_id: u64,
    buf_len: usize,
    dst: UserSlice,
    vfs_file: Arc<dyn VfsFile>,
) -> SyscallFuture {
    // Get the current offset
    let offset = scheduler::with_current_process(|proc| {
        proc.handles()
            .get(handle_id)
            .map(|h| h.offset())
            .unwrap_or(0)
    });

    Box::pin(async move {
        let file_lock = vfs_file.file();
        let mut file = file_lock.lock();

        // Seek to current offset
        let seek_result: Result<u64, crate::vfs::FsError> =
            file.seek(SeekFrom::Start(offset)).await;
        if seek_result.is_err() {
            return SyscallResult::err(-1);
        }

        // Read into kernel bounce buffer (capped to prevent kernel heap exhaustion)
        let buf_len = buf_len.min(panda_abi::MAX_FILE_IO_SIZE);
        let mut kernel_buf = vec![0u8; buf_len];
        match file.read(&mut kernel_buf).await {
            Ok(n) => {
                // Update handle offset
                scheduler::with_current_process(|proc| {
                    if let Some(handle) = proc.handles_mut().get_mut(handle_id) {
                        handle.set_offset(offset + n as u64);
                    }
                });
                kernel_buf.truncate(n);
                SyscallResult::write_back(n as isize, kernel_buf, dst)
            }
            Err(_) => SyscallResult::err(-1),
        }
    })
}

/// Synchronous read path for non-VFS resources (event sources, etc.).
///
/// If `flags` includes `FILE_NONBLOCK`, returns 0 immediately when no data is available
/// instead of blocking.
fn handle_read_sync(handle_id: u64, dst: UserSlice, flags: u32) -> SyscallFuture {
    // Try to read immediately
    let immediate_result: Option<Option<(isize, alloc::vec::Vec<u8>)>> =
        scheduler::with_current_process(|proc| {
            let handle = proc.handles_mut().get_mut(handle_id)?;

            if let Some(event_source) = handle.as_event_source() {
                if let Some(event) = event_source.poll() {
                    let event_bytes = match event {
                        crate::resource::Event::Key(key) => {
                            // struct InputEvent { event_type: u16, code: u16, value: u32 }
                            let mut bytes = [0u8; 8];
                            bytes[0..2].copy_from_slice(&0x01u16.to_ne_bytes()); // EV_KEY
                            bytes[2..4].copy_from_slice(&key.code.to_ne_bytes());
                            bytes[4..8].copy_from_slice(&key.value.to_ne_bytes());
                            bytes.to_vec()
                        }
                        _ => return Some(Some((0, alloc::vec::Vec::new()))),
                    };
                    let n = event_bytes.len().min(dst.len());
                    Some(Some((n as isize, event_bytes[..n].to_vec())))
                } else {
                    Some(None) // No event available
                }
            } else {
                Some(Some((0, alloc::vec::Vec::new())))
            }
        });

    match immediate_result {
        Some(Some((n, data))) => {
            if data.is_empty() {
                Box::pin(core::future::ready(SyscallResult::ok(n)))
            } else {
                Box::pin(core::future::ready(SyscallResult::write_back(n, data, dst)))
            }
        }
        Some(None) => {
            // No data available
            if flags & FILE_NONBLOCK != 0 {
                return Box::pin(core::future::ready(SyscallResult::ok(0)));
            }

            // Need to block — use poll_fn to retry on each wake
            let resource = scheduler::with_current_process(|proc| {
                proc.handles().get(handle_id).map(|h| h.resource_arc())
            });
            let waker = scheduler::with_current_process(|proc| {
                proc.handles().get(handle_id).and_then(|h| h.waker())
            });

            Box::pin(poll_fn(move |_cx| {
                let Some(ref resource) = resource else {
                    return Poll::Ready(SyscallResult::err(-1));
                };
                let Some(event_source) = resource.as_event_source() else {
                    return Poll::Ready(SyscallResult::err(-1));
                };

                if let Some(event) = event_source.poll() {
                    let event_bytes = match event {
                        crate::resource::Event::Key(key) => {
                            let mut bytes = [0u8; 8];
                            bytes[0..2].copy_from_slice(&0x01u16.to_ne_bytes());
                            bytes[2..4].copy_from_slice(&key.code.to_ne_bytes());
                            bytes[4..8].copy_from_slice(&key.value.to_ne_bytes());
                            bytes.to_vec()
                        }
                        _ => return Poll::Ready(SyscallResult::ok(0)),
                    };
                    let n = event_bytes.len().min(dst.len());
                    let data = event_bytes[..n].to_vec();
                    Poll::Ready(SyscallResult::write_back(n as isize, data, dst))
                } else {
                    // Re-register waker
                    if let Some(ref waker) = waker {
                        waker.set_waiting(scheduler::current_process_id());
                    }
                    Poll::Pending
                }
            }))
        }
        None => {
            // Invalid handle
            Box::pin(core::future::ready(SyscallResult::err(-1)))
        }
    }
}

/// Handle file write operation.
///
/// For VFS files, this is async and may yield to the scheduler if I/O is needed.
pub fn handle_write(
    ua: &UserAccess,
    handle_id: u64,
    buf_ptr: usize,
    buf_len: usize,
) -> SyscallFuture {
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
        handle_write_vfs(ua, handle_id, buf_ptr, buf_len, vfs_file)
    } else {
        // Sync path for non-VFS resources (char output, etc.)
        handle_write_sync(ua, handle_id, buf_ptr, buf_len)
    }
}

/// Async write path for VFS files.
///
/// Copies data from userspace into a kernel buffer before building the future.
fn handle_write_vfs(
    ua: &UserAccess,
    handle_id: u64,
    buf_ptr: usize,
    buf_len: usize,
    vfs_file: Arc<dyn VfsFile>,
) -> SyscallFuture {
    let offset = scheduler::with_current_process(|proc| {
        proc.handles()
            .get(handle_id)
            .map(|h| h.offset())
            .unwrap_or(0)
    });

    // Cap write size to prevent kernel heap exhaustion
    let buf_len = buf_len.min(panda_abi::MAX_FILE_IO_SIZE);

    // Copy data from userspace into kernel buffer before building future
    let data = match ua.read(UserSlice::new(buf_ptr, buf_len)) {
        Ok(d) => d,
        Err(_) => return Box::pin(core::future::ready(SyscallResult::err(-1))),
    };

    Box::pin(async move {
        let file_lock = vfs_file.file();
        let mut file = file_lock.lock();

        // Seek to current offset
        if file.seek(SeekFrom::Start(offset)).await.is_err() {
            return SyscallResult::err(-1);
        }

        // Write data from kernel buffer
        match file.write(&data).await {
            Ok(n) => {
                scheduler::with_current_process(|proc| {
                    if let Some(handle) = proc.handles_mut().get_mut(handle_id) {
                        handle.set_offset(offset + n as u64);
                    }
                });
                SyscallResult::ok(n as isize)
            }
            Err(_) => SyscallResult::err(-1),
        }
    })
}

/// Synchronous write path for non-VFS resources.
fn handle_write_sync(
    ua: &UserAccess,
    handle_id: u64,
    buf_ptr: usize,
    buf_len: usize,
) -> SyscallFuture {
    // Cap write size to prevent kernel heap exhaustion
    let buf_len = buf_len.min(panda_abi::MAX_FILE_IO_SIZE);

    // Copy data from userspace
    let data = match ua.read(UserSlice::new(buf_ptr, buf_len)) {
        Ok(d) => d,
        Err(_) => return Box::pin(core::future::ready(SyscallResult::err(-1))),
    };

    let result = scheduler::with_current_process(|proc| {
        let Some(handle) = proc.handles_mut().get_mut(handle_id) else {
            return -1;
        };

        if let Some(char_out) = handle.as_char_output() {
            match char_out.write(&data) {
                Ok(n) => n as isize,
                Err(_) => -1,
            }
        } else {
            -1
        }
    });

    Box::pin(core::future::ready(SyscallResult::ok(result)))
}

/// Handle file seek operation.
///
/// For VFS files, this updates the handle offset and performs an async stat to get size.
pub fn handle_seek(handle_id: u64, offset_lo: usize, offset_hi: usize) -> SyscallFuture {
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
        return Box::pin(core::future::ready(SyscallResult::err(-1)));
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
            return Box::pin(core::future::ready(SyscallResult::err(-1)));
        }
        scheduler::with_current_process(|proc| {
            if let Some(handle) = proc.handles_mut().get_mut(handle_id) {
                handle.set_offset(new_offset as u64);
            }
        });
        return Box::pin(core::future::ready(SyscallResult::ok(new_offset as isize)));
    }

    if whence == SEEK_CUR {
        let new_offset = current as i64 + offset as i64;
        if new_offset < 0 {
            return Box::pin(core::future::ready(SyscallResult::err(-1)));
        }
        scheduler::with_current_process(|proc| {
            if let Some(handle) = proc.handles_mut().get_mut(handle_id) {
                handle.set_offset(new_offset as u64);
            }
        });
        return Box::pin(core::future::ready(SyscallResult::ok(new_offset as isize)));
    }

    if whence == SEEK_END {
        // Need to get file size via async stat
        Box::pin(async move {
            let file_lock = vfs_file.file();
            let file = file_lock.lock();
            let stat = file.stat().await;
            drop(file);

            match stat {
                Ok(s) => {
                    let new_offset = s.size as i64 + offset as i64;
                    if new_offset < 0 {
                        return SyscallResult::err(-1);
                    }
                    scheduler::with_current_process(|proc| {
                        if let Some(handle) = proc.handles_mut().get_mut(handle_id) {
                            handle.set_offset(new_offset as u64);
                        }
                    });
                    SyscallResult::ok(new_offset as isize)
                }
                Err(_) => SyscallResult::err(-1),
            }
        })
    } else {
        Box::pin(core::future::ready(SyscallResult::err(-1)))
    }
}

/// Handle file stat operation.
///
/// For VFS files, this performs an async stat operation.
/// The result is written back to userspace via WriteBack.
pub fn handle_stat(handle_id: u64, stat_ptr: usize) -> SyscallFuture {
    let dst = UserSlice::new(stat_ptr, core::mem::size_of::<FileStat>());

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
        Box::pin(async move {
            let file_lock = vfs_file.file();
            let file = file_lock.lock();
            let stat = file.stat().await;
            drop(file);

            match stat {
                Ok(s) => {
                    let file_stat = FileStat {
                        size: s.size,
                        is_dir: s.is_dir,
                    };
                    SyscallResult::write_back_struct(0, &file_stat, dst)
                }
                Err(_) => SyscallResult::err(-1),
            }
        })
    } else {
        // Sync path for non-VFS resources
        let result = scheduler::with_current_process(|proc| {
            let Some(handle) = proc.handles().get(handle_id) else {
                return Err(());
            };

            let file_stat = if handle.as_directory().is_some() {
                FileStat {
                    size: 0,
                    is_dir: true,
                }
            } else {
                FileStat {
                    size: 0,
                    is_dir: false,
                }
            };
            Ok(file_stat)
        });

        match result {
            Ok(file_stat) => Box::pin(core::future::ready(SyscallResult::write_back_struct(
                0, &file_stat, dst,
            ))),
            Err(()) => Box::pin(core::future::ready(SyscallResult::err(-1))),
        }
    }
}

/// Handle file close operation.
pub fn handle_close(handle_id: u64) -> SyscallFuture {
    let result = scheduler::with_current_process(|proc| {
        if proc.handles_mut().remove(handle_id).is_some() {
            0
        } else {
            -1
        }
    });
    Box::pin(core::future::ready(SyscallResult::ok(result)))
}

/// Handle directory read operation (read next entry from directory handle).
///
/// The directory handle maintains a cursor that advances with each call.
/// The cursor is stored in the handle's offset field.
pub fn handle_readdir(ua: &UserAccess, handle_id: u64, entry_ptr: usize) -> SyscallFuture {
    let result = scheduler::with_current_process(|proc| {
        let Some(handle) = proc.handles_mut().get_mut(handle_id) else {
            return Err(());
        };

        let Some(directory) = handle.as_directory() else {
            return Err(());
        };

        // Use the handle's offset as the cursor
        let cursor = handle.offset() as usize;

        if let Some(entry) = directory.entry(cursor) {
            let name_bytes = entry.name.as_bytes();
            let name_len = name_bytes.len().min(panda_abi::DIRENT_NAME_MAX);

            let mut dir_entry = panda_abi::DirEntry {
                name_len: name_len as u8,
                is_dir: entry.is_dir,
                name: [0u8; panda_abi::DIRENT_NAME_MAX],
            };
            dir_entry.name[..name_len].copy_from_slice(&name_bytes[..name_len]);

            // Advance the cursor
            handle.set_offset((cursor + 1) as u64);
            Ok(Some(dir_entry))
        } else {
            Ok(None) // End of directory
        }
    });

    match result {
        Ok(Some(dir_entry)) => {
            // Write entry to userspace via validated pointer
            match ua.write_struct(entry_ptr, &dir_entry) {
                Ok(_) => Box::pin(core::future::ready(SyscallResult::ok(1))),
                Err(_) => Box::pin(core::future::ready(SyscallResult::err(-1))),
            }
        }
        Ok(None) => Box::pin(core::future::ready(SyscallResult::ok(0))),
        Err(()) => Box::pin(core::future::ready(SyscallResult::err(-1))),
    }
}
