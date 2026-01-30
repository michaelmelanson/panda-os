#![no_std]
#![no_main]

extern crate alloc;
extern crate panda_abi;

mod commands;
mod input;
mod render;

use alloc::string::String;
use alloc::vec::Vec;
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
        ClearRegion, ColourSupport, Event as TerminalEvent, QueryResponse, Request,
        TerminalCapabilities, TerminalQuery,
    },
    value::Value,
    BlitParams, UpdateParamsIn, OP_SURFACE_BLIT, OP_SURFACE_FLUSH, OP_SURFACE_UPDATE_PARAMS,
};

use crate::input::PendingInput;
use crate::render::{colour_to_argb, Word, WordIter};

fn rdtsc() -> u64 {
    let lo: u32;
    let hi: u32;
    unsafe {
        core::arch::asm!("rdtsc", out("eax") lo, out("edx") hi, options(nomem, nostack));
    }
    ((hi as u64) << 32) | lo as u64
}

// Terminal colours (ARGB format)
const COLOUR_BACKGROUND: u32 = 0xFF1E1E1E; // Dark grey
const COLOUR_DEFAULT_FG: u32 = 0xFFD4D4D4; // Light grey

const BG_B: u8 = (COLOUR_BACKGROUND & 0xFF) as u8;
const BG_G: u8 = ((COLOUR_BACKGROUND >> 8) & 0xFF) as u8;
const BG_R: u8 = ((COLOUR_BACKGROUND >> 16) & 0xFF) as u8;

const MARGIN: u32 = 10;
const FONT_SIZE: f32 = 16.0;
const LINE_HEIGHT: u32 = 20; // Font size + spacing

// Embed the Hack font at compile time
const FONT_DATA: &[u8] = include_bytes!("../fonts/Hack-Regular.ttf");

/// Dirty rectangle tracking for batched blits.
#[derive(Clone, Copy)]
struct DirtyRect {
    x0: u32,
    y0: u32,
    x1: u32,
    y1: u32,
}

