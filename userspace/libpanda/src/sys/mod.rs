//! Low-level syscall wrappers.
//!
//! This module provides thin, zero-cost wrappers around the raw syscall interface.
//! All functions return raw `isize` error codes and perform no allocations.
//!
//! For ergonomic, type-safe APIs, use the high-level modules in the crate root.

use core::arch::asm;

pub mod buffer;
pub mod channel;
pub mod env;
pub mod file;
pub mod mailbox;
pub mod process;
pub mod surface;

// Re-export the raw Handle type
pub use crate::handle::Handle;

// Re-export commonly used ABI constants
pub use panda_abi::{
    // Shared types
    BufferAllocInfo,
    // Flag constants
    CHANNEL_NONBLOCK,
    DirEntry,
    // Event flags
    EVENT_CHANNEL_CLOSED,
    EVENT_CHANNEL_READABLE,
    EVENT_CHANNEL_WRITABLE,
    EVENT_KEYBOARD_KEY,
    EVENT_PROCESS_EXITED,
    FILE_NONBLOCK,
    FileStat,
    // Well-known handles
    HANDLE_ENVIRONMENT,
    HANDLE_MAILBOX,
    HANDLE_PARENT,
    HANDLE_SELF,
    // Size constants
    MAX_MESSAGE_SIZE,
    // Seek constants
    SEEK_CUR,
    SEEK_END,
    SEEK_SET,
    // Syscall number
    SYSCALL_SEND,
};

/// Raw syscall interface.
///
/// This performs the actual syscall instruction. Prefer using the typed
/// wrappers in this module's submodules.
#[inline(always)]
fn syscall(
    code: usize,
    arg0: usize,
    arg1: usize,
    arg2: usize,
    arg3: usize,
    arg4: usize,
    arg5: usize,
) -> isize {
    let result: isize;
    unsafe {
        asm!(
            "syscall",
            inlateout("rax") code => result,
            inlateout("rdi") arg0 => _,
            inlateout("rsi") arg1 => _,
            inlateout("rdx") arg2 => _,
            inlateout("r10") arg3 => _,
            inlateout("r8") arg4 => _,
            inlateout("r9") arg5 => _,
            out("rcx") _,
            out("r11") _,
            out("xmm0") _,
            out("xmm1") _,
            out("xmm2") _,
            out("xmm3") _,
            out("xmm4") _,
            out("xmm5") _,
            out("xmm6") _,
            out("xmm7") _,
            out("xmm8") _,
            out("xmm9") _,
            out("xmm10") _,
            out("xmm11") _,
            out("xmm12") _,
            out("xmm13") _,
            out("xmm14") _,
            out("xmm15") _,
        );
    }
    result
}

/// Send an operation to a resource handle (unified syscall interface).
///
/// This is the fundamental syscall wrapper. All kernel operations go through
/// this interface.
///
/// # Arguments
/// * `handle` - The resource handle to operate on
/// * `operation` - The operation code (from `panda_abi::Operation`)
/// * `arg0..arg3` - Operation-specific arguments
///
/// # Returns
/// Operation-specific result, or negative error code.
#[inline(always)]
pub fn send(
    handle: Handle,
    operation: u32,
    arg0: usize,
    arg1: usize,
    arg2: usize,
    arg3: usize,
) -> isize {
    syscall(
        SYSCALL_SEND,
        u64::from(handle) as usize,
        operation as usize,
        arg0,
        arg1,
        arg2,
        arg3,
    )
}
