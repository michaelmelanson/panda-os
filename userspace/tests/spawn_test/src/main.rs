#![no_std]
#![no_main]

use libpanda::{environment, process::Child};

libpanda::main! {
    environment::log("Spawn test: starting");

    // Spawn a child process
    let mut child = match Child::spawn("file:/initrd/spawn_child") {
        Ok(c) => c,
        Err(_) => {
            environment::log("FAIL: spawn returned error");
            return 1;
        }
    };

    environment::log("Spawn test: child spawned, waiting for exit...");

    // Wait for the child to exit
    let status = match child.wait() {
        Ok(s) => s,
        Err(_) => {
            environment::log("FAIL: wait returned error");
            return 1;
        }
    };

    if status.code() == 42 {
        environment::log("Spawn test: child exited with expected code 42");
        environment::log("PASS");
        0
    } else {
        environment::log("FAIL: child exited with unexpected code");
        1
    }
}
