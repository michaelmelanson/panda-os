//! Startup message protocol for passing arguments to child processes.
//!
//! When a process is spawned, the parent can send a startup message over
//! the HANDLE_PARENT channel containing command-line arguments.
//!
//! Message format:
//! - StartupMessageHeader (8 bytes)
//! - [u16; arg_count] argument lengths
//! - Packed argument strings (no null terminators)

use alloc::string::String;
use alloc::vec::Vec;
use panda_abi::StartupMessageHeader;

/// Current protocol version.
pub const PROTOCOL_VERSION: u16 = 1;

/// Error type for startup message operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartupError {
    /// Buffer too small to hold the encoded message.
    BufferTooSmall,
    /// Message is malformed or truncated.
    InvalidMessage,
    /// Protocol version mismatch.
    VersionMismatch,
    /// Too many arguments.
    TooManyArgs,
    /// Argument too long.
    ArgTooLong,
}

/// Encode arguments into a startup message.
///
/// Returns the number of bytes written to `buf`.
pub fn encode(args: &[&str], buf: &mut [u8]) -> Result<usize, StartupError> {
    if args.len() > u16::MAX as usize {
        return Err(StartupError::TooManyArgs);
    }

    // Calculate total size needed
    let header_size = core::mem::size_of::<StartupMessageHeader>();
    let lengths_size = args.len() * 2; // u16 per arg
    let strings_size: usize = args.iter().map(|s| s.len()).sum();
    let total_size = header_size + lengths_size + strings_size;

    if buf.len() < total_size {
        return Err(StartupError::BufferTooSmall);
    }

    // Validate arg lengths
    for arg in args {
        if arg.len() > u16::MAX as usize {
            return Err(StartupError::ArgTooLong);
        }
    }

    // Write header
    let header = StartupMessageHeader {
        version: PROTOCOL_VERSION,
        arg_count: args.len() as u16,
        env_count: 0,
        flags: 0,
    };
    let header_bytes: [u8; 8] = unsafe { core::mem::transmute(header) };
    buf[..header_size].copy_from_slice(&header_bytes);

    // Write argument lengths
    let mut offset = header_size;
    for arg in args {
        let len_bytes = (arg.len() as u16).to_le_bytes();
        buf[offset..offset + 2].copy_from_slice(&len_bytes);
        offset += 2;
    }

    // Write argument strings
    for arg in args {
        buf[offset..offset + arg.len()].copy_from_slice(arg.as_bytes());
        offset += arg.len();
    }

    Ok(total_size)
}

/// Decode a startup message into arguments.
///
/// Returns a vector of argument strings.
pub fn decode(buf: &[u8]) -> Result<Vec<String>, StartupError> {
    let header_size = core::mem::size_of::<StartupMessageHeader>();

    if buf.len() < header_size {
        return Err(StartupError::InvalidMessage);
    }

    // Read header
    let header: StartupMessageHeader =
        unsafe { core::ptr::read(buf.as_ptr() as *const StartupMessageHeader) };

    if header.version != PROTOCOL_VERSION {
        return Err(StartupError::VersionMismatch);
    }

    let arg_count = header.arg_count as usize;
    let lengths_size = arg_count * 2;

    if buf.len() < header_size + lengths_size {
        return Err(StartupError::InvalidMessage);
    }

    // Read argument lengths
    let mut lengths = Vec::with_capacity(arg_count);
    let mut offset = header_size;
    for _ in 0..arg_count {
        let len = u16::from_le_bytes([buf[offset], buf[offset + 1]]) as usize;
        lengths.push(len);
        offset += 2;
    }

    // Validate total string size
    let strings_size: usize = lengths.iter().sum();
    if buf.len() < header_size + lengths_size + strings_size {
        return Err(StartupError::InvalidMessage);
    }

    // Read argument strings
    let mut args = Vec::with_capacity(arg_count);
    for len in lengths {
        let s = core::str::from_utf8(&buf[offset..offset + len])
            .map_err(|_| StartupError::InvalidMessage)?;
        args.push(String::from(s));
        offset += len;
    }

    Ok(args)
}

/// Calculate the size needed to encode the given arguments.
pub fn encoded_size(args: &[&str]) -> usize {
    let header_size = core::mem::size_of::<StartupMessageHeader>();
    let lengths_size = args.len() * 2;
    let strings_size: usize = args.iter().map(|s| s.len()).sum();
    header_size + lengths_size + strings_size
}

/// Receive startup arguments from the parent process.
///
/// This should be called early in program startup to receive the
/// arguments sent by the parent via the HANDLE_PARENT channel.
///
/// Blocks until the startup message is received.
/// Returns an empty Vec if no parent channel exists or the message is invalid.
pub fn receive_args() -> Vec<String> {
    use crate::channel;
    use crate::handle::Handle;
    use panda_abi::HANDLE_PARENT;

    let parent = Handle::from(HANDLE_PARENT);
    let mut buf = [0u8; panda_abi::MAX_MESSAGE_SIZE];

    // Block waiting for the startup message from parent
    match channel::recv(parent, &mut buf) {
        Ok(len) => decode(&buf[..len]).unwrap_or_default(),
        Err(_) => Vec::new(),
    }
}
