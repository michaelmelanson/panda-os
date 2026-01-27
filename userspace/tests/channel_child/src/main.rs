#![no_std]
#![no_main]

use libpanda::{environment, ipc::Channel};

libpanda::main! {
    environment::log("Channel child: starting, waiting for message...");

    // Get channel to parent process
    let Some(parent) = Channel::parent() else {
        environment::log("Channel child: no parent channel");
        return 1;
    };

    // Receive message from parent
    let mut buf = [0u8; 64];
    match parent.recv(&mut buf) {
        Ok(len) => {
            if len == 4 && &buf[..4] == b"ping" {
                environment::log("Channel child: received ping!");
            } else {
                environment::log("Channel child: unexpected message");
                return 1;
            }
        }
        Err(_) => {
            environment::log("Channel child: recv failed");
            return 1;
        }
    }

    // Send response back to parent
    environment::log("Channel child: sending pong...");
    if parent.send(b"pong").is_err() {
        environment::log("Channel child: send failed");
        return 1;
    }

    environment::log("Channel child: done, exiting");
    0
}
