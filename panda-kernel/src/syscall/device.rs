//! Device driver model syscall handlers: `OP_DEVICE_SUBSCRIBE`,
//! `OP_DEVICE_POLL_EVENT`, `OP_DEVICE_CLAIM`, `OP_DEVICE_MAP_MMIO`,
//! `OP_DMA_ALLOC`, `OP_DMA_FREE`, and `OP_DEVICE_SUBSCRIBE_IRQ`.
//!
//! All seven `OP_DEVICE_*`/`OP_DMA_*` operations have real kernel handlers.
//! `OP_DEVICE_CLAIM` returns an owned `Device`-typed handle
//! (`resource::DeviceResource`) that is the root capability the other four
//! hardware-access syscalls require — see that type's doc comment for what
//! it tracks and how its cleanup (MMIO unmap, DMA free, IRQ unsubscribe,
//! IOMMU domain teardown, `DeviceRegistry::release`) is wired into both the
//! explicit handle-close and process-exit paths.
//!
//! `OP_DEVICE_SUBSCRIBE`'s replay (and later `EVENT_DEVICE_ADDED`/
//! `EVENT_DEVICE_REMOVED` from `device::pci::register_enumerated_devices` /
//! future hotplug) posts a mailbox event *bit* only — it cannot carry the
//! matched `DeviceEvent` payload (bus type, identity, claim token) inline.
//! `OP_DEVICE_POLL_EVENT` is the payload-delivery path: it drains one
//! `DeviceEvent` at a time off the subscription's queue
//! (`resource::SubscriptionResource`), the same `MailboxEventResult`-style
//! convention `OP_MAILBOX_POLL` uses.

#![deny(unsafe_code)]

use alloc::boxed::Box;
use alloc::sync::Arc;

use alloc::collections::VecDeque;

use panda_abi::HandleType;
use panda_abi::device::{BusType, DmaAllocOut, PciAddress};
use spinning_top::Spinlock;

use crate::device::{ClaimError, DEVICE_REGISTRY, DeviceInfo};
use crate::iommu::manager;
use crate::process::ProcessId;
use crate::resource::{DeviceResource, DeviceResourceError, MailboxRef, Resource, SubscriptionResource};
use crate::scheduler;

use super::helpers::attach_to_mailbox;
use super::user_ptr::{SyscallFuture, SyscallResult, UserAccess, UserPtr, UserSlice};

