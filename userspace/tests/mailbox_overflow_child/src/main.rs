//! Child process for the mailbox overflow test.
//!
//! Sends many messages rapidly to the parent to flood the mailbox event queue,
//! then exits cleanly.

#![no_std]
#![no_main]

use libpanda::{environment, ipc::Channel};

/// Number of messages to send (well above MAX_MAILBOX_EVENTS).
const FLOOD_COUNT: usize = 512;

libpanda::main! {
    environment::log("Overflow child: starting");

    let Some(parent) = Channel::parent() else {
        environment::log("Overflow child: no parent channel");
        return 1;
    };

    // Send many small messages rapidly. Each send triggers a
    // CHANNEL_READABLE event on the parent's mailbox. The mailbox
    // should coalesce these into a single pending entry.
    let msg = b"ping";
    for _ in 0..FLOOD_COUNT {
        // Use try_send to avoid blocking if the channel queue is full.
        // Channel queue is only 16 deep, so most sends will fail —
        // that's fine, we just need enough to trigger many events.
        if parent.try_send(msg).is_ok() {
            // sent successfully
        } else {
            // Channel full — yield and retry.
            libpanda::process::yield_now();
            let _ = parent.try_send(msg);
        }
    }

    environment::log("Overflow child: done sending");
    0
}
