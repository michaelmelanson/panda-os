//! Syscall handling infrastructure.
//!
//! This module handles the system call interface between userspace and the kernel.
//! It provides:
//! - GDT/TSS setup for privilege transitions
//! - Syscall entry point (via SYSCALL/SYSRET)
//! - Dispatch to operation-specific handlers

mod entry;
mod environment;
mod file;
pub mod gdt;
mod process;

use log::{debug, error};
use x86_64::VirtAddr;

use alloc::sync::Arc;

use crate::{
    process::{SavedState, waker::Waker},
    scheduler,
};

/// Callee-saved registers that must be preserved across syscalls.
/// These are saved by syscall_entry and passed to syscall_handler for use
/// when a process blocks and needs to restore full state on resume.
#[derive(Clone, Copy, Default)]
#[repr(C)]
pub struct CalleeSavedRegs {
    pub rbx: u64,
    pub rbp: u64,
    pub r12: u64,
    pub r13: u64,
    pub r14: u64,
    pub r15: u64,
}

/// Get the user code segment selector. Must be called after init().
pub fn user_code_selector() -> u16 {
    gdt::user_code_selector()
}

/// Initialize syscall infrastructure (GDT, TSS, MSRs).
pub fn init() {
    gdt::init();
    entry::init();
}

/// Syscall arguments - used to save state for restart after blocking.
#[derive(Clone, Copy)]
pub struct SyscallArgs {
    pub code: usize,
    pub arg0: usize,
    pub arg1: usize,
    pub arg2: usize,
    pub arg3: usize,
    pub arg4: usize,
    pub arg5: usize,
}

impl SyscallArgs {
    /// Get all 6 arguments as an array (for SavedState construction).
    pub fn args(&self) -> [usize; 6] {
        [
            self.arg0, self.arg1, self.arg2, self.arg3, self.arg4, self.arg5,
        ]
    }
}

/// Context passed to syscall handlers.
///
/// Provides access to syscall arguments and helper methods for common operations
/// like blocking the current process.
pub struct SyscallContext<'a> {
    pub return_rip: usize,
    pub user_rsp: usize,
    pub args: &'a SyscallArgs,
    pub callee_saved: &'a CalleeSavedRegs,
}

impl SyscallContext<'_> {
    /// Block the current process until the waker fires.
    ///
    /// This saves the current syscall state so it can be re-executed when
    /// the process is woken. This function does not return.
    pub fn block_on(&self, waker: Arc<Waker>) -> ! {
        // Save RIP-2 to re-execute the syscall instruction when resumed.
        // The syscall instruction is 2 bytes (0F 05).
        let syscall_ip = (self.return_rip - 2) as u64;
        let saved_state = SavedState::for_syscall_restart(
            syscall_ip,
            self.user_rsp as u64,
            self.args.code,
            &self.args.args(),
            self.callee_saved,
        );
        unsafe {
            scheduler::block_current_on(
                waker,
                VirtAddr::new(syscall_ip),
                VirtAddr::new(self.user_rsp as u64),
                saved_state,
            );
        }
    }
}

/// Main syscall handler called from entry.rs.
///
/// This is called from the naked syscall_entry function with all registers saved.
#[allow(clippy::too_many_arguments)]
extern "sysv64" fn syscall_handler(
    arg0: usize,
    arg1: usize,
    arg2: usize,
    arg3: usize,
    arg4: usize,
    arg5: usize,
    code: usize,
    return_rip: usize,
    user_rsp: usize,
    callee_saved: *const CalleeSavedRegs,
) -> isize {
    debug!("SYSCALL: code={code:X}, args: {arg0:X}, {arg1:X}, {arg2:X}, {arg3:X}");

    let syscall_args = SyscallArgs {
        code,
        arg0,
        arg1,
        arg2,
        arg3,
        arg4,
        arg5,
    };

    let callee_saved = unsafe { &*callee_saved };

    let ctx = SyscallContext {
        return_rip,
        user_rsp,
        args: &syscall_args,
        callee_saved,
    };

    match code {
        panda_abi::SYSCALL_SEND => {
            let handle = arg0 as u32;
            let operation = arg1 as u32;
            handle_send(&ctx, handle, operation, arg2, arg3)
        }
        _ => -1,
    }
}

/// Handle the unified send syscall, dispatching to operation-specific handlers.
fn handle_send(
    ctx: &SyscallContext,
    handle: u32,
    operation: u32,
    arg0: usize,
    arg1: usize,
) -> isize {
    use panda_abi::*;

    match operation {
        // File operations
        OP_FILE_READ => file::handle_read(ctx, handle, arg0, arg1),
        OP_FILE_WRITE => file::handle_write(handle, arg0, arg1),
        OP_FILE_SEEK => file::handle_seek(handle, arg0, arg1),
        OP_FILE_STAT => file::handle_stat(handle, arg0),
        OP_FILE_CLOSE => file::handle_close(handle),
        OP_FILE_READDIR => file::handle_readdir(handle, arg0),

        // Process operations
        OP_PROCESS_YIELD => process::handle_yield(ctx),
        OP_PROCESS_EXIT => process::handle_exit(arg0 as i32),
        OP_PROCESS_GET_PID => process::handle_get_pid(),
        OP_PROCESS_WAIT => process::handle_wait(ctx, handle),
        OP_PROCESS_SIGNAL => process::handle_signal(),
        OP_PROCESS_BRK => process::handle_brk(arg0),

        // Environment operations
        OP_ENVIRONMENT_OPEN => environment::handle_open(arg0, arg1),
        OP_ENVIRONMENT_SPAWN => environment::handle_spawn(arg0, arg1),
        OP_ENVIRONMENT_LOG => environment::handle_log(arg0, arg1),
        OP_ENVIRONMENT_TIME => environment::handle_time(),
        OP_ENVIRONMENT_OPENDIR => environment::handle_opendir(arg0, arg1),

        _ => {
            error!("Unknown operation: {:#x}", operation);
            -1
        }
    }
}
