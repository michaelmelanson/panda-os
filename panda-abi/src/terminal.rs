//! Terminal IPC protocol types.
//!
//! A structured, stateless message-passing protocol between terminal emulators
//! and child processes, replacing the traditional character-oriented VT100/ANSI model.

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;

use crate::encoding::{Decode, DecodeError, Decoder, Encode, Encoder};

// =============================================================================
// Message type identifiers
// =============================================================================

/// Message type identifiers for TLV encoding.
#[repr(u16)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageType {
    // Request messages (child -> terminal via PARENT): 0x0000 - 0x00FF
    Write = 0x0001,
    MoveCursor = 0x0002,
    Clear = 0x0003,
    RequestInput = 0x0004,
    SetTitle = 0x0005,
    Progress = 0x0006,
    Query = 0x0007,
    Exit = 0x0008,
    Error = 0x0009,
    Warning = 0x000A,

    // Event messages (terminal -> child via PARENT): 0x0100 - 0x01FF
    InputResponse = 0x0100,
    Key = 0x0101,
    Resize = 0x0102,
    Signal = 0x0103,
    QueryResponse = 0x0104,
}

// =============================================================================
// Styling types
// =============================================================================

/// Text styling (stateless - each span carries its style).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Style {
    pub foreground: Option<Colour>,
    pub background: Option<Colour>,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub strikethrough: bool,
}

impl Encode for Style {
    fn encode(&self, enc: &mut Encoder) {
        let flags = (if self.bold { 1 } else { 0 })
            | (if self.italic { 2 } else { 0 })
            | (if self.underline { 4 } else { 0 })
            | (if self.strikethrough { 8 } else { 0 });
        enc.write_u8(flags);
        self.foreground.encode(enc);
        self.background.encode(enc);
    }
}

impl Decode for Style {
    fn decode(dec: &mut Decoder) -> Result<Self, DecodeError> {
        let flags = dec.read_u8()?;
        let foreground = Option::<Colour>::decode(dec)?;
        let background = Option::<Colour>::decode(dec)?;
        Ok(Self {
            bold: flags & 1 != 0,
            italic: flags & 2 != 0,
            underline: flags & 4 != 0,
            strikethrough: flags & 8 != 0,
            foreground,
            background,
        })
    }
}

/// Colour specification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Colour {
    /// Standard named colours (0-15)
    Named(NamedColour),
    /// 256-colour palette
    Palette(u8),
    /// True colour RGB
    Rgb { r: u8, g: u8, b: u8 },
}

impl Encode for Colour {
    fn encode(&self, enc: &mut Encoder) {
        match self {
            Colour::Named(c) => {
                enc.write_u8(1);
                enc.write_u8(*c as u8);
            }
            Colour::Palette(p) => {
                enc.write_u8(2);
                enc.write_u8(*p);
            }
            Colour::Rgb { r, g, b } => {
                enc.write_u8(3);
                enc.write_u8(*r);
                enc.write_u8(*g);
                enc.write_u8(*b);
            }
        }
    }
}

impl Decode for Colour {
    fn decode(dec: &mut Decoder) -> Result<Self, DecodeError> {
        match dec.read_u8()? {
            1 => {
                let c = dec.read_u8()?;
                let named = NamedColour::from_u8(c).ok_or(DecodeError::InvalidValue)?;
                Ok(Colour::Named(named))
            }
            2 => Ok(Colour::Palette(dec.read_u8()?)),
            3 => {
                let r = dec.read_u8()?;
                let g = dec.read_u8()?;
                let b = dec.read_u8()?;
                Ok(Colour::Rgb { r, g, b })
            }
            _ => Err(DecodeError::InvalidValue),
        }
    }
}

/// Standard terminal colours.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NamedColour {
    Black = 0,
    Red = 1,
    Green = 2,
    Yellow = 3,
    Blue = 4,
    Magenta = 5,
    Cyan = 6,
    White = 7,
    BrightBlack = 8,
    BrightRed = 9,
    BrightGreen = 10,
    BrightYellow = 11,
    BrightBlue = 12,
    BrightMagenta = 13,
    BrightCyan = 14,
    BrightWhite = 15,
}

