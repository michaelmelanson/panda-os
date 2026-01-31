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
        Handle::from(0u64), // handle arg unused for create
        OP_MAILBOX_CREATE,
        0,
        0,
        0,
        0,
    )
}

/// Wait for an event on a mailbox (blocking).
///
/// Writes the result to the provided `MailboxEventResult` struct.
/// Returns 0 on success, negative error code on failure.
#[inline(always)]
pub fn wait(mailbox: Handle, result: &mut MailboxEventResult) -> isize {
    send(
        mailbox,
        OP_MAILBOX_WAIT,
        result as *mut MailboxEventResult as usize,
        0,
        0,
        0,
    )
}

/// Poll for an event on a mailbox (non-blocking).
///
/// Writes the result to the provided `MailboxEventResult` struct.
/// Returns 1 if an event was available, 0 if no events, negative on error.
#[inline(always)]
pub fn poll(mailbox: Handle, result: &mut MailboxEventResult) -> isize {
    send(
        mailbox,
        OP_MAILBOX_POLL,
        result as *mut MailboxEventResult as usize,
        0,
        0,
        0,
    )
}
