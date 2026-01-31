//! Keyboard input handling utilities.
//!
//! Provides key code constants and character conversion for Linux input events.
//! The virtio keyboard device emits events using the Linux evdev protocol, so
//! all scan codes here correspond to the `KEY_*` constants defined in the Linux
//! kernel header `linux/input-event-codes.h`.

/// Raw keyboard input event structure (matches kernel's input event layout).
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct RawInputEvent {
    pub event_type: u16,
    pub code: u16,
    pub value: u32,
}

/// Key event value indicating press, release, or repeat.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyValue {
    Release,
    Press,
    Repeat,
}

impl KeyValue {
    /// Convert from raw u32 value.
    pub fn from_u32(value: u32) -> Self {
        match value {
            0 => KeyValue::Release,
            1 => KeyValue::Press,
            2 => KeyValue::Repeat,
            _ => KeyValue::Release,
        }
    }
}

// Linux evdev key codes (from linux/input-event-codes.h).
// These values match the KEY_* constants from the Linux kernel's
// input subsystem. The virtio-input keyboard device uses this same
// encoding, so we reuse the standard codes directly.
// Reference: https://github.com/torvalds/linux/blob/master/include/uapi/linux/input-event-codes.h
pub const KEY_RESERVED: u16 = 0;
pub const KEY_ESC: u16 = 1;
pub const KEY_1: u16 = 2;
pub const KEY_2: u16 = 3;
pub const KEY_3: u16 = 4;
pub const KEY_4: u16 = 5;
pub const KEY_5: u16 = 6;
pub const KEY_6: u16 = 7;
pub const KEY_7: u16 = 8;
pub const KEY_8: u16 = 9;
pub const KEY_9: u16 = 10;
pub const KEY_0: u16 = 11;
pub const KEY_MINUS: u16 = 12;
pub const KEY_EQUAL: u16 = 13;
pub const KEY_BACKSPACE: u16 = 14;
pub const KEY_TAB: u16 = 15;
pub const KEY_Q: u16 = 16;
pub const KEY_W: u16 = 17;
pub const KEY_E: u16 = 18;
pub const KEY_R: u16 = 19;
pub const KEY_T: u16 = 20;
pub const KEY_Y: u16 = 21;
pub const KEY_U: u16 = 22;
pub const KEY_I: u16 = 23;
pub const KEY_O: u16 = 24;
pub const KEY_P: u16 = 25;
pub const KEY_LEFTBRACE: u16 = 26;
pub const KEY_RIGHTBRACE: u16 = 27;
pub const KEY_ENTER: u16 = 28;
pub const KEY_LEFTCTRL: u16 = 29;
pub const KEY_A: u16 = 30;
pub const KEY_S: u16 = 31;
pub const KEY_D: u16 = 32;
pub const KEY_F: u16 = 33;
pub const KEY_G: u16 = 34;
pub const KEY_H: u16 = 35;
pub const KEY_I_: u16 = 23; // Alias
pub const KEY_J: u16 = 36;
pub const KEY_K: u16 = 37;
pub const KEY_L: u16 = 38;
pub const KEY_SEMICOLON: u16 = 39;
pub const KEY_APOSTROPHE: u16 = 40;
pub const KEY_GRAVE: u16 = 41;
pub const KEY_LEFTSHIFT: u16 = 42;
pub const KEY_BACKSLASH: u16 = 43;
pub const KEY_Z: u16 = 44;
pub const KEY_X: u16 = 45;
pub const KEY_C: u16 = 46;
pub const KEY_V: u16 = 47;
pub const KEY_B: u16 = 48;
pub const KEY_N: u16 = 49;
pub const KEY_M: u16 = 50;
pub const KEY_COMMA: u16 = 51;
pub const KEY_DOT: u16 = 52;
pub const KEY_SLASH: u16 = 53;
pub const KEY_RIGHTSHIFT: u16 = 54;
pub const KEY_KPASTERISK: u16 = 55;
pub const KEY_LEFTALT: u16 = 56;
pub const KEY_SPACE: u16 = 57;
pub const KEY_CAPSLOCK: u16 = 58;

