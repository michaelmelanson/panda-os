//! Buffer operation syscall handlers (OP_BUFFER_*).

use alloc::boxed::Box;
use alloc::sync::Arc;

use panda_abi::HandleType;

use crate::process::PendingSyscall;
use crate::resource::{Buffer, SharedBuffer, VfsFile};
use crate::scheduler;
use crate::vfs::SeekFrom;

use super::{SyscallContext, VfsFileWrapper};

/// Handle buffer allocation.
/// Returns handle_id on success, negative on error.
/// If info_ptr is non-zero, writes BufferAllocInfo to that address.
pub fn handle_alloc(size: usize, info_ptr: usize) -> isize {
    scheduler::with_current_process(|proc| {
        match SharedBuffer::alloc(proc, size) {
            Ok((buffer, mapped_addr)) => {
                let buffer_size = Buffer::size(&*buffer);
                let handle_id = proc.handles_mut().insert_typed(HandleType::Buffer, buffer);

                // Write full info to userspace if pointer provided
                if info_ptr != 0 {
                    unsafe {
                        let info = info_ptr as *mut panda_abi::BufferAllocInfo;
                        (*info).addr = mapped_addr;
                        (*info).size = buffer_size;
                    }
                }

                handle_id as isize
            }
            Err(_) => -1,
        }
    })
}

/// Handle buffer resize.
/// Returns 0 on success, negative on error.
/// If info_ptr is non-zero, writes BufferAllocInfo to that address.
pub fn handle_resize(handle_id: u32, new_size: usize, info_ptr: usize) -> isize {
    scheduler::with_current_process(|proc| {
        // Try in-place resize first
        let resize_result = {
            let Some(handle) = proc.handles().get(handle_id) else {
                return -1;
            };
            let Some(buffer) = handle.as_buffer() else {
                return -1;
            };
            buffer.resize(new_size)
        };

        match resize_result {
            Ok(new_addr) => {
                // Write info to userspace if pointer provided
                if info_ptr != 0 {
                    let buffer_size = {
                        let Some(handle) = proc.handles().get(handle_id) else {
                            return -1;
                        };
                        let Some(buffer) = handle.as_buffer() else {
                            return -1;
                        };
                        buffer.size()
                    };

                    unsafe {
                        let info = info_ptr as *mut panda_abi::BufferAllocInfo;
                        (*info).addr = new_addr;
                        (*info).size = buffer_size;
                    }
                }
                0
            }
            Err(_) => {
                // In-place resize failed, need to reallocate
                // Get old buffer data, virtual address, and size
                let (old_data, old_vaddr, old_num_pages) = {
                    let Some(handle) = proc.handles().get(handle_id) else {
                        return -1;
                    };
                    let Some(buffer) = handle.as_buffer() else {
                        return -1;
                    };
                    let old_size = buffer.size();
                    let copy_size = old_size.min(new_size);
                    let vaddr = x86_64::VirtAddr::new(buffer.mapped_addr() as u64);
                    let num_pages = (old_size + 4095) / 4096; // Round up to pages
                    (buffer.as_slice()[..copy_size].to_vec(), vaddr, num_pages)
                };

                // Allocate new buffer
                let (new_buffer, new_addr) = match SharedBuffer::alloc(proc, new_size) {
                    Ok(result) => result,
                    Err(_) => return -1,
                };

                // Replace the buffer and copy data
                let buffer_size = {
                    let Some(handle) = proc.handles_mut().get_mut(handle_id) else {
                        return -1;
                    };
                    handle.replace_resource(new_buffer);
                    let Some(buffer) = handle.as_buffer() else {
                        return -1;
                    };
                    buffer.as_mut_slice()[..old_data.len()].copy_from_slice(&old_data);
                    buffer.size()
                };

                // Free the old buffer's virtual address space
                proc.free_buffer_vaddr(old_vaddr, old_num_pages);

                // Write info to userspace if pointer provided
                if info_ptr != 0 {
                    unsafe {
                        let info = info_ptr as *mut panda_abi::BufferAllocInfo;
                        (*info).addr = new_addr;
                        (*info).size = buffer_size;
                    }
                }

                0
            }
        }
    })
}

/// Handle buffer free.
pub fn handle_free(handle_id: u32) -> isize {
    scheduler::with_current_process(|proc| {
        // Get buffer's virtual address and size before removing
        let (vaddr, num_pages) = {
            let Some(handle) = proc.handles().get(handle_id) else {
                return -1;
            };
            let Some(buffer) = handle.as_buffer() else {
                return -1;
            };
            let vaddr = x86_64::VirtAddr::new(buffer.mapped_addr() as u64);
            let num_pages = (buffer.size() + 4095) / 4096; // Round up to pages
            (vaddr, num_pages)
        };

        // Remove the handle (drops the buffer, unmapping pages)
        if proc.handles_mut().remove(handle_id).is_none() {
            return -1;
        }

        // Free the virtual address space
        proc.free_buffer_vaddr(vaddr, num_pages);

        0
    })
}

/// Handle reading from file into buffer.
/// Returns bytes read on success, negative on error.
pub fn handle_read_buffer(
    ctx: &SyscallContext,
    file_handle_id: u32,
    buffer_handle_id: u32,
) -> isize {
    // Get buffer info (shared buffer pointer and size)
    let (buffer_arc, buffer_size) = match scheduler::with_current_process(|proc| {
        let buffer_handle = proc.handles().get(buffer_handle_id)?;
        let buffer = buffer_handle.resource_arc().as_shared_buffer()?;
        Some((buffer.clone(), buffer.size()))
    }) {
        Some(info) => info,
        None => return -1,
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
        None => return -1,
    };

    // Create async future for the read
    let future = Box::pin(async move {
        let file_lock = vfs_file.file();
        let mut file = file_lock.lock();

        // Seek to current offset
        if file.seek(SeekFrom::Start(file_offset)).await.is_err() {
            return -1isize;
        }

        // Read into buffer
        let buf = buffer_arc.as_mut_slice();
        let to_read = buf.len().min(buffer_size);
        match file.read(&mut buf[..to_read]).await {
            Ok(n) => {
                // Update file offset
                scheduler::with_current_process(|proc| {
                    if let Some(handle) = proc.handles_mut().get_mut(file_handle_id) {
                        handle.set_offset(file_offset + n as u64);
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

/// Handle writing from buffer to file.
/// Returns bytes written on success, negative on error.
pub fn handle_write_buffer(
    ctx: &SyscallContext,
    file_handle_id: u32,
    buffer_handle_id: u32,
    len: usize,
) -> isize {
    // Get buffer data (copy to owned Vec since we need it in async block)
    let (buffer_data, write_len) = match scheduler::with_current_process(|proc| {
        let buffer_handle = proc.handles().get(buffer_handle_id)?;
        let buffer = buffer_handle.as_buffer()?;
        let buf_slice = buffer.as_slice();
        let write_len = len.min(buf_slice.len());
        let mut data = alloc::vec![0u8; write_len];
        data.copy_from_slice(&buf_slice[..write_len]);
        Some((data, write_len))
    }) {
        Some(info) => info,
        None => return -1,
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
        None => return -1,
    };

    // Create async future for the write
    let future = Box::pin(async move {
        let file_lock = vfs_file.file();
        let mut file = file_lock.lock();

        // Seek to current offset
        if file.seek(SeekFrom::Start(file_offset)).await.is_err() {
            return -1isize;
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
