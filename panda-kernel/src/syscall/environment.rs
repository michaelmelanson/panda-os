//! Environment operation syscall handlers (OP_ENVIRONMENT_*).

use alloc::boxed::Box;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::{slice, str};

use log::{debug, error, info};

use crate::{
    process::{PendingSyscall, Process, context::Context},
    resource, scheduler,
};

use super::SyscallContext;

/// Handle environment open operation.
///
/// This syscall is async - if the underlying filesystem needs to do I/O,
/// the process will be blocked until the operation completes.
/// This function does not return - it yields to the scheduler.
///
/// Arguments:
/// - uri_ptr, uri_len: URI of resource to open
/// - mailbox_handle: Handle of mailbox to attach to (0 = don't attach, use HANDLE_MAILBOX for default)
/// - event_mask: Events to listen for (0 = no events)
pub fn handle_open(
    ctx: &SyscallContext,
    uri_ptr: usize,
    uri_len: usize,
    mailbox_handle: usize,
    event_mask: usize,
) -> isize {
    let mailbox_handle = mailbox_handle as u32;
    let event_mask = event_mask as u32;
    let uri_ptr = uri_ptr as *const u8;
    let uri = unsafe { slice::from_raw_parts(uri_ptr, uri_len) };
    let uri = match str::from_utf8(uri) {
        Ok(u) => u,
        Err(_) => return -1,
    };

    debug!(
        "handle_open: uri={}, mailbox={}, event_mask={:#x}",
        uri, mailbox_handle, event_mask
    );

    // Create a future for the open operation
    let uri_owned = String::from(uri);
    let future = Box::pin(async move {
        debug!("handle_open future: opening {}", uri_owned);
        match resource::open(&uri_owned).await {
            Some(resource) => {
                debug!("handle_open future: opened {} successfully", uri_owned);
                let handle_id = scheduler::with_current_process(|proc| {
                    let handle_id = proc.handles_mut().insert(Arc::from(resource));

                    // Attach to mailbox if requested
                    if mailbox_handle != 0 && event_mask != 0 {
                        if let Some(mailbox_h) = proc.handles().get(mailbox_handle) {
                            if let Some(mailbox) = mailbox_h.as_mailbox() {
                                // Tell mailbox which handles to track
                                mailbox.attach(handle_id, event_mask);

                                // Tell the resource where to post events
                                // (needed for resources that generate async events like keyboards)
                                if let Some(opened_h) = proc.handles().get(handle_id) {
                                    if let Some(keyboard) = opened_h.as_keyboard() {
                                        let mailbox_ref =
                                            resource::MailboxRef::new(mailbox, handle_id);
                                        keyboard.attach_mailbox(mailbox_ref);
                                    }
                                }
                            }
                        }
                    }

                    handle_id as isize
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
///
/// Arguments:
/// - uri_ptr, uri_len: URI of executable to spawn
/// - mailbox_handle: Handle of mailbox to attach spawn handle to (0 = don't attach)
/// - event_mask: Events to listen for on the spawn handle
///
/// Creates a channel between parent and child. Child receives its endpoint at HANDLE_PARENT.
/// Parent receives a SpawnHandle that combines channel + process info.
pub fn handle_spawn(
    ctx: &SyscallContext,
    uri_ptr: usize,
    uri_len: usize,
    mailbox_handle: usize,
    event_mask: usize,
) -> isize {
    let mailbox_handle = mailbox_handle as u32;
    let event_mask = event_mask as u32;
    debug!(
        "SPAWN: uri_ptr={:#x}, uri_len={}, mailbox={}, event_mask={:#x}",
        uri_ptr, uri_len, mailbox_handle, event_mask
    );
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

        // Read the file via async VFS interface
        let Some(vfs_file) = resource.as_vfs_file() else {
            error!("SPAWN: {} is not a readable file", uri_owned);
            return -1;
        };

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

        let elf_data = elf_data.into_boxed_slice();
        let elf_ptr: *const [u8] = alloc::boxed::Box::leak(elf_data);

        let mut process = match Process::from_elf_data(Context::new_user_context(), elf_ptr) {
            Ok(p) => p,
            Err(e) => {
                error!(
                    "SPAWN: failed to create process from {}: {:?}",
                    uri_owned, e
                );
                return -1;
            }
        };
        let pid = process.id();
        let process_info = process.info().clone();
        debug!("SPAWN: created process {:?}", pid);

        // Create channel pair for parent-child communication
        let (parent_endpoint, child_endpoint) = resource::ChannelEndpoint::create_pair();

        // Give child endpoint at HANDLE_PARENT
        process
            .handles_mut()
            .insert_at(panda_abi::HANDLE_PARENT, Arc::new(child_endpoint));

        scheduler::add_process(process);

        // Create SpawnHandle combining channel and process info
        let spawn_handle = resource::SpawnHandle::new(parent_endpoint, process_info);

        let handle_id = scheduler::with_current_process(|proc| {
            // First, insert the handle to get its ID
            let handle_id = proc.handles_mut().insert(Arc::new(spawn_handle));

            // Attach to mailbox if requested
            if mailbox_handle != 0 && event_mask != 0 {
                if let Some(mailbox_h) = proc.handles().get(mailbox_handle) {
                    if let Some(mailbox) = mailbox_h.as_mailbox() {
                        // Attach handle to mailbox (tells mailbox which handles to track)
                        mailbox.attach(handle_id, event_mask);

                        // Attach mailbox to resource (tells resource where to post events)
                        if let Some(spawn_h) = proc.handles().get(handle_id) {
                            let mailbox_ref = resource::MailboxRef::new(mailbox, handle_id);
                            spawn_h.attach_mailbox(mailbox_ref);
                        }
                    }
                }
            }

            handle_id
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
