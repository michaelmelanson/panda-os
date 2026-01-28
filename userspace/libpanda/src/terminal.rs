//! Terminal IPC module for userspace programs.
//!
//! Provides a high-level API for communicating with the terminal emulator
//! using the structured message-passing protocol.
//!
//! # Example
//!
//! ```no_run
//! use libpanda::terminal::{self, Colour, NamedColour, TerminalStyle};
//!
//! // Simple output
//! terminal::println("Hello, world!");
//!
//! // Styled output
//! let red = TerminalStyle::fg(Colour::Named(NamedColour::Red));
//! terminal::print_styled("Error: ", red);
//! terminal::println("file not found");
//!
//! // Input
//! if let Some(name) = terminal::input("What is your name? ") {
//!     terminal::println(&libpanda::format!("Hello, {}!", name));
//! }
//! ```

use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec::Vec;

use panda_abi::terminal::{
    ClearRegion, Event, InputKind, InputRequest, InputResponse, InputValue, QueryResponse, Request,
    Style, TerminalQuery,
};
use panda_abi::value::{Table, Value};
use panda_abi::{HANDLE_PARENT, MAX_MESSAGE_SIZE};

use crate::Handle;
use crate::channel;

// Re-export commonly used types
pub use panda_abi::terminal::{
    Alignment, Colour, ColourSupport, NamedColour, Style as TerminalStyle, TerminalCapabilities,
};

// =============================================================================
// Output functions (send Value to terminal via PARENT)
// =============================================================================

/// Print plain text (no newline).
pub fn print(s: &str) {
    send_value(Value::String(String::from(s)));
}

/// Print plain text with newline.
pub fn println(s: &str) {
    let mut text = String::from(s);
    text.push('\n');
    send_value(Value::String(text));
}

/// Print styled text.
pub fn print_styled(s: &str, style: Style) {
    send_value(Value::Styled(
        style,
        Box::new(Value::String(String::from(s))),
    ));
}

/// Print a Value directly.
pub fn print_value(value: Value) {
    send_value(value);
}

/// Display a table.
pub fn print_table(table: Table) {
    send_value(Value::Table(table));
}

/// Print a hyperlink.
pub fn print_link(text: &str, url: &str) {
    send_value(Value::Link {
        url: String::from(url),
        inner: Box::new(Value::String(String::from(text))),
    });
}

// =============================================================================
// Control plane functions (UI control, always via PARENT)
// =============================================================================

/// Clear a region of the terminal.
pub fn clear(region: ClearRegion) {
    send_request(Request::Clear(region));
}

/// Clear the entire screen.
pub fn clear_screen() {
    clear(ClearRegion::Screen);
}

/// Move the cursor to a position.
pub fn move_cursor(row: u16, col: u16) {
    send_request(Request::MoveCursor { row, col });
}

/// Set the window title.
pub fn set_title(title: &str) {
    send_request(Request::SetTitle(String::from(title)));
}

/// Report progress.
pub fn progress(current: u32, total: u32, message: &str) {
    send_request(Request::Progress {
        current,
        total,
        message: String::from(message),
    });
}

// =============================================================================
// Error/Warning functions (control plane, always reach terminal)
// =============================================================================

/// Report an error via the control plane.
///
/// This message is sent directly to the terminal (via PARENT channel) and
/// is always displayed, even if this process is in the middle of a pipeline.
/// Use this for errors that must reach the user.
pub fn error(message: &str) {
    send_request(Request::Error(Value::String(String::from(message))));
}

/// Report a warning via the control plane.
///
/// This message is sent directly to the terminal (via PARENT channel) and
/// is always displayed, even if this process is in the middle of a pipeline.
/// Use this for warnings that must reach the user.
pub fn warning(message: &str) {
    send_request(Request::Warning(Value::String(String::from(message))));
}

/// Report an error with a structured Value via the control plane.
pub fn error_value(value: Value) {
    send_request(Request::Error(value));
}

/// Report a warning with a structured Value via the control plane.
pub fn warning_value(value: Value) {
    send_request(Request::Warning(value));
}

// =============================================================================
// Input functions
// =============================================================================

/// Request ID counter for input requests.
static mut NEXT_REQUEST_ID: u32 = 1;

fn next_request_id() -> u32 {
    // Safety: Single-threaded userspace
    unsafe {
        let id = NEXT_REQUEST_ID;
        NEXT_REQUEST_ID = NEXT_REQUEST_ID.wrapping_add(1);
        if NEXT_REQUEST_ID == 0 {
            NEXT_REQUEST_ID = 1;
        }
        id
    }
}

/// Read a line of input (no prompt).
pub fn read_line() -> Option<String> {
    input_with_kind(None, InputKind::Line).and_then(|v| {
        if let InputValue::Text(s) = v {
            Some(s)
        } else {
            None
        }
    })
}

/// Read a line of input with a prompt.
pub fn input(prompt: &str) -> Option<String> {
    input_with_kind(Some(Value::String(String::from(prompt))), InputKind::Line).and_then(|v| {
        if let InputValue::Text(s) = v {
            Some(s)
        } else {
            None
        }
    })
}

/// Read a line of input with a styled prompt.
pub fn input_styled(prompt: Value) -> Option<String> {
    input_with_kind(Some(prompt), InputKind::Line).and_then(|v| {
        if let InputValue::Text(s) = v {
            Some(s)
        } else {
            None
        }
    })
}

