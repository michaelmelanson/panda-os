//! Low-level environment operations.
//!
//! These functions provide direct syscall access for system-level operations
//! like opening files, spawning processes, and logging.

use super::{Handle, send};
use panda_abi::*;

/// Open a file by path.
///
/// Returns file handle on success, or negative error code.
///
/// To attach the handle to a mailbox for event notifications, pass the
/// mailbox handle and event mask. Pass `(0, 0)` for no mailbox attachment.
#[inline(always)]
pub fn open(path: &str, mailbox: u64, event_mask: u32) -> isize {
    send(
        Handle::ENVIRONMENT,
        OP_ENVIRONMENT_OPEN,
        path.as_ptr() as usize,
        path.len(),
        mailbox as usize,
        event_mask as usize,
    )
}

/// Spawn a new process from an executable path.
///
/// Returns process handle on success, or negative error code.
///
/// To attach the handle to a mailbox for event notifications, pass the
/// mailbox handle and event mask. Pass `(0, 0)` for no mailbox attachment.
///
/// To redirect child's stdin/stdout, pass the handle values. Pass 0 for default
/// behavior (uses HANDLE_PARENT for both).
///
/// Note: This is the raw spawn syscall. Use `crate::environment::spawn` for
/// the higher-level version that also sends startup arguments.
#[inline(always)]
pub fn spawn(path: &str, mailbox: u64, event_mask: u32, stdin: u64, stdout: u64) -> isize {
    let params = SpawnParams {
        path_ptr: path.as_ptr() as usize,
        path_len: path.len(),
        mailbox,
        event_mask,
        _pad: 0,
        stdin,
        stdout,
    };
    send(
        Handle::ENVIRONMENT,
        OP_ENVIRONMENT_SPAWN,
        &params as *const SpawnParams as usize,
        0,
        0,
        0,
    )
}

/// Log a message to the system console.
#[inline(always)]
pub fn log(msg: &str) {
    let _ = send(
        Handle::ENVIRONMENT,
        OP_ENVIRONMENT_LOG,
        msg.as_ptr() as usize,
        msg.len(),
        0,
        0,
    );
}

/// Get the current system time.
///
/// Returns a timestamp, or negative error code.
#[inline(always)]
pub fn time() -> isize {
    send(Handle::ENVIRONMENT, OP_ENVIRONMENT_TIME, 0, 0, 0, 0)
}

/// Open a directory for iteration.
///
/// Returns directory handle on success, or negative error code.
#[inline(always)]
pub fn opendir(path: &str) -> isize {
    send(
        Handle::ENVIRONMENT,
        OP_ENVIRONMENT_OPENDIR,
        path.as_ptr() as usize,
        path.len(),
        0,
        0,
    )
}

/// Mount a filesystem.
///
/// Returns 0 on success, or negative error code.
#[inline(always)]
pub fn mount(fstype: &str, mountpoint: &str) -> isize {
    send(
        Handle::ENVIRONMENT,
        OP_ENVIRONMENT_MOUNT,
        fstype.as_ptr() as usize,
        fstype.len(),
        mountpoint.as_ptr() as usize,
        mountpoint.len(),
    )
}

/// Create a new file in a directory.
///
/// Returns file handle on success, or negative error code.
/// The `dir_handle` must be a directory handle opened via `opendir`.
/// `name` is just the filename (not a full path).
#[inline(always)]
pub fn dir_create(dir_handle: u64, name: &str, mode: u16, mailbox: u64) -> isize {
    send(
        dir_handle,
        OP_DIRECTORY_CREATE,
        name.as_ptr() as usize,
        name.len(),
        mode as usize,
        mailbox as usize,
    )
}

/// Unlink (delete) a file from a directory.
///
/// Returns 0 on success, or negative error code.
/// The `dir_handle` must be a directory handle opened via `opendir`.
/// `name` is just the filename (not a full path).
#[inline(always)]
pub fn dir_unlink(dir_handle: u64, name: &str) -> isize {
    send(
        dir_handle,
        OP_DIRECTORY_UNLINK,
        name.as_ptr() as usize,
        name.len(),
        0,
        0,
    )
}
