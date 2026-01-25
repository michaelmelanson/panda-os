#![no_std]
#![no_main]

extern crate alloc;
extern crate panda_abi;

use alloc::string::String;
use alloc::vec::Vec;
use fontdue::{Font, FontSettings};
use libpanda::{
    buffer::Buffer,
    channel, environment, file,
    mailbox::{Event, Mailbox},
    process,
    syscall::send,
    Handle,
};
use panda_abi::{
    terminal::{
        ClearRegion, Colour, ColourSupport, InputKind, InputResponse, InputValue, NamedColour,
        Output, QueryResponse, StyledText, Table, TerminalCapabilities, TerminalInput,
        TerminalOutput, TerminalQuery,
    },
    BlitParams, FillParams, UpdateParamsIn, EVENT_CHANNEL_READABLE, EVENT_KEYBOARD_KEY,
    EVENT_PROCESS_EXITED, MAX_MESSAGE_SIZE, OP_SURFACE_BLIT, OP_SURFACE_FILL, OP_SURFACE_FLUSH,
    OP_SURFACE_UPDATE_PARAMS,
};

// Terminal colours (ARGB format)
const COLOUR_BACKGROUND: u32 = 0xFF1E1E1E; // Dark grey
const COLOUR_DEFAULT_FG: u32 = 0xFFD4D4D4; // Light grey

const MARGIN: u32 = 10;
const FONT_SIZE: f32 = 16.0;
const LINE_HEIGHT: u32 = 20; // Font size + spacing

// =============================================================================
// Word iterator for line wrapping
// =============================================================================

/// A word or separator in text for line wrapping purposes.
enum Word<'a> {
    /// A newline character
    Newline,
    /// Whitespace (spaces, tabs)
    Whitespace(&'a str),
    /// A word (non-whitespace text)
    Text(&'a str),
}

/// Iterator that splits text into words, whitespace, and newlines.
struct WordIter<'a> {
    remaining: &'a str,
}

impl<'a> WordIter<'a> {
    fn new(s: &'a str) -> Self {
        Self { remaining: s }
    }
}

impl<'a> Iterator for WordIter<'a> {
    type Item = Word<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.remaining.is_empty() {
            return None;
        }

        // Check for newline
        if self.remaining.starts_with('\n') {
            self.remaining = &self.remaining[1..];
            return Some(Word::Newline);
        }

        // Check for whitespace run
        let ws_end = self
            .remaining
            .find(|c: char| c == '\n' || !c.is_whitespace())
            .unwrap_or(self.remaining.len());

        if ws_end > 0 {
            let ws = &self.remaining[..ws_end];
            self.remaining = &self.remaining[ws_end..];
            return Some(Word::Whitespace(ws));
        }

        // Find end of word (next whitespace or newline)
        let word_end = self
            .remaining
            .find(|c: char| c.is_whitespace())
            .unwrap_or(self.remaining.len());

        let word = &self.remaining[..word_end];
        self.remaining = &self.remaining[word_end..];
        Some(Word::Text(word))
    }
}

// Embed the Hack font at compile time
const FONT_DATA: &[u8] = include_bytes!("../fonts/Hack-Regular.ttf");

// Shift key codes
const KEY_LEFTSHIFT: u16 = 42;
const KEY_RIGHTSHIFT: u16 = 54;
const KEY_ENTER: u16 = 28;
const KEY_BACKSPACE: u16 = 14;

/// Key event value (press/release/repeat)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum KeyValue {
    Release,
    Press,
    Repeat,
}

impl KeyValue {
    fn from_u32(value: u32) -> Self {
        match value {
            0 => KeyValue::Release,
            1 => KeyValue::Press,
            2 => KeyValue::Repeat,
            _ => KeyValue::Release,
        }
    }
}

/// Pending input request state
struct PendingInput {
    /// Request ID for correlation
    id: u32,
    /// Type of input requested
    kind: InputKind,
    /// Handle to send response to
    handle: Handle,
    /// Buffer for line input
    buffer: String,
}

