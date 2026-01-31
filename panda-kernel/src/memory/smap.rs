//! Supervisor Mode Access Prevention (SMAP) support.
//!
//! SMAP causes the CPU to fault when kernel code accesses user-mapped pages
//! without explicit bracketing via `stac` (set AC flag) and `clac` (clear AC
//! flag). This prevents the kernel from accidentally dereferencing user
//! pointers, a common vulnerability class in OS kernels.
//!
//! # Usage
//!
//! All intentional kernel accesses to userspace memory must be wrapped in
//! [`with_userspace_access`] or use a [`UserspaceAccessGuard`] directly:
//!
//! ```ignore
//! let value = smap::with_userspace_access(|| unsafe {
//!     core::ptr::read(user_addr as *const u32)
//! });
//! ```
//!
//! The AC flag is automatically cleared when entering the kernel via `syscall`
//! (the `SFMASK` MSR clears it) and is cleared as defence-in-depth on
//! interrupt entry.

use core::sync::atomic::{AtomicBool, Ordering};

use log::info;

/// Whether SMAP is enabled on this CPU.
static SMAP_ENABLED: AtomicBool = AtomicBool::new(false);

/// Check whether the CPU supports SMAP (CPUID leaf 7, subleaf 0, EBX bit 20).
fn cpu_supports_smap() -> bool {
    let result: u32;
    unsafe {
        core::arch::asm!(
            "mov eax, 7",
            "xor ecx, ecx",
            "cpuid",
            out("eax") _,
            out("ebx") result,
            out("ecx") _,
            out("edx") _,
        );
    }
    result & (1 << 20) != 0
}

/// Enable SMAP by setting CR4.SMAP (bit 21).
///
/// Must be called during early kernel init, before any userspace access paths
/// are reachable. If the CPU does not support SMAP, this is a no-op.
pub fn enable() {
    if !cpu_supports_smap() {
        info!("SMAP: CPU does not support SMAP, skipping");
        return;
    }

    unsafe {
        x86_64::registers::control::Cr4::update(|cr4| {
            cr4.insert(x86_64::registers::control::Cr4Flags::SUPERVISOR_MODE_ACCESS_PREVENTION);
        });
    }

    SMAP_ENABLED.store(true, Ordering::Relaxed);
    info!("SMAP: enabled (CR4.SMAP set)");
}

/// Returns true if SMAP is enabled on this CPU.
pub fn is_enabled() -> bool {
    SMAP_ENABLED.load(Ordering::Relaxed)
}

/// Temporarily disable SMAP (allow kernel access to userspace pages).
///
/// # Safety
/// Must be paired with a subsequent [`clac`] call. Prefer using
/// [`UserspaceAccessGuard`] or [`with_userspace_access`] instead.
#[inline(always)]
pub unsafe fn stac() {
    core::arch::asm!("stac", options(nomem, nostack));
}

/// Re-enable SMAP (disallow kernel access to userspace pages).
///
/// # Safety
/// Must be called after a preceding [`stac`] call.
#[inline(always)]
pub unsafe fn clac() {
    core::arch::asm!("clac", options(nomem, nostack));
}

/// RAII guard that enables kernel access to user pages.
///
/// On creation, executes `stac` to set the AC flag. On drop, executes `clac`
/// to clear it. This follows the same pattern as [`super::write_protection::WriteProtectGuard`].
///
/// Nested guards are safe: AC is a single bit (not a counter), so the
/// outermost drop will re-clear it correctly.
pub struct UserspaceAccessGuard(());

impl UserspaceAccessGuard {
    /// Create a new guard, temporarily disabling SMAP.
    pub fn new() -> Self {
        unsafe {
            stac();
        }
        Self(())
    }
}

impl Drop for UserspaceAccessGuard {
    fn drop(&mut self) {
        unsafe {
            clac();
        }
    }
}

/// Execute a closure with SMAP temporarily disabled.
///
/// This is the preferred way to bracket intentional userspace memory access.
/// The AC flag is set before the closure runs and cleared after it returns
/// (even on panic).
#[inline]
pub fn with_userspace_access<R>(f: impl FnOnce() -> R) -> R {
    let _guard = UserspaceAccessGuard::new();
    f()
}

/// Assert that the AC flag is clear. Panics if AC is set.
///
/// Call this before returning to userspace to catch unbalanced `stac`/`clac`.
#[inline]
pub fn assert_ac_clear() {
    let rflags: u64;
    unsafe {
        core::arch::asm!("pushfq; pop {}", out(reg) rflags, options(nomem));
    }
    assert!(
        rflags & (1 << 18) == 0,
        "BUG: AC flag set on return to userspace — unbalanced stac/clac"
    );
}
