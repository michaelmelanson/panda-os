//! Low-level device driver model syscalls.
//!
//! All seven `OP_DEVICE_*`/`OP_DMA_*` operations round-trip to real kernel
//! handlers (see `panda-kernel/src/syscall/device.rs`).

use super::{Handle, send};
use panda_abi::device::{
    DmaAllocOut, MmioMapOut, OP_DEVICE_CLAIM, OP_DEVICE_MAP_MMIO, OP_DEVICE_PCI_CONFIG_READ,
    OP_DEVICE_POLL_EVENT, OP_DEVICE_SUBSCRIBE, OP_DEVICE_SUBSCRIBE_IRQ, OP_DMA_ALLOC, OP_DMA_FREE,
};

/// Subscribe to device events for `bus_type`, matching `match_data`.
///
/// If `mailbox` is non-zero, the returned subscription handle is attached
/// to it with `EVENT_DEVICE_ADDED | EVENT_DEVICE_REMOVED`. Returns the
/// subscription handle, or a negative error code.
///
/// Note: this extends the plan's 3-argument `OP_DEVICE_SUBSCRIBE(bus_type,
/// match_ptr, len)` with a 4th `mailbox` argument, following the existing
/// codebase convention (e.g. `handle_open`, `handle_spawn`) of attaching a
/// freshly created handle to a mailbox at creation time — there is no
/// separate "attach" syscall to do this after the fact.
#[inline(always)]
pub fn subscribe(bus_type: u32, match_data: &[u8], mailbox: Handle) -> isize {
    send(
        Handle::from(0u64),
        OP_DEVICE_SUBSCRIBE,
        bus_type as usize,
        match_data.as_ptr() as usize,
        match_data.len(),
        u64::from(mailbox) as usize,
    )
}

/// Claim a device using a token received via `EVENT_DEVICE_ADDED`.
///
/// Returns the owned device handle (a `DeviceId`, per
/// `panda-kernel/src/device/mod.rs`), or a negative error code.
#[inline(always)]
pub fn claim(token: Handle) -> isize {
    send(token, OP_DEVICE_CLAIM, 0, 0, 0, 0)
}

/// Drain one pending `DeviceEvent` off a subscription's queue into `out`.
///
/// Returns `1` (and fills `*out`) if an event was pending, `0` if the queue
/// is empty, or a negative error code.
#[inline(always)]
pub fn poll_event(subscription: Handle, out: &mut panda_abi::device::DeviceEvent) -> isize {
    send(
        subscription,
        OP_DEVICE_POLL_EVENT,
        out as *mut panda_abi::device::DeviceEvent as usize,
        0,
        0,
        0,
    )
}

/// Map BAR `bar_index` of a claimed device into this process, filling `out`
/// with `{virt_addr, size}`.
///
/// Returns `0` on success, or a negative error code.
#[inline(always)]
pub fn map_mmio(device: Handle, bar_index: u32, out: &mut MmioMapOut) -> isize {
    send(
        device,
        OP_DEVICE_MAP_MMIO,
        bar_index as usize,
        out as *mut MmioMapOut as usize,
        0,
        0,
    )
}

/// Allocate an IOMMU-mapped DMA buffer for a claimed device, filling `out`
/// with `{virt_addr, iova}`.
///
/// Returns `0` on success, or a negative error code.
#[inline(always)]
pub fn dma_alloc(device: Handle, size: usize, out: &mut DmaAllocOut) -> isize {
    send(
        device,
        OP_DMA_ALLOC,
        size,
        out as *mut DmaAllocOut as usize,
        0,
        0,
    )
}

/// Free memory allocated by [`dma_alloc`].
///
/// Returns `0` on success, or a negative error code.
#[inline(always)]
pub fn dma_free(device: Handle, virt_addr: usize, size: usize) -> isize {
    send(device, OP_DMA_FREE, virt_addr, size, 0, 0)
}

/// Subscribe to `EVENT_DEVICE_IRQ` for a claimed device, posted to
/// `mailbox`.
///
/// Returns `0` on success, or a negative error code.
#[inline(always)]
pub fn subscribe_irq(device: Handle, mailbox: Handle) -> isize {
    send(
        device,
        OP_DEVICE_SUBSCRIBE_IRQ,
        u64::from(mailbox) as usize,
        0,
        0,
        0,
    )
}

/// Read `width` (1, 2, or 4) bytes of raw PCI config space at `offset`.
///
/// Returns the value (zero-extended to `isize`), or a negative error code.
#[inline(always)]
pub fn pci_config_read(device: Handle, offset: u32, width: u32) -> isize {
    send(device, OP_DEVICE_PCI_CONFIG_READ, offset as usize, width as usize, 0, 0)
}
