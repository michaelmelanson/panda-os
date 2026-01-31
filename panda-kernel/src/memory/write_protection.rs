//! Write protection control for page table modifications.
//!
//! Provides an RAII guard that disables CR0.WP (write protection) and
//! interrupts for the duration, ensuring both are restored on drop.

use x86_64::{
    instructions::interrupts,
    registers::control::{Cr0, Cr0Flags},
};

/// RAII guard that disables write protection and interrupts.
///
/// On creation, interrupts are disabled and CR0.WP is cleared.
/// On drop, CR0.WP is re-enabled and interrupts are restored to
/// their previous state.
struct WriteProtectGuard {
    interrupts_were_enabled: bool,
}

impl WriteProtectGuard {
    fn new() -> Self {
        let were_enabled = interrupts::are_enabled();
        interrupts::disable();

        unsafe {
            Cr0::update(|cr0| cr0.remove(Cr0Flags::WRITE_PROTECT));
        }

        Self {
            interrupts_were_enabled: were_enabled,
        }
    }
}

impl Drop for WriteProtectGuard {
    fn drop(&mut self) {
        unsafe {
            Cr0::update(|cr0| cr0.insert(Cr0Flags::WRITE_PROTECT));
        }
        if self.interrupts_were_enabled {
            interrupts::enable();
        }
    }
}

/// Execute a closure with write protection disabled.
///
/// Interrupts are disabled for the duration to prevent interrupt handlers
/// from running with write protection off. A scope guard ensures WP is
/// re-enabled even if the closure panics.
pub fn without_write_protection(f: impl FnOnce()) {
    let _guard = WriteProtectGuard::new();
    f();
}
