# Terminal IPC Protocol

## Overview

A structured, stateless message-passing protocol between terminal emulators and child processes, replacing the traditional character-oriented VT100/ANSI model.

## Motivation

The Unix terminal model has fundamental limitations:

1. **In-band signaling**: Control sequences mixed with data (escape codes). Programs must parse byte streams.
2. **Stateful protocol**: Terminal state is implicit and easily corrupted.
3. **Character-oriented**: No structure for rich content (images, tables, links).
4. **No capability negotiation**: Programs guess terminal features via `$TERM`.
5. **Stdout hijacking**: Child processes write directly to a shared byte stream.

## Design Principles

1. **Stateless**: Style is a property of output content, not terminal state. Each `Write` message is self-contained.
2. **Typed messages**: Structured data over channels, not byte streams.
3. **Feature discovery**: Query capabilities before using optional features.
4. **Native by default**: New programs use the native protocol; ANSI is opt-in via client-side translation.
5. **Graceful degradation**: Unsupported features are silently ignored (use capability queries to avoid this).

## Design

### Architecture

```
┌─────────────┐     Channel      ┌─────────────┐
│   Terminal  │◄────(messages)───►│    Child    │
│  (renders)  │                  │  (app logic)│
└─────────────┘                  └─────────────┘
```

The terminal and child communicate via the parent channel (HANDLE_PARENT) using typed messages. The terminal owns rendering; the child sends content.

For shells running multiple background jobs, the shell acts as intermediary - each job has its own channel to the shell, and the shell multiplexes output to the terminal. Jobs are distinguished by their channel handle.

### Message Encoding

Messages use a simple TLV (type-length-value) format:

```
┌──────────┬──────────┬─────────────┐
│ Type(u16)│Length(u32)│ Payload ... │
└──────────┴──────────┴─────────────┘
```

This is simple to parse in no_std environments without serde.

### Message Protocol

All messages are serialized structs sent over the channel. Messages are auto-flushed (no explicit flush needed).

#### Output Messages (Child → Terminal)

```rust
/// Message from child process to terminal
enum TerminalOutput {
    /// Write content to the terminal (stateless - style is in the content)
    Write(Output),
    
    /// Move cursor to position (for interactive apps)
    MoveCursor { row: u16, col: u16 },
    
    /// Clear a region
    Clear(ClearRegion),
    
    /// Request input from user
    RequestInput(InputRequest),
    
    /// Set window title
    SetTitle(String),
    
    /// Report progress
    Progress { current: u32, total: u32, message: String },
    
    /// Query terminal capabilities/state
    Query(TerminalQuery),
    
    /// Exit with status (optional, process exit also works)
    Exit(i32),
}

/// Content that can be written to terminal
enum Output {
    /// Plain text (may contain newlines)
    Text(String),
    
    /// Text with embedded style spans (stateless - style per span)
    Styled(StyledText),
    
    /// Raw bytes (for binary data)
    Bytes(Vec<u8>),
    
    /// Image (terminal decides how to render - inline, sixel, kitty protocol, or placeholder)
    Image(Image),
    
    /// Hyperlink
    Link { text: String, url: String, style: Option<Style> },
    
    /// Table (terminal handles layout)
    Table(Table),
    
    /// Structured data (terminal can format as appropriate)
    Data(StructuredData),
}

/// Image content
struct Image {
    format: ImageFormat,
    data: Vec<u8>,
    alt: Option<String>,
    size: Option<ImageSize>,
}

enum ImageFormat {
    Png,
    Jpeg,
    Svg,
}

struct ImageSize {
    width_cells: Option<u16>,
    height_cells: Option<u16>,
    preserve_aspect: bool,
}

/// Table content
struct Table {
    headers: Option<Vec<StyledText>>,
    rows: Vec<Vec<StyledText>>,
    alignment: Vec<Alignment>,
}

/// Structured data that terminal can format
enum StructuredData {
    /// Key-value pairs (terminal can align, colorize)
    KeyValue(Vec<(StyledText, StyledText)>),
    /// List items (terminal can bullet/number)
    List(Vec<StyledText>),
    /// Tree structure (for file listings, etc.)
    Tree(TreeNode),
    /// JSON (terminal can pretty-print, syntax highlight)
    Json(String),
}

/// Text with style information (stateless - each span carries its style)
struct StyledText {
    spans: Vec<StyledSpan>,
}

struct StyledSpan {
    text: String,
    style: Style,
}

/// Text styling
#[derive(Default)]
struct Style {
    foreground: Option<Color>,
    background: Option<Color>,
    bold: bool,
    italic: bool,
    underline: bool,
    strikethrough: bool,
}

enum Color {
    Named(NamedColor),
    Palette(u8),
    Rgb { r: u8, g: u8, b: u8 },
}

enum ClearRegion {
    Screen,
    ToEndOfScreen,
    ToEndOfLine,
    Line,
}

struct InputRequest {
    prompt: Option<StyledText>,
    kind: InputKind,
    id: u32,
}

enum InputKind {
    Line,
    Password,
    Char,
    Confirm,
    Choice(Vec<String>),
    RawKeys,
}

enum TerminalQuery {
    Size,
    Capabilities,
    CursorPosition,
}
```

#### Input Messages (Terminal → Child)

