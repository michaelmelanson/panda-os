//! Startup message protocol for passing arguments and environment to child processes.
//!
//! When a process is spawned, the parent sends a startup message over
//! the HANDLE_PARENT channel containing command-line arguments and environment variables.
//!
//! Message format (using panda_abi::encoding):
//! - version: u16
//! - args: Vec<String>
//! - env: Vec<(String, String)>

use alloc::string::String;
use alloc::vec::Vec;
use panda_abi::encoding::{Decoder, Encoder};

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
}

/// Encode arguments into a startup message (no environment variables).
///
/// Returns the number of bytes written to `buf`.
///
/// # Examples
///
/// ```
/// use libpanda::startup::{encode, decode, StartupError};
///
/// let mut buf = [0u8; 256];
/// let len = encode(&["hello", "world"], &mut buf).unwrap();
/// let args = decode(&buf[..len]).unwrap();
/// assert_eq!(args, vec!["hello", "world"]);
/// ```
///
/// ```
/// use libpanda::startup::{encode, StartupError};
///
/// // Buffer too small
/// let mut buf = [0u8; 4];
/// assert_eq!(encode(&["test"], &mut buf), Err(StartupError::BufferTooSmall));
/// ```
pub fn encode(args: &[&str], buf: &mut [u8]) -> Result<usize, StartupError> {
    encode_with_env(args, &[], buf)
}

/// Encode arguments and environment variables into a startup message.
///
/// Returns the number of bytes written to `buf`.
///
/// # Examples
///
/// ```
/// use libpanda::startup::{encode_with_env, decode_full};
///
/// let mut buf = [0u8; 256];
/// let len = encode_with_env(
///     &["prog", "arg1"],
///     &[("KEY", "value")],
///     &mut buf
/// ).unwrap();
///
/// let (args, env) = decode_full(&buf[..len]).unwrap();
/// assert_eq!(args, vec!["prog", "arg1"]);
/// assert_eq!(env, vec![("KEY".into(), "value".into())]);
/// ```
///
/// ```
/// use libpanda::startup::{encode_with_env, StartupError};
///
/// // Buffer too small for environment variables
/// let mut buf = [0u8; 16];
/// let result = encode_with_env(&["a"], &[("LONG_KEY", "long_value")], &mut buf);
/// assert_eq!(result, Err(StartupError::BufferTooSmall));
/// ```
pub fn encode_with_env(
    args: &[&str],
    env: &[(&str, &str)],
    buf: &mut [u8],
) -> Result<usize, StartupError> {
    let mut enc = Encoder::new();

    // Write version
    enc.write_u16(PROTOCOL_VERSION);

    // Write args as Vec<String>
    enc.write_u16(args.len() as u16);
    for arg in args {
        enc.write_string(arg);
    }

    // Write env as Vec<(String, String)>
    enc.write_u16(env.len() as u16);
    for (key, value) in env {
        enc.write_string(key);
        enc.write_string(value);
    }

    let encoded = enc.finish();
    if encoded.len() > buf.len() {
        return Err(StartupError::BufferTooSmall);
    }

    buf[..encoded.len()].copy_from_slice(&encoded);
    Ok(encoded.len())
}

/// Decode a startup message into arguments (ignoring environment variables).
///
/// Returns a vector of argument strings.
pub fn decode(buf: &[u8]) -> Result<Vec<String>, StartupError> {
    let (args, _env) = decode_full(buf)?;
    Ok(args)
}

/// Decode a startup message into arguments and environment variables.
///
/// Returns both arguments and environment variables.
///
/// # Examples
///
/// ```
/// use libpanda::startup::{decode_full, StartupError};
///
/// // Truncated message (too short for header)
/// assert_eq!(decode_full(&[]), Err(StartupError::InvalidMessage));
/// assert_eq!(decode_full(&[1]), Err(StartupError::InvalidMessage));
/// ```
///
/// ```
/// use libpanda::startup::{decode_full, StartupError};
///
/// // Wrong protocol version
/// let bad_version = [0x00, 0x00, 0x00, 0x00, 0x00, 0x00]; // version 0
/// assert_eq!(decode_full(&bad_version), Err(StartupError::VersionMismatch));
/// ```
///
/// ```
/// use libpanda::startup::{decode_full, encode_with_env, StartupError};
///
/// // Truncated env (cut off mid-message)
/// let mut buf = [0u8; 256];
/// let len = encode_with_env(&["arg"], &[("KEY", "value")], &mut buf).unwrap();
/// // Truncate before env value completes
/// assert_eq!(decode_full(&buf[..len - 3]), Err(StartupError::InvalidMessage));
/// ```
pub fn decode_full(buf: &[u8]) -> Result<(Vec<String>, Vec<(String, String)>), StartupError> {
    let mut dec = Decoder::new(buf);

    // Read version
    let version = dec.read_u16().map_err(|_| StartupError::InvalidMessage)?;
    if version != PROTOCOL_VERSION {
        return Err(StartupError::VersionMismatch);
    }

    // Read args
    let arg_count = dec.read_u16().map_err(|_| StartupError::InvalidMessage)? as usize;
    let mut args = Vec::with_capacity(arg_count);
    for _ in 0..arg_count {
        args.push(
            dec.read_string()
                .map_err(|_| StartupError::InvalidMessage)?,
        );
    }

    // Read env
    let env_count = dec.read_u16().map_err(|_| StartupError::InvalidMessage)? as usize;
    let mut env = Vec::with_capacity(env_count);
    for _ in 0..env_count {
        let key = dec
            .read_string()
            .map_err(|_| StartupError::InvalidMessage)?;
        let value = dec
            .read_string()
            .map_err(|_| StartupError::InvalidMessage)?;
        env.push((key, value));
    }

    Ok((args, env))
}

/// Calculate the size needed to encode the given arguments.
pub fn encoded_size(args: &[&str]) -> usize {
    encoded_size_with_env(args, &[])
}

/// Calculate the size needed to encode arguments and environment variables.
pub fn encoded_size_with_env(args: &[&str], env: &[(&str, &str)]) -> usize {
    let mut size = 2; // version

    // Args: count (u16) + for each: length (u16) + bytes
    size += 2;
    for arg in args {
        size += 2 + arg.len();
    }

    // Env: count (u16) + for each: key_len (u16) + key + value_len (u16) + value
    size += 2;
    for (key, value) in env {
        size += 2 + key.len() + 2 + value.len();
    }

    size
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
