//! Test reading files from the ext2 filesystem mounted at /mnt.

#![no_std]
#![no_main]

use libpanda::environment;
use libpanda::file;
use libpanda::ipc::Channel;

libpanda::main! {
    environment::log("ext2_test: Starting");

    // This test plays the role of `init` (it's the initrd's `init`
    // binary), so unlike the real boot path there is no service manager
    // running to spawn the virtio-blk driver the mount below needs — do it
    // directly, the same way `keyboard_test` spawns virtio-keyboard.
    let Ok(driver_handle) = environment::spawn("initrd:/virtio-blk") else {
        environment::log("FAIL: could not spawn virtio-blk driver");
        return 1;
    };
    let Some(driver_channel) = Channel::from_handle_borrowed(driver_handle.into()) else {
        environment::log("FAIL: virtio-blk driver handle is not a channel");
        return 1;
    };
    let mut ready_msg = [0u8; 64];
    if driver_channel.recv(&mut ready_msg).is_err() {
        environment::log("FAIL: did not receive ready signal from virtio-blk driver");
        return 1;
    }

    // Start the filesystem service: it registers `file:` and mounts ext2
    // on `block:/pci/storage/0` itself. There is no mount syscall — the
    // kernel has no mount table — so this spawn plus
    // its readiness handshake is what replaces it: `wait_ready` returns only
    // once the mount has been attempted and `file:` is registered.
    environment::log("ext2_test: Starting filesystem service");
    let Ok(fs_handle) = environment::spawn("initrd:/fs") else {
        environment::log("FAIL: could not spawn filesystem service");
        return 1;
    };
    if libpanda::process::wait_ready(fs_handle).is_err() {
        environment::log("FAIL: filesystem service did not become ready");
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

    // The virtio-blk driver runs forever (a persistent driver process in
    // the real boot path), so this test (playing `init`) must ask it to
    // exit before returning itself, or QEMU never shuts down — see
    // `block_test`'s identical shutdown request.
    // The filesystem service is likewise persistent and must be asked to
    // exit too.
    if let Some(fs_channel) = libpanda::ipc::Channel::from_handle_borrowed(fs_handle) {
        let _ = fs_channel.send(b"shutdown");
    }
    let _ = driver_channel.send(b"shutdown");

    environment::log("ext2_test: All tests passed!");
    0
}
