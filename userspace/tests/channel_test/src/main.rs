#![no_std]
#![no_main]

use libpanda::{environment, ipc::Channel, process};

libpanda::main! {
    environment::log("Channel test: starting");

    // Spawn a child process - this creates a channel between parent and child
    let Ok(child_handle) = environment::spawn("file:/initrd/channel_child", &[], 0, 0) else {
        environment::log("FAIL: spawn returned error");
        return 1;
    };

    // Wrap the child handle in a Channel (borrowed - we still need the handle for wait)
    let channel = Channel::from_handle_borrowed(child_handle.into());

    environment::log("Channel test: child spawned, sending ping...");

    // Send a message to the child
    if channel.send(b"ping").is_err() {
        environment::log("FAIL: send failed");
        return 1;
    }

    environment::log("Channel test: ping sent, waiting for pong...");

    // Receive response from child
    let mut buf = [0u8; 64];
    match channel.recv(&mut buf) {
        Ok(len) => {
            if len == 4 && &buf[..4] == b"pong" {
                environment::log("Channel test: received pong!");
            } else {
                environment::log("FAIL: unexpected response");
                return 1;
            }
        }
        Err(_) => {
            environment::log("FAIL: recv failed");
            return 1;
        }
    }

    // Wait for child to exit
    let exit_code = process::wait(child_handle);
    if exit_code != 0 {
        environment::log("FAIL: child exited with non-zero code");
        return 1;
    }

    environment::log("Channel test: child exited successfully");
    environment::log("PASS");
    0
}
