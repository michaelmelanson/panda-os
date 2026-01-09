#![no_std]
#![no_main]

use libpanda::syscall::{self, syscall_log};

libpanda::main! {
    syscall_log("VFS test starting");

    // Test: Open a file from initrd
    let handle = syscall::open("/initrd/hello.txt");
    if handle < 0 {
        syscall_log("FAIL: Could not open /initrd/hello.txt");
        return 1;
    }

    // Test: Read from the file
    let mut buf = [0u8; 64];
    let n = syscall::read(handle as u32, &mut buf);
    if n <= 0 {
        syscall_log("FAIL: Could not read from file");
        return 1;
    }

    // Test: Close the file
    syscall::close(handle as u32);

    syscall_log("VFS test passed");
    0
}
