//! Process operation syscall handlers (OP_PROCESS_*).

use log::{debug, info};
use x86_64::VirtAddr;

use crate::scheduler;

use super::SyscallContext;

/// Handle process yield operation.
pub fn handle_yield(ctx: &SyscallContext) -> ! {
    unsafe {
        scheduler::yield_current(
            VirtAddr::new(ctx.return_rip as u64),
            VirtAddr::new(ctx.user_rsp as u64),
        );
    }
}

/// Handle process exit operation.
pub fn handle_exit(exit_code: i32) -> ! {
    let current_pid = scheduler::current_process_id();
    info!("Process {:?} exiting with code {exit_code}", current_pid);

    // Get the process info before removing the process.
    // We'll set the exit code after releasing the scheduler lock
    // to avoid deadlock (set_exit_code -> wake -> wake_process needs the lock).
    let process_info = scheduler::with_current_process(|proc| proc.info().clone());

    scheduler::remove_process(current_pid);

    // Set exit code after removing from scheduler (wakes any waiters)
    process_info.set_exit_code(exit_code);

    unsafe {
        scheduler::exec_next_runnable();
    }
}

/// Handle process get PID operation.
pub fn handle_get_pid() -> isize {
    // For now, just return 0 for self - we'll implement proper PIDs later
    0
}

/// Handle process wait operation.
pub fn handle_wait(ctx: &SyscallContext, handle: u32) -> isize {
    let result = scheduler::with_current_process(|proc| {
        proc.handles()
            .get_process(handle)
            .map(|ph| (ph.exit_code(), ph.waker().clone()))
    });

    match result {
        Some((Some(exit_code), _)) => {
            // Process already exited, return exit code immediately
            exit_code as isize
        }
        Some((None, waker)) => {
            // Process still running, block until it exits
            ctx.block_on(waker);
        }
        None => {
            // Invalid handle
            -1
        }
    }
}

/// Handle process signal operation.
pub fn handle_signal() -> isize {
    // TODO: Implement signals
    -1
}

/// Handle process brk operation.
pub fn handle_brk(new_brk: usize) -> isize {
    debug!("BRK: requested new_brk = {:#x}", new_brk);
    scheduler::with_current_process(|proc| {
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
    })
}
