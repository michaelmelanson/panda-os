//! Shared boot sequence for the higher-half jump.
//!
//! The production entry point (`main.rs`) and the test harness
//! (`testing.rs`) both perform the same dance: run early init, stash the
//! ACPI RSDP in a static (data cannot travel through the stack switch),
//! compute the boot stack top, and jump to a higher-half continuation that
//! finishes initialization. The parts that differ — which extra pointer each
//! stashes and how it is translated after the jump — stay with the callers.

use core::sync::atomic::{AtomicU64, Ordering};

use crate::memory;
use crate::syscall::gdt::SYSCALL_STACK;

/// ACPI RSDP address, stored before the higher-half jump.
static ACPI2_RSDP: AtomicU64 = AtomicU64::new(0);

/// Run early kernel init and jump to higher-half execution.
///
/// `continuation` runs at higher-half addresses on the boot stack and must
/// begin by calling [`init_higher_half`]. Callers stash any data they need
/// across the jump in statics beforehand.
///
/// # Safety
///
/// Must be called exactly once, from the boot entry point, before any other
/// kernel subsystem is used.
pub unsafe fn init_and_jump(continuation: unsafe extern "C" fn() -> !) -> ! {
    let acpi2_rsdp = crate::init();
    ACPI2_RSDP.store(acpi2_rsdp.as_u64(), Ordering::SeqCst);

    let boot_stack_top =
        SYSCALL_STACK.inner.as_ptr() as u64 + SYSCALL_STACK.inner.len() as u64;

    unsafe { memory::jump_to_higher_half(boot_stack_top, continuation) }
}

/// Complete kernel initialization from inside a higher-half continuation.
///
/// This must be the first call in any continuation passed to
/// [`init_and_jump`]. After it returns, the continuation should translate
/// any pointers it stashed before the jump and then call
/// `memory::remove_identity_mapping()`.
pub fn init_higher_half() {
    let acpi2_rsdp = x86_64::PhysAddr::new(ACPI2_RSDP.load(Ordering::SeqCst));
    crate::init_after_higher_half_jump(acpi2_rsdp);
}