```rust
/// Message from terminal to child process
enum TerminalInput {
    /// Response to RequestInput
    Input(InputResponse),
    
    /// Raw key event (when in RawKeys mode)
    Key(KeyEvent),
    
    /// Terminal resized
    Resize { cols: u16, rows: u16 },
    
    /// Signal from user (Ctrl+C, etc.)
    Signal(Signal),
    
    /// Response to query
    QueryResponse(QueryResponse),
}

struct InputResponse {
    id: u32,
    value: Option<InputValue>,
}

enum InputValue {
    Text(String),
    Char(char),
    Bool(bool),
    Choice(usize),
}

struct KeyEvent {
    code: KeyCode,
    modifiers: Modifiers,
}

enum Signal {
    Interrupt,  // Ctrl+C
    Quit,       // Ctrl+\
    Suspend,    // Ctrl+Z (if supported)
}

enum QueryResponse {
    Size { cols: u16, rows: u16 },
    Capabilities(TerminalCapabilities),
    CursorPosition { row: u16, col: u16 },
}

struct TerminalCapabilities {
    colors: ColorSupport,
    images: bool,
    hyperlinks: bool,
    unicode: bool,
}
```

### Userspace API

Simple high-level API in libpanda for common cases:

```rust
pub mod terminal {
    /// Print plain text (no newline)
    pub fn print(s: &str);
    
    /// Print plain text with newline
    pub fn println(s: &str);
    
    /// Print styled text
    pub fn print_styled(text: StyledText);
    
    /// Read a line of input
    pub fn read_line() -> Option<String>;
    
    /// Read a line with prompt
    pub fn input(prompt: &str) -> Option<String>;
    
    /// Read password (hidden)
    pub fn password(prompt: &str) -> Option<String>;
    
    /// Ask yes/no question
    pub fn confirm(prompt: &str) -> bool;
    
    /// Display a table
    pub fn table(t: Table);
    
    /// Display an image
    pub fn image(img: Image);
    
    /// Query terminal capabilities
    pub fn capabilities() -> TerminalCapabilities;
    
    /// Query terminal size
    pub fn size() -> (u16, u16);
}

// Low-level access for interactive apps
pub mod terminal::raw {
    /// Enter raw key mode
    pub fn enable_raw_keys();
    
    /// Exit raw key mode  
    pub fn disable_raw_keys();
    
    /// Get next key event
    pub fn read_key() -> KeyEvent;
    
    /// Send arbitrary output message
    pub fn send(msg: TerminalOutput);
    
    /// Receive input message
    pub fn recv() -> TerminalInput;
}

// Builder for styled text
impl StyledText {
    pub fn new() -> Self;
    pub fn plain(s: &str) -> Self;
    pub fn push(&mut self, text: &str, style: Style);
    pub fn bold(&mut self, text: &str);
    pub fn colored(&mut self, text: &str, fg: Color);
    // etc.
}
```

### Example: Simple Program

```rust
libpanda::main! {
    terminal::println("Hello, world!");
    0
}
```

### Example: Styled Output

```rust
libpanda::main! {
    let mut text = StyledText::new();
    text.colored("Error: ", Color::Named(NamedColor::Red));
    text.plain("file not found");
    terminal::print_styled(text);
    1
}
```

### Example: Interactive Program

```rust
libpanda::main! {
    terminal::raw::enable_raw_keys();
    terminal::raw::send(TerminalOutput::Clear(ClearRegion::Screen));
    
    loop {
        match terminal::raw::recv() {
            TerminalInput::Key(key) => {
                match key.code {
                    KeyCode::Char('q') if key.modifiers.ctrl => break,
                    KeyCode::Char(c) => { /* insert character */ }
                    _ => {}
                }
            }
            TerminalInput::Resize { cols, rows } => { /* redraw */ }
            TerminalInput::Signal(Signal::Interrupt) => break,
            _ => {}
        }
    }
    
    terminal::raw::disable_raw_keys();
    0
}
```

## ANSI Compatibility Layer

For porting existing software that emits ANSI escape codes, libpanda provides a client-side translation layer. This runs in the child process, not the terminal - the terminal only speaks the native protocol.

```rust
/// Client-side wrapper that translates ANSI escape codes to native protocol.
/// Use this when porting legacy software.
pub struct AnsiWriter {
    parser: AnsiParser,
}

impl AnsiWriter {
    pub fn new() -> Self;
    
    /// Write bytes, parsing ANSI escapes and sending native messages
    pub fn write(&mut self, bytes: &[u8]) {
        for action in self.parser.parse(bytes) {
            match action {
                AnsiAction::Print(text, style) => {
                    let styled = StyledText::with_style(&text, style);
                    terminal::raw::send(TerminalOutput::Write(Output::Styled(styled)));
                }
                AnsiAction::MoveCursor(row, col) => {
                    terminal::raw::send(TerminalOutput::MoveCursor { row, col });
                }
                AnsiAction::Clear(region) => {
                    terminal::raw::send(TerminalOutput::Clear(region));
                }
                // ... etc
            }
        }
    }
}

// Usage in ported legacy code:
let mut writer = AnsiWriter::new();
writer.write(b"\x1b[31mRed text\x1b[0m normal");
```

This approach:
- Keeps the terminal simple (only native protocol)
- Puts complexity where it belongs (legacy compatibility in client)
- Allows gradual migration from ANSI to native

## Implementation Plan

### Phase 1: Core Protocol
1. Define message types in panda-abi
2. Add TLV serialization (simple, no_std compatible)
3. Implement terminal side: receive messages, render
4. Implement libpanda::terminal module with print/println/input

### Phase 2: Enhanced Output
1. Add StyledText support with builder API
2. Add Table rendering
3. Add StructuredData formatting (KeyValue, List, Tree)

### Phase 3: Rich Content
1. Add Image support (inline rendering or alt text fallback)
2. Add hyperlink support

### Phase 4: Compatibility
1. Implement AnsiWriter parser in libpanda
2. Test with legacy code patterns

### Phase 5: Advanced Features
1. Raw key mode for interactive apps
2. Query/response for capabilities
3. Progress indicators
