//! Shared ABI definitions between kernel and userspace.
//!
//! This crate contains syscall numbers, constants, and shared types
//! that both the kernel and userspace need to agree on.

#![no_std]

// Syscall numbers
pub const SYSCALL_LOG: usize = 0x10;
pub const SYSCALL_EXIT: usize = 0x11;
pub const SYSCALL_OPEN: usize = 0x20;
pub const SYSCALL_CLOSE: usize = 0x21;
pub const SYSCALL_READ: usize = 0x22;
pub const SYSCALL_SEEK: usize = 0x23;
pub const SYSCALL_FSTAT: usize = 0x24;

// Seek whence values
pub const SEEK_SET: usize = 0;
pub const SEEK_CUR: usize = 1;
pub const SEEK_END: usize = 2;

/// File stat structure shared between kernel and userspace
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct FileStat {
    pub size: u64,
    pub is_dir: bool,
}
