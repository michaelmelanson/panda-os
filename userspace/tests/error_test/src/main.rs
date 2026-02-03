#![no_std]
#![no_main]

use libpanda::{ErrorCode, Handle, environment, file, ipc, process::ChildBuilder};

libpanda::main! {
    environment::log("Error test: starting");

    // Test 1: Spawn failure with invalid path
    environment::log("Test 1: Spawn with invalid path");

    match ChildBuilder::new("file:/nonexistent/path").spawn() {
        Ok(_) => {
            environment::log("FAIL: spawn should fail for nonexistent path");
            return 1;
        }
        Err(e) => {
            if e != ErrorCode::NotFound {
                environment::log("FAIL: expected NotFound error");
                return 1;
            }
        }
    }
    environment::log("  invalid path spawn: OK");

    // Test 2: Channel operations on invalid handle
    environment::log("Test 2: Channel operations on invalid handle");

    let invalid_handle = Handle::from(0xDEADu64);

    match ipc::try_send(invalid_handle, b"test") {
        Ok(_) => {
            environment::log("FAIL: send to invalid handle should fail");
            return 1;
        }
        Err(e) => {
            if e != ErrorCode::InvalidHandle {
                environment::log("FAIL: expected InvalidHandle, got different error");
                return 1;
            }
        }
    }
    environment::log("  send to invalid handle: OK (InvalidHandle)");

    let mut buf = [0u8; 64];
    match ipc::try_recv(invalid_handle, &mut buf) {
        Ok(_) => {
            environment::log("FAIL: recv from invalid handle should fail");
            return 1;
        }
        Err(e) => {
            if e != ErrorCode::InvalidHandle {
                environment::log("FAIL: expected InvalidHandle, got different error");
                return 1;
            }
        }
    }
    environment::log("  recv from invalid handle: OK (InvalidHandle)");

    // Test 3: Channel operations after close
    environment::log("Test 3: Channel operations after close");

    let (a, b) = ipc::create_pair().expect("create_pair failed");

    // Close one end
    file::close(b.into());

    // Try to send - should fail because peer is closed
    match ipc::try_send(a.into(), b"hello") {
        Ok(_) => {
            environment::log("FAIL: send to closed channel should fail");
            return 1;
        }
        Err(e) => {
            if e != ErrorCode::ChannelClosed {
                environment::log("FAIL: expected ChannelClosed, got different error");
                return 1;
            }
        }
    }
    environment::log("  send to closed peer: OK (ChannelClosed)");

    // Test 4: Open non-existent file
    environment::log("Test 4: Open non-existent file");

    match environment::open("file:/nonexistent/file.txt", 0, 0) {
        Ok(h) => {
            file::close(h);
            environment::log("FAIL: open non-existent file should fail");
            return 1;
        }
        Err(e) => {
            if e != ErrorCode::NotFound {
                environment::log("FAIL: expected NotFound for missing file");
                return 1;
            }
        }
    }
    environment::log("  open missing file: OK (NotFound)");

    // Test 5: Open invalid scheme
    environment::log("Test 5: Open invalid scheme");

    match environment::open("badscheme:/foo", 0, 0) {
        Ok(h) => {
            file::close(h);
            environment::log("FAIL: open invalid scheme should fail");
            return 1;
        }
        Err(e) => {
            if e != ErrorCode::NotFound {
                environment::log("FAIL: expected NotFound for bad scheme");
                return 1;
            }
        }
    }
    environment::log("  open bad scheme: OK (NotFound)");

    // Test 6: ErrorCode round-trip via to_isize/from_isize
    environment::log("Test 6: ErrorCode round-trip");

    let codes: &[ErrorCode] = &[
        ErrorCode::Ok,
        ErrorCode::NotFound,
        ErrorCode::InvalidOffset,
        ErrorCode::NotReadable,
        ErrorCode::NotWritable,
        ErrorCode::NotSeekable,
        ErrorCode::NotSupported,
        ErrorCode::PermissionDenied,
        ErrorCode::IoError,
        ErrorCode::WouldBlock,
        ErrorCode::InvalidArgument,
        ErrorCode::Protocol,
        ErrorCode::InvalidHandle,
        ErrorCode::TooManyHandles,
        ErrorCode::ChannelClosed,
        ErrorCode::MessageTooLarge,
        ErrorCode::BufferTooSmall,
        ErrorCode::AlreadyExists,
        ErrorCode::NoSpace,
        ErrorCode::NotEmpty,
        ErrorCode::IsDirectory,
        ErrorCode::NotDirectory,
    ];

    for &code in codes {
        let as_isize = code.to_isize();
        match ErrorCode::from_isize(as_isize) {
            Some(back) => {
                if back != code {
                    environment::log("FAIL: round-trip mismatch");
                    return 1;
                }
            }
            None => {
                environment::log("FAIL: from_isize returned None");
                return 1;
            }
        }
    }
    environment::log("  all 22 variants round-trip: OK");

    // Verify unknown codes return None
    if ErrorCode::from_isize(-99).is_some() {
        environment::log("FAIL: from_isize(-99) should return None");
        return 1;
    }
    environment::log("  unknown code returns None: OK");

    environment::log("PASS");
    0
}
