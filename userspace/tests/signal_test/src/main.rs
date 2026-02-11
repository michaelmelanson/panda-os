//! Signal test - tests SIGKILL and SIGTERM signal delivery.

#![no_std]
#![no_main]

use libpanda::{environment, process::Child};

libpanda::main! {
    environment::log("Signal test: starting");

    // Test 1: SIGKILL a looping child
    environment::log("Test 1: SIGKILL");
    if !test_sigkill() {
        environment::log("FAIL: Test 1 failed");
        return 1;
    }
    environment::log("Test 1: PASS");

    // Test 2: SIGTERM with graceful handling
    environment::log("Test 2: SIGTERM");
    if !test_sigterm() {
        environment::log("FAIL: Test 2 failed");
        return 1;
    }
    environment::log("Test 2: PASS");

    environment::log("Signal test: all tests passed");
    environment::log("PASS");
    0
}

/// Test SIGKILL: spawn a child, kill it, verify termination with exit code -9.
fn test_sigkill() -> bool {
    let mut child = match Child::spawn("file:/initrd/signal_child") {
        Ok(c) => c,
        Err(_) => {
            environment::log("  SIGKILL: spawn failed");
            return false;
        }
    };

    // Give child a moment to start
    for _ in 0..100 {
        libpanda::process::yield_now();
    }

    // Send SIGKILL
    if let Err(_) = child.kill() {
        environment::log("  SIGKILL: kill() failed");
        return false;
    }

    // Wait for child
    let status = match child.wait() {
        Ok(s) => s,
        Err(_) => {
            environment::log("  SIGKILL: wait() failed");
            return false;
        }
    };

    // Verify exit code is -9 (killed by signal)
    if status.code() != -9 {
        environment::log("  SIGKILL: wrong exit code (expected -9)");
        return false;
    }

    true
}

/// Test SIGTERM: spawn a child that handles SIGTERM gracefully.
fn test_sigterm() -> bool {
    use libpanda::process::Signal;

    let mut child = match Child::spawn("file:/initrd/signal_child") {
        Ok(c) => c,
        Err(_) => {
            environment::log("  SIGTERM: spawn failed");
            return false;
        }
    };

    // Give child a moment to start
    for _ in 0..100 {
        libpanda::process::yield_now();
    }

    // Send SIGTERM
    if let Err(_) = child.signal(Signal::Term) {
        environment::log("  SIGTERM: signal() failed");
        return false;
    }

    // Wait for child to handle it gracefully
    let status = match child.wait() {
        Ok(s) => s,
        Err(_) => {
            environment::log("  SIGTERM: wait() failed");
            return false;
        }
    };

    // Verify clean exit (code 0 indicates graceful shutdown)
    if status.code() != 0 {
        environment::log("  SIGTERM: wrong exit code (expected 0)");
        return false;
    }

    true
}
