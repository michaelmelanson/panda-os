#![no_std]
#![no_main]

use libpanda::environment;
use libpanda::file;

libpanda::main! {
    environment::log("VFS test starting");

    // Test: Open a file from initrd
    let handle = environment::open("/initrd/hello.txt", 0);
    if handle < 0 {
        environment::log("FAIL: Could not open /initrd/hello.txt");
        return 1;
    }
    let handle = handle as u32;

    // Test: Read from the file
    let mut buf = [0u8; 64];
    let n = file::read(handle, &mut buf);
    if n <= 0 {
        environment::log("FAIL: Could not read from file");
        return 1;
    }

    // Test: Close the file
    file::close(handle);

    environment::log("VFS test passed");
    0
}
