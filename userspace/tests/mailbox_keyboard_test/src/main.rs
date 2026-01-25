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
use libpanda::mailbox::{Event, Mailbox};
use panda_abi::EVENT_KEYBOARD_KEY;

/// Input event structure (matches kernel's InputEvent)
#[repr(C)]
struct InputEvent {
    event_type: u16,
    code: u16,
    value: u32,
}

const EV_KEY: u16 = 0x01;

/// Convert Linux keycode to ASCII character (simplified, lowercase only)
fn keycode_to_char(code: u16) -> Option<char> {
    match code {
        30 => Some('a'),
        48 => Some('b'),
        46 => Some('c'),
        32 => Some('d'),
        18 => Some('e'),
        33 => Some('f'),
        34 => Some('g'),
        35 => Some('h'),
        23 => Some('i'),
        36 => Some('j'),
        37 => Some('k'),
        38 => Some('l'),
        50 => Some('m'),
        49 => Some('n'),
        24 => Some('o'),
        25 => Some('p'),
        16 => Some('q'),
        19 => Some('r'),
        31 => Some('s'),
        20 => Some('t'),
        22 => Some('u'),
        47 => Some('v'),
        17 => Some('w'),
        45 => Some('x'),
        21 => Some('y'),
        44 => Some('z'),
        _ => None,
    }
}

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

        if n as usize >= core::mem::size_of::<InputEvent>() {
            let event = unsafe { &*(event_buf.as_ptr() as *const InputEvent) };
            if event.event_type == EV_KEY {
                let key_char = keycode_to_char(event.code).unwrap_or('?');

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
        let (_handle, event) = mailbox.recv();

        match event {
            Event::KeyboardReady => {
                // Keyboard has events - read them with non-blocking reads
                process_keyboard_events(keyboard, &mut events_read);
            }
            _ => {
                // Unexpected event type
                environment::log("Unexpected event type received");
            }
        }
    }

    file::close(keyboard);
    environment::log("Mailbox keyboard test passed");
    0
}