impl NamedColour {
    fn from_u8(value: u8) -> Option<Self> {
        match value {
            0 => Some(Self::Black),
            1 => Some(Self::Red),
            2 => Some(Self::Green),
            3 => Some(Self::Yellow),
            4 => Some(Self::Blue),
            5 => Some(Self::Magenta),
            6 => Some(Self::Cyan),
            7 => Some(Self::White),
            8 => Some(Self::BrightBlack),
            9 => Some(Self::BrightRed),
            10 => Some(Self::BrightGreen),
            11 => Some(Self::BrightYellow),
            12 => Some(Self::BrightBlue),
            13 => Some(Self::BrightMagenta),
            14 => Some(Self::BrightCyan),
            15 => Some(Self::BrightWhite),
            _ => None,
        }
    }
}

// =============================================================================
// Alignment
// =============================================================================

/// Table alignment (for terminal rendering hints).
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Alignment {
    #[default]
    Left = 0,
    Centre = 1,
    Right = 2,
}

impl Encode for Alignment {
    fn encode(&self, enc: &mut Encoder) {
        enc.write_u8(*self as u8);
    }
}

impl Decode for Alignment {
    fn decode(dec: &mut Decoder) -> Result<Self, DecodeError> {
        match dec.read_u8()? {
            0 => Ok(Self::Left),
            1 => Ok(Self::Centre),
            2 => Ok(Self::Right),
            _ => Err(DecodeError::InvalidValue),
        }
    }
}

// =============================================================================
// Other output types
// =============================================================================

/// Region to clear.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClearRegion {
    /// Entire screen
    Screen = 0,
    /// From cursor to end of screen
    ToEndOfScreen = 1,
    /// From cursor to end of line
    ToEndOfLine = 2,
    /// Current line only
    Line = 3,
}

impl Encode for ClearRegion {
    fn encode(&self, enc: &mut Encoder) {
        enc.write_u8(*self as u8);
    }
}

impl Decode for ClearRegion {
    fn decode(dec: &mut Decoder) -> Result<Self, DecodeError> {
        match dec.read_u8()? {
            0 => Ok(Self::Screen),
            1 => Ok(Self::ToEndOfScreen),
            2 => Ok(Self::ToEndOfLine),
            3 => Ok(Self::Line),
            _ => Err(DecodeError::InvalidValue),
        }
    }
}

/// Input request kind.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputKind {
    /// Read a line of text
    Line = 0,
    /// Read a line without echo (for passwords)
    Password = 1,
    /// Read a single character
    Char = 2,
    /// Yes/no confirmation
    Confirm = 3,
    /// Choice from options
    Choice = 4,
    /// Raw key events mode
    RawKeys = 5,
}

impl Encode for InputKind {
    fn encode(&self, enc: &mut Encoder) {
        enc.write_u8(*self as u8);
    }
}

impl Decode for InputKind {
    fn decode(dec: &mut Decoder) -> Result<Self, DecodeError> {
        match dec.read_u8()? {
            0 => Ok(Self::Line),
            1 => Ok(Self::Password),
            2 => Ok(Self::Char),
            3 => Ok(Self::Confirm),
            4 => Ok(Self::Choice),
            5 => Ok(Self::RawKeys),
            _ => Err(DecodeError::InvalidValue),
        }
    }
}

/// Input request from child to terminal.
#[derive(Debug, Clone, PartialEq)]
pub struct InputRequest {
    /// Request ID for correlation
    pub id: u32,
    /// Type of input requested
    pub kind: InputKind,
    /// Optional prompt to display (as a Value for styled prompts)
    pub prompt: Option<crate::value::Value>,
    /// Choices (only for InputKind::Choice)
    pub choices: Vec<String>,
}

impl Encode for InputRequest {
    fn encode(&self, enc: &mut Encoder) {
        self.id.encode(enc);
        self.kind.encode(enc);
        self.prompt.encode(enc);
        self.choices.encode(enc);
    }
}

