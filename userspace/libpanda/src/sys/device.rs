//! Low-level device driver model syscalls.
//!
//! Only [`subscribe`] and [`claim`] round-trip to a working kernel handler
//! today (see `panda-kernel/src/syscall/device.rs`). The MMIO/DMA/IRQ
//! operations are defined for ABI completeness but require IOMMU support
//! (a later, separate task) to implement safely — see the `Err`-returning
//! stubs in `crate::device` for those.

use super::{Handle, send};
use panda_abi::device::{OP_DEVICE_CLAIM, OP_DEVICE_SUBSCRIBE};

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
