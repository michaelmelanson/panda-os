#![no_std]
#![no_main]

use libpanda::{environment, file, DirEntry};

libpanda::main! {
    environment::log("readdir test starting");

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

    environment::log("readdir test passed");
    0
}
