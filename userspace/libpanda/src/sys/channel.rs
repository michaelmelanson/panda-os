//! Low-level channel operations.
//!
//! These functions provide direct syscall access for message-passing.
//! For higher-level abstractions, use `crate::ipc::Channel`.

use super::{Handle, send};
use panda_abi::*;

/// Create a channel pair (raw syscall).
///
/// On success, writes two u64 handle IDs to the `handles` output parameter
/// and returns 0.
///
/// On failure, returns a negative error code.
///
/// Note: This is the raw syscall. Use `crate::ipc::channel::create_pair()` for
/// a safer interface that returns `Result<(ChannelHandle, ChannelHandle)>`.
#[inline(always)]
pub fn create_raw(handles: &mut [u64; 2]) -> isize {
    send(
        Handle::from(0u64), // No source handle for create
        OP_CHANNEL_CREATE,
        handles.as_mut_ptr() as usize,
        0,
        0,
        0,
    )
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
        0, // attach_handle = 0, no attachment
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
        0, // attach_handle = 0, no attachment
    )
}

/// Send a message on a channel with an attached handle (blocking if queue full).
///
/// The attached handle is duplicated into the receiver's handle table; the
/// sender's own handle remains valid afterwards. See
/// `crate::ipc::channel::send_with_handle` and docs/IPC.md "Handle transfer".
///
/// Returns 0 on success, or negative error code.
#[inline(always)]
pub fn send_msg_with_handle(handle: Handle, msg: &[u8], attach: Handle) -> isize {
    send(
        handle,
        OP_CHANNEL_SEND,
        msg.as_ptr() as usize,
        msg.len(),
        0, // flags = 0, blocking
        u64::from(attach) as usize,
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
        0, // out_handle_ptr = 0, caller doesn't care about an attachment
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
        0, // out_handle_ptr = 0, caller doesn't care about an attachment
    )
}

/// Receive a message from a channel, reporting any attached handle
/// (blocking if queue empty).
///
/// On success, `*out_handle` is set to the transferred handle id, or 0 if
/// the message carried no attachment. See
/// `crate::ipc::channel::recv_with_handle` and docs/IPC.md "Handle transfer".
///
/// Returns number of bytes received on success, or negative error code.
#[inline(always)]
pub fn recv_msg_with_handle(handle: Handle, buf: &mut [u8], out_handle: &mut u64) -> isize {
    send(
        handle,
        OP_CHANNEL_RECV,
        buf.as_mut_ptr() as usize,
        buf.len(),
        0, // flags = 0, blocking
        out_handle as *mut u64 as usize,
    )
}
