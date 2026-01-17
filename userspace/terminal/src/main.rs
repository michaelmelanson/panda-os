#![no_std]
#![no_main]

extern crate panda_abi;

use libpanda::{environment, syscall::send};
use panda_abi::{
    FillParams, PixelFormat, SurfaceInfoOut, OP_SURFACE_FILL, OP_SURFACE_FLUSH, OP_SURFACE_INFO,
};

// Terminal colors (ARGB format)
const COLOR_BACKGROUND: u32 = 0xFF000000; // Black
const COLOR_BORDER: u32 = 0xFF303030; // Dark gray
const COLOR_TITLEBAR: u32 = 0xFF1E1E1E; // Slightly lighter gray
const COLOR_ACCENT: u32 = 0xFF007ACC; // Blue accent

const BORDER_WIDTH: u32 = 2;
const TITLEBAR_HEIGHT: u32 = 24;

/// Draw the terminal UI to the framebuffer.
fn draw_terminal_ui(
    surface: libpanda::Handle,
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
    // Top border
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

    // Left border
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

    // Right border
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

    // Bottom border
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

    // Draw a simple "cursor" indicator in the top-left of the content area
    // (just to show something is drawn)
    let cursor_x = BORDER_WIDTH + 8;
    let cursor_y = TITLEBAR_HEIGHT + 8;
    let cursor_width = 8;
    let cursor_height = 16;

    let fill_params = FillParams {
        x: cursor_x,
        y: cursor_y,
        width: cursor_width,
        height: cursor_height,
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
        return Err("Failed to draw cursor");
    }

    // Flush the entire surface
    let result = send(surface, OP_SURFACE_FLUSH, 0, 0, 0, 0);
    if result < 0 {
        return Err("Failed to flush surface");
    }

    Ok(())
}

libpanda::main! {
    environment::log("terminal: Starting");

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

    // Draw the terminal UI
    if let Err(msg) = draw_terminal_ui(surface, info.width, info.height) {
        environment::log(msg);
        return 1;
    }

    environment::log("terminal: Drew UI");

    // TODO: Main event loop
    // - Read keyboard input
    // - Render characters (once we have font rendering)
    // - Handle scrolling
    // - Process escape sequences

    // For now, just loop forever (terminal stays visible)
    loop {
        // Could yield to be nice to other processes
        // libpanda::process::yield_cpu();
    }
}
