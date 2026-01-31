//! Mailbox operation syscall handlers (OP_MAILBOX_*).

#![deny(unsafe_code)]

use alloc::boxed::Box;
use core::task::Poll;

use panda_abi::HandleType;

use crate::resource::Mailbox;
use crate::scheduler;

use super::poll_fn;
use super::user_ptr::{SyscallFuture, SyscallResult, UserAccess, UserPtr};

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
/// Writes the result to a userspace MailboxEventResult struct via out-pointer.
///
/// Arguments:
/// - mailbox_handle: The mailbox handle
/// - out_ptr: Pointer to MailboxEventResult struct in userspace
///
/// Returns 0 on success, negative error code on failure.
/// If no events are available, blocks until one arrives.
pub fn handle_wait(_ua: &UserAccess, mailbox_handle: u64, out_ptr: usize) -> SyscallFuture {
    if out_ptr == 0 {
        return Box::pin(core::future::ready(SyscallResult::err(-1)));
    }

    let dst = super::user_ptr::UserSlice::new(
        out_ptr,
        core::mem::size_of::<panda_abi::MailboxEventResult>(),
    );

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
            let event_result = panda_abi::MailboxEventResult {
                handle_id,
                events,
                _pad: 0,
            };
            Poll::Ready(SyscallResult::write_back_struct(0, &event_result, dst))
        } else {
            // No events, block until one arrives
            mailbox.waker().set_waiting(scheduler::current_process_id());
            Poll::Pending
        }
    }))
}

/// Handle mailbox poll operation (non-blocking).
/// Writes the result to a userspace MailboxEventResult struct via out-pointer.
///
/// Arguments:
/// - mailbox_handle: The mailbox handle
/// - out_ptr: Pointer to MailboxEventResult struct in userspace
///
/// Returns 1 if an event was available, 0 if no events, negative on error.
pub fn handle_poll(ua: &UserAccess, mailbox_handle: u64, out_ptr: usize) -> SyscallFuture {
    if out_ptr == 0 {
        return Box::pin(core::future::ready(SyscallResult::err(-1)));
    }

    let result = scheduler::with_current_process(|proc| {
        let handle = proc.handles().get(mailbox_handle)?;
        let mailbox = handle.as_mailbox()?;
        Some(mailbox.poll())
    });

    match result {
        Some(Some((handle_id, events))) => {
            let event_result = panda_abi::MailboxEventResult {
                handle_id,
                events,
                _pad: 0,
            };
            match ua.write_user(UserPtr::new(out_ptr), &event_result) {
                Ok(_) => Box::pin(core::future::ready(SyscallResult::ok(1))),
                Err(_) => Box::pin(core::future::ready(SyscallResult::err(-1))),
            }
        }
        Some(None) => Box::pin(core::future::ready(SyscallResult::ok(0))),
        None => Box::pin(core::future::ready(SyscallResult::err(-1))),
    }
}
