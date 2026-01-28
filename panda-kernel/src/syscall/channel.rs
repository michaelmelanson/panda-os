//! Channel operation syscall handlers (OP_CHANNEL_*).

use alloc::sync::Arc;
use core::slice;

use log::debug;
use panda_abi::CHANNEL_NONBLOCK;

use crate::resource::{ChannelEndpoint, ChannelError, Resource};
use crate::scheduler;

use super::SyscallContext;

/// Handle channel create operation.
/// Creates a pair of connected channel endpoints and returns handles to both.
///
/// Arguments:
/// - out_handles_ptr: Pointer to array of two u32s to receive handle IDs [endpoint_a, endpoint_b]
///
/// Returns 0 on success, negative error code on failure.
pub fn handle_create(out_handles_ptr: usize) -> isize {
    debug!("channel_create: out_handles_ptr={:#x}", out_handles_ptr);

    // Create the channel pair
    let (endpoint_a, endpoint_b) = ChannelEndpoint::create_pair();

    // Insert both endpoints into the current process's handle table
    let result = scheduler::with_current_process(|proc| {
        let handle_a = proc.handles_mut().insert(Arc::new(endpoint_a));
        let handle_b = proc.handles_mut().insert(Arc::new(endpoint_b));
        (handle_a, handle_b)
    });

    // Write the handle IDs to userspace
    let out_handles = out_handles_ptr as *mut [u32; 2];
    unsafe {
        (*out_handles)[0] = result.0;
        (*out_handles)[1] = result.1;
    }

    debug!(
        "channel_create: created handles {} and {}",
        result.0, result.1
    );
    0
}

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

    // Scope the resource Arc so it's dropped before any potential block_on call.
    // block_on never returns (it switches to another process), so stack locals
    // aren't dropped - we must ensure the Arc is released before blocking.
    let waker = {
        let Some(resource) = get_channel(handle) else {
            return -1; // Invalid handle
        };
        let Some(channel) = resource.as_channel() else {
            return -1; // Not a channel
        };

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
                channel.waker()
            }
            Err(ChannelError::MessageTooLarge) => return -2,
            Err(ChannelError::PeerClosed) => return -3,
            Err(_) => return -4,
        }
    };
    // resource Arc is now dropped, safe to block
    ctx.block_on(waker);
    // block_on doesn't return - when resumed, syscall restarts from beginning
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

    // Scope the resource Arc so it's dropped before any potential block_on call.
    // block_on never returns (it switches to another process), so stack locals
    // aren't dropped - we must ensure the Arc is released before blocking.
    let waker = {
        let Some(resource) = get_channel(handle) else {
            return -1; // Invalid handle
        };
        let Some(channel) = resource.as_channel() else {
            return -1; // Not a channel
        };

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
                channel.waker()
            }
            Err(ChannelError::BufferTooSmall) => return -2,
            Err(ChannelError::PeerClosed) => return -3,
            Err(_) => return -4,
        }
    };
    // resource Arc is now dropped, safe to block
    ctx.block_on(waker);
    // block_on doesn't return - when resumed, syscall restarts from beginning
}
