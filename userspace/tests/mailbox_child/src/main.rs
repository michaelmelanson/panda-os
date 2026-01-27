#![no_std]
#![no_main]

use libpanda::{environment, ipc::Channel};

libpanda::main! {
    environment::log("Mailbox child: starting");

    // Get channel to parent
    let Some(parent) = Channel::parent() else {
        environment::log("Mailbox child: no parent channel");
        return 1;
    };

    // Send a message to parent - this should trigger a ChannelReadable event
    environment::log("Mailbox child: sending message to parent...");
    if parent.send(b"hello from child").is_err() {
        environment::log("Mailbox child: send failed");
        return 1;
    }

    environment::log("Mailbox child: message sent, exiting");
    // Exit - this should trigger a ChannelClosed event
    0
}
