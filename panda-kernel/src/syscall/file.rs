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
use super::user_ptr::{SyscallFuture, SyscallResult, UserAccess, UserPtr, UserSlice};

/// Resolve `handle_id` to a VFS file, if it is one.
///
/// A single handle-table lookup: checks `as_vfs_file()` and, if it matches, clones
/// the resource `Arc` and wraps it in `VfsFileWrapper` so it can be held as
/// `Arc<dyn VfsFile>` outside the process lock.
fn get_vfs_file(handle_id: u64) -> Option<Arc<dyn VfsFile>> {
    scheduler::with_current_process(|proc| {
        let handle = proc.handles().get(handle_id)?;
        if handle.as_vfs_file().is_some() {
            Some(Arc::new(VfsFileWrapper(handle.resource_arc())) as Arc<dyn VfsFile>)
        } else {
            None
        }
    })
}

/// Resolve `handle_id` to a userspace scheme provider's proxy resource
/// (M2.2), if it is one. A scheme provider's resources are not VFS-backed
/// (see `resource::SchemeProxyResource`'s doc comment), so this is a
/// separate resolution path from `get_vfs_file` rather than being forced
/// through it.
fn get_scheme_proxy(handle_id: u64) -> Option<Arc<dyn crate::resource::Resource>> {
    scheduler::with_current_process(|proc| {
        let handle = proc.handles().get(handle_id)?;
        if handle.as_scheme_proxy().is_some() {
            Some(handle.resource_arc())
        } else {
            None
        }
    })
}

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

    // Scheme-provider proxy resources round-trip to a userspace process
    // instead of going through the VFS or the sync event-source path.
    if let Some(resource) = get_scheme_proxy(handle_id) {
        let offset = scheduler::with_current_process(|proc| {
            proc.handles().get(handle_id).map(|h| h.offset()).unwrap_or(0)
        });
        return handle_read_scheme_proxy(
            handle_id,
            offset,
            buf_len,
            dst,
            resource,
            flags & panda_abi::FILE_NONBLOCK != 0,
        );
    }

    // First, check if this is a VFS file (which needs async handling)
    if let Some(vfs_file) = get_vfs_file(handle_id) {
        handle_read_vfs(handle_id, buf_len, dst, vfs_file)
    } else {
        // Sync path for non-VFS resources (blocks, event sources, etc.)
        handle_read_sync(handle_id, dst, flags)
    }
}

