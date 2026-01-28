#![no_std]
#![no_main]

use libpanda::{Handle, environment, error::Error, file, ipc, process::ChildBuilder};

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
            if e != Error::NotFound {
                environment::log("FAIL: expected NotFound error");
                return 1;
            }
        }
    }
    environment::log("  invalid path spawn: OK");

    // Test 2: Channel operations on invalid handle
    environment::log("Test 2: Channel operations on invalid handle");

    let invalid_handle = Handle::from(0xDEAD);

    match ipc::try_send(invalid_handle, b"test") {
        Ok(_) => {
            environment::log("FAIL: send to invalid handle should fail");
            return 1;
        }
        Err(_) => {}
    }
    environment::log("  send to invalid handle: OK");

    let mut buf = [0u8; 64];
    match ipc::try_recv(invalid_handle, &mut buf) {
        Ok(_) => {
            environment::log("FAIL: recv from invalid handle should fail");
            return 1;
        }
        Err(_) => {}
    }
    environment::log("  recv from invalid handle: OK");

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
        Err(_) => {}
    }
    environment::log("  send to closed peer: OK");

    environment::log("PASS");
    0
}
