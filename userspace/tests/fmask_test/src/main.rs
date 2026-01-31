//! Userspace test verifying that FMASK clears the DF (Direction Flag) on syscall entry
//! but restores it on return to userspace.
//!
//! Sets DF before making syscalls, verifies the syscall completes without corruption
//! (proving FMASK clears DF on entry), and checks DF is still set after return
//! (proving RFLAGS are properly restored via sysret).

#![no_std]
#![no_main]

use libpanda::environment;

/// Read the current RFLAGS register value.
#[inline(always)]
fn read_rflags() -> u64 {
    let flags: u64;
    unsafe {
        core::arch::asm!("pushfq; pop {}", out(reg) flags);
    }
    flags
}

const DF_BIT: u64 = 1 << 10;

libpanda::main! {
    environment::log("FMASK test: starting");

    // Test: set DF before a syscall and verify the syscall completes correctly.
    // If FMASK is not programmed, DF would persist into the kernel and corrupt
    // any string operations (rep movsb, etc.) used during syscall handling.
    for _i in 0..5 {
        // Set the Direction Flag (DF) — this reverses string operation direction
        unsafe {
            core::arch::asm!("std");
        }

        // Make a syscall — if FMASK is working, DF is cleared on entry
        environment::log("FMASK test: syscall with DF set completed");

        // Verify DF is still set after the syscall returns.
        // sysretq restores RFLAGS from r11, so userspace flags should be preserved.
        let flags = read_rflags();
        if flags & DF_BIT == 0 {
            environment::log("FMASK test: FAIL — DF was cleared by syscall return");
            return 1;
        }

        // Clear DF in userspace to keep our own code safe
        unsafe {
            core::arch::asm!("cld");
        }
    }

    environment::log("FMASK test: all syscalls completed successfully");
    0
}
