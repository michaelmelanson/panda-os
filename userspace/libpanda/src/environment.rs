//! Environment operations using the send-based API
//!
//! The environment handle provides access to system-level operations
//! like opening files, spawning processes, and logging.

use crate::handle::Handle;
use crate::syscall::send;
use panda_abi::*;

/// Open a file by path
///
/// Returns a file handle on success, or error code
#[inline(always)]
pub fn open(path: &str, flags: u32) -> Result<Handle, isize> {
    let result = send(
        Handle::ENVIRONMENT,
        OP_ENVIRONMENT_OPEN,
        path.as_ptr() as usize,
        path.len(),
        flags as usize,
        0,
    );
    if result < 0 {
        Err(result)
    } else {
        Ok(Handle::from(result as u32))
    }
}

/// Spawn a new process from an executable path
///
/// Returns a process handle on success, or error code
#[inline(always)]
pub fn spawn(path: &str) -> Result<Handle, isize> {
    let result = send(
        Handle::ENVIRONMENT,
        OP_ENVIRONMENT_SPAWN,
        path.as_ptr() as usize,
        path.len(),
        0,
        0,
    );
    if result < 0 {
        Err(result)
    } else {
        Ok(Handle::from(result as u32))
    }
}

/// Log a message to the system console
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

/// Get the current system time
///
/// Returns a timestamp, or negative error code
#[inline(always)]
pub fn time() -> isize {
    send(Handle::ENVIRONMENT, OP_ENVIRONMENT_TIME, 0, 0, 0, 0)
}

/// Open a directory for iteration
///
/// Returns a directory handle on success, or error code
#[inline(always)]
pub fn opendir(path: &str) -> Result<Handle, isize> {
    let result = send(
        Handle::ENVIRONMENT,
        OP_ENVIRONMENT_OPENDIR,
        path.as_ptr() as usize,
        path.len(),
        0,
        0,
    );
    if result < 0 {
        Err(result)
    } else {
        Ok(Handle::from(result as u32))
    }
}
