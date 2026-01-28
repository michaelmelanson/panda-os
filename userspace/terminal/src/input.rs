//! Input handling for the terminal.
//!
//! This module handles pending input requests from child processes and
//! keyboard event processing.

use alloc::string::String;
use libpanda::{
    channel, file,
    keyboard::{self, KeyValue, RawInputEvent, KEY_BACKSPACE, KEY_ENTER},
    Handle,
};
use panda_abi::terminal::{Event as TerminalEvent, InputKind, InputResponse, InputValue};

use crate::Terminal;

/// Pending input request state
pub struct PendingInput {
    /// Request ID for correlation
    pub id: u32,
    /// Type of input requested
    pub kind: InputKind,
    /// Handle to send response to
    pub handle: Handle,
    /// Buffer for line input
    pub buffer: String,
}

impl Terminal {
    /// Send input response to child
    pub fn send_input_response(&mut self, value: Option<InputValue>) {
        if let Some(pending) = self.pending_input.take() {
            let response = InputResponse {
                id: pending.id,
                value,
            };
            let msg = TerminalEvent::Input(response);
            let bytes = msg.to_bytes();
            let _ = channel::send(pending.handle, &bytes);
        }
    }

    /// Handle a typed character when there's a pending input request
    pub fn handle_input_char(&mut self, ch: char) {
        let Some(ref pending) = self.pending_input else {
            return;
        };

        let kind = pending.kind;

        match kind {
            InputKind::Char => {
                // Single character - send immediately
                let _ = self.draw_char(ch);
                self.flush();
                self.send_input_response(Some(InputValue::Char(ch)));
            }
            InputKind::Line | InputKind::Password => {
                if kind == InputKind::Password {
                    // Don't echo password characters, just show *
                    let _ = self.draw_char('*');
                } else {
                    let _ = self.draw_char(ch);
                }
                // Now we can mutate pending_input
                if let Some(ref mut pending) = self.pending_input {
                    pending.buffer.push(ch);
                }
                self.flush();
            }
            InputKind::Confirm => {
                // Accept y/Y for yes, n/N for no
                let result = match ch {
                    'y' | 'Y' => Some(true),
                    'n' | 'N' => Some(false),
                    _ => None,
                };
                if let Some(b) = result {
                    let _ = self.draw_char(ch);
                    self.newline();
                    self.flush();
                    self.send_input_response(Some(InputValue::Bool(b)));
                }
            }
            InputKind::Choice | InputKind::RawKeys => {
                // TODO: Handle these input types
            }
        }
    }

    /// Handle Enter key when there's a pending input request
    pub fn handle_input_enter(&mut self) {
        let Some(ref pending) = self.pending_input else {
            return;
        };

        let kind = pending.kind;
        let text = pending.buffer.clone();

        match kind {
            InputKind::Line | InputKind::Password => {
                self.newline();
                self.flush();
                self.send_input_response(Some(InputValue::Text(text)));
            }
            _ => {}
        }
    }

    /// Handle Backspace key when there's a pending input request
    pub fn handle_input_backspace(&mut self) {
        let Some(ref pending) = self.pending_input else {
            return;
        };

        let kind = pending.kind;
        let is_empty = pending.buffer.is_empty();

        match kind {
            InputKind::Line | InputKind::Password => {
                if !is_empty {
                    // Get the character being removed to measure its width
                    let removed_char = if let Some(ref mut pending) = self.pending_input {
                        pending.buffer.pop()
                    } else {
                        None
                    };
                    // For password mode, we displayed '*', so measure that instead
                    let display_char = if kind == InputKind::Password {
                        '*'
                    } else {
                        removed_char.unwrap_or(' ')
                    };
                    let char_width = self.measure_char(display_char);
                    self.backspace_width(char_width);
                    self.flush();
                }
            }
            _ => {}
        }
    }
}

/// Handle a key event
pub fn handle_key_event(term: &mut Terminal, code: u16, value: KeyValue, shift_pressed: &mut bool) {
    match value {
        KeyValue::Press | KeyValue::Repeat => {
            // Track shift state
            if keyboard::is_shift_key(code) {
                *shift_pressed = true;
                return;
            }

            // Handle special keys
            match code {
                KEY_ENTER => term.handle_enter(),
                KEY_BACKSPACE => term.handle_backspace(),
                _ => {
                    // Try to convert to character
                    if let Some(ch) = keyboard::keycode_to_char(code, *shift_pressed) {
                        // If there's pending input from child, route to that
                        if term.pending_input.is_some() {
                            term.handle_input_char(ch);
                        } else if term.child.is_none() {
                            // Only accept shell input when no child is running
                            term.handle_char(ch);
                        }
                    }
                }
            }
        }
        KeyValue::Release => {
            if keyboard::is_shift_key(code) {
                *shift_pressed = false;
            }
        }
    }
}

/// Process any pending keyboard events
pub fn process_keyboard_events(term: &mut Terminal, shift_pressed: &mut bool) {
    let mut buf = [0u8; 8]; // RawInputEvent is 8 bytes

    loop {
        let n = file::try_read(term.keyboard, &mut buf);
        if n <= 0 {
            break;
        }

        if n >= 8 {
            let event = unsafe { &*(buf.as_ptr() as *const RawInputEvent) };
            let value = KeyValue::from_u32(event.value);
            handle_key_event(term, event.code, value, shift_pressed);
        }
    }
}