/// Terminal state
struct Terminal {
    surface: Handle,
    keyboard: Handle,
    mailbox: Mailbox,
    font: Font,
    width: u32,
    height: u32,
    cursor_x: u32,
    cursor_y: u32,
    line_buffer: String,
    /// Currently running child process (if any)
    child: Option<Handle>,
    /// Pending input request from child
    pending_input: Option<PendingInput>,
    /// Current foreground colour
    current_fg: u32,
    /// Average character width for grid-based calculations (terminal size, cursor positioning)
    avg_char_width: u32,
}

impl Terminal {
    fn new(
        surface: Handle,
        keyboard: Handle,
        mailbox: Mailbox,
        font: Font,
        width: u32,
        height: u32,
    ) -> Self {
        // Measure average character width using 'M' (a wide character)
        let (metrics, _) = font.rasterize('M', FONT_SIZE);
        let avg_char_width = metrics.advance_width as u32;

        Self {
            surface,
            keyboard,
            mailbox,
            font,
            width,
            height,
            cursor_x: MARGIN,
            cursor_y: MARGIN,
            line_buffer: String::new(),
            child: None,
            pending_input: None,
            current_fg: COLOUR_DEFAULT_FG,
            avg_char_width,
        }
    }

    /// Measure the pixel width of a string using actual font metrics
    fn measure_text(&self, text: &str) -> u32 {
        let mut width = 0u32;
        for ch in text.chars() {
            let (metrics, _) = self.font.rasterize(ch, FONT_SIZE);
            width += metrics.advance_width as u32;
        }
        width
    }

    /// Measure the pixel width of a single character
    fn measure_char(&self, ch: char) -> u32 {
        let (metrics, _) = self.font.rasterize(ch, FONT_SIZE);
        metrics.advance_width as u32
    }

    /// Clear the screen
    fn clear(&mut self) {
        let fill_params = FillParams {
            x: 0,
            y: 0,
            width: self.width,
            height: self.height,
            color: COLOUR_BACKGROUND,
        };
        send(
            self.surface,
            OP_SURFACE_FILL,
            &fill_params as *const FillParams as usize,
            0,
            0,
            0,
        );
        self.flush();
        self.cursor_x = MARGIN;
        self.cursor_y = MARGIN;
    }

    /// Clear a specific region
    fn clear_region(&mut self, region: ClearRegion) {
        match region {
            ClearRegion::Screen => self.clear(),
            ClearRegion::ToEndOfScreen => {
                // Clear from cursor to end of current line
                self.clear_to_end_of_line();
                // Clear remaining lines
                let y_start = self.cursor_y + LINE_HEIGHT;
                if y_start < self.height - MARGIN {
                    let fill_params = FillParams {
                        x: MARGIN,
                        y: y_start,
                        width: self.width - 2 * MARGIN,
                        height: self.height - y_start - MARGIN,
                        color: COLOUR_BACKGROUND,
                    };
                    send(
                        self.surface,
                        OP_SURFACE_FILL,
                        &fill_params as *const FillParams as usize,
                        0,
                        0,
                        0,
                    );
                }
            }
            ClearRegion::ToEndOfLine => self.clear_to_end_of_line(),
            ClearRegion::Line => {
                let fill_params = FillParams {
                    x: MARGIN,
                    y: self.cursor_y,
                    width: self.width - 2 * MARGIN,
                    height: LINE_HEIGHT,
                    color: COLOUR_BACKGROUND,
                };
                send(
                    self.surface,
                    OP_SURFACE_FILL,
                    &fill_params as *const FillParams as usize,
                    0,
                    0,
                    0,
                );
                self.cursor_x = MARGIN;
            }
        }
        self.flush();
    }

    fn clear_to_end_of_line(&mut self) {
        let fill_params = FillParams {
            x: self.cursor_x,
            y: self.cursor_y,
            width: self.width - self.cursor_x - MARGIN,
            height: LINE_HEIGHT,
            color: COLOUR_BACKGROUND,
        };
        send(
            self.surface,
            OP_SURFACE_FILL,
            &fill_params as *const FillParams as usize,
            0,
            0,
            0,
        );
    }

