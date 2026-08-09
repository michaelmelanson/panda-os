#![no_std]
#![no_main]

use libpanda::{environment, file, DirEntry};

libpanda::main! {
    environment::log("readdir test starting");

    // This test plays the role of `init`, so it starts the filesystem
    // service that provides `file:` itself (roadmap M5) — see `vfs_test`.
    let Ok(fs_handle) = environment::spawn("initrd:/fs") else {
        environment::log("FAIL: could not spawn filesystem service");
        return 1;
    };
    if libpanda::process::wait_ready(fs_handle).is_err() {
        environment::log("FAIL: filesystem service did not become ready");
        return 1;
    }

    // Test: Open the initrd directory
    let handle = if let Ok(h) = environment::opendir("file:/initrd") {
        h
    } else {
        environment::log("FAIL: Could not open file:/initrd");
        return 1;
    };

    // Test: Read directory entries
    let mut count = 0;
    let mut entry = DirEntry {
        name_len: 0,
        is_dir: false,
        name: [0; 255],
    };

    loop {
        let result = file::readdir(handle, &mut entry);
        if result < 0 {
            environment::log("FAIL: readdir returned error");
            return 1;
        }
        if result == 0 {
            // End of directory
            break;
        }
        count += 1;
    }

    if count == 0 {
        environment::log("FAIL: No entries found in /initrd");
        return 1;
    }

    // Test: Close the directory handle
    if file::close(handle) < 0 {
        environment::log("FAIL: Could not close directory handle");
        return 1;
    }

    // Ask the filesystem service to exit, or QEMU never shuts down — see
    // `ext2_test`'s identical shutdown request.
    if let Some(fs_channel) = libpanda::ipc::Channel::from_handle_borrowed(fs_handle) {
        let _ = fs_channel.send(b"shutdown");
    }

    environment::log("readdir test passed");
    0
}
