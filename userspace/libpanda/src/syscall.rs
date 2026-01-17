use core::arch::asm;

// Re-export ABI constants
pub use panda_abi::{
    // Well-known handles
    HANDLE_ENVIRONMENT, HANDLE_SELF,
    // Operation codes
    OP_ENVIRONMENT_LOG, OP_ENVIRONMENT_OPEN, OP_ENVIRONMENT_SPAWN, OP_ENVIRONMENT_TIME,
    OP_FILE_CLOSE, OP_FILE_READ, OP_FILE_SEEK, OP_FILE_STAT, OP_FILE_WRITE,
    OP_PROCESS_BRK, OP_PROCESS_EXIT, OP_PROCESS_GET_PID, OP_PROCESS_SIGNAL, OP_PROCESS_WAIT,
    OP_PROCESS_YIELD,
    // Shared types
    FileStat, SEEK_CUR, SEEK_END, SEEK_SET,
    // Syscall number
    SYSCALL_SEND,
};

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
            in("rax") code,
            in("rdi") arg0,
            in("rsi") arg1,
            in("rdx") arg2,
            in("r10") arg3,
            in("r8") arg4,
            in("r9") arg5,
            lateout("rax") result,
            out("rcx") _,
            out("r11") _,
        );
    }
    result
}

/// Send an operation to a resource handle (unified syscall interface)
///
/// This is the low-level syscall wrapper. Prefer using the typed wrappers
/// in `file`, `process`, and `environment` modules.
#[inline(always)]
pub fn send(
    handle: crate::Handle,
    operation: u32,
    arg0: usize,
    arg1: usize,
    arg2: usize,
    arg3: usize,
) -> isize {
    syscall(
        SYSCALL_SEND,
        u32::from(handle) as usize,
        operation as usize,
        arg0,
        arg1,
        arg2,
        arg3,
    )
}
