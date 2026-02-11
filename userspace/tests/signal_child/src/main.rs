//! Signal child - a helper process that handles signals gracefully.
//!
//! This process loops until it receives a SIGTERM signal on HANDLE_PARENT,
//! then exits gracefully with code 0.

#![no_std]
#![no_main]

use libpanda::{environment, mailbox::Mailbox};
use panda_abi::{SignalMessage, Signal, SIGNAL_MESSAGE_SIZE, HANDLE_PARENT};

libpanda::main! {
    environment::log("Signal child: starting event loop");

    // Get the default mailbox
    let mailbox = Mailbox::default();
    let parent_handle = libpanda::handle::Handle::from(HANDLE_PARENT);

    // Loop until we receive SIGTERM
    loop {
        // Poll the mailbox for events (non-blocking with timeout via yield)
        if let Some((handle, events)) = mailbox.try_recv() {
            // Check if this is a signal on the parent channel
            if handle == parent_handle && events.is_signal_received() {
                environment::log("Signal child: received signal event");

                // Try to receive the signal message (non-blocking)
                let mut buf = [0u8; SIGNAL_MESSAGE_SIZE];
                let result = libpanda::sys::channel::try_recv_msg(
                    parent_handle,
                    &mut buf
                );

                if result >= SIGNAL_MESSAGE_SIZE as isize {
                    let len = result as usize;
                    // Decode the signal message
                    match SignalMessage::decode(&buf[..len]) {
                        Ok(Some(msg)) => {
                            environment::log("Signal child: decoded signal message");
                            match msg.signal {
                                Signal::Terminate => {
                                    environment::log("Signal child: SIGTERM received, exiting gracefully");
                                    return 0; // Clean exit
                                }
                                Signal::Kill => {
                                    // We shouldn't receive SIGKILL as a message
                                    // (it's handled by the kernel immediately)
                                    environment::log("Signal child: unexpected SIGKILL message");
                                }
                            }
                        }
                        Ok(None) => {
                            environment::log("Signal child: not a signal message");
                        }
                        Err(_) => {
                            environment::log("Signal child: failed to decode signal");
                        }
                    }
                } else if result > 0 {
                    environment::log("Signal child: message too short");
                }
                // Negative result means error (e.g., queue empty), which is fine
            }

            // Check for channel close (parent died)
            if handle == parent_handle && events.is_channel_closed() {
                environment::log("Signal child: parent closed channel, exiting");
                return 1;
            }
        }

        // Yield to allow other processes to run
        libpanda::process::yield_now();
    }
}
