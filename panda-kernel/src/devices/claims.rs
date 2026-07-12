//! Device claim table enforcing exclusive ownership.
//!
//! Some devices must never be accessed by more than one owner at a time:
//!
//! - A block device that ext2 has mounted must not also be opened raw via
//!   `block:/...`, or writes from one side can corrupt what the other reads.
//! - The display framebuffer must not be blitted into by two independent
//!   userspace surfaces at once, or their writes tear each other's pixels
//!   (see `FramebufferSurface`, which uses a raw pointer with no locking of
//!   its own).
//!
//! This module is the single place that arbitrates ownership. Claiming a
//! [`DeviceAddress`] returns a [`ClaimGuard`]; holding the guard *is* the
//! proof of exclusive ownership, and there is no other way to release a
//! claim. Because the guard is ordinary Rust value, embedding it in a
//! resource (or a mount table entry) means the claim is released exactly
//! when that value is dropped — on `close()`, or on process exit when the
//! handle table itself is dropped — with no special-cased cleanup code
//! required anywhere else in the kernel.

use alloc::collections::BTreeMap;
use spinning_top::Spinlock;

use crate::device_address::DeviceAddress;

/// Who holds a claim, for diagnostics (logging, debugging `Busy` errors).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClaimOwner {
    /// A mounted filesystem (e.g. ext2) is using the device.
    Mount,
    /// A raw scheme open (e.g. `block:/pci/storage/0`) is using the device.
    RawOpen,
    /// The display is open via the surface scheme (e.g. `surface:/fb0`).
    Display,
}

/// Error returned when a claim cannot be granted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClaimError {
    /// The device is already claimed by another owner.
    Busy,
}

/// Global claim table, keyed by device address.
static CLAIMS: Spinlock<BTreeMap<DeviceAddress, ClaimOwner>> = Spinlock::new(BTreeMap::new());

/// Claim exclusive ownership of `address` on behalf of `owner`.
///
/// Returns a [`ClaimGuard`] on success. Dropping the guard releases the
/// claim, making the address available again. If the address is already
/// claimed, returns `Err(ClaimError::Busy)` and leaves the existing claim
/// untouched.
pub fn claim(address: DeviceAddress, owner: ClaimOwner) -> Result<ClaimGuard, ClaimError> {
    let mut claims = CLAIMS.lock();
    if let Some(existing) = claims.get(&address) {
        log::debug!(
            "claim: {} already held by {:?}, denying {:?}",
            address,
            existing,
            owner
        );
        return Err(ClaimError::Busy);
    }
    claims.insert(address.clone(), owner);
    Ok(ClaimGuard { address, owner })
}

/// RAII proof of exclusive device ownership.
///
/// The claim is released automatically when this guard is dropped. There is
/// deliberately no explicit `release()` method: ownership lifetime is tied
/// to wherever the guard is stored (a resource, a mount table entry, ...),
/// which is what makes "close releases the claim" and "process exit
/// releases the claim" fall out of ordinary Rust drop semantics rather than
/// needing dedicated cleanup paths.
#[derive(Debug)]
pub struct ClaimGuard {
    address: DeviceAddress,
    owner: ClaimOwner,
}

impl ClaimGuard {
    /// The device address this guard holds a claim on.
    pub fn address(&self) -> &DeviceAddress {
        &self.address
    }

    /// The owner tag this claim was granted to.
    pub fn owner(&self) -> ClaimOwner {
        self.owner
    }
}

impl Drop for ClaimGuard {
    fn drop(&mut self) {
        CLAIMS.lock().remove(&self.address);
    }
}

// Note: this crate builds with `[lib] test = false` (see panda-kernel/Cargo.toml),
// so `cfg(test)` unit tests here would never actually run. Behavioural
// coverage for this module lives in the QEMU-driven integration test at
// panda-kernel/tests/claims.rs instead, following the convention used by
// the rest of the kernel test suite.
