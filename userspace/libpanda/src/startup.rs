//! Startup message protocol for passing arguments and environment to child processes.
//!
//! When a process is spawned, the parent can send a startup message over
//! the HANDLE_PARENT channel containing command-line arguments and environment variables.
//!
//! Message format:
//! - StartupMessageHeader (8 bytes)
//! - [u16; arg_count] argument lengths
//! - Packed argument strings (no null terminators)
//! - [u16; env_count] key lengths
//! - [u16; env_count] value lengths
//! - Packed key strings (no null terminators)
//! - Packed value strings (no null terminators)

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
    /// Too many environment variables.
    TooManyEnvVars,
    /// Environment key or value too long.
    EnvTooLong,
}

/// Encode arguments into a startup message (no environment variables).
///
/// Returns the number of bytes written to `buf`.
pub fn encode(args: &[&str], buf: &mut [u8]) -> Result<usize, StartupError> {
    encode_with_env(args, &[], buf)
}

/// Encode arguments and environment variables into a startup message.
///
/// Returns the number of bytes written to `buf`.
pub fn encode_with_env(
    args: &[&str],
    env: &[(&str, &str)],
    buf: &mut [u8],
) -> Result<usize, StartupError> {
    if args.len() > u16::MAX as usize {
        return Err(StartupError::TooManyArgs);
    }
    if env.len() > u16::MAX as usize {
        return Err(StartupError::TooManyEnvVars);
    }

    // Calculate total size needed
    let header_size = core::mem::size_of::<StartupMessageHeader>();
    let arg_lengths_size = args.len() * 2; // u16 per arg
    let arg_strings_size: usize = args.iter().map(|s| s.len()).sum();
    let env_lengths_size = env.len() * 4; // u16 for key + u16 for value per env var
    let env_strings_size: usize = env.iter().map(|(k, v)| k.len() + v.len()).sum();
    let total_size =
        header_size + arg_lengths_size + arg_strings_size + env_lengths_size + env_strings_size;

    if buf.len() < total_size {
        return Err(StartupError::BufferTooSmall);
    }

    // Validate arg lengths
    for arg in args {
        if arg.len() > u16::MAX as usize {
            return Err(StartupError::ArgTooLong);
        }
    }

    // Validate env key/value lengths
    for (key, value) in env {
        if key.len() > u16::MAX as usize || value.len() > u16::MAX as usize {
            return Err(StartupError::EnvTooLong);
        }
    }

    // Write header
    let header = StartupMessageHeader {
        version: PROTOCOL_VERSION,
        arg_count: args.len() as u16,
        env_count: env.len() as u16,
        flags: 0,
    };
    // SAFETY: StartupMessageHeader is a repr(C) struct of 4 u16 fields (8 bytes
    // total) with no padding. Transmuting to [u8; 8] gives a well-defined byte
    // representation for serialization.
    let header_bytes: [u8; 8] = unsafe { core::mem::transmute(header) };
    buf[..header_size].copy_from_slice(&header_bytes);

    let mut offset = header_size;

    // Write argument lengths
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

    // Write environment key lengths
    for (key, _) in env {
        let len_bytes = (key.len() as u16).to_le_bytes();
        buf[offset..offset + 2].copy_from_slice(&len_bytes);
        offset += 2;
    }

    // Write environment value lengths
    for (_, value) in env {
        let len_bytes = (value.len() as u16).to_le_bytes();
        buf[offset..offset + 2].copy_from_slice(&len_bytes);
        offset += 2;
    }

    // Write environment key strings
    for (key, _) in env {
        buf[offset..offset + key.len()].copy_from_slice(key.as_bytes());
        offset += key.len();
    }

    // Write environment value strings
    for (_, value) in env {
        buf[offset..offset + value.len()].copy_from_slice(value.as_bytes());
        offset += value.len();
    }

    Ok(total_size)
}

/// Decode a startup message into arguments (ignoring environment variables).
///
/// Returns a vector of argument strings.
pub fn decode(buf: &[u8]) -> Result<Vec<String>, StartupError> {
    let (args, _env) = decode_full(buf)?;
    Ok(args)
}

/// Startup data containing arguments and environment variables.
pub struct StartupData {
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
}

