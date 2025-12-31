#![no_std]
#![no_main]

use libpanda::syscall::{syscall_exit, syscall_log};

#[unsafe(no_mangle)]
extern "C" fn _start() -> ! {
    syscall_log("HELLO FROM USERSPACE");
    syscall_exit(0);
}

#[panic_handler]
#[cfg(not(test))]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    loop {}
}
