#![no_std]
#![no_main]

use libpanda::{channel, environment, Handle};

libpanda::main! {
    environment::log("Mailbox child: starting");

    // Send a message to parent - this should trigger a ChannelReadable event
    environment::log("Mailbox child: sending message to parent...");
    if let Err(_) = channel::send(Handle::PARENT, b"hello from child") {
        environment::log("Mailbox child: send failed");
        return 1;
    }

    environment::log("Mailbox child: message sent, exiting");
    // Exit - this should trigger a ProcessExited event
    0
}
