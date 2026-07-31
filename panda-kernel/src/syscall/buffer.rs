//! Buffer operation syscall handlers (OP_BUFFER_*).

#![deny(unsafe_code)]

use alloc::boxed::Box;
use alloc::sync::Arc;

use panda_abi::HandleType;

use crate::resource::{Buffer, BufferExt, SharedBuffer, VfsFile};
use crate::scheduler;
use crate::vfs::SeekFrom;

use super::VfsFileWrapper;
use super::user_ptr::{SyscallFuture, SyscallResult, UserAccess, UserPtr};

/// Handle buffer allocation.
/// Returns handle_id on success, negative on error.
/// If info_ptr is non-zero, writes BufferAllocInfo to that address.
pub fn handle_alloc(ua: &UserAccess, size: usize, info_ptr: usize) -> SyscallFuture {
    let info_out: Option<UserPtr<panda_abi::BufferAllocInfo>> = if info_ptr != 0 {
        Some(UserPtr::new(info_ptr))
    } else {
        None
    };

    let result = scheduler::with_current_process(|proc| {
        match SharedBuffer::alloc(proc, size) {
            Ok((buffer, mapped_addr)) => {
                let buffer_size = Buffer::size(&*buffer);
                let handle_id = match proc.handles_mut().insert_typed(HandleType::Buffer, buffer) {
                    Ok(id) => id,
                    Err(_) => {
                        let num_pages = (size + 4095) / 4096;
                        proc.free_buffer_vaddr(
                            x86_64::VirtAddr::new(mapped_addr as u64),
                            num_pages,
                        );
                        return None;
                    }
                };

                // Write full info to userspace if pointer provided
                if info_out.is_some() {
                    let info = panda_abi::BufferAllocInfo {
                        addr: mapped_addr,
                        size: buffer_size,
                    };
                    Some((handle_id, Some(info)))
                } else {
                    Some((handle_id, None))
                }
            }
            Err(_) => None,
        }
    });

    match result {
        Some((handle_id, Some(info))) => {
            let Some(out) = info_out else {
                return Box::pin(core::future::ready(SyscallResult::err(
                    panda_abi::ErrorCode::InvalidArgument,
                )));
            };
            if ua.write_user(out, &info).is_err() {
                return Box::pin(core::future::ready(SyscallResult::err(
                    panda_abi::ErrorCode::InvalidArgument,
                )));
            }
            Box::pin(core::future::ready(SyscallResult::ok(handle_id as isize)))
        }
        Some((handle_id, None)) => {
            Box::pin(core::future::ready(SyscallResult::ok(handle_id as isize)))
        }
        None => Box::pin(core::future::ready(SyscallResult::err(
            panda_abi::ErrorCode::IoError,
        ))),
    }
}

