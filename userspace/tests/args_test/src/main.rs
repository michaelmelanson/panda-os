#![no_std]
#![no_main]

use libpanda::{environment, process::Child};

libpanda::main! {
    environment::log("Args test: starting");

    // Spawn child with arguments
    let mut child = match Child::spawn_with_args(
        "file:/initrd/args_child",
        &["args_child", "hello", "world", "123"],
    ) {
        Ok(c) => c,
        Err(_) => {
            environment::log("FAIL: spawn failed");
            return 1;
        }
    };

    environment::log("Args test: child spawned");

    // Wait for child to exit
    let status = match child.wait() {
        Ok(s) => s,
        Err(_) => {
            environment::log("FAIL: wait failed");
            return 1;
        }
    };

    if !status.success() {
        environment::log("FAIL: child exited with non-zero code");
        return 1;
    }

    environment::log("Args test: child exited successfully");
    environment::log("PASS");
    0
}
