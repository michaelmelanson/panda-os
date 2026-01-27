#![no_std]
#![no_main]

use libpanda::environment;
use libpanda::graphics::{Colour, PixelBuffer, Window};

libpanda::main! {
    environment::log("Partial refresh test starting");

    let window_width = 400u32;
    let window_height = 400u32;

    // Create a 400x400 window at (100, 50)
    let mut window = match Window::builder()
        .size(window_width, window_height)
        .position(100, 50)
        .visible(true)
        .build()
    {
        Ok(w) => w,
        Err(_) => {
            environment::log("FAIL: Could not open window");
            return 1;
        }
    };
    environment::log("PASS: Opened window");
    environment::log("PASS: Set window parameters");

    // Allocate buffer for full window and fill with blue
    let mut full_buffer = match PixelBuffer::new(window_width, window_height) {
        Ok(b) => b,
        Err(_) => {
            environment::log("FAIL: Could not allocate full buffer");
            return 1;
        }
    };

    full_buffer.clear(Colour::BLUE);

    let _ = window.blit(&full_buffer, 0, 0);
    let _ = window.flush();
    environment::log("PASS: Filled window with blue");

    // Now create buffers for partial updates
    let partial_width = 200u32;
    let partial_height = 200u32;

    // Update top-left quarter with red
    let mut red_buffer = match PixelBuffer::new(partial_width, partial_height) {
        Ok(b) => b,
        Err(_) => {
            environment::log("FAIL: Could not allocate red buffer");
            return 1;
        }
    };

    red_buffer.clear(Colour::RED);

    let _ = window.blit(&red_buffer, 0, 0);
    let _ = window.flush();
    environment::log("PASS: Updated top-left quarter with red");

    // Update bottom-right quarter with green
    let mut green_buffer = match PixelBuffer::new(partial_width, partial_height) {
        Ok(b) => b,
        Err(_) => {
            environment::log("FAIL: Could not allocate green buffer");
            return 1;
        }
    };

    green_buffer.clear(Colour::GREEN);

    let _ = window.blit(&green_buffer, 200, 200);
    let _ = window.flush();
    environment::log("PASS: Updated bottom-right quarter with green");

    environment::log("PASS: Partial refresh test complete");

    // Signal that screenshot can be taken
    environment::screenshot_ready();

    // Loop forever
    loop {}
}