/// Handle buffer resize.
/// Returns 0 on success, negative on error.
/// If info_ptr is non-zero, writes BufferAllocInfo to that address.
pub fn handle_resize(
    ua: &UserAccess,
    handle_id: u64,
    new_size: usize,
    info_ptr: usize,
) -> SyscallFuture {
    let info_out: Option<UserPtr<panda_abi::BufferAllocInfo>> = if info_ptr != 0 {
        Some(UserPtr::new(info_ptr))
    } else {
        None
    };
    // Resize is an owner-only operation: `buffer.mapped_addr()` /
    // `with_slice` / `with_mut_slice` below are only meaningful in the
    // allocating process's address space, and the reallocation path
    // additionally calls `proc.free_buffer_vaddr` and
    // `handle.replace_resource` — reclaiming vaddr space and swapping the
    // resource behind a handle the caller doesn't own would corrupt the
    // caller's own allocator state and silently change the buffer the
    // actual owner sees. A process that merely received the handle via
    // transfer must not be able to reach any of this.
    let owner_check = scheduler::with_current_process(|proc| {
        let handle = proc.handles().get(handle_id)?;
        let buffer = handle.resource_arc().as_shared_buffer()?;
        Some(buffer.owner() == proc.id())
    });
    match owner_check {
        Some(true) => {}
        Some(false) => {
            return Box::pin(core::future::ready(SyscallResult::err(
                panda_abi::ErrorCode::PermissionDenied,
            )));
        }
        None => {
            return Box::pin(core::future::ready(SyscallResult::err(
                panda_abi::ErrorCode::InvalidHandle,
            )));
        }
    }

    let result = scheduler::with_current_process(|proc| {
        // Try in-place resize first
        let resize_result = {
            let Some(handle) = proc.handles().get(handle_id) else {
                return Err(());
            };
            let Some(buffer) = handle.as_buffer() else {
                return Err(());
            };
            buffer.resize(new_size)
        };

        match resize_result {
            Ok(new_addr) => {
                // Write info to userspace if pointer provided
                if info_out.is_some() {
                    let buffer_size = {
                        let Some(handle) = proc.handles().get(handle_id) else {
                            return Err(());
                        };
                        let Some(buffer) = handle.as_buffer() else {
                            return Err(());
                        };
                        buffer.size()
                    };
                    Ok(Some(panda_abi::BufferAllocInfo {
                        addr: new_addr,
                        size: buffer_size,
                    }))
                } else {
                    Ok(None)
                }
            }
            Err(_) => {
                // In-place resize failed, need to reallocate
                let (old_data, old_vaddr, old_num_pages) = {
                    let Some(handle) = proc.handles().get(handle_id) else {
                        return Err(());
                    };
                    let Some(buffer) = handle.as_buffer() else {
                        return Err(());
                    };
                    let old_size = buffer.size();
                    let copy_size = old_size.min(new_size);
                    let vaddr = x86_64::VirtAddr::new(buffer.mapped_addr() as u64);
                    let num_pages = (old_size + 4095) / 4096;
                    let old_data = buffer.with_slice(|s| s[..copy_size].to_vec());
                    (old_data, vaddr, num_pages)
                };

                // Allocate new buffer
                let (new_buffer, new_addr) = match SharedBuffer::alloc(proc, new_size) {
                    Ok(result) => result,
                    Err(_) => return Err(()),
                };

                // Replace the buffer and copy data
                let buffer_size = {
                    let Some(handle) = proc.handles_mut().get_mut(handle_id) else {
                        return Err(());
                    };
                    handle.replace_resource(new_buffer);
                    let Some(buffer) = handle.as_buffer() else {
                        return Err(());
                    };
                    buffer.with_mut_slice(|s| s[..old_data.len()].copy_from_slice(&old_data));
                    buffer.size()
                };

                // Free the old buffer's virtual address space
                proc.free_buffer_vaddr(old_vaddr, old_num_pages);

                if info_out.is_some() {
                    Ok(Some(panda_abi::BufferAllocInfo {
                        addr: new_addr,
                        size: buffer_size,
                    }))
                } else {
                    Ok(None)
                }
            }
        }
    });

    match result {
        Ok(Some(info)) => {
            let Some(out) = info_out else {
                return Box::pin(core::future::ready(SyscallResult::err(
                    panda_abi::ErrorCode::InvalidArgument,
                )));
            };
            if ua.write_user(out, &info).is_err() {
                return Box::pin(core::future::ready(SyscallResult::err(
                    panda_abi::ErrorCode::InvalidArgument,
                )));
            }
            Box::pin(core::future::ready(SyscallResult::ok(0)))
        }
        Ok(None) => Box::pin(core::future::ready(SyscallResult::ok(0))),
        Err(()) => Box::pin(core::future::ready(SyscallResult::err(
            panda_abi::ErrorCode::InvalidHandle,
        ))),
    }
}

/// Handle buffer map: map an already-existing `SharedBuffer` into the
/// CURRENT process's address space (`OP_BUFFER_MAP`).
///
/// Unlike `OP_BUFFER_ALLOC`, this never creates a buffer — `handle_id` must
/// already resolve to a `SharedBuffer` (e.g. received via
/// `Channel::recv_with_handle`, or the caller's own buffer). Returns the
/// newly mapped virtual address on success, `InvalidHandle` if `handle_id`
/// doesn't resolve to a buffer.
///
/// # Idempotence policy
///
/// Mapping the same buffer more than once in the same process — including
/// the allocating process, which already has the buffer mapped from
/// `OP_BUFFER_ALLOC` — is **not** deduplicated: each call creates a new,
/// independent mapping at a fresh vaddr range rather than returning an
/// existing one. See `SharedBuffer::map_into_process` for the full
/// rationale; `buffer_test` and `buffer_transfer_test` cover this.
///
/// # Lifetime
///
/// The mapping is registered with the process (`Process::add_mapping`), so
/// it is unmapped automatically when the process exits — see
/// `resource::buffer::SharedBuffer`'s "Cross-process mapping safety" doc
/// comment for why this can never leave a dangling mapping into freed
/// memory. Closing `handle_id` does NOT unmap this mapping — the mapping
/// itself keeps the buffer's frames alive independent of the handle; that
/// is deliberate future work (see the same doc comment).
pub fn handle_map(handle_id: u64) -> SyscallFuture {
    let result: Result<usize, panda_abi::ErrorCode> = scheduler::with_current_process(|proc| {
        let buffer = {
            let Some(handle) = proc.handles().get(handle_id) else {
                return Err(panda_abi::ErrorCode::InvalidHandle);
            };
            let Some(buffer) = handle.resource_arc().as_shared_buffer() else {
                return Err(panda_abi::ErrorCode::InvalidHandle);
            };
            buffer
        };

        buffer
            .map_into_process(proc)
            .map_err(|_| panda_abi::ErrorCode::IoError)
    });

    match result {
        Ok(vaddr) => Box::pin(core::future::ready(SyscallResult::ok(vaddr as isize))),
        Err(code) => Box::pin(core::future::ready(SyscallResult::err(code))),
    }
}

