//! Test creating and deleting files on the ext2 filesystem.
//!
//! Exercises:
//! 1. Create a new file in the root directory, write to it, and read back
//! 2. Create a new file in a subdirectory
//! 3. Attempt to create a duplicate file (expect AlreadyExists)
//! 4. Unlink a file and verify it disappears
//! 5. Verify directory listings reflect create/unlink changes

#![no_std]
#![no_main]

use libpanda::environment;
use libpanda::file;

libpanda::main! {
    environment::log("ext2_create_test: Starting");

    // Mount ext2 filesystem
    environment::log("ext2_create_test: Mounting ext2 filesystem");
    if let Err(_) = environment::mount("ext2", "/mnt") {
        environment::log("FAIL: Could not mount ext2 filesystem");
        return 1;
    }
    environment::log("ext2_create_test: ext2 mounted at /mnt");

    // Open the root directory handle for create/unlink operations
    let Ok(root_dir) = environment::opendir("file:/mnt") else {
        environment::log("FAIL: Could not opendir file:/mnt");
        return 1;
    };

    // =========================================================================
    // Test 1: Create a new file, write to it, close, reopen and read back
    // =========================================================================
    environment::log("ext2_create_test: Test 1 - Create new file");
    let Ok(handle) = environment::create(root_dir, "newfile.txt", 0o644, 0) else {
        environment::log("FAIL: Could not create newfile.txt in /mnt");
        return 1;
    };

    let data = b"Created by Panda!";
    let n = file::write(handle, data);
    if n != data.len() as isize {
        environment::log("FAIL: write returned wrong count");
        return 1;
    }
    file::close(handle);

    // Reopen and read back
    let Ok(handle) = environment::open("file:/mnt/newfile.txt", 0, 0) else {
        environment::log("FAIL: Could not reopen file:/mnt/newfile.txt");
        return 1;
    };
    let mut buf = [0u8; 64];
    let n = file::read(handle, &mut buf);
    if n <= 0 {
        environment::log("FAIL: Could not read after create");
        return 1;
    }
    let content = core::str::from_utf8(&buf[..n as usize]).unwrap_or("");
    if content != "Created by Panda!" {
        environment::log("FAIL: Read-back content mismatch");
        return 1;
    }
    file::close(handle);
    environment::log("ext2_create_test: Test 1 passed");

    // =========================================================================
    // Test 2: Create a file in a subdirectory
    // =========================================================================
    environment::log("ext2_create_test: Test 2 - Create file in subdirectory");
    let Ok(subdir_handle) = environment::opendir("file:/mnt/subdir") else {
        environment::log("FAIL: Could not opendir file:/mnt/subdir");
        return 1;
    };
    let Ok(handle) = environment::create(subdir_handle, "created.txt", 0o644, 0) else {
        environment::log("FAIL: Could not create created.txt in /mnt/subdir");
        return 1;
    };

    let data = b"In subdir";
    let n = file::write(handle, data);
    if n != data.len() as isize {
        environment::log("FAIL: subdir write returned wrong count");
        return 1;
    }
    file::close(handle);
    file::close(subdir_handle);
    environment::log("ext2_create_test: Test 2 passed");

    // =========================================================================
    // Test 3: Create duplicate file returns error
    // =========================================================================
    environment::log("ext2_create_test: Test 3 - Duplicate create returns error");
    match environment::create(root_dir, "newfile.txt", 0o644, 0) {
        Err(libpanda::ErrorCode::AlreadyExists) => {
            // Expected
        }
        Err(_) => {
            // Also acceptable — any error for duplicate create
        }
        Ok(h) => {
            file::close(h);
            environment::log("FAIL: Duplicate create succeeded");
            return 1;
        }
    }
    environment::log("ext2_create_test: Test 3 passed");

    // =========================================================================
    // Test 4: Unlink a file
    // =========================================================================
    environment::log("ext2_create_test: Test 4 - Unlink file");
    if let Err(_) = environment::unlink(root_dir, "hello.txt") {
        environment::log("FAIL: Could not unlink hello.txt from /mnt");
        return 1;
    }

    // Verify it's gone
    match environment::open("file:/mnt/hello.txt", 0, 0) {
        Err(_) => {
            // Expected — file not found
        }
        Ok(h) => {
            file::close(h);
            environment::log("FAIL: Unlinked file still openable");
            return 1;
        }
    }
    environment::log("ext2_create_test: Test 4 passed");

    // =========================================================================
    // Test 5: Verify directory listing
    // =========================================================================
    environment::log("ext2_create_test: Test 5 - Verify directory listing");

    // Close the old root_dir handle (its snapshot is stale) and reopen
    file::close(root_dir);
    let Ok(dir_handle) = environment::opendir("file:/mnt") else {
        environment::log("FAIL: Could not opendir file:/mnt");
        return 1;
    };

    let mut found_newfile = false;
    let mut found_hello = false;
    let mut entry = libpanda::DirEntry {
        name: [0u8; 255],
        name_len: 0,
        is_dir: false,
    };
    loop {
        let n = file::readdir(dir_handle, &mut entry);
        if n <= 0 {
            break;
        }
        let name = core::str::from_utf8(&entry.name[..entry.name_len as usize]).unwrap_or("");
        if name == "newfile.txt" {
            found_newfile = true;
        }
        if name == "hello.txt" {
            found_hello = true;
        }
    }
    file::close(dir_handle);

    if !found_newfile {
        environment::log("FAIL: newfile.txt not found in directory listing");
        return 1;
    }
    if found_hello {
        environment::log("FAIL: hello.txt still in directory listing after unlink");
        return 1;
    }
    environment::log("ext2_create_test: Test 5 passed");

    environment::log("ext2_create_test: All tests passed!");
    0
}
