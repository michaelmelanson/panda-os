# Terminal IPC Protocol

## Overview

A structured message-passing protocol between terminal emulators and child processes, replacing the traditional character-oriented VT100/ANSI model.

## Motivation

The Unix terminal model has fundamental limitations:

1. **In-band signaling**: Control sequences mixed with data (escape codes). Programs must parse byte streams.
2. **Stateful protocol**: Terminal state is implicit and easily corrupted.
3. **Character-oriented**: No structure for rich content (images, tables, links).
4. **No capability negotiation**: Programs guess terminal features via `$TERM`.
5. **Stdout hijacking**: Child processes write directly to a shared byte stream.

## Design

### Architecture

```
┌─────────────┐     Channel      ┌─────────────┐
│   Terminal  │◄────(messages)───►│    Child    │
│  (renders)  │                  │  (app logic)│
└─────────────┘                  └─────────────┘
```

The terminal and child communicate via the parent channel (HANDLE_PARENT) using typed messages. The terminal owns rendering; the child sends content.

### Message Protocol

All messages are serialized structs sent over the channel.

#### Output Messages (Child → Terminal)

```rust
/// Message from child process to terminal
enum TerminalOutput {
    /// Write content to the terminal
    Write(Output),
    
    /// Set text style for subsequent writes
    SetStyle(Style),
    
    /// Move cursor to position
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
    
    /// Text with embedded style spans
    StyledText(StyledText),
    
    /// Raw bytes (for compatibility/binary data)
    Bytes(Vec<u8>),
    
    /// Image (terminal decides how to render - inline, sixel, kitty protocol, or placeholder)
    Image(Image),
    
    /// Hyperlink
    Link { text: String, url: String },
    
    /// Table (terminal handles layout)
    Table(Table),
    
    /// Structured data (terminal can format as appropriate)
    Data(StructuredData),
}

/// Image content
struct Image {
    /// Image format
    format: ImageFormat,
    /// Raw image data
    data: Vec<u8>,
    /// Optional alt text
    alt: Option<String>,
    /// Sizing hints
    size: Option<ImageSize>,
}

enum ImageFormat {
    Png,
    Jpeg,
    Svg,
    // Future: video, animated
}

struct ImageSize {
    /// Desired width in cells (None = auto)
    width_cells: Option<u16>,
    /// Desired height in cells (None = auto)  
    height_cells: Option<u16>,
    /// Preserve aspect ratio
    preserve_aspect: bool,
}

/// Table content
struct Table {
    headers: Option<Vec<String>>,
    rows: Vec<Vec<String>>,
    /// Column alignment hints
    alignment: Vec<Alignment>,
}

/// Structured data that terminal can format
enum StructuredData {
    /// Key-value pairs (terminal can align, colorize)
    KeyValue(Vec<(String, String)>),
    /// List items (terminal can bullet/number)
    List(Vec<String>),
    /// Tree structure (for file listings, etc.)
    Tree(TreeNode),
    /// JSON (terminal can pretty-print, syntax highlight)
    Json(String),
}

/// Text with style information
struct StyledText {
    spans: Vec<StyledSpan>,
}

struct StyledSpan {
    text: String,
    style: Style,
}

/// Text styling
struct Style {
    foreground: Option<Color>,
    background: Option<Color>,
    bold: bool,
    italic: bool,
    underline: bool,
    strikethrough: bool,
}

enum Color {
    /// Basic 16 colors
    Named(NamedColor),
    /// 256 color palette
    Palette(u8),
    /// True color
    Rgb { r: u8, g: u8, b: u8 },
}

enum ClearRegion {
    /// Clear entire screen
    Screen,
    /// Clear from cursor to end of screen
    ToEndOfScreen,
    /// Clear from cursor to end of line
    ToEndOfLine,
    /// Clear entire line
    Line,
}

/// Input request types
struct InputRequest {
    /// Prompt to display (if any)
    prompt: Option<String>,
    /// Type of input expected
    kind: InputKind,
    /// Request ID for matching response
    id: u32,
}

enum InputKind {
    /// Single line of text
    Line,
    /// Password (hidden input)
    Password,
    /// Single character
    Char,
    /// Yes/no confirmation
    Confirm,
    /// Choice from options
    Choice(Vec<String>),
    /// Raw key events (for interactive apps)
    RawKeys,
}

enum TerminalQuery {
    /// Get terminal size
    Size,
    /// Get supported capabilities
    Capabilities,
    /// Get current cursor position
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
    /// Matches InputRequest.id
    id: u32,
    /// The input value (None if cancelled)
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
// Simple output (most programs just need this)
pub mod terminal {
    /// Print text (no newline)
    pub fn print(s: &str);
    
    /// Print text with newline
    pub fn println(s: &str);
    
    /// Print formatted text with styles
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
    
    /// Set text style for subsequent prints
    pub fn set_style(style: Style);
    
    /// Reset to default style
    pub fn reset_style();
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
```

