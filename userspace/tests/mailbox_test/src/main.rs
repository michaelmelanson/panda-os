#![no_std]
#![no_main]

use libpanda::{
    environment,
    ipc::{Channel, ChannelEvent, Event, Mailbox},
    process,
};
use panda_abi::{EVENT_CHANNEL_CLOSED, EVENT_CHANNEL_READABLE};

libpanda::main! {
    environment::log("Mailbox test: starting");

    // Get the default mailbox
    let mailbox = Mailbox::default();

    // Spawn a child with mailbox attachment for channel events
    // We listen for READABLE (message from child) and CLOSED (child exited and dropped channel)
    let Ok(child_handle) = environment::spawn(
        "file:/initrd/mailbox_child",
        &[],
        mailbox.handle().as_raw(),
        EVENT_CHANNEL_READABLE | EVENT_CHANNEL_CLOSED,
    ) else {
        environment::log("FAIL: spawn failed");
        return 1;
    };

    // Wrap the child handle in a Channel for receiving messages
    let channel = Channel::from_handle_borrowed(child_handle.into());

    environment::log("Mailbox test: child spawned, waiting for events...");

    // The child will send us a message, then exit (which closes the channel)
    let mut got_readable = false;
    let mut got_closed = false;
    let mut iterations = 0;
    const MAX_ITERATIONS: i32 = 10;

    while iterations < MAX_ITERATIONS && (!got_readable || !got_closed) {
        iterations += 1;

        let (handle, events) = mailbox.recv();

        if handle.as_raw() == child_handle.as_raw() {
            for event in events {
                match event {
                    Event::Channel(ChannelEvent::Readable) => {
                        environment::log("Mailbox test: got ChannelReadable event!");
                        got_readable = true;

                        // Read the message
                        let mut buf = [0u8; 64];
                        if let Ok(len) = channel.recv(&mut buf) {
                            if &buf[..len] == b"hello from child" {
                                environment::log("Mailbox test: message content correct");
                            } else {
                                environment::log("FAIL: unexpected message content");
                                return 1;
                            }
                        }
                    }
                    Event::Channel(ChannelEvent::Closed) => {
                        environment::log("Mailbox test: got ChannelClosed event!");
                        got_closed = true;
                    }
                    _ => {
                        environment::log("Mailbox test: got unexpected event type");
                    }
                }
            }
        } else {
            environment::log("Mailbox test: got event from unexpected handle");
        }
    }

    if !got_readable {
        environment::log("FAIL: never got ChannelReadable event");
        return 1;
    }

    if !got_closed {
        environment::log("FAIL: never got ChannelClosed event");
        return 1;
    }

    // Verify child exit code
    let exit_code = process::wait(child_handle);
    if exit_code != 0 {
        environment::log("FAIL: child exited with non-zero code");
        return 1;
    }

    environment::log("Mailbox test: all events received correctly");
    environment::log("PASS");
    0
}
