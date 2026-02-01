//! Directory operation syscall handlers (OP_DIRECTORY_*).

#![deny(unsafe_code)]

use alloc::boxed::Box;
use alloc::sync::Arc;

use log::{error, info};

use crate::{resource, scheduler};

use super::user_ptr::{SyscallFuture, SyscallResult, UserAccess};

/// Map a VFS `FsError` to a syscall error code (negative value).
///
/// These codes match the `ErrorCode` enum in `panda_abi` so that userspace
/// can decode them correctly.
pub(crate) fn fs_error_code(e: crate::vfs::FsError) -> isize {
    use crate::vfs::FsError;
    match e {
        FsError::NotFound => -1,
        FsError::InvalidOffset => -2,
        FsError::NotReadable => -3,
        FsError::NotWritable => -4,
        FsError::NotSeekable => -5,
        FsError::AlreadyExists => -12,
        FsError::NoSpace => -13,
        FsError::ReadOnlyFs => -14,
        FsError::NotEmpty => -10,
        FsError::IsDirectory => -10,
        FsError::NotDirectory => -10,
        FsError::IoError => -8,
    }
}

/// Handle directory create operation.
///
/// This syscall is async — creating a file requires disk I/O.
/// The operation is sent to a directory handle (opened via `EnvironmentOpendir`),
/// so the file is created within that directory.
///
/// Arguments:
/// - handle_id: Directory handle
/// - name_ptr, name_len: Name of the file to create (just the name, not a full path)
/// - mode: File permissions (e.g., 0o644)
/// - mailbox_handle: Handle of mailbox to attach to (0 = don't attach)
pub fn handle_create(
    ua: &UserAccess,
    handle_id: u64,
    name_ptr: usize,
    name_len: usize,
    mode: usize,
    mailbox_handle: usize,
) -> SyscallFuture {
    let mailbox_handle = mailbox_handle as u64;
    let mode = mode as u16;

    let name = match ua.read_str(name_ptr, name_len) {
        Ok(n) => n,
        Err(_) => return Box::pin(core::future::ready(SyscallResult::err(-1))),
    };

    // Get the VFS path from the directory handle
    let dir_path: Option<alloc::string::String> = scheduler::with_current_process(|proc| {
        proc.handles()
            .get(handle_id)
            .and_then(|h| h.as_vfs_directory_path())
    });

    let Some(dir_path) = dir_path else {
        error!("handle_create: handle {} is not a VFS directory", handle_id);
        return Box::pin(core::future::ready(SyscallResult::err(-1)));
    };

    info!(
        "handle_create: dir={}, name={}, mode={:#o}",
        dir_path, name, mode
    );

    Box::pin(async move {
        // Build the full path: dir_path + "/" + name
        let full_path = if dir_path.is_empty() || dir_path == "/" {
            alloc::format!("/{}", name)
        } else {
            alloc::format!("{}/{}", dir_path, name)
        };

        match crate::vfs::create(&full_path, mode).await {
            Ok(file) => {
                let vfs_resource = resource::scheme::VfsFileResource::new(file);
                let handle_id = scheduler::with_current_process(|proc| {
                    let handle_id = proc.handles_mut().insert(Arc::new(vfs_resource));

                    // Attach to mailbox if requested
                    if mailbox_handle != 0 {
                        if let Some(mailbox_h) = proc.handles().get(mailbox_handle) {
                            if let Some(mailbox) = mailbox_h.as_mailbox() {
                                mailbox.attach(handle_id, 0);
                            }
                        }
                    }

                    handle_id as isize
                });
                info!("handle_create: created file, handle_id={}", handle_id);
                SyscallResult::ok(handle_id)
            }
            Err(e) => {
                info!("handle_create: failed: {:?}", e);
                SyscallResult::err(fs_error_code(e))
            }
        }
    })
}

/// Handle directory unlink operation.
///
/// This syscall is async — unlinking a file requires disk I/O.
/// The operation is sent to a directory handle, so the file is unlinked
/// within that directory.
///
/// Arguments:
/// - handle_id: Directory handle
/// - name_ptr, name_len: Name of the file to unlink (just the name, not a full path)
pub fn handle_unlink(
    ua: &UserAccess,
    handle_id: u64,
    name_ptr: usize,
    name_len: usize,
) -> SyscallFuture {
    let name = match ua.read_str(name_ptr, name_len) {
        Ok(n) => n,
        Err(_) => return Box::pin(core::future::ready(SyscallResult::err(-1))),
    };

    // Get the VFS path from the directory handle
    let dir_path: Option<alloc::string::String> = scheduler::with_current_process(|proc| {
        proc.handles()
            .get(handle_id)
            .and_then(|h| h.as_vfs_directory_path())
    });

    let Some(dir_path) = dir_path else {
        error!(
            "handle_unlink: handle {} is not a VFS directory",
            handle_id
        );
        return Box::pin(core::future::ready(SyscallResult::err(-1)));
    };

    info!("handle_unlink: dir={}, name={}", dir_path, name);

    Box::pin(async move {
        // Build the full path: dir_path + "/" + name
        let full_path = if dir_path.is_empty() || dir_path == "/" {
            alloc::format!("/{}", name)
        } else {
            alloc::format!("{}/{}", dir_path, name)
        };

        match crate::vfs::unlink(&full_path).await {
            Ok(()) => {
                info!("handle_unlink: unlinked {}", full_path);
                SyscallResult::ok(0)
            }
            Err(e) => {
                info!("handle_unlink: failed: {:?}", e);
                SyscallResult::err(fs_error_code(e))
            }
        }
    })
}
