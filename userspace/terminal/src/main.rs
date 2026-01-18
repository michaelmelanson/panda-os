#![no_std]
#![no_main]

extern crate alloc;
extern crate panda_abi;

use alloc::string::String;
use fontdue::{Font, FontSettings};
use libpanda::{buffer::Buffer, environment, file, syscall::send};
use panda_abi::{
    BlitParams, FillParams, PixelFormat, SurfaceInfoOut, UpdateParamsIn, OP_SURFACE_BLIT,
    OP_SURFACE_FILL, OP_SURFACE_FLUSH, OP_SURFACE_INFO, OP_SURFACE_UPDATE_PARAMS,
};

// Terminal colors (ARGB format)
const COLOR_BACKGROUND: u32 = 0xFF000000; // Black

const MARGIN: u32 = 10;
const FONT_SIZE: f32 = 16.0;
const LINE_HEIGHT: u32 = 20; // Font size + spacing
const CHAR_WIDTH: u32 = 10; // Approximate monospace width

// Embed the Hack font at compile time
const FONT_DATA: &[u8] = include_bytes!("../fonts/Hack-Regular.ttf");

/// Input event from keyboard
#[repr(C)]
struct InputEvent {
    event_type: u16,
    code: u16,
    value: u32,
}

const EV_KEY: u16 = 0x01;

/// Terminal state
struct Terminal {
    surface: libpanda::Handle,
    font: Font,
    width: u32,
    height: u32,
    cursor_x: u32,
    cursor_y: u32,
    line_buffer: String,
}

impl Terminal {
    fn new(surface: libpanda::Handle, font: Font, width: u32, height: u32) -> Self {
        Self {
            surface,
            font,
            width,
            height,
            cursor_x: MARGIN,
            cursor_y: MARGIN,
            line_buffer: String::new(),
        }
    }

    /// Clear the screen
    fn clear(&mut self) {
        let fill_params = FillParams {
            x: 0,
            y: 0,
            width: self.width,
            height: self.height,
            color: COLOR_BACKGROUND,
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

    /// Draw a single character at current cursor position
    fn draw_char(&mut self, ch: char) -> Result<(), &'static str> {
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

        // Convert grayscale bitmap to ARGB
        let pixels = glyph_buffer.as_mut_slice();
        for py in 0..glyph_height {
            for px in 0..glyph_width {
                let src_idx = py * glyph_width + px;
                let dst_idx = (py * glyph_width + px) * 4;
                let alpha = bitmap[src_idx];

                // Write BGRA (little-endian ARGB)
                pixels[dst_idx] = 0xFF;     // B
                pixels[dst_idx + 1] = 0xFF; // G
                pixels[dst_idx + 2] = 0xFF; // R
                pixels[dst_idx + 3] = alpha; // A
            }
        }

        // Calculate position
        let glyph_x = self.cursor_x + metrics.xmin as u32;
        let glyph_y = self.cursor_y + (FONT_SIZE as i32 - metrics.height as i32 - metrics.ymin) as u32;

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

    /// Handle a newline
    fn newline(&mut self) {
        self.cursor_x = MARGIN;
        self.cursor_y += LINE_HEIGHT;

        // Simple scrolling: if we go off-screen, clear and start over
        if self.cursor_y + LINE_HEIGHT > self.height - MARGIN {
            self.clear();
        }
    }

    /// Handle backspace
    fn backspace(&mut self) {
        if self.cursor_x > MARGIN {
            // Erase by drawing a black rectangle over the previous character
            self.cursor_x = self.cursor_x.saturating_sub(CHAR_WIDTH);

            let fill_params = FillParams {
                x: self.cursor_x,
                y: self.cursor_y,
                width: CHAR_WIDTH,
                height: LINE_HEIGHT,
                color: COLOR_BACKGROUND,
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

    /// Handle a typed character
    fn handle_char(&mut self, ch: char) {
        // Check if we need to wrap
        if self.cursor_x + CHAR_WIDTH > self.width - MARGIN {
            self.newline();
        }

        if let Ok(()) = self.draw_char(ch) {
            self.line_buffer.push(ch);
        }
        self.flush();
    }

    /// Handle Enter key
    fn handle_enter(&mut self) {
        // For now, just newline. Later can process line_buffer as command
        environment::log(&self.line_buffer);
        self.line_buffer.clear();
        self.newline();
        self.flush();
    }

    /// Handle Backspace key
    fn handle_backspace(&mut self) {
        if !self.line_buffer.is_empty() {
            self.line_buffer.pop();
            self.backspace();
            self.flush();
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
        57 => Some(' '),  // Space
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

libpanda::main! {
    environment::log("terminal: Starting");

    // Load the font
    let font = Font::from_bytes(FONT_DATA, FontSettings::default())
        .expect("Failed to load font");

    // Open a window surface
    let Ok(surface) = environment::open("surface:/window", 0) else {
        environment::log("terminal: Failed to open window");
        return 1;
    };

    // Set window parameters (640x480 window at position 50, 50)
    let window_width = 640u32;
    let window_height = 480u32;

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

    // Open keyboard
    let Ok(keyboard) = environment::open("keyboard:/pci/00:03.0", 0) else {
        environment::log("terminal: Failed to open keyboard");
        return 1;
    };

    // Create terminal state
    let mut term = Terminal::new(surface, font, window_width, window_height);
    term.clear();

    environment::log("terminal: Ready - type to see characters echoed");

    // Main event loop
    let mut event_buf = [0u8; 8];
    let mut shift_pressed = false;

    loop {
        // Read keyboard event (blocking)
        let n = file::read(keyboard, &mut event_buf);
        if n < 0 {
            continue;
        }

        if n as usize >= core::mem::size_of::<InputEvent>() {
            let event = unsafe { &*(event_buf.as_ptr() as *const InputEvent) };

            if event.event_type == EV_KEY && event.value == 1 {
                // Key pressed (value=1), ignore release (value=0) and repeat (value=2)

                // Track shift state
                if event.code == 42 || event.code == 54 {
                    // Left shift (42) or right shift (54)
                    shift_pressed = true;
                    continue;
                }

                // Handle special keys
                match event.code {
                    28 => term.handle_enter(),     // Enter
                    14 => term.handle_backspace(), // Backspace
                    _ => {
                        // Try to convert to character
                        if let Some(ch) = keycode_to_char(event.code, shift_pressed) {
                            term.handle_char(ch);
                        }
                    }
                }
            } else if event.event_type == EV_KEY && event.value == 0 {
                // Key released
                if event.code == 42 || event.code == 54 {
                    shift_pressed = false;
                }
            }
        }
    }
}
