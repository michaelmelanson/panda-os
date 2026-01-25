//! Environment operation syscall handlers (OP_ENVIRONMENT_*).

use alloc::boxed::Box;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::{slice, str};

use log::{debug, error, info};

use crate::{
    process::{PendingSyscall, Process, context::Context},
    resource::{self, ProcessResource},
    scheduler,
};

use super::SyscallContext;

/// Handle environment open operation.
///
/// This syscall is async - if the underlying filesystem needs to do I/O,
/// the process will be blocked until the operation completes.
/// This function does not return - it yields to the scheduler.
pub fn handle_open(ctx: &SyscallContext, uri_ptr: usize, uri_len: usize) -> isize {
    let uri_ptr = uri_ptr as *const u8;
    let uri = unsafe { slice::from_raw_parts(uri_ptr, uri_len) };
    let uri = match str::from_utf8(uri) {
        Ok(u) => u,
        Err(_) => return -1,
    };

    info!("handle_open: uri={}", uri);

    // Create a future for the open operation
    let uri_owned = String::from(uri);
    let future = Box::pin(async move {
        info!("handle_open future: opening {}", uri_owned);
        match resource::open(&uri_owned).await {
            Some(resource) => {
                info!("handle_open future: opened {} successfully", uri_owned);
                let handle_id = scheduler::with_current_process(|proc| {
                    proc.handles_mut().insert(Arc::from(resource)) as isize
                });
                info!("handle_open future: returning handle_id={}", handle_id);
                handle_id
            }
            None => {
                info!("handle_open future: failed to open {}", uri_owned);
                -1
            }
        }
    });

    // Store the pending syscall (don't change state - yield_for_async will do that)
    scheduler::with_current_process(|proc| {
        proc.set_pending_syscall(PendingSyscall::new(future));
    });

    // Yield to scheduler - it will poll the future and return the result
    ctx.yield_for_async()
}

/// Handle environment mount operation.
///
/// This syscall is async - mounting a filesystem requires reading from disk.
/// This function does not return - it yields to the scheduler.
///
/// Arguments:
/// - fstype_ptr, fstype_len: Filesystem type string (e.g., "ext2")
/// - mountpoint_ptr, mountpoint_len: Mount point path (e.g., "/mnt")
///
/// The arguments are packed as: arg0 = (fstype_len << 32) | fstype_ptr_lo
///                              arg1 = (mountpoint_len << 32) | mountpoint_ptr_lo
/// For simplicity in this initial implementation, we use:
///   arg0 = fstype_ptr, arg1 = fstype_len, arg2 = mountpoint_ptr, arg3 = mountpoint_len
pub fn handle_mount(
    ctx: &SyscallContext,
    fstype_ptr: usize,
    fstype_len: usize,
    mountpoint_ptr: usize,
    mountpoint_len: usize,
) -> isize {
    let fstype_ptr = fstype_ptr as *const u8;
    let fstype = unsafe { slice::from_raw_parts(fstype_ptr, fstype_len) };
    let fstype = match str::from_utf8(fstype) {
        Ok(s) => s,
        Err(_) => return -1,
    };

    let mountpoint_ptr = mountpoint_ptr as *const u8;
    let mountpoint = unsafe { slice::from_raw_parts(mountpoint_ptr, mountpoint_len) };
    let mountpoint = match str::from_utf8(mountpoint) {
        Ok(s) => s,
        Err(_) => return -1,
    };

    info!("handle_mount: fstype={}, mountpoint={}", fstype, mountpoint);

    let fstype_owned = String::from(fstype);
    let mountpoint_owned = String::from(mountpoint);

    let future = Box::pin(async move {
        match fstype_owned.as_str() {
            "ext2" => {
                // Mount ext2 from the first block device
                match crate::vfs::mount_ext2(&mountpoint_owned).await {
                    Ok(()) => {
                        info!("Mounted ext2 filesystem at {}", mountpoint_owned);
                        0isize
                    }
                    Err(e) => {
                        error!("Failed to mount ext2 at {}: {}", mountpoint_owned, e);
                        -1isize
                    }
                }
            }
            _ => {
                error!("Unknown filesystem type: {}", fstype_owned);
                -1isize
            }
        }
    });

    scheduler::with_current_process(|proc| {
        proc.set_pending_syscall(PendingSyscall::new(future));
    });

    ctx.yield_for_async()
}

