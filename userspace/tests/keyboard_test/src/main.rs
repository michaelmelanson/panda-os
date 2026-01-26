#![no_std]
#![no_main]

use libpanda::environment;
use libpanda::file;
use libpanda::keyboard::{RawInputEvent, keycode_to_char};

const EV_KEY: u16 = 0x01;

libpanda::main! {
    environment::log("Keyboard test starting");

    // Open the keyboard device at PCI 00:03.0
    let keyboard = if let Ok(h) = environment::open("keyboard:/pci/00:03.0", 0, 0) {
        h
    } else {
        environment::log("Could not open keyboard");
        return 1;
    };

    environment::log("Keyboard opened successfully!");
    environment::log("Waiting for key events (this will block)...");
    environment::log("Press keys to see events. Test will read 10 events then exit.");

    let mut event_buf = [0u8; 8]; // sizeof(RawInputEvent) = 8 bytes
    let mut events_read = 0;
    let mut msg_buf = [0u8; 32];

    while events_read < 10 {
        let n = file::read(keyboard, &mut event_buf);
        if n < 0 {
            environment::log("Error reading from keyboard");
            return 1;
        }
        if n as usize >= core::mem::size_of::<RawInputEvent>() {
            let event = unsafe { &*(event_buf.as_ptr() as *const RawInputEvent) };
            if event.event_type == EV_KEY {
                let key_char = keycode_to_char(event.code, false).unwrap_or('?');

                // Format message with key character
                let action = if event.value == 1 {
                    "pressed"
                } else if event.value == 0 {
                    "released"
                } else {
                    "repeat"
                };

                // Build message: "Key 'x' pressed" or similar
                let msg = format_key_msg(&mut msg_buf, key_char, action);
                environment::log(msg);
                events_read += 1;
            }
        }
    }

    file::close(keyboard);
    environment::log("Keyboard test passed - received 10 key events");
    0
}

fn format_key_msg<'a>(buf: &'a mut [u8], key: char, action: &str) -> &'a str {
    // "Key 'x' pressed"
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
