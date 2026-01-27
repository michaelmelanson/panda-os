#![no_std]
#![no_main]

extern crate alloc;
extern crate panda_abi;

mod commands;
mod input;
mod render;

use alloc::string::String;
use fontdue::{Font, FontSettings};
use libpanda::{
    buffer::Buffer,
    channel, environment,
    mailbox::{ChannelEvent, Event, InputEvent, Mailbox, ProcessEvent},
    sys::send,
    Handle,
};
use panda_abi::{
    terminal::{
        ClearRegion, ColourSupport, QueryResponse, StyledText, Table, TerminalCapabilities,
        TerminalInput, TerminalOutput, TerminalQuery,
    },
    BlitParams, FillParams, UpdateParamsIn, OP_SURFACE_BLIT, OP_SURFACE_FILL, OP_SURFACE_FLUSH,
    OP_SURFACE_UPDATE_PARAMS,
};

use crate::input::PendingInput;
use crate::render::{colour_to_argb, Word, WordIter};

// Terminal colours (ARGB format)
const COLOUR_BACKGROUND: u32 = 0xFF1E1E1E; // Dark grey
const COLOUR_DEFAULT_FG: u32 = 0xFFD4D4D4; // Light grey

const MARGIN: u32 = 10;
const FONT_SIZE: f32 = 16.0;
const LINE_HEIGHT: u32 = 20; // Font size + spacing

// Embed the Hack font at compile time
const FONT_DATA: &[u8] = include_bytes!("../fonts/Hack-Regular.ttf");

/// Terminal state
pub struct Terminal {
    pub surface: Handle,
    pub keyboard: Handle,
    pub mailbox: Mailbox,
    font: Font,
    width: u32,
    height: u32,
    cursor_x: u32,
    cursor_y: u32,
    pub line_buffer: String,
    /// Currently running child process (if any)
    pub child: Option<Handle>,
    /// Pending input request from child
    pub pending_input: Option<PendingInput>,
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
    pub fn measure_text(&self, text: &str) -> u32 {
        let mut width = 0u32;
        for ch in text.chars() {
            let (metrics, _) = self.font.rasterize(ch, FONT_SIZE);
            width += metrics.advance_width as u32;
        }
        width
    }

    /// Measure the pixel width of a single character
    pub fn measure_char(&self, ch: char) -> u32 {
        let (metrics, _) = self.font.rasterize(ch, FONT_SIZE);
        metrics.advance_width as u32
    }

    /// Clear the screen
    pub fn clear(&mut self) {
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

    /// Draw a single character at current cursor position with colour
    pub fn draw_char_coloured(
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

        // Convert grayscale bitmap to ARGB with foreground colour
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
    pub fn draw_char(&mut self, ch: char) -> Result<(), &'static str> {
        self.draw_char_coloured(ch, self.current_fg, None)
    }

    /// Handle a newline
    pub fn newline(&mut self) {
        self.cursor_x = MARGIN;
        self.cursor_y += LINE_HEIGHT;

        // Simple scrolling: if we go off-screen, clear and start over
        if self.cursor_y + LINE_HEIGHT > self.height - MARGIN {
            self.clear();
        }
    }

    /// Handle backspace, erasing the given character width
    pub fn backspace_width(&mut self, char_width: u32) {
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
    pub fn flush(&self) {
        send(self.surface, OP_SURFACE_FLUSH, 0, 0, 0, 0);
    }

    /// Handle a typed character (when not in input mode from child)
    pub fn handle_char(&mut self, ch: char) {
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
    pub fn write_str(&mut self, s: &str) {
        self.write_str_coloured(s, COLOUR_DEFAULT_FG);
    }

    /// Write a string with specific colour, wrapping at word boundaries
    pub fn write_str_coloured(&mut self, s: &str, colour: u32) {
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
                .map(colour_to_argb)
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
    pub fn write_line(&mut self, s: &str) {
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
    pub fn handle_terminal_output(&mut self, msg: TerminalOutput, child_handle: Handle) {
        match msg {
            TerminalOutput::Write(output) => {
                use panda_abi::terminal::Output;
                match output {
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
                }
            }
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

    /// Handle Enter key
    pub fn handle_enter(&mut self) {
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
    pub fn handle_backspace(&mut self) {
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
        panda_abi::EVENT_KEYBOARD_KEY,
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
        let (handle, events) = term.mailbox.recv();

        for event in events {
            match event {
                Event::Input(InputEvent::Keyboard) => {
                    input::process_keyboard_events(&mut term, &mut shift_pressed);
                }
                Event::Channel(ChannelEvent::Readable) => {
                    // Child process sent a message
                    term.process_child_messages(handle);
                }
                Event::Process(ProcessEvent::Exited) => {
                    term.handle_child_exit(handle);
                    term.write_str("> ");
                }
                _ => {}
            }
        }
    }
}