/// Decode a startup message into arguments and environment variables.
///
/// Returns both arguments and environment variables.
pub fn decode_full(buf: &[u8]) -> Result<(Vec<String>, Vec<(String, String)>), StartupError> {
    let header_size = core::mem::size_of::<StartupMessageHeader>();

    if buf.len() < header_size {
        return Err(StartupError::InvalidMessage);
    }

    // Read header
    // SAFETY: We verified buf.len() >= header_size (8 bytes) above.
    // StartupMessageHeader is repr(C) with defined layout. The pointer is
    // derived from a valid slice, so alignment is at least 1 (u8). Since
    // StartupMessageHeader has alignment 2, we use ptr::read_unaligned to
    // handle potentially unaligned input buffers safely.
    let header: StartupMessageHeader =
        unsafe { core::ptr::read_unaligned(buf.as_ptr() as *const StartupMessageHeader) };

    if header.version != PROTOCOL_VERSION {
        return Err(StartupError::VersionMismatch);
    }

    let arg_count = header.arg_count as usize;
    let env_count = header.env_count as usize;

    let mut offset = header_size;

    // Read argument lengths
    let mut arg_lengths = Vec::with_capacity(arg_count);
    for _ in 0..arg_count {
        if offset + 2 > buf.len() {
            return Err(StartupError::InvalidMessage);
        }
        let len = u16::from_le_bytes([buf[offset], buf[offset + 1]]) as usize;
        arg_lengths.push(len);
        offset += 2;
    }

    // Read argument strings
    let mut args = Vec::with_capacity(arg_count);
    for len in &arg_lengths {
        if offset + len > buf.len() {
            return Err(StartupError::InvalidMessage);
        }
        let s = core::str::from_utf8(&buf[offset..offset + len])
            .map_err(|_| StartupError::InvalidMessage)?;
        args.push(String::from(s));
        offset += len;
    }

    // Read environment key lengths
    let mut key_lengths = Vec::with_capacity(env_count);
    for _ in 0..env_count {
        if offset + 2 > buf.len() {
            return Err(StartupError::InvalidMessage);
        }
        let len = u16::from_le_bytes([buf[offset], buf[offset + 1]]) as usize;
        key_lengths.push(len);
        offset += 2;
    }

    // Read environment value lengths
    let mut value_lengths = Vec::with_capacity(env_count);
    for _ in 0..env_count {
        if offset + 2 > buf.len() {
            return Err(StartupError::InvalidMessage);
        }
        let len = u16::from_le_bytes([buf[offset], buf[offset + 1]]) as usize;
        value_lengths.push(len);
        offset += 2;
    }

    // Read environment key strings
    let mut keys = Vec::with_capacity(env_count);
    for len in &key_lengths {
        if offset + len > buf.len() {
            return Err(StartupError::InvalidMessage);
        }
        let s = core::str::from_utf8(&buf[offset..offset + len])
            .map_err(|_| StartupError::InvalidMessage)?;
        keys.push(String::from(s));
        offset += len;
    }

    // Read environment value strings
    let mut env = Vec::with_capacity(env_count);
    for (i, len) in value_lengths.iter().enumerate() {
        if offset + len > buf.len() {
            return Err(StartupError::InvalidMessage);
        }
        let s = core::str::from_utf8(&buf[offset..offset + len])
            .map_err(|_| StartupError::InvalidMessage)?;
        env.push((keys[i].clone(), String::from(s)));
        offset += len;
    }

    Ok((args, env))
}

/// Calculate the size needed to encode the given arguments.
pub fn encoded_size(args: &[&str]) -> usize {
    encoded_size_with_env(args, &[])
}

/// Calculate the size needed to encode arguments and environment variables.
pub fn encoded_size_with_env(args: &[&str], env: &[(&str, &str)]) -> usize {
    let header_size = core::mem::size_of::<StartupMessageHeader>();
    let arg_lengths_size = args.len() * 2;
    let arg_strings_size: usize = args.iter().map(|s| s.len()).sum();
    let env_lengths_size = env.len() * 4;
    let env_strings_size: usize = env.iter().map(|(k, v)| k.len() + v.len()).sum();
    header_size + arg_lengths_size + arg_strings_size + env_lengths_size + env_strings_size
}

/// Receive startup arguments from the parent process.
///
/// This should be called early in program startup to receive the
/// arguments sent by the parent via the HANDLE_PARENT channel.
///
/// Blocks until the startup message is received.
/// Returns an empty Vec if no parent channel exists or the message is invalid.
pub fn receive_args() -> Vec<String> {
    let (args, _env) = receive_startup();
    args
}

/// Receive startup arguments and environment from the parent process.
///
/// This should be called early in program startup to receive the
/// startup message sent by the parent via the HANDLE_PARENT channel.
///
/// Blocks until the startup message is received.
/// Returns empty Vecs if no parent channel exists or the message is invalid.
pub fn receive_startup() -> (Vec<String>, Vec<(String, String)>) {
    use crate::channel;
    use crate::handle::Handle;
    use panda_abi::HANDLE_PARENT;

    let parent = Handle::from(HANDLE_PARENT);
    let mut buf = [0u8; panda_abi::MAX_MESSAGE_SIZE];

    // Block waiting for the startup message from parent
    match channel::recv(parent, &mut buf) {
        Ok(len) => decode_full(&buf[..len]).unwrap_or_default(),
        Err(_) => (Vec::new(), Vec::new()),
    }
}
