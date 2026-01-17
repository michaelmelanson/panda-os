#![no_std]
#![no_main]

use libpanda::{environment, file};

/// Input event structure (matches kernel's InputEvent)
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct InputEvent {
    event_type: u16,
    code: u16,
    value: u32,
}

const EV_KEY: u16 = 0x01;

/// Convert Linux keycode to ASCII character
fn keycode_to_char(code: u16, shift: bool) -> Option<char> {
    let c = match code {
        // Letters
        30 => 'a', 48 => 'b', 46 => 'c', 32 => 'd', 18 => 'e',
        33 => 'f', 34 => 'g', 35 => 'h', 23 => 'i', 36 => 'j',
        37 => 'k', 38 => 'l', 50 => 'm', 49 => 'n', 24 => 'o',
        25 => 'p', 16 => 'q', 19 => 'r', 31 => 's', 20 => 't',
        22 => 'u', 47 => 'v', 17 => 'w', 45 => 'x', 21 => 'y',
        44 => 'z',
        // Numbers
        2 => '1', 3 => '2', 4 => '3', 5 => '4', 6 => '5',
        7 => '6', 8 => '7', 9 => '8', 10 => '9', 11 => '0',
        // Special characters
        57 => ' ',   // space
        28 => '\n',  // enter
        14 => '\x08', // backspace
        15 => '\t',  // tab
        12 => '-',
        13 => '=',
        26 => '[',
        27 => ']',
        43 => '\\',
        39 => ';',
        40 => '\'',
        41 => '`',
        51 => ',',
        52 => '.',
        53 => '/',
        _ => return None,
    };

    if shift && c.is_ascii_lowercase() {
        Some(c.to_ascii_uppercase())
    } else {
        Some(c)
    }
}

/// Key codes for modifier keys
const KEY_LEFTSHIFT: u16 = 42;
const KEY_RIGHTSHIFT: u16 = 54;

libpanda::main! {
    // Open the keyboard device
    let Ok(keyboard) = environment::open("keyboard:/pci/00:03.0", 0) else {
        environment::log("shell: failed to open keyboard");
        return 1;
    };

    // Open console for output
    let Ok(console) = environment::open("console:/serial/0", 0) else {
        environment::log("shell: failed to open console");
        return 1;
    };

    // Print prompt
    let prompt = b"\n> ";
    file::write(console, prompt);

    let mut event = InputEvent::default();
    let mut shift_held = false;

    loop {
        // Read directly into the aligned struct
        let event_bytes = unsafe {
            core::slice::from_raw_parts_mut(
                &mut event as *mut InputEvent as *mut u8,
                core::mem::size_of::<InputEvent>(),
            )
        };
        let n = file::read(keyboard, event_bytes);
        if n < 0 {
            environment::log("shell: keyboard read error");
            return 1;
        }

        if n as usize >= core::mem::size_of::<InputEvent>() {
            if event.event_type == EV_KEY {
                // Track shift key state
                if event.code == KEY_LEFTSHIFT || event.code == KEY_RIGHTSHIFT {
                    shift_held = event.value != 0;
                    continue;
                }

                // Only handle key press events (value == 1)
                if event.value == 1 {
                    if let Some(c) = keycode_to_char(event.code, shift_held) {
                        // Echo the character
                        let char_buf = [c as u8];
                        file::write(console, &char_buf);

                        // Print new prompt after enter
                        if c == '\n' {
                            file::write(console, b"> ");
                        }
                    }
                }
            }
        }
    }
}
