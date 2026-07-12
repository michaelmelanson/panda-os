//! Channel operation syscall handlers (OP_CHANNEL_*).

#![deny(unsafe_code)]

use alloc::boxed::Box;
use alloc::sync::Arc;
use alloc::vec;
use core::task::Poll;

use log::debug;
use panda_abi::{CHANNEL_NONBLOCK, HandleType};

use crate::resource::{self, ChannelError};
use crate::scheduler;

use super::helpers::{downcast_or_invalid, resolve_resource};
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
    let result = scheduler::with_current_process(|proc| {
        let handle_a = proc
            .handles_mut()
            .insert_typed(HandleType::Channel, Arc::new(endpoint_a))
            .ok()?;
        let handle_b = match proc
            .handles_mut()
            .insert_typed(HandleType::Channel, Arc::new(endpoint_b))
        {
            Ok(id) => id,
            Err(_) => {
                proc.handles_mut().remove(handle_a);
                return None;
            }
        };
        Some((handle_a, handle_b))
    });

    let Some((handle_a, handle_b)) = result else {
        return Box::pin(core::future::ready(SyscallResult::err(
            panda_abi::ErrorCode::TooManyHandles,
        )));
    };

    // Write the handle IDs to userspace
    let result = ua.write_user(
        UserPtr::<[u64; 2]>::new(out_handles_ptr),
        &[handle_a, handle_b],
    );

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

/// Handle channel send operation.
/// Sends a message to the channel peer, optionally attaching a handle.
///
/// Arguments:
/// - handle: The channel handle
/// - buf_ptr: Pointer to message data
/// - buf_len: Length of message
/// - flags: CHANNEL_NONBLOCK to fail instead of blocking if queue full
/// - attach_handle: 0 = no attachment, else a handle in the caller's table to
///   duplicate-transfer to the receiver (see docs/SYSCALLS.md "Handle
///   transfer"). Resolved and whitelist-checked synchronously, so a bad or
///   non-transferable handle fails the syscall immediately rather than on a
///   later poll.
///
/// Returns 0 on success, negative error code on failure.
pub fn handle_send(
    ua: &UserAccess,
    handle: u64,
    buf_ptr: usize,
    buf_len: usize,
    flags: usize,
    attach_handle: u64,
) -> Result<SyscallFuture, SyscallError> {
    let flags = flags as u32;

    debug!(
        "channel_send: handle={}, buf_len={}, flags={}, attach_handle={}",
        handle, buf_len, flags, attach_handle
    );

    // Copy message data from userspace NOW, while page table is active.
    let msg = ua.read(UserSlice::new(buf_ptr, buf_len))?;

    // Resolve and whitelist-check the attached handle now, before building
    // the future. The sender keeps its own handle either way — attaching
    // clones the Arc rather than removing it from the sender's table (see
    // the doc comment on `resource::ChannelMessage`).
    let attachment = if attach_handle == 0 {
        None
    } else {
        let attached = scheduler::with_current_process(|proc| {
            proc.handles().get(attach_handle).map(|h| h.resource_arc())
        });
        match attached {
            Some(res) if resource::is_transferable(&res) => Some(res),
            _ => return Err(SyscallError::InvalidHandle),
        }
    };

    let resource = resolve_resource(handle, |h| h.as_channel().is_some());

    // Future only captures msg (Vec<u8>), attachment (Option<Arc>), and
    // resource (Arc). ua is NOT captured — compiler enforces this since
    // UserAccess is !Send.
    Ok(Box::pin(poll_fn(move |_cx| {
        let Some(channel) = downcast_or_invalid(&resource, |r| r.as_channel()) else {
            return Poll::Ready(SyscallResult::err(panda_abi::ErrorCode::InvalidHandle));
        };

        match channel.send_with_attachment(&msg, attachment.clone()) {
            Ok(()) => {
                debug!("channel_send: sent successfully");
                Poll::Ready(SyscallResult::ok(0))
            }
            Err(ChannelError::QueueFull) => {
                if flags & CHANNEL_NONBLOCK != 0 {
                    return Poll::Ready(SyscallResult::err(panda_abi::ErrorCode::WouldBlock));
                }
                debug!("channel_send: queue full, blocking...");
                channel.waker().set_waiting(scheduler::current_process_id());
                Poll::Pending
            }
            Err(ChannelError::MessageTooLarge) => {
                Poll::Ready(SyscallResult::err(panda_abi::ErrorCode::MessageTooLarge))
            }
            Err(ChannelError::PeerClosed) => {
                Poll::Ready(SyscallResult::err(panda_abi::ErrorCode::ChannelClosed))
            }
            Err(_) => Poll::Ready(SyscallResult::err(panda_abi::ErrorCode::IoError)),
        }
    })))
}