/// Handle environment spawn operation.
///
/// This syscall is async - it needs to open and read the ELF file.
/// This function does not return - it yields to the scheduler.
pub fn handle_spawn(ctx: &SyscallContext, uri_ptr: usize, uri_len: usize) -> isize {
    debug!("SPAWN: uri_ptr={:#x}, uri_len={}", uri_ptr, uri_len);
    let uri_ptr = uri_ptr as *const u8;
    let uri = unsafe { slice::from_raw_parts(uri_ptr, uri_len) };
    debug!("SPAWN: created slice");
    let uri = match str::from_utf8(uri) {
        Ok(u) => u,
        Err(_) => return -1,
    };

    debug!("SPAWN: uri={}", uri);

    // Create a future for the spawn operation
    let uri_owned = String::from(uri);
    let future = Box::pin(async move {
        let Some(resource) = resource::open(&uri_owned).await else {
            error!("SPAWN: failed to open {}", uri_owned);
            return -1;
        };

        // Try VFS file first (for async files like ext2), then Block (for sync like tarfs)
        let elf_data: Vec<u8> = if let Some(vfs_file) = resource.as_vfs_file() {
            // Async read path
            let file_lock = vfs_file.file();
            let mut file = file_lock.lock();

            // Get file size via stat
            let stat = match file.stat().await {
                Ok(s) => s,
                Err(e) => {
                    error!("SPAWN: failed to stat {}: {:?}", uri_owned, e);
                    return -1;
                }
            };
            let size = stat.size as usize;

            let mut elf_data = Vec::new();
            elf_data.resize(size, 0);

            // Read the entire file
            let mut total_read = 0;
            while total_read < size {
                match file.read(&mut elf_data[total_read..]).await {
                    Ok(0) => break, // EOF
                    Ok(n) => total_read += n,
                    Err(e) => {
                        error!("SPAWN: failed to read {}: {:?}", uri_owned, e);
                        return -1;
                    }
                }
            }

            if total_read != size {
                error!("SPAWN: incomplete read: {} of {} bytes", total_read, size);
                return -1;
            }

            elf_data
        } else if let Some(block) = resource.as_block() {
            // Sync block read path (for tarfs, etc.)
            let size = block.size() as usize;
            let mut elf_data = Vec::new();
            elf_data.resize(size, 0);

            match block.read_at(0, &mut elf_data) {
                Ok(n) if n == size => {}
                Ok(n) => {
                    error!("SPAWN: incomplete read: {} of {} bytes", n, size);
                    return -1;
                }
                Err(e) => {
                    error!("SPAWN: failed to read {}: {:?}", uri_owned, e);
                    return -1;
                }
            }

            elf_data
        } else {
            error!("SPAWN: {} is not a readable file", uri_owned);
            return -1;
        };

        let elf_data = elf_data.into_boxed_slice();
        let elf_ptr: *const [u8] = alloc::boxed::Box::leak(elf_data);

        let process = Process::from_elf_data(Context::new_user_context(), elf_ptr);
        let pid = process.id();
        let process_info = process.info().clone();
        debug!("SPAWN: created process {:?}", pid);

        scheduler::add_process(process);

        // Create a handle for the parent to track the child
        let process_resource = ProcessResource::new(process_info);
        let handle_id = scheduler::with_current_process(|proc| {
            proc.handles_mut().insert(Arc::new(process_resource))
        });
        handle_id as isize
    });

    // Store the pending syscall (don't change state - yield_for_async will do that)
    scheduler::with_current_process(|proc| {
        proc.set_pending_syscall(PendingSyscall::new(future));
    });

    // Yield to scheduler - it will poll the future and return the result
    ctx.yield_for_async()
}

/// Handle environment log operation.
pub fn handle_log(msg_ptr: usize, msg_len: usize) -> isize {
    debug!("LOG: msg_ptr={:#x}, msg_len={}", msg_ptr, msg_len);
    let msg_ptr = msg_ptr as *const u8;
    let msg = unsafe { slice::from_raw_parts(msg_ptr, msg_len) };
    debug!("LOG: created slice");
    let msg = match str::from_utf8(msg) {
        Ok(m) => m,
        Err(_) => return -1,
    };
    info!("LOG: {msg}");
    0
}

/// Handle environment time operation.
pub fn handle_time() -> isize {
    // TODO: Implement getting time
    0
}

/// Handle environment opendir operation.
///
/// This syscall is async - directory listing may require disk I/O.
/// This function does not return - it yields to the scheduler.
pub fn handle_opendir(ctx: &SyscallContext, uri_ptr: usize, uri_len: usize) -> isize {
    let uri_ptr = uri_ptr as *const u8;
    let uri = unsafe { slice::from_raw_parts(uri_ptr, uri_len) };
    let uri = match str::from_utf8(uri) {
        Ok(u) => u,
        Err(_) => return -1,
    };

    // Create a future for the opendir operation
    let uri_owned = String::from(uri);
    let future = Box::pin(async move {
        let Some(entries) = resource::readdir(&uri_owned).await else {
            return -1;
        };

        let dir_resource = resource::DirectoryResource::new(entries);
        let handle_id = scheduler::with_current_process(|proc| {
            proc.handles_mut().insert(Arc::new(dir_resource))
        });
        handle_id as isize
    });

    // Store the pending syscall (don't change state - yield_for_async will do that)
    scheduler::with_current_process(|proc| {
        proc.set_pending_syscall(PendingSyscall::new(future));
    });

    // Yield to scheduler - it will poll the future and return the result
    ctx.yield_for_async()
}
