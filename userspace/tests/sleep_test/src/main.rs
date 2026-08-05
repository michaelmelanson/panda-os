//! Tests OP_PROCESS_SLEEP: a process should block for at least the requested
//! duration and be woken again afterwards, with the elapsed uptime (as
//! reported by `environment::time()`) reflecting real time passing rather
//! than a fixed/stub value.

#![no_std]
#![no_main]

use libpanda::{environment, process};

/// Requested sleep duration. Kept short so the test suite stays fast, but
/// long enough to comfortably exceed one scheduler time slice (10ms).
const SLEEP_MS: u64 = 200;

/// Generous upper bound on elapsed time to catch a hung/never-woken sleep
/// without making the test flaky under normal scheduler load.
const MAX_ELAPSED_MS: isize = SLEEP_MS as isize + 5000;

libpanda::main! {
    environment::log("sleep_test: starting");

    let before = environment::time();
    if before < 0 {
        environment::log("FAIL: environment::time() returned an error before sleeping");
        return 1;
    }

    let result = process::sleep(SLEEP_MS);
    if result != 0 {
        environment::log("FAIL: process::sleep() returned an error");
        return 1;
    }

    let after = environment::time();
    if after < 0 {
        environment::log("FAIL: environment::time() returned an error after sleeping");
        return 1;
    }

    let elapsed = after - before;

    if elapsed <= 0 {
        environment::log("FAIL: elapsed time was zero or negative");
        return 1;
    }

    if elapsed < SLEEP_MS as isize {
        environment::log("FAIL: elapsed time was less than the requested sleep duration");
        return 1;
    }

    if elapsed > MAX_ELAPSED_MS {
        environment::log("FAIL: elapsed time was implausibly large");
        return 1;
    }

    environment::log("sleep_test: slept for at least the requested duration");
    environment::log("PASS");
    0
}