/// Read path for scheme-provider proxy resources (M2.2): round-trips a
/// `Read` request to the provider process and writes back whatever it
/// returns. `buf_len` is capped to `MAX_TRANSFER_SIZE` so the response is
/// guaranteed to fit in one channel message.
///
/// `offset` is the handle's current file position (see `handle.offset()`),
/// forwarded as `Request::Read`'s `offset` field so random-access providers
/// (`block:`) see the position `OP_FILE_SEEK` last set — stream-like
/// providers (e.g. `keyboard:`) are free to ignore it. On success, the
/// handle's offset is advanced by the number of bytes actually read, same
/// as `handle_read_vfs` — this is what gives `block_test`'s
/// seek/write/seek/read sequence real random-access semantics over a
/// request/response protocol.
fn handle_read_scheme_proxy(
    handle_id: u64,
    offset: u64,
    buf_len: usize,
    dst: UserSlice,
    resource: Arc<dyn crate::resource::Resource>,
    nonblocking: bool,
) -> SyscallFuture {
    let buf_len = buf_len.min(panda_abi::scheme_protocol::MAX_TRANSFER_SIZE);
    Box::pin(async move {
        // Safety of unwrap: `resource` was resolved by `get_scheme_proxy`,
        // which only returns handles where `as_scheme_proxy()` is `Some`.
        let proxy = resource.as_scheme_proxy().unwrap();
        match proxy.read_at(offset, buf_len, nonblocking).await {
            Ok(data) => {
                let n = data.len() as u64;
                scheduler::with_current_process(|proc| {
                    if let Some(handle) = proc.handles_mut().get_mut(handle_id) {
                        handle.set_offset(offset + n);
                    }
                });
                SyscallResult::write_back(data.len() as isize, data, dst)
            }
            Err(code) => SyscallResult::err(code),
        }
    })
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
            return SyscallResult::err(panda_abi::ErrorCode::IoError);
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
            Err(_) => SyscallResult::err(panda_abi::ErrorCode::IoError),
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
                    return Poll::Ready(SyscallResult::err(panda_abi::ErrorCode::InvalidHandle));
                };
                let Some(event_source) = resource.as_event_source() else {
                    return Poll::Ready(SyscallResult::err(panda_abi::ErrorCode::InvalidHandle));
                };

                let event_to_result = |event: crate::resource::Event| {
                    let event_bytes = match event {
                        crate::resource::Event::Key(key) => {
                            let mut bytes = [0u8; 8];
                            bytes[0..2].copy_from_slice(&0x01u16.to_ne_bytes());
                            bytes[2..4].copy_from_slice(&key.code.to_ne_bytes());
                            bytes[4..8].copy_from_slice(&key.value.to_ne_bytes());
                            bytes.to_vec()
                        }
                    };
                    let n = event_bytes.len().min(dst.len());
                    let data = event_bytes[..n].to_vec();
                    SyscallResult::write_back(n as isize, data, dst)
                };

                if let Some(event) = event_source.poll() {
                    Poll::Ready(event_to_result(event))
                } else {
                    // Register waker, then re-check: an event may have
                    // arrived (and called `wake()`) between the `poll()`
                    // above and this registration, in which case `wake()`
                    // finds no registered waiter and the event would
                    // otherwise be missed, leaving us blocked forever.
                    if let Some(ref waker) = waker {
                        waker.set_waiting(scheduler::current_process_id());
                    }
                    match event_source.poll() {
                        Some(event) => Poll::Ready(event_to_result(event)),
                        None => Poll::Pending,
                    }
                }
            }))
        }
        None => {
            // Invalid handle
            Box::pin(core::future::ready(SyscallResult::err(
                panda_abi::ErrorCode::InvalidHandle,
            )))
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
    // Scheme-provider proxy resources round-trip to a userspace process.
    if let Some(resource) = get_scheme_proxy(handle_id) {
        let offset = scheduler::with_current_process(|proc| {
            proc.handles().get(handle_id).map(|h| h.offset()).unwrap_or(0)
        });
        return handle_write_scheme_proxy(ua, handle_id, offset, buf_ptr, buf_len, resource);
    }

    // There is no VFS write path: the kernel's one remaining filesystem
    // (`initrd:`'s `TarFs`) is read-only, so `vfs::File` has no `write`
    // method. A VFS-backed handle therefore falls through to the sync path
    // below and is rejected `NotWritable` there, same as any other
    // non-writable resource.
    handle_write_sync(ua, handle_id, buf_ptr, buf_len)
}

