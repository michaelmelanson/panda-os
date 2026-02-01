//! Error helpers for libpanda operations.
//!
//! Uses `panda_abi::ErrorCode` as the single error type. This module
//! provides helper functions for converting raw syscall return values
//! into typed `Result`s.

use panda_abi::ErrorCode;

/// Result type alias using `ErrorCode`.
pub type Result<T> = core::result::Result<T, ErrorCode>;

/// Convert a raw negative error code to an `ErrorCode`.
///
/// This is the inverse of `ErrorCode::to_isize()`. Falls back to
/// `ErrorCode::IoError` for unrecognised codes.
#[inline]
pub fn from_code(code: isize) -> ErrorCode {
    ErrorCode::from_isize(code).unwrap_or(ErrorCode::IoError)
}

/// Convert a raw syscall result to a `Result<usize>`.
///
/// If `result` is negative, converts it to an `ErrorCode`.
/// Otherwise, returns the value as the success type.
#[inline]
pub fn from_syscall(result: isize) -> Result<usize> {
    if result < 0 {
        Err(ErrorCode::from_isize(result).unwrap_or(ErrorCode::IoError))
    } else {
        Ok(result as usize)
    }
}

/// Convert a raw syscall result to a `Result<()>`.
#[inline]
pub fn from_syscall_unit(result: isize) -> Result<()> {
    if result < 0 {
        Err(ErrorCode::from_isize(result).unwrap_or(ErrorCode::IoError))
    } else {
        Ok(())
    }
}

/// Convert a raw syscall result to a `Result<Handle>`.
#[inline]
pub fn from_syscall_handle(result: isize) -> Result<crate::Handle> {
    if result < 0 {
        Err(ErrorCode::from_isize(result).unwrap_or(ErrorCode::IoError))
    } else {
        Ok(crate::Handle::from(result as u64))
    }
}
