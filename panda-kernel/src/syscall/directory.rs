//! Directory operation syscall handlers (OP_DIRECTORY_*).

#![deny(unsafe_code)]

use alloc::boxed::Box;
use alloc::sync::Arc;

use log::{debug, error};

use crate::{resource, scheduler};

use super::user_ptr::{SyscallFuture, SyscallResult, UserAccess};

use super::environment::fs_error_code;

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
        Err(_) => {
            return Box::pin(core::future::ready(SyscallResult::err(
                panda_abi::ErrorCode::InvalidArgument,
            )));
        }
    };

    // Get the VFS path from the directory handle
    let dir_path: Option<alloc::string::String> = scheduler::with_current_process(|proc| {
        proc.handles()
            .get(handle_id)
            .and_then(|h| h.as_vfs_directory_path())
    });

    let Some(dir_path) = dir_path else {
        error!("handle_create: handle {} is not a VFS directory", handle_id);
        return Box::pin(core::future::ready(SyscallResult::err(
            panda_abi::ErrorCode::InvalidHandle,
        )));
    };

    debug!(
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
                let result = scheduler::with_current_process(|proc| {
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
    let name = match ua.read_str(name_ptr, name_len) {
        Ok(n) => n,
        Err(_) => {
            return Box::pin(core::future::ready(SyscallResult::err(
                panda_abi::ErrorCode::InvalidArgument,
            )));
        }
    };

    // Get the VFS path from the directory handle
    let dir_path: Option<alloc::string::String> = scheduler::with_current_process(|proc| {
        proc.handles()
            .get(handle_id)
            .and_then(|h| h.as_vfs_directory_path())
    });

    let Some(dir_path) = dir_path else {
        error!("handle_unlink: handle {} is not a VFS directory", handle_id);
        return Box::pin(core::future::ready(SyscallResult::err(
            panda_abi::ErrorCode::InvalidHandle,
        )));
    };

    debug!("handle_unlink: dir={}, name={}", dir_path, name);

    Box::pin(async move {
        // Build the full path: dir_path + "/" + name
        let full_path = if dir_path.is_empty() || dir_path == "/" {
            alloc::format!("/{}", name)
        } else {
            alloc::format!("{}/{}", dir_path, name)
        };

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

    let name = match ua.read_str(name_ptr, name_len) {
        Ok(n) => n,
        Err(_) => {
            return Box::pin(core::future::ready(SyscallResult::err(
                panda_abi::ErrorCode::InvalidArgument,
            )));
        }
    };

    // Get the VFS path from the directory handle
    let dir_path: Option<alloc::string::String> = scheduler::with_current_process(|proc| {
        proc.handles()
            .get(handle_id)
            .and_then(|h| h.as_vfs_directory_path())
    });

    let Some(dir_path) = dir_path else {
        error!("handle_mkdir: handle {} is not a VFS directory", handle_id);
        return Box::pin(core::future::ready(SyscallResult::err(
            panda_abi::ErrorCode::InvalidHandle,
        )));
    };

    debug!(
        "handle_mkdir: dir={}, name={}, mode={:#o}",
        dir_path, name, mode
    );

    Box::pin(async move {
        // Build the full path: dir_path + "/" + name
        let full_path = if dir_path.is_empty() || dir_path == "/" {
            alloc::format!("/{}", name)
        } else {
            alloc::format!("{}/{}", dir_path, name)
        };

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
    let name = match ua.read_str(name_ptr, name_len) {
        Ok(n) => n,
        Err(_) => {
            return Box::pin(core::future::ready(SyscallResult::err(
                panda_abi::ErrorCode::InvalidArgument,
            )));
        }
    };

    // Get the VFS path from the directory handle
    let dir_path: Option<alloc::string::String> = scheduler::with_current_process(|proc| {
        proc.handles()
            .get(handle_id)
            .and_then(|h| h.as_vfs_directory_path())
    });

    let Some(dir_path) = dir_path else {
        error!("handle_rmdir: handle {} is not a VFS directory", handle_id);
        return Box::pin(core::future::ready(SyscallResult::err(
            panda_abi::ErrorCode::InvalidHandle,
        )));
    };

    debug!("handle_rmdir: dir={}, name={}", dir_path, name);

    Box::pin(async move {
        // Build the full path: dir_path + "/" + name
        let full_path = if dir_path.is_empty() || dir_path == "/" {
            alloc::format!("/{}", name)
        } else {
            alloc::format!("{}/{}", dir_path, name)
        };

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