impl Decode for InputRequest {
    fn decode(dec: &mut Decoder) -> Result<Self, DecodeError> {
        let id = u32::decode(dec)?;
        let kind = InputKind::decode(dec)?;
        let prompt = Option::<crate::value::Value>::decode(dec)?;
        let choices = Vec::<String>::decode(dec)?;
        Ok(Self {
            id,
            kind,
            prompt,
            choices,
        })
    }
}

/// Terminal query types.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminalQuery {
    /// Query terminal size
    Size = 0,
    /// Query terminal capabilities
    Capabilities = 1,
    /// Query cursor position
    CursorPosition = 2,
}

impl Encode for TerminalQuery {
    fn encode(&self, enc: &mut Encoder) {
        enc.write_u8(*self as u8);
    }
}

impl Decode for TerminalQuery {
    fn decode(dec: &mut Decoder) -> Result<Self, DecodeError> {
        match dec.read_u8()? {
            0 => Ok(Self::Size),
            1 => Ok(Self::Capabilities),
            2 => Ok(Self::CursorPosition),
            _ => Err(DecodeError::InvalidValue),
        }
    }
}

// =============================================================================
// Control plane messages (child -> terminal)
// =============================================================================

/// Control message from child process to terminal (via PARENT channel).
///
/// These are control plane messages for interactive features (input prompts,
/// queries), error/warning display, and UI control. For data output, send
/// `Value` objects through STDOUT instead.
#[derive(Debug, Clone, PartialEq)]
pub enum Request {
    /// Display error message (always shown, even from middle pipeline stages).
    /// Use this for errors that must reach the user regardless of pipeline position.
    Error(crate::value::Value),
    /// Display warning message (always shown, even from middle pipeline stages).
    Warning(crate::value::Value),
    /// Move cursor to position
    MoveCursor { row: u16, col: u16 },
    /// Clear a region
    Clear(ClearRegion),
    /// Request input from user
    RequestInput(InputRequest),
    /// Set window title
    SetTitle(String),
    /// Report progress
    Progress {
        current: u32,
        total: u32,
        message: String,
    },
    /// Query terminal capabilities/state
    Query(TerminalQuery),
    /// Exit with status
    Exit(i32),
    /// Write a Value to the terminal for display.
    /// For standalone programs, this is how output reaches the terminal.
    /// For pipeline programs, prefer using STDOUT channel for data flow.
    Write(crate::value::Value),
}