/// Read a password (input not echoed).
pub fn password(prompt: &str) -> Option<String> {
    input_with_kind(
        Some(Value::String(String::from(prompt))),
        InputKind::Password,
    )
    .and_then(|v| {
        if let InputValue::Text(s) = v {
            Some(s)
        } else {
            None
        }
    })
}

/// Ask a yes/no confirmation question.
pub fn confirm(prompt: &str) -> bool {
    input_with_kind(
        Some(Value::String(String::from(prompt))),
        InputKind::Confirm,
    )
    .map(|v| {
        if let InputValue::Bool(b) = v {
            b
        } else {
            false
        }
    })
    .unwrap_or(false)
}

/// Read a single character.
pub fn read_char() -> Option<char> {
    input_with_kind(None, InputKind::Char).and_then(|v| {
        if let InputValue::Char(c) = v {
            Some(c)
        } else {
            None
        }
    })
}

/// Present a choice and get the selected index.
pub fn choose(prompt: &str, choices: &[&str]) -> Option<usize> {
    let id = next_request_id();
    let req = InputRequest {
        id,
        kind: InputKind::Choice,
        prompt: Some(Value::String(String::from(prompt))),
        choices: choices.iter().map(|s| String::from(*s)).collect(),
    };

    send_request(Request::RequestInput(req));

    // Wait for response
    wait_for_input_response(id).and_then(|v| {
        if let InputValue::Choice(i) = v {
            Some(i)
        } else {
            None
        }
    })
}

fn input_with_kind(prompt: Option<Value>, kind: InputKind) -> Option<InputValue> {
    let id = next_request_id();
    let req = InputRequest {
        id,
        kind,
        prompt,
        choices: Vec::new(),
    };

    send_request(Request::RequestInput(req));

    // Wait for response
    wait_for_input_response(id)
}

fn wait_for_input_response(expected_id: u32) -> Option<InputValue> {
    let parent = unsafe { Handle::from_raw(HANDLE_PARENT) };
    let mut buf = [0u8; MAX_MESSAGE_SIZE];

    loop {
        match channel::recv(parent, &mut buf) {
            Ok(len) => {
                if let Ok((msg, _)) = Event::from_bytes(&buf[..len]) {
                    match msg {
                        Event::Input(InputResponse { id, value }) if id == expected_id => {
                            return value;
                        }
                        // Ignore other messages while waiting
                        _ => {}
                    }
                }
            }
            Err(_) => return None,
        }
    }
}

// =============================================================================
// Query functions
// =============================================================================

/// Query the terminal size.
pub fn size() -> Option<(u16, u16)> {
    send_request(Request::Query(TerminalQuery::Size));

    let parent = unsafe { Handle::from_raw(HANDLE_PARENT) };
    let mut buf = [0u8; MAX_MESSAGE_SIZE];

    loop {
        match channel::recv(parent, &mut buf) {
            Ok(len) => {
                if let Ok((msg, _)) = Event::from_bytes(&buf[..len]) {
                    if let Event::QueryResponse(QueryResponse::Size { cols, rows }) = msg {
                        return Some((cols, rows));
                    }
                }
            }
            Err(_) => return None,
        }
    }
}

/// Query the terminal capabilities.
pub fn capabilities() -> Option<TerminalCapabilities> {
    send_request(Request::Query(TerminalQuery::Capabilities));

    let parent = unsafe { Handle::from_raw(HANDLE_PARENT) };
    let mut buf = [0u8; MAX_MESSAGE_SIZE];

    loop {
        match channel::recv(parent, &mut buf) {
            Ok(len) => {
                if let Ok((msg, _)) = Event::from_bytes(&buf[..len]) {
                    if let Event::QueryResponse(QueryResponse::Capabilities(caps)) = msg {
                        return Some(caps);
                    }
                }
            }
            Err(_) => return None,
        }
    }
}

// =============================================================================
// Internal helpers
// =============================================================================

/// Send a Value to the terminal for display.
fn send_value(value: Value) {
    send_request(Request::Write(value));
}

/// Send a Request to the terminal.
fn send_request(msg: Request) {
    let bytes = msg.to_bytes();
    let parent = unsafe { Handle::from_raw(HANDLE_PARENT) };
    let _ = channel::send(parent, &bytes);
}

// =============================================================================
// Helper functions for building styled Values
// =============================================================================

/// Create a styled Value from text and style.
pub fn styled(text: &str, style: Style) -> Value {
    Value::Styled(style, Box::new(Value::String(String::from(text))))
}

/// Create a bold text Value.
pub fn bold(text: &str) -> Value {
    Value::Styled(Style::bold(), Box::new(Value::String(String::from(text))))
}

/// Create a coloured text Value.
pub fn coloured(text: &str, colour: Colour) -> Value {
    Value::Styled(
        Style::fg(colour),
        Box::new(Value::String(String::from(text))),
    )
}

/// Create an error-styled Value (red text).
pub fn error_text(text: &str) -> Value {
    coloured(text, Colour::Named(NamedColour::Red))
}

/// Create a warning-styled Value (yellow text).
pub fn warning_text(text: &str) -> Value {
    coloured(text, Colour::Named(NamedColour::Yellow))
}

/// Create a success-styled Value (green text).
pub fn success_text(text: &str) -> Value {
    coloured(text, Colour::Named(NamedColour::Green))
}

/// Create an info-styled Value (blue text).
pub fn info_text(text: &str) -> Value {
    coloured(text, Colour::Named(NamedColour::Blue))
}