/// Handle buffer free.
pub fn handle_free(handle_id: u64) -> SyscallFuture {
    let result: Result<(), panda_abi::ErrorCode> = scheduler::with_current_process(|proc| {
        // Get buffer's virtual address and size before removing, and check
        // whether the current process is the owner: `vaddr` (`user_vaddr`)
        // only belongs to the current process's vaddr allocator if it is —
        // for a merely-transferred handle, reclaiming it via
        // `free_buffer_vaddr` below would corrupt the current process's own
        // allocator state.
        let reclaim = {
            let Some(handle) = proc.handles().get(handle_id) else {
                return Err(panda_abi::ErrorCode::InvalidHandle);
            };
            let Some(buffer) = handle.resource_arc().as_shared_buffer() else {
                return Err(panda_abi::ErrorCode::InvalidHandle);
            };
            if buffer.owner() == proc.id() {
                let vaddr = x86_64::VirtAddr::new(buffer.mapped_addr() as u64);
                let num_pages = (buffer.size() + 4095) / 4096;
                Some((vaddr, num_pages))
            } else {
                None
            }
        };

        // Remove the handle (drops the buffer, unmapping pages, if this was
        // the last reference)
        if proc.handles_mut().remove(handle_id).is_none() {
            return Err(panda_abi::ErrorCode::InvalidHandle);
        }

        // Free the virtual address space — only if we're the owner.
        if let Some((vaddr, num_pages)) = reclaim {
            proc.free_buffer_vaddr(vaddr, num_pages);
        }

        Ok(())
    });
    match result {
        Ok(()) => Box::pin(core::future::ready(SyscallResult::ok(0))),
        Err(code) => Box::pin(core::future::ready(SyscallResult::err(code))),
    }
}

/// Handle reading from file into buffer.
/// Returns bytes read on success, negative on error.
pub fn handle_read_buffer(file_handle_id: u64, buffer_handle_id: u64) -> SyscallFuture {
    // Get buffer info (shared buffer pointer and size)
    let (buffer_arc, buffer_size) = match scheduler::with_current_process(|proc| {
        let buffer_handle = proc.handles().get(buffer_handle_id)?;
        let buffer = buffer_handle.resource_arc().as_shared_buffer()?;
        Some((buffer.clone(), buffer.size()))
    }) {
        Some(info) => info,
        None => {
            return Box::pin(core::future::ready(SyscallResult::err(
                panda_abi::ErrorCode::InvalidHandle,
            )));
        }
    };

    // `user_vaddr` (dereferenced below via `with_mut_slice`) is only
    // meaningful in the allocating process's address space — a process that
    // merely received this handle via transfer must not be able to reach it.
    if buffer_arc.owner() != scheduler::current_process_id() {
        return Box::pin(core::future::ready(SyscallResult::err(
            panda_abi::ErrorCode::PermissionDenied,
        )));
    }

    // Get VFS file and current offset
    let (vfs_file, file_offset) = match scheduler::with_current_process(|proc| {
        let file_handle = proc.handles().get(file_handle_id)?;
        let _ = file_handle.as_vfs_file()?;
        let offset = file_handle.offset();
        let resource_arc = file_handle.resource_arc();
        Some((
            Arc::new(VfsFileWrapper(resource_arc)) as Arc<dyn VfsFile>,
            offset,
        ))
    }) {
        Some(info) => info,
        None => {
            return Box::pin(core::future::ready(SyscallResult::err(
                panda_abi::ErrorCode::InvalidHandle,
            )));
        }
    };

    // Create async future for the read
    Box::pin(async move {
        let file_lock = vfs_file.file();
        let mut file = file_lock.lock();

        // Seek to current offset
        if file.seek(SeekFrom::Start(file_offset)).await.is_err() {
            return SyscallResult::err(panda_abi::ErrorCode::IoError);
        }

        // Read into a kernel bounce buffer, then copy into the user-mapped buffer.
        // We cannot hold a reference to user-mapped memory across an .await point
        // because SMAP must be re-enabled between polls.
        let to_read = buffer_arc.size().min(buffer_size);
        let mut kernel_buf = alloc::vec![0u8; to_read];
        match file.read(&mut kernel_buf[..to_read]).await {
            Ok(n) => {
                // Copy from kernel bounce buffer into user-mapped SharedBuffer
                buffer_arc.with_mut_slice(|buf| {
                    buf[..n].copy_from_slice(&kernel_buf[..n]);
                });
                // Update file offset
                scheduler::with_current_process(|proc| {
                    if let Some(handle) = proc.handles_mut().get_mut(file_handle_id) {
                        handle.set_offset(file_offset + n as u64);
                    }
                });
                SyscallResult::ok(n as isize)
            }
            Err(_) => SyscallResult::err(panda_abi::ErrorCode::IoError),
        }
    })
}

