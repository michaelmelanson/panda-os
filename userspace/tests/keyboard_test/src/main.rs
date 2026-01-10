#![no_std]
#![no_main]

use libpanda::environment;
use libpanda::file;

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

libpanda::main! {
    environment::log("Keyboard test starting");

    // Open the keyboard device at PCI 00:03.0
    let keyboard = environment::open("keyboard:/pci/00:03.0", 0);
    if keyboard < 0 {
        environment::log("Could not open keyboard");
        return 1;
    }
    let keyboard = keyboard as u32;

    environment::log("Keyboard opened successfully!");
    environment::log("Waiting for key events (this will block)...");
    environment::log("Press keys to see events. Test will read 10 events then exit.");

    let mut event_buf = [0u8; 8]; // sizeof(InputEvent) = 8 bytes
    let mut events_read = 0;
    let mut msg_buf = [0u8; 32];

    while events_read < 10 {
        let n = file::read(keyboard, &mut event_buf);
        if n < 0 {
            environment::log("Error reading from keyboard");
            return 1;
        }
        if n as usize >= core::mem::size_of::<InputEvent>() {
            let event = unsafe { &*(event_buf.as_ptr() as *const InputEvent) };
            if event.event_type == EV_KEY {
                let key_char = keycode_to_char(event.code).unwrap_or('?');

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