### Example: Simple Program

```rust
// hello.rs
libpanda::main! {
    terminal::println("Hello, world!");
    0
}

// cat.rs
libpanda::main! {
    let args = libpanda::args();
    for path in &args[1..] {
        let content = fs::read_to_string(path)?;
        terminal::print(&content);
    }
    0
}

// interactive greeter
libpanda::main! {
    let name = terminal::input("What's your name? ")?;
    terminal::println(&format!("Hello, {}!", name));
    0
}
```

### Example: Interactive Program

```rust
// Simple text editor (conceptual)
libpanda::main! {
    terminal::raw::enable_raw_keys();
    terminal::send(TerminalOutput::Clear(ClearRegion::Screen));
    
    loop {
        match terminal::raw::recv() {
            TerminalInput::Key(key) => {
                match key.code {
                    KeyCode::Char('q') if key.modifiers.ctrl => break,
                    KeyCode::Char(c) => { /* insert character */ }
                    KeyCode::Arrow(dir) => { /* move cursor */ }
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

For porting existing software, provide an ANSI escape code parser that translates to terminal messages:

```rust
/// Wraps a channel to accept raw bytes and parse ANSI sequences
struct AnsiCompat {
    channel: Channel,
    parser: AnsiParser,
}

impl AnsiCompat {
    /// Write bytes, parsing ANSI escapes into terminal messages
    fn write(&mut self, bytes: &[u8]) {
        for action in self.parser.parse(bytes) {
            match action {
                AnsiAction::Print(text) => {
                    self.channel.send(TerminalOutput::Write(Output::Text(text)));
                }
                AnsiAction::SetColor(fg, bg) => {
                    self.channel.send(TerminalOutput::SetStyle(Style { 
                        foreground: fg, 
                        background: bg,
                        ..Default::default()
                    }));
                }
                AnsiAction::MoveCursor(row, col) => {
                    self.channel.send(TerminalOutput::MoveCursor { row, col });
                }
                AnsiAction::Clear(region) => {
                    self.channel.send(TerminalOutput::Clear(region));
                }
                // ... etc
            }
        }
    }
}
```

This allows running legacy programs that emit ANSI codes, while native programs use the cleaner message API.

## Implementation Plan

### Phase 1: Core Protocol
1. Define message types in panda-abi
2. Add serialization (simple binary format, not serde - we're no_std)
3. Implement terminal side: receive messages, render appropriately
4. Implement libpanda::terminal module with basic print/println/input

### Phase 2: Enhanced Output
1. Add StyledText support
2. Add Table rendering
3. Add StructuredData formatting (KeyValue, List, Tree)

### Phase 3: Rich Content
1. Add Image support (inline rendering or placeholder)
2. Add hyperlink support

### Phase 4: Compatibility
1. Implement AnsiCompat parser
2. Add legacy mode for programs that write raw bytes

### Phase 5: Advanced Features
1. Raw key mode for interactive apps
2. Query/response for capabilities
3. Progress indicators

## Open Questions

1. **Message encoding**: Use a simple TLV (type-length-value) format? Or something more structured?

2. **Backward compatibility**: Should HANDLE_PARENT default to ANSI mode, requiring opt-in for structured mode? Or vice versa?

3. **Buffering**: Should Write messages be auto-flushed, or should there be explicit flush? (Probably auto-flush for simplicity, with batching hints for performance.)

4. **Error handling**: What happens if terminal doesn't support a feature (e.g., images)? Silent fallback to alt text? Error response?

5. **Multiplexing**: If a shell runs multiple background jobs, how do their outputs interleave? (Probably: shell is intermediary, not terminal.)
