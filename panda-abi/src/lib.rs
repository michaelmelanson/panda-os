//! Shared ABI definitions between kernel and userspace.
//!
//! This crate contains syscall numbers, constants, and shared types
//! that both the kernel and userspace need to agree on.

#![no_std]

// =============================================================================
// Syscall numbers
// =============================================================================

/// The unified send syscall - all operations go through this
pub const SYSCALL_SEND: usize = 0x30;

// =============================================================================
// Well-known handles
// =============================================================================

/// Handle to the current process (Process resource)
pub const HANDLE_SELF: u32 = 0;

/// Handle to the system environment (Environment resource)
pub const HANDLE_ENVIRONMENT: u32 = 1;

// =============================================================================
// Operation codes
// =============================================================================

// File operations (0x1_0000 - 0x1_FFFF)
/// Read from file: (buf_ptr, buf_len) -> bytes_read
pub const OP_FILE_READ: u32 = 0x1_0000;
/// Write to file: (buf_ptr, buf_len) -> bytes_written
pub const OP_FILE_WRITE: u32 = 0x1_0001;
/// Seek in file: (offset_lo, offset_hi, whence) -> new_position
pub const OP_FILE_SEEK: u32 = 0x1_0002;
/// Get file stats: (stat_ptr) -> 0 or error
pub const OP_FILE_STAT: u32 = 0x1_0003;
/// Close file: () -> 0 or error
pub const OP_FILE_CLOSE: u32 = 0x1_0004;

// Process operations (0x2_0000 - 0x2_FFFF)
/// Yield execution: () -> 0
pub const OP_PROCESS_YIELD: u32 = 0x2_0000;
/// Exit process: (code) -> !
pub const OP_PROCESS_EXIT: u32 = 0x2_0001;
/// Get process ID: () -> pid
pub const OP_PROCESS_GET_PID: u32 = 0x2_0002;
/// Wait for child: () -> exit_code or error
pub const OP_PROCESS_WAIT: u32 = 0x2_0003;
/// Signal process: (signal) -> 0 or error
pub const OP_PROCESS_SIGNAL: u32 = 0x2_0004;
/// Set program break: (new_brk) -> current_brk or error
/// If new_brk is 0, returns current break without changing it.
/// Pages are allocated on demand via page faults.
pub const OP_PROCESS_BRK: u32 = 0x2_0005;

// Userspace heap region constants
/// Base address of the userspace heap region (16MB after stack at 0xb0000000000)
/// Must be a canonical x86_64 address (bit 47 = 0, so < 0x800000000000).
pub const HEAP_BASE: usize = 0xb000_1000_000;
/// Maximum size of the userspace heap (1 TB virtual address space)
/// Actual physical memory is allocated on demand via page faults.
pub const HEAP_MAX_SIZE: usize = 0x100_0000_0000;

// Environment operations (0x3_0000 - 0x3_FFFF)
/// Open file: (path_ptr, path_len, flags) -> handle
pub const OP_ENVIRONMENT_OPEN: u32 = 0x3_0000;
/// Spawn process: (path_ptr, path_len) -> process_handle
pub const OP_ENVIRONMENT_SPAWN: u32 = 0x3_0001;
/// Log message: (msg_ptr, msg_len) -> 0
pub const OP_ENVIRONMENT_LOG: u32 = 0x3_0002;
/// Get time: () -> timestamp
pub const OP_ENVIRONMENT_TIME: u32 = 0x3_0003;

// =============================================================================
// Constants
// =============================================================================

// Seek whence values
pub const SEEK_SET: usize = 0;
pub const SEEK_CUR: usize = 1;
pub const SEEK_END: usize = 2;

// =============================================================================
// Shared types
// =============================================================================

/// File stat structure shared between kernel and userspace
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct FileStat {
    pub size: u64,
    pub is_dir: bool,
}
