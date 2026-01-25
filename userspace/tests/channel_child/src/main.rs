#![no_std]
#![no_main]

use libpanda::{channel, environment, Handle};

libpanda::main! {
    environment::log("Channel child: starting, waiting for message...");

    // Receive message from parent via HANDLE_PARENT
    let mut buf = [0u8; 64];
    match channel::recv(Handle::PARENT, &mut buf) {
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
    if let Err(_) = channel::send(Handle::PARENT, b"pong") {
        environment::log("Channel child: send failed");
        return 1;
    }

    environment::log("Channel child: done, exiting");
    0
}
