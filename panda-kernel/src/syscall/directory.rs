//! Directory operation syscall handlers (OP_DIRECTORY_*).

#![deny(unsafe_code)]

use alloc::boxed::Box;
use alloc::string::String;
use alloc::sync::Arc;

use log::{debug, error};

use crate::{resource, scheduler};

use super::helpers::{attach_to_mailbox, read_user_str};
use super::user_ptr::{SyscallFuture, SyscallResult, UserAccess};

/// Read the entry name from userspace and resolve it to a full URI within
/// the directory handle's directory.
///
/// Shared prelude for the four directory-mutating ops (create, unlink,
/// mkdir, rmdir): read `name` from userspace, resolve `handle_id`'s URI via
/// `as_directory()`, then join `dir_uri + "/" + name`. `op` is used only for
/// log messages, to keep them identifiable per-caller.
///
/// The result is a full URI (e.g. `"file:/mnt/newfile.txt"`) rather than a
/// bare path because roadmap M5 moved the filesystem out of the kernel:
/// these operations are now dispatched through the scheme registry to
/// whichever provider owns the scheme (see `resource::create` and friends),
/// instead of calling `vfs::create` directly.
fn resolve_dir_op_uri(
    ua: &UserAccess,
    handle_id: u64,
    name_ptr: usize,
    name_len: usize,
    op: &str,
) -> Result<String, SyscallFuture> {
    let name = read_user_str(ua, name_ptr, name_len)?;

    // Get the URI from the directory handle
    let dir_uri: Option<String> = scheduler::with_current_process(|proc| {
        proc.handles()
            .get(handle_id)
            .and_then(|h| h.as_directory())
            .and_then(|d| d.uri())
    });

    let Some(dir_uri) = dir_uri else {
        error!("{op}: handle {handle_id} is not a mutable directory");
        return Err(Box::pin(core::future::ready(SyscallResult::err(
            panda_abi::ErrorCode::InvalidHandle,
        ))));
    };

    debug!("{op}: dir={dir_uri}, name={name}");

    // Build the full URI. A `dir_uri` whose path part is empty or "/" (e.g.
    // "file:/") must not produce a doubled slash.
    Ok(if dir_uri.ends_with('/') {
        alloc::format!("{}{}", dir_uri, name)
    } else {
        alloc::format!("{}/{}", dir_uri, name)
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

    let full_uri = match resolve_dir_op_uri(ua, handle_id, name_ptr, name_len, "handle_create") {
        Ok(p) => p,
        Err(e) => return e,
    };

    debug!("handle_create: uri={}, mode={:#o}", full_uri, mode);

    Box::pin(async move {
        match resource::create(&full_uri, mode).await {
            Ok(file) => {
                let result = scheduler::with_current_process(|proc| {
                    let handle_id = proc
                        .handles_mut()
                        .insert(Arc::from(file))
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
                SyscallResult::err(e)
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
    let full_uri =
        match resolve_dir_op_uri(ua, handle_id, name_ptr, name_len, "handle_unlink") {
            Ok(p) => p,
            Err(e) => return e,
        };

    Box::pin(async move {
        match resource::unlink(&full_uri).await {
            Ok(()) => {
                debug!("handle_unlink: unlinked {}", full_uri);
                SyscallResult::ok(0)
            }
            Err(e) => {
                error!("handle_unlink: failed: {:?}", e);
                SyscallResult::err(e)
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

    let full_uri = match resolve_dir_op_uri(ua, handle_id, name_ptr, name_len, "handle_mkdir") {
        Ok(p) => p,
        Err(e) => return e,
    };

    debug!("handle_mkdir: path={}, mode={:#o}", full_uri, mode);

    Box::pin(async move {
        match resource::mkdir(&full_uri, mode).await {
            Ok(()) => {
                debug!("handle_mkdir: created directory {}", full_uri);
                SyscallResult::ok(0)
            }
            Err(e) => {
                error!("handle_mkdir: failed: {:?}", e);
                SyscallResult::err(e)
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
    let full_uri = match resolve_dir_op_uri(ua, handle_id, name_ptr, name_len, "handle_rmdir") {
        Ok(p) => p,
        Err(e) => return e,
    };

    Box::pin(async move {
        match resource::rmdir(&full_uri).await {
            Ok(()) => {
                debug!("handle_rmdir: removed directory {}", full_uri);
                SyscallResult::ok(0)
            }
            Err(e) => {
                error!("handle_rmdir: failed: {:?}", e);
                SyscallResult::err(e)
            }
        }
    })
}
