//! Mailbox overflow test: verifies that flooding a mailbox with events
//! does not cause unbounded queue growth.
//!
//! Spawns a child that sends many messages, producing many CHANNEL_READABLE
//! events. After the child exits, drains the mailbox and verifies that:
//! - Events are received without kernel panic or hang.
//! - The child exits cleanly.

#![no_std]
#![no_main]

use libpanda::{
    environment,
    ipc::{Channel, ChannelEvent, Event, Mailbox},
    process,
    process::ChildBuilder,
};
use panda_abi::{EVENT_CHANNEL_CLOSED, EVENT_CHANNEL_READABLE};

libpanda::main! {
    environment::log("Mailbox overflow test: starting");

    let mailbox = Mailbox::default();

    let Ok(child_handle) = ChildBuilder::new("file:/initrd/mailbox_overflow_child")
        .mailbox(mailbox.handle(), EVENT_CHANNEL_READABLE | EVENT_CHANNEL_CLOSED)
        .spawn_handle()
    else {
        environment::log("FAIL: spawn failed");
        return 1;
    };

    let Some(channel) = Channel::from_handle_borrowed(child_handle.into()) else {
        environment::log("FAIL: handle is not a channel");
        return 1;
    };

    environment::log("Mailbox overflow test: child spawned, draining events...");

    let mut got_closed = false;
    let mut messages_received = 0usize;
    let mut iterations = 0;
    const MAX_ITERATIONS: i32 = 1000;

    while iterations < MAX_ITERATIONS && !got_closed {
        iterations += 1;

        let (_handle, events) = mailbox.recv();

        for event in events {
            match event {
                Event::Channel(ChannelEvent::Readable) => {
                    // Drain all available messages from the channel.
                    let mut buf = [0u8; 64];
                    while let Ok(Some(len)) = channel.try_recv(&mut buf) {
                        if len > 0 {
                            messages_received += 1;
                        }
                    }
                }
                Event::Channel(ChannelEvent::Closed) => {
                    // Drain any remaining messages after close.
                    let mut buf = [0u8; 64];
                    while let Ok(Some(len)) = channel.try_recv(&mut buf) {
                        if len > 0 {
                            messages_received += 1;
                        }
                    }
                    got_closed = true;
                }
                _ => {}
            }
        }
    }

    if !got_closed {
        environment::log("FAIL: never got ChannelClosed event");
        return 1;
    }

    // Verify child exit code.
    let exit_code = process::wait(child_handle);
    if exit_code != 0 {
        environment::log("FAIL: child exited with non-zero code");
        return 1;
    }

    // We should have received at least some messages (channel queue is 16 deep,
    // child sends up to 512). The exact count depends on scheduling, but we must
    // get at least 1 to confirm the pipeline worked.
    if messages_received == 0 {
        environment::log("FAIL: no messages received");
        return 1;
    }

    environment::log("Mailbox overflow test: all events drained successfully");
    environment::log("PASS");
    0
}
