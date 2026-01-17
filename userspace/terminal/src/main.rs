#![no_std]
#![no_main]

extern crate alloc;
extern crate panda_abi;

use fontdue::{Font, FontSettings};
use libpanda::{buffer::Buffer, environment, syscall::send};
use panda_abi::{
    BlitParams, FillParams, PixelFormat, SurfaceInfoOut, OP_SURFACE_BLIT, OP_SURFACE_FILL,
    OP_SURFACE_FLUSH, OP_SURFACE_INFO,
};

// Terminal colors (ARGB format)
const COLOR_BACKGROUND: u32 = 0xFF000000; // Black
const COLOR_BORDER: u32 = 0xFF303030; // Dark gray
const COLOR_TITLEBAR: u32 = 0xFF1E1E1E; // Slightly lighter gray
const COLOR_ACCENT: u32 = 0xFF007ACC; // Blue accent
const COLOR_TEXT: u32 = 0xFFFFFFFF; // White

const BORDER_WIDTH: u32 = 2;
const TITLEBAR_HEIGHT: u32 = 24;
const FONT_SIZE: f32 = 16.0;

// Embed the Hack font at compile time
const FONT_DATA: &[u8] = include_bytes!("../fonts/Hack-Regular.ttf");

/// Draw text at the specified position.
fn draw_text(
    surface: libpanda::Handle,
    font: &Font,
    text: &str,
    x: u32,
    y: u32,
    color: u32,
) -> Result<(), &'static str> {
    let mut cursor_x = x;

    for ch in text.chars() {
        // Rasterize the character
        let (metrics, bitmap) = font.rasterize(ch, FONT_SIZE);

        if metrics.width == 0 || metrics.height == 0 {
            // Space or other non-visible character
            cursor_x += metrics.advance_width as u32;
            continue;
        }

        // Create a buffer for the glyph
        let glyph_width = metrics.width;
        let glyph_height = metrics.height;
        let buffer_size = (glyph_width * glyph_height * 4) as usize;

        let Some(mut glyph_buffer) = Buffer::alloc(buffer_size) else {
            return Err("Failed to allocate glyph buffer");
        };

        // Convert grayscale bitmap to ARGB with the specified color
        let pixels = glyph_buffer.as_mut_slice();
        for py in 0..glyph_height {
            for px in 0..glyph_width {
                let src_idx = py * glyph_width + px;
                let dst_idx = (py * glyph_width + px) * 4;

                let alpha = bitmap[src_idx];

                // Extract RGB from color
                let r = ((color >> 16) & 0xFF) as u8;
                let g = ((color >> 8) & 0xFF) as u8;
                let b = (color & 0xFF) as u8;

                // Write ARGB
                pixels[dst_idx] = b;
                pixels[dst_idx + 1] = g;
                pixels[dst_idx + 2] = r;
                pixels[dst_idx + 3] = alpha;
            }
        }

        // Calculate position (accounting for bearing)
        let glyph_x = cursor_x + metrics.xmin as u32;
        let glyph_y = y + (FONT_SIZE as i32 - metrics.height as i32 - metrics.ymin) as u32;

        // Blit the glyph to the surface
        let blit_params = BlitParams {
            x: glyph_x,
            y: glyph_y,
            width: glyph_width as u32,
            height: glyph_height as u32,
            buffer_handle: glyph_buffer.handle().as_raw(),
        };

        let result = send(
            surface,
            OP_SURFACE_BLIT,
            &blit_params as *const BlitParams as usize,
            0,
            0,
            0,
        );

        if result < 0 {
            return Err("Failed to blit glyph");
        }

        // Advance cursor
        cursor_x += metrics.advance_width as u32;
    }

    Ok(())
}

