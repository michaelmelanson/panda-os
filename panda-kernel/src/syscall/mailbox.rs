//! Mailbox operation syscall handlers (OP_MAILBOX_*).

use crate::resource::Mailbox;
use crate::scheduler;

use super::SyscallContext;

/// Handle mailbox create operation.
/// Returns a new mailbox handle.
pub fn handle_create() -> isize {
    let mailbox = Mailbox::new();
    let handle_id = scheduler::with_current_process(|proc| proc.handles_mut().insert(mailbox));
    handle_id as isize
}

/// Handle mailbox wait operation (blocking).
/// Waits for an event on any handle attached to the mailbox.
/// Returns packed (handle_id, events) in the result.
///
/// The result is encoded as: (handle_id << 32) | events
/// If no events are available, blocks until one arrives.
pub fn handle_wait(ctx: &SyscallContext, mailbox_handle: u32) -> isize {
    // Get the mailbox
    let result = scheduler::with_current_process(|proc| {
        let handle = proc.handles().get(mailbox_handle)?;
        let mailbox = handle.as_mailbox()?;

        // Try to get a pending event
        if let Some((handle_id, events)) = mailbox.wait() {
            // Event available, return it
            Some(Ok((handle_id, events)))
        } else {
            // No events, need to block
            Some(Err(mailbox.waker()))
        }
    });

    match result {
        Some(Ok((handle_id, events))) => {
            // Pack handle_id and events into result
            ((handle_id as isize) << 32) | (events as isize)
        }
        Some(Err(waker)) => {
            // Block until an event arrives
            ctx.block_on(waker);
        }
        None => {
            // Invalid mailbox handle
            -1
        }
    }
}

/// Handle mailbox poll operation (non-blocking).
/// Returns packed (handle_id, events) or (0, 0) if no events.
///
/// The result is encoded as: (handle_id << 32) | events
pub fn handle_poll(mailbox_handle: u32) -> isize {
    let result = scheduler::with_current_process(|proc| {
        let handle = proc.handles().get(mailbox_handle)?;
        let mailbox = handle.as_mailbox()?;
        Some(mailbox.poll())
    });

    match result {
        Some(Some((handle_id, events))) => {
            // Pack handle_id and events into result
            ((handle_id as isize) << 32) | (events as isize)
        }
        Some(None) => {
            // No events available
            0
        }
        None => {
            // Invalid mailbox handle
            -1
        }
    }
}