/// Write path for scheme-provider proxy resources (M2.2): round-trips
/// `Write` requests to the provider process.
///
/// One protocol frame carries at most `MAX_TRANSFER_SIZE` (4000) bytes, so
/// anything larger is split across several round trips rather than
/// truncated to the first. A single `OP_FILE_WRITE` therefore still writes
/// the whole caller-supplied buffer (up to `MAX_FILE_IO_SIZE`, the same cap
/// the deleted VFS path used), which is what callers expect —
/// `ext2_write_test`'s single 4096-byte write caught the truncation
/// immediately when this path was first exercised.
fn handle_write_scheme_proxy(
    ua: &UserAccess,
    handle_id: u64,
    offset: u64,
    buf_ptr: usize,
    buf_len: usize,
    resource: Arc<dyn crate::resource::Resource>,
) -> SyscallFuture {
    let buf_len = buf_len.min(panda_abi::MAX_FILE_IO_SIZE);
    let data = match ua.read(UserSlice::new(buf_ptr, buf_len)) {
        Ok(d) => d,
        Err(_) => {
            return Box::pin(core::future::ready(SyscallResult::err(
                panda_abi::ErrorCode::InvalidArgument,
            )));
        }
    };

    Box::pin(async move {
        let proxy = resource.as_scheme_proxy().unwrap();
        let mut done = 0;
        while done < data.len() {
            let end = (done + panda_abi::scheme_protocol::MAX_TRANSFER_SIZE).min(data.len());
            let n = match proxy.write_at(offset + done as u64, &data[done..end]).await {
                Ok(n) => n,
                // Report a partial write rather than losing it: the bytes
                // before the failure really did reach the provider.
                Err(code) if done == 0 => return SyscallResult::err(code),
                Err(_) => break,
            };
            if n == 0 {
                break;
            }
            done += n;
        }
        scheduler::with_current_process(|proc| {
            if let Some(handle) = proc.handles_mut().get_mut(handle_id) {
                handle.set_offset(offset + done as u64);
            }
        });
        SyscallResult::ok(done as isize)
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
        Err(_) => {
            return Box::pin(core::future::ready(SyscallResult::err(
                panda_abi::ErrorCode::InvalidArgument,
            )));
        }
    };

    let result: Result<isize, panda_abi::ErrorCode> = scheduler::with_current_process(|proc| {
        let Some(handle) = proc.handles_mut().get_mut(handle_id) else {
            return Err(panda_abi::ErrorCode::InvalidHandle);
        };

        if let Some(char_out) = handle.as_char_output() {
            match char_out.write(&data) {
                Ok(n) => Ok(n as isize),
                Err(_) => Err(panda_abi::ErrorCode::IoError),
            }
        } else {
            Err(panda_abi::ErrorCode::NotWritable)
        }
    });

    match result {
        Ok(n) => Box::pin(core::future::ready(SyscallResult::ok(n))),
        Err(code) => Box::pin(core::future::ready(SyscallResult::err(code))),
    }
}

/// Handle file seek operation.
///
/// For VFS files, this updates the handle offset and performs an async stat to get size.
pub fn handle_seek(handle_id: u64, offset_lo: usize, offset_hi: usize) -> SyscallFuture {
    let offset = ((offset_hi as u64) << 32) | (offset_lo as u64);
    let whence = (offset_hi >> 32) as u32;

    // Scheme-provider proxy resources (`block:`) support seeking too —
    // SEEK_SET/SEEK_CUR are pure handle-table arithmetic (no VFS file
    // needed at all), and SEEK_END queries the provider's size via
    // `Request::Size` (see `SchemeProxyResource::size`) instead of a VFS
    // `stat()`.
    if let Some(resource) = get_scheme_proxy(handle_id) {
        return handle_seek_scheme_proxy(handle_id, offset, whence, resource);
    }

    // Check if this is a VFS file
    let Some(vfs_file) = get_vfs_file(handle_id) else {
        return Box::pin(core::future::ready(SyscallResult::err(
            panda_abi::ErrorCode::InvalidHandle,
        )));
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
            return Box::pin(core::future::ready(SyscallResult::err(
                panda_abi::ErrorCode::InvalidOffset,
            )));
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
            return Box::pin(core::future::ready(SyscallResult::err(
                panda_abi::ErrorCode::InvalidOffset,
            )));
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
                        return SyscallResult::err(panda_abi::ErrorCode::InvalidOffset);
                    }
                    scheduler::with_current_process(|proc| {
                        if let Some(handle) = proc.handles_mut().get_mut(handle_id) {
                            handle.set_offset(new_offset as u64);
                        }
                    });
                    SyscallResult::ok(new_offset as isize)
                }
                Err(_) => SyscallResult::err(panda_abi::ErrorCode::IoError),
            }
        })
    } else {
        Box::pin(core::future::ready(SyscallResult::err(
            panda_abi::ErrorCode::InvalidArgument,
        )))
    }
}