    /// Convert a Colour to ARGB u32
    fn colour_to_argb(colour: &Colour) -> u32 {
        match colour {
            Colour::Named(named) => match named {
                NamedColour::Black => 0xFF000000,
                NamedColour::Red => 0xFFCD3131,
                NamedColour::Green => 0xFF0DBC79,
                NamedColour::Yellow => 0xFFE5E510,
                NamedColour::Blue => 0xFF2472C8,
                NamedColour::Magenta => 0xFFBC3FBC,
                NamedColour::Cyan => 0xFF11A8CD,
                NamedColour::White => 0xFFE5E5E5,
                NamedColour::BrightBlack => 0xFF666666,
                NamedColour::BrightRed => 0xFFF14C4C,
                NamedColour::BrightGreen => 0xFF23D18B,
                NamedColour::BrightYellow => 0xFFF5F543,
                NamedColour::BrightBlue => 0xFF3B8EEA,
                NamedColour::BrightMagenta => 0xFFD670D6,
                NamedColour::BrightCyan => 0xFF29B8DB,
                NamedColour::BrightWhite => 0xFFFFFFFF,
            },
            Colour::Palette(idx) => {
                // Basic 256-colour palette approximation
                if *idx < 16 {
                    // Use named colours for first 16
                    let named = match idx {
                        0 => NamedColour::Black,
                        1 => NamedColour::Red,
                        2 => NamedColour::Green,
                        3 => NamedColour::Yellow,
                        4 => NamedColour::Blue,
                        5 => NamedColour::Magenta,
                        6 => NamedColour::Cyan,
                        7 => NamedColour::White,
                        8 => NamedColour::BrightBlack,
                        9 => NamedColour::BrightRed,
                        10 => NamedColour::BrightGreen,
                        11 => NamedColour::BrightYellow,
                        12 => NamedColour::BrightBlue,
                        13 => NamedColour::BrightMagenta,
                        14 => NamedColour::BrightCyan,
                        _ => NamedColour::BrightWhite,
                    };
                    Self::colour_to_argb(&Colour::Named(named))
                } else if *idx < 232 {
                    // 216 colour cube (6x6x6)
                    let idx = idx - 16;
                    let r = (idx / 36) * 51;
                    let g = ((idx / 6) % 6) * 51;
                    let b = (idx % 6) * 51;
                    0xFF000000 | ((r as u32) << 16) | ((g as u32) << 8) | (b as u32)
                } else {
                    // Grayscale (24 shades)
                    let grey = (idx - 232) * 10 + 8;
                    0xFF000000 | ((grey as u32) << 16) | ((grey as u32) << 8) | (grey as u32)
                }
            }
            Colour::Rgb { r, g, b } => {
                0xFF000000 | ((*r as u32) << 16) | ((*g as u32) << 8) | (*b as u32)
            }
        }
    }

    /// Draw a single character at current cursor position with colour
    fn draw_char_coloured(
        &mut self,
        ch: char,
        fg: u32,
        _bg: Option<u32>,
    ) -> Result<(), &'static str> {
        let (metrics, bitmap) = self.font.rasterize(ch, FONT_SIZE);

        if metrics.width == 0 || metrics.height == 0 {
            // Space or non-visible character - just advance cursor
            self.cursor_x += metrics.advance_width as u32;
            return Ok(());
        }

        let glyph_width = metrics.width;
        let glyph_height = metrics.height;
        let buffer_size = (glyph_width * glyph_height * 4) as usize;

        let Some(mut glyph_buffer) = Buffer::alloc(buffer_size) else {
            return Err("Failed to allocate glyph buffer");
        };

        // Extract RGB from foreground colour
        let fg_r = ((fg >> 16) & 0xFF) as u8;
        let fg_g = ((fg >> 8) & 0xFF) as u8;
        let fg_b = (fg & 0xFF) as u8;

        // Convert grayscale bitmap to ARGB with colour
        let pixels = glyph_buffer.as_mut_slice();
        for py in 0..glyph_height {
            for px in 0..glyph_width {
                let src_idx = py * glyph_width + px;
                let dst_idx = (py * glyph_width + px) * 4;
                let alpha = bitmap[src_idx];

                // Write BGRA (little-endian ARGB) with foreground colour
                pixels[dst_idx] = fg_b;
                pixels[dst_idx + 1] = fg_g;
                pixels[dst_idx + 2] = fg_r;
                pixels[dst_idx + 3] = alpha;
            }
        }

        // Calculate position
        let glyph_x = self.cursor_x + metrics.xmin as u32;
        let glyph_y =
            self.cursor_y + (FONT_SIZE as i32 - metrics.height as i32 - metrics.ymin) as u32;

