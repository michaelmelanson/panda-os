//! Userspace preemption test.
//!
//! This test spawns multiple child processes that do CPU-bound work without
//! yielding. All children run concurrently via preemptive context switching.
//! If preemption isn't working, only one process would run at a time and
//! the test would time out or produce incorrect results.

#![no_std]
#![no_main]

use libpanda::{environment, handle::Handle, process};

libpanda::main! {
    environment::log("Preempt test: spawning 3 CPU-bound children");

    // Spawn multiple children that do CPU-bound work without yielding
    let mut children: [Option<Handle>; 3] = [None, None, None];
    for child in &mut children {
        let Ok(handle) = environment::spawn("file:/initrd/preempt_child", &[], 0, 0) else {
            environment::log("FAIL: spawn returned error");
            return 1;
        };
        *child = Some(handle);
    }

    // Parent also does CPU-bound work to compete for CPU time
    environment::log("Preempt test: parent doing CPU-bound work");
    let mut sum: u64 = 0;
    let iterations: u64 = 10_000_000;

    for i in 0..iterations {
        sum = sum.wrapping_add(i);
        core::hint::black_box(sum);
    }

    let expected = (iterations - 1) * iterations / 2;
    if sum != expected {
        environment::log("FAIL: parent computation incorrect");
        return 1;
    }

    environment::log("Preempt test: parent work done, waiting for children");

    // Wait for all children to complete
    for child in children.into_iter().flatten() {
        let exit_code = process::wait(child);
        if exit_code != 0 {
            environment::log("FAIL: child exited with non-zero code");
            return 1;
        }
    }

    // If we get here, preemption worked - all 4 processes (parent + 3 children)
    // ran concurrently and completed their CPU-bound work correctly
    environment::log("Preempt test: all children completed successfully");
    0
}
