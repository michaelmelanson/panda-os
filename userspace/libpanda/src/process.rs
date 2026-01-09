//! Process operations using the send-based API

use core::arch::asm;

use crate::syscall::send;
use panda_abi::*;

/// Yield the CPU to another process
#[inline(always)]
pub fn yield_now() {
    let _ = send(HANDLE_SELF, OP_PROCESS_YIELD, 0, 0, 0, 0);
}

/// Exit the current process with the given exit code
#[inline(always)]
pub fn exit(code: i32) -> ! {
    let _ = send(HANDLE_SELF, OP_PROCESS_EXIT, code as usize, 0, 0, 0);
    // Should never return, but just in case
    loop {
        unsafe {
            asm!("int3", "hlt");
        }
    }
}

/// Get the current process ID
#[inline(always)]
pub fn getpid() -> u64 {
    send(HANDLE_SELF, OP_PROCESS_GET_PID, 0, 0, 0, 0) as u64
}

/// Wait for a child process to exit
///
/// Returns the exit code of the child, or negative error code
#[inline(always)]
pub fn wait(child_handle: u32) -> i32 {
    send(child_handle, OP_PROCESS_WAIT, 0, 0, 0, 0) as i32
}

/// Send a signal to a process
///
/// Returns 0 on success, or negative error code
#[inline(always)]
pub fn signal(process_handle: u32, sig: u32) -> isize {
    send(process_handle, OP_PROCESS_SIGNAL, sig as usize, 0, 0, 0)
}
