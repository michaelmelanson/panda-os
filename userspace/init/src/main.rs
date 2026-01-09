#![no_std]
#![no_main]

use libpanda::syscall::{self, syscall_exit, syscall_log};

#[unsafe(no_mangle)]
extern "C" fn _start() -> ! {
    syscall_log("HELLO FROM USERSPACE");

    // Test VFS: open and read a file from the initrd
    syscall_log("Opening /initrd/hello.txt...");
    let handle = syscall::open("/initrd/hello.txt");
    if handle < 0 {
        syscall_log("ERROR: Failed to open file");
        syscall_exit(1);
    }
    syscall_log("File opened successfully");

    syscall_log("About to read...");

    // Read the file contents
    let mut buf = [0u8; 64];
    let bytes_read = syscall::read(handle as u32, &mut buf);

    syscall_log("Read returned");

    if bytes_read < 0 {
        syscall_log("ERROR: Failed to read file");
        syscall_exit(1);
    }

    syscall_log("Read successful");

    // Close the file
    syscall::close(handle as u32);
    syscall_log("File closed");

    syscall_log("VFS test passed!");
    syscall_exit(0);
}

#[panic_handler]
#[cfg(not(test))]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    loop {}
}
