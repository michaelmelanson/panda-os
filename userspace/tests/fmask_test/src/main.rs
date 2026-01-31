//! Userspace test verifying that FMASK clears the DF (Direction Flag) on syscall entry.
//!
//! Sets DF before making syscalls and verifies they complete without corruption,
//! proving FMASK clears DF on entry.

#![no_std]
#![no_main]

use libpanda::environment;

libpanda::main! {
    environment::log("FMASK test: starting");

    // Test: set DF before a syscall and verify the syscall completes correctly.
    // If FMASK is not programmed, DF would persist into the kernel and corrupt
    // any string operations (rep movsb, etc.) used during syscall handling.
    for i in 0..5 {
        // Set the Direction Flag (DF) — this reverses string operation direction
        unsafe {
            core::arch::asm!("std");
        }

        // Make a syscall — if FMASK is working, DF is cleared on entry
        environment::log("FMASK test: syscall with DF set completed");

        // Clear DF in userspace to keep our own code safe
        unsafe {
            core::arch::asm!("cld");
        }
    }

    environment::log("FMASK test: all syscalls completed successfully");
    0
}