impl Request {
    /// Encode this message to bytes with TLV header.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut enc = Encoder::new();
        self.encode_with_header(&mut enc);
        enc.finish()
    }

    /// Decode a message from bytes with TLV header.
    pub fn from_bytes(bytes: &[u8]) -> Result<(Self, usize), DecodeError> {
        let mut dec = Decoder::new(bytes);
        let msg = Self::decode_with_header(&mut dec)?;
        Ok((msg, dec.position()))
    }

    fn encode_with_header(&self, enc: &mut Encoder) {
        let msg_type = match self {
            Self::Error(_) => MessageType::Error,
            Self::Warning(_) => MessageType::Warning,
            Self::MoveCursor { .. } => MessageType::MoveCursor,
            Self::Clear(_) => MessageType::Clear,
            Self::RequestInput(_) => MessageType::RequestInput,
            Self::SetTitle(_) => MessageType::SetTitle,
            Self::Progress { .. } => MessageType::Progress,
            Self::Query(_) => MessageType::Query,
            Self::Exit(_) => MessageType::Exit,
            Self::Write(_) => MessageType::Write,
        };

        // Write header with placeholder length
        let len_pos = enc.write_tlv_header(msg_type as u16, 0);
        let content_start = enc.len();

        // Write content
        match self {
            Self::Error(value) => value.encode(enc),
            Self::Warning(value) => value.encode(enc),
            Self::MoveCursor { row, col } => {
                enc.write_u16(*row);
                enc.write_u16(*col);
            }
            Self::Clear(region) => region.encode(enc),
            Self::RequestInput(req) => req.encode(enc),
            Self::SetTitle(title) => title.encode(enc),
            Self::Progress {
                current,
                total,
                message,
            } => {
                enc.write_u32(*current);
                enc.write_u32(*total);
                message.encode(enc);
            }
            Self::Query(query) => query.encode(enc),
            Self::Exit(code) => enc.write_i32(*code),
            Self::Write(value) => value.encode(enc),
        }

        // Update length
        let content_len = enc.len() - content_start;
        enc.update_length(len_pos, content_len as u32);
    }

    fn decode_with_header(dec: &mut Decoder) -> Result<Self, DecodeError> {
        let (msg_type, length) = dec.read_tlv_header()?;
        let _length = length; // We trust the content decoding to consume the right amount

        match msg_type {
            x if x == MessageType::Error as u16 => {
                let value = crate::value::Value::decode(dec)?;
                Ok(Self::Error(value))
            }
            x if x == MessageType::Warning as u16 => {
                let value = crate::value::Value::decode(dec)?;
                Ok(Self::Warning(value))
            }
            x if x == MessageType::Write as u16 => {
                let value = crate::value::Value::decode(dec)?;
                Ok(Self::Write(value))
            }
            x if x == MessageType::MoveCursor as u16 => {
                let row = dec.read_u16()?;
                let col = dec.read_u16()?;
                Ok(Self::MoveCursor { row, col })
            }
            x if x == MessageType::Clear as u16 => {
                let region = ClearRegion::decode(dec)?;
                Ok(Self::Clear(region))
            }
            x if x == MessageType::RequestInput as u16 => {
                let req = InputRequest::decode(dec)?;
                Ok(Self::RequestInput(req))
            }
            x if x == MessageType::SetTitle as u16 => {
                let title = String::decode(dec)?;
                Ok(Self::SetTitle(title))
            }
            x if x == MessageType::Progress as u16 => {
                let current = dec.read_u32()?;
                let total = dec.read_u32()?;
                let message = String::decode(dec)?;
                Ok(Self::Progress {
                    current,
                    total,
                    message,
                })
            }
            x if x == MessageType::Query as u16 => {
                let query = TerminalQuery::decode(dec)?;
                Ok(Self::Query(query))
            }
            x if x == MessageType::Exit as u16 => {
                let code = dec.read_i32()?;
                Ok(Self::Exit(code))
            }
            _ => Err(DecodeError::UnknownType),
        }
    }
}

// =============================================================================
// Input messages (Terminal -> Child)
// =============================================================================

/// Signal from terminal to child.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Signal {
    /// Ctrl+C
    Interrupt = 0,
    /// Ctrl+\
    Quit = 1,
    /// Ctrl+Z
    Suspend = 2,
}

impl Encode for Signal {
    fn encode(&self, enc: &mut Encoder) {
        enc.write_u8(*self as u8);
    }
}

impl Decode for Signal {
    fn decode(dec: &mut Decoder) -> Result<Self, DecodeError> {
        match dec.read_u8()? {
            0 => Ok(Self::Interrupt),
            1 => Ok(Self::Quit),
            2 => Ok(Self::Suspend),
            _ => Err(DecodeError::InvalidValue),
        }
    }
}

/// Input value in response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InputValue {
    /// Text input (Line, Password)
    Text(String),
    /// Single character (Char)
    Char(char),
    /// Boolean (Confirm)
    Bool(bool),
    /// Choice index (Choice)
    Choice(usize),
}

impl Encode for InputValue {
    fn encode(&self, enc: &mut Encoder) {
        match self {
            InputValue::Text(s) => {
                enc.write_u8(0);
                s.encode(enc);
            }
            InputValue::Char(c) => {
                enc.write_u8(1);
                let mut buf = [0u8; 4];
                let s = c.encode_utf8(&mut buf);
                enc.write_string(s);
            }
            InputValue::Bool(b) => {
                enc.write_u8(2);
                enc.write_bool(*b);
            }
            InputValue::Choice(idx) => {
                enc.write_u8(3);
                enc.write_u32(*idx as u32);
            }
        }
    }
}

impl Decode for InputValue {
    fn decode(dec: &mut Decoder) -> Result<Self, DecodeError> {
        match dec.read_u8()? {
            0 => Ok(InputValue::Text(String::decode(dec)?)),
            1 => {
                let s = String::decode(dec)?;
                let c = s.chars().next().ok_or(DecodeError::InvalidValue)?;
                Ok(InputValue::Char(c))
            }
            2 => Ok(InputValue::Bool(dec.read_bool()?)),
            3 => Ok(InputValue::Choice(dec.read_u32()? as usize)),
            _ => Err(DecodeError::InvalidValue),
        }
    }
}

