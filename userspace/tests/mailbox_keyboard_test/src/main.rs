//! Test mailbox-based keyboard input (notification + non-blocking read pattern).
//!
//! This tests the same flow used by the terminal:
//! 1. Open keyboard with mailbox attachment
//! 2. Wait for KeyboardReady event on mailbox
//! 3. Read key data from keyboard handle using non-blocking read

#![no_std]
#![no_main]

use libpanda::environment;
use libpanda::file;
use libpanda::ipc::Channel;
use libpanda::keyboard::{RawInputEvent, keycode_to_char};
use libpanda::mailbox::{Event, InputEvent, Mailbox};
use panda_abi::EVENT_KEYBOARD_KEY;

const EV_KEY: u16 = 0x01;

fn format_key_msg<'a>(buf: &'a mut [u8], key: char, action: &str) -> &'a str {
    let prefix = b"Key '";
    let mid = b"' ";

    let mut pos = 0;
    for &b in prefix {
        buf[pos] = b;
        pos += 1;
    }
    buf[pos] = key as u8;
    pos += 1;
    for &b in mid {
        buf[pos] = b;
        pos += 1;
    }
    for &b in action.as_bytes() {
        buf[pos] = b;
        pos += 1;
    }

    core::str::from_utf8(&buf[..pos]).unwrap_or("Key event")
}

/// Read all available key events from keyboard using non-blocking reads
fn process_keyboard_events(keyboard: libpanda::Handle, events_read: &mut usize) {
    let mut event_buf = [0u8; 8];
    let mut msg_buf = [0u8; 32];

    loop {
        let n = file::try_read(keyboard, &mut event_buf);
        if n <= 0 {
            // No more events available
            break;
        }

        if n as usize >= core::mem::size_of::<RawInputEvent>() {
            let event = unsafe { &*(event_buf.as_ptr() as *const RawInputEvent) };
            if event.event_type == EV_KEY {
                let key_char = keycode_to_char(event.code, false).unwrap_or('?');

                let action = if event.value == 1 {
                    "pressed"
                } else if event.value == 0 {
                    "released"
                } else {
                    "repeat"
                };

                let msg = format_key_msg(&mut msg_buf, key_char, action);
                environment::log(msg);
                *events_read += 1;
            }
        }
    }
}

libpanda::main! {
    environment::log("Mailbox keyboard test starting");

    // This test plays the role of `init` — spawn the virtio-keyboard
    // driver directly and wait for its readiness signal before opening
    // `keyboard:`. See `userspace/tests/keyboard_test` for the identical
    // pattern and rationale.
    let Ok(driver_handle) = environment::spawn("file:/initrd/virtio-keyboard") else {
        environment::log("Could not spawn virtio-keyboard driver");
        return 1;
    };
    let Some(driver_channel) = Channel::from_handle_borrowed(driver_handle.into()) else {
        environment::log("Driver handle is not a channel");
        return 1;
    };
    let mut ready_msg = [0u8; 64];
    if driver_channel.recv(&mut ready_msg).is_err() {
        environment::log("Did not receive ready signal from virtio-keyboard driver");
        return 1;
    }

    // Get the default mailbox
    let mailbox = Mailbox::default();

    // Open keyboard with mailbox attachment for key events
    let keyboard = if let Ok(h) = environment::open(
        "keyboard:/pci/00:03.0",
        mailbox.handle().as_raw(),
        EVENT_KEYBOARD_KEY,
    ) {
        h
    } else {
        environment::log("Could not open keyboard with mailbox");
        return 1;
    };

    environment::log("Keyboard opened with mailbox attachment");
    environment::log("Waiting for KeyboardReady events...");

    let mut events_read = 0;

    // Event loop - wait for mailbox notifications, then read from keyboard
    while events_read < 10 {
        let (_handle, events) = mailbox.recv();

        for event in events {
            match event {
                Event::Input(InputEvent::Keyboard) => {
                    // Keyboard has events - read them with non-blocking reads
                    process_keyboard_events(keyboard, &mut events_read);
                }
                _ => {
                    // Unexpected event type
                    environment::log("Unexpected event type received");
                }
            }
        }
    }

    file::close(keyboard);

    // See `userspace/tests/keyboard_test`'s identical cleanup step: the
    // virtio-keyboard driver runs forever, so this test (which plays
    // `init`) must ask it to exit before returning, or QEMU never sees "no
    // processes remaining" and just runs until the harness's timeout.
    let _ = driver_channel.send(b"shutdown");

    environment::log("Mailbox keyboard test passed");
    0
}
