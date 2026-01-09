#![no_std]
#![no_main]

use libpanda::syscall::syscall_log;

libpanda::main! {
    syscall_log("HELLO FROM USERSPACE");
    0
}
