#![no_std]
#![no_main]

use libpanda::syscall::{spawn, syscall_log};

libpanda::main! {
    syscall_log("Spawn test: starting");

    // Spawn a child process
    let result = spawn("/initrd/spawn_child");
    if result < 0 {
        syscall_log("FAIL: spawn returned error");
        return 1;
    }

    syscall_log("Spawn test: child spawned successfully");
    syscall_log("Spawn test: parent exiting with code 0");
    0
}
