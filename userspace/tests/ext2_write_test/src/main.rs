//! Test writing files on the ext2 filesystem mounted at /mnt.
//!
//! Exercises several write paths:
//! 1. Overwrite existing file content, then read back
//! 2. Write that extends file size (append by seeking past current data)
//! 3. Multi-block write (larger than one block)
//! 4. Partial block write preserves surrounding data

#![no_std]
#![no_main]

use libpanda::environment;
use libpanda::file;
use libpanda::handle::Handle;
use libpanda::ipc::Channel;

/// Seek to absolute position (SEEK_SET = 0).
fn seek_to(handle: Handle, pos: i64) {
    file::seek(handle, pos, 0);
}

/// Read the entire file from position 0, returning the number of bytes read.
fn read_all(handle: Handle, buf: &mut [u8]) -> usize {
    seek_to(handle, 0);
    let mut total = 0usize;
    loop {
        let n = file::read(handle, &mut buf[total..]);
        if n <= 0 {
            break;
        }
        total += n as usize;
    }
    total
}

libpanda::main! {
    environment::log("ext2_write_test: Starting");

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

    // Start the filesystem service, which registers `file:` and mounts
    // ext2 itself — see `ext2_test` for why there is no mount syscall to
    // call instead.
    environment::log("ext2_write_test: Starting filesystem service");
    let Ok(fs_handle) = environment::spawn("initrd:/fs") else {
        environment::log("FAIL: could not spawn filesystem service");
        return 1;
    };
    if libpanda::process::wait_ready(fs_handle).is_err() {
        environment::log("FAIL: filesystem service did not become ready");
        return 1;
    }
    environment::log("ext2_write_test: ext2 mounted at /mnt");

    // =========================================================================
    // Test 1: Write data to hello.txt (overwrite existing content)
    // =========================================================================
    environment::log("ext2_write_test: Test 1 - Write to existing file");
    let Ok(handle) = environment::open("file:/mnt/hello.txt", 0, 0) else {
        environment::log("FAIL: Could not open file:/mnt/hello.txt");
        return 1;
    };

    let data = b"Written by Panda OS!";
    let n = file::write(handle, data);
    if n != data.len() as isize {
        environment::log("FAIL: write returned wrong count");
        return 1;
    }

    // Seek back to start and read
    seek_to(handle, 0);
    let mut buf = [0u8; 64];
    let n = file::read(handle, &mut buf);
    if n <= 0 {
        environment::log("FAIL: Could not read after write");
        return 1;
    }
    let content = core::str::from_utf8(&buf[..n as usize]).unwrap_or("");
    if !content.starts_with("Written by Panda OS!") {
        environment::log("FAIL: Read-back content mismatch");
        return 1;
    }
    file::close(handle);
    environment::log("ext2_write_test: Test 1 passed");

    // =========================================================================
    // Test 2: Write that extends the file (append past current data)
    // =========================================================================
    environment::log("ext2_write_test: Test 2 - Write extends file size");
    let Ok(handle) = environment::open("file:/mnt/hello.txt", 0, 0) else {
        environment::log("FAIL: Could not reopen file:/mnt/hello.txt");
        return 1;
    };

    // Read to find file size
    let mut tmp = [0u8; 256];
    let file_size = read_all(handle, &mut tmp);

    // Seek to end (we know the size now) and write more data
    seek_to(handle, file_size as i64);
    let extra = b" Extra data appended.";
    let n = file::write(handle, extra);
    if n != extra.len() as isize {
        environment::log("FAIL: append write returned wrong count");
        return 1;
    }

    // Read from the beginning to verify full content
    let mut buf = [0u8; 256];
    let total = read_all(handle, &mut buf);
    if total == 0 {
        environment::log("FAIL: Could not read after append");
        return 1;
    }
    let content = core::str::from_utf8(&buf[..total]).unwrap_or("");
    if !content.contains("Extra data appended") {
        environment::log("FAIL: Appended content not found");
        return 1;
    }
    file::close(handle);
    environment::log("ext2_write_test: Test 2 passed");

    // =========================================================================
    // Test 3: Multi-block write (write 4KB of data to large.bin which is 8KB)
    // =========================================================================
    environment::log("ext2_write_test: Test 3 - Multi-block write");
    let Ok(handle) = environment::open("file:/mnt/large.bin", 0, 0) else {
        environment::log("FAIL: Could not open file:/mnt/large.bin");
        return 1;
    };

    // Create a 4096-byte pattern
    let mut pattern = [0u8; 4096];
    let mut i = 0usize;
    while i < pattern.len() {
        pattern[i] = (i & 0xFF) as u8;
        i += 1;
    }

    let n = file::write(handle, &pattern);
    if n != 4096 {
        environment::log("FAIL: multi-block write returned wrong count");
        return 1;
    }

    // Read it back from the start, checking in chunks
    seek_to(handle, 0);
    let mut read_buf = [0u8; 1024];
    let mut total_read = 0usize;
    let mut ok = true;
    while total_read < 4096 {
        let n = file::read(handle, &mut read_buf);
        if n <= 0 { break; }
        let chunk = n as usize;
        // Verify this chunk matches the pattern
        i = 0;
        while i < chunk && (total_read + i) < 4096 {
            if read_buf[i] != ((total_read + i) & 0xFF) as u8 {
                ok = false;
                break;
            }
            i += 1;
        }
        total_read += chunk;
        if !ok { break; }
    }

    if total_read < 4096 || !ok {
        environment::log("FAIL: multi-block pattern mismatch");
        return 1;
    }
    file::close(handle);
    environment::log("ext2_write_test: Test 3 passed");

    // =========================================================================
    // Test 4: Partial block write preserves existing data (nested.txt)
    // =========================================================================
    environment::log("ext2_write_test: Test 4 - Partial block write");
    let Ok(handle) = environment::open("file:/mnt/subdir/nested.txt", 0, 0) else {
        environment::log("FAIL: Could not open file:/mnt/subdir/nested.txt");
        return 1;
    };

    // Seek to offset 7 (middle of "Nested file content\n")
    seek_to(handle, 7);
    let patch = b"PATCHED";
    let n = file::write(handle, patch);
    if n != patch.len() as isize {
        environment::log("FAIL: partial write returned wrong count");
        return 1;
    }

    // Read back from start
    seek_to(handle, 0);
    let mut check = [0u8; 64];
    let n = file::read(handle, &mut check);
    if n <= 0 {
        environment::log("FAIL: Could not read after partial write");
        return 1;
    }
    let content = core::str::from_utf8(&check[..n as usize]).unwrap_or("");
    // "Nested " + "PATCHED" + "ontent\n" = "Nested PATCHEDontent\n"
    if !content.starts_with("Nested ") || !content.contains("PATCHED") {
        environment::log("FAIL: partial write content mismatch");
        return 1;
    }
    file::close(handle);
    environment::log("ext2_write_test: Test 4 passed");

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

    environment::log("ext2_write_test: All tests passed!");
    0
}