/// Seek path for scheme-provider proxy resources (`block:`) — see
/// `handle_seek`'s doc comment for how this differs from the VFS path.
fn handle_seek_scheme_proxy(
    handle_id: u64,
    offset: u64,
    whence: u32,
    resource: Arc<dyn crate::resource::Resource>,
) -> SyscallFuture {
    let current = scheduler::with_current_process(|proc| {
        proc.handles().get(handle_id).map(|h| h.offset()).unwrap_or(0)
    });

    if whence == SEEK_SET {
        let new_offset = offset as i64;
        if new_offset < 0 {
            return Box::pin(core::future::ready(SyscallResult::err(
                panda_abi::ErrorCode::InvalidOffset,
            )));
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
            return Box::pin(core::future::ready(SyscallResult::err(
                panda_abi::ErrorCode::InvalidOffset,
            )));
        }
        scheduler::with_current_process(|proc| {
            if let Some(handle) = proc.handles_mut().get_mut(handle_id) {
                handle.set_offset(new_offset as u64);
            }
        });
        return Box::pin(core::future::ready(SyscallResult::ok(new_offset as isize)));
    }

    if whence == SEEK_END {
        Box::pin(async move {
            // Safety of unwrap: `resource` was resolved by `get_scheme_proxy`,
            // which only returns handles where `as_scheme_proxy()` is `Some`.
            let proxy = resource.as_scheme_proxy().unwrap();
            match proxy.size().await {
                Ok(size) => {
                    let new_offset = size as i64 + offset as i64;
                    if new_offset < 0 {
                        return SyscallResult::err(panda_abi::ErrorCode::InvalidOffset);
                    }
                    scheduler::with_current_process(|proc| {
                        if let Some(handle) = proc.handles_mut().get_mut(handle_id) {
                            handle.set_offset(new_offset as u64);
                        }
                    });
                    SyscallResult::ok(new_offset as isize)
                }
                Err(code) => SyscallResult::err(code),
            }
        })
    } else {
        Box::pin(core::future::ready(SyscallResult::err(
            panda_abi::ErrorCode::InvalidArgument,
        )))
    }
}

/// Handle file stat operation.
///
/// For VFS files, this performs an async stat operation.
/// The result is written back to userspace via WriteBack.
pub fn handle_stat(handle_id: u64, stat_ptr: usize) -> SyscallFuture {
    let dst = UserSlice::new(stat_ptr, core::mem::size_of::<FileStat>());

    // Scheme-provider proxy resources (`file:`) answer via a
    // `Request::Stat` round trip. Without this they would fall through to
    // the sync path below, which reports `size: 0, is_dir: false` for
    // anything that isn't a directory handle — see `MSG_STAT`'s doc comment.
    if let Some(resource) = get_scheme_proxy(handle_id) {
        return Box::pin(async move {
            // Safety of unwrap: `resource` was resolved by `get_scheme_proxy`,
            // which only returns handles where `as_scheme_proxy()` is `Some`.
            let proxy = resource.as_scheme_proxy().unwrap();
            match proxy.stat().await {
                Ok((size, is_dir)) => {
                    let file_stat = FileStat { size, is_dir };
                    SyscallResult::write_back_struct(0, &file_stat, dst)
                }
                Err(code) => SyscallResult::err(code),
            }
        });
    }

    // Check if this is a VFS file
    if let Some(vfs_file) = get_vfs_file(handle_id) {
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
                Err(_) => SyscallResult::err(panda_abi::ErrorCode::IoError),
            }
        })
    } else {
        // Sync path for non-VFS resources
        let result = scheduler::with_current_process(|proc| {
            let Some(handle) = proc.handles().get(handle_id) else {
                return Err(panda_abi::ErrorCode::InvalidHandle);
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
            Err(code) => Box::pin(core::future::ready(SyscallResult::err(code))),
        }
    }
}

/// Handle file close operation.
///
/// Uses `helpers::remove_handle` rather than removing the handle inline so
/// the removed resource's `Drop` runs after the scheduler lock is
/// released, not before — see that function's doc comment for why (a
/// self-deadlock found via `scheme_provider_test`'s child-exit case
/// hanging instead of erroring cleanly).
pub fn handle_close(handle_id: u64) -> SyscallFuture {
    let removed = super::helpers::remove_handle(handle_id);
    let found = removed.is_some();
    drop(removed);
    if found {
        Box::pin(core::future::ready(SyscallResult::ok(0)))
    } else {
        Box::pin(core::future::ready(SyscallResult::err(
            panda_abi::ErrorCode::InvalidHandle,
        )))
    }
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
            match ua.write_user(UserPtr::new(entry_ptr), &dir_entry) {
                Ok(_) => Box::pin(core::future::ready(SyscallResult::ok(1))),
                Err(_) => Box::pin(core::future::ready(SyscallResult::err(
                    panda_abi::ErrorCode::InvalidArgument,
                ))),
            }
        }
        Ok(None) => Box::pin(core::future::ready(SyscallResult::ok(0))),
        Err(()) => Box::pin(core::future::ready(SyscallResult::err(
            panda_abi::ErrorCode::InvalidHandle,
        ))),
    }
}
