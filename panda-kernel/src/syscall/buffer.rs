//! Buffer operation syscall handlers (OP_BUFFER_*).

use alloc::boxed::Box;

use crate::{
    resource::{Buffer, SharedBuffer},
    scheduler,
};

/// Handle buffer allocation.
/// Returns handle_id on success, negative on error.
/// If info_ptr is non-zero, writes BufferAllocInfo to that address.
pub fn handle_alloc(size: usize, info_ptr: usize) -> isize {
    scheduler::with_current_process(|proc| {
        match SharedBuffer::alloc(proc, size) {
            Ok((buffer, mapped_addr)) => {
                let buffer_size = Buffer::size(&buffer);
                let handle_id = proc.handles_mut().insert(Box::new(buffer));

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
            let Some(handle) = proc.handles_mut().get_mut(handle_id) else {
                return -1;
            };
            let Some(buffer) = handle.as_buffer_mut() else {
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
                    handle.replace_resource(Box::new(new_buffer));
                    let Some(buffer) = handle.as_buffer_mut() else {
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
pub fn handle_read_buffer(file_handle_id: u32, buffer_handle_id: u32) -> isize {
    scheduler::with_current_process(|proc| {
        // Get buffer size
        let buffer_size = {
            let Some(buffer_handle) = proc.handles().get(buffer_handle_id) else {
                return -1;
            };
            let Some(buffer) = buffer_handle.as_buffer() else {
                return -1;
            };
            buffer.size()
        };

        // Get file's block interface info and read data
        let (data, file_offset) = {
            let Some(file_handle) = proc.handles().get(file_handle_id) else {
                return -1;
            };
            let Some(block) = file_handle.as_block() else {
                return -1;
            };
            let file_offset = file_handle.offset();
            let block_size = block.size();

            // Calculate how much to read
            let remaining = block_size.saturating_sub(file_offset) as usize;
            let to_read = buffer_size.min(remaining);

            if to_read == 0 {
                return 0;
            }

            // Read into temporary buffer
            let mut temp = alloc::vec![0u8; to_read];
            match block.read_at(file_offset, &mut temp) {
                Ok(n) => {
                    temp.truncate(n);
                    (temp, file_offset)
                }
                Err(_) => return -1,
            }
        };

        let bytes_read = data.len();

        // Copy data to the shared buffer
        {
            let Some(buffer_handle) = proc.handles_mut().get_mut(buffer_handle_id) else {
                return -1;
            };
            let Some(buffer) = buffer_handle.as_buffer_mut() else {
                return -1;
            };
            buffer.as_mut_slice()[..bytes_read].copy_from_slice(&data);
        }

        // Update file offset
        {
            let Some(file_handle) = proc.handles_mut().get_mut(file_handle_id) else {
                return -1;
            };
            file_handle.set_offset(file_offset + bytes_read as u64);
        }

        bytes_read as isize
    })
}

/// Handle writing from buffer to file.
/// Returns bytes written on success, negative on error.
pub fn handle_write_buffer(file_handle_id: u32, buffer_handle_id: u32, len: usize) -> isize {
    scheduler::with_current_process(|proc| {
        // Get buffer data
        let (buffer_data, write_len) = {
            let Some(buffer_handle) = proc.handles().get(buffer_handle_id) else {
                return -1;
            };
            let Some(buffer) = buffer_handle.as_buffer() else {
                return -1;
            };
            let buf_slice = buffer.as_slice();
            let write_len = len.min(buf_slice.len());
            // Copy the data since we need to release the borrow
            let mut data = alloc::vec![0u8; write_len];
            data.copy_from_slice(&buf_slice[..write_len]);
            (data, write_len)
        };

        // Get file handle and write
        let handles = proc.handles_mut();
        let Some(file_handle) = handles.get_mut(file_handle_id) else {
            return -1;
        };

        let file_offset = file_handle.offset();

        let Some(block) = file_handle.as_block() else {
            return -1;
        };

        match block.write_at(file_offset, &buffer_data[..write_len]) {
            Ok(n) => {
                // Update file offset - need to get handle again
                let file_handle = handles.get_mut(file_handle_id).unwrap();
                file_handle.set_offset(file_offset + n as u64);
                n as isize
            }
            Err(_) => -1,
        }
    })
}
