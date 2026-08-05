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

use super::helpers::{downcast_or_invalid, resolve_resource};
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
    let resource = resolve_resource(handle_id, |h| h.as_process().is_some());

    Box::pin(poll_fn(move |_cx| {
        let Some(process_iface) = downcast_or_invalid(&resource, |r| r.as_process()) else {
            return Poll::Ready(SyscallResult::err(panda_abi::ErrorCode::InvalidHandle));
        };

        // Re-check after registering: the process may exit (and call
        // `wake()`) between our first `exit_code()` check and
        // `set_waiting()` below, in which case `wake()` finds no
        // registered waiter yet and the exit signal would otherwise be
        // missed entirely, leaving us blocked forever.
        match process_iface.exit_code() {
            Some(exit_code) => Poll::Ready(SyscallResult::ok(exit_code as isize)),
            None => {
                process_iface
                    .waker()
                    .set_waiting(scheduler::current_process_id());
                match process_iface.exit_code() {
                    Some(exit_code) => Poll::Ready(SyscallResult::ok(exit_code as isize)),
                    None => Poll::Pending,
                }
            }
        }
    }))
}

/// Handle process sleep operation.
///
/// Blocks the calling process until `duration_ms` milliseconds have elapsed,
/// then returns 0. The wakeup deadline is computed once, on the first poll,
/// from the current uptime.
pub fn handle_sleep(duration_ms: u64) -> SyscallFuture {
    let mut wakeup_time = None;

    Box::pin(poll_fn(move |_cx| {
        let deadline = *wakeup_time.get_or_insert_with(|| crate::time::uptime_ms() + duration_ms);

        if crate::time::uptime_ms() >= deadline {
            return Poll::Ready(SyscallResult::ok(0));
        }

        // Re-check after registering: a timer tick could advance uptime past
        // `deadline` between our check above and the registration below, in
        // which case the deadline is already expired by the time it's
        // registered. That's fine (`wake_deadline_tasks` wakes anything whose
        // deadline is `<= now`, not just ones that expired "just now"), but we
        // still re-check here to avoid needlessly blocking in that case,
        // mirroring the check-register-recheck pattern used by `handle_wait`
        // and `channel::handle_recv` to avoid lost wakeups.
        let pid = scheduler::current_process_id();
        scheduler::register_deadline(scheduler::SchedulableEntity::Process(pid), deadline);

        if crate::time::uptime_ms() >= deadline {
            return Poll::Ready(SyscallResult::ok(0));
        }

        Poll::Pending
    }))
}

/// Handle process signal operation.
pub fn handle_signal() -> SyscallFuture {
    Box::pin(core::future::ready(SyscallResult::err(
        panda_abi::ErrorCode::NotSupported,
    )))
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