/// Convert a key code to a character, with optional shift modifier.
///
/// Returns `None` for keys that don't produce printable characters
/// (like Enter, Backspace, Shift, etc.).
pub fn keycode_to_char(code: u16, shift: bool) -> Option<char> {
    match code {
        // Letters
        KEY_A => Some(if shift { 'A' } else { 'a' }),
        KEY_B => Some(if shift { 'B' } else { 'b' }),
        KEY_C => Some(if shift { 'C' } else { 'c' }),
        KEY_D => Some(if shift { 'D' } else { 'd' }),
        KEY_E => Some(if shift { 'E' } else { 'e' }),
        KEY_F => Some(if shift { 'F' } else { 'f' }),
        KEY_G => Some(if shift { 'G' } else { 'g' }),
        KEY_H => Some(if shift { 'H' } else { 'h' }),
        KEY_I => Some(if shift { 'I' } else { 'i' }),
        KEY_J => Some(if shift { 'J' } else { 'j' }),
        KEY_K => Some(if shift { 'K' } else { 'k' }),
        KEY_L => Some(if shift { 'L' } else { 'l' }),
        KEY_M => Some(if shift { 'M' } else { 'm' }),
        KEY_N => Some(if shift { 'N' } else { 'n' }),
        KEY_O => Some(if shift { 'O' } else { 'o' }),
        KEY_P => Some(if shift { 'P' } else { 'p' }),
        KEY_Q => Some(if shift { 'Q' } else { 'q' }),
        KEY_R => Some(if shift { 'R' } else { 'r' }),
        KEY_S => Some(if shift { 'S' } else { 's' }),
        KEY_T => Some(if shift { 'T' } else { 't' }),
        KEY_U => Some(if shift { 'U' } else { 'u' }),
        KEY_V => Some(if shift { 'V' } else { 'v' }),
        KEY_W => Some(if shift { 'W' } else { 'w' }),
        KEY_X => Some(if shift { 'X' } else { 'x' }),
        KEY_Y => Some(if shift { 'Y' } else { 'y' }),
        KEY_Z => Some(if shift { 'Z' } else { 'z' }),

        // Number row (top row)
        KEY_1 => Some(if shift { '!' } else { '1' }),
        KEY_2 => Some(if shift { '@' } else { '2' }),
        KEY_3 => Some(if shift { '#' } else { '3' }),
        KEY_4 => Some(if shift { '$' } else { '4' }),
        KEY_5 => Some(if shift { '%' } else { '5' }),
        KEY_6 => Some(if shift { '^' } else { '6' }),
        KEY_7 => Some(if shift { '&' } else { '7' }),
        KEY_8 => Some(if shift { '*' } else { '8' }),
        KEY_9 => Some(if shift { '(' } else { '9' }),
        KEY_0 => Some(if shift { ')' } else { '0' }),

        // Symbols
        KEY_SPACE => Some(' '),
        KEY_MINUS => Some(if shift { '_' } else { '-' }),
        KEY_EQUAL => Some(if shift { '+' } else { '=' }),
        KEY_LEFTBRACE => Some(if shift { '{' } else { '[' }),
        KEY_RIGHTBRACE => Some(if shift { '}' } else { ']' }),
        KEY_SEMICOLON => Some(if shift { ':' } else { ';' }),
        KEY_APOSTROPHE => Some(if shift { '"' } else { '\'' }),
        KEY_GRAVE => Some(if shift { '~' } else { '`' }),
        KEY_BACKSLASH => Some(if shift { '|' } else { '\\' }),
        KEY_COMMA => Some(if shift { '<' } else { ',' }),
        KEY_DOT => Some(if shift { '>' } else { '.' }),
        KEY_SLASH => Some(if shift { '?' } else { '/' }),

        _ => None,
    }
}

/// Check if a key code is a shift key.
pub fn is_shift_key(code: u16) -> bool {
    code == KEY_LEFTSHIFT || code == KEY_RIGHTSHIFT
}
