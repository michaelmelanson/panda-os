//! Safe userspace memory access.
//!
//! Provides compile-time and runtime enforcement that userspace memory is only
//! accessed when the process's page table is active.
//!
//! - `UserSlice`: An opaque (address, length) pair. `Send + Copy`, safe to capture in futures.
//! - `UserAccess`: A `!Send` token proving the page table is active. Cannot be captured in futures.
//! - `SyscallResult`: Return type for syscall futures, with optional writeback to userspace.
//! - `SyscallError`: Early-return error type for syscall setup (bad pointer, invalid handle, etc.).

use alloc::boxed::Box;
use alloc::vec::Vec;
use core::future::Future;
use core::pin::Pin;

/// Upper bound of userspace addresses (lower canonical half).
const USER_ADDR_MAX: usize = 0x0000_7fff_ffff_ffff;

/// A boxed syscall future. All non-diverging syscall handlers return this type.
pub type SyscallFuture = Pin<Box<dyn Future<Output = SyscallResult> + Send>>;

/// A region of userspace memory. Stores address and length but cannot be
/// dereferenced directly — you need a `UserAccess` token.
///
/// `UserSlice` is `Send + Copy`, so it can safely be captured in futures
/// (it's just two integers with private fields).
#[derive(Clone, Copy)]
pub struct UserSlice {
    addr: usize,
    len: usize,
}

impl UserSlice {
    pub fn new(addr: usize, len: usize) -> Self {
        Self { addr, len }
    }

    pub fn len(&self) -> usize {
        self.len
    }
}

/// Proof that the current process's page table is active.
///
/// Not `Send` — cannot be captured in a `Send` future. This is the key
/// invariant: futures run when the scheduler polls them, potentially with
/// a different page table active. By making `UserAccess` non-`Send`, the
/// compiler prevents futures from holding onto it.
///
/// All reads and writes validate that the pointer falls within the userspace
/// address range (lower canonical half: `0` to `0x0000_7fff_ffff_ffff`)
/// before accessing memory.
pub struct UserAccess(());

impl !Send for UserAccess {}

impl UserAccess {
    /// Create a new `UserAccess` token.
    ///
    /// # Safety
    /// Caller must ensure the current process's page table is active.
    pub(crate) unsafe fn new() -> Self {
        Self(())
    }

    /// Validate that a `UserSlice` falls entirely within userspace.
    fn validate(&self, slice: UserSlice) -> Result<(), SyscallError> {
        if slice.len == 0 {
            return Ok(());
        }
        let end = slice
            .addr
            .checked_add(slice.len)
            .ok_or(SyscallError::BadUserPointer)?;
        if end - 1 > USER_ADDR_MAX {
            return Err(SyscallError::BadUserPointer);
        }
        Ok(())
    }

    /// Copy data from userspace into a kernel `Vec`.
    pub fn read(&self, src: UserSlice) -> Result<Vec<u8>, SyscallError> {
        self.validate(src)?;
        let slice = unsafe { core::slice::from_raw_parts(src.addr as *const u8, src.len) };
        Ok(slice.to_vec())
    }

    /// Copy data from kernel into userspace. Returns the number of bytes written.
    pub fn write(&self, dst: UserSlice, data: &[u8]) -> Result<usize, SyscallError> {
        self.validate(dst)?;
        let slice = unsafe { core::slice::from_raw_parts_mut(dst.addr as *mut u8, dst.len) };
        let n = data.len().min(slice.len());
        slice[..n].copy_from_slice(&data[..n]);
        Ok(n)
    }

    /// Read a `Copy` struct from userspace.
    pub fn read_struct<T: Copy>(&self, addr: usize) -> Result<T, SyscallError> {
        let slice = UserSlice::new(addr, core::mem::size_of::<T>());
        self.validate(slice)?;
        Ok(unsafe { core::ptr::read(addr as *const T) })
    }

    /// Write a `Copy` struct to userspace.
    pub fn write_struct<T: Copy>(&self, addr: usize, value: &T) -> Result<(), SyscallError> {
        let slice = UserSlice::new(addr, core::mem::size_of::<T>());
        self.validate(slice)?;
        unsafe { core::ptr::write(addr as *mut T, *value) };
        Ok(())
    }

    /// Read a UTF-8 string from userspace.
    pub fn read_str(&self, addr: usize, len: usize) -> Result<&str, SyscallError> {
        let slice = UserSlice::new(addr, len);
        self.validate(slice)?;
        let bytes = unsafe { core::slice::from_raw_parts(addr as *const u8, len) };
        core::str::from_utf8(bytes).map_err(|_| SyscallError::BadUserPointer)
    }
}

/// Errors that can occur during syscall setup (before the future runs).
/// Handlers return these via `?` to bail out early.
#[derive(Debug)]
pub enum SyscallError {
    /// A userspace pointer was outside the valid address range.
    BadUserPointer,
    /// The handle ID was invalid or of the wrong type.
    InvalidHandle,
}

/// Result of a syscall future, with optional data to write back to userspace.
pub struct SyscallResult {
    /// The return code (placed in `rax` when returning to userspace).
    pub code: isize,
    /// Optional data to copy to userspace after the future completes.
    pub writeback: Option<WriteBack>,
}

impl SyscallResult {
    /// A successful result with no writeback.
    pub fn ok(code: isize) -> Self {
        Self {
            code,
            writeback: None,
        }
    }

    /// An error result.
    pub fn err(code: isize) -> Self {
        Self {
            code,
            writeback: None,
        }
    }

    /// A result with data to write back to userspace.
    pub fn write_back(code: isize, data: Vec<u8>, dst: UserSlice) -> Self {
        Self {
            code,
            writeback: Some(WriteBack { data, dst }),
        }
    }

    /// A result that writes a `Copy` struct back to userspace.
    ///
    /// This safely converts the struct to bytes without requiring `unsafe` in
    /// handler code.
    pub fn write_back_struct<T: Copy>(code: isize, value: &T, dst: UserSlice) -> Self {
        let bytes = unsafe {
            core::slice::from_raw_parts(value as *const T as *const u8, core::mem::size_of::<T>())
        };
        Self {
            code,
            writeback: Some(WriteBack {
                data: bytes.to_vec(),
                dst,
            }),
        }
    }
}

/// Data to copy from kernel to userspace after a future completes.
pub struct WriteBack {
    /// Kernel-side data to copy out.
    pub data: Vec<u8>,
    /// Destination in userspace.
    pub dst: UserSlice,
}
