#![no_std]
#![no_main]

use libpanda::syscall::{spawn, syscall_log, yield_now};

libpanda::main! {
    syscall_log("Parent: spawning child");
    if spawn("/initrd/yield_child") < 0 {
        syscall_log("Parent: FAIL - spawn failed");
        return 1;
    }

    for i in 0..3 {
        syscall_log("Parent: before yield");
        yield_now();
        syscall_log("Parent: after yield");
    }

    syscall_log("Parent: done");
    0
}