impl DirtyRect {
    fn expand(&mut self, x: u32, y: u32, w: u32, h: u32) {
        self.x0 = self.x0.min(x);
        self.y0 = self.y0.min(y);
        self.x1 = self.x1.max(x + w);
        self.y1 = self.y1.max(y + h);
    }
}

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
    /// Currently running child process (last stage of pipeline, or single command)
    pub child: Option<Handle>,
    /// All children in a pipeline (for cleanup)
    pub pipeline_children: Vec<Handle>,
    /// Pending input request from child
    pub pending_input: Option<PendingInput>,
    /// Current foreground colour
    current_fg: u32,
    /// Average character width for grid-based calculations (terminal size, cursor positioning)
    avg_char_width: u32,
    /// Persistent framebuffer — all rendering composites into this buffer
    framebuffer: Buffer,
    /// Dirty region tracking for batched blits
    dirty: Option<DirtyRect>,
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

        // Allocate persistent framebuffer for the entire window surface
        let fb_size = (width * height * 4) as usize;
        let framebuffer = Buffer::alloc(fb_size).expect("Failed to allocate terminal framebuffer");

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
            pipeline_children: Vec::new(),
            pending_input: None,
            current_fg: COLOUR_DEFAULT_FG,
            avg_char_width,
            framebuffer,
            dirty: None,
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
        self.fb_fill(0, 0, self.width, self.height, COLOUR_BACKGROUND);
        self.flush();
        self.cursor_x = MARGIN;
        self.cursor_y = MARGIN;
    }

    /// Clear a specific region
    fn clear_region(&mut self, region: ClearRegion) {
        match region {
            ClearRegion::Screen => self.clear(),
            ClearRegion::ToEndOfScreen => {
                self.clear_to_end_of_line();
                let y_start = self.cursor_y + LINE_HEIGHT;
                if y_start < self.height - MARGIN {
                    self.fb_fill(
                        MARGIN,
                        y_start,
                        self.width - 2 * MARGIN,
                        self.height - y_start - MARGIN,
                        COLOUR_BACKGROUND,
                    );
                }
            }
            ClearRegion::ToEndOfLine => self.clear_to_end_of_line(),
            ClearRegion::Line => {
                self.fb_fill(
                    MARGIN,
                    self.cursor_y,
                    self.width - 2 * MARGIN,
                    LINE_HEIGHT,
                    COLOUR_BACKGROUND,
                );
                self.cursor_x = MARGIN;
            }
        }
        self.flush();
    }

    fn clear_to_end_of_line(&mut self) {
        let w = self.width - self.cursor_x - MARGIN;
        self.fb_fill(
            self.cursor_x,
            self.cursor_y,
            w,
            LINE_HEIGHT,
            COLOUR_BACKGROUND,
        );
    }

    /// Mark a region as dirty, expanding the existing dirty rect or creating a new one.
    fn mark_dirty(&mut self, x: u32, y: u32, w: u32, h: u32) {
        // Clamp to framebuffer bounds
        let x1 = (x + w).min(self.width);
        let y1 = (y + h).min(self.height);
        match self.dirty {
            Some(ref mut d) => d.expand(x, y, x1 - x, y1 - y),
            None => {
                self.dirty = Some(DirtyRect {
                    x0: x,
                    y0: y,
                    x1,
                    y1,
                });
            }
        }
    }

    /// Fill a rectangle in the framebuffer with a solid ARGB colour.
    fn fb_fill(&mut self, x: u32, y: u32, w: u32, h: u32, colour: u32) {
        let fb = self.framebuffer.as_mut_slice();
        let stride = self.width;
        let b = (colour & 0xFF) as u8;
        let g = ((colour >> 8) & 0xFF) as u8;
        let r = ((colour >> 16) & 0xFF) as u8;
        let a = ((colour >> 24) & 0xFF) as u8;

        let x_end = (x + w).min(self.width);
        let y_end = (y + h).min(self.height);

        for row in y..y_end {
            for col in x..x_end {
                let off = ((row * stride + col) * 4) as usize;
                fb[off] = b;
                fb[off + 1] = g;
                fb[off + 2] = r;
                fb[off + 3] = a;
            }
        }
        self.mark_dirty(x, y, x_end - x, y_end - y);
    }

    /// Draw a single character at current cursor position with colour.
    ///
    /// Composites the glyph directly into the framebuffer — no syscalls.
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

        // Extract RGB from foreground colour
        let fg_r = ((fg >> 16) & 0xFF) as u16;
        let fg_g = ((fg >> 8) & 0xFF) as u16;
        let fg_b = (fg & 0xFF) as u16;

        // Calculate position in framebuffer
        let glyph_x = self.cursor_x + metrics.xmin as u32;
        let glyph_y =
            self.cursor_y + (FONT_SIZE as i32 - metrics.height as i32 - metrics.ymin) as u32;

        // Composite glyph into framebuffer with alpha blending
        let fb = self.framebuffer.as_mut_slice();
        let stride = self.width;

        for py in 0..glyph_height {
            let dst_y = glyph_y + py as u32;
            if dst_y >= self.height {
                break;
            }
            for px in 0..glyph_width {
                let dst_x = glyph_x + px as u32;
                if dst_x >= self.width {
                    break;
                }

                let alpha = bitmap[py * glyph_width + px] as u16;
                if alpha == 0 {
                    continue;
                }

                let off = ((dst_y * stride + dst_x) * 4) as usize;

                if alpha == 255 {
                    // Fully opaque — direct write
                    fb[off] = fg_b as u8;
                    fb[off + 1] = fg_g as u8;
                    fb[off + 2] = fg_r as u8;
                    fb[off + 3] = 255;
                } else {
                    // Alpha blend over background
                    let inv = 255 - alpha;
                    fb[off] = ((fg_b * alpha + BG_B as u16 * inv) / 255) as u8;
                    fb[off + 1] = ((fg_g * alpha + BG_G as u16 * inv) / 255) as u8;
                    fb[off + 2] = ((fg_r * alpha + BG_R as u16 * inv) / 255) as u8;
                    fb[off + 3] = 255;
                }
            }
        }

        self.mark_dirty(glyph_x, glyph_y, glyph_width as u32, glyph_height as u32);

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
            self.cursor_x = self.cursor_x.saturating_sub(char_width);
            self.fb_fill(
                self.cursor_x,
                self.cursor_y,
                char_width,
                LINE_HEIGHT,
                COLOUR_BACKGROUND,
            );
        }
    }

    /// Flush the surface to display.
    ///
    /// Blits the framebuffer to the window surface (if dirty), then
    /// asks the compositor to present the frame.
    pub fn flush(&mut self) {
        if self.dirty.is_some() {
            // Blit the full framebuffer to the window surface.
            // All pixels are fully opaque so the kernel uses the fast copy path.
            let blit_params = BlitParams {
                x: 0,
                y: 0,
                width: self.width,
                height: self.height,
                buffer_handle: self.framebuffer.handle().as_raw(),
            };
            send(
                self.surface,
                OP_SURFACE_BLIT,
                &blit_params as *const BlitParams as usize,
                0,
                0,
                0,
            );
            self.dirty = None;
        }
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

    /// Write a line (string + newline) to the terminal
    pub fn write_line(&mut self, s: &str) {
        self.write_str(s);
        self.newline();
        self.flush();
    }

    /// Render a Value object (for structured pipeline data)
    fn render_value(&mut self, value: &panda_abi::value::Value) {
        use panda_abi::value::Value;
        match value {
            Value::Null => self.write_str("null"),
            Value::Bool(b) => self.write_str(if *b { "true" } else { "false" }),
            Value::Int(i) => self.write_str(&alloc::format!("{}", i)),
            Value::Float(f) => self.write_str(&alloc::format!("{}", f)),
            Value::String(s) => self.write_str(s),
            Value::Bytes(b) => self.write_str(&alloc::format!("<{} bytes>", b.len())),
            Value::Array(arr) => {
                for (i, item) in arr.iter().enumerate() {
                    if i > 0 {
                        self.newline();
                    }
                    self.write_str("  - ");
                    self.render_value(item);
                }
            }
            Value::Map(map) => {
                for (i, (k, v)) in map.iter().enumerate() {
                    if i > 0 {
                        self.newline();
                    }
                    self.write_str(k);
                    self.write_str(": ");
                    self.render_value(v);
                }
            }
            Value::Styled(style, inner) => {
                let fg = style
                    .foreground
                    .as_ref()
                    .map(colour_to_argb)
                    .unwrap_or(COLOUR_DEFAULT_FG);
                // For styled values, we need to render the inner value with the style
                // For simplicity, only handle String for now
                if let Value::String(s) = inner.as_ref() {
                    self.write_str_coloured(s, fg);
                } else {
                    // Fall back to rendering without style
                    self.render_value(inner);
                }
            }
            Value::Link { url, inner } => {
                self.render_value(inner);
                self.write_str(" (");
                self.write_str(url);
                self.write_str(")");
            }
            Value::Table(table) => {
                self.render_value_table(table);
            }
        }
    }

    /// Render a Value::Table with proper column width calculation
    fn render_value_table(&mut self, table: &panda_abi::value::Table) {
        let t0 = rdtsc();
        let num_cols = table.cols as usize;
        if num_cols == 0 {
            return;
        }

        // Calculate column widths by measuring all values
        let mut col_widths = alloc::vec![0usize; num_cols];

        // Measure headers
        if let Some(ref headers) = table.headers {
            for (i, header) in headers.iter().enumerate() {
                if i < num_cols {
                    let width = self.measure_value_width(header);
                    if width > col_widths[i] {
                        col_widths[i] = width;
                    }
                }
            }
        }

        // Measure all cells
        for row in table.row_iter() {
            for (i, cell) in row.iter().enumerate() {
                if i < num_cols {
                    let width = self.measure_value_width(cell);
                    if width > col_widths[i] {
                        col_widths[i] = width;
                    }
                }
            }
        }

        let t1 = rdtsc();

        // Render headers if present
        if let Some(ref headers) = table.headers {
            for (i, header) in headers.iter().enumerate() {
                if i > 0 {
                    self.write_str("  ");
                }
                self.render_value(header);
                // Pad to column width
                if i < headers.len() - 1 {
                    let content_width = self.measure_value_width(header);
                    let padding = col_widths
                        .get(i)
                        .unwrap_or(&0)
                        .saturating_sub(content_width);
                    for _ in 0..padding {
                        let _ = self.draw_char(' ');
                    }
                }
            }
            self.newline();

            // Separator line
            let total_width: usize =
                col_widths.iter().sum::<usize>() + (num_cols.saturating_sub(1)) * 2;
            for _ in 0..total_width {
                let _ = self.draw_char('-');
            }
            self.newline();
        }

        // Render rows
        for row in table.row_iter() {
            let row_vec: alloc::vec::Vec<_> = row.iter().collect();
            for (i, cell) in row_vec.iter().enumerate() {
                if i > 0 {
                    self.write_str("  ");
                }
                self.render_value(cell);
                // Pad to column width (except last column)
                if i < row_vec.len() - 1 {
                    let content_width = self.measure_value_width(cell);
                    let padding = col_widths
                        .get(i)
                        .unwrap_or(&0)
                        .saturating_sub(content_width);
                    for _ in 0..padding {
                        let _ = self.draw_char(' ');
                    }
                }
            }
            self.newline();
        }
        self.flush();

        let t2 = rdtsc();
        environment::log(&alloc::format!(
            "[terminal table timing] measure={} render={}",
            t1 - t0,
            t2 - t1
        ));
    }

    /// Measure the display width of a Value in characters
    fn measure_value_width(&self, value: &Value) -> usize {
        match value {
            Value::Null => 4, // "null"
            Value::Bool(b) => {
                if *b {
                    4
                } else {
                    5
                }
            } // "true" or "false"
            Value::Int(n) => {
                // Count digits (including sign)
                let mut v = *n;
                let mut count = if v < 0 { 1 } else { 0 }; // account for minus sign
                if v == 0 {
                    return 1;
                }
                if v < 0 {
                    v = v.wrapping_neg();
                }
                while v > 0 {
                    count += 1;
                    v /= 10;
                }
                count
            }
            Value::Float(f) => {
                // Approximate width for float display
                alloc::format!("{}", f).len()
            }
            Value::String(s) => s.len(),
            Value::Bytes(data) => {
                // "<N bytes>"
                alloc::format!("<{} bytes>", data.len()).len()
            }
            Value::Array(items) => {
                // [item, item, ...]
                if items.is_empty() {
                    2 // "[]"
                } else {
                    2 + items
                        .iter()
                        .map(|v| self.measure_value_width(v))
                        .sum::<usize>()
                        + (items.len() - 1) * 2 // ", " separators
                }
            }
            Value::Map(fields) => {
                // {key: value, ...}
                if fields.is_empty() {
                    2 // "{}"
                } else {
                    2 + fields
                        .iter()
                        .map(|(k, v)| k.len() + 2 + self.measure_value_width(v))
                        .sum::<usize>()
                        + (fields.len() - 1) * 2
                }
            }
            Value::Styled(_, inner) => {
                // Styled doesn't change display width
                self.measure_value_width(inner)
            }
            Value::Link { inner, .. } => {
                // Link text width
                self.measure_value_width(inner)
            }
            Value::Table(_) => 7, // "<table>" placeholder
        }
    }

    /// Handle a terminal request message from child
    pub fn handle_request(&mut self, msg: Request, child_handle: Handle) {
        match msg {
            Request::Error(value) => {
                // Display error in red
                self.write_str_coloured("[ERROR] ", 0xFFFF0000);
                self.render_value(&value);
                self.newline();
                self.flush();
            }
            Request::Warning(value) => {
                // Display warning in yellow
                self.write_str_coloured("[WARN] ", 0xFFFFFF00);
                self.render_value(&value);
                self.newline();
                self.flush();
            }
            Request::Write(value) => {
                // Render the Value
                self.render_value(&value);
                self.flush();
            }
            Request::MoveCursor { row, col } => {
                self.cursor_x = MARGIN + col as u32 * self.avg_char_width;
                self.cursor_y = MARGIN + row as u32 * LINE_HEIGHT;
            }
            Request::Clear(region) => {
                self.clear_region(region);
            }
            Request::RequestInput(req) => {
                // Display prompt if provided
                if let Some(ref prompt) = req.prompt {
                    self.render_value(prompt);
                }

                // Store pending input state
                self.pending_input = Some(PendingInput {
                    id: req.id,
                    kind: req.kind,
                    handle: child_handle,
                    buffer: String::new(),
                });
            }
            Request::SetTitle(title) => {
                // TODO: Set window title when supported
                let _ = title;
            }
            Request::Progress {
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
            Request::Query(query) => {
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

                let event_msg = TerminalEvent::QueryResponse(response);
                let bytes = event_msg.to_bytes();
                let _ = channel::send(child_handle, &bytes);
            }
            Request::Exit(_code) => {
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

    // Set up initial environment variables
    libpanda::env::set("PATH", "/mnt:/initrd");
    libpanda::env::set("HOME", "/");
    libpanda::env::set("TERM", "panda");

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

    environment::log("terminal: creating Terminal");
    let mut term = Terminal::new(surface, keyboard, mailbox, font, window_width, window_height);
    environment::log("terminal: calling clear");
    term.clear();
    environment::log("terminal: clear done");

    term.write_line("Panda OS Terminal");
    environment::log("terminal: wrote first line");
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
