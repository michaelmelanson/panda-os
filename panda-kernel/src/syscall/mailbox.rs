//! Mailbox operation syscall handlers (OP_MAILBOX_*).

#![deny(unsafe_code)]

use alloc::boxed::Box;
use core::task::Poll;

use panda_abi::HandleType;

use crate::resource::Mailbox;
use crate::scheduler;

use super::poll_fn;
use super::user_ptr::{SyscallFuture, SyscallResult};

/// Handle mailbox create operation.
/// Returns a new mailbox handle.
pub fn handle_create() -> SyscallFuture {
    let handle_id = scheduler::with_current_process(|proc| {
        let mailbox = Mailbox::new();
        proc.handles_mut()
            .insert_typed(HandleType::Mailbox, mailbox)
    });
    Box::pin(core::future::ready(SyscallResult::ok(handle_id as isize)))
}

/// Handle mailbox wait operation (blocking).
/// Waits for an event on any handle attached to the mailbox.
/// Returns packed (handle_id, events) in the result.
///
/// The result is encoded as: (handle_id << 32) | events
/// If no events are available, blocks until one arrives.
pub fn handle_wait(mailbox_handle: u32) -> SyscallFuture {
    let resource = scheduler::with_current_process(|proc| {
        let handle = proc.handles().get(mailbox_handle)?;
        if handle.as_mailbox().is_some() {
            Some(handle.resource_arc())
        } else {
            None
        }
    });

    Box::pin(poll_fn(move |_cx| {
        let Some(ref resource) = resource else {
            return Poll::Ready(SyscallResult::err(-1));
        };
        let Some(mailbox) = resource.as_mailbox() else {
            return Poll::Ready(SyscallResult::err(-1));
        };

        if let Some((handle_id, events)) = mailbox.wait() {
            let result = ((handle_id as isize) << 32) | (events as isize);
            Poll::Ready(SyscallResult::ok(result))
        } else {
            // No events, block until one arrives
            mailbox.waker().set_waiting(scheduler::current_process_id());
            Poll::Pending
        }
    }))
}

/// Handle mailbox poll operation (non-blocking).
/// Returns packed (handle_id, events) or 0 if no events.
///
/// The result is encoded as: (handle_id << 32) | events
pub fn handle_poll(mailbox_handle: u32) -> SyscallFuture {
    let result = scheduler::with_current_process(|proc| {
        let handle = proc.handles().get(mailbox_handle)?;
        let mailbox = handle.as_mailbox()?;
        Some(mailbox.poll())
    });

    let code = match result {
        Some(Some((handle_id, events))) => ((handle_id as isize) << 32) | (events as isize),
        Some(None) => 0,
        None => -1,
    };
    Box::pin(core::future::ready(SyscallResult::ok(code)))
}
