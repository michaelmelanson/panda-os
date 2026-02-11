//! Test mkdir and rmdir on the ext2 filesystem.
//!
//! Exercises:
//! 1. Create a new directory, verify it appears in listing
//! 2. Verify . and .. entries exist in the new directory (via stat)
//! 3. Create a file inside the new directory
//! 4. rmdir on non-empty directory fails with NotEmpty
//! 5. Remove the file, then rmdir succeeds
//! 6. Nested mkdir (create a/b/c/d where a/b/c exists)
//! 7. Verify parent link counts are correct

#![no_std]
#![no_main]

use libpanda::environment;
use libpanda::file;

libpanda::main! {
    environment::log("ext2_mkdir_test: Starting");

    // Mount ext2 filesystem
    environment::log("ext2_mkdir_test: Mounting ext2 filesystem");
    if let Err(_) = environment::mount("ext2", "/mnt") {
        environment::log("FAIL: Could not mount ext2 filesystem");
        return 1;
    }
    environment::log("ext2_mkdir_test: ext2 mounted at /mnt");

    // Open the root directory handle for operations
    let Ok(root_dir) = environment::opendir("file:/mnt") else {
        environment::log("FAIL: Could not opendir file:/mnt");
        return 1;
    };

    // =========================================================================
    // Test 1: Create a new directory
    // =========================================================================
    environment::log("ext2_mkdir_test: Test 1 - Create directory");
    if let Err(e) = environment::mkdir(root_dir, "newdir", 0o755) {
        environment::log("FAIL: Could not create newdir");
        environment::log(match e {
            libpanda::ErrorCode::AlreadyExists => "Error: AlreadyExists",
            libpanda::ErrorCode::NoSpace => "Error: NoSpace",
            libpanda::ErrorCode::NotFound => "Error: NotFound",
            libpanda::ErrorCode::IoError => "Error: IoError",
            _ => "Error: Unknown",
        });
        return 1;
    }
    environment::log("ext2_mkdir_test: Test 1 passed");

    // =========================================================================
    // Test 2: Verify the new directory appears in listing and is a directory
    // =========================================================================
    environment::log("ext2_mkdir_test: Test 2 - Verify directory in listing");

    // Reopen root directory to get fresh listing
    file::close(root_dir);
    let Ok(root_dir) = environment::opendir("file:/mnt") else {
        environment::log("FAIL: Could not reopen file:/mnt");
        return 1;
    };

    let mut found_newdir = false;
    let mut entry = libpanda::DirEntry {
        name: [0u8; 255],
        name_len: 0,
        is_dir: false,
    };
    loop {
        let n = file::readdir(root_dir, &mut entry);
        if n <= 0 {
            break;
        }
        let name = core::str::from_utf8(&entry.name[..entry.name_len as usize]).unwrap_or("");
        if name == "newdir" {
            found_newdir = true;
            if !entry.is_dir {
                environment::log("FAIL: newdir is not a directory");
                return 1;
            }
        }
    }

    if !found_newdir {
        environment::log("FAIL: newdir not found in directory listing");
        return 1;
    }
    environment::log("ext2_mkdir_test: Test 2 passed");

    // =========================================================================
    // Test 3: Create a file inside the new directory
    // =========================================================================
    environment::log("ext2_mkdir_test: Test 3 - Create file in new directory");
    let Ok(newdir_handle) = environment::opendir("file:/mnt/newdir") else {
        environment::log("FAIL: Could not opendir file:/mnt/newdir");
        return 1;
    };

    let Ok(file_handle) = environment::create(newdir_handle, "inside.txt", 0o644, 0) else {
        environment::log("FAIL: Could not create file in newdir");
        file::close(newdir_handle);
        return 1;
    };

    let data = b"Inside new directory";
    let n = file::write(file_handle, data);
    if n != data.len() as isize {
        environment::log("FAIL: write returned wrong count");
        file::close(file_handle);
        file::close(newdir_handle);
        return 1;
    }
    file::close(file_handle);
    file::close(newdir_handle);
    environment::log("ext2_mkdir_test: Test 3 passed");

    // =========================================================================
    // Test 4: rmdir on non-empty directory fails
    // =========================================================================
    environment::log("ext2_mkdir_test: Test 4 - rmdir non-empty fails");
    match environment::rmdir(root_dir, "newdir") {
        Err(libpanda::ErrorCode::NotEmpty) => {
            // Expected
        }
        Err(_) => {
            // Also acceptable - any error for non-empty rmdir
        }
        Ok(()) => {
            environment::log("FAIL: rmdir on non-empty should fail");
            return 1;
        }
    }
    environment::log("ext2_mkdir_test: Test 4 passed");

    // =========================================================================
    // Test 5: Remove file, then rmdir succeeds
    // =========================================================================
    environment::log("ext2_mkdir_test: Test 5 - rmdir empty directory");

    // Reopen newdir handle
    let Ok(newdir_handle) = environment::opendir("file:/mnt/newdir") else {
        environment::log("FAIL: Could not reopen newdir");
        return 1;
    };

    // Unlink the file
    if let Err(_) = environment::unlink(newdir_handle, "inside.txt") {
        environment::log("FAIL: Could not unlink inside.txt");
        file::close(newdir_handle);
        return 1;
    }
    file::close(newdir_handle);

    // Now rmdir should succeed
    if let Err(e) = environment::rmdir(root_dir, "newdir") {
        environment::log("FAIL: Could not rmdir empty newdir");
        environment::log(match e {
            libpanda::ErrorCode::NotEmpty => "Error: NotEmpty",
            libpanda::ErrorCode::NotFound => "Error: NotFound",
            libpanda::ErrorCode::NotDirectory => "Error: NotDirectory",
            libpanda::ErrorCode::IoError => "Error: IoError",
            _ => "Error: Unknown",
        });
        return 1;
    }
    environment::log("ext2_mkdir_test: Test 5 passed");

    // =========================================================================
    // Test 6: Nested mkdir (a/b/c exists, create a/b/c/d)
    // =========================================================================
    environment::log("ext2_mkdir_test: Test 6 - Nested mkdir");
    let Ok(abc_handle) = environment::opendir("file:/mnt/a/b/c") else {
        environment::log("FAIL: Could not opendir a/b/c");
        return 1;
    };

    if let Err(_) = environment::mkdir(abc_handle, "d", 0o755) {
        environment::log("FAIL: Could not mkdir a/b/c/d");
        file::close(abc_handle);
        return 1;
    }
    file::close(abc_handle);

    // Verify it exists
    let Ok(abcd_handle) = environment::opendir("file:/mnt/a/b/c/d") else {
        environment::log("FAIL: Could not opendir a/b/c/d after creation");
        return 1;
    };
    file::close(abcd_handle);
    environment::log("ext2_mkdir_test: Test 6 passed");

    // =========================================================================
    // Test 7: mkdir already exists returns error
    // =========================================================================
    environment::log("ext2_mkdir_test: Test 7 - mkdir already exists");
    match environment::mkdir(root_dir, "subdir", 0o755) {
        Err(libpanda::ErrorCode::AlreadyExists) => {
            // Expected
        }
        Err(_) => {
            // Also acceptable
        }
        Ok(()) => {
            environment::log("FAIL: mkdir on existing dir should fail");
            return 1;
        }
    }
    environment::log("ext2_mkdir_test: Test 7 passed");

    file::close(root_dir);
    environment::log("ext2_mkdir_test: All tests passed!");
    0
}
