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
//! ```no_run
//! use libpanda::stdio;
//!
//! // Simple pipeline-compatible program
//! let mut buf = [0u8; 4096];
//! while let Ok(n) = stdio::read(&mut buf) {
//!     if n == 0 { break; }
//!     let _ = stdio::write(&buf[..n]);
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

// =============================================================================
// Value-based I/O for structured pipelines
// =============================================================================

use panda_abi::MAX_MESSAGE_SIZE;
use panda_abi::encoding::{Decode, Decoder, Encode, Encoder};
use panda_abi::value::Value;

/// Write a structured Value to standard output.
///
/// This encodes the Value to binary and sends it through the stdout channel.
/// Used for structured pipeline communication.
pub fn write_value(value: &Value) -> Result<()> {
    let mut encoder = Encoder::new();
    value.encode(&mut encoder);
    let bytes = encoder.finish();

    if bytes.len() > MAX_MESSAGE_SIZE {
        return Err(Error::from_code(-2)); // Message too large
    }

    write(&bytes)
}

/// Read a structured Value from standard input.
///
/// This reads bytes from stdin and decodes them as a Value.
/// Returns `None` if the channel is closed.
pub fn read_value() -> Result<Option<Value>> {
    let mut buf = [0u8; MAX_MESSAGE_SIZE];
    let n = read(&mut buf)?;

    if n == 0 {
        return Ok(None);
    }

    let mut decoder = Decoder::new(&buf[..n]);
    match Value::decode(&mut decoder) {
        Ok(value) => Ok(Some(value)),
        Err(_) => Err(Error::from_code(-4)), // Decode error
    }
}

/// Read a structured Value from standard input (non-blocking).
///
/// Returns `Ok(Some(value))` if data available, `Ok(None)` if no data,
/// or `Err` on channel error.
pub fn try_read_value() -> Result<Option<Value>> {
    let mut buf = [0u8; MAX_MESSAGE_SIZE];
    match try_read(&mut buf) {
        Ok(n) if n > 0 => {
            let mut decoder = Decoder::new(&buf[..n]);
            match Value::decode(&mut decoder) {
                Ok(value) => Ok(Some(value)),
                Err(_) => Err(Error::from_code(-4)), // Decode error
            }
        }
        Ok(_) => Ok(None), // No data available
        Err(e) => Err(e),
    }
}

// =============================================================================
// Pipeline detection and output helpers
// =============================================================================

/// Check if this process is running in a pipeline (has valid STDOUT).
///
/// Returns `true` if STDOUT is connected to another pipeline stage,
/// `false` if running standalone (output goes to PARENT/terminal).
///
/// This is determined by attempting a non-blocking write to STDOUT.
/// If STDOUT is invalid (not set up by parent), the operation fails.
pub fn is_pipeline() -> bool {
    // Try to check if STDOUT handle is valid by attempting a non-blocking operation
    // A non-blocking send to an invalid handle returns an error immediately
    // We use an empty message to minimize overhead
    let result = sys::channel::try_send_msg(Handle::STDOUT, &[]);
    // If result is 0, STDOUT is valid and we're in a pipeline
    // If result is negative, either STDOUT is invalid or queue is full
    // We consider "queue full" (-1) as a valid pipeline state
    result >= -1
}

/// Output a Value, choosing the appropriate channel based on context.
///
/// - In a pipeline: sends Value to STDOUT for next stage
/// - Standalone: sends Value via PARENT to terminal for display
///
/// This allows tools to output structured data that works both in
/// pipelines and standalone execution.
pub fn output_value(value: &Value) -> Result<()> {
    // For now, always send via STDOUT if possible, fall back to PARENT
    // In the future, we could check is_pipeline() to choose the channel
    match write_value(value) {
        Ok(()) => Ok(()),
        Err(_) => {
            // STDOUT not available, send via PARENT for terminal display
            use panda_abi::terminal::Request;

            // Send Value directly via Request::Write
            let msg = Request::Write(value.clone());

            let bytes = msg.to_bytes();
            let result = sys::channel::send_msg(Handle::PARENT, &bytes);
            if result < 0 {
                Err(Error::from_code(result))
            } else {
                Ok(())
            }
        }
    }
}
