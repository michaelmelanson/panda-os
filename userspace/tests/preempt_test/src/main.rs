//! Userspace preemption test.
//!
//! This test runs a long compute loop that will be interrupted many times
//! by the preemption timer. If preemption isn't working correctly (e.g.,
//! registers not preserved, stack corruption), the loop will produce
//! incorrect results or crash.

#![no_std]
#![no_main]

use libpanda::syscall::syscall_log;

libpanda::main! {
    syscall_log("Preempt test: starting long computation");

    // Run a computation that takes long enough to be preempted multiple times.
    // The 10ms timer should fire many times during this loop.
    let mut sum: u64 = 0;
    let iterations: u64 = 50_000_000;

    for i in 0..iterations {
        // Use a computation that would break if registers aren't preserved
        sum = sum.wrapping_add(i);
        core::hint::black_box(sum);
    }

    // Verify the result is correct
    // Sum of 0..n = n*(n-1)/2
    let expected = (iterations - 1) * iterations / 2;

    if sum != expected {
        syscall_log("FAIL: computation produced incorrect result");
        return 1;
    }

    syscall_log("Preempt test: computation completed correctly");
    0
}