/// Draw the terminal UI to the framebuffer.
fn draw_terminal_ui(
    surface: libpanda::Handle,
    font: &Font,
    width: u32,
    height: u32,
) -> Result<(), &'static str> {
    // Fill background
    let fill_params = FillParams {
        x: 0,
        y: 0,
        width,
        height,
        color: COLOR_BACKGROUND,
    };
    let result = send(
        surface,
        OP_SURFACE_FILL,
        &fill_params as *const FillParams as usize,
        0,
        0,
        0,
    );
    if result < 0 {
        return Err("Failed to fill background");
    }

    // Draw title bar
    let fill_params = FillParams {
        x: 0,
        y: 0,
        width,
        height: TITLEBAR_HEIGHT,
        color: COLOR_TITLEBAR,
    };
    let result = send(
        surface,
        OP_SURFACE_FILL,
        &fill_params as *const FillParams as usize,
        0,
        0,
        0,
    );
    if result < 0 {
        return Err("Failed to draw title bar");
    }

    // Draw accent line at bottom of title bar
    let fill_params = FillParams {
        x: 0,
        y: TITLEBAR_HEIGHT - 1,
        width,
        height: 1,
        color: COLOR_ACCENT,
    };
    let result = send(
        surface,
        OP_SURFACE_FILL,
        &fill_params as *const FillParams as usize,
        0,
        0,
        0,
    );
    if result < 0 {
        return Err("Failed to draw accent line");
    }

    // Draw border (top, left, right, bottom)
    let fill_params = FillParams {
        x: 0,
        y: 0,
        width,
        height: BORDER_WIDTH,
        color: COLOR_BORDER,
    };
    send(
        surface,
        OP_SURFACE_FILL,
        &fill_params as *const FillParams as usize,
        0,
        0,
        0,
    );

    let fill_params = FillParams {
        x: 0,
        y: 0,
        width: BORDER_WIDTH,
        height,
        color: COLOR_BORDER,
    };
    send(
        surface,
        OP_SURFACE_FILL,
        &fill_params as *const FillParams as usize,
        0,
        0,
        0,
    );

    let fill_params = FillParams {
        x: width - BORDER_WIDTH,
        y: 0,
        width: BORDER_WIDTH,
        height,
        color: COLOR_BORDER,
    };
    send(
        surface,
        OP_SURFACE_FILL,
        &fill_params as *const FillParams as usize,
        0,
        0,
        0,
    );

    let fill_params = FillParams {
        x: 0,
        y: height - BORDER_WIDTH,
        width,
        height: BORDER_WIDTH,
        color: COLOR_BORDER,
    };
    send(
        surface,
        OP_SURFACE_FILL,
        &fill_params as *const FillParams as usize,
        0,
        0,
        0,
    );

    // Draw title text
    draw_text(
        surface,
        font,
        "Panda Terminal",
        BORDER_WIDTH + 8,
        4,
        COLOR_TEXT,
    )?;

    // Draw some sample text in the content area
    let content_x = BORDER_WIDTH + 8;
    let mut content_y = TITLEBAR_HEIGHT + 8;

    draw_text(
        surface,
        font,
        "Welcome to Panda OS!",
        content_x,
        content_y,
        COLOR_TEXT,
    )?;

    content_y += FONT_SIZE as u32 + 4;
    draw_text(
        surface,
        font,
        "Terminal emulator with fontdue rendering",
        content_x,
        content_y,
        COLOR_TEXT,
    )?;

    content_y += FONT_SIZE as u32 + 4;
    draw_text(
        surface,
        font,
        "Font: Hack 16px",
        content_x,
        content_y,
        COLOR_ACCENT,
    )?;

    content_y += FONT_SIZE as u32 + 8;
    draw_text(
        surface,
        font,
        "ABCDEFGHIJKLMNOPQRSTUVWXYZ",
        content_x,
        content_y,
        COLOR_TEXT,
    )?;

    content_y += FONT_SIZE as u32 + 4;
    draw_text(
        surface,
        font,
        "abcdefghijklmnopqrstuvwxyz",
        content_x,
        content_y,
        COLOR_TEXT,
    )?;

    content_y += FONT_SIZE as u32 + 4;
    draw_text(
        surface,
        font,
        "0123456789 !@#$%^&*()_+-=[]{}",
        content_x,
        content_y,
        COLOR_TEXT,
    )?;

    // Flush the entire surface
    let result = send(surface, OP_SURFACE_FLUSH, 0, 0, 0, 0);
    if result < 0 {
        return Err("Failed to flush surface");
    }

    Ok(())
}

libpanda::main! {
    environment::log("terminal: Starting");

    // Load the font
    let font = Font::from_bytes(FONT_DATA, FontSettings::default())
        .expect("Failed to load font");

    environment::log("terminal: Loaded font");

    // Open the framebuffer surface
    let Ok(surface) = environment::open("surface:/fb0", 0) else {
        environment::log("terminal: Failed to open framebuffer");
        return 1;
    };

    environment::log("terminal: Opened framebuffer");

    // Get surface info
    let mut info = SurfaceInfoOut {
        width: 0,
        height: 0,
        format: 0,
        stride: 0,
    };

    let result = send(
        surface,
        OP_SURFACE_INFO,
        &mut info as *mut SurfaceInfoOut as usize,
        0,
        0,
        0,
    );

    if result < 0 {
        environment::log("terminal: Failed to get surface info");
        return 1;
    }

    if info.format != PixelFormat::ARGB8888 as u32 {
        environment::log("terminal: Unsupported pixel format");
        return 1;
    }

    environment::log("terminal: Got surface info");

    // Draw the terminal UI with text
    if let Err(msg) = draw_terminal_ui(surface, &font, info.width, info.height) {
        environment::log(msg);
        return 1;
    }

    environment::log("terminal: Drew UI with text");

    // TODO: Main event loop
    // - Read keyboard input
    // - Render characters as they're typed
    // - Handle scrolling
    // - Process escape sequences

    // For now, just loop forever (terminal stays visible)
    loop {
        // Could yield to be nice to other processes
        // libpanda::process::yield_cpu();
    }
}
