//! Error types for libpanda operations.
//!
//! This module provides a unified error type for high-level operations,
//! replacing the raw `isize` error codes from syscalls.

use core::fmt;

/// Error type for libpanda operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// Resource not found.
    NotFound,
    /// Permission denied.
    PermissionDenied,
    /// Invalid argument.
    InvalidArgument,
    /// I/O error.
    IoError,
    /// Operation would block (for non-blocking operations).
    WouldBlock,
    /// Operation not supported.
    NotSupported,
    /// Resource already exists.
    AlreadyExists,
    /// Buffer too small for operation.
    BufferTooSmall,
    /// Connection closed by peer.
    ConnectionClosed,
    /// Operation timed out.
    Timeout,
    /// Invalid seek position.
    InvalidOffset,
    /// Resource is not readable.
    NotReadable,
    /// Resource is not writable.
    NotWritable,
    /// Resource is not seekable.
    NotSeekable,
    /// Unknown error with raw code.
    Unknown(i32),
}

impl Error {
    /// Convert a raw syscall error code to an Error.
    ///
    /// Syscall error codes are negative `isize` values.
    pub fn from_code(code: isize) -> Self {
        // Map based on panda_abi::ErrorCode values
        match code {
            -1 => Error::NotFound,
            -2 => Error::InvalidOffset,
            -3 => Error::NotReadable,
            -4 => Error::NotWritable,
            -5 => Error::NotSeekable,
            -6 => Error::NotSupported,
            -7 => Error::PermissionDenied,
            -8 => Error::IoError,
            -9 => Error::WouldBlock,
            -10 => Error::InvalidArgument,
            -11 => Error::ConnectionClosed, // Protocol error -> ConnectionClosed
            _ => Error::Unknown(code as i32),
        }
    }

    /// Convert an Error back to a raw error code.
    pub fn to_code(self) -> isize {
        match self {
            Error::NotFound => -1,
            Error::InvalidOffset => -2,
            Error::NotReadable => -3,
            Error::NotWritable => -4,
            Error::NotSeekable => -5,
            Error::NotSupported => -6,
            Error::PermissionDenied => -7,
            Error::IoError => -8,
            Error::WouldBlock => -9,
            Error::InvalidArgument => -10,
            Error::ConnectionClosed => -11,
            Error::AlreadyExists => -12,
            Error::BufferTooSmall => -13,
            Error::Timeout => -14,
            Error::Unknown(code) => code as isize,
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::NotFound => write!(f, "not found"),
            Error::PermissionDenied => write!(f, "permission denied"),
            Error::InvalidArgument => write!(f, "invalid argument"),
            Error::IoError => write!(f, "I/O error"),
            Error::WouldBlock => write!(f, "operation would block"),
            Error::NotSupported => write!(f, "not supported"),
            Error::AlreadyExists => write!(f, "already exists"),
            Error::BufferTooSmall => write!(f, "buffer too small"),
            Error::ConnectionClosed => write!(f, "connection closed"),
            Error::Timeout => write!(f, "timeout"),
            Error::InvalidOffset => write!(f, "invalid offset"),
            Error::NotReadable => write!(f, "not readable"),
            Error::NotWritable => write!(f, "not writable"),
            Error::NotSeekable => write!(f, "not seekable"),
            Error::Unknown(code) => write!(f, "unknown error ({})", code),
        }
    }
}

/// Result type alias using the libpanda Error type.
pub type Result<T> = core::result::Result<T, Error>;

/// Convert a raw syscall result to a Result.
///
/// If `result` is negative, converts it to an Error.
/// Otherwise, returns the value as the success type.
#[inline]
pub fn from_syscall(result: isize) -> Result<usize> {
    if result < 0 {
        Err(Error::from_code(result))
    } else {
        Ok(result as usize)
    }
}

/// Convert a raw syscall result to a Result, mapping success to ().
#[inline]
pub fn from_syscall_unit(result: isize) -> Result<()> {
    if result < 0 {
        Err(Error::from_code(result))
    } else {
        Ok(())
    }
}

/// Convert a raw syscall result to a Result containing a Handle.
#[inline]
pub fn from_syscall_handle(result: isize) -> Result<crate::Handle> {
    if result < 0 {
        Err(Error::from_code(result))
    } else {
        Ok(crate::Handle::from(result as u32))
    }
}
