//! Environment operation syscall handlers (OP_ENVIRONMENT_*).

#![deny(unsafe_code)]

use alloc::boxed::Box;
use alloc::sync::Arc;
use alloc::vec::Vec;

use log::{debug, error, info};

use crate::{
    process::{Process, context::Context},
    resource, scheduler,
};

use super::user_ptr::{SyscallFuture, SyscallResult, UserAccess, UserPtr};

/// Handle environment open operation.
///
/// This syscall is async - if the underlying filesystem needs to do I/O,
/// the process will be blocked until the operation completes.
///
/// Arguments:
/// - uri_ptr, uri_len: URI of resource to open
/// - mailbox_handle: Handle of mailbox to attach to (0 = don't attach, use HANDLE_MAILBOX for default)
/// - event_mask: Events to listen for (0 = no events)
pub fn handle_open(
    ua: &UserAccess,
    uri_ptr: usize,
    uri_len: usize,
    mailbox_handle: usize,
    event_mask: usize,
) -> SyscallFuture {
    let mailbox_handle = mailbox_handle as u64;
    let event_mask = event_mask as u32;

    let uri = match ua.read_str(uri_ptr, uri_len) {
        Ok(u) => u,
        Err(_) => {
            return Box::pin(core::future::ready(SyscallResult::err(
                panda_abi::ErrorCode::InvalidArgument,
            )));
        }
    };

    debug!(
        "handle_open: uri={}, mailbox={}, event_mask={:#x}",
        uri, mailbox_handle, event_mask
    );

    Box::pin(async move {
        debug!("handle_open future: opening {}", uri);
        match resource::open(&uri).await {
            Some(resource) => {
                debug!("handle_open future: opened {} successfully", uri);
                let result = scheduler::with_current_process(|proc| {
                    let handle_id = proc.handles_mut().insert(Arc::from(resource)).ok()?;

                    // Attach to mailbox if requested
                    if mailbox_handle != 0 && event_mask != 0 {
                        if let Some(mailbox_h) = proc.handles().get(mailbox_handle) {
                            if let Some(mailbox) = mailbox_h.as_mailbox() {
                                mailbox.attach(handle_id, event_mask);

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

                    Some(handle_id as isize)
                });
                match result {
                    Some(handle_id) => {
                        info!("handle_open future: returning handle_id={}", handle_id);
                        SyscallResult::ok(handle_id)
                    }
                    None => {
                        info!("handle_open future: handle limit reached for {}", uri);
                        SyscallResult::err(panda_abi::ErrorCode::TooManyHandles)
                    }
                }
            }
            None => {
                info!("handle_open future: failed to open {}", uri);
                SyscallResult::err(panda_abi::ErrorCode::NotFound)
            }
        }
    })
}

/// Handle environment mount operation.
///
/// This syscall is async - mounting a filesystem requires reading from disk.
///
/// Arguments:
/// - fstype_ptr, fstype_len: Filesystem type string (e.g., "ext2")
/// - mountpoint_ptr, mountpoint_len: Mount point path (e.g., "/mnt")
pub fn handle_mount(
    ua: &UserAccess,
    fstype_ptr: usize,
    fstype_len: usize,
    mountpoint_ptr: usize,
    mountpoint_len: usize,
) -> SyscallFuture {
    let fstype = match ua.read_str(fstype_ptr, fstype_len) {
        Ok(s) => s,
        Err(_) => {
            return Box::pin(core::future::ready(SyscallResult::err(
                panda_abi::ErrorCode::InvalidArgument,
            )));
        }
    };

    let mountpoint = match ua.read_str(mountpoint_ptr, mountpoint_len) {
        Ok(s) => s,
        Err(_) => {
            return Box::pin(core::future::ready(SyscallResult::err(
                panda_abi::ErrorCode::InvalidArgument,
            )));
        }
    };

    info!("handle_mount: fstype={}, mountpoint={}", fstype, mountpoint);

    Box::pin(async move {
        match fstype.as_str() {
            "ext2" => match crate::vfs::mount_ext2(&mountpoint).await {
                Ok(()) => {
                    info!("Mounted ext2 filesystem at {}", mountpoint);
                    SyscallResult::ok(0)
                }
                Err(e) => {
                    error!("Failed to mount ext2 at {}: {}", mountpoint, e);
                    SyscallResult::err(panda_abi::ErrorCode::IoError)
                }
            },
            _ => {
                error!("Unknown filesystem type: {}", fstype);
                SyscallResult::err(panda_abi::ErrorCode::NotSupported)
            }
        }
    })
}

/// Handle environment spawn operation.
///
/// This syscall is async - it needs to open and read the ELF file.
///
/// Arguments:
/// - params_ptr: Pointer to SpawnParams struct
///
/// Creates a channel between parent and child. Child receives its endpoint at HANDLE_PARENT.
/// Parent receives a SpawnHandle that combines channel + process info.
pub fn handle_spawn(ua: &UserAccess, params_ptr: UserPtr<panda_abi::SpawnParams>) -> SyscallFuture {
    // Read spawn parameters from userspace
    let params: panda_abi::SpawnParams = match ua.read_user(params_ptr) {
        Ok(p) => p,
        Err(_) => {
            return Box::pin(core::future::ready(SyscallResult::err(
                panda_abi::ErrorCode::InvalidArgument,
            )));
        }
    };

    let mailbox_handle = params.mailbox;
    let event_mask = params.event_mask;
    let stdin_handle = params.stdin;
    let stdout_handle = params.stdout;

    debug!(
        "SPAWN: path_ptr={:#x}, path_len={}, mailbox={}, event_mask={:#x}, stdin={}, stdout={}",
        params.path_ptr, params.path_len, mailbox_handle, event_mask, stdin_handle, stdout_handle
    );

    let uri = match ua.read_str(params.path_ptr, params.path_len) {
        Ok(u) => u,
        Err(_) => {
            return Box::pin(core::future::ready(SyscallResult::err(
                panda_abi::ErrorCode::InvalidArgument,
            )));
        }
    };

    debug!("SPAWN: uri={}", uri);

    // Get stdin/stdout resources from parent's handle table (if specified)
    let stdin_resource = if stdin_handle != 0 {
        scheduler::with_current_process(|proc| {
            proc.handles().get(stdin_handle).map(|h| h.resource_arc())
        })
    } else {
        None
    };
    let stdout_resource = if stdout_handle != 0 {
        scheduler::with_current_process(|proc| {
            proc.handles().get(stdout_handle).map(|h| h.resource_arc())
        })
    } else {
        None
    };

    Box::pin(async move {
        let Some(resource) = resource::open(&uri).await else {
            error!("SPAWN: failed to open {}", uri);
            return SyscallResult::err(panda_abi::ErrorCode::NotFound);
        };

        // Read the file via async VFS interface
        let Some(vfs_file) = resource.as_vfs_file() else {
            error!("SPAWN: {} is not a readable file", uri);
            return SyscallResult::err(panda_abi::ErrorCode::NotReadable);
        };

        let file_lock = vfs_file.file();
        let mut file = file_lock.lock();

        // Get file size via stat
        let stat = match file.stat().await {
            Ok(s) => s,
            Err(e) => {
                error!("SPAWN: failed to stat {}: {:?}", uri, e);
                return SyscallResult::err(panda_abi::ErrorCode::IoError);
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
                    error!("SPAWN: failed to read {}: {:?}", uri, e);
                    return SyscallResult::err(panda_abi::ErrorCode::IoError);
                }
            }
        }

        if total_read != size {
            error!("SPAWN: incomplete read: {} of {} bytes", total_read, size);
            return SyscallResult::err(panda_abi::ErrorCode::IoError);
        }

        let elf_data = elf_data.into_boxed_slice();
        let elf_ptr: *const [u8] = alloc::boxed::Box::leak(elf_data);

        let mut process = match Process::from_elf_data(Context::new_user_context(), elf_ptr) {
            Ok(p) => p,
            Err(e) => {
                error!("SPAWN: failed to create process from {}: {:?}", uri, e);
                return SyscallResult::err(panda_abi::ErrorCode::InvalidArgument);
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

        // Set up stdin/stdout if specified by parent
        if let Some(stdin_res) = stdin_resource {
            process
                .handles_mut()
                .insert_at(panda_abi::HANDLE_STDIN, stdin_res);
        }
        if let Some(stdout_res) = stdout_resource {
            process
                .handles_mut()
                .insert_at(panda_abi::HANDLE_STDOUT, stdout_res);
        }

        scheduler::add_process(process);

        // Create SpawnHandle combining channel and process info
        let spawn_handle = resource::SpawnHandle::new(parent_endpoint, process_info);

        let result = scheduler::with_current_process(|proc| {
            let handle_id = proc.handles_mut().insert(Arc::new(spawn_handle)).ok()?;

            // Attach to mailbox if requested
            if mailbox_handle != 0 && event_mask != 0 {
                if let Some(mailbox_h) = proc.handles().get(mailbox_handle) {
                    if let Some(mailbox) = mailbox_h.as_mailbox() {
                        mailbox.attach(handle_id, event_mask);

                        if let Some(spawn_h) = proc.handles().get(handle_id) {
                            let mailbox_ref = resource::MailboxRef::new(mailbox, handle_id);
                            spawn_h.attach_mailbox(mailbox_ref);
                        }
                    }
                }
            }

            Some(handle_id)
        });
        match result {
            Some(handle_id) => SyscallResult::ok(handle_id as isize),
            None => SyscallResult::err(panda_abi::ErrorCode::TooManyHandles),
        }
    })
}

/// Handle environment log operation.
pub fn handle_log(ua: &UserAccess, msg_ptr: usize, msg_len: usize) -> SyscallFuture {
    debug!("LOG: msg_ptr={:#x}, msg_len={}", msg_ptr, msg_len);
    let msg = match ua.read_str(msg_ptr, msg_len) {
        Ok(m) => m,
        Err(_) => {
            return Box::pin(core::future::ready(SyscallResult::err(
                panda_abi::ErrorCode::InvalidArgument,
            )));
        }
    };
    info!("LOG: {msg}");
    Box::pin(core::future::ready(SyscallResult::ok(0)))
}

/// Handle environment time operation.
pub fn handle_time() -> SyscallFuture {
    Box::pin(core::future::ready(SyscallResult::ok(0)))
}

/// Map a VFS `FsError` to a syscall error code (negative value).
fn fs_error_code(e: crate::vfs::FsError) -> panda_abi::ErrorCode {
    use crate::vfs::FsError;
    match e {
        FsError::NotFound => panda_abi::ErrorCode::NotFound,
        FsError::InvalidOffset => panda_abi::ErrorCode::InvalidOffset,
        FsError::NotReadable => panda_abi::ErrorCode::NotReadable,
        FsError::NotWritable => panda_abi::ErrorCode::NotWritable,
        FsError::NotSeekable => panda_abi::ErrorCode::NotSeekable,
        FsError::ReadOnlyFs => panda_abi::ErrorCode::NotSupported,
        FsError::NoSpace => panda_abi::ErrorCode::NoSpace,
        FsError::AlreadyExists => panda_abi::ErrorCode::AlreadyExists,
        FsError::NotEmpty => panda_abi::ErrorCode::NotEmpty,
        FsError::IsDirectory => panda_abi::ErrorCode::IsDirectory,
        FsError::NotDirectory => panda_abi::ErrorCode::NotDirectory,
        FsError::IoError => panda_abi::ErrorCode::IoError,
    }
}

/// Handle environment create operation.
///
/// This syscall is async — creating a file requires disk I/O.
///
/// Arguments:
/// - path_ptr, path_len: URI of file to create (e.g., "file:/mnt/newfile.txt")
/// - mode: File permissions (e.g., 0o644)
/// - mailbox_handle: Handle of mailbox to attach to (0 = don't attach)
pub fn handle_create(
    ua: &UserAccess,
    path_ptr: usize,
    path_len: usize,
    mode: usize,
    mailbox_handle: usize,
) -> SyscallFuture {
    let mailbox_handle = mailbox_handle as u64;
    let mode = mode as u16;

    let uri = match ua.read_str(path_ptr, path_len) {
        Ok(u) => u,
        Err(_) => {
            return Box::pin(core::future::ready(SyscallResult::err(
                panda_abi::ErrorCode::InvalidArgument,
            )));
        }
    };

    info!("handle_create: uri={}, mode={:#o}", uri, mode);

    Box::pin(async move {
        // Parse the URI — expect "file:/path"
        let path = match uri.strip_prefix("file:") {
            Some(p) => p,
            None => {
                error!("handle_create: unsupported URI scheme: {}", uri);
                return SyscallResult::err(panda_abi::ErrorCode::InvalidArgument);
            }
        };

        match crate::vfs::create(path, mode).await {
            Ok(file) => {
                let vfs_resource = resource::scheme::VfsFileResource::new(file);
                let result: Result<isize, panda_abi::ErrorCode> =
                    scheduler::with_current_process(|proc| {
                        let handle_id = proc
                            .handles_mut()
                            .insert(Arc::new(vfs_resource))
                            .map_err(|_| panda_abi::ErrorCode::TooManyHandles)?;

                        // Attach to mailbox if requested
                        if mailbox_handle != 0 {
                            if let Some(mailbox_h) = proc.handles().get(mailbox_handle) {
                                if let Some(mailbox) = mailbox_h.as_mailbox() {
                                    mailbox.attach(handle_id, 0);
                                }
                            }
                        }

                        Ok(handle_id as isize)
                    });
                match result {
                    Ok(handle_id) => {
                        info!("handle_create: created file, handle_id={}", handle_id);
                        SyscallResult::ok(handle_id)
                    }
                    Err(code) => SyscallResult::err(code),
                }
            }
            Err(e) => {
                info!("handle_create: failed: {:?}", e);
                SyscallResult::err(fs_error_code(e))
            }
        }
    })
}

/// Handle environment unlink operation.
///
/// This syscall is async — unlinking a file requires disk I/O.
///
/// Arguments:
/// - path_ptr, path_len: URI of file to unlink (e.g., "file:/mnt/file.txt")
pub fn handle_unlink(ua: &UserAccess, path_ptr: usize, path_len: usize) -> SyscallFuture {
    let uri = match ua.read_str(path_ptr, path_len) {
        Ok(u) => u,
        Err(_) => {
            return Box::pin(core::future::ready(SyscallResult::err(
                panda_abi::ErrorCode::InvalidArgument,
            )));
        }
    };

    info!("handle_unlink: uri={}", uri);

    Box::pin(async move {
        // Parse the URI — expect "file:/path"
        let path = match uri.strip_prefix("file:") {
            Some(p) => p,
            None => {
                error!("handle_unlink: unsupported URI scheme: {}", uri);
                return SyscallResult::err(panda_abi::ErrorCode::InvalidArgument);
            }
        };

        match crate::vfs::unlink(path).await {
            Ok(()) => {
                info!("handle_unlink: unlinked {}", path);
                SyscallResult::ok(0)
            }
            Err(e) => {
                info!("handle_unlink: failed: {:?}", e);
                SyscallResult::err(fs_error_code(e))
            }
        }
    })
}

/// Handle environment opendir operation.
///
/// This syscall is async - directory listing may require disk I/O.
pub fn handle_opendir(ua: &UserAccess, uri_ptr: usize, uri_len: usize) -> SyscallFuture {
    let uri = match ua.read_str(uri_ptr, uri_len) {
        Ok(u) => u,
        Err(_) => {
            return Box::pin(core::future::ready(SyscallResult::err(
                panda_abi::ErrorCode::InvalidArgument,
            )));
        }
    };

    Box::pin(async move {
        let Some(entries) = resource::readdir(&uri).await else {
            return SyscallResult::err(panda_abi::ErrorCode::NotFound);
        };

        let dir_resource = resource::DirectoryResource::new(entries);
        let result = scheduler::with_current_process(|proc| {
            proc.handles_mut().insert(Arc::new(dir_resource)).ok()
        });
        match result {
            Some(handle_id) => SyscallResult::ok(handle_id as isize),
            None => SyscallResult::err(panda_abi::ErrorCode::TooManyHandles),
        }
    })
}
