//! Environment operations using the send-based API
//!
//! The environment handle provides access to system-level operations
//! like opening files, spawning processes, and logging.

use crate::syscall::send;
use panda_abi::*;

/// Open a file by path
///
/// Returns a file handle on success, or negative error code
#[inline(always)]
pub fn open(path: &str, flags: u32) -> isize {
    send(
        HANDLE_ENVIRONMENT,
        OP_ENVIRONMENT_OPEN,
        path.as_ptr() as usize,
        path.len(),
        flags as usize,
        0,
    )
}

/// Spawn a new process from an executable path
///
/// Returns a process handle on success, or negative error code
#[inline(always)]
pub fn spawn(path: &str) -> isize {
    send(
        HANDLE_ENVIRONMENT,
        OP_ENVIRONMENT_SPAWN,
        path.as_ptr() as usize,
        path.len(),
        0,
        0,
    )
}

/// Log a message to the system console
#[inline(always)]
pub fn log(msg: &str) {
    let _ = send(
        HANDLE_ENVIRONMENT,
        OP_ENVIRONMENT_LOG,
        msg.as_ptr() as usize,
        msg.len(),
        0,
        0,
    );
}

/// Get the current system time
///
/// Returns a timestamp, or negative error code
#[inline(always)]
pub fn time() -> isize {
    send(HANDLE_ENVIRONMENT, OP_ENVIRONMENT_TIME, 0, 0, 0, 0)
}
