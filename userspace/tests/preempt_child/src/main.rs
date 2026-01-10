//! Child process for preemption test.
//!
//! Does CPU-bound work without yielding. If preemption works correctly,
//! multiple instances of this will interleave execution via timer interrupts.

#![no_std]
#![no_main]

use libpanda::environment;

libpanda::main! {
    // Do CPU-bound work - no yields, relies on preemption
    let mut sum: u64 = 0;
    let iterations: u64 = 10_000_000;

    for i in 0..iterations {
        sum = sum.wrapping_add(i);
        core::hint::black_box(sum);
    }

    // Verify computation was correct (registers preserved across preemptions)
    let expected = (iterations - 1) * iterations / 2;
    if sum != expected {
        environment::log("preempt_child: FAIL - incorrect result");
        return 1;
    }

    environment::log("preempt_child: completed");
    0
}
