//! Test that writing to a read-only page causes a segmentation fault.
//!
//! This test verifies that the kernel correctly kills a process that attempts
//! to write to a read-only (code) page, rather than silently allocating a new
//! writable page via demand paging.

#![no_std]
#![no_main]

use libpanda::environment;

/// A dummy function whose address we'll use as a read-only code page target.
#[inline(never)]
fn dummy_function() -> u64 {
    42
}

libpanda::main! {
    environment::log("Segfault test: starting");

    // Get the address of a code page (mapped read-only + executable).
    let code_ptr = dummy_function as *const u8 as *mut u8;

    environment::log("Segfault test: writing to read-only code page");

    // Attempt to write to the code page. This should trigger a protection
    // violation page fault, causing the kernel to kill this process.
    unsafe {
        core::ptr::write_volatile(code_ptr, 0xCC);
    }

    // If we reach here, the protection violation was NOT enforced -- the kernel
    // incorrectly demand-paged a writable page instead of killing us.
    environment::log("FAIL: write to read-only page succeeded (should have been killed)");
    1
}
