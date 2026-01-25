//! Channel operation syscall handlers (OP_CHANNEL_*).

use alloc::sync::Arc;
use core::slice;

use log::debug;
use panda_abi::CHANNEL_NONBLOCK;

use crate::resource::{ChannelError, Resource};
use crate::scheduler;

use super::SyscallContext;

/// Get the channel endpoint from a handle, returning a cloned Arc.
/// This allows us to call methods on the channel outside of with_current_process.
fn get_channel(handle: u32) -> Option<Arc<dyn Resource>> {
    scheduler::with_current_process(|proc| {
        let h = proc.handles().get(handle)?;
        // Check that it's actually a channel
        if h.as_channel().is_some() {
            Some(h.resource_arc())
        } else {
            None
        }
    })
}

/// Handle channel send operation.
/// Sends a message to the channel peer.
///
/// Arguments:
/// - handle: The channel handle
/// - buf_ptr: Pointer to message data
/// - buf_len: Length of message
/// - flags: CHANNEL_NONBLOCK to fail instead of blocking if queue full
///
/// Returns 0 on success, negative error code on failure.
pub fn handle_send(
    ctx: &SyscallContext,
    handle: u32,
    buf_ptr: usize,
    buf_len: usize,
    flags: usize,
) -> isize {
    let flags = flags as u32;
    let buf = unsafe { slice::from_raw_parts(buf_ptr as *const u8, buf_len) };

    debug!(
        "channel_send: handle={}, buf_len={}, flags={}",
        handle, buf_len, flags
    );

    // Get the channel Arc outside of with_current_process to avoid holding
    // the scheduler lock while calling send() (which may call waker.wake()).
    let Some(resource) = get_channel(handle) else {
        return -1; // Invalid handle
    };
    let Some(channel) = resource.as_channel() else {
        return -1; // Not a channel
    };

    loop {
        match channel.send(buf) {
            Ok(()) => {
                debug!("channel_send: sent successfully");
                return 0;
            }
            Err(ChannelError::QueueFull) => {
                if flags & CHANNEL_NONBLOCK != 0 {
                    return -1; // Non-blocking mode: return error
                }
                debug!("channel_send: queue full, blocking...");
                // Blocking mode: wait for space
                let waker = channel.waker();
                ctx.block_on(waker);
                // block_on doesn't return - when resumed, syscall restarts from beginning
            }
            Err(ChannelError::MessageTooLarge) => return -2,
            Err(ChannelError::PeerClosed) => return -3,
            Err(_) => return -4,
        }
    }
}

/// Handle channel recv operation.
/// Receives a message from the channel peer.
///
/// Arguments:
/// - handle: The channel handle
/// - buf_ptr: Pointer to buffer for message data
/// - buf_len: Length of buffer
/// - flags: CHANNEL_NONBLOCK to fail instead of blocking if queue empty
///
/// Returns message length on success, negative error code on failure.
pub fn handle_recv(
    ctx: &SyscallContext,
    handle: u32,
    buf_ptr: usize,
    buf_len: usize,
    flags: usize,
) -> isize {
    let flags = flags as u32;
    let buf = unsafe { slice::from_raw_parts_mut(buf_ptr as *mut u8, buf_len) };

    debug!(
        "channel_recv: handle={}, buf_len={}, flags={}",
        handle, buf_len, flags
    );

    // Get the channel Arc outside of with_current_process to avoid holding
    // the scheduler lock while calling recv() (which may call waker.wake()).
    let Some(resource) = get_channel(handle) else {
        return -1; // Invalid handle
    };
    let Some(channel) = resource.as_channel() else {
        return -1; // Not a channel
    };

    loop {
        match channel.recv(buf) {
            Ok(len) => {
                debug!("channel_recv: received {} bytes", len);
                return len as isize;
            }
            Err(ChannelError::QueueEmpty) => {
                if flags & CHANNEL_NONBLOCK != 0 {
                    return -1; // Non-blocking mode: return error
                }
                debug!("channel_recv: queue empty, blocking...");
                // Blocking mode: wait for data
                let waker = channel.waker();
                ctx.block_on(waker);
                // block_on doesn't return - when resumed, syscall restarts from beginning
            }
            Err(ChannelError::BufferTooSmall) => return -2,
            Err(ChannelError::PeerClosed) => return -3,
            Err(_) => return -4,
        }
    }
}