/// Input response from terminal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InputResponse {
    /// Request ID for correlation
    pub id: u32,
    /// Input value (None if cancelled)
    pub value: Option<InputValue>,
}

impl Encode for InputResponse {
    fn encode(&self, enc: &mut Encoder) {
        self.id.encode(enc);
        self.value.encode(enc);
    }
}

impl Decode for InputResponse {
    fn decode(dec: &mut Decoder) -> Result<Self, DecodeError> {
        let id = u32::decode(dec)?;
        let value = Option::<InputValue>::decode(dec)?;
        Ok(Self { id, value })
    }
}

/// Key event data.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyEvent {
    /// Key code
    pub code: u16,
    /// Modifiers
    pub modifiers: KeyModifiers,
    /// Value: 0=release, 1=press, 2=repeat
    pub value: u8,
}

impl Encode for KeyEvent {
    fn encode(&self, enc: &mut Encoder) {
        enc.write_u16(self.code);
        let mods = (if self.modifiers.shift { 1 } else { 0 })
            | (if self.modifiers.ctrl { 2 } else { 0 })
            | (if self.modifiers.alt { 4 } else { 0 });
        enc.write_u8(mods);
        enc.write_u8(self.value);
    }
}

impl Decode for KeyEvent {
    fn decode(dec: &mut Decoder) -> Result<Self, DecodeError> {
        let code = dec.read_u16()?;
        let mods = dec.read_u8()?;
        let value = dec.read_u8()?;
        Ok(Self {
            code,
            modifiers: KeyModifiers {
                shift: mods & 1 != 0,
                ctrl: mods & 2 != 0,
                alt: mods & 4 != 0,
            },
            value,
        })
    }
}

/// Key modifiers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct KeyModifiers {
    pub shift: bool,
    pub ctrl: bool,
    pub alt: bool,
}

/// Colour support level.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColourSupport {
    /// No colour support
    None = 0,
    /// 16 colours
    Basic = 1,
    /// 256 colours
    Palette = 2,
    /// True colour (24-bit)
    TrueColour = 3,
}

impl Encode for ColourSupport {
    fn encode(&self, enc: &mut Encoder) {
        enc.write_u8(*self as u8);
    }
}

impl Decode for ColourSupport {
    fn decode(dec: &mut Decoder) -> Result<Self, DecodeError> {
        match dec.read_u8()? {
            0 => Ok(Self::None),
            1 => Ok(Self::Basic),
            2 => Ok(Self::Palette),
            3 => Ok(Self::TrueColour),
            _ => Err(DecodeError::InvalidValue),
        }
    }
}

/// Terminal capabilities.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TerminalCapabilities {
    /// Colour support level
    pub colours: ColourSupport,
    /// Supports inline images
    pub images: bool,
    /// Supports hyperlinks
    pub hyperlinks: bool,
    /// Supports Unicode
    pub unicode: bool,
}

impl Encode for TerminalCapabilities {
    fn encode(&self, enc: &mut Encoder) {
        self.colours.encode(enc);
        enc.write_bool(self.images);
        enc.write_bool(self.hyperlinks);
        enc.write_bool(self.unicode);
    }
}

impl Decode for TerminalCapabilities {
    fn decode(dec: &mut Decoder) -> Result<Self, DecodeError> {
        let colours = ColourSupport::decode(dec)?;
        let images = dec.read_bool()?;
        let hyperlinks = dec.read_bool()?;
        let unicode = dec.read_bool()?;
        Ok(Self {
            colours,
            images,
            hyperlinks,
            unicode,
        })
    }
}

/// Query response from terminal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QueryResponse {
    /// Terminal size
    Size { cols: u16, rows: u16 },
    /// Terminal capabilities
    Capabilities(TerminalCapabilities),
    /// Cursor position
    CursorPosition { row: u16, col: u16 },
}