/// Map a [`DeviceResourceError`] to the syscall error code it should
/// surface as.
fn device_resource_error_code(err: DeviceResourceError) -> panda_abi::ErrorCode {
    match err {
        DeviceResourceError::NoPciDevice
        | DeviceResourceError::InvalidBar
        | DeviceResourceError::NoSuchAllocation => panda_abi::ErrorCode::InvalidArgument,
        DeviceResourceError::OutOfVirtualSpace => panda_abi::ErrorCode::NoSpace,
        DeviceResourceError::IrqUnavailable => panda_abi::ErrorCode::NotSupported,
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
            // `proc.id()`, not `scheduler::current_process_id()`: that free
            // function takes `SCHEDULER.read()`, and this closure already
            // runs inside `with_current_process`'s `SCHEDULER.write()` (via
            // `with_scheduler_mut`) — calling it here would self-deadlock
            // the non-reentrant `RwSpinlock` (write held, then read spins
            // forever waiting for a writer — itself — to release). The
            // kernel unit tests in `panda-kernel/tests/device.rs` call
            // `DeviceRegistry::subscribe` directly and don't exercise this
            // code path.
            let pid = proc.id();

            let queue = Arc::new(Spinlock::new(VecDeque::new()));
            let resource: Arc<dyn Resource> = Arc::new(SubscriptionResource::new(queue.clone()));
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
                    .subscribe(bus_type, match_bytes, pid, mailbox_ref, queue);
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
/// success, assigns the device an isolated IOMMU domain (if IOMMU hardware
/// is present — see `iommu::manager::create_device_domain`), builds a
/// `Device`-typed resource handle wrapping it, installs that in the
/// caller's handle table, and returns the new handle. This handle is the
/// root capability for `OP_DEVICE_MAP_MMIO`/`OP_DMA_ALLOC`/`OP_DMA_FREE`/
/// `OP_DEVICE_SUBSCRIBE_IRQ`.
pub fn handle_device_claim(handle: u64) -> SyscallFuture {
    let pid: ProcessId = scheduler::current_process_id();
    let device_id = match DEVICE_REGISTRY.lock().claim(handle, pid) {
        Ok(device_id) => device_id,
        Err(ClaimError::InvalidToken) => {
            return Box::pin(core::future::ready(SyscallResult::err(
                panda_abi::ErrorCode::InvalidHandle,
            )));
        }
        Err(ClaimError::AlreadyClaimed) => {
            return Box::pin(core::future::ready(SyscallResult::err(
                panda_abi::ErrorCode::Busy,
            )));
        }
    };

    let info = DEVICE_REGISTRY.lock().info(device_id);
    let pci_address = match info {
        Some(DeviceInfo::Pci { address, .. }) => Some(PciAddress {
            segment: address.segment,
            bus: address.bus,
            device: address.device,
            function: address.function,
            _pad: [0; 3],
        }),
        _ => None,
    };

    // Bus Master Enable must be set before the caller can do anything with
    // DMA or interrupts (both are memory-write bus transactions the device
    // cannot issue without it) — see `PciDevice::enable_bus_mastering`'s
    // doc comment.
    if let Some(address) = pci_address
        && let Some(pci_device) = crate::device::pci::find_by_address(address)
    {
        pci_device.enable_bus_mastering();
    }

    // Only PCI devices are assigned an IOMMU domain today (the hardware
    // and BAR/MSI-X access this phase builds on is PCI-specific); other
    // bus types claim successfully but get no hardware DMA isolation.
    // `create_device_domain` also returns `Err` (leaving `domain: None`)
    // if no IOMMU hardware was found at boot — the claim still succeeds
    // without hardware-enforced isolation, same as kernel-resident drivers
    // already run today.
    let domain = pci_address.and_then(|address| {
        manager::create_device_domain(iommu::PciAddress::new(
            address.segment,
            address.bus,
            address.device,
            address.function,
        ))
        .ok()
    });

    let resource: Arc<dyn Resource> = Arc::new(DeviceResource::new(device_id, pci_address, domain));
    let result = scheduler::with_current_process(|proc| {
        match proc.handles_mut().insert_typed(HandleType::Device, resource) {
            Ok(handle_id) => SyscallResult::ok(handle_id as isize),
            Err(_) => SyscallResult::err(panda_abi::ErrorCode::TooManyHandles),
        }
    });
    Box::pin(core::future::ready(result))
}

/// Handle `OP_DEVICE_MAP_MMIO(device_handle, bar_index)`.
///
/// Maps BAR `bar_index` of the claimed device into the caller's address
/// space. See `DeviceResource::map_mmio` for the caching-flags limitation
/// this currently has.
pub fn handle_device_map_mmio(
    ua: &UserAccess,
    device_handle: u64,
    bar_index: usize,
    out_ptr: UserPtr<panda_abi::device::MmioMapOut>,
) -> SyscallFuture {
    if out_ptr.addr() == 0 {
        return Box::pin(core::future::ready(SyscallResult::err(
            panda_abi::ErrorCode::InvalidArgument,
        )));
    }

    let result = scheduler::with_current_process(|proc| {
        // Take an owned `Arc` first (as `handle_map`/`OP_DISPLAY_MAP` does):
        // `map_mmio` needs `&mut Process`, so the borrow of `proc.handles()`
        // must end before `proc` is borrowed mutably.
        let resource = proc
            .handles()
            .get(device_handle)
            .ok_or(panda_abi::ErrorCode::InvalidHandle)?
            .resource_arc();
        let device = resource
            .as_device()
            .ok_or(panda_abi::ErrorCode::InvalidHandle)?;
        device
            .map_mmio(proc, bar_index as u32)
            .map_err(device_resource_error_code)
    });

    let (virt_addr, size) = match result {
        Ok(pair) => pair,
        Err(code) => return Box::pin(core::future::ready(SyscallResult::err(code))),
    };

    let out = panda_abi::device::MmioMapOut {
        virt_addr,
        size: size as u64,
    };
    Box::pin(core::future::ready(match ua.write_user(out_ptr, &out) {
        Ok(_) => SyscallResult::ok(0),
        Err(_) => SyscallResult::err(panda_abi::ErrorCode::InvalidArgument),
    }))
}

/// Handle `OP_DMA_ALLOC(device_handle, size, out: *mut DmaAllocOut)`.
///
/// Allocates a physically-contiguous, IOMMU-mapped DMA buffer for the
/// claimed device and writes `{virt_addr, iova}` to `out` — the two-value
/// return convention `OP_DISPLAY_INFO` uses for `SurfaceInfoOut`, since a
/// syscall can only return a single `isize`.
pub fn handle_dma_alloc(ua: &UserAccess, device_handle: u64, size: usize, out_ptr: UserPtr<DmaAllocOut>) -> SyscallFuture {
    if out_ptr.addr() == 0 {
        return Box::pin(core::future::ready(SyscallResult::err(
            panda_abi::ErrorCode::InvalidArgument,
        )));
    }

    let result = scheduler::with_current_process(|proc| {
        let resource = proc
            .handles()
            .get(device_handle)
            .ok_or(panda_abi::ErrorCode::InvalidHandle)?
            .resource_arc();
        let device = resource
            .as_device()
            .ok_or(panda_abi::ErrorCode::InvalidHandle)?;
        device
            .dma_alloc(proc, size)
            .map_err(device_resource_error_code)
    });

    match result {
        Ok((virt_addr, iova)) => {
            let out = DmaAllocOut { virt_addr, iova };
            if ua.write_user(out_ptr, &out).is_err() {
                return Box::pin(core::future::ready(SyscallResult::err(
                    panda_abi::ErrorCode::InvalidArgument,
                )));
            }
            Box::pin(core::future::ready(SyscallResult::ok(0)))
        }
        Err(code) => Box::pin(core::future::ready(SyscallResult::err(code))),
    }
}

/// Handle `OP_DMA_FREE(device_handle, virt_addr, size)`.
///
/// `size` is accepted for ABI symmetry with `OP_DMA_ALLOC` but is not
/// needed to locate the allocation (tracked by `virt_addr`) — mismatches
/// are not validated against it.
pub fn handle_dma_free(device_handle: u64, virt_addr: u64, _size: usize) -> SyscallFuture {
    let result = scheduler::with_current_process(|proc| {
        let device = proc
            .handles()
            .get(device_handle)
            .and_then(|h| h.as_device())
            .ok_or(panda_abi::ErrorCode::InvalidHandle)?;
        device.dma_free(virt_addr).map_err(device_resource_error_code)
    });
    Box::pin(core::future::ready(match result {
        Ok(()) => SyscallResult::ok(0),
        Err(code) => SyscallResult::err(code),
    }))
}

/// Handle `OP_DEVICE_SUBSCRIBE_IRQ(device_handle, mailbox_handle)`.
///
/// See `DeviceResource::subscribe_irq` for the real capabilities and
/// limitations of the backing mechanism (a small fixed pool of MSI-X
/// vectors).
pub fn handle_device_subscribe_irq(device_handle: u64, mailbox_handle: u64) -> SyscallFuture {
    let result = scheduler::with_current_process(|proc| {
        let mailbox = proc
            .handles()
            .get(mailbox_handle)
            .and_then(|h| h.as_mailbox())
            .ok_or(panda_abi::ErrorCode::InvalidHandle)?;
        // `MailboxRef::post_event` only queues an event if `device_handle` is
        // attached to the mailbox first (`Mailbox::attach` — see its doc
        // comment and `MailboxInner::post_event`'s `self.attached.get(...)`
        // check) — without this, every `EVENT_DEVICE_IRQ` the device's
        // interrupt handler posts (`resource::device::dynamic_irq_common`)
        // is silently dropped, since nothing ever attached this handle_id.
        // Found by tracing that the interrupt handler *does* fire (confirmed
        // via a real key press generating a real MSI-X interrupt) but the
        // driver process never saw `EVENT_DEVICE_IRQ` on its mailbox. Every
        // other subscribe-style handler in this codebase (e.g.
        // `handle_device_subscribe`, `handle_open`) goes through
        // `helpers::attach_to_mailbox`, which does this — this handler was
        // the one exception that built a `MailboxRef` directly instead.
        mailbox.attach(device_handle, panda_abi::device::EVENT_DEVICE_IRQ);
        let mailbox_ref = MailboxRef::new(mailbox, device_handle);

        let device = proc
            .handles()
            .get(device_handle)
            .and_then(|h| h.as_device())
            .ok_or(panda_abi::ErrorCode::InvalidHandle)?;
        device.subscribe_irq(mailbox_ref).map_err(device_resource_error_code)
    });
    Box::pin(core::future::ready(match result {
        Ok(()) => SyscallResult::ok(0),
        Err(code) => SyscallResult::err(code),
    }))
}

/// Handle `OP_DEVICE_POLL_EVENT(subscription_handle, out: *mut DeviceEvent)`.
///
/// Pops the oldest pending `DeviceEvent` off `subscription_handle`'s queue
/// (see `resource::SubscriptionResource`) and writes it to `out`. This is
/// the payload-delivery path the module doc comment used to describe as a
/// gap: `OP_DEVICE_SUBSCRIBE`'s replay and later `EVENT_DEVICE_ADDED`/
/// `EVENT_DEVICE_REMOVED` mailbox posts only carry an event bit; the actual
/// `DeviceEvent` (bus type, identity, and — for `EVENT_DEVICE_ADDED` — the
/// claim token `OP_DEVICE_CLAIM` needs) is read back with this call,
/// mirroring `OP_MAILBOX_POLL`'s `(handle_id, events)` -> `MailboxEventResult`
/// convention.
///
/// Returns `1` with `*out` populated if an event was pending, `0` (leaving
/// `*out` untouched) if the queue is empty, or an error for an invalid
/// handle/pointer.
pub fn handle_device_poll_event(
    ua: &UserAccess,
    subscription_handle: u64,
    out_ptr: usize,
) -> SyscallFuture {
    if out_ptr == 0 {
        return Box::pin(core::future::ready(SyscallResult::err(
            panda_abi::ErrorCode::InvalidArgument,
        )));
    }

    let event = scheduler::with_current_process(|proc| {
        let subscription = proc
            .handles()
            .get(subscription_handle)
            .and_then(|h| h.as_device_subscription())
            .ok_or(panda_abi::ErrorCode::InvalidHandle)?;
        Ok(subscription.poll_event())
    });

    let event = match event {
        Ok(event) => event,
        Err(code) => return Box::pin(core::future::ready(SyscallResult::err(code))),
    };

    match event {
        Some(event) => match ua.write_user(UserPtr::new(out_ptr), &event) {
            Ok(_) => Box::pin(core::future::ready(SyscallResult::ok(1))),
            Err(_) => Box::pin(core::future::ready(SyscallResult::err(
                panda_abi::ErrorCode::InvalidArgument,
            ))),
        },
        None => Box::pin(core::future::ready(SyscallResult::ok(0))),
    }
}

/// Handle `OP_DEVICE_PCI_CONFIG_READ(device_handle, offset, width)`.
///
/// See the op's doc comment in `panda_abi::device` for why userspace
/// drivers need raw config-space reads (walking a virtio-pci capability
/// list with no in-kernel virtio transport to do it for them).
pub fn handle_device_pci_config_read(device_handle: u64, offset: usize, width: usize) -> SyscallFuture {
    if offset > u8::MAX as usize || !matches!(width, 1 | 2 | 4) || offset + width > 256 {
        return Box::pin(core::future::ready(SyscallResult::err(
            panda_abi::ErrorCode::InvalidArgument,
        )));
    }

    let result = scheduler::with_current_process(|proc| {
        let device = proc
            .handles()
            .get(device_handle)
            .and_then(|h| h.as_device())
            .ok_or(panda_abi::ErrorCode::InvalidHandle)?;
        device
            .pci_config_read(offset as u8, width as u32)
            .map_err(device_resource_error_code)
    });
    Box::pin(core::future::ready(match result {
        Ok(value) => SyscallResult::ok(value as isize),
        Err(code) => SyscallResult::err(code),
    }))
}
