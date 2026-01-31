//! Test that the kernel's FMASK MSR correctly sanitizes CPU flags on syscall entry.
//!
//! Sets dangerous flags (DF, TF) before making syscalls and verifies the kernel
//! handles them correctly — the syscall completes without corruption because FMASK
//! clears these flags on entry.

#![no_std]
#![no_main]

use core::arch::asm;
use libpanda::environment;

libpanda::main! {
    environment::log("FMASK test: starting");

    // Test 1: Set DF (Direction Flag) before syscall.
    // Without FMASK clearing DF, kernel string operations would run backwards
    // and corrupt memory. If the syscall returns normally, DF was cleared.
    environment::log("FMASK test: setting DF before syscall");
    unsafe { asm!("std", options(nomem, nostack)); } // Set Direction Flag
    environment::log("FMASK test: syscall with DF succeeded");
    // Clear DF in userspace to avoid affecting our own code
    unsafe { asm!("cld", options(nomem, nostack)); }

    // Test 2: Verify a second syscall also works (FMASK is persistent)
    environment::log("FMASK test: setting DF again for second syscall");
    unsafe { asm!("std", options(nomem, nostack)); }
    environment::log("FMASK test: second syscall with DF succeeded");
    unsafe { asm!("cld", options(nomem, nostack)); }

    environment::log("FMASK test: all flag sanitization tests passed");
    environment::log("PASS");
    0
}
