#![no_std]
#![no_main]

use libpanda::{environment, process};

libpanda::main! {
    environment::log("Args test: starting");

    // Spawn child with arguments
    let Ok(child_handle) = environment::spawn(
        "file:/initrd/args_child",
        &["args_child", "hello", "world", "123"],
        0, 0,
    ) else {
        environment::log("FAIL: spawn failed");
        return 1;
    };

    environment::log("Args test: child spawned");

    // Wait for child to exit
    let exit_code = process::wait(child_handle);
    if exit_code != 0 {
        environment::log("FAIL: child exited with non-zero code");
        return 1;
    }

    environment::log("Args test: child exited successfully");
    environment::log("PASS");
    0
}
