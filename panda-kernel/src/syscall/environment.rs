//! Environment operation syscall handlers (OP_ENVIRONMENT_*).

#![deny(unsafe_code)]

use alloc::boxed::Box;
use alloc::sync::Arc;

use log::{debug, error, info};

use crate::{
    process::{Process, context::Context},
    resource, scheduler,
};

use super::helpers::{attach_to_mailbox, complete_mailbox_attach, read_user_str};
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

    let uri = match read_user_str(ua, uri_ptr, uri_len) {
        Ok(u) => u,
        Err(e) => return e,
    };

    debug!(
        "handle_open: uri={}, mailbox={}, event_mask={:#x}",
        uri, mailbox_handle, event_mask
    );

    Box::pin(async move {
        debug!("handle_open future: opening {}", uri);
        match resource::open(&uri).await {
            Ok(resource) => {
                debug!("handle_open future: opened {} successfully", uri);
                let result = scheduler::with_current_process(|proc| {
                    let handle_id = proc.handles_mut().insert(Arc::from(resource)).ok()?;

                    // Attach to mailbox if requested
                    if event_mask != 0 {
                        if let Some(mailbox) =
                            attach_to_mailbox(proc, mailbox_handle, handle_id, event_mask)
                        {
                            complete_mailbox_attach(proc, mailbox, handle_id);
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
            Err(resource::OpenError::NotFound) => {
                info!("handle_open future: failed to open {}", uri);
                SyscallResult::err(panda_abi::ErrorCode::NotFound)
            }
            Err(resource::OpenError::Busy) => {
                info!("handle_open future: {} is exclusively claimed", uri);
                SyscallResult::err(panda_abi::ErrorCode::Busy)
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
    let fstype = match read_user_str(ua, fstype_ptr, fstype_len) {
        Ok(s) => s,
        Err(e) => return e,
    };

    let mountpoint = match read_user_str(ua, mountpoint_ptr, mountpoint_len) {
        Ok(s) => s,
        Err(e) => return e,
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

    let uri = match read_user_str(ua, params.path_ptr, params.path_len) {
        Ok(u) => u,
        Err(e) => return e,
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
        // Read the binary via the shared resource-loading path (also used to
        // load the first process at boot), so the underlying filesystem is
        // only ever parsed through this one route.
        let elf_ptr = match resource::load_binary(&uri).await {
            Ok(ptr) => ptr,
            Err(resource::LoadBinaryError::NotFound) => {
                error!("SPAWN: failed to open {}", uri);
                return SyscallResult::err(panda_abi::ErrorCode::NotFound);
            }
            Err(resource::LoadBinaryError::NotReadable) => {
                error!("SPAWN: {} is not a readable file", uri);
                return SyscallResult::err(panda_abi::ErrorCode::NotReadable);
            }
            Err(resource::LoadBinaryError::IoError) => {
                error!("SPAWN: failed to read {}", uri);
                return SyscallResult::err(panda_abi::ErrorCode::IoError);
            }
        };

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
            if event_mask != 0 {
                if let Some(mailbox) =
                    attach_to_mailbox(proc, mailbox_handle, handle_id, event_mask)
                {
                    complete_mailbox_attach(proc, mailbox, handle_id);
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
    let msg = match read_user_str(ua, msg_ptr, msg_len) {
        Ok(m) => m,
        Err(e) => return e,
    };
    info!("LOG: {msg}");
    Box::pin(core::future::ready(SyscallResult::ok(0)))
}

/// Handle environment time operation.
///
/// Returns the system uptime in milliseconds, the same time source used by
/// `OP_PROCESS_SLEEP` (`crate::time::uptime_ms()`), so userspace can measure
/// elapsed time around a sleep.
pub fn handle_time() -> SyscallFuture {
    let uptime = crate::time::uptime_ms() as isize;
    Box::pin(core::future::ready(SyscallResult::ok(uptime)))
}

/// Map a VFS `FsError` to an `ErrorCode`.
pub(super) fn fs_error_code(e: crate::vfs::FsError) -> panda_abi::ErrorCode {
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

/// Handle environment opendir operation.
///
/// This syscall is async - directory listing may require disk I/O.
/// For `file:` URIs, the returned directory handle supports create/unlink
/// operations via the VFS path.
pub fn handle_opendir(ua: &UserAccess, uri_ptr: usize, uri_len: usize) -> SyscallFuture {
    let uri = match read_user_str(ua, uri_ptr, uri_len) {
        Ok(u) => u,
        Err(e) => return e,
    };

    Box::pin(async move {
        let Some(entries) = resource::readdir(&uri).await else {
            return SyscallResult::err(panda_abi::ErrorCode::NotFound);
        };

        // For file: URIs, store the VFS path so create/unlink can use it
        let dir_resource = if let Some(vfs_path) = uri.strip_prefix("file:") {
            resource::DirectoryResource::with_vfs_path(
                entries,
                alloc::string::String::from(vfs_path),
            )
        } else {
            resource::DirectoryResource::new(entries)
        };
        let result = scheduler::with_current_process(|proc| {
            proc.handles_mut().insert(Arc::new(dir_resource)).ok()
        });
        match result {
            Some(handle_id) => SyscallResult::ok(handle_id as isize),
            None => SyscallResult::err(panda_abi::ErrorCode::TooManyHandles),
        }
    })
}
