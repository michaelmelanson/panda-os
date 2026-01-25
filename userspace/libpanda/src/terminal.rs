//! Terminal IPC module for userspace programs.
//!
//! Provides a high-level API for communicating with the terminal emulator
//! using the structured message-passing protocol.
//!
//! # Example
//!
//! ```
//! use libpanda::terminal::{self, StyledTextExt, TableExt};
//!
//! // Simple output
//! terminal::println("Hello, world!");
//!
//! // Styled output
//! let mut text = terminal::StyledText::new();
//! text.push_error("Error: ");
//! text.push_plain("file not found");
//! terminal::print_styled(text);
//!
//! // Input
//! if let Some(name) = terminal::input("What is your name? ") {
//!     terminal::println(&format!("Hello, {}!", name));
//! }
//! ```

use alloc::string::String;
use alloc::vec::Vec;

use panda_abi::terminal::{
    InputKind, InputRequest, InputResponse, InputValue, Output, QueryResponse, TerminalInput,
    TerminalOutput, TerminalQuery,
};
use panda_abi::{HANDLE_PARENT, MAX_MESSAGE_SIZE};

use crate::Handle;
use crate::channel;

// Re-export commonly used types from panda_abi::terminal
pub use panda_abi::terminal::{
    Alignment, ClearRegion, Colour, ColourSupport, NamedColour, Style, StyledSpan, StyledText,
    Table, TerminalCapabilities,
};

// =============================================================================
// Output functions
// =============================================================================

/// Print plain text (no newline).
pub fn print(s: &str) {
    send_output(Output::Text(String::from(s)));
}

/// Print plain text with newline.
pub fn println(s: &str) {
    let mut text = String::from(s);
    text.push('\n');
    send_output(Output::Text(text));
}

/// Print styled text.
pub fn print_styled(text: StyledText) {
    send_output(Output::Styled(text));
}

/// Display a table.
pub fn print_table(table: Table) {
    send_output(Output::Table(table));
}

/// Display key-value pairs.
pub fn print_key_values(pairs: Vec<(StyledText, StyledText)>) {
    send_output(Output::KeyValue(pairs));
}

/// Display a list.
pub fn print_list(items: Vec<StyledText>) {
    send_output(Output::List(items));
}

/// Print JSON (terminal can pretty-print and syntax highlight).
pub fn print_json(json: &str) {
    send_output(Output::Json(String::from(json)));
}

/// Print a hyperlink.
pub fn print_link(text: &str, url: &str) {
    send_output(Output::Link {
        text: String::from(text),
        url: String::from(url),
        style: None,
    });
}

/// Clear a region of the terminal.
pub fn clear(region: ClearRegion) {
    send_message(TerminalOutput::Clear(region));
}

/// Clear the entire screen.
pub fn clear_screen() {
    clear(ClearRegion::Screen);
}

/// Move the cursor to a position.
pub fn move_cursor(row: u16, col: u16) {
    send_message(TerminalOutput::MoveCursor { row, col });
}

/// Set the window title.
pub fn set_title(title: &str) {
    send_message(TerminalOutput::SetTitle(String::from(title)));
}

/// Report progress.
pub fn progress(current: u32, total: u32, message: &str) {
    send_message(TerminalOutput::Progress {
        current,
        total,
        message: String::from(message),
    });
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
    input_with_kind(Some(StyledText::plain(prompt)), InputKind::Line).and_then(|v| {
        if let InputValue::Text(s) = v {
            Some(s)
        } else {
            None
        }
    })
}

/// Read a line of input with a styled prompt.
pub fn input_styled(prompt: StyledText) -> Option<String> {
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
    input_with_kind(Some(StyledText::plain(prompt)), InputKind::Password).and_then(|v| {
        if let InputValue::Text(s) = v {
            Some(s)
        } else {
            None
        }
    })
}

/// Ask a yes/no confirmation question.
pub fn confirm(prompt: &str) -> bool {
    input_with_kind(Some(StyledText::plain(prompt)), InputKind::Confirm)
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
        prompt: Some(StyledText::plain(prompt)),
        choices: choices.iter().map(|s| String::from(*s)).collect(),
    };

    send_message(TerminalOutput::RequestInput(req));

    // Wait for response
    wait_for_input_response(id).and_then(|v| {
        if let InputValue::Choice(i) = v {
            Some(i)
        } else {
            None
        }
    })
}

fn input_with_kind(prompt: Option<StyledText>, kind: InputKind) -> Option<InputValue> {
    let id = next_request_id();
    let req = InputRequest {
        id,
        kind,
        prompt,
        choices: Vec::new(),
    };

    send_message(TerminalOutput::RequestInput(req));

    // Wait for response
    wait_for_input_response(id)
}

