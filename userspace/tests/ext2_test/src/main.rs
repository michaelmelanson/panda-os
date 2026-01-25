//! Test reading files from the ext2 filesystem mounted at /mnt.

#![no_std]
#![no_main]

use libpanda::environment;
use libpanda::file;

libpanda::main! {
    environment::log("ext2_test: Starting");

    // Mount ext2 filesystem first
    environment::log("ext2_test: Mounting ext2 filesystem");
    if let Err(_) = environment::mount("ext2", "/mnt") {
        environment::log("FAIL: Could not mount ext2 filesystem");
        return 1;
    }
    environment::log("ext2_test: ext2 mounted at /mnt");

    // Test 1: Open and read hello.txt from ext2
    environment::log("ext2_test: Test 1 - Reading hello.txt");
    let Ok(handle) = environment::open("file:/mnt/hello.txt", 0, 0) else {
        environment::log("FAIL: Could not open file:/mnt/hello.txt");
        return 1;
    };

    let mut buf = [0u8; 64];
    let n = file::read(handle, &mut buf);
    if n <= 0 {
        environment::log("FAIL: Could not read from hello.txt");
        return 1;
    }

    // Verify content contains "Hello from ext2!"
    let content = core::str::from_utf8(&buf[..n as usize]).unwrap_or("");
    if !content.contains("Hello from ext2") {
        environment::log("FAIL: Unexpected content in hello.txt");
        return 1;
    }

    file::close(handle);
    environment::log("ext2_test: Test 1 passed");

    // Test 2: Read nested file
    environment::log("ext2_test: Test 2 - Reading nested file");
    let Ok(handle) = environment::open("file:/mnt/subdir/nested.txt", 0, 0) else {
        environment::log("FAIL: Could not open file:/mnt/subdir/nested.txt");
        return 1;
    };

    let mut buf = [0u8; 64];
    let n = file::read(handle, &mut buf);
    if n <= 0 {
        environment::log("FAIL: Could not read from nested.txt");
        return 1;
    }

    file::close(handle);
    environment::log("ext2_test: Test 2 passed");

    // Test 3: Read deeply nested file
    environment::log("ext2_test: Test 3 - Reading deep path");
    let Ok(handle) = environment::open("file:/mnt/a/b/c/deep.txt", 0, 0) else {
        environment::log("FAIL: Could not open file:/mnt/a/b/c/deep.txt");
        return 1;
    };

    let mut buf = [0u8; 64];
    let n = file::read(handle, &mut buf);
    if n <= 0 {
        environment::log("FAIL: Could not read from deep.txt");
        return 1;
    }

    let content = core::str::from_utf8(&buf[..n as usize]).unwrap_or("");
    if !content.contains("Deep file") {
        environment::log("FAIL: Unexpected content in deep.txt");
        return 1;
    }

    file::close(handle);
    environment::log("ext2_test: Test 3 passed");

    // Test 4: Read large file (multiple blocks)
    environment::log("ext2_test: Test 4 - Reading large file");
    let Ok(handle) = environment::open("file:/mnt/large.bin", 0, 0) else {
        environment::log("FAIL: Could not open file:/mnt/large.bin");
        return 1;
    };

    // Read in chunks and count total bytes
    let mut total = 0usize;
    let mut buf = [0u8; 1024];
    loop {
        let n = file::read(handle, &mut buf);
        if n <= 0 {
            break;
        }
        total += n as usize;
    }

    // Large file is 8KB
    if total != 8192 {
        environment::log("FAIL: Large file size mismatch");
        return 1;
    }

    file::close(handle);
    environment::log("ext2_test: Test 4 passed");

    environment::log("ext2_test: All tests passed!");
    0
}
