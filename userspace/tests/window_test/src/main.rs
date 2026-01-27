#![no_std]
#![no_main]

use libpanda::environment;
use libpanda::graphics::{Colour, PixelBuffer, Rect, Window};

libpanda::main! {
    environment::log("Window test starting");

    // Create a window at position (50, 50) with size 400x300
    let mut window = match Window::builder()
        .size(400, 300)
        .position(50, 50)
        .visible(true)
        .build()
    {
        Ok(w) => w,
        Err(_) => {
            environment::log("FAIL: Could not create window");
            return 1;
        }
    };
    environment::log("PASS: Created window");

    // Allocate a pixel buffer for the window content
    let (window_width, window_height) = window.size();
    let mut buffer = match PixelBuffer::new(window_width, window_height) {
        Ok(b) => b,
        Err(_) => {
            environment::log("FAIL: Could not allocate buffer");
            return 1;
        }
    };
    environment::log("PASS: Allocated buffer");

    // Fill the buffer with a test pattern (4 coloured quadrants)
    let half_width = window_width / 2;
    let half_height = window_height / 2;

    // Red (top-left)
    buffer.fill_rect(Rect::new(0, 0, half_width, half_height), Colour::RED);
    // Green (top-right)
    buffer.fill_rect(Rect::new(half_width, 0, half_width, half_height), Colour::GREEN);
    // Blue (bottom-left)
    buffer.fill_rect(Rect::new(0, half_height, half_width, half_height), Colour::BLUE);
    // Yellow (bottom-right)
    buffer.fill_rect(Rect::new(half_width, half_height, half_width, half_height), Colour::YELLOW);

    environment::log("PASS: Filled buffer with test pattern");

    // Blit the buffer to the window
    if window.blit(&buffer, 0, 0).is_err() {
        environment::log("FAIL: Could not blit buffer to window");
        return 1;
    }
    environment::log("PASS: Blitted buffer to window");

    // Flush the window to display
    if window.flush().is_err() {
        environment::log("FAIL: Could not flush window");
        return 1;
    }
    environment::log("PASS: Flushed window");

    // Signal that screenshot can be taken
    environment::screenshot_ready();

    // Loop forever - test runner will capture screenshot and quit QEMU
    loop {
        // Just spin
    }
}