/// Handle writing from buffer to file.
/// Returns bytes written on success, negative on error.
pub fn handle_write_buffer(
    file_handle_id: u64,
    buffer_handle_id: u64,
    len: usize,
) -> SyscallFuture {
    // Get buffer data (copy to owned Vec since we need it in async block).
    // SMAP: use with_slice to access user-mapped buffer pages.
    let write_result = scheduler::with_current_process(|proc| {
        let buffer_handle = proc.handles().get(buffer_handle_id)?;
        let buffer = buffer_handle.resource_arc().as_shared_buffer()?;

        // `user_vaddr` (dereferenced below via `with_slice`) is only
        // meaningful in the allocating process's address space — a process
        // that merely received this handle via transfer must not be able to
        // reach it.
        if buffer.owner() != proc.id() {
            return Some(Err(()));
        }

        Some(Ok(buffer.with_slice(|buf_slice| {
            let write_len = len.min(buf_slice.len());
            let mut data = alloc::vec![0u8; write_len];
            data.copy_from_slice(&buf_slice[..write_len]);
            (data, write_len)
        })))
    });
    let (buffer_data, write_len) = match write_result {
        Some(Ok(info)) => info,
        Some(Err(())) => {
            return Box::pin(core::future::ready(SyscallResult::err(
                panda_abi::ErrorCode::PermissionDenied,
            )));
        }
        None => {
            return Box::pin(core::future::ready(SyscallResult::err(
                panda_abi::ErrorCode::InvalidHandle,
            )));
        }
    };

    // Get VFS file and current offset
    let (vfs_file, file_offset) = match scheduler::with_current_process(|proc| {
        let file_handle = proc.handles().get(file_handle_id)?;
        let _ = file_handle.as_vfs_file()?;
        let offset = file_handle.offset();
        let resource_arc = file_handle.resource_arc();
        Some((
            Arc::new(VfsFileWrapper(resource_arc)) as Arc<dyn VfsFile>,
            offset,
        ))
    }) {
        Some(info) => info,
        None => {
            return Box::pin(core::future::ready(SyscallResult::err(
                panda_abi::ErrorCode::InvalidHandle,
            )));
        }
    };

    // Create async future for the write
    Box::pin(async move {
        let file_lock = vfs_file.file();
        let mut file = file_lock.lock();

        // Seek to current offset
        if file.seek(SeekFrom::Start(file_offset)).await.is_err() {
            return SyscallResult::err(panda_abi::ErrorCode::IoError);
        }

        // Write from buffer
        match file.write(&buffer_data[..write_len]).await {
            Ok(n) => {
                // Update file offset
                scheduler::with_current_process(|proc| {
                    if let Some(handle) = proc.handles_mut().get_mut(file_handle_id) {
                        handle.set_offset(file_offset + n as u64);
                    }
                });
                SyscallResult::ok(n as isize)
            }
            Err(_) => SyscallResult::err(panda_abi::ErrorCode::IoError),
        }
    })
}
