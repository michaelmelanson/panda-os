//! Low-level channel operations.
//!
//! These functions provide direct syscall access for message-passing.
//! For higher-level abstractions, use `crate::ipc::Channel`.

use super::{Handle, send};
use panda_abi::*;

/// Create a channel pair.
///
/// Returns handles to both endpoints: (endpoint_a, endpoint_b).
/// Messages sent on endpoint_a are received by endpoint_b, and vice versa.
#[inline(always)]
pub fn create() -> (Handle, Handle) {
    let mut handles: [u32; 2] = [0, 0];
    let result = send(
        Handle::from(0u32), // No source handle for create
        OP_CHANNEL_CREATE,
        handles.as_mut_ptr() as usize,
        0,
        0,
        0,
    );
    if result < 0 {
        panic!("channel_create failed: {}", result);
    }
    (Handle::from(handles[0]), Handle::from(handles[1]))
}

/// Send a message on a channel (blocking if queue full).
///
/// Returns 0 on success, or negative error code.
#[inline(always)]
pub fn send_msg(handle: Handle, msg: &[u8]) -> isize {
    send(
        handle,
        OP_CHANNEL_SEND,
        msg.as_ptr() as usize,
        msg.len(),
        0, // flags = 0, blocking
        0,
    )
}

/// Send a message on a channel (non-blocking).
///
/// Returns 0 on success, or negative error code (e.g., queue full).
#[inline(always)]
pub fn try_send_msg(handle: Handle, msg: &[u8]) -> isize {
    send(
        handle,
        OP_CHANNEL_SEND,
        msg.as_ptr() as usize,
        msg.len(),
        CHANNEL_NONBLOCK as usize,
        0,
    )
}

/// Receive a message from a channel (blocking if queue empty).
///
/// Returns number of bytes received on success, or negative error code.
#[inline(always)]
pub fn recv_msg(handle: Handle, buf: &mut [u8]) -> isize {
    send(
        handle,
        OP_CHANNEL_RECV,
        buf.as_mut_ptr() as usize,
        buf.len(),
        0, // flags = 0, blocking
        0,
    )
}

/// Receive a message from a channel (non-blocking).
///
/// Returns number of bytes received on success, or negative error code (e.g., queue empty).
#[inline(always)]
pub fn try_recv_msg(handle: Handle, buf: &mut [u8]) -> isize {
    send(
        handle,
        OP_CHANNEL_RECV,
        buf.as_mut_ptr() as usize,
        buf.len(),
        CHANNEL_NONBLOCK as usize,
        0,
    )
}
