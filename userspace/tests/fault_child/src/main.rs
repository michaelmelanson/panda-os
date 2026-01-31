//! Child process that triggers a protection violation by writing to read-only memory.
//! The kernel should kill this process and the parent should continue running.

#![no_std]
#![no_main]

use libpanda::environment;

libpanda::main! {
    environment::log("Fault child: starting");
    environment::log("Fault child: writing to read-only code page...");

    // Write to read-only code page — triggers protection violation
    let code_ptr = fault_child_target as *const fn() as *mut u8;
    unsafe {
        core::ptr::write_volatile(code_ptr, 0xCC);
    }

    // Should never reach here
    environment::log("FAIL: fault child was not killed");
    1
}

fn fault_child_target() {}
