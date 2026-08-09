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
use libpanda::ipc::Channel;

libpanda::main! {
    environment::log("ext2_create_test: Starting");

    // This test plays the role of `init` (it's the initrd's `init`
    // binary), so unlike the real boot path there is no service manager
    // running to spawn the virtio-blk driver the mount below needs — do it
    // directly, the same way `keyboard_test` spawns virtio-keyboard (M4
    // Phase 2, plans/device-driver-model.md).
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

    // Start the filesystem service, which registers `file:` and mounts
    // ext2 itself — see `ext2_test` for why this replaces the deleted mount
    // syscall (roadmap M5).
    environment::log("ext2_create_test: Starting filesystem service");
    let Ok(fs_handle) = environment::spawn("initrd:/fs") else {
        environment::log("FAIL: could not spawn filesystem service");
        return 1;
    };
    if libpanda::process::wait_ready(fs_handle).is_err() {
        environment::log("FAIL: filesystem service did not become ready");
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

    // The virtio-blk driver runs forever (a persistent driver process in
    // the real boot path), so this test (playing `init`) must ask it to
    // exit before returning itself, or QEMU never shuts down — see
    // `block_test`'s identical shutdown request.
    let _ = driver_channel.send(b"shutdown");

    environment::log("ext2_create_test: All tests passed!");
    0
}
