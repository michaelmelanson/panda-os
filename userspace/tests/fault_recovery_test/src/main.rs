//! Test that the system keeps running after a process is killed by a page fault.
//!
//! Spawns a child that triggers a protection violation. The kernel should kill
//! the child, and this parent process should continue running and observe the
//! non-zero exit code.

#![no_std]
#![no_main]

use libpanda::{environment, process::Child};

libpanda::main! {
    environment::log("Fault recovery test: starting");

    // Spawn the child that will fault
    let mut child = match Child::spawn("file:/initrd/fault_child") {
        Ok(c) => c,
        Err(_) => {
            environment::log("FAIL: could not spawn fault_child");
            return 1;
        }
    };

    environment::log("Fault recovery test: child spawned, waiting for exit...");

    // Wait for the child — it should be killed by the kernel with exit code 1
    let status = match child.wait() {
        Ok(s) => s,
        Err(_) => {
            environment::log("FAIL: wait returned error");
            return 1;
        }
    };

    if status.code() != 0 {
        environment::log("Fault recovery test: child was killed with non-zero exit code");
    } else {
        environment::log("FAIL: child exited with code 0 (should have been killed)");
        return 1;
    }

    // If we get here, the system survived the fault and the parent is still running
    environment::log("Fault recovery test: system kept running after fault");
    environment::log("PASS");
    0
}
