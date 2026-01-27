//! Process operations.
//!
//! This module provides both low-level process control operations and
//! high-level RAII wrappers.
//!
//! ## Low-level functions
//!
//! - [`yield_now()`] - Yield the CPU to other processes
//! - [`exit()`] - Exit the current process
//! - [`getpid()`] - Get the current process ID
//! - [`wait()`] - Wait for a child process to exit
//! - [`signal()`] - Send a signal to a process
//!
//! ## High-level types
//!
//! - [`Child`] - RAII wrapper for spawned child processes

mod child;

pub use child::{Child, ChildBuilder, ExitStatus};

use crate::Handle;
use crate::sys;

/// Yield the CPU to another process.
#[inline(always)]
pub fn yield_now() {
    sys::process::yield_now();
}

/// Exit the current process with the given exit code.
#[inline(always)]
pub fn exit(code: i32) -> ! {
    sys::process::exit(code);
}

/// Get the current process ID.
#[inline(always)]
pub fn getpid() -> u64 {
    sys::process::getpid()
}

/// Wait for a child process to exit.
///
/// Returns the exit code of the child, or negative error code.
#[inline(always)]
pub fn wait(child_handle: Handle) -> i32 {
    sys::process::wait(child_handle)
}

/// Send a signal to a process.
///
/// Returns 0 on success, or negative error code.
#[inline(always)]
pub fn signal(process_handle: Handle, sig: u32) -> isize {
    sys::process::signal(process_handle, sig)
}
