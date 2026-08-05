//! Device subscribe/claim syscall handlers (`OP_DEVICE_SUBSCRIBE`,
//! `OP_DEVICE_CLAIM`).
//!
//! These are the only two `OP_DEVICE_*` operations with real kernel
//! handlers in this phase — `OP_DEVICE_MAP_MMIO`, `OP_DMA_ALLOC`,
//! `OP_DMA_FREE`, and `OP_DEVICE_SUBSCRIBE_IRQ` are defined in `panda_abi`
//! for ABI completeness but require IOMMU support (Phase 6) to implement
//! safely, so they are not dispatched here (see `syscall/mod.rs`, which
//! falls through to `NotSupported` for them, same as any unrecognised op).
//!
//! Known gap: `OP_DEVICE_SUBSCRIBE`'s replay (and later `EVENT_DEVICE_ADDED`
//! from `device::pci::register_enumerated_devices` / future hotplug) posts
//! the mailbox bit, but does not yet hand the matched `DeviceEvent` payload
//! (bus/identity/token) back to userspace — there is no syscall in the
//! plan's table for reading it off the subscription. `device::DeviceRegistry
//! ::subscribe` returns the replayed `(DeviceId, token)` pairs to its Rust
//! caller today (exercised directly by kernel tests); wiring a userspace
//! delivery path is left for the driver that actually needs it (Phase 7).

#![deny(unsafe_code)]

use alloc::boxed::Box;
use alloc::sync::Arc;

use panda_abi::HandleType;
use panda_abi::device::BusType;

use crate::device::{ClaimError, DEVICE_REGISTRY};
use crate::process::ProcessId;
use crate::resource::{MailboxRef, Resource};
use crate::scheduler;

use super::helpers::attach_to_mailbox;
use super::user_ptr::{SyscallFuture, SyscallResult, UserAccess, UserSlice};

/// Opaque handle installed in the caller's table when `OP_DEVICE_SUBSCRIBE`
/// succeeds. Carries no state of its own today — see the module doc comment
/// for what's deferred.
struct SubscriptionResource;

impl Resource for SubscriptionResource {
    fn handle_type(&self) -> HandleType {
        HandleType::DeviceSubscription
    }
}

/// Handle `OP_DEVICE_SUBSCRIBE(bus_type, match_ptr, match_len, mailbox_handle)`.
///
/// Subscribes the calling process to `bus_type` devices matching the raw
/// bytes at `match_ptr..match_ptr+match_len` (a bus-specific `*DeviceId`
/// struct — see `panda_abi::device`). If `mailbox_handle` is non-zero, the
/// new subscription handle is attached to that mailbox with
/// `EVENT_DEVICE_ADDED | EVENT_DEVICE_REMOVED`. Immediately replays
/// `EVENT_DEVICE_ADDED` for every currently-known matching device.
///
/// Returns the subscription handle, or `InvalidArgument` for an unknown bus
/// type or unreadable match buffer.
pub fn handle_device_subscribe(
    ua: &UserAccess,
    bus_type_raw: usize,
    match_ptr: usize,
    match_len: usize,
    mailbox_handle: usize,
) -> SyscallFuture {
    let Some(bus_type) = BusType::from_u32(bus_type_raw as u32) else {
        return Box::pin(core::future::ready(SyscallResult::err(
            panda_abi::ErrorCode::InvalidArgument,
        )));
    };

    let match_bytes = match ua.read(UserSlice::new(match_ptr, match_len)) {
        Ok(bytes) => bytes,
        Err(_) => {
            return Box::pin(core::future::ready(SyscallResult::err(
                panda_abi::ErrorCode::InvalidArgument,
            )));
        }
    };

    Box::pin(core::future::ready(scheduler::with_current_process(
        |proc| {
            let pid = scheduler::current_process_id();

            let resource: Arc<dyn Resource> = Arc::new(SubscriptionResource);
            let handle_id = match proc.handles_mut().insert_typed(HandleType::DeviceSubscription, resource)
            {
                Ok(id) => id,
                Err(_) => return SyscallResult::err(panda_abi::ErrorCode::TooManyHandles),
            };

            // Attach to the mailbox before replay, so a subscriber that
            // races the replay with an attach can't miss the wakeup.
            let mailbox_ref = attach_to_mailbox(
                proc,
                mailbox_handle as u64,
                handle_id,
                panda_abi::device::EVENT_DEVICE_ADDED | panda_abi::device::EVENT_DEVICE_REMOVED,
            )
            .map(|mailbox| MailboxRef::new(mailbox, handle_id));

            if let Some(mailbox_ref) = mailbox_ref {
                DEVICE_REGISTRY
                    .lock()
                    .subscribe(bus_type, match_bytes, pid, mailbox_ref);
            }
            // mailbox_handle == 0: subscription is registered with no
            // mailbox to post to. Matches `attach_to_mailbox`'s existing
            // convention elsewhere (e.g. `handle_open`) of silently
            // skipping attachment for a zero handle.

            SyscallResult::ok(handle_id as isize)
        },
    )))
}

/// Handle `OP_DEVICE_CLAIM(device_token)`.
///
/// `handle` carries the raw token value (not a resource in the caller's
/// handle table — see `device::DeviceRegistry`). Consumes the token; on
/// success returns the claimed device's `DeviceId` as the "owned device
/// handle" (a real `Device`-typed resource handle, and the MMIO/DMA/IRQ
/// syscalls that act on it, are Phase 6).
pub fn handle_device_claim(handle: u64) -> SyscallFuture {
    let pid: ProcessId = scheduler::current_process_id();
    let result = match DEVICE_REGISTRY.lock().claim(handle, pid) {
        Ok(device_id) => SyscallResult::ok(device_id as isize),
        Err(ClaimError::InvalidToken) => SyscallResult::err(panda_abi::ErrorCode::InvalidHandle),
        Err(ClaimError::AlreadyClaimed) => SyscallResult::err(panda_abi::ErrorCode::Busy),
    };
    Box::pin(core::future::ready(result))
}
