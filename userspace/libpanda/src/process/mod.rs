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
//! - [`sleep()`] - Sleep for a duration
//!
//! ## High-level types
//!
//! - [`Child`] - RAII wrapper for spawned child processes

mod child;

pub use child::{Child, ChildBuilder, ExitStatus, Signal};

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

/// Sleep for at least `duration_ms` milliseconds.
///
/// Returns 0 on success, or negative error code.
#[inline(always)]
pub fn sleep(duration_ms: u64) -> isize {
    sys::process::sleep(duration_ms)
}

/// How long [`wait_ready`] polls for a spawned driver's readiness message
/// before giving up. Also reused as the shared retry budget for
/// [`crate::environment::open_retrying`] — same 200 * 10ms = ~2s total that
/// `MOUNT_RETRY_ATTEMPTS`/`MOUNT_RETRY_SLEEP_MS`
/// (`panda-kernel/src/vfs/mod.rs`) and `DISPLAY_OPEN_RETRY_ATTEMPTS`/
/// `DISPLAY_OPEN_RETRY_SLEEP_MS` (`userspace/compositor/src/display.rs`)
/// use.
pub const READY_WAIT_ATTEMPTS: u32 = 200;
/// See [`READY_WAIT_ATTEMPTS`].
pub const READY_WAIT_SLEEP_MS: u64 = 10;

/// Block until a driver spawned via `environment::spawn` signals it's ready
/// (has registered its scheme), or the retry budget in
/// [`READY_WAIT_ATTEMPTS`]/[`READY_WAIT_SLEEP_MS`] is exhausted.
///
/// # Background
///
/// Spawning a driver (`init` spawning `virtio-blk` before mounting `ext2`
/// off `block:`, or `virtio-gpu` before spawning the compositor onto
/// `display:`) only starts the driver process — it doesn't block until the
/// driver has actually claimed its device and called `OP_SCHEME_REGISTER`,
/// so a dependant's first scheme-open attempt can lose that race under real
/// multi-device boot timing.
///
/// Every boot driver (`virtio-blk`, `virtio-gpu`, `virtio-keyboard`, ...)
/// sends a single `b"ready"` message on its spawn channel right after
/// registering its scheme (see e.g. the `main!` loop in
/// `userspace/drivers/virtio-blk/src/main.rs`) — nothing else is ever sent
/// unprompted on that channel in the child-to-parent direction (the only
/// other traffic on it is `init` asking the driver to shut down, which
/// flows parent-to-child). Calling this right after `environment::spawn`,
/// before doing anything that depends on the driver being ready, eliminates
/// the race outright: the dependant's very first scheme-open attempt
/// succeeds instead of merely "eventually"
/// succeeding within a retry budget.
///
/// The bounded retry here is defense-in-depth, not the mechanism that
/// closes the race: a driver whose device never appears (e.g. missing
/// hardware) must still fail boot rather than hang it forever.
///
/// `handle` must be a raw handle from `environment::spawn` (not a `Child` —
/// this borrows a channel view of it without taking ownership or closing
/// it on return).
pub fn wait_ready(handle: Handle) -> crate::error::Result<()> {
    use crate::ipc::Channel;
    use panda_abi::ErrorCode;

    let channel = Channel::from_handle_borrowed(handle).ok_or(ErrorCode::InvalidHandle)?;
    let mut buf = [0u8; 16];
    let mut attempt = 0;
    loop {
        match channel.try_recv(&mut buf) {
            Ok(Some(_)) => return Ok(()),
            Ok(None) if attempt < READY_WAIT_ATTEMPTS => {
                attempt += 1;
                sleep(READY_WAIT_SLEEP_MS);
            }
            Ok(None) => return Err(ErrorCode::IoError),
            Err(e) => return Err(e),
        }
    }
}
