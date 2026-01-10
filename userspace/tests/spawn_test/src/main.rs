#![no_std]
#![no_main]

use libpanda::{environment, process};

libpanda::main! {
    environment::log("Spawn test: starting");

    // Spawn a child process using file: scheme
    let child_handle = environment::spawn("file:/initrd/spawn_child");
    if child_handle < 0 {
        environment::log("FAIL: spawn returned error");
        return 1;
    }

    environment::log("Spawn test: child spawned, waiting for exit...");

    // Wait for the child to exit
    let exit_code = process::wait(child_handle as u32);

    if exit_code == 42 {
        environment::log("Spawn test: child exited with expected code 42");
        environment::log("PASS");
        0
    } else {
        environment::log("FAIL: child exited with unexpected code");
        1
    }
}