impl Encode for QueryResponse {
    fn encode(&self, enc: &mut Encoder) {
        match self {
            QueryResponse::Size { cols, rows } => {
                enc.write_u8(0);
                enc.write_u16(*cols);
                enc.write_u16(*rows);
            }
            QueryResponse::Capabilities(caps) => {
                enc.write_u8(1);
                caps.encode(enc);
            }
            QueryResponse::CursorPosition { row, col } => {
                enc.write_u8(2);
                enc.write_u16(*row);
                enc.write_u16(*col);
            }
        }
    }
}

impl Decode for QueryResponse {
    fn decode(dec: &mut Decoder) -> Result<Self, DecodeError> {
        match dec.read_u8()? {
            0 => {
                let cols = dec.read_u16()?;
                let rows = dec.read_u16()?;
                Ok(QueryResponse::Size { cols, rows })
            }
            1 => {
                let caps = TerminalCapabilities::decode(dec)?;
                Ok(QueryResponse::Capabilities(caps))
            }
            2 => {
                let row = dec.read_u16()?;
                let col = dec.read_u16()?;
                Ok(QueryResponse::CursorPosition { row, col })
            }
            _ => Err(DecodeError::InvalidValue),
        }
    }
}

/// Event message from terminal to child process (via PARENT channel).
///
/// Control plane events: the terminal notifies the child about input,
/// signals, and query responses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// Response to RequestInput
    Input(InputResponse),
    /// Raw key event (when in RawKeys mode)
    Key(KeyEvent),
    /// Terminal resized
    Resize { cols: u16, rows: u16 },
    /// Signal from user
    Signal(Signal),
    /// Response to query
    QueryResponse(QueryResponse),
}

impl Event {
    /// Encode this message to bytes with TLV header.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut enc = Encoder::new();
        self.encode_with_header(&mut enc);
        enc.finish()
    }

    /// Decode a message from bytes with TLV header.
    pub fn from_bytes(bytes: &[u8]) -> Result<(Self, usize), DecodeError> {
        let mut dec = Decoder::new(bytes);
        let msg = Self::decode_with_header(&mut dec)?;
        Ok((msg, dec.position()))
    }

    fn encode_with_header(&self, enc: &mut Encoder) {
        let msg_type = match self {
            Self::Input(_) => MessageType::InputResponse,
            Self::Key(_) => MessageType::Key,
            Self::Resize { .. } => MessageType::Resize,
            Self::Signal(_) => MessageType::Signal,
            Self::QueryResponse(_) => MessageType::QueryResponse,
        };

        // Write header with placeholder length
        let len_pos = enc.write_tlv_header(msg_type as u16, 0);
        let content_start = enc.len();

        // Write content
        match self {
            Self::Input(resp) => resp.encode(enc),
            Self::Key(key) => key.encode(enc),
            Self::Resize { cols, rows } => {
                enc.write_u16(*cols);
                enc.write_u16(*rows);
            }
            Self::Signal(sig) => sig.encode(enc),
            Self::QueryResponse(resp) => resp.encode(enc),
        }

        // Update length
        let content_len = enc.len() - content_start;
        enc.update_length(len_pos, content_len as u32);
    }

    fn decode_with_header(dec: &mut Decoder) -> Result<Self, DecodeError> {
        let (msg_type, _length) = dec.read_tlv_header()?;

        match msg_type {
            x if x == MessageType::InputResponse as u16 => {
                let resp = InputResponse::decode(dec)?;
                Ok(Self::Input(resp))
            }
            x if x == MessageType::Key as u16 => {
                let key = KeyEvent::decode(dec)?;
                Ok(Self::Key(key))
            }
            x if x == MessageType::Resize as u16 => {
                let cols = dec.read_u16()?;
                let rows = dec.read_u16()?;
                Ok(Self::Resize { cols, rows })
            }
            x if x == MessageType::Signal as u16 => {
                let sig = Signal::decode(dec)?;
                Ok(Self::Signal(sig))
            }
            x if x == MessageType::QueryResponse as u16 => {
                let resp = QueryResponse::decode(dec)?;
                Ok(Self::QueryResponse(resp))
            }
            _ => Err(DecodeError::UnknownType),
        }
    }
}