/// Handle channel recv operation.
/// Receives a message from the channel peer, delivering any attached handle.
///
/// Arguments:
/// - handle: The channel handle
/// - buf_ptr: Pointer to buffer for message data
/// - buf_len: Length of buffer
/// - flags: CHANNEL_NONBLOCK to fail instead of blocking if queue empty
/// - out_handle_ptr: 0 = caller doesn't care, else the address of a `u64`
///   that receives the transferred handle id (0 if the message had no
///   attachment). See docs/SYSCALLS.md "Handle transfer".
///
/// Returns message length on success, negative error code on failure. If the
/// dequeued message carries an attachment but the receiver's handle table is
/// full, returns `TooManyHandles` and leaves the message queued (it is not
/// lost — the caller can free a handle and retry).
pub fn handle_recv(
    handle: u64,
    buf_ptr: usize,
    buf_len: usize,
    flags: usize,
    out_handle_ptr: usize,
) -> SyscallFuture {
    let flags = flags as u32;
    let dst = UserSlice::new(buf_ptr, buf_len);
    let out_handle_dst = (out_handle_ptr != 0).then(|| UserPtr::<u64>::new(out_handle_ptr));

    debug!(
        "channel_recv: handle={}, buf_len={}, flags={}, out_handle_ptr={:#x}",
        handle, buf_len, flags, out_handle_ptr
    );

    let resource = resolve_resource(handle, |h| h.as_channel().is_some());

    Box::pin(poll_fn(move |_cx| {
        let Some(channel) = downcast_or_invalid(&resource, |r| r.as_channel()) else {
            return Poll::Ready(SyscallResult::err(panda_abi::ErrorCode::InvalidHandle));
        };

        // If the queued message carries an attachment, check handle-table
        // capacity BEFORE popping it — recv_with_attachment has no way to
        // "un-pop" a message once its attachment has been handed to us, so
        // we must not dequeue at all if we can't install the attachment.
        if channel.peek_has_attachment() == Some(true) {
            let table_full = scheduler::with_current_process(|proc| proc.handles().is_full());
            if table_full {
                return Poll::Ready(SyscallResult::err(panda_abi::ErrorCode::TooManyHandles));
            }
        }

        // Cap allocation to MAX_MESSAGE_SIZE (messages can never exceed this)
        let alloc_len = dst.len().min(panda_abi::MAX_MESSAGE_SIZE);
        let mut kernel_buf = vec![0u8; alloc_len];
        match channel.recv_with_attachment(&mut kernel_buf) {
            Ok((len, attachment)) => {
                debug!("channel_recv: received {} bytes", len);
                kernel_buf.truncate(len);

                // Install the attachment into our own handle table. Capacity
                // was already verified above (single-core kernel, interrupts
                // disabled for the whole syscall, so nothing else could have
                // filled the table in between) — insert() failing here would
                // indicate 56-bit ID space exhaustion, not a capacity race.
                // In that unreachable-in-practice case we drop the
                // attachment rather than lose the already-dequeued message.
                let received_handle = attachment
                    .and_then(|res| {
                        scheduler::with_current_process(|proc| proc.handles_mut().insert(res).ok())
                    })
                    .unwrap_or(0);

                match out_handle_dst {
                    Some(ptr) => Poll::Ready(SyscallResult::write_back_with_handle(
                        len as isize,
                        kernel_buf,
                        dst,
                        ptr,
                        received_handle,
                    )),
                    None => Poll::Ready(SyscallResult::write_back(len as isize, kernel_buf, dst)),
                }
            }
            Err(ChannelError::QueueEmpty) => {
                if flags & CHANNEL_NONBLOCK != 0 {
                    return Poll::Ready(SyscallResult::err(panda_abi::ErrorCode::WouldBlock));
                }
                debug!("channel_recv: queue empty, blocking...");
                channel.waker().set_waiting(scheduler::current_process_id());
                Poll::Pending
            }
            Err(ChannelError::BufferTooSmall) => {
                Poll::Ready(SyscallResult::err(panda_abi::ErrorCode::BufferTooSmall))
            }
            Err(ChannelError::PeerClosed) => {
                Poll::Ready(SyscallResult::err(panda_abi::ErrorCode::ChannelClosed))
            }
            Err(_) => Poll::Ready(SyscallResult::err(panda_abi::ErrorCode::IoError)),
        }
    }))
}
