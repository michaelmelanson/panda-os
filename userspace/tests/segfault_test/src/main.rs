//! Test that writing to a read-only page triggers a protection violation
//! and kills the process instead of silently allocating a new writable page.

#![no_std]
#![no_main]

use libpanda::environment;

libpanda::main! {
    environment::log("Segfault test starting");
    environment::log("Writing to read-only code page...");

    // Get a pointer to our own code — this is mapped read-only + execute.
    // Writing to it must trigger a protection violation, not demand paging.
    let code_ptr = segfault_test_target as *const fn() as *mut u8;
    unsafe {
        core::ptr::write_volatile(code_ptr, 0xCC);
    }

    // If we reach here, the protection violation was not caught
    environment::log("FAIL: write to read-only page did not fault");
    1
}

/// A dummy function whose address we use as a read-only code page target.
fn segfault_test_target() {}
