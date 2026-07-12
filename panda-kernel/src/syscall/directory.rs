//! Directory operation syscall handlers (OP_DIRECTORY_*).

#![deny(unsafe_code)]

use alloc::boxed::Box;
use alloc::string::String;
use alloc::sync::Arc;

use log::{debug, error};

use crate::{resource, scheduler};

use super::helpers::{attach_to_mailbox, read_user_str};
use super::user_ptr::{SyscallFuture, SyscallResult, UserAccess};

use super::environment::fs_error_code;

/// Read the entry name from userspace and resolve it to a full VFS path within the
/// directory handle's directory.
///
/// Shared prelude for the four directory-mutating ops (create, unlink, mkdir, rmdir):
/// read `name` from userspace, resolve `handle_id`'s VFS path via `as_directory()`, then
/// join `dir_path + "/" + name` (treating an empty or root `dir_path` as `"/"`).
/// `op` is used only for log messages, to keep them identifiable per-caller.
fn resolve_dir_op_path(
    ua: &UserAccess,
    handle_id: u64,
    name_ptr: usize,
    name_len: usize,
    op: &str,
) -> Result<String, SyscallFuture> {
    let name = read_user_str(ua, name_ptr, name_len)?;

    // Get the VFS path from the directory handle
    let dir_path: Option<String> = scheduler::with_current_process(|proc| {
        proc.handles()
            .get(handle_id)
            .and_then(|h| h.as_directory())
            .and_then(|d| d.vfs_path())
    });

    let Some(dir_path) = dir_path else {
        error!("{op}: handle {handle_id} is not a VFS directory");
        return Err(Box::pin(core::future::ready(SyscallResult::err(
            panda_abi::ErrorCode::InvalidHandle,
        ))));
    };

    debug!("{op}: dir={dir_path}, name={name}");

    // Build the full path: dir_path + "/" + name
    Ok(if dir_path.is_empty() || dir_path == "/" {
        alloc::format!("/{}", name)
    } else {
        alloc::format!("{}/{}", dir_path, name)
    })
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

    let full_path =
        match resolve_dir_op_path(ua, handle_id, name_ptr, name_len, "handle_create") {
            Ok(p) => p,
            Err(e) => return e,
        };

    debug!("handle_create: path={}, mode={:#o}", full_path, mode);

    Box::pin(async move {
        match crate::vfs::create(&full_path, mode).await {
            Ok(file) => {
                let vfs_resource = resource::scheme::VfsFileResource::new(file);
                let result = scheduler::with_current_process(|proc| {
                    let handle_id = proc
                        .handles_mut()
                        .insert(Arc::new(vfs_resource))
                        .map_err(|_| panda_abi::ErrorCode::TooManyHandles)?;

                    // Attach to mailbox if requested
                    attach_to_mailbox(proc, mailbox_handle, handle_id, 0);

                    Ok(handle_id as isize)
                });
                match result {
                    Ok(handle_id) => {
                        debug!("handle_create: created file, handle_id={}", handle_id);
                        SyscallResult::ok(handle_id)
                    }
                    Err(e) => SyscallResult::err(e),
                }
            }
            Err(e) => {
                error!("handle_create: failed: {:?}", e);
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
    let full_path =
        match resolve_dir_op_path(ua, handle_id, name_ptr, name_len, "handle_unlink") {
            Ok(p) => p,
            Err(e) => return e,
        };

    Box::pin(async move {
        match crate::vfs::unlink(&full_path).await {
            Ok(()) => {
                debug!("handle_unlink: unlinked {}", full_path);
                SyscallResult::ok(0)
            }
            Err(e) => {
                error!("handle_unlink: failed: {:?}", e);
                SyscallResult::err(fs_error_code(e))
            }
        }
    })
}

/// Handle directory mkdir operation.
///
/// This syscall is async — creating a directory requires disk I/O.
/// The operation is sent to a directory handle, so the new subdirectory
/// is created within that directory.
///
/// Arguments:
/// - handle_id: Directory handle
/// - name_ptr, name_len: Name of the directory to create (just the name, not a full path)
/// - mode: Directory permissions (e.g., 0o755)
pub fn handle_mkdir(
    ua: &UserAccess,
    handle_id: u64,
    name_ptr: usize,
    name_len: usize,
    mode: usize,
) -> SyscallFuture {
    let mode = mode as u16;

    let full_path = match resolve_dir_op_path(ua, handle_id, name_ptr, name_len, "handle_mkdir") {
        Ok(p) => p,
        Err(e) => return e,
    };

    debug!("handle_mkdir: path={}, mode={:#o}", full_path, mode);

    Box::pin(async move {
        match crate::vfs::mkdir(&full_path, mode).await {
            Ok(()) => {
                debug!("handle_mkdir: created directory {}", full_path);
                SyscallResult::ok(0)
            }
            Err(e) => {
                error!("handle_mkdir: failed: {:?}", e);
                SyscallResult::err(fs_error_code(e))
            }
        }
    })
}

/// Handle directory rmdir operation.
///
/// This syscall is async — removing a directory requires disk I/O.
/// The operation is sent to a directory handle, so the subdirectory
/// is removed from that directory.
///
/// Arguments:
/// - handle_id: Directory handle
/// - name_ptr, name_len: Name of the directory to remove (just the name, not a full path)
pub fn handle_rmdir(
    ua: &UserAccess,
    handle_id: u64,
    name_ptr: usize,
    name_len: usize,
) -> SyscallFuture {
    let full_path = match resolve_dir_op_path(ua, handle_id, name_ptr, name_len, "handle_rmdir") {
        Ok(p) => p,
        Err(e) => return e,
    };

    Box::pin(async move {
        match crate::vfs::rmdir(&full_path).await {
            Ok(()) => {
                debug!("handle_rmdir: removed directory {}", full_path);
                SyscallResult::ok(0)
            }
            Err(e) => {
                error!("handle_rmdir: failed: {:?}", e);
                SyscallResult::err(fs_error_code(e))
            }
        }
    })
}
