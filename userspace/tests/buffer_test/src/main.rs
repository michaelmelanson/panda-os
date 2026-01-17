#![no_std]
#![no_main]

use libpanda::buffer::Buffer;
use libpanda::environment;

libpanda::main! {
    environment::log("Buffer test starting");

    // Test 1: Allocate a buffer
    let Some(mut buf) = Buffer::alloc(4096) else {
        environment::log("FAIL: Could not allocate buffer");
        return 1;
    };
    environment::log("PASS: Buffer allocated");

    // Test 2: Check buffer size (should be at least requested size)
    if buf.size() < 4096 {
        environment::log("FAIL: Buffer size too small");
        return 1;
    }
    environment::log("PASS: Buffer size correct");

    // Test 3: Write to buffer
    let slice = buf.as_mut_slice();
    slice[0] = 0xDE;
    slice[1] = 0xAD;
    slice[2] = 0xBE;
    slice[3] = 0xEF;
    environment::log("PASS: Wrote to buffer");

    // Test 4: Read back from buffer
    let slice = buf.as_slice();
    if slice[0] != 0xDE || slice[1] != 0xAD || slice[2] != 0xBE || slice[3] != 0xEF {
        environment::log("FAIL: Buffer data mismatch");
        return 1;
    }
    environment::log("PASS: Read back correct data");

    // Test 5: Read file into buffer
    let Ok(file_handle) = environment::open("file:/initrd/hello.txt", 0) else {
        environment::log("FAIL: Could not open test file");
        return 1;
    };

    let Some(bytes_read) = buf.read_from(file_handle) else {
        environment::log("FAIL: Could not read file into buffer");
        return 1;
    };
    if bytes_read == 0 {
        environment::log("FAIL: Read 0 bytes from file");
        return 1;
    }
    environment::log("PASS: Read file into buffer");

    // Test 6: Verify file content in buffer
    let slice = buf.as_slice();
    // hello.txt should start with "Hello"
    if slice[0] != b'H' || slice[1] != b'e' || slice[2] != b'l' || slice[3] != b'l' || slice[4] != b'o' {
        environment::log("FAIL: File content mismatch in buffer");
        return 1;
    }
    environment::log("PASS: File content correct in buffer");

    // Test 7: Buffer is automatically freed when dropped
    drop(buf);
    environment::log("PASS: Buffer freed");

    // Test 8: Allocate multiple buffers to test tracking
    let Some(buf1) = Buffer::alloc(4096) else {
        environment::log("FAIL: Could not allocate multiple buffers");
        return 1;
    };
    let Some(buf2) = Buffer::alloc(8192) else {
        environment::log("FAIL: Could not allocate multiple buffers");
        return 1;
    };
    let Some(buf3) = Buffer::alloc(4096) else {
        environment::log("FAIL: Could not allocate multiple buffers");
        return 1;
    };
    environment::log("PASS: Allocated multiple buffers");

    // Test 9: Free them in different order
    drop(buf2);
    drop(buf1);
    drop(buf3);
    environment::log("PASS: Freed multiple buffers");

    // Test 10: Allocate again after freeing
    let Some(_buf4) = Buffer::alloc(16384) else {
        environment::log("FAIL: Could not allocate buffer after freeing");
        return 1;
    };
    environment::log("PASS: Allocated buffer after freeing");

    environment::log("Buffer test passed");
    0
}
