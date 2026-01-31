//! Channel operation syscall handlers (OP_CHANNEL_*).

#![deny(unsafe_code)]

use alloc::boxed::Box;
use alloc::sync::Arc;
use alloc::vec;
use core::task::Poll;

use log::debug;
use panda_abi::{CHANNEL_NONBLOCK, HandleType};

use crate::resource::{ChannelError, Resource};
use crate::scheduler;

use super::poll_fn;
use super::user_ptr::{SyscallError, SyscallFuture, SyscallResult, UserAccess, UserPtr, UserSlice};

/// Handle channel create operation.
/// Creates a pair of connected channel endpoints and returns handles to both.
///
/// Arguments:
/// - out_handles_ptr: Pointer to array of two u64s to receive handle IDs [endpoint_a, endpoint_b]
///
/// Returns 0 on success, negative error code on failure.
pub fn handle_create(ua: &UserAccess, out_handles_ptr: usize) -> SyscallFuture {
    use crate::resource::ChannelEndpoint;

    debug!("channel_create: out_handles_ptr={:#x}", out_handles_ptr);

    // Create the channel pair
    let (endpoint_a, endpoint_b) = ChannelEndpoint::create_pair();

    // Insert both endpoints into the current process's handle table
    let (handle_a, handle_b) = scheduler::with_current_process(|proc| {
        let handle_a = proc
            .handles_mut()
            .insert_typed(HandleType::Channel, Arc::new(endpoint_a));
        let handle_b = proc
            .handles_mut()
            .insert_typed(HandleType::Channel, Arc::new(endpoint_b));
        (handle_a, handle_b)
    });

    // Write the handle IDs to userspace
    let result = ua.write_user(UserPtr::<[u64; 2]>::new(out_handles_ptr), &[handle_a, handle_b]);

    let code = match result {
        Ok(_) => {
            debug!(
                "channel_create: created handles {} and {}",
                handle_a, handle_b
            );
            0
        }
        Err(_) => -1,
    };

    Box::pin(core::future::ready(SyscallResult::ok(code)))
}

/// Get the channel endpoint from a handle, returning a cloned Arc.
/// This allows us to call methods on the channel outside of with_current_process.
fn get_channel(handle: u64) -> Option<Arc<dyn Resource>> {
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
    ua: &UserAccess,
    handle: u64,
    buf_ptr: usize,
    buf_len: usize,
    flags: usize,
) -> Result<SyscallFuture, SyscallError> {
    let flags = flags as u32;

    debug!(
        "channel_send: handle={}, buf_len={}, flags={}",
        handle, buf_len, flags
    );

    // Copy message data from userspace NOW, while page table is active.
    let msg = ua.read(UserSlice::new(buf_ptr, buf_len))?;

    let resource = get_channel(handle);

    // Future only captures msg (Vec<u8>) and resource (Arc).
    // ua is NOT captured — compiler enforces this since UserAccess is !Send.
    Ok(Box::pin(poll_fn(move |_cx| {
        let Some(ref resource) = resource else {
            return Poll::Ready(SyscallResult::err(-1));
        };
        let Some(channel) = resource.as_channel() else {
            return Poll::Ready(SyscallResult::err(-1));
        };

        match channel.send(&msg) {
            Ok(()) => {
                debug!("channel_send: sent successfully");
                Poll::Ready(SyscallResult::ok(0))
            }
            Err(ChannelError::QueueFull) => {
                if flags & CHANNEL_NONBLOCK != 0 {
                    return Poll::Ready(SyscallResult::err(-1));
                }
                debug!("channel_send: queue full, blocking...");
                channel.waker().set_waiting(scheduler::current_process_id());
                Poll::Pending
            }
            Err(ChannelError::MessageTooLarge) => Poll::Ready(SyscallResult::err(-2)),
            Err(ChannelError::PeerClosed) => Poll::Ready(SyscallResult::err(-3)),
            Err(_) => Poll::Ready(SyscallResult::err(-4)),
        }
    })))
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
pub fn handle_recv(handle: u64, buf_ptr: usize, buf_len: usize, flags: usize) -> SyscallFuture {
    let flags = flags as u32;
    let dst = UserSlice::new(buf_ptr, buf_len);

    debug!(
        "channel_recv: handle={}, buf_len={}, flags={}",
        handle, buf_len, flags
    );

    let resource = get_channel(handle);

    Box::pin(poll_fn(move |_cx| {
        let Some(ref resource) = resource else {
            return Poll::Ready(SyscallResult::err(-1));
        };
        let Some(channel) = resource.as_channel() else {
            return Poll::Ready(SyscallResult::err(-1));
        };

        // Cap allocation to MAX_MESSAGE_SIZE (messages can never exceed this)
        let alloc_len = dst.len().min(panda_abi::MAX_MESSAGE_SIZE);
        let mut kernel_buf = vec![0u8; alloc_len];
        match channel.recv(&mut kernel_buf) {
            Ok(len) => {
                debug!("channel_recv: received {} bytes", len);
                kernel_buf.truncate(len);
                // Return data + destination for top-level to copy out
                Poll::Ready(SyscallResult::write_back(len as isize, kernel_buf, dst))
            }
            Err(ChannelError::QueueEmpty) => {
                if flags & CHANNEL_NONBLOCK != 0 {
                    return Poll::Ready(SyscallResult::err(-1));
                }
                debug!("channel_recv: queue empty, blocking...");
                channel.waker().set_waiting(scheduler::current_process_id());
                Poll::Pending
            }
            Err(ChannelError::BufferTooSmall) => Poll::Ready(SyscallResult::err(-2)),
            Err(ChannelError::PeerClosed) => Poll::Ready(SyscallResult::err(-3)),
            Err(_) => Poll::Ready(SyscallResult::err(-4)),
        }
    }))
}
