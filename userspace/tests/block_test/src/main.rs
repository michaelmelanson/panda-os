#![no_std]
#![no_main]

use libpanda::environment;
use libpanda::file;
use panda_abi::SEEK_SET;

libpanda::main! {
    environment::log("Block test starting");

    // Use fixed PCI address - QEMU virtio-blk is at 00:04.0
    const BLOCK_DEVICE_URI: &str = "block:/pci/00:04.0";

    // Test 1: Open the block device
    environment::log("Test 1: Opening block device");
    let Ok(handle) = environment::open(BLOCK_DEVICE_URI, 0) else {
        environment::log("FAIL: Could not open block device at pci/00:04.0");
        return 1;
    };
    environment::log("Block device opened");

    // Test 2: Read from device
    environment::log("Test 2: Reading from block device");
    let mut buf = [0u8; 512];
    let n = file::read(handle, &mut buf);
    if n < 0 {
        environment::log("FAIL: Read from block device failed");
        file::close(handle);
        return 1;
    }
    environment::log("Read succeeded");

    // Test 3: Write, overwrite, and verify
    environment::log("Test 3: Write, overwrite, and verify");

    // Seek to offset 512 (second sector)
    let pos = file::seek(handle, 512, SEEK_SET);
    if pos < 0 {
        environment::log("FAIL: Seek failed");
        file::close(handle);
        return 1;
    }

    // Write initial pattern
    let initial_data = [0xAAu8; 64];
    let n = file::write(handle, &initial_data);
    if n != 64 {
        environment::log("FAIL: Initial write failed");
        file::close(handle);
        return 1;
    }

    // Overwrite with different pattern
    file::seek(handle, 512, SEEK_SET);
    let overwrite_data = b"PANDA_BLOCK_TEST_OVERWRITE_DATA!";
    let n = file::write(handle, overwrite_data);
    if n != 32 {
        environment::log("FAIL: Overwrite failed");
        file::close(handle);
        return 1;
    }

    // Read back and verify against expected final result
    file::seek(handle, 512, SEEK_SET);
    let mut read_buf = [0u8; 64];
    let n = file::read(handle, &mut read_buf);
    if n != 64 {
        environment::log("FAIL: Read-back failed");
        file::close(handle);
        return 1;
    }

    // Expected: 32 bytes of overwrite data + 32 bytes of 0xAA
    let expected: [u8; 64] = *b"PANDA_BLOCK_TEST_OVERWRITE_DATA!\xAA\xAA\xAA\xAA\xAA\xAA\xAA\xAA\xAA\xAA\xAA\xAA\xAA\xAA\xAA\xAA\xAA\xAA\xAA\xAA\xAA\xAA\xAA\xAA\xAA\xAA\xAA\xAA\xAA\xAA\xAA\xAA";

    if read_buf != expected {
        environment::log("FAIL: Data mismatch");
        file::close(handle);
        return 1;
    }
    environment::log("Write/overwrite/read verified");

    file::close(handle);

    environment::log("Block test passed");
    0
}