        // Blit to surface
        let blit_params = BlitParams {
            x: glyph_x,
            y: glyph_y,
            width: glyph_width as u32,
            height: glyph_height as u32,
            buffer_handle: glyph_buffer.handle().as_raw(),
        };

        send(
            self.surface,
            OP_SURFACE_BLIT,
            &blit_params as *const BlitParams as usize,
            0,
            0,
            0,
        );

        // Advance cursor
        self.cursor_x += metrics.advance_width as u32;
        Ok(())
    }

    /// Draw a single character at current cursor position (default colour)
    fn draw_char(&mut self, ch: char) -> Result<(), &'static str> {
        self.draw_char_coloured(ch, self.current_fg, None)
    }

    /// Handle a newline
    fn newline(&mut self) {
        self.cursor_x = MARGIN;
        self.cursor_y += LINE_HEIGHT;

        // Simple scrolling: if we go off-screen, clear and start over
        if self.cursor_y + LINE_HEIGHT > self.height - MARGIN {
            self.clear();
        }
    }

    /// Handle backspace, erasing the given character width
    fn backspace_width(&mut self, char_width: u32) {
        if self.cursor_x > MARGIN {
            // Erase by drawing a rectangle over the previous character
            self.cursor_x = self.cursor_x.saturating_sub(char_width);

            let fill_params = FillParams {
                x: self.cursor_x,
                y: self.cursor_y,
                width: char_width,
                height: LINE_HEIGHT,
                color: COLOUR_BACKGROUND,
            };
            send(
                self.surface,
                OP_SURFACE_FILL,
                &fill_params as *const FillParams as usize,
                0,
                0,
                0,
            );
        }
    }

    /// Flush the surface to display
    fn flush(&self) {
        send(self.surface, OP_SURFACE_FLUSH, 0, 0, 0, 0);
    }

    /// Handle a typed character (when not in input mode from child)
    fn handle_char(&mut self, ch: char) {
        // Check if we need to wrap
        let char_width = self.measure_char(ch);
        if self.cursor_x + char_width > self.width - MARGIN {
            self.newline();
        }

        if let Ok(()) = self.draw_char(ch) {
            self.line_buffer.push(ch);
        }
        self.flush();
    }

    /// Write a string to the terminal with default colour
    fn write_str(&mut self, s: &str) {
        self.write_str_coloured(s, COLOUR_DEFAULT_FG);
    }

    /// Write a string with specific colour, wrapping at word boundaries
    fn write_str_coloured(&mut self, s: &str, colour: u32) {
        let max_x = self.width - MARGIN;

        for word in WordIter::new(s) {
            match word {
                Word::Newline => self.newline(),
                Word::Whitespace(ws) => {
                    for ch in ws.chars() {
                        let char_width = self.measure_char(ch);
                        if self.cursor_x + char_width > max_x {
                            self.newline();
                        }
                        let _ = self.draw_char_coloured(ch, colour, None);
                    }
                }
                Word::Text(text) => {
                    let word_width = self.measure_text(text);
                    // If word doesn't fit on current line and we're not at the start,
                    // move to next line first
                    if self.cursor_x > MARGIN && self.cursor_x + word_width > max_x {
                        self.newline();
                    }
                    // Now write the word, character by character (handles very long words)
                    for ch in text.chars() {
                        let char_width = self.measure_char(ch);
                        if self.cursor_x + char_width > max_x {
                            self.newline();
                        }
                        let _ = self.draw_char_coloured(ch, colour, None);
                    }
                }
            }
        }
        self.flush();
    }

    /// Write styled text
    fn write_styled(&mut self, styled: &StyledText) {
        let max_x = self.width - MARGIN;

        for span in &styled.spans {
            let fg = span
                .style
                .foreground
                .as_ref()
                .map(Self::colour_to_argb)
                .unwrap_or(COLOUR_DEFAULT_FG);

            // Check if this span is a "word" (no whitespace) - if so, try to keep it together
            let is_word = !span.text.chars().any(|c| c.is_whitespace());
            if is_word {
                let span_width = self.measure_text(&span.text);
                // If we're not at the start of a line and the span won't fit, wrap first
                if self.cursor_x > MARGIN && self.cursor_x + span_width > max_x {
                    self.newline();
                }
            }

            self.write_str_coloured(&span.text, fg);
        }
    }

    /// Write a line (string + newline) to the terminal
    fn write_line(&mut self, s: &str) {
        self.write_str(s);
        self.newline();
        self.flush();
    }

    /// Render a table
    fn render_table(&mut self, table: &Table) {
        // Calculate column widths
        let num_cols = table
            .headers
            .as_ref()
            .map(|h| h.len())
            .unwrap_or_else(|| table.rows.first().map(|r| r.len()).unwrap_or(0));

        if num_cols == 0 {
            return;
        }

        let mut col_widths = alloc::vec![0usize; num_cols];

        // Measure headers
        if let Some(ref headers) = table.headers {
            for (i, header) in headers.iter().enumerate() {
                let width: usize = header.spans.iter().map(|s| s.text.len()).sum();
                if i < col_widths.len() && width > col_widths[i] {
                    col_widths[i] = width;
                }
            }
        }

        // Measure rows
        for row in &table.rows {
            for (i, cell) in row.iter().enumerate() {
                let width: usize = cell.spans.iter().map(|s| s.text.len()).sum();
                if i < col_widths.len() && width > col_widths[i] {
                    col_widths[i] = width;
                }
            }
        }

        // Render headers
        if let Some(ref headers) = table.headers {
            for (i, header) in headers.iter().enumerate() {
                self.write_styled(header);
                if i < headers.len() - 1 {
                    // Pad to column width
                    let content_width: usize = header.spans.iter().map(|s| s.text.len()).sum();
                    let padding = col_widths
                        .get(i)
                        .unwrap_or(&0)
                        .saturating_sub(content_width)
                        + 2;
                    for _ in 0..padding {
                        let _ = self.draw_char(' ');
                    }
                }
            }
            self.newline();

            // Separator line
            let total_width: usize = col_widths.iter().sum::<usize>() + (num_cols - 1) * 2;
            for _ in 0..total_width {
                let _ = self.draw_char('-');
            }
            self.newline();
        }

        // Render rows
        for row in &table.rows {
            for (i, cell) in row.iter().enumerate() {
                self.write_styled(cell);
                if i < row.len() - 1 {
                    let content_width: usize = cell.spans.iter().map(|s| s.text.len()).sum();
                    let padding = col_widths
                        .get(i)
                        .unwrap_or(&0)
                        .saturating_sub(content_width)
                        + 2;
                    for _ in 0..padding {
                        let _ = self.draw_char(' ');
                    }
                }
            }
            self.newline();
        }

        self.flush();
    }

    /// Handle a terminal output message from child
    fn handle_terminal_output(&mut self, msg: TerminalOutput, child_handle: Handle) {
        match msg {
            TerminalOutput::Write(output) => match output {
                Output::Text(text) => self.write_str(&text),
                Output::Styled(styled) => self.write_styled(&styled),
                Output::Table(table) => self.render_table(&table),
                Output::KeyValue(pairs) => {
                    for (key, value) in &pairs {
                        self.write_styled(key);
                        self.write_str(": ");
                        self.write_styled(value);
                        self.newline();
                    }
                    self.flush();
                }
                Output::List(items) => {
                    for item in &items {
                        self.write_str("  - ");
                        self.write_styled(item);
                        self.newline();
                    }
                    self.flush();
                }
                Output::Bytes(data) => {
                    // Display as hex dump
                    self.write_str(&alloc::format!("<{} bytes>", data.len()));
                    self.newline();
                    self.flush();
                }
                Output::Link { text, url, .. } => {
                    // Just show text and URL
                    self.write_str(&text);
                    self.write_str(" (");
                    self.write_str(&url);
                    self.write_str(")");
                    self.flush();
                }
                Output::Json(json) => {
                    // Just write the JSON as-is for now
                    self.write_str(&json);
                    self.newline();
                    self.flush();
                }
            },
            TerminalOutput::MoveCursor { row, col } => {
                self.cursor_x = MARGIN + col as u32 * self.avg_char_width;
                self.cursor_y = MARGIN + row as u32 * LINE_HEIGHT;
            }
            TerminalOutput::Clear(region) => {
                self.clear_region(region);
            }
            TerminalOutput::RequestInput(req) => {
                // Display prompt if provided
                if let Some(ref prompt) = req.prompt {
                    self.write_styled(prompt);
                }

                // Store pending input state
                self.pending_input = Some(PendingInput {
                    id: req.id,
                    kind: req.kind,
                    handle: child_handle,
                    buffer: String::new(),
                });
            }
            TerminalOutput::SetTitle(title) => {
                // TODO: Set window title when supported
                let _ = title;
            }
            TerminalOutput::Progress {
                current,
                total,
                message,
            } => {
                // Simple progress display
                let percent = if total > 0 {
                    (current * 100) / total
                } else {
                    0
                };
                self.write_str(&alloc::format!("[{}%] {}", percent, message));
                self.newline();
                self.flush();
            }
            TerminalOutput::Query(query) => {
                let response = match query {
                    TerminalQuery::Size => {
                        let cols = (self.width - 2 * MARGIN) / self.avg_char_width;
                        let rows = (self.height - 2 * MARGIN) / LINE_HEIGHT;
                        QueryResponse::Size {
                            cols: cols as u16,
                            rows: rows as u16,
                        }
                    }
                    TerminalQuery::Capabilities => {
                        QueryResponse::Capabilities(TerminalCapabilities {
                            colours: ColourSupport::TrueColour,
                            images: false,
                            hyperlinks: false,
                            unicode: true,
                        })
                    }
                    TerminalQuery::CursorPosition => {
                        let col = (self.cursor_x - MARGIN) / self.avg_char_width;
                        let row = (self.cursor_y - MARGIN) / LINE_HEIGHT;
                        QueryResponse::CursorPosition {
                            row: row as u16,
                            col: col as u16,
                        }
                    }
                };

                let input_msg = TerminalInput::QueryResponse(response);
                let bytes = input_msg.to_bytes();
                let _ = channel::send(child_handle, &bytes);
            }
            TerminalOutput::Exit(_code) => {
                // Child is exiting via protocol - will also get ProcessExited event
            }
        }
    }

    /// Send input response to child
    fn send_input_response(&mut self, value: Option<InputValue>) {
        if let Some(pending) = self.pending_input.take() {
            let response = InputResponse {
                id: pending.id,
                value,
            };
            let msg = TerminalInput::Input(response);
            let bytes = msg.to_bytes();
            let _ = channel::send(pending.handle, &bytes);
        }
    }

    /// Handle a typed character when there's a pending input request
    fn handle_input_char(&mut self, ch: char) {
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
    fn handle_input_enter(&mut self) {
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
    fn handle_input_backspace(&mut self) {
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

    /// Parse the line buffer into command and arguments
    fn parse_command(&self) -> Option<(String, Vec<String>)> {
        let trimmed = self.line_buffer.trim();
        if trimmed.is_empty() {
            return None;
        }

        let parts: Vec<&str> = trimmed.split_whitespace().collect();
        if parts.is_empty() {
            return None;
        }

        let cmd = String::from(parts[0]);
        let args: Vec<String> = parts.iter().map(|s| String::from(*s)).collect();
        Some((cmd, args))
    }

    /// Resolve a command name to an executable path
    fn resolve_command(&self, cmd: &str) -> Option<String> {
        if cmd.contains('/') {
            return Some(alloc::format!("file:{}", cmd));
        }

        // Try /mnt first (ext2 filesystem)
        let mnt_path = alloc::format!("file:/mnt/{}", cmd);
        if environment::stat(&mnt_path).is_ok() {
            return Some(mnt_path);
        }

        // Try /initrd
        let initrd_path = alloc::format!("file:/initrd/{}", cmd);
        if environment::stat(&initrd_path).is_ok() {
            return Some(initrd_path);
        }

        None
    }

    /// Execute a command
    fn execute_command(&mut self) {
        let Some((cmd, args)) = self.parse_command() else {
            return;
        };

        // Handle built-in commands
        match cmd.as_str() {
            "clear" => {
                self.clear();
                return;
            }
            "exit" => {
                process::exit(0);
            }
            _ => {}
        }

        // Resolve command to executable path
        let Some(path) = self.resolve_command(&cmd) else {
            self.write_line(&alloc::format!("{}: command not found", cmd));
            return;
        };

        // Convert args to &str slice for spawn
        let arg_refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();

        // Spawn the process with mailbox attachment for events
        // We want both channel readable (for IPC) and process exited events
        let events = EVENT_PROCESS_EXITED | EVENT_CHANNEL_READABLE;
        match environment::spawn(&path, &arg_refs, self.mailbox.handle().as_raw(), events) {
            Ok(child_handle) => {
                self.child = Some(child_handle);
            }
            Err(_) => {
                self.write_line(&alloc::format!("{}: failed to execute", cmd));
            }
        }
    }

    /// Handle child process exit
    fn handle_child_exit(&mut self, handle: Handle) {
        if let Some(child) = self.child.take() {
            if child.as_raw() == handle.as_raw() {
                let exit_code = process::wait(child);
                if exit_code != 0 {
                    self.write_line(&alloc::format!("(exited with code {})", exit_code));
                }
                // Clear any pending input state
                self.pending_input = None;
            }
        }
    }

    /// Process channel messages from child
    fn process_child_messages(&mut self, handle: Handle) {
        let mut buf = [0u8; MAX_MESSAGE_SIZE];

        loop {
            match channel::try_recv(handle, &mut buf) {
                Ok(len) if len > 0 => {
                    if let Ok((msg, _)) = TerminalOutput::from_bytes(&buf[..len]) {
                        self.handle_terminal_output(msg, handle);
                    }
                }
                _ => break,
            }
        }
    }

    /// Handle Enter key
    fn handle_enter(&mut self) {
        // If there's pending input from child, handle that
        if self.pending_input.is_some() {
            self.handle_input_enter();
            return;
        }

        self.newline();

        if !self.line_buffer.trim().is_empty() {
            self.execute_command();
        }

        self.line_buffer.clear();
        self.flush();
    }

    /// Handle Backspace key
    fn handle_backspace(&mut self) {
        // If there's pending input from child, handle that
        if self.pending_input.is_some() {
            self.handle_input_backspace();
            return;
        }

        if !self.line_buffer.is_empty() {
            if let Some(ch) = self.line_buffer.pop() {
                let char_width = self.measure_char(ch);
                self.backspace_width(char_width);
                self.flush();
            }
        }
    }
}

/// Convert Linux keycode to ASCII character
fn keycode_to_char(code: u16, shift: bool) -> Option<char> {
    match code {
        // Letters
        30 => Some(if shift { 'A' } else { 'a' }),
        48 => Some(if shift { 'B' } else { 'b' }),
        46 => Some(if shift { 'C' } else { 'c' }),
        32 => Some(if shift { 'D' } else { 'd' }),
        18 => Some(if shift { 'E' } else { 'e' }),
        33 => Some(if shift { 'F' } else { 'f' }),
        34 => Some(if shift { 'G' } else { 'g' }),
        35 => Some(if shift { 'H' } else { 'h' }),
        23 => Some(if shift { 'I' } else { 'i' }),
        36 => Some(if shift { 'J' } else { 'j' }),
        37 => Some(if shift { 'K' } else { 'k' }),
        38 => Some(if shift { 'L' } else { 'l' }),
        50 => Some(if shift { 'M' } else { 'm' }),
        49 => Some(if shift { 'N' } else { 'n' }),
        24 => Some(if shift { 'O' } else { 'o' }),
        25 => Some(if shift { 'P' } else { 'p' }),
        16 => Some(if shift { 'Q' } else { 'q' }),
        19 => Some(if shift { 'R' } else { 'r' }),
        31 => Some(if shift { 'S' } else { 's' }),
        20 => Some(if shift { 'T' } else { 't' }),
        22 => Some(if shift { 'U' } else { 'u' }),
        47 => Some(if shift { 'V' } else { 'v' }),
        17 => Some(if shift { 'W' } else { 'w' }),
        45 => Some(if shift { 'X' } else { 'x' }),
        21 => Some(if shift { 'Y' } else { 'y' }),
        44 => Some(if shift { 'Z' } else { 'z' }),

        // Numbers
        11 => Some(if shift { '!' } else { '1' }),
        2 => Some(if shift { '@' } else { '2' }),
        3 => Some(if shift { '#' } else { '3' }),
        4 => Some(if shift { '$' } else { '4' }),
        5 => Some(if shift { '%' } else { '5' }),
        6 => Some(if shift { '^' } else { '6' }),
        7 => Some(if shift { '&' } else { '7' }),
        8 => Some(if shift { '*' } else { '8' }),
        9 => Some(if shift { '(' } else { '9' }),
        10 => Some(if shift { ')' } else { '0' }),

        // Symbols
        57 => Some(' '), // Space
        12 => Some(if shift { '_' } else { '-' }),
        13 => Some(if shift { '+' } else { '=' }),
        26 => Some(if shift { '{' } else { '[' }),
        27 => Some(if shift { '}' } else { ']' }),
        39 => Some(if shift { ':' } else { ';' }),
        40 => Some(if shift { '"' } else { '\'' }),
        41 => Some(if shift { '~' } else { '`' }),
        43 => Some(if shift { '|' } else { '\\' }),
        51 => Some(if shift { '<' } else { ',' }),
        52 => Some(if shift { '>' } else { '.' }),
        53 => Some(if shift { '?' } else { '/' }),

        _ => None,
    }
}

/// Input event structure (matches kernel's InputEvent)
#[repr(C)]
struct InputEvent {
    event_type: u16,
    code: u16,
    value: u32,
}

/// Handle a key event
fn handle_key_event(term: &mut Terminal, code: u16, value: KeyValue, shift_pressed: &mut bool) {
    match value {
        KeyValue::Press | KeyValue::Repeat => {
            // Track shift state
            if code == KEY_LEFTSHIFT || code == KEY_RIGHTSHIFT {
                *shift_pressed = true;
                return;
            }

            // Handle special keys
            match code {
                KEY_ENTER => term.handle_enter(),
                KEY_BACKSPACE => term.handle_backspace(),
                _ => {
                    // Try to convert to character
                    if let Some(ch) = keycode_to_char(code, *shift_pressed) {
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
            if code == KEY_LEFTSHIFT || code == KEY_RIGHTSHIFT {
                *shift_pressed = false;
            }
        }
    }
}

/// Process any pending keyboard events
fn process_keyboard_events(term: &mut Terminal, shift_pressed: &mut bool) {
    let mut buf = [0u8; 8]; // InputEvent is 8 bytes

    loop {
        let n = file::try_read(term.keyboard, &mut buf);
        if n <= 0 {
            break;
        }

        if n >= 8 {
            let event = unsafe { &*(buf.as_ptr() as *const InputEvent) };
            let value = KeyValue::from_u32(event.value);
            handle_key_event(term, event.code, value, shift_pressed);
        }
    }
}

libpanda::main! {
    environment::log("terminal: Starting");

    let font = Font::from_bytes(FONT_DATA, FontSettings::default())
        .expect("Failed to load font");

    let mailbox = Mailbox::default();

    let Ok(surface) = environment::open("surface:/window", 0, 0) else {
        environment::log("terminal: Failed to open window");
        return 1;
    };

    let window_width = 800u32;
    let window_height = 600u32;

    let window_params = UpdateParamsIn {
        x: 50,
        y: 50,
        width: window_width,
        height: window_height,
        visible: 1,
    };

    send(
        surface,
        OP_SURFACE_UPDATE_PARAMS,
        &window_params as *const UpdateParamsIn as usize,
        0,
        0,
        0,
    );

    let Ok(keyboard) = environment::open(
        "keyboard:/pci/00:03.0",
        mailbox.handle().as_raw(),
        EVENT_KEYBOARD_KEY,
    ) else {
        environment::log("terminal: Failed to open keyboard");
        return 1;
    };

    let mut term = Terminal::new(surface, keyboard, mailbox, font, window_width, window_height);
    term.clear();

    term.write_line("Panda OS Terminal");
    term.write_line("Type 'help' for available commands.");
    term.write_str("> ");

    let mut shift_pressed = false;

    loop {
        let (handle, event) = term.mailbox.recv();

        match event {
            Event::KeyboardReady => {
                process_keyboard_events(&mut term, &mut shift_pressed);
            }
            Event::ChannelReadable => {
                // Child process sent a message
                term.process_child_messages(handle);
            }
            Event::ProcessExited => {
                term.handle_child_exit(handle);
                term.write_str("> ");
            }
            _ => {}
        }
    }
}
