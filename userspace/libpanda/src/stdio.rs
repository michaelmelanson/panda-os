//! Standard I/O abstraction.
//!
//! This module provides stdin/stdout handles for programs that support
//! pipeline redirection. Programs spawned as part of a pipeline have their
//! `HANDLE_STDIN` and `HANDLE_STDOUT` set to channel endpoints connecting
//! them to adjacent pipeline stages.
//!
//! For programs not in a pipeline, these handles are invalid. Such programs
//! should use `Handle::PARENT` directly to communicate with their parent
//! (typically the terminal).
//!
//! # Design
//!
//! - `stdin()` / `stdout()` - return the raw stdio handles (may be invalid)
//! - `read()` / `write()` - operate on stdin/stdout handles
//! - `parent()` - returns `HANDLE_PARENT` for terminal protocol communication
//!
//! Programs that want to work both standalone and in pipelines can:
//! 1. Try stdio operations first
//! 2. Fall back to parent channel on error
//!
//! Or use the higher-level `terminal` module which handles this automatically.
//!
//! # Example
//!
//! ```
//! use libpanda::stdio;
//!
//! // Simple pipeline-compatible program
//! let mut buf = [0u8; 4096];
//! while let Ok(n) = stdio::read(&mut buf) {
//!     if n == 0 { break; }
//!     stdio::write(&buf[..n])?;
//! }
//! ```

use crate::error::{Error, Result};
use crate::handle::Handle;
use crate::sys;

/// Returns the standard input handle.
///
/// This handle is only valid if the process was spawned with stdin redirection
/// (e.g., as part of a pipeline). For non-pipeline processes, this handle is
/// invalid and operations on it will fail.
#[inline]
pub fn stdin() -> Handle {
    Handle::STDIN
}

/// Returns the standard output handle.
///
/// This handle is only valid if the process was spawned with stdout redirection
/// (e.g., as part of a pipeline). For non-pipeline processes, this handle is
/// invalid and operations on it will fail.
#[inline]
pub fn stdout() -> Handle {
    Handle::STDOUT
}

/// Returns the parent channel handle.
///
/// Use this for communication with the parent process (e.g., terminal protocol).
/// This is separate from stdio - a pipeline process might have both a parent
/// channel (for control) and stdio handles (for data flow).
#[inline]
pub fn parent() -> Handle {
    Handle::PARENT
}

/// Read from standard input (blocking).
///
/// Returns the number of bytes read, or an error if stdin is invalid or
/// the peer closed the channel.
pub fn read(buf: &mut [u8]) -> Result<usize> {
    let result = sys::channel::recv_msg(Handle::STDIN, buf);
    if result < 0 {
        Err(Error::from_code(result))
    } else {
        Ok(result as usize)
    }
}

/// Read from standard input (non-blocking).
///
/// Returns `Ok(n)` with bytes read, or `Err` if no data available or error.
pub fn try_read(buf: &mut [u8]) -> Result<usize> {
    let result = sys::channel::try_recv_msg(Handle::STDIN, buf);
    if result < 0 {
        Err(Error::from_code(result))
    } else {
        Ok(result as usize)
    }
}

/// Write to standard output (blocking).
///
/// Blocks if the output queue is full. Returns error if stdout is invalid
/// or the peer closed the channel.
pub fn write(data: &[u8]) -> Result<()> {
    let result = sys::channel::send_msg(Handle::STDOUT, data);
    if result < 0 {
        Err(Error::from_code(result))
    } else {
        Ok(())
    }
}

/// Write to standard output (non-blocking).
///
/// Returns `Ok(())` if written, or `Err` if queue full or error.
pub fn try_write(data: &[u8]) -> Result<()> {
    let result = sys::channel::try_send_msg(Handle::STDOUT, data);
    if result < 0 {
        Err(Error::from_code(result))
    } else {
        Ok(())
    }
}

/// Write a string to standard output.
#[inline]
pub fn print(s: &str) -> Result<()> {
    write(s.as_bytes())
}

/// Write a string followed by a newline to standard output.
pub fn println(s: &str) -> Result<()> {
    write(s.as_bytes())?;
    write(b"\n")
}