fn wait_for_input_response(expected_id: u32) -> Option<InputValue> {
    let parent = unsafe { Handle::from_raw(HANDLE_PARENT) };
    let mut buf = [0u8; MAX_MESSAGE_SIZE];

    loop {
        match channel::recv(parent, &mut buf) {
            Ok(len) => {
                if let Ok((msg, _)) = TerminalInput::from_bytes(&buf[..len]) {
                    match msg {
                        TerminalInput::Input(InputResponse { id, value }) if id == expected_id => {
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
    send_message(TerminalOutput::Query(TerminalQuery::Size));

    let parent = unsafe { Handle::from_raw(HANDLE_PARENT) };
    let mut buf = [0u8; MAX_MESSAGE_SIZE];

    loop {
        match channel::recv(parent, &mut buf) {
            Ok(len) => {
                if let Ok((msg, _)) = TerminalInput::from_bytes(&buf[..len]) {
                    if let TerminalInput::QueryResponse(QueryResponse::Size { cols, rows }) = msg {
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
    send_message(TerminalOutput::Query(TerminalQuery::Capabilities));

    let parent = unsafe { Handle::from_raw(HANDLE_PARENT) };
    let mut buf = [0u8; MAX_MESSAGE_SIZE];

    loop {
        match channel::recv(parent, &mut buf) {
            Ok(len) => {
                if let Ok((msg, _)) = TerminalInput::from_bytes(&buf[..len]) {
                    if let TerminalInput::QueryResponse(QueryResponse::Capabilities(caps)) = msg {
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

fn send_output(output: Output) {
    send_message(TerminalOutput::Write(output));
}

fn send_message(msg: TerminalOutput) {
    let bytes = msg.to_bytes();
    let parent = unsafe { Handle::from_raw(HANDLE_PARENT) };
    let _ = channel::send(parent, &bytes);
}

// =============================================================================
// Extension traits for builder helpers
// =============================================================================

/// Extension trait for StyledText with additional builder methods.
pub trait StyledTextExt {
    /// Create styled text with a single bold span.
    fn bold(text: &str) -> StyledText;

    /// Create styled text with a single coloured span.
    fn coloured(text: &str, colour: Colour) -> StyledText;

    /// Create styled text with a single span using full style.
    fn styled(text: &str, style: Style) -> StyledText;

    /// Push italic text.
    fn push_italic(&mut self, text: &str);

    /// Push underlined text.
    fn push_underline(&mut self, text: &str);

    /// Push text with foreground and background colours.
    fn push_coloured_bg(&mut self, text: &str, fg: Colour, bg: Colour);

    /// Convenience: push red error text.
    fn push_error(&mut self, text: &str);

    /// Convenience: push green success text.
    fn push_success(&mut self, text: &str);

    /// Convenience: push yellow warning text.
    fn push_warning(&mut self, text: &str);

    /// Convenience: push blue info text.
    fn push_info(&mut self, text: &str);
}

impl StyledTextExt for StyledText {
    fn bold(text: &str) -> StyledText {
        let mut st = StyledText::new();
        st.push_bold(text);
        st
    }

    fn coloured(text: &str, colour: Colour) -> StyledText {
        let mut st = StyledText::new();
        st.push_coloured(text, colour);
        st
    }

    fn styled(text: &str, style: Style) -> StyledText {
        StyledText {
            spans: alloc::vec![StyledSpan {
                text: String::from(text),
                style,
            }],
        }
    }

    fn push_italic(&mut self, text: &str) {
        self.push(
            text,
            Style {
                italic: true,
                ..Default::default()
            },
        );
    }

    fn push_underline(&mut self, text: &str) {
        self.push(
            text,
            Style {
                underline: true,
                ..Default::default()
            },
        );
    }

    fn push_coloured_bg(&mut self, text: &str, fg: Colour, bg: Colour) {
        self.push(
            text,
            Style {
                foreground: Some(fg),
                background: Some(bg),
                ..Default::default()
            },
        );
    }

    fn push_error(&mut self, text: &str) {
        self.push_coloured(text, Colour::Named(NamedColour::Red));
    }

    fn push_success(&mut self, text: &str) {
        self.push_coloured(text, Colour::Named(NamedColour::Green));
    }

    fn push_warning(&mut self, text: &str) {
        self.push_coloured(text, Colour::Named(NamedColour::Yellow));
    }

    fn push_info(&mut self, text: &str) {
        self.push_coloured(text, Colour::Named(NamedColour::Blue));
    }
}

/// Extension trait for Table with additional builder methods.
pub trait TableExt {
    /// Create a table with headers from string slices.
    fn with_header_strs(headers: &[&str]) -> Table;

    /// Add a row from string slices.
    fn add_row_strs(&mut self, cells: &[&str]);
}

impl TableExt for Table {
    fn with_header_strs(headers: &[&str]) -> Table {
        Table {
            headers: Some(headers.iter().map(|s| StyledText::plain(s)).collect()),
            rows: Vec::new(),
            alignment: Vec::new(),
        }
    }

    fn add_row_strs(&mut self, cells: &[&str]) {
        self.rows
            .push(cells.iter().map(|s| StyledText::plain(s)).collect());
    }
}
