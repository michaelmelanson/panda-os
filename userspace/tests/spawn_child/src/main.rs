#![no_std]
#![no_main]

use libpanda::syscall::syscall_log;

libpanda::main! {
    syscall_log("Child process running!");
    syscall_log("Child process exiting with code 0");
    0
}
