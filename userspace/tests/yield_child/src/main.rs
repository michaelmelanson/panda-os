#![no_std]
#![no_main]

use libpanda::syscall::{syscall_log, yield_now};

libpanda::main! {
    for i in 0..3 {
        syscall_log("Child: before yield");
        yield_now();
        syscall_log("Child: after yield");
    }
    syscall_log("Child: done");
    0
}
