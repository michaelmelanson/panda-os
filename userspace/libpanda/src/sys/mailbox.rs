//! Low-level mailbox operations.
//!
//! These functions provide direct syscall access for event multiplexing.
//! For higher-level abstractions, use `crate::ipc::Mailbox`.

use super::{Handle, send};
use panda_abi::*;

/// Create a new mailbox.
///
/// Returns mailbox handle on success, or negative error code.
#[inline(always)]
pub fn create() -> isize {
    send(
        Handle::from(0), // handle arg unused for create
        OP_MAILBOX_CREATE,
        0,
        0,
        0,
        0,
    )
}

/// Wait for an event on a mailbox (blocking).
///
/// Returns packed result: `(handle_id << 32) | event_flags`.
#[inline(always)]
pub fn wait(mailbox: Handle) -> isize {
    send(mailbox, OP_MAILBOX_WAIT, 0, 0, 0, 0)
}

/// Poll for an event on a mailbox (non-blocking).
///
/// Returns packed result: `(handle_id << 32) | event_flags`, or 0 if no events.
#[inline(always)]
pub fn poll(mailbox: Handle) -> isize {
    send(mailbox, OP_MAILBOX_POLL, 0, 0, 0, 0)
}

/// Unpack a mailbox result into (handle_id, event_flags).
#[inline(always)]
pub fn unpack_result(result: isize) -> (u32, u32) {
    let handle_id = (result >> 32) as u32;
    let events = result as u32;
    (handle_id, events)
}
