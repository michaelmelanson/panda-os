//! Shared boilerplate for syscall handlers.
//!
//! These helpers factor out patterns that recur across multiple handler modules:
//! reading a string from userspace with a uniform error future, resolving a handle
//! to a resource `Arc` for use inside a blocking `poll_fn` closure, and attaching a
//! freshly created handle to a mailbox for event notifications.
//!
//! Single-module-specific boilerplate (e.g. the VFS-file resolution in `file.rs`, or
//! the directory-op path-building prelude in `directory.rs`) stays local to those
//! modules rather than living here.

#![deny(unsafe_code)]

use alloc::boxed::Box;
use alloc::string::String;
use alloc::sync::Arc;

use crate::handle::Handle;
use crate::process::Process;
use crate::resource::{Mailbox, MailboxRef, Resource};
use crate::scheduler;

use super::user_ptr::{SyscallFuture, SyscallResult, UserAccess};

/// Read a UTF-8 string from userspace, or produce an `InvalidArgument` error future.
///
/// Centralises the `ua.read_str(...)` boilerplate: every call site wants the same
/// `InvalidArgument` result on failure, so this collapses a five-line match down to a
/// two-line one at the call site.
pub(super) fn read_user_str(ua: &UserAccess, ptr: usize, len: usize) -> Result<String, SyscallFuture> {
    ua.read_str(ptr, len).map_err(|_| {
        Box::pin(core::future::ready(SyscallResult::err(
            panda_abi::ErrorCode::InvalidArgument,
        ))) as SyscallFuture
    })
}

/// Resolve `handle_id` to a resource `Arc` if it passes `check`.
///
/// Performs a single handle-table lookup, cloning the resource `Arc` in the same
/// closure that runs `check` rather than looking the handle up a second time to
/// satisfy the borrow checker. Blocking handlers use this to grab a resource
/// *outside* the process lock before retrying the operation on each poll (the lock
/// cannot be held across a `poll_fn` closure's `Pending` return).
pub(super) fn resolve_resource(
    handle_id: u64,
    check: impl FnOnce(&Handle) -> bool,
) -> Option<Arc<dyn Resource>> {
    scheduler::with_current_process(|proc| {
        let handle = proc.handles().get(handle_id)?;
        check(handle).then(|| handle.resource_arc())
    })
}

/// Downcast a resource previously resolved by [`resolve_resource`] to the interface
/// expected inside a `poll_fn` closure.
///
/// Both "the handle disappeared before resolution" and "the resource doesn't support
/// this interface" collapse to the same `None`, matching the identical `InvalidHandle`
/// result that every blocking handler returns for either case.
pub(super) fn downcast_or_invalid<'r, T: ?Sized>(
    resource: &'r Option<Arc<dyn Resource>>,
    as_iface: impl FnOnce(&'r dyn Resource) -> Option<&'r T>,
) -> Option<&'r T> {
    as_iface(resource.as_ref()?.as_ref())
}

/// Attach `handle_id` to the mailbox referenced by `mailbox_handle`, if `mailbox_handle`
/// is non-zero and resolves to a mailbox resource.
///
/// Returns the mailbox on success so callers that need bidirectional attachment (so the
/// resource can post events back via `Resource::attach_mailbox`) can build a
/// [`MailboxRef`] from it. Callers that only want the one-way registration (mailbox ->
/// handle) can ignore the return value.
///
/// This does not check `event_mask` — some callers (e.g. directory create) always
/// attach with a mask of 0, while others require a non-zero mask before attaching at
/// all. That guard stays at the call site since it isn't uniform across callers.
pub(super) fn attach_to_mailbox<'p>(
    proc: &'p Process,
    mailbox_handle: u64,
    handle_id: u64,
    event_mask: u32,
) -> Option<&'p Mailbox> {
    if mailbox_handle == 0 {
        return None;
    }
    let mailbox = proc.handles().get(mailbox_handle)?.as_mailbox()?;
    mailbox.attach(handle_id, event_mask);
    Some(mailbox)
}

/// Build a [`MailboxRef`] for `mailbox` and attach it to `handle_id`'s resource, so the
/// resource can post events back to the mailbox.
///
/// Pairs with [`attach_to_mailbox`] for the two call sites (`handle_open`,
/// `handle_spawn`) that need bidirectional attachment.
pub(super) fn complete_mailbox_attach(proc: &Process, mailbox: &Mailbox, handle_id: u64) {
    if let Some(handle) = proc.handles().get(handle_id) {
        handle.attach_mailbox(MailboxRef::new(mailbox, handle_id));
    }
}
