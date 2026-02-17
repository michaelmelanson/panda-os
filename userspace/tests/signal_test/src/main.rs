//! Signal test - tests StopImmediately and Stop signal delivery.

#![no_std]
#![no_main]

use libpanda::{environment, process::Child};

libpanda::main! {
    environment::log("Signal test: starting");

    // Test 1: StopImmediately a looping child
    environment::log("Test 1: StopImmediately");
    if !test_stop_immediately() {
        environment::log("FAIL: Test 1 failed");
        return 1;
    }
    environment::log("Test 1: PASS");

    // Test 2: Stop with graceful handling
    environment::log("Test 2: Stop");
    if !test_stop() {
        environment::log("FAIL: Test 2 failed");
        return 1;
    }
    environment::log("Test 2: PASS");

    environment::log("Signal test: all tests passed");
    environment::log("PASS");
    0
}

/// Test StopImmediately: spawn a child, kill it, verify termination with exit code -9.
fn test_stop_immediately() -> bool {
    let mut child = match Child::spawn("file:/initrd/signal_child") {
        Ok(c) => c,
        Err(_) => {
            environment::log("  StopImmediately: spawn failed");
            return false;
        }
    };

    // Give child a moment to start
    for _ in 0..100 {
        libpanda::process::yield_now();
    }

    // Send StopImmediately
    if let Err(_) = child.kill() {
        environment::log("  StopImmediately: kill() failed");
        return false;
    }

    // Wait for child
    let status = match child.wait() {
        Ok(s) => s,
        Err(_) => {
            environment::log("  StopImmediately: wait() failed");
            return false;
        }
    };

    // Verify exit code is -9 (killed by signal)
    if status.code() != -9 {
        environment::log("  StopImmediately: wrong exit code (expected -9)");
        return false;
    }

    true
}

/// Test Stop: spawn a child that handles Stop gracefully.
fn test_stop() -> bool {
    use libpanda::process::Signal;

    let mut child = match Child::spawn("file:/initrd/signal_child") {
        Ok(c) => c,
        Err(_) => {
            environment::log("  Stop: spawn failed");
            return false;
        }
    };

    // Give child a moment to start
    for _ in 0..100 {
        libpanda::process::yield_now();
    }

    // Send Stop
    if let Err(_) = child.signal(Signal::Stop) {
        environment::log("  Stop: signal() failed");
        return false;
    }

    // Wait for child to handle it gracefully
    let status = match child.wait() {
        Ok(s) => s,
        Err(_) => {
            environment::log("  Stop: wait() failed");
            return false;
        }
    };

    // Verify clean exit (code 0 indicates graceful shutdown)
    if status.code() != 0 {
        environment::log("  Stop: wrong exit code (expected 0)");
        return false;
    }

    true
}
