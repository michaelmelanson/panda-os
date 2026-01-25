#![no_std]
#![no_main]

extern crate alloc;
extern crate panda_abi;

use alloc::string::String;
use alloc::vec::Vec;
use fontdue::{Font, FontSettings};
use libpanda::{
    buffer::Buffer,
    environment,
    mailbox::{Event, KeyValue, Mailbox},
    process,
    syscall::send,
    Handle,
};
use panda_abi::{
    BlitParams, FillParams, UpdateParamsIn, EVENT_KEYBOARD_KEY, EVENT_PROCESS_EXITED,
    OP_SURFACE_BLIT, OP_SURFACE_FILL, OP_SURFACE_FLUSH, OP_SURFACE_UPDATE_PARAMS,
};

// Terminal colors (ARGB format)
const COLOR_BACKGROUND: u32 = 0xFF000000; // Black

const MARGIN: u32 = 10;
const FONT_SIZE: f32 = 16.0;
const LINE_HEIGHT: u32 = 20; // Font size + spacing
const CHAR_WIDTH: u32 = 10; // Approximate monospace width

// Embed the Hack font at compile time
const FONT_DATA: &[u8] = include_bytes!("../fonts/Hack-Regular.ttf");

// Shift key codes
const KEY_LEFTSHIFT: u16 = 42;
const KEY_RIGHTSHIFT: u16 = 54;
const KEY_ENTER: u16 = 28;
const KEY_BACKSPACE: u16 = 14;

/// Terminal state
struct Terminal {
    surface: Handle,
    mailbox: Mailbox,
    font: Font,
    width: u32,
    height: u32,
    cursor_x: u32,
    cursor_y: u32,
    line_buffer: String,
    /// Currently running child process (if any)
    child: Option<Handle>,
}

impl Terminal {
    fn new(surface: Handle, mailbox: Mailbox, font: Font, width: u32, height: u32) -> Self {
        Self {
            surface,
            mailbox,
            font,
            width,
            height,
            cursor_x: MARGIN,
            cursor_y: MARGIN,
            line_buffer: String::new(),
            child: None,
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
                pixels[dst_idx] = 0xFF; // B
                pixels[dst_idx + 1] = 0xFF; // G
                pixels[dst_idx + 2] = 0xFF; // R
                pixels[dst_idx + 3] = alpha; // A
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

    /// Write a string to the terminal
    fn write_str(&mut self, s: &str) {
        for ch in s.chars() {
            if ch == '\n' {
                self.newline();
            } else {
                if self.cursor_x + CHAR_WIDTH > self.width - MARGIN {
                    self.newline();
                }
                let _ = self.draw_char(ch);
            }
        }
        self.flush();
    }

    /// Write a line (string + newline) to the terminal
    fn write_line(&mut self, s: &str) {
        self.write_str(s);
        self.newline();
        self.flush();
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
        // Search paths in order:
        // 1. If command contains '/', use as-is
        // 2. Look in /mnt (ext2 filesystem)
        // 3. Look in /initrd

        if cmd.contains('/') {
            // Absolute or relative path - use as-is with file: prefix
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

        // Spawn the process with mailbox attachment for exit notification
        match environment::spawn(
            &path,
            &arg_refs,
            self.mailbox.handle().as_raw(),
            EVENT_PROCESS_EXITED,
        ) {
            Ok(child_handle) => {
                self.child = Some(child_handle);
                // Don't print prompt until child exits
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
                // Get exit code
                let exit_code = process::wait(child);
                if exit_code != 0 {
                    self.write_line(&alloc::format!("(exited with code {})", exit_code));
                }
            }
        }
    }

    /// Handle Enter key
    fn handle_enter(&mut self) {
        self.newline();

        if !self.line_buffer.trim().is_empty() {
            self.execute_command();
        }

        self.line_buffer.clear();
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

/// Handle a key event from the mailbox
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
                        term.handle_char(ch);
                    }
                }
            }
        }
        KeyValue::Release => {
            // Track shift release
            if code == KEY_LEFTSHIFT || code == KEY_RIGHTSHIFT {
                *shift_pressed = false;
            }
        }
    }
}

libpanda::main! {
    environment::log("terminal: Starting");

    // Load the font
    let font = Font::from_bytes(FONT_DATA, FontSettings::default())
        .expect("Failed to load font");

    // Get the default mailbox for event aggregation
    let mailbox = Mailbox::default();

    // Open a window surface (no mailbox attachment needed)
    let Ok(surface) = environment::open("surface:/window", 0, 0) else {
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

    // Open keyboard with mailbox attachment for key events
    let Ok(_keyboard) = environment::open(
        "keyboard:/pci/00:03.0",
        mailbox.handle().as_raw(),
        EVENT_KEYBOARD_KEY,
    ) else {
        environment::log("terminal: Failed to open keyboard");
        return 1;
    };

    // Create terminal state (keyboard handle not needed since events come via mailbox)
    let mut term = Terminal::new(surface, mailbox, font, window_width, window_height);
    term.clear();

    // Print welcome message
    term.write_line("Panda OS Terminal");
    term.write_line("Type 'help' for available commands.");
    term.write_str("> ");

    let mut shift_pressed = false;

    // Main event loop using mailbox
    loop {
        let (handle, event) = term.mailbox.recv();

        match event {
            Event::Key(key_event) => {
                // Process key event directly from mailbox
                handle_key_event(&mut term, key_event.code, key_event.value, &mut shift_pressed);
            }
            Event::ProcessExited => {
                // Child process exited
                term.handle_child_exit(handle);
                // Show prompt for next command
                term.write_str("> ");
            }
            _ => {
                // Ignore other events
            }
        }
    }
}
