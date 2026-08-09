#![no_std]
#![no_main]

use libpanda::environment;
use libpanda::file;

libpanda::main! {
    environment::log("VFS test starting");

    // This test plays the role of `init` (it's the initrd's `init` binary),
    // so nothing has started the filesystem service that provides `file:`
    // (roadmap M5) — do it directly, the same way the ext2 tests spawn
    // virtio-blk. `wait_ready` returns once the service has mounted and
    // registered `file:`, so the open below can't race it.
    let Ok(fs_handle) = environment::spawn("initrd:/fs") else {
        environment::log("FAIL: could not spawn filesystem service");
        return 1;
    };
    if libpanda::process::wait_ready(fs_handle).is_err() {
        environment::log("FAIL: filesystem service did not become ready");
        return 1;
    }

    // Test: Open a file from initrd using file: scheme
    let Ok(handle) = environment::open("file:/initrd/hello.txt", 0, 0) else {
        environment::log("FAIL: Could not open file:/initrd/hello.txt");
        return 1;
    };

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
