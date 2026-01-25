#![no_std]
#![no_main]

use libpanda::environment;
use libpanda::file;

libpanda::main! {
    environment::log("Resource test starting");

    // Test 1: Open a file using file: scheme
    environment::log("Test 1: file: scheme");
    let handle = if let Ok(h) = environment::open("file:/initrd/hello.txt", 0, 0) {
        h
    } else {
        environment::log("FAIL: Could not open file:/initrd/hello.txt");
        return 1;
    };

    // Read from the file to verify it works
    let mut buf = [0u8; 64];
    let n = file::read(handle, &mut buf);
    if n <= 0 {
        environment::log("FAIL: Could not read from file");
        return 1;
    }
    file::close(handle);
    environment::log("Test 1 passed");

    // Test 2: Open console using console: scheme
    environment::log("Test 2: console: scheme");
    let console = if let Ok(h) = environment::open("console:/serial/0", 0, 0) {
        h
    } else {
        environment::log("FAIL: Could not open console:/serial/0");
        return 1;
    };

    // Write to console
    let msg = b"Hello from console write!\n";
    let n = file::write(console, msg);
    if n != msg.len() as isize {
        environment::log("FAIL: Console write returned wrong count");
        return 1;
    }
    file::close(console);
    environment::log("Test 2 passed");

    // Test 3: Invalid scheme should fail
    environment::log("Test 3: invalid scheme");
    if let Ok(_) = environment::open("badscheme:/foo", 0, 0) {
        environment::log("FAIL: Invalid scheme should return error");
        return 1;
    }
    environment::log("Test 3 passed");

    // Test 4: Invalid path within scheme should fail
    environment::log("Test 4: invalid path");
    if let Ok(_) = environment::open("console:/invalid/path", 0, 0) {
        environment::log("FAIL: Invalid path should return error");
        return 1;
    }
    environment::log("Test 4 passed");

    environment::log("Resource test passed");
    0
}
