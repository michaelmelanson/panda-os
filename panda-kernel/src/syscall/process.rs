//! Process operation syscall handlers (OP_PROCESS_*).
//!
//! Diverging operations (yield, exit) are handled directly in `mod.rs` since
//! they require unsafe scheduler calls. This module only contains safe handlers.

#![deny(unsafe_code)]

use alloc::boxed::Box;
use core::task::Poll;

use log::debug;
use x86_64::VirtAddr;

use crate::scheduler;

use super::poll_fn;
use super::user_ptr::{SyscallFuture, SyscallResult};

/// Handle process get PID operation.
pub fn handle_get_pid() -> SyscallFuture {
    Box::pin(core::future::ready(SyscallResult::ok(0)))
}

/// Handle process wait operation.
///
/// Blocks until the target process exits, then returns its exit code.
pub fn handle_wait(handle_id: u64) -> SyscallFuture {
    let resource = scheduler::with_current_process(|proc| {
        let handle = proc.handles().get(handle_id)?;
        if handle.as_process().is_some() {
            Some(handle.resource_arc())
        } else {
            None
        }
    });

    Box::pin(poll_fn(move |_cx| {
        let Some(ref resource) = resource else {
            return Poll::Ready(SyscallResult::err(panda_abi::ErrorCode::InvalidHandle));
        };
        let Some(process_iface) = resource.as_process() else {
            return Poll::Ready(SyscallResult::err(panda_abi::ErrorCode::InvalidHandle));
        };

        match process_iface.exit_code() {
            Some(exit_code) => Poll::Ready(SyscallResult::ok(exit_code as isize)),
            None => {
                // Register waker so we get woken when the process exits
                process_iface
                    .waker()
                    .set_waiting(scheduler::current_process_id());
                Poll::Pending
            }
        }
    }))
}

/// Handle process signal operation.
///
/// Sends a signal to the target process:
/// - `Signal::StopImmediately`: Immediately terminates the process (no userspace code runs)
/// - `Signal::Stop`: Delivers a message to the process's parent channel for graceful handling
pub fn handle_signal(handle_id: u64, signal: u32) -> SyscallFuture {
    // 1. Validate signal number
    let signal = match panda_abi::Signal::from_u32(signal) {
        Some(s) => s,
        None => {
            return Box::pin(core::future::ready(SyscallResult::err(
                panda_abi::ErrorCode::InvalidArgument,
            )))
        }
    };

    // 2. Get target process resource from handle
    let resource = scheduler::with_current_process(|proc| {
        let handle = proc.handles().get(handle_id)?;
        if handle.as_process().is_some() {
            Some(handle.resource_arc())
        } else {
            None
        }
    });

    let Some(resource) = resource else {
        return Box::pin(core::future::ready(SyscallResult::err(
            panda_abi::ErrorCode::InvalidHandle,
        )));
    };
    let Some(process_iface) = resource.as_process() else {
        return Box::pin(core::future::ready(SyscallResult::err(
            panda_abi::ErrorCode::InvalidHandle,
        )));
    };

    let result = match process_iface.signal(signal.as_u32()) {
        Ok(()) => SyscallResult::ok(0),
        Err(crate::resource::ProcessError::NotSupported) => {
            SyscallResult::err(panda_abi::ErrorCode::NotSupported)
        }
        Err(crate::resource::ProcessError::NotFound) => {
            SyscallResult::err(panda_abi::ErrorCode::NotFound)
        }
        Err(crate::resource::ProcessError::PermissionDenied) => {
            SyscallResult::err(panda_abi::ErrorCode::PermissionDenied)
        }
        Err(crate::resource::ProcessError::WouldBlock) => {
            SyscallResult::err(panda_abi::ErrorCode::WouldBlock)
        }
    };

    Box::pin(core::future::ready(result))
}

/// Handle process brk operation.
pub fn handle_brk(new_brk: usize) -> SyscallFuture {
    debug!("BRK: requested new_brk = {:#x}", new_brk);
    let result = scheduler::with_current_process(|proc| {
        if new_brk == 0 {
            // Query current break
            let current = proc.brk().as_u64() as isize;
            debug!("BRK: query, returning {:#x}", current);
            current
        } else {
            // Set new break
            let result = proc.set_brk(VirtAddr::new(new_brk as u64));
            debug!("BRK: set, returning {:#x}", result.as_u64());
            result.as_u64() as isize
        }
    });
    Box::pin(core::future::ready(SyscallResult::ok(result)))
}
